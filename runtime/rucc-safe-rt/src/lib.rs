//! The memory safety runtime.
//!
//! Design: `spec/safe-memory/15-integration.md` section 15.1. Outside the layer stack, beside
//! `rucc-builtins`, and compiled *for the target* rather than for the host.
//!
//! Everything the compiler can decide at compile time it decides, and what is left over lands
//! here: the planes of document 04 section 4.3, the allocator of document 05 section 5.2.2, the
//! interposition API of document 10 section 10.4, the libc wrappers of document 10 section 10.3,
//! and the reporter behind `__rucc_safety_fail`. That list is short on purpose. Document 14
//! section 14.8 puts this crate in the trust set explicitly, and a trust set entry that is a few
//! thousand lines can be read by the person relying on it.
//!
//! # Status
//!
//! The trap entry point and the descriptor it is handed, and the lifetime plane. Milestone S1 in
//! `spec/safe-memory/16-milestones.md` is the one being built, and it asks for bounds and
//! lifetime and nothing else, so the type, init and epoch planes are not here and neither is the
//! allocator that drives this one. The allocator is next, the interposition API is S3 and the
//! reporter is S2.

#![no_std]
#![doc(html_root_url = "https://docs.rs/rucc-safe-rt/0.5.0")]

// The tests format and compare, which `core` cannot do. The crate itself never sees this.
#[cfg(test)]
extern crate std;

pub mod fail;
pub mod heap;
pub mod layout;
pub mod plane;

/// The milestone in `spec/safe-memory/16-milestones.md` that fills this crate in.
pub const MILESTONE: &str = "S1";

/// A `#![no_std]` crate needs a panic handler of its own, on every target and not only a bare
/// one, because nothing here links the standard library that would otherwise supply it. Under
/// `cargo test` the test harness does link it, so this is only compiled in when it is actually
/// missing.
#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    // A monitor that panics is a monitor that has lost track of the program it is watching, and
    // there is no unwinder to hand the panic to and no allocator to format it with. Stopping is
    // the only honest thing left.
    loop {}
}
