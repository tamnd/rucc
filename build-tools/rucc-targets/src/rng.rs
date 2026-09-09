//! The deterministic generator the two corpora are drawn from.

/// A small deterministic generator.
///
/// splitmix64, which is four lines and has no state to get wrong. Not `rand`, because this crate
/// has three dependencies and every one of them is a crate the compiler already contains, and a
/// generated corpus that is the same on every machine needs an algorithm rather than a library.
pub(crate) struct Rng(u64);

impl Rng {
    pub(crate) fn new(seed: u64) -> Rng {
        Rng(seed)
    }

    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// A number below `limit`, which is biased for a limit that does not divide two to the
    /// sixty four and does not matter here: a corpus has to be the same everywhere, not uniform.
    pub(crate) fn below(&mut self, limit: u64) -> u64 {
        self.next() % limit
    }
}
