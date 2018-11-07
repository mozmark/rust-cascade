#![allow(dead_code)]
#![allow(unused_imports)]

extern crate bitvec;
extern crate byteorder;
extern crate digest;
extern crate murmurhash3;
extern crate rand;

use bitvec::*;
use byteorder::{LittleEndian, ReadBytesExt};
use murmurhash3::murmurhash3_x86_32;

use std::io::prelude::*;
use std::io;
use std::io::Read;
use std::iter::FromIterator;
use std::str;

pub struct Bloom {
    level: u32,
    n_hash_funcs: u32,
    size: usize,
    bitvec: BitVec<bitvec::LittleEndian>,
}

pub fn calculate_n_hash_funcs(error_rate: f32) -> u32 {
        return ((1.0 / error_rate).ln() / (2.0_f32).ln()).ceil() as u32;
}

pub fn calculate_size(elements: usize, error_rate: f32) -> usize {
        let n_hash_funcs = calculate_n_hash_funcs(error_rate);
        let hashes = n_hash_funcs as f32;
        return (1.0_f32 - (hashes * (elements as f32 + 0.5) / (1.0_f32 - error_rate.powf(1.0 / hashes)).ln())).ceil() as usize;
}

impl Bloom {
    pub fn new(size: usize, n_hash_funcs: u32, level: u32) -> Bloom {
        let bitvec: BitVec<bitvec::LittleEndian> = bitvec![LittleEndian; 0; size];

        Bloom {
            level: level,
            n_hash_funcs: n_hash_funcs,
            size: size,
            bitvec: bitvec,
        }
    }

    // TODO: MDG - this could usefully return a Result since parsing can fail
    pub fn from_bytes(mut bytes: &[u8]) -> Bloom {
        // Load the layer metadata. bloomer.py writes size, nHashFuncs and level as little-endian
        // unsigned ints.
        // TODO: MDG - we should match on bytes.len() and return an error result if too small
        let size = bytes.read_i32::<LittleEndian>().unwrap() as usize;
        let n_hash_funcs = bytes.read_i32::<LittleEndian>().unwrap() as u32;
        let level = bytes.read_i32::<LittleEndian>().unwrap() as u32;

        let byte_count = (size as f32 / 8.0).ceil() as usize;

        // TODO: MDG - check the byte_count matches the available data and return an error result if too small

        Bloom {
            level: level,
            n_hash_funcs: n_hash_funcs,
            size: size,
            bitvec: bytes[0..byte_count].into(),
        }
    }

    fn hash(&self, n_fn: u32, key: &[u8]) -> usize {
        println!("key is {:?}", key);
        let hash_seed = (n_fn << 16) + self.level;
        let h = murmurhash3_x86_32(key, hash_seed) as usize % self.size;
        println!("h from hash is {}, maxu32 is {}", h, std::u32::MAX);
        h
    }

    pub fn put(&mut self, item: &[u8]) {
        for i in 0..self.n_hash_funcs {
            let index = self.hash(i, item);
            self.bitvec.set(index, true);
        }
    }

    pub fn has(&self, item: &[u8]) -> bool {
        println!("n_hash_funcs {}", self.n_hash_funcs);
        for i in 0..self.n_hash_funcs {
            if  self.bitvec.get(self.hash(i, item)) == false {
                println!("not in {}#{}", self.level, i);
                return false;
            } else {
                println!("in {}#{}", self.level, i);
            }
        }

        return true
    }

    pub fn clear(&mut self) {
        self.bitvec.clear()
    }
}

pub struct Cascade {
    filter: Bloom,
    child_layer: Option<Box<Cascade>>,
}

impl Cascade {
    pub fn new(size: usize, n_hash_funcs: u32) -> Cascade {
        return Cascade::new_layer(size, n_hash_funcs, 1);
    }

    // TODO: MDG - this could usefully return a Result since parsing can fail
    pub fn from_bytes(bytes: &[u8]) -> Option<Box<Cascade>> {
        match bytes.len() {
            0 => Option::None,
            _ => {
                let fil = Bloom::from_bytes(bytes);
                let len = fil.size;
                let byte_count = (len as f32 / 8.0).ceil() as usize;

                return Option::Some(Box::new(Cascade{
                    filter: fil,
                    child_layer: Cascade::from_bytes(&bytes[(12 + byte_count)..]), // a layer header is 12 bytes (f32, u32, u32)
                }));
            }
        }
    }

    fn new_layer(size: usize, n_hash_funcs: u32, layer: u32) -> Cascade {
        Cascade {
            filter: Bloom::new(size, n_hash_funcs, layer),
            child_layer: Option::None,
        }
    }

    pub fn initialize(&mut self, entries: Vec<Vec<u8>>, exclusions: Vec<Vec<u8>>) {
        let mut false_positives = Vec::new();
        for entry in &entries {
            self.filter.put(entry);
        }

        for entry in exclusions {
            if self.filter.has(&entry) {
                false_positives.push(entry);
            }
        }

        if false_positives.len() > 0 {
            let n_hash_funcs = calculate_n_hash_funcs(0.5);
            let size = calculate_size(false_positives.len(), 0.5);
            let mut child = Box::new(
                Cascade::new_layer(size, n_hash_funcs, self.filter.level + 1));
            child.initialize(false_positives, entries);
            self.child_layer = Some(child);
        }
    }

