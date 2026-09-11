//! The one edge that carries an ordering between two threads.
//!
//! Design: `spec/safe-memory/09-type-init-and-races.md` section 9.5, interposed the way
//! `spec/safe-memory/10-boundaries.md` section 10.3 interposes everything else.
//!
//! [`crate::epoch`] counts each thread's own work and nothing else, and a counter per thread with
//! nothing joining them says every pair of threads is concurrent forever. That is not a detector, it
//! is a machine for reporting on correct programs. Section 9.5 names the one thing that joins them:
//! a lock. Whoever gives the lock up publishes the clock they gave it up at, whoever takes it next
//! reads that clock and moves their own past it, and everything the first thread did before the
//! release is then ordered before everything the second does after the acquire.
//!
//! That is Lamport's rule and it is the whole of the ordering. There are no vector clocks here, so
//! what this establishes is an ordering that really holds, and what it misses is orderings that hold
//! for reasons this never saw. Missing one costs a report. Inventing one costs a report too, which
//! is why it is worth being exact about when the edge is taken.
//!
//! # When the edge is taken
//!
//! A release publishes before the lock is really given up. Publishing afterwards would leave a
//! window where another thread already holds the lock, is already writing under it, and has read a
//! clock from before the work it was handed.
//!
//! An acquire takes the edge after the call returns and only when the call says it got the lock. A
//! `pthread_mutex_trylock` that came back `EBUSY` was handed nothing, and ordering the caller behind
//! the holder's work for it would be an ordering that does not exist, which is the direction that
//! hides real races.
//!
//! # The table, and why being wrong about it is safe
//!
//! A lock has nowhere to keep a clock. `pthread_mutex_t` is the C library's and this crate may not
//! grow it, so the clocks live in a fixed table here, keyed by the lock's address, open addressed,
//! never emptied and never grown. A program with more live locks than [`EDGES`] has locks whose
//! clock lands in a cell some other lock is using, and a cell is taken over rather than shared.
//!
//! That sounds worse than it is, and the reason is worth writing down because it is what makes a
//! table this simple acceptable. Every ordering this detector knows about was established by an edge
//! that carried the publisher's clock at the moment it published it, and taking an edge only ever
//! moves a clock forward. So a stale entry, a stolen cell, a lost release, even a clock from an
//! entirely different lock, can only put a thread further ahead than it needed to be, and a thread
//! further ahead reports fewer races. It cannot make a pair that really was ordered look
//! concurrent, because the chain of edges that ordered them carried real clocks and is still there.
//!
//! What a full table costs is recall, and it costs it only for the locks that collided. That is the
//! same trade the thread numbers make when they run out and the same one the clock makes when it
//! stops.
//!
//! # What is not here
//!
//! The other two edges. A thread that is created is ordered after everything its creator had done,
//! and a thread that is joined orders everything it did before the join returns, and neither of
//! those goes through a lock. Without them a program that fills a buffer, hands it to a worker and
//! joins looks like two threads that never met, so those are the next thing this file wants and
//! they come before anything reads the plane.
//!
//! The atomics are not here either. A C11 `atomic_store` with release ordering is an edge and it is
//! not a call, so there is nothing to interpose: it is the compiler's half, and it belongs with the
//! judgements rather than here.

use core::ffi::{c_int, c_void};
use core::sync::atomic::Ordering::Relaxed;
use core::sync::atomic::{AtomicU64, AtomicUsize};

use crate::epoch::{self, Stamp};
use crate::interpose;

/// The C library's own, called once the edge has been taken or published.
mod real {
    use core::ffi::{c_int, c_void};

    unsafe extern "C" {
        pub(super) fn pthread_mutex_lock(mutex: *mut c_void) -> c_int;
        pub(super) fn pthread_mutex_trylock(mutex: *mut c_void) -> c_int;
        pub(super) fn pthread_mutex_unlock(mutex: *mut c_void) -> c_int;
        pub(super) fn pthread_rwlock_rdlock(lock: *mut c_void) -> c_int;
        pub(super) fn pthread_rwlock_wrlock(lock: *mut c_void) -> c_int;
        pub(super) fn pthread_rwlock_tryrdlock(lock: *mut c_void) -> c_int;
        pub(super) fn pthread_rwlock_trywrlock(lock: *mut c_void) -> c_int;
        pub(super) fn pthread_rwlock_unlock(lock: *mut c_void) -> c_int;
    }
}

