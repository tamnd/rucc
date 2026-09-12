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
//! `spec/safe-memory/16-milestones.md`, which asks for bounds and lifetime and nothing else.
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
//! Milestone S5 is the rest of the planes and it has started at the bottom. [`types`] is document
//! 09 section 9.1's type plane, which is what a byte was last stored through, in the shape document
//! 05 section 5.2.3 measured: one slot per eight bytes, and a side entry for a granule whose bytes
//! disagree. Four of document 03's type classes and one of its spatial classes are that plane
//! answering a question. It is mapped over every watched region beside the lifetime plane, an
//! instance forgets its bytes' types when it begins, and [`check`] has judgement J3 and the two
//! judgements that record it. The compiler's half is in: `rucc_safety` is where a store records
//! what its bytes were stored through, a copy carries whatever the bytes it read said over to the
//! bytes it wrote, and a read that names a type asks whether the bytes agree with it. So all three
//! of `__rucc_meta_type`, `__rucc_meta_type_copy` and `__rucc_check_type` are called by a program
//! built with `-fsafety`, and the type plane is a thing this runtime both keeps and decides on. The
//! union punning of C 6.5.2.3 is answered too: an access through a member of a union names the
//! character type, so a store through one member leaves bytes every later read of them agrees with.
//!
//! [`init`] is section 9.2's init plane, a bit for every byte of storage, which is document 03's Y6
//! and the kernel infoleak with it. The bit means uninitialized rather than initialized, so a byte
//! nobody has said anything about counts as written and a gap in instrumentation loses a check
//! rather than inventing a refusal, which is the inversion of MSan that section 9.2 argues for and
//! the reason this one could be shipped. It is mapped over every watched region beside the other
//! two, an instance forgets its bytes when it begins, `calloc` says it wrote what it zeroed and
//! `realloc` carries the answers of the bytes it moved, and [`check`] has the question a read asks
//! along with the two judgements that record what it asks about. The compiler emits those two
//! judgements beside every store and every copy, and the C library wrappers record what they wrote
//! as well, because a plane only some of the writes maintain reports on programs that are correct.
//! What is missing is the reader: no generated code reaches `__rucc_check_init` yet, so nothing is
//! refused on this plane's account, and that waits on section 9.3's padding mode.
//!
//! [`epoch`] is section 9.5's epoch plane, which is which thread last wrote a word and when in that
//! thread's own counting. It is the plane the races of document 03's C1 through C4 are answered
//! from, and what makes those answerable at all cheaply is that the ordering is Lamport's rather
//! than a vector clock: one word per eight bytes, one compare on the path an access already takes,
//! and an incompleteness the module writes out. It is mapped over every watched region beside the
//! other three, an instance forgets its stamps when it begins, each thread keeps a clock of its own
//! in the slot [`tls`] holds, and [`check`] has the judgement a store through a pointer shaped slot
//! makes and the check that reads it back. That check is J9, which document 04 section 4.4 numbers
//! apart from J1 for the reason J8 is numbered apart: it is a relation between two operations rather
//! than a property of one. It covers C2 and C3, which are the same comparison looked at from the
//! load side and the store side, and it names both threads in the report, because an address on its
//! own says which word raced and not who else was in it. C4 is answered from the same plane without
//! a check of its own: [`alloc`] stamps an instance's bytes with the freeing thread as it ends, so
//! the use after free [`check`] already refuses can say whether the free was another thread's and
//! whether anything ordered it against the access, which is the difference between a lifetime one
//! author got wrong and a lifetime two threads got wrong between them. C1, the torn store, is the
//! one class left, and it waits on the aux slot, since what it compares is a pointer word's stamp
//! against the stamp its capability was written at and there is nowhere yet for the second of those
//! to live.
//!
//! The compiler's half is in. `-fsafety-races` puts a `__rucc_meta_epoch` after every store of a
//! pointer and a `__rucc_check_race` in front of it, and `=pointer` puts one in front of a read of a
//! pointer as well, so a program built with it fills its own plane and asks it questions rather than
//! leaving both to the C library wrappers. The ordering that is not a call at all comes from the
//! compiler with them: a `__rucc_meta_release` in front of an atomic that publishes and a
//! `__rucc_meta_acquire` after one that takes, both landing in the same tables the interposed
//! primitives use. A bare `atomic_thread_fence` is in with them, as a pair of calls with no key at
//! all, since it orders against every thread rather than against an object and so has no address to
//! be keyed on. This is the only plane in the crate where missing
//! instrumentation costs a false report rather than a missed one, because two threads an edge
//! nobody saw really did join look exactly like two threads nothing joined.
//!
//! [`sync`] is the edges that make any of that answerable without reporting on correct programs. A
//! counter per thread with nothing joining them says every pair of threads is concurrent forever, so
//! section 9.5's ordering comes from the primitives that really do join them, interposed the way
//! everything else at the boundary is. All three are in. A lock release publishes the clock it was
//! given up at into a table keyed by the lock's address and the next thread to take the lock moves
//! its own clock past that. A thread being created starts inside this crate at a trampoline that
//! moves the new thread past its creator before the program's own start routine runs, which is the
//! edge a program that fills a buffer and hands it to a worker depends on and that no lock in such a
//! program stands in for. A thread finishing publishes what it ended at under the identifier a join
//! is given, and the join takes it. A condition variable gives the caller's mutex up inside the call
//! and holds it again by the time the call comes back, so it has a row of its own rather than being
//! covered by the lock the program wrote around it, and a semaphore is the lock edge under another
//! name. What is left is the ordering that is not a call at all, which is the atomics and belongs
//! with the judgements, and the primitives one Unix has and another does not, which the table has no
//! way to say yet.
//!
//! [`restrict`] is section 9.6, which is the one judgement that is not about a single access: a
//! block that declares `restrict` pointers promises that no object modified through one of them is
//! reached through another, and deciding that means comparing an access against the ones the same
//! block already made. It keeps a small scope in that block's own stack, the way [`frame`] keeps a
//! call frame, and it is J8. Nothing reaches it yet either: the front end works out which pointer
//! an access went through, and the pass that turns that into a call is the compiler's half.
//!
//! What is still missing is the `printf` family, which [`wrap`] says why about, and the `ioctl`
//! and `sockaddr` shaped syscalls, which [`syscall`] does. Everything the C library allocates
//! through a name other than those four is a hole of the same kind, and a program that frees one of
//! those results today gets a refusal it did not earn. The compiler's half of S2 is
//! `rucc_safety::wrap`, which points a call site at the wrapper, so the rows here are reached by a
//! program that was built with `-fsafety`.
//!
//! The report itself is short of what document 06 section 6.5 asks for, and [`report`] says which
//! three of the six things it names are there and why the other three are not. [`posture`] is the
//! rest of that section: what happens after the report, which is stopping by default and carrying on
//! under the posture a corpus run asks for, so that one bug does not hide a hundred.

