//! The edges that carry an ordering between two threads.
//!
//! Design: `spec/safe-memory/09-type-init-and-races.md` section 9.5, interposed the way
//! `spec/safe-memory/10-boundaries.md` section 10.3 interposes everything else.
//!
//! [`crate::epoch`] counts each thread's own work and nothing else, and a counter per thread with
//! nothing joining them says every pair of threads is concurrent forever. That is not a detector, it
//! is a machine for reporting on correct programs. Section 9.5 names what joins them: a lock, and
//! the thread itself being made and being waited for. Whoever gives a lock up publishes the clock
//! they gave it up at, whoever takes it next reads that clock and moves their own past it, and
//! everything the first thread did before the release is then ordered before everything the second
//! does after the acquire. Creating and joining are the same rule with a different key.
//!
//! That is Lamport's rule and it is the whole of the ordering. There are no vector clocks here, so
//! what this establishes is an ordering that really holds, and what it misses is orderings that hold
//! for reasons this never saw. Missing one costs a report. Inventing one costs a report too, which
//! is why it is worth being exact about when each edge is taken.
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
//! A thread being created takes the edge before it runs a line of the program's own code. The
//! creator's clock is read at the call, travels in a birth slot, and the first thing the new thread
//! does is move its own clock past it. A thread being joined takes the edge the other way: the
//! thread publishes what it is at as it returns from its start routine, and the join reads that once
//! the call says the thread really is finished.
//!
//! # The table, and why being wrong about it is safe
//!
//! Neither a lock nor a thread has anywhere to keep a clock. `pthread_mutex_t` is the C library's
//! and this crate may not grow it, and a `pthread_t` is a number the C library hands out, so the
//! clocks live in fixed tables here, keyed by the lock's address or by the thread's own identifier,
//! open addressed, never emptied and never grown. A program with more live keys than [`EDGES`] has
//! keys whose clock lands in a cell some other key is using, and a cell is taken over rather than
//! shared. A `pthread_t` is reused once a thread has been joined, so a cell can also hold a finished
//! thread's clock under a name that now means a different thread.
//!
//! That sounds worse than it is, and the reason is worth writing down because it is what makes
//! tables this simple acceptable. Every ordering this detector knows about was established by an
//! edge that carried the publisher's clock at the moment it published it, and taking an edge only
//! ever moves a clock forward. So a stale entry, a stolen cell, a lost release, a reused thread
//! identifier, even a clock from an entirely different lock, can only put a thread further ahead
//! than it needed to be, and a thread further ahead reports fewer races. None of it can make a pair
//! that really was ordered look concurrent, because the chain of edges that ordered them carried
//! real clocks and is still there.
//!
//! What a full table costs is recall, and it costs it only for the keys that collided. That is the
//! same trade the thread numbers make when they run out and the same one the clock makes when it
//! stops.
//!
//! # What is not here
//!
//! A thread that leaves through `pthread_exit` rather than by returning. That unwinds the start
//! routine's frame from underneath, so the trampoline never gets to publish and whoever joins that
//! thread is left with no edge from it. A lost edge is lost reports, so it is a gap rather than a
//! wrong answer, but it is a common enough way to end a thread to be worth naming.
//!
//! The primitives one Unix has and another does not. `pthread_spin_lock`, `pthread_mutex_timedlock`
//! and `sem_timedwait` are all on Linux and none of them is on macOS, and a row is a symbol this
//! archive refers to on every target it is built for. Naming one that the C library there does not
//! define is a link failure in a program that never called it, so they wait for the table to grow a
//! way of saying which Unix a row is for.
//!
//! The barriers. `pthread_barrier_wait` is a release and an acquire on the barrier, which is the
//! shape `waits` already has, but it answers `PTHREAD_BARRIER_SERIAL_THREAD` to one of the threads
//! that met at it rather than zero to all of them, so it does not fit the group's rule that zero is
//! how a row spells success. It is not on macOS either.
//!
//! The atomics are not interposed, because there is nothing to interpose: a C11 `atomic_store` with
//! release ordering is an edge and it is a machine instruction rather than a call. The compiler
//! emits the edge instead, and what it emits lands in [`edges`] below and goes into the same two
//! tables under the same keys, so an ordering established through an atomic and one established
//! through a mutex are indistinguishable to everything that reads them.
//!
//! A bare `atomic_thread_fence` is still missing. It orders against every other thread rather than
//! against one object, so there is no address to key it on and the tables here are keyed by
//! address. A program that synchronizes through a fence and nothing else is one this can report
//! against wrongly, which is the direction that matters, and it is on the milestone.

