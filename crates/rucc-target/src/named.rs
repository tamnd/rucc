//! Finding an entry of a target's tables by its name.
//!
//! Every question a pass asks a target about an opcode is asked by name, and the tables that
//! answer are lists of names in the order the description was written in. Looking a name up by
//! walking the list compares it against every name before it, which for x86-64's seven hundred
//! opcodes is a few hundred string comparisons, and a pass asks that for every instruction of
//! every function. On jtckdint that walk was about a seventh of the whole `-O2` build.
//!
//! So each table is hashed once, the first time anybody asks it anything, and every question after
//! that is one hash of the name.
//!
//! The hash is not SipHash. Every name is one this crate wrote into a table, so there is nobody to
//! defend against, and SipHash over the name was still over a percent of jtckdint's build once
//! the walk was gone.

use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};

/// A table from each name to what is known about it, hashed with [`Words`].
pub(crate) type Names<T> = HashMap<&'static str, T, BuildHasherDefault<Words>>;

/// Hashes a name eight bytes at a time, with a rotate, an exclusive or and a multiply for each.
#[derive(Debug, Default)]
pub(crate) struct Words(u64);

impl Words {
    fn mix(&mut self, word: u64) {
        self.0 = (self.0.rotate_left(5) ^ word).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    }
}

impl Hasher for Words {
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
}

/// Where each name in that table is.
///
/// A name that is in the table twice is found where it is first, which is what walking the table
/// would have found.
pub(crate) fn index<T>(table: &[(&'static str, T)]) -> Names<usize> {
    let mut at = Names::with_capacity_and_hasher(table.len(), BuildHasherDefault::default());
    for (number, &(name, _)) in table.iter().enumerate() {
        at.entry(name).or_insert(number);
    }
    at
}

/// Every place each name is in that table, in the order they come in, for a table where one name
/// has several rows and the caller picks between them.
pub(crate) fn every<T>(table: &[T], name: impl Fn(&T) -> &'static str) -> Names<Vec<usize>> {
    let mut at: Names<Vec<usize>> = Names::default();
    for (number, entry) in table.iter().enumerate() {
        at.entry(name(entry)).or_default().push(number);
    }
    at
}
