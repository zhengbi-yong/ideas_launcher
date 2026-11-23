//! A quick hash for the tables that are keyed by number.
//!
//! The hasher of the standard library is made to keep the keys from leaking,
//! and that costs about twenty cycles for every lookup. The index of the file
//! system is looked up once for **every candidate of a search**, and a broad
//! query has millions of candidates — so the difference between twenty cycles
//! and three is a fifth of the search.
//!
//! The hash is not meant to keep anything secret: the keys are the numbers of
//! the entries of the index, and nobody is after them.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

/// A hasher that is quick and good enough for the numbers of the index.
#[derive(Default, Clone, Copy)]
pub struct FastHasher {
    state: u64,
}

impl FastHasher {
    /// Mix a number into the state.
    #[inline]
    fn add(&mut self, value: u64) {
        // The multiply spreads the bits of the key over the whole word, so
        // that the tables do not end up with long runs of the same bucket.
        // The rotation keeps the keys that differ in one bit from landing in
        // the same bucket.
        self.state = (self.state.rotate_left(5) ^ value).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
}

impl Hasher for FastHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.add(byte as u64);
        }
    }

    #[inline]
    fn write_u8(&mut self, value: u8) {
        self.add(value as u64);
    }

    #[inline]
    fn write_u16(&mut self, value: u16) {
        self.add(value as u64);
    }

    #[inline]
    fn write_u32(&mut self, value: u32) {
        self.add(value as u64);
    }

    #[inline]
    fn write_u64(&mut self, value: u64) {
        self.add(value);
    }

    #[inline]
    fn write_usize(&mut self, value: usize) {
        self.add(value as u64);
    }

    #[inline]
    fn finish(&self) -> u64 {
        self.state
    }
}

/// A map whose keys are hashed with [`FastHasher`].
pub type FastMap<K, V> = HashMap<K, V, BuildHasherDefault<FastHasher>>;

/// A map whose keys are hashed with [`FastHasher`], with room for `n` entries.
pub fn fast_map<K, V>() -> FastMap<K, V> {
    FastMap::default()
}

/// A map whose keys are hashed with [`FastHasher`], with room for `n` entries.
pub fn fast_map_with_capacity<K, V>(n: usize) -> FastMap<K, V> {
    FastMap::with_capacity_and_hasher(n, BuildHasherDefault::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn puts_and_finds_the_keys() {
        let mut map = fast_map_with_capacity::<u32, u32>(16);
        for key in 0..1000u32 {
            map.insert(key, key * 2);
        }
        for key in 0..1000u32 {
            assert_eq!(map.get(&key), Some(&(key * 2)));
        }
        assert_eq!(map.get(&1234), None);
    }

    #[test]
    fn spreads_the_keys_over_the_buckets() {
        // The keys that differ in one bit must not land in the same bucket,
        // or the table degenerates into a list.
        let mut map = fast_map_with_capacity::<u64, u64>(0);
        for bit in 0..64u64 {
            map.insert(1u64 << bit, bit);
        }
        assert_eq!(map.len(), 64);
        for bit in 0..64u64 {
            assert_eq!(map.get(&(1u64 << bit)), Some(&bit));
        }
    }
}
