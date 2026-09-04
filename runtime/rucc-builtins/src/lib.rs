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
//! # Status
//!
//! The block routines are here: `memcpy`, `memmove`, `memset` and `memcmp`, which are what
//! `rucc-codegen` calls when a structure copy or a fill is too big to open up into moves. The
//! rest of section 12.8, which is the wide division and modulo, the `__int128` arithmetic, the
//! soft float and the atomics, is not written, and the set is driven by what the target ladder
//! in `spec/14-target-ladder.md` actually calls.

#![no_std]
// A `memcpy` written as a loop is a loop the optimizer is allowed to recognize and replace with
// a call to `memcpy`, which would be this function calling itself forever. This is the attribute
// that says the names in this crate are the implementations rather than uses.
#![no_builtins]
#![doc(html_root_url = "https://docs.rs/rucc-builtins/0.1.0")]

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
