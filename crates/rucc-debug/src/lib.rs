//! DWARF 5 generation.
//!
//! Design: `spec/11-asm-objects-debug.md`. Layer rank 10, see `spec/18-package-layout.md`.
//!
//! # Status
//!
//! The line table, the types, the functions, the variables a unit defines at file scope and the
//! locals that have a frame slot. [`write()`] takes one unit's worth of addresses, the places in
//! the source they came from, the types the unit names, what each of its functions takes and gives
//! back, what each of its file-scope variables is and where in its frame each of its placed locals
//! sits, and gives back the sections that say so. That is enough for `addr2line` to answer a
//! program counter with a file and a line, enough for a debugger to produce a backtrace with
//! argument types in it, enough for one to print a global, and enough for one to print a local that
//! is in memory. It is not enough for a local held in an SSA value, because one of those has to be
//! said where it is at the program counter that is asking, which is a list of places rather than
//! one. The shape of that list is here and nothing fills it in yet: a local can be said to be in a
//! register over one stretch of a function's addresses and somewhere else over the next, and a
//! `.debug_loclists` saying so comes out, but what the driver hands over is still one place for the
//! whole of a function. Working out the stretches needs the register allocator's output and is the
//! rest of M8 and of tamnd/rucc#9.
//!
//! The reason that part came first is tamnd/rucc#1558. A report from the safety monitor carries a
//! program counter and deliberately carries no source location, because
//! `spec/safe-memory/06-instrumentation.md` section 6.5 would rather have one line table than two
//! that can disagree, and that only works once there is one. Until there was, every report out of a
//! corpus run had to be read backwards out of a disassembly.
//!
//! Every crate in the workspace is published, and publishing implies a promise. This one is
//! tier 3: its Rust API is explicitly unstable and will change without a major version bump.
//! Depend on the `rucc` binary's behaviour, not on this.
//!
//! # Two things still waiting
//!
//! `Options::prefix_map` in `rucc-session` holds a `debug` list, which is what
//! `-fdebug-prefix-map=` and `-ffile-prefix-map=` put there. Every path that reaches [`Unit`] has
//! already been through `PrefixMap::apply`, because that is the whole reason a distribution passes
//! those flags and because a build is only reproducible if all of the paths in it are rewritten
//! rather than most. The rewriting is the driver's rather than this crate's for one reason: the
//! driver is where a path is still a path, and by the time one arrives here it is a string in a
//! table that nothing is allowed to reinterpret.
//!
//! `Options::compress` is the other, and it is what `-gz` put there. Every debug section this
//! crate hands to the object writer has to be compressed the way that field says, which for
//! `Compress::ZlibGnu` also means the section is named `.zdebug_info` rather than `.debug_info`
//! and carries a `ZLIB` tag and a length instead of an `Elf64_Chdr`. Compressing the sections is
//! worth more than anything else a distribution shipping debug symbols passes, so a build that
//! asked for it and got a file twice the size it expected has a real complaint. Nothing reads it
//! yet, and on a line table alone the saving is smaller than it will be.

#![doc(html_root_url = "https://docs.rs/rucc-debug/0.10.74")]

mod line;
mod shape;
mod tree;

pub use crate::line::{Error, Function, Row, Unit, write};
pub use crate::shape::{
    Bits, Constant, Encoding, Global, Held, Local, Member, Param, Place, Qualifier, Shape, Sig,
    Span, Spot,
};

/// The milestone in `spec/17-milestones.md` that fills this crate in.
pub const MILESTONE: &str = "M8";

#[cfg(test)]
mod tests {
    #[test]
    fn milestone_is_recorded() {
        assert!(super::MILESTONE.starts_with('M'));
    }
}
