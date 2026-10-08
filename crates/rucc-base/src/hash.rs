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
        let mut words = bytes.chunks_exact(8);
        for word in &mut words {
            self.mix(u64::from_le_bytes(word.try_into().expect("a chunk of eight")));
        }
        let rest = words.remainder();
        if !rest.is_empty() {
            self.mix(short_word(rest));
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

/// One to seven bytes as the little endian word they are the bottom of, the rest of it zero.
///
/// The same word copying them into a zeroed array of eight would make, read with at most three
/// loads. The copy was a call to `memcpy` for every word of every name the compiler hashed,
/// because its length is only known at run time.
#[inline]
fn short_word(bytes: &[u8]) -> u64 {
    let len = bytes.len();
    debug_assert!((1..8).contains(&len));
    if len >= 4 {
        // The two halves overlap when there are fewer than eight, and agree where they do.
        let low = u32::from_le_bytes(bytes[..4].try_into().expect("four bytes"));
        let high = u32::from_le_bytes(bytes[len - 4..].try_into().expect("four bytes"));
        u64::from(low) | (u64::from(high) << (8 * (len - 4)))
    } else {
        // The first, middle and last byte, which are the only ones there are for one to three.
        let middle = len / 2;
        u64::from(bytes[0])
            | (u64::from(bytes[middle]) << (8 * middle))
            | (u64::from(bytes[len - 1]) << (8 * (len - 1)))
    }
}

#[cfg(test)]
mod tests {
    use std::hash::{BuildHasher, BuildHasherDefault};

    use super::{Map, Mix, Set};

    /// The same key hashes the same way every time, and a map and a set built with it find what
    /// was put in them.
    /// Each short tail reads as the word a zero padded copy of it would be.
    #[test]
    fn a_short_tail_is_the_word_its_padded_copy_would_be() {
        let bytes = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77];
        for len in 1..8 {
            let mut word = [0; 8];
            word[..len].copy_from_slice(&bytes[..len]);
            assert_eq!(super::short_word(&bytes[..len]), u64::from_le_bytes(word), "{len} bytes");
        }
    }

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