/// How many bits of a lock's address the table is indexed by.
const BITS: u32 = 10;

/// How many locks can have a clock of their own at once.
///
/// A thousand, which is more live locks than almost any program has and sixteen kilobytes of
/// nothing for the ones that have none. What happens past it is in the module note: the edge for
/// the locks that collided is lost, and a lost edge is lost reports rather than invented ones.
pub const EDGES: usize = 1 << BITS;

/// How far a lookup walks before it decides the lock is not in the table.
///
/// Short on purpose. This runs inside every lock and unlock in the program, so a walk that got
/// longer as the table filled would make the slowest programs the slowest to check. Eight cells is
/// one or two cache lines and it is a bound rather than an average.
const PROBES: usize = 8;

/// One lock's published clock.
struct Edge {
    /// The lock's address, or zero for a cell nobody has taken.
    lock: AtomicUsize,
    /// What its last releaser's clock was.
    stamp: AtomicU64,
}

impl Edge {
    /// A cell holding nothing.
    const fn new() -> Self {
        Self { lock: AtomicUsize::new(0), stamp: AtomicU64::new(epoch::NONE) }
    }
}

/// Every lock's published clock, for the whole process.
///
/// Relaxed throughout, and it is enough for the same reason [`crate::epoch::Epochs`] gives: what a
/// stamp means is the number inside it and not the ordering of the load that found it. The edge
/// that matters is carried by the real lock anyway. A relaxed store sequenced before a real unlock
/// is visible to a relaxed load sequenced after the matching real lock, because the C library's own
/// implementation is what establishes the happens-before between those two, and every other path
/// through here is one where a stale read costs a report and nothing else.
static PUBLISHED: [Edge; EDGES] = [const { Edge::new() }; EDGES];

/// Where a lock's clock would be kept if it has one.
///
/// Multiplicative, because the low bits of a lock's address say more about what the allocator did
/// than about which lock it is: a table of mutexes inside one structure is a run of addresses a
/// stride apart, and masking the low bits of those puts every one of them in the same few cells.
fn home(lock: usize) -> usize {
    ((lock as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> (u64::BITS - BITS)) as usize
}

/// The cell `step` places along from where `lock` would start.
fn cell(lock: usize, step: usize) -> &'static Edge {
    &PUBLISHED[(home(lock) + step) % EDGES]
}

/// What the last thread to give `lock` up was at, or [`crate::epoch::NONE`].
///
/// The walk stops at a cell nobody has taken. Cells are only ever taken and never given back, and a
/// lookup takes the first free cell of the run, so a gap in the run means the lock was never put in
/// it. That is what makes the common case of a lock nothing has released one load rather than
/// eight.
fn published(lock: usize) -> Stamp {
    for step in 0..PROBES {
        let cell = cell(lock, step);
        match cell.lock.load(Relaxed) {
            0 => return epoch::NONE,
            held if held == lock => return cell.stamp.load(Relaxed),
            _ => {}
        }
    }
    epoch::NONE
}

/// This thread is giving `lock` up, so publish what it is at.
///
/// Called before the lock is really released, for the reason the module note gives.
pub fn released(lock: *mut c_void) {
    let lock = lock as usize;
    if lock == 0 {
        return;
    }
    let stamp = epoch::here();
    if stamp == epoch::NONE {
        // A thread with nowhere to keep a clock. It has published nothing all along, so there is
        // nothing here for anybody to take, and writing zero would erase what a thread that does
        // have a clock had honestly left.
        return;
    }
    for step in 0..PROBES {
        let cell = cell(lock, step);
        let held = cell.lock.load(Relaxed);
        if held == lock
            || (held == 0 && cell.lock.compare_exchange(0, lock, Relaxed, Relaxed).is_ok())
        {
            cell.stamp.store(stamp, Relaxed);
            return;
        }
    }
    // Every cell of the run belongs to some other lock. Take the first one over rather than giving
    // up on this lock forever: whichever of the two loses its edge loses reports, and the one that
    // is still being released is the one more likely to still matter.
    let cell = cell(lock, 0);
    cell.lock.store(lock, Relaxed);
    cell.stamp.store(stamp, Relaxed);
}