    pub fn has(&self, entry: &[u8]) -> bool {
        if self.filter.has(&entry) {
            match self.child_layer {
                Some(ref child) => {
                    let child_value = ! child.has(entry);
                    println!("child_value is {}", child_value);
                    return child_value;
                },
                None => {
                    println!("no child; returning true");
                    return true;
                }
            }
        }
        println!("no entry; returning false");
        return false;
    }

    pub fn check(&self, entries: Vec<Vec<u8>>, exclusions: Vec<Vec<u8>>) -> bool {
        for entry in entries {
            if ! self.has(&entry.clone()) {
                return false;
            }
        }

        for entry in exclusions {
            if self.has(&entry.clone()) {
                return false;
            }
        }

        true
    }
}

#[cfg(test)]
mod tests {
    use Bloom;
    use Cascade;
    use calculate_n_hash_funcs;
    use calculate_size;
    use rand::prelude::*;

    use std::fs::File;
    use std::io::Read;
    use bitvec::{BitVec, Bits};

    #[test]
    fn bloom_test_bloom_size() {
        let error_rate = 0.01;
        let elements = 1024;
        let n_hash_funcs = calculate_n_hash_funcs(error_rate);
        let size = calculate_size(elements, error_rate);

        let bloom = Bloom::new(size, n_hash_funcs, 0);
        println!("{}", bloom.bitvec.len());
        assert!(bloom.bitvec.len() == 9829);
    }

    #[test]
    fn bloom_test_put() {
        let error_rate = 0.01;
        let elements = 1024;
        let n_hash_funcs = calculate_n_hash_funcs(error_rate);
        let size = calculate_size(elements, error_rate);

        let mut bloom = Bloom::new(size, n_hash_funcs, 0);
        let key: &[u8] = b"foo";

        bloom.put(key);
    }

    #[test]
    fn bloom_test_has() {
        let error_rate = 0.01;
        let elements = 1024;
        let n_hash_funcs = calculate_n_hash_funcs(error_rate);
        let size = calculate_size(elements, error_rate);

        let mut bloom = Bloom::new(size, n_hash_funcs, 0);
        let key: &[u8] = b"foo";

        bloom.put(key);
        assert!(bloom.has(key) == true);
        assert!(bloom.has(b"bar") == false);
    }

    #[test]
    fn bloom_test_from_bytes() {
        let src: Vec<u8> = vec![0x09, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x41, 0x00];

        let mut bloom = Bloom::from_bytes(&src);
        assert!(bloom.has(b"this") ==  true);
        assert!(bloom.has(b"that") ==  true);
        assert!(bloom.has(b"other") ==  false);

        bloom.put(b"other");
        assert!(bloom.has(b"other") ==  true);
    }

    #[test]
    fn bloom_test_from_file() {
        let f = File::open("test_data/test_bf").unwrap();
        
        let file_result: Result<Vec<u8>, _> = f.bytes().collect();
        let mut v: Vec<u8> = vec![];
        match file_result {
            Ok(data) => {
                for elem in data {
                    v.push(elem);
                }
                        
                let bloom = Bloom::from_bytes(&v);

                assert!(bloom.has(b"this") == true);
                assert!(bloom.has(b"that") == true);
                assert!(bloom.has(b"yet another test") == false);
            },
            Err(err) => {
                println!("Something went wrong! {:?}", err);
            }
        }
    }

    #[test]
    fn cascade_test() {
        // thread_rng is often the most convenient source of randomness:
        let mut rng = thread_rng();

        // create some entries and exclusions
        let mut foo : Vec<Vec<u8>> = Vec::new();
        let mut bar : Vec<Vec<u8>> = Vec::new();

        for i in 0..500 {
            let s = format!("{}", i);
            let bytes = s.into_bytes();
            foo.push(bytes);
        }

        for _ in 0..100 {
            let idx = rng.gen_range(0, foo.len());
            bar.push(foo.swap_remove(idx));
        }

        let error_rate = 0.5;
        let elements = 500;
        let n_hash_funcs = calculate_n_hash_funcs(error_rate);
        let size = calculate_size(elements, error_rate);

        let mut cascade = Cascade::new(size, n_hash_funcs);
        cascade.initialize(foo.clone(), bar.clone());

        assert!(cascade.check(foo.clone(), bar.clone()) == true);
    }

    #[test]
    fn cascade_from_file_bytes_test() {
        
        let mut f = File::open("test_data/test_mlbf").unwrap();

        let mut v: Vec<u8> = Vec::with_capacity(f.metadata().unwrap().len() as usize);
        f.read_to_end(&mut v).unwrap();
                
        let opt = Cascade::from_bytes(&v);

        match opt {
            Some(cascade) => {
                assert!(cascade.has(b"test") == true);
                assert!(cascade.has(b"another test") == true);
                assert!(cascade.has(b"yet another test") == true);
                assert!(cascade.has(b"blah") == false);
                assert!(cascade.has(b"blah blah") == false);
                assert!(cascade.has(b"blah blah blah") == false);
            },
            None => {}
        }
    }
}