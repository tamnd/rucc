//! A hasher for the maps the compiler keys by its own small numbers.
//!
//! The standard library hashes with SipHash, which is built to stand up to somebody choosing keys
//! that collide. A register, a value or an instruction is a small number the compiler handed out
//! itself, so there is nobody choosing them, and on jtckdint's `test.c` hashing them with SipHash
//! was more than a percent of the build in more than one pass. [`Mix`] does one rotate, one
//! exclusive or and one multiply per word instead.
//!
//! Nothing may read the order of a map hashed with this any more than one hashed with SipHash.
//! The order is fixed for a given set of keys, but it is a fact about the hash and not about the
//! input, and `spec/02-the-goal.md` asks for output that only depends on the input.

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};

/// A map hashed with [`Mix`].
pub type Map<K, V> = HashMap<K, V, BuildHasherDefault<Mix>>;

/// A set hashed with [`Mix`].
pub type Set<T> = HashSet<T, BuildHasherDefault<Mix>>;

/// Hashes a word at a time, with a rotate, an exclusive or and a multiply for each.
///
/// The multiply spreads each word over the high bits, which are the ones the table reads. Bytes
/// are taken eight at a time, so a name costs one step per eight bytes of it.
#[derive(Debug, Default, Clone, Copy)]
pub struct Mix(u64);

impl Mix {
    fn mix(&mut self, word: u64) {
        self.0 = (self.0.rotate_left(5) ^ word).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    }
}

impl Hasher for Mix {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for chunk in bytes.chunks(8) {
            let mut word = [0; 8];
            word[..chunk.len()].copy_from_slice(chunk);
            self.mix(u64::from_le_bytes(word));
        }
    }

    fn write_u8(&mut self, byte: u8) {
        self.mix(u64::from(byte));
    }

    fn write_u16(&mut self, half: u16) {
        self.mix(u64::from(half));
    }

    fn write_u32(&mut self, word: u32) {
        self.mix(u64::from(word));
    }

    fn write_u64(&mut self, word: u64) {
        self.mix(word);
    }

    fn write_usize(&mut self, word: usize) {
        self.mix(word as u64);
    }
}

#[cfg(test)]
mod tests {
    use std::hash::{BuildHasher, BuildHasherDefault};

    use super::{Map, Mix, Set};

    /// The same key hashes the same way every time, and a map and a set built with it find what
    /// was put in them.
    #[test]
    fn a_key_hashes_the_same_way_every_time_and_is_found_again() {
        let build = BuildHasherDefault::<Mix>::default();
        assert_eq!(build.hash_one((3u8, 17u32)), build.hash_one((3u8, 17u32)));
        assert_ne!(build.hash_one(1u32), build.hash_one(2u32));
        assert_ne!(build.hash_one("add.i32"), build.hash_one("add.i64"));

        let mut map: Map<u32, usize> = Map::default();
        let mut set: Set<&str> = Set::default();
        for number in 0..1000 {
            map.insert(number * 7, number as usize);
        }
        for name in ["mov", "movzx", "movsx", "lea", "a name longer than eight bytes"] {
            set.insert(name);
        }
        assert!((0..1000).all(|number| map[&(number * 7)] == number as usize));
        assert_eq!(map.get(&1), None);
        assert!(set.contains("a name longer than eight bytes"));
        assert!(!set.contains("a name longer than eight byte"));
    }
}