/// This thread has just taken `lock`, so take the ordering that came with it.
///
/// A lock nobody has released through this monitor answers [`crate::epoch::NONE`], which moves
/// nothing: a clock that is already at least one cannot be pushed below where it is.
pub fn acquired(lock: *mut c_void) {
    let lock = lock as usize;
    if lock == 0 {
        return;
    }
    epoch::sync(published(lock));
}

interpose! {
    group: Ordering;

    /// `pthread_mutex_lock`, which is the edge in the form nearly every program spells it.
    ///
    /// Everything the last holder did before it unlocked is ordered before everything this thread
    /// does from here, and saying so is what stops the whole of document 09 section 9.5 from
    /// reporting on every correctly locked program in the corpus.
    fn pthread_mutex_lock(mutex: *mut c_void) -> c_int
        where acquires(mutex)
    {
        // SAFETY: the pointer is the one the program passed, and this is what it passed it to.
        unsafe { real::pthread_mutex_lock(mutex) }
    }

    /// `pthread_mutex_trylock`, which is the same edge when it worked and no edge when it did not.
    ///
    /// The one row where the return value decides, and the reason the generator tests it rather
    /// than taking the edge unconditionally. A failed try hands the caller nothing, so ordering it
    /// behind the holder's work would be an ordering the program never had.
    fn pthread_mutex_trylock(mutex: *mut c_void) -> c_int
        where acquires(mutex)
    {
        // SAFETY: as `pthread_mutex_lock`.
        unsafe { real::pthread_mutex_trylock(mutex) }
    }

    /// `pthread_mutex_unlock`, which is the other half and the one that publishes.
    fn pthread_mutex_unlock(mutex: *mut c_void) -> c_int
        where releases(mutex)
    {
        // SAFETY: as `pthread_mutex_lock`.
        unsafe { real::pthread_mutex_unlock(mutex) }
    }

    /// `pthread_rwlock_rdlock`, whose edge comes from the last writer.
    ///
    /// Two readers holding this at once take the same clock and are not ordered against each other,
    /// which is right: neither of them wrote anything, so there is nothing between them to order.
    fn pthread_rwlock_rdlock(lock: *mut c_void) -> c_int
        where acquires(lock)
    {
        // SAFETY: as `pthread_mutex_lock`.
        unsafe { real::pthread_rwlock_rdlock(lock) }
    }

    /// `pthread_rwlock_wrlock`, which is the exclusive half and the plain edge.
    fn pthread_rwlock_wrlock(lock: *mut c_void) -> c_int
        where acquires(lock)
    {
        // SAFETY: as `pthread_mutex_lock`.
        unsafe { real::pthread_rwlock_wrlock(lock) }
    }

    /// `pthread_rwlock_tryrdlock`, which is `pthread_rwlock_rdlock` when it worked.
    fn pthread_rwlock_tryrdlock(lock: *mut c_void) -> c_int
        where acquires(lock)
    {
        // SAFETY: as `pthread_mutex_lock`.
        unsafe { real::pthread_rwlock_tryrdlock(lock) }
    }

    /// `pthread_rwlock_trywrlock`, which is `pthread_rwlock_wrlock` when it worked.
    fn pthread_rwlock_trywrlock(lock: *mut c_void) -> c_int
        where acquires(lock)
    {
        // SAFETY: as `pthread_mutex_lock`.
        unsafe { real::pthread_rwlock_trywrlock(lock) }
    }

    /// `pthread_rwlock_unlock`, which publishes whichever of the two locks was held.
    ///
    /// A reader publishing its clock gives the next writer an edge from the reader, which is a real
    /// one: the read lock really was given up before the write lock was taken.
    fn pthread_rwlock_unlock(lock: *mut c_void) -> c_int
        where releases(lock)
    {
        // SAFETY: as `pthread_mutex_lock`.
        unsafe { real::pthread_rwlock_unlock(lock) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::Group;
    use crate::epoch::{clock, thread, unordered};

    /// A lock address nothing dereferences.
    ///
    /// The table is keyed by the address and never reads through it, which is the property that
    /// lets these tests use numbers rather than real mutexes and stay out of the way of whatever
    /// the harness's own locks are doing. Each test uses its own, because the table is one table
    /// for the whole process and tests run beside each other.
    const fn fake(which: usize) -> *mut c_void {
        (0x5000 + which * 0x40) as *mut c_void
    }

    #[test]
    fn a_thread_that_takes_a_lock_gets_past_everything_the_releaser_had_done() {
        // The whole point of the file. Without this every store the first thread made stays
        // concurrent with everything the second one does for the rest of the program, which is a
        // report about a program that locked correctly.
        let lock = fake(1);
        // The address travels as a number, because a raw pointer is not `Send` and the table keys
        // on the number anyway. Nothing on either side reads through it.
        let travelling = lock as usize;
        let theirs = std::thread::spawn(move || {
            let stamp = epoch::tick();
            released(travelling as *mut c_void);
            stamp
        })
        .join()
        .expect("the thread ran");

        assert!(unordered(theirs, epoch::here()), "and before the edge it was concurrent");
        acquired(lock);
        assert!(!unordered(theirs, epoch::here()));
        assert_ne!(thread(theirs), thread(epoch::here()), "two threads, not one");
    }

    #[test]
    fn taking_the_edge_puts_this_thread_strictly_past_the_releaser() {
        // Strictly, and the difference is the whole of the false positive. Landing on the same
        // count as the releaser would leave that thread's last store looking concurrent with
        // everything this one does next, since a stamp is unordered against an equal clock.
        let lock = fake(2);
        released(lock);
        let published = published(lock as usize);

        acquired(lock);
        assert!(clock(epoch::here()) > clock(published));
    }

    #[test]
    fn a_lock_nobody_has_released_carries_nothing() {
        // Which is most locks, most of the time, and it has to move the clock by nothing at all. A
        // lock the program only ever takes is not an ordering and pretending it is would hide the
        // races it is failing to protect against.
        let lock = fake(3);
        let before = epoch::here();
        acquired(lock);
        assert_eq!(epoch::here(), before);
        assert_eq!(published(lock as usize), epoch::NONE);
    }

    #[test]
    fn a_cell_taken_over_by_another_lock_answers_nothing_rather_than_somebody_elses_clock() {
        // The table filling up, which is the case the module note argues is safe. What it must not
        // do is hand one lock's clock out under another lock's name often enough to matter, and
        // what it does instead is answer that it knows nothing, which loses a report.
        for which in 0..EDGES * 4 {
            released(fake(0x1000 + which));
        }
        for which in 0..EDGES * 4 {
            let lock = fake(0x1000 + which) as usize;
            let found = published(lock);
            if found != epoch::NONE {
                assert_eq!(thread(found), thread(epoch::here()), "this thread released them all");
            }
        }
    }

    #[test]
    fn a_lock_is_not_its_neighbour() {
        // Two mutexes a stride apart inside one structure is the commonest arrangement there is,
        // and an index that used the low bits of the address would put a whole array of them in one
        // cell. Every one of these has to keep its own clock.
        let stamps: std::vec::Vec<Stamp> = (0..16)
            .map(|which| {
                let _ = epoch::tick();
                released(fake(0x200 + which));
                epoch::here()
            })
            .collect();

        for (which, stamp) in stamps.iter().enumerate() {
            assert_eq!(published(fake(0x200 + which) as usize), *stamp, "lock {which} lost its");
        }
    }

    #[test]
    fn a_null_lock_is_left_alone() {
        // A program that passes null to `pthread_mutex_lock` has a bug the C library gets to
        // report. Zero is also how this table spells a cell nobody has taken, so letting one
        // through would key an entry under the word that means empty.
        let before = epoch::here();
        released(core::ptr::null_mut());
        acquired(core::ptr::null_mut());
        assert_eq!(epoch::here(), before);
    }

    #[test]
    fn every_row_is_in_the_ordering_group_and_none_of_them_touches_a_range() {
        // The rows as data, which is what `--emit=safety-summary` counts and what `cargo xtask
        // interpose` reads. An ordering row with an effects clause would be a row in the wrong
        // group, since a range judgement here would be a claim about memory this never looks at.
        assert_eq!(TABLE.len(), 8);
        for row in TABLE {
            assert_eq!(row.group, Group::Ordering, "{} is in the wrong group", row.name);
            assert!(row.effects.is_empty(), "{} claims a range", row.name);
            assert!(row.wrapper.starts_with("__rucc_wrap_"), "{} has the wrong symbol", row.name);
        }
    }
}
