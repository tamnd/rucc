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
//! Double precision is there now except for one pair, with the four operations, the negation, the
//! eight comparisons and the eight conversions to and from an integer on a `double`, which is the
//! same shape one format up and two routines that are genuinely new, a product that no longer fits in
//! a word and a division that cannot be written as a `/`. The rest of section 12.8, which is the pair
//! that widens and narrows between the two formats, quad soft float, the remaining `__int128`
//! arithmetic, the eighty bit and quad conversions and the atomics, is not written, and the set is
//! driven by what the target ladder in `spec/14-target-ladder.md` actually calls.

#![no_std]
// A `memcpy` written as a loop is a loop the optimizer is allowed to recognize and replace with
// a call to `memcpy`, which would be this function calling itself forever. This is the attribute
// that says the names in this crate are the implementations rather than uses.
#![no_builtins]
#![doc(html_root_url = "https://docs.rs/rucc-builtins/0.10.35")]

pub mod convert;
pub mod div;
pub mod double;
pub mod float;
pub mod mem;

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