use core::ffi::{c_int, c_void};
use core::sync::atomic::Ordering::{Acquire, Relaxed, Release};
use core::sync::atomic::{AtomicU64, AtomicUsize};

use crate::epoch::{self, Stamp};
use crate::interpose;

/// The C library's own, called once the edge has been taken or published.
mod real {
    use core::ffi::{c_int, c_void};

    use super::Start;

    unsafe extern "C" {
        pub(super) fn pthread_mutex_lock(mutex: *mut c_void) -> c_int;
        pub(super) fn pthread_mutex_trylock(mutex: *mut c_void) -> c_int;
        pub(super) fn pthread_mutex_unlock(mutex: *mut c_void) -> c_int;
        pub(super) fn pthread_rwlock_rdlock(lock: *mut c_void) -> c_int;
        pub(super) fn pthread_rwlock_wrlock(lock: *mut c_void) -> c_int;
        pub(super) fn pthread_rwlock_tryrdlock(lock: *mut c_void) -> c_int;
        pub(super) fn pthread_rwlock_trywrlock(lock: *mut c_void) -> c_int;
        pub(super) fn pthread_rwlock_unlock(lock: *mut c_void) -> c_int;
        pub(super) fn pthread_create(
            thread: *mut c_void,
            attr: *const c_void,
            start: Start,
            arg: *mut c_void,
        ) -> c_int;
        pub(super) fn pthread_join(thread: *mut c_void, value: *mut *mut c_void) -> c_int;
        pub(super) fn pthread_self() -> *mut c_void;
        pub(super) fn pthread_cond_wait(cond: *mut c_void, mutex: *mut c_void) -> c_int;
        pub(super) fn pthread_cond_timedwait(
            cond: *mut c_void,
            mutex: *mut c_void,
            until: *const c_void,
        ) -> c_int;
        pub(super) fn sem_wait(sem: *mut c_void) -> c_int;
        pub(super) fn sem_trywait(sem: *mut c_void) -> c_int;
        pub(super) fn sem_post(sem: *mut c_void) -> c_int;
    }
}

/// What a thread starts by running.
///
/// The C signature, spelled out here because the row for `pthread_create` takes one and hands back
/// another, which is the only way the edge can be in place before the program's own code runs.
pub type Start = unsafe extern "C" fn(*mut c_void) -> *mut c_void;

/// How many bits of a key the tables are indexed by.
const BITS: u32 = 10;

/// How many locks, or how many threads, can have a clock of their own at once.
///
/// A thousand of each, which is more live locks than almost any program has and sixteen kilobytes of
/// nothing for the ones that have none. What happens past it is in the module note: the edge for the
/// keys that collided is lost, and a lost edge is lost reports rather than invented ones.
pub const EDGES: usize = 1 << BITS;

/// How far a lookup walks before it decides the key is not in the table.
///
/// Short on purpose. This runs inside every lock and unlock in the program, so a walk that got
/// longer as the table filled would make the slowest programs the slowest to check. Eight cells is
/// one or two cache lines and it is a bound rather than an average.
const PROBES: usize = 8;

/// One key's published clock.
struct Edge {
    /// The lock's address or the thread's identifier, or zero for a cell nobody has taken.
    key: AtomicUsize,
    /// What the clock of whoever published under it was.
    stamp: AtomicU64,
}

impl Edge {
    /// A cell holding nothing.
    const fn new() -> Self {
        Self { key: AtomicUsize::new(0), stamp: AtomicU64::new(epoch::NONE) }
    }
}

/// A set of published clocks, keyed by a number that means something to whoever reads it.
///
/// Relaxed throughout, and it is enough for the same reason [`crate::epoch::Epochs`] gives: what a
/// stamp means is the number inside it and not the ordering of the load that found it. The edge that
/// matters is carried by the real primitive anyway. A relaxed store sequenced before a real unlock
/// is visible to a relaxed load sequenced after the matching real lock, because the C library's own
/// implementation is what establishes the happens-before between those two, and every other path
/// through here is one where a stale read costs a report and nothing else.
struct Table {
    /// The cells, in the order [`home`] indexes them.
    cells: [Edge; EDGES],
}

