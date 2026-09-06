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
//! The trap entry point and the descriptor it is handed, the lifetime plane, the allocator over
//! it, `malloc`, `free`, `calloc` and `realloc`, the three checks generated code calls, and the
//! reporter that turns a refusal into words. That is milestone S1 in
//! `spec/safe-memory/16-milestones.md`, which asks for bounds and lifetime and nothing else, so the
//! type, init and epoch planes are not here.
//!
//! Milestone S2 is the boundary and it has started. [`effects`] is the vocabulary a row of document
//! 10 section 10.3's interposition table is written in and the generator that turns a row into a
//! wrapper, and [`wrap`] is the table itself, which now holds the whole movement group: the
//! functions whose extent is an argument, the ones whose extent is a terminator, and the ones that
//! copy, whose destination is judged against a length discovered while the call runs.
//!
//! [`syscall`] is section 10.5's group, where the kernel writes user memory without consulting
//! anything, and [`adopt`] is section 10.4's five functions, which is how an allocator this crate
//! did not write says that it has taken a region from the operating system and is carving objects
//! out of it. The heap is a list of regions rather than one reservation so that there is somewhere
//! for those to go.
//!
//! [`frame`] is section 5.3's call frame, which is where the capability of a pointer argument
//! travels. It travels beside the call rather than in the pointer because an instrumented
//! function's calling convention does not change, and that is the property the boundary is made
//! of: a caller this compiler built can hand its arguments to a callee some other compiler built,
//! and the other way round.
//!
//! [`recover`] is the other half of that. A caller that knows nothing about any of this publishes
//! no frame, so the callee has to reconstruct its arguments' capabilities from what the runtime
//! already knows about the addresses, and how much that is depends on where the address lands.
//! Recovery says which of four situations it was as well as what it found, and counts each one
//! separately, because those counts are most of what section 10.2's summary is for. The same module
//! answers the classification without the bounds walk, which is the form generated code calls
//! today: there is nowhere to keep a capability until the aux plane of milestone S5, so a crossing
//! is counted rather than reconstructed, and the counts are the same either way.
//!
//! What is still missing is the `printf` family, which [`wrap`] says why about, and the `ioctl`
//! and `sockaddr` shaped syscalls, which [`syscall`] does. Everything the C library allocates
//! through a name other than those four is a hole of the same kind, and a program that frees one of
//! those results today gets a refusal it did not earn. The compiler's half of S2 is
//! `rucc_safety::wrap`, which points a call site at the wrapper, so the rows here are reached by a
//! program that was built with `-fsafety`.
//!
//! The report itself is short of what document 06 section 6.5 asks for, and [`report`] says which
//! three of the six things it names are there and why the other three are not.

#![no_std]
#![doc(html_root_url = "https://docs.rs/rucc-safe-rt/0.7.4")]

// The tests format and compare, which `core` cannot do. The crate itself never sees this.
#[cfg(test)]
extern crate std;

#[cfg(unix)]
pub mod adopt;
#[cfg(unix)]
pub mod alloc;
#[cfg(unix)]
pub mod check;
#[cfg(unix)]
pub mod effects;
pub mod fail;
#[cfg(unix)]
pub mod frame;
pub mod heap;
pub mod layout;
pub mod plane;
#[cfg(unix)]
pub mod recover;
pub mod report;
#[cfg(unix)]
pub mod syscall;
#[cfg(unix)]
pub mod wrap;

/// Every group of the interposition table, as one thing to walk.
///
/// A slice of slices rather than one flat table, because the generator writes a `TABLE` per group
/// and a group is a file. What `--emit=safety-summary` wants is the count per group as well as the
/// total, so the shape that keeps them apart is the shape it is going to ask for.
#[cfg(unix)]
pub static TABLES: &[&[effects::Row]] = &[wrap::TABLE, syscall::TABLE];

/// The milestone in `spec/safe-memory/16-milestones.md` that fills this crate in.
pub const MILESTONE: &str = "S1";

/// The turnstile the tests in this crate queue at.
///
/// There is one heap and it is a `static`, so two tests that allocate at the same time are two
/// tests sharing a free list. Several of them say which address comes back next or which version
/// the plane holds for one, and neither is a fact unless nothing else allocated in between. Every
/// test that touches the heap takes this first, which makes the whole file sequential and costs
/// nothing worth counting.
///
/// Gated the same way its callers are. [`alloc`] and [`check`] are the two modules that queue
/// here and both of them are Unix only, so on Windows this would be a lock nothing takes, which
/// under `-D warnings` is a build failure rather than a spare `static`.
#[cfg(all(test, unix))]
mod turnstile {
    /// The lock itself, held for the whole of a test rather than for each call.
    static TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Waits for this test's turn at the heap.
    ///
    /// A poisoned lock is taken anyway: one test having failed should not turn the rest into
    /// failures about the lock.
    pub(crate) fn turn() -> std::sync::MutexGuard<'static, ()> {
        TURN.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// A `#![no_std]` crate needs a panic handler of its own, on every target and not only a bare
/// one, because nothing here links the standard library that would otherwise supply it. Under
/// `cargo test` the test harness does link it, so this is only compiled in when it is actually
/// missing.
#[cfg(not(test))]
#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    // The message is dropped rather than printed. Every panic this crate raises is a refused
    // judgement, `report` has already said what it was in the form somebody can read, and the
    // panic itself is only how stopping is spelled. There is no unwinder to hand it to.
    report::stop()
}