#![no_std]
#![doc(html_root_url = "https://docs.rs/rucc-safe-rt/0.10.25")]

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
pub mod epoch;
pub mod fail;
#[cfg(unix)]
pub mod frame;
pub mod heap;
pub mod init;
pub mod layout;
pub mod plane;
pub mod posture;
#[cfg(unix)]
pub mod recover;
pub mod report;
#[cfg(unix)]
pub mod restrict;
#[cfg(unix)]
pub mod sync;
#[cfg(unix)]
pub mod syscall;
#[cfg(unix)]
pub mod tls;
pub mod types;
#[cfg(unix)]
pub mod wrap;

/// Every group of the interposition table, as one thing to walk.
///
/// A slice of slices rather than one flat table, because the generator writes a `TABLE` per group
/// and a group is a file. What `--emit=safety-summary` wants is the count per group as well as the
/// total, so the shape that keeps them apart is the shape it is going to ask for.
#[cfg(unix)]
pub static TABLES: &[&[effects::Row]] = &[wrap::TABLE, syscall::TABLE, sync::TABLE];

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

/// The name an unwinder would call, defined here because a link that never unwinds still has to
/// resolve it.
///
/// This crate is built with `panic = "abort"` and raises nothing anybody could catch, so nothing
/// in it wants a personality routine. `compiler_builtins` is not built that way. It ships in the
/// standard library for the target, it is linked into this archive, and one of its units holds a
/// reference to this name beside the weak definition of `fmod`. A C program that calls `fmod` and
/// gets it from here rather than from libm pulls that unit in, and the link then fails on a name
/// no part of the program ever calls.
///
/// SQLite is such a program, which is how this was found: every case in the safety suite is small
/// enough that no unit of `compiler_builtins` is ever needed, so the archive was only ever linked
/// against programs that could not hit it.
///
/// An empty body is the right one. Being called would mean an unwind is in progress in a build
/// where unwinding is off, which cannot happen, and returning normally from a personality routine
/// is what a phase the routine has nothing to say about looks like anyway.
#[cfg(not(test))]
#[unsafe(no_mangle)]
extern "C" fn rust_eh_personality() {}