impl Table {
    /// A table nobody has published into.
    const fn new() -> Self {
        Self { cells: [const { Edge::new() }; EDGES] }
    }

    /// The cell `step` places along from where `key` would start.
    fn cell(&self, key: usize, step: usize) -> &Edge {
        &self.cells[(home(key) + step) % EDGES]
    }

    /// What the last publisher under `key` was at, or [`crate::epoch::NONE`].
    ///
    /// The walk stops at a cell nobody has taken. Cells are only ever taken and never given back,
    /// and a lookup takes the first free cell of the run, so a gap in the run means the key was
    /// never put in it. That is what makes the common case of a lock nothing has released one load
    /// rather than eight.
    fn published(&self, key: usize) -> Stamp {
        for step in 0..PROBES {
            let cell = self.cell(key, step);
            match cell.key.load(Relaxed) {
                0 => return epoch::NONE,
                held if held == key => return cell.stamp.load(Relaxed),
                _ => {}
            }
        }
        epoch::NONE
    }

    /// Writes `stamp` down as what whoever publishes under `key` is at.
    fn publish(&self, key: usize, stamp: Stamp) {
        for step in 0..PROBES {
            let cell = self.cell(key, step);
            let held = cell.key.load(Relaxed);
            if held == key
                || (held == 0 && cell.key.compare_exchange(0, key, Relaxed, Relaxed).is_ok())
            {
                cell.stamp.store(stamp, Relaxed);
                return;
            }
        }
        // Every cell of the run belongs to some other key. Take the first one over rather than
        // giving up on this one forever: whichever of the two loses its edge loses reports, and the
        // one that is still being published under is the one more likely to still matter.
        let cell = self.cell(key, 0);
        cell.key.store(key, Relaxed);
        cell.stamp.store(stamp, Relaxed);
    }
}

/// Every lock's published clock, for the whole process.
static PUBLISHED: Table = Table::new();

/// Every finished thread's last clock, keyed by the identifier a join is given.
static EXITED: Table = Table::new();

