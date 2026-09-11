//! DWARF 5 generation.
//!
//! Design: `spec/11-asm-objects-debug.md`. Layer rank 10, see `spec/18-package-layout.md`.
//!
//! # Status
//!
//! Not implemented. This crate exists from the first commit so that the layer rank it holds
//! is real and `cargo xtask layers` has something to check. The work lands in M8.
//!
//! Every crate in the workspace is published, and publishing implies a promise. This one is
//! tier 3: its Rust API is explicitly unstable and will change without a major version bump.
//! Depend on the `rucc` binary's behaviour, not on this.
//!
//! # One thing already waiting
//!
//! `Options::prefix_map` in `rucc-session` holds a `debug` list, which is what
//! `-fdebug-prefix-map=` and `-ffile-prefix-map=` put there. Every path this crate writes into a
//! compilation unit, a line table or a file table has to go through `PrefixMap::apply` on the way,
//! because that is the whole reason a distribution passes those flags and because a build is only
//! reproducible if all of the paths in it are rewritten rather than most. The driver takes the
//! flags today and nothing reads that list, which is honest only while there is no DWARF at all.

#![doc(html_root_url = "https://docs.rs/rucc-debug/0.10.19")]

/// The milestone in `spec/17-milestones.md` that fills this crate in.
pub const MILESTONE: &str = "M8";

#[cfg(test)]
mod tests {
    #[test]
    fn milestone_is_recorded() {
        assert!(super::MILESTONE.starts_with('M'));
    }
}
