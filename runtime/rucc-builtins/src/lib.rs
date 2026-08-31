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
//! Not implemented. The first entries land with the x86-64 backend in `M3`, and the set is
//! driven by what the target ladder in `spec/14-target-ladder.md` actually calls.

#![no_std]
#![doc(html_root_url = "https://docs.rs/rucc-builtins/0.0.1")]

/// The milestone in `spec/17-milestones.md` that fills this crate in.
pub const MILESTONE: &str = "M3";

/// A `#![no_std]` crate still needs a panic handler when it is built as a staticlib for a
/// bare target. Under `cargo test` on the host the test harness supplies one, so this is
/// only compiled in when it is actually missing.
#[cfg(all(not(test), target_os = "none"))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    // Nothing in a runtime support routine should ever panic. If one does, there is no
    // unwinder to hand it to and no allocator to format with, so the honest thing is to stop.
    loop {}
}
