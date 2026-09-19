//! The runtime support library.
//!
//! Design: `spec/12-abi-and-runtime.md` section 12.8. Outside the layer stack, and the only
//! crate in the workspace compiled *for the target* rather than for the host.
//!
//! When the backend cannot express an operation in the target's instructions it emits a call,
//! and this is what it calls. The names and the calling conventions are libgcc's, because
//! object files we produce get linked against object files GCC produced and one of us has to
//! give way.
//!
//! # What ships is not this crate
//!
//! Section 12.8 settled in tamnd/rucc#912 that the archive a cross link reads is C compiled by
//! rucc itself, for two reasons that are both about thirty targets. The Rust path needs a Rust
//! target for every row of `spec/cross-compile/04-target-matrix.md` and that table has rows rustc
//! does not have, and the staticlib rustc produces brings Rust's own `compiler_builtins` with it,
//! which is 4.5 MB for the four routines below against a 10 MB budget for every tier-1 and tier-2
//! archive together.
//!
//! So this crate is the reference implementation rather than the shipped one. Section 12.8 asks
//! for the soft float paths to be differentially tested against a reference over the whole hazard
//! list, and this is what it means by one. Keeping it is cheaper than writing a second set of
//! tests for the C, and a reference compiled by a different compiler is a better reference than
//! one compiled by the compiler under test.
//!
//! # Status
//!
//! The block routines are here: `memcpy`, `memmove`, `memset` and `memcmp`, which are what
//! `rucc-codegen` calls when a structure copy or a fill is too big to open up into moves. The
//! 128-bit division and modulo are here too, which is the six entry points a `/` or a `%` on an
//! `__int128` becomes, since that is the one arithmetic `rucc-codegen` cannot split into
//! instructions over the halves, and the same six at 64 bits, which is what a 32-bit target makes
//! of a `/` on a `long long` for the same reason one width up. So are the conversions between 128
//! bits and a `float` or a `double`, which is eight more entry points and the other operation at
//! that width the machine has no instruction for. Single precision soft float is here as well,
//! which is everything a target with no floating point unit calls for a `float`: the four
//! operations and the negation, the eight comparisons, since `a < b` is a call there too, and the
//! eight conversions to and from an integer, since a cast is. That set is the first of these where
//! the reference and the shipped C are different algorithms rather than the same one written twice.
//! Double precision is here too, with the four operations, the negation, the eight comparisons and
//! the eight conversions to and from an integer on a `double`, which is the same shape one format up
//! and two routines that are genuinely new, a product that no longer fits in a word and a division
//! that cannot be written as a `/`. And so is the pair between the two formats, `__extendsfdf2` and
//! `__truncdfsf2`, which is the first thing here that needed both of them and is the end of the two
//! format soft float set. Quad precision is here now as well, with the four operations, the negation
//! and the eight comparisons on a `_Float128`, which is not a soft float set in the sense the other
//! two are: no machine has an instruction at that format, so those thirteen are calls on every target
//! rather than only on a target with no floating point unit. The conversions at that format are here
//! too, which is the twelve against an integer at each of the three widths and in each direction, and
//! the four that cross between a quad and a `float` or a `double`. The atomics are here as well, which
//! is the twenty routines libatomic exports at a width no machine reaches in one instruction and the
//! table of locks under them: the four with no width in their name that take a size and work through
//! pointers, and the sixteen at sixteen bytes that take the value itself, four of them the same
//! operations and twelve of them the read and update pairs. The rest of section 12.8, which is the
//! remaining `__int128` arithmetic and the eighty bit conversions, is not written, and the set is
//! driven by what the target ladder in `spec/14-target-ladder.md` actually calls.

#![no_std]
// A `memcpy` written as a loop is a loop the optimizer is allowed to recognize and replace with
// a call to `memcpy`, which would be this function calling itself forever. This is the attribute
// that says the names in this crate are the implementations rather than uses.
#![no_builtins]
// A 128-bit integer in an `extern "C"` signature was not FFI-safe as far as rustc was concerned
// until it settled how one is passed, and the floor this workspace builds against is older than
// that, so the warning is on the oldest compiler and on none of the newer ones. There is nothing
// here for it to be about. These routines exist because rucc emits calls to them for an `__int128`,
// and what rucc passes one in is the register pair the psABI names, which is the pair rustc passes
// it in as well. `atomic`, `convert` and `div` are the modules it lands on. The three modules that
// carry the same attribute of their own carry it for vector types instead, which is a separate
// question and one they answer where they ask it.
#![allow(improper_ctypes_definitions)]
#![doc(html_root_url = "https://docs.rs/rucc-builtins/0.10.65")]

pub mod atomic;
pub mod convert;
pub mod div;
pub mod double;
pub mod float;
pub mod mem;
pub mod quad;

// The tests allocate and compare, which `core` cannot do. The crate itself never sees this.
#[cfg(test)]
extern crate std;

/// The milestone in `spec/17-milestones.md` that fills this crate in.
pub const MILESTONE: &str = "M3";

/// A `#![no_std]` crate needs a panic handler of its own, on every target and not only a bare
/// one, because nothing here links the standard library that would otherwise supply it. Under
/// `cargo test` the test harness does link it, so this is only compiled in when it is actually
/// missing.
#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    // Nothing in a runtime support routine should ever panic. If one does, there is no
    // unwinder to hand it to and no allocator to format with, so the honest thing is to stop.
    loop {}
}