/// Where a key's clock would be kept if it has one.
///
/// Multiplicative, because the low bits of a lock's address say more about what the allocator did
/// than about which lock it is: a table of mutexes inside one structure is a run of addresses a
/// stride apart, and masking the low bits of those puts every one of them in the same few cells. A
/// `pthread_t` is the same shape of number for the same reason, since on this platform it is the
/// address of the thread's own descriptor.
fn home(key: usize) -> usize {
    ((key as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> (u64::BITS - BITS)) as usize
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
    PUBLISHED.publish(lock, stamp);
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
    epoch::sync(PUBLISHED.published(lock));
}

/// This thread has finished its start routine, so publish what it ended at.
///
/// Keyed by the identifier this thread answers to rather than by anything the creator wrote down,
/// because `pthread_create` fills in its output argument at a moment the new thread has no way to
/// wait for, and the value a join is handed is this same one.
pub fn exited() {
    // SAFETY: it takes nothing and reads none of this program's memory.
    let me = unsafe { real::pthread_self() } as usize;
    let stamp = epoch::here();
    if me == 0 || stamp == epoch::NONE {
        return;
    }
    EXITED.publish(me, stamp);
}

/// A join has just finished, so take everything the thread that ended did.
pub fn joined(thread: *mut c_void) {
    let thread = thread as usize;
    if thread == 0 {
        return;
    }
    epoch::sync(EXITED.published(thread));
}

/// How many threads can be starting at once and still be handed an edge.
///
/// A slot is held from the moment `pthread_create` is called to the moment the new thread has copied
/// three words out of it, which is microseconds, so this is a bound on threads being made at the
/// same instant rather than on threads. A program that beats it gets a thread with no edge from its
/// creator, which is the usual trade: fewer reports, never a wrong one.
const BIRTHS: usize = 64;

/// A slot nobody is using.
const EMPTY: usize = 0;
/// A slot a creator has claimed and is still writing into.
const FILLING: usize = 1;
/// A slot the thread it was filled in for can read.
const READY: usize = 2;

/// What one thread being created is handed.
struct Birth {
    /// Which of the three states above the slot is in.
    state: AtomicUsize,
    /// The start routine the program asked for, as a number.
    start: AtomicUsize,
    /// The argument the program asked for it to be given.
    arg: AtomicUsize,
    /// What the creator's clock was at the call.
    stamp: AtomicU64,
}

impl Birth {
    /// A slot nobody has claimed.
    const fn new() -> Self {
        Self {
            state: AtomicUsize::new(EMPTY),
            start: AtomicUsize::new(0),
            arg: AtomicUsize::new(0),
            stamp: AtomicU64::new(epoch::NONE),
        }
    }
}

/// Every thread that is between being asked for and starting.
static BEING_BORN: [Birth; BIRTHS] = [const { Birth::new() }; BIRTHS];

/// Takes a slot and fills it in, or answers `None` if every one of them is in use.
///
/// The claim is the compare exchange and the publication is the store of [`READY`], which is what
/// the new thread reads against. Both of them are ordered rather than relaxed: the words in between
/// are handed from one thread to another, and a slot being given back is handed the same way to
/// whichever creator claims it next.
fn claim(start: Start, arg: *mut c_void, stamp: Stamp) -> Option<usize> {
    for (which, slot) in BEING_BORN.iter().enumerate() {
        if slot.state.compare_exchange(EMPTY, FILLING, Acquire, Relaxed).is_ok() {
            slot.start.store(start as usize, Relaxed);
            slot.arg.store(arg as usize, Relaxed);
            slot.stamp.store(stamp, Relaxed);
            slot.state.store(READY, Release);
            return Some(which);
        }
    }
    None
}

/// What a thread this monitor created really starts at.
///
/// Three things in order: take the creator's clock, give the slot back, then run what the program
/// actually asked for. Publishing the exit clock on the way out is the other half of the join edge,
/// and it is here rather than in the join because the thread is the only one that knows what it
/// ended at.
///
/// # Safety
///
/// Called by the C library with the slot number this crate handed it, and by nobody else.
unsafe extern "C" fn born(slot: *mut c_void) -> *mut c_void {
    let slot = &BEING_BORN[slot as usize];
    // The creator wrote the three words and stored `READY` before it asked for this thread, and
    // asking for this thread is what made it, so the state is already there and this reads it
    // rather than waiting for it. The loop is what makes that reasoning unnecessary.
    while slot.state.load(Acquire) != READY {
        core::hint::spin_loop();
    }
    let start = slot.start.load(Relaxed);
    let arg = slot.arg.load(Relaxed) as *mut c_void;
    let stamp = slot.stamp.load(Relaxed);
    slot.state.store(EMPTY, Release);

    epoch::sync(stamp);
    // SAFETY: the number is a `Start` that `claim` was given one call ago and wrote down as a
    // number, because an atomic cell holds a number and a function pointer is one.
    let start: Start = unsafe { core::mem::transmute::<usize, Start>(start) };
    // SAFETY: the program asked for this function to be run with this argument on a thread of its
    // own, and this is that thread.
    let done = unsafe { start(arg) };
    exited();
    done
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

    /// `pthread_create`, which is the edge no lock in the program can stand in for.
    ///
    /// A program that fills a buffer and hands it to a worker has locked nothing and raced nothing,
    /// and without this row the buffer it filled is concurrent with every read the worker makes of
    /// it for the rest of the run. So the thread starts at `born` instead, which moves the new
    /// thread's clock past its creator's before the program's own start routine is entered.
    ///
    /// Every failure here is the same failure: run the thread the program asked for with no edge on
    /// it. That is what happens when no birth slot is free, and it is what the slot being given
    /// back on a create that failed keeps from becoming permanent.
    fn pthread_create(
        thread: *mut c_void,
        attr: *const c_void,
        start: Start,
        arg: *mut c_void,
    ) -> c_int
        where spawns(start)
    {
        let Some(slot) = claim(start, arg, epoch::here()) else {
            // SAFETY: every argument is the program's own, passed on untouched.
            return unsafe { real::pthread_create(thread, attr, start, arg) };
        };
        // SAFETY: as above, except that the thread is asked to start inside this crate, at a
        // function whose contract is the slot number it is being given here.
        let made = unsafe { real::pthread_create(thread, attr, born, slot as *mut c_void) };
        if made != 0 {
            // Nothing will ever read it, so hand it back rather than leaking a slot per failed
            // create until a long running program has none left.
            BEING_BORN[slot].state.store(EMPTY, Release);
        }
        made
    }

    /// `pthread_join`, which is the same edge pointing the other way.
    ///
    /// Everything the thread did is ordered before everything the joiner does next, which is what
    /// makes reading a worker's results after joining it not a race. The edge is taken only when
    /// the join really worked: a join that came back `EINVAL` because somebody else was already
    /// waiting on that thread was told nothing about whether it has finished.
    ///
    /// The identifier is spelled as a pointer because that is the shape it is passed in. A
    /// `pthread_t` is a word, and both of this compiler's targets hand a word and a pointer over in
    /// the same register.
    fn pthread_join(thread: *mut c_void, value: *mut *mut c_void) -> c_int
        where joins(thread)
    {
        // SAFETY: both arguments are the program's own, passed on untouched.
        unsafe { real::pthread_join(thread, value) }
    }

    /// `pthread_cond_wait`, which gives the caller's mutex up and takes it back without either of
    /// those going through the rows above.
    ///
    /// That is why it needs a row of its own rather than being covered by the lock the program
    /// wrote around it. The unlock is inside the call, so a thread that signalled under the mutex
    /// and released it publishes a clock the waiter would never read, and the waiter would come
    /// back holding the mutex and standing exactly where it was before it slept. Every read it then
    /// makes of what it was signalled about is a thread reading another thread's work with nothing
    /// between them.
    ///
    /// The edge is taken whether the call worked or not, which is the difference from `acquires`.
    /// POSIX says the mutex is held again when this returns however it returns, so a timed wait
    /// that came back `ETIMEDOUT` is still a thread holding a lock somebody else gave up.
    fn pthread_cond_wait(cond: *mut c_void, mutex: *mut c_void) -> c_int
        where waits(mutex)
    {
        // SAFETY: both pointers are the program's own and neither is read through here.
        unsafe { real::pthread_cond_wait(cond, mutex) }
    }

    /// `pthread_cond_timedwait`, which is the same row with a deadline on it.
    fn pthread_cond_timedwait(cond: *mut c_void, mutex: *mut c_void, until: *const c_void) -> c_int
        where waits(mutex)
    {
        // SAFETY: as `pthread_cond_wait`, and the deadline is read by the C library alone.
        unsafe { real::pthread_cond_timedwait(cond, mutex, until) }
    }

    /// `sem_wait`, which is the lock edge under another name.
    ///
    /// A semaphore is the other half of the producer and consumer pair a condition variable makes,
    /// and it carries an ordering for the same reason: whoever posted really did post before
    /// whoever waited came back, so everything the poster did first is ordered before everything
    /// the waiter does next.
    fn sem_wait(sem: *mut c_void) -> c_int
        where acquires(sem)
    {
        // SAFETY: the pointer is the program's own and is never read through here.
        unsafe { real::sem_wait(sem) }
    }

    /// `sem_trywait`, which is `sem_wait` when there was something to take.
    fn sem_trywait(sem: *mut c_void) -> c_int
        where acquires(sem)
    {
        // SAFETY: as `sem_wait`.
        unsafe { real::sem_trywait(sem) }
    }

    /// `sem_post`, which publishes the way an unlock does.
    fn sem_post(sem: *mut c_void) -> c_int
        where releases(sem)
    {
        // SAFETY: as `sem_wait`.
        unsafe { real::sem_post(sem) }
    }
}

/// What generated code calls, which is the one edge that is not a call in the program itself.
///
/// Everything else in this module is an interposed `pthread` function, so the program calls it by
/// name and the wrapper is what the linker resolves. A C11 atomic is a machine instruction, so
/// there is nothing to interpose and the compiler emits these instead: `rucc_safety`'s `edges` puts
/// a release in front of an atomic that publishes and an acquire after one that takes, keyed on the
/// atomic object's own address. That is the same key a mutex uses, and deliberately, so that a
/// program synchronizing through both is one table and one clock rather than two of each.
///
/// Its own module rather than the `exports` beside it, because that one is written by the
/// interposition macro out of the table above and these two are not rows: there is no C library
/// function here to wrap and no return value to look at.
pub mod edges {
    use core::ffi::c_void;

    /// Publishes this thread's clock at `object`, before the atomic that carries the edge runs.
    ///
    /// # Safety
    ///
    /// `object` is whatever address the program computed for the atomic and is never read through.
    /// Nothing here dereferences it: it is hashed and used as a key.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_meta_release(object: *mut c_void) {
        super::released(object);
    }

    /// Takes whatever was published at `object`, after the atomic that carries the edge has run.
    ///
    /// # Safety
    ///
    /// As [`__rucc_meta_release`]. The address is a key and is never read through.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn __rucc_meta_acquire(object: *mut c_void) {
        super::acquired(object);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::Group;
    use crate::epoch::{clock, thread, unordered};

    /// A key nothing dereferences.
    ///
    /// The tables are keyed by a number and never read through it, which is the property that lets
    /// these tests use numbers rather than real mutexes and stay out of the way of whatever the
    /// harness's own locks are doing. Each test uses its own, because a table is one table for the
    /// whole process and tests run beside each other.
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
        let published = PUBLISHED.published(lock as usize);

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
        assert_eq!(PUBLISHED.published(lock as usize), epoch::NONE);
    }

    #[test]
    fn a_cell_taken_over_by_another_lock_answers_nothing_rather_than_somebody_elses_clock() {
        // The table filling up, which is the case the module note argues is safe. What it must not
        // do is hand one lock's clock out under another lock's name often enough to matter, and
        // what it does instead is answer that it knows nothing, which loses a report.
        //
        // On a table of its own rather than the process wide one. Filling that one would leave
        // every cell taken for the rest of the run, which is a state the other tests in this file
        // would then be sharing with whichever of them happened to go first.
        let table = Table::new();
        for which in 0..EDGES * 4 {
            table.publish(fake(0x1000 + which) as usize, epoch::stamp(1, which as u64 + 1));
        }
        let mut found = 0;
        for which in 0..EDGES * 4 {
            let answer = table.published(fake(0x1000 + which) as usize);
            if answer == epoch::NONE {
                continue;
            }
            assert_eq!(answer, epoch::stamp(1, which as u64 + 1), "another lock's clock came back");
            found += 1;
        }
        assert!(found >= EDGES / 2, "and a table this full still answers about {found} of them");
    }

    #[test]
    fn a_lock_is_not_its_neighbour() {
        // Two mutexes a stride apart inside one structure is the commonest arrangement there is,
        // and an index that used the low bits of the address would put a whole array of them in one
        // cell. Every one of these has to keep its own clock.
        let table = Table::new();
        let stamps: std::vec::Vec<Stamp> = (0..16)
            .map(|which| {
                let stamp = epoch::tick();
                table.publish(fake(0x200 + which) as usize, stamp);
                stamp
            })
            .collect();

        for (which, stamp) in stamps.iter().enumerate() {
            assert_eq!(table.published(fake(0x200 + which) as usize), *stamp, "lock {which}");
        }
    }

    #[test]
    fn a_null_lock_is_left_alone() {
        // A program that passes null to `pthread_mutex_lock` has a bug the C library gets to
        // report. Zero is also how the table spells a cell nobody has taken, so letting one through
        // would key an entry under the word that means empty.
        let before = epoch::here();
        released(core::ptr::null_mut());
        acquired(core::ptr::null_mut());
        joined(core::ptr::null_mut());
        assert_eq!(epoch::here(), before);
    }

    /// What the thread in [`a_thread_starts_past_everything_its_creator_had_done`] reached.
    static STARTED: AtomicU64 = AtomicU64::new(epoch::NONE);

    /// A start routine that does one unit of work and says where it was when it did.
    ///
    /// # Safety
    ///
    /// Takes no argument and reads none, so there is nothing for a caller to get wrong.
    unsafe extern "C" fn notes(_: *mut c_void) -> *mut c_void {
        STARTED.store(epoch::tick(), Relaxed);
        core::ptr::null_mut()
    }

    #[test]
    fn a_thread_starts_past_everything_its_creator_had_done() {
        // A worker being handed a filled buffer, which is the case no lock in the program covers.
        // The child has to come out of the gate already behind its creator or every byte the
        // creator wrote before the call reads as concurrent with the worker's first look at it.
        let _turn = crate::turnstile::turn();
        let mine = epoch::tick();
        let mut id: usize = 0;
        // SAFETY: a `pthread_t` is one word, the start routine takes nothing, and the attributes
        // being null asks for the default thread.
        let made = unsafe {
            pthread_create((&raw mut id).cast(), core::ptr::null(), notes, core::ptr::null_mut())
        };
        assert_eq!(made, 0, "the thread was made");
        // SAFETY: the identifier is the one the call above just wrote, and nothing else has joined
        // this thread.
        let done = unsafe { pthread_join(id as *mut c_void, core::ptr::null_mut()) };
        assert_eq!(done, 0, "and it was joined");

        let started = STARTED.load(Relaxed);
        assert_ne!(thread(started), thread(mine), "two threads, not one");
        assert!(!unordered(mine, started), "the creator's work is behind the child's");
    }

    /// What the thread in [`joining_a_thread_gets_past_everything_it_did`] ended at.
    static ENDED: AtomicU64 = AtomicU64::new(epoch::NONE);

    /// A start routine whose last act is the one the joiner must end up behind.
    ///
    /// # Safety
    ///
    /// As [`notes`].
    unsafe extern "C" fn works(_: *mut c_void) -> *mut c_void {
        let _ = epoch::tick();
        ENDED.store(epoch::tick(), Relaxed);
        core::ptr::null_mut()
    }

    #[test]
    fn joining_a_thread_gets_past_everything_it_did() {
        // The other direction, and the one that makes reading a worker's results legal. The stamp
        // taken before the join is what the comparison is against, because a stamp read afterwards
        // would have the edge in it already and would prove nothing.
        let _turn = crate::turnstile::turn();
        let mut id: usize = 0;
        let before = epoch::here();
        // SAFETY: as the test above.
        let made = unsafe {
            pthread_create((&raw mut id).cast(), core::ptr::null(), works, core::ptr::null_mut())
        };
        assert_eq!(made, 0, "the thread was made");
        // SAFETY: as the test above.
        let done = unsafe { pthread_join(id as *mut c_void, core::ptr::null_mut()) };
        assert_eq!(done, 0, "and it was joined");

        let ended = ENDED.load(Relaxed);
        assert!(unordered(ended, before), "before the join it was concurrent");
        assert!(!unordered(ended, epoch::here()), "and after the join it is not");
    }

    /// The C library calls these tests make that no row here wraps.
    mod libc {
        use core::ffi::{c_int, c_void};

        unsafe extern "C" {
            pub(super) fn pthread_mutex_init(mutex: *mut c_void, attr: *const c_void) -> c_int;
            pub(super) fn pthread_mutex_destroy(mutex: *mut c_void) -> c_int;
            pub(super) fn pthread_cond_init(cond: *mut c_void, attr: *const c_void) -> c_int;
            pub(super) fn pthread_cond_signal(cond: *mut c_void) -> c_int;
            pub(super) fn pthread_cond_destroy(cond: *mut c_void) -> c_int;
        }
    }

    /// Where the signalling thread stood when it did.
    static SIGNALLED: AtomicU64 = AtomicU64::new(epoch::NONE);

    /// Whether it has signalled, which is the thing the wait is really waiting for.
    static TOLD: AtomicUsize = AtomicUsize::new(0);

    #[test]
    fn waiting_on_a_condition_takes_the_edge_its_mutex_carries() {
        // A real condition variable and a real mutex, because the whole point of the row is that
        // the unlock and the relock are inside the call and so are invisible to the lock rows. A
        // test that stood in for them would be a test of the generator rather than of the edge.
        let _turn = crate::turnstile::turn();
        let mut storage = std::vec![0u64; 32];
        let (mutex, cond) = storage.split_at_mut(16);
        let mutex: *mut c_void = mutex.as_mut_ptr().cast();
        let cond: *mut c_void = cond.as_mut_ptr().cast();
        // SAFETY: both point at enough zeroed, aligned storage for what the C library keeps in
        // them on either of the two Unixes, and the attributes being null asks for the default.
        unsafe {
            assert_eq!(libc::pthread_mutex_init(mutex, core::ptr::null()), 0);
            assert_eq!(libc::pthread_cond_init(cond, core::ptr::null()), 0);
        }

        // SAFETY: the mutex above, taken through the row that carries the edge.
        unsafe { assert_eq!(pthread_mutex_lock(mutex), 0) };
        // The addresses travel as numbers, because a raw pointer is not `Send`. Both are the
        // caller's stack and outlive the thread, which is joined below.
        let travelling = (mutex as usize, cond as usize);
        let signaller = std::thread::spawn(move || {
            let (mutex, cond) = (travelling.0 as *mut c_void, travelling.1 as *mut c_void);
            // SAFETY: the same two objects, still alive for as long as this thread runs.
            unsafe {
                assert_eq!(pthread_mutex_lock(mutex), 0);
                SIGNALLED.store(epoch::tick(), Relaxed);
                TOLD.store(1, Relaxed);
                assert_eq!(libc::pthread_cond_signal(cond), 0);
                assert_eq!(pthread_mutex_unlock(mutex), 0);
            }
        });

        let before = epoch::here();
        while TOLD.load(Relaxed) == 0 {
            // SAFETY: this thread holds the mutex, which is what the call requires of it.
            unsafe { assert_eq!(pthread_cond_wait(cond, mutex), 0) };
        }
        let theirs = SIGNALLED.load(Relaxed);
        assert!(unordered(theirs, before), "before the wait the signal was concurrent");
        assert!(!unordered(theirs, epoch::here()), "and coming back from it took the edge");

        // SAFETY: this thread holds the mutex and nothing is waiting on either object once the
        // thread below has been joined.
        unsafe { assert_eq!(pthread_mutex_unlock(mutex), 0) };
        signaller.join().expect("the thread ran");
        // SAFETY: as above.
        unsafe {
            assert_eq!(libc::pthread_cond_destroy(cond), 0);
            assert_eq!(libc::pthread_mutex_destroy(mutex), 0);
        }
    }

    #[test]
    fn a_thread_this_never_saw_finish_carries_nothing() {
        // A thread created before this library was in the picture, or one that left through
        // `pthread_exit`. There is no clock under its name, so the join has to move nothing rather
        // than take whatever the cell it hashes to happens to hold.
        let before = epoch::here();
        joined(fake(0x40));
        assert_eq!(epoch::here(), before);
        assert_eq!(EXITED.published(fake(0x40) as usize), epoch::NONE);
    }

    #[test]
    fn a_birth_slot_is_given_back_once_the_thread_has_read_it() {
        // A pool that leaked a slot per thread would stop carrying the edge after sixty four
        // threads, which is a detector that quietly switches itself off partway through a run.
        let _turn = crate::turnstile::turn();
        let taken: std::vec::Vec<usize> =
            core::iter::from_fn(|| claim(notes, core::ptr::null_mut(), epoch::here())).collect();
        assert_eq!(taken.len(), BIRTHS, "every slot, and then no more");
        assert!(claim(notes, core::ptr::null_mut(), epoch::here()).is_none());

        for slot in taken {
            BEING_BORN[slot].state.store(EMPTY, Release);
        }
        let again = claim(notes, core::ptr::null_mut(), epoch::here()).expect("a free slot");
        BEING_BORN[again].state.store(EMPTY, Release);
    }

    #[test]
    fn every_row_is_in_the_ordering_group_and_none_of_them_touches_a_range() {
        // The rows as data, which is what `--emit=safety-summary` counts and what `cargo xtask
        // interpose` reads. An ordering row with an effects clause would be a row in the wrong
        // group, since a range judgement here would be a claim about memory this never looks at.
        assert_eq!(TABLE.len(), 15);
        for row in TABLE {
            assert_eq!(row.group, Group::Ordering, "{} is in the wrong group", row.name);
            assert!(row.effects.is_empty(), "{} claims a range", row.name);
            assert!(row.wrapper.starts_with("__rucc_wrap_"), "{} has the wrong symbol", row.name);
        }
    }
}
