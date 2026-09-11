//! The epoch plane: which thread last wrote a word, and when in its own counting.
//!
//! Design: `spec/safe-memory/09-type-init-and-races.md` section 9.5, and the layout in
//! `spec/safe-memory/05-representation.md` section 5.2.3.
//!
//! Eight bytes of shadow per eight bytes of program, holding a thread number and a clock. That
//! buys document 03's C1 through C4, which are the races that produce a wrong pointer rather than
//! a wrong number: a pointer word paired with somebody else's capability, two threads writing one
//! aux slot with nothing between them, and a use after free where the report can name both the
//! thread that freed and the thread that read.
//!
//! # What this detector is, said plainly
//!
//! It is happens before free, has no false positives, and is incomplete. There are no vector
//! clocks here and nothing reconstructs the happens before relation. It reports the races that
//! actually happened in the interleaving that actually ran, and it says nothing about the ones a
//! different schedule would have produced. That is strictly weaker than ThreadSanitizer, and it is
//! the trade section 9.5 makes on purpose: TSan costs five to fifteen times the time and five to
//! ten times the memory, which is why it is a testing tool, and this costs one load from a line
//! that is already being read and one compare, which is why it can be on in production.
//!
//! # The clock is Lamport's and that is the whole of the ordering
//!
//! Each thread counts its own metadata stores. Acquiring a lock takes the larger of the thread's
//! own count and whatever the lock was released at, and that is the shape of every edge between
//! threads there is: whoever hands something over publishes the count they handed it over at, and
//! whoever takes it moves past that. Document 10's interposed synchronization primitives are where
//! the edges come from and [`crate::sync`] is the table of them.
//!
//! Which makes the completeness of that table a different sort of thing here than anywhere else in
//! this crate. Every other plane loses a report when a piece of instrumentation is missing. This one
//! invents one, because two threads a missing edge really did join look exactly like two threads
//! nothing joined. That is why the edges went in before this file grew a reader and why the atomics,
//! which are not a call and so cannot be interposed, are a stated gate on turning one on.
//!
//! What that buys is one implication and not the other. If one write happened before another then
//! its clock is smaller, so a clock that is not smaller means the two are not ordered, and that is
//! the direction [`unordered`] tests. A smaller clock does not mean the two were ordered, so a pair
//! this calls ordered may have been concurrent, and that is a race nobody reports. Missing a report
//! is the only direction available to a detector whose false positive rate is a release blocking
//! property, and it is the same trade [`crate::init`] makes from the other end.
//!
//! # What is here so far
//!
//! The stamp, the clock and the plane's arithmetic, with the shadow handed in, which is the
//! division [`crate::plane`] explains and the other two planes follow. The plane is mapped over
//! every watched region beside the other three, an instance forgets its stamps when it begins, each
//! thread has a clock of its own in the slot [`crate::tls`] keeps, and
//! [`crate::check::stamped`] is the judgement a store through a pointer shaped slot makes.
//!
//! The reader is here now as well. [`Epochs::stranger`] is the walk that finds a stamp nothing this
//! thread has done orders, and [`crate::check::raced`] is the check that turns one into a report,
//! which is judgement J9 and is document 03's C2 and C3. C4 is in too, and it is not a check of its
//! own: an instance ending stamps its bytes with the freeing thread, so the refusal
//! [`crate::check::live`] already made for a use after free can say the free was another thread's
//! and nothing ordered it against the access. What is left of the four classes is C1, the torn
//! store, which [`torn`] is the arithmetic of and which nothing can call yet: it compares a pointer
//! word's stamp against the stamp its aux slot was written at, and the aux slot is the part of
//! milestone S5 that does not exist.
//!
//! The edges are all in. [`sync`] is what an interposed primitive calls and [`crate::sync`] is the
//! table of them, so a lock given up and taken, a thread created, a thread joined, a condition
//! variable waited on and a semaphore posted each carry an ordering. That matters more here than the
//! coverage of anything else in the crate does, for the reason the section above this one gives.
//!
//! The compiler's half is in. `-fsafety-races` puts a `__rucc_meta_epoch` after every store of a
//! pointer and a `__rucc_check_race` in front of it, and `=pointer` puts one in front of a read of a
//! pointer too, so the plane a program runs with holds what that program wrote and is asked about it
//! where the program can act on the answer. The ordering that is not a call at all is in with it: an
//! atomic that publishes gets a `__rucc_meta_release` in front of it and one that takes gets a
//! `__rucc_meta_acquire` after it, keyed on the atomic object, which is the last edge that had
//! nowhere to be interposed. A bare `atomic_thread_fence` gets the same pair with no key, because
//! it orders against every thread rather than against an object, and the runtime keeps one clock
//! for all of them.

#[cfg(unix)]
use core::sync::atomic::AtomicBool;
use core::sync::atomic::{AtomicU64, Ordering};

/// A thread number and that thread's own count of its metadata stores, in sixty four bits.
///
/// One word rather than two fields, because it travels in an aux slot and in a shadow slot and
/// both of those are read on the path an access takes. Sixteen bits of thread and forty eight of
/// clock, which [`THREADS`] and [`CLOCKS`] are the two ends of.
pub type Stamp = u64;

/// The stamp of a word nothing has been seen to write.
///
/// A fresh shadow reservation reads as zero, so untouched address space already spells this and
/// nothing walks it to say so. No real stamp is zero, because thread numbers start at one.
pub const NONE: Stamp = 0;

/// How many bits of a stamp are the clock.
const CLOCK_BITS: u32 = 48;

/// The largest clock a thread can reach.
///
/// Two hundred and eighty one trillion metadata stores, which at one a nanosecond is eight and a
/// half years of a thread doing nothing else. A thread that reaches it stops counting rather than
/// wrapping, for the reason [`Clock::tick`] gives.
pub const CLOCKS: u64 = (1 << CLOCK_BITS) - 1;

/// How many threads can be told apart at once.
///
/// Sixty five thousand and a bit, and what happens when a program has made more of them than that
/// is in [`Threads::next`]: the numbers stop going up and the threads past the end share the last
/// one. Two threads sharing a number look like one thread to [`unordered`], so what that costs is
/// reports rather than correctness.
pub const THREADS: u64 = (1 << (64 - CLOCK_BITS)) - 1;

/// A stamp saying `thread` wrote this at its own `clock`.
///
/// Out of range arguments are clamped rather than refused. This is called on the path a store
/// takes and there is nothing useful to do about a number that is too large except carry on with
/// the largest one there is, which is the same answer [`Clock::tick`] arrives at from the other
/// side.
#[must_use]
pub const fn stamp(thread: u64, clock: u64) -> Stamp {
    let thread = if thread > THREADS { THREADS } else { thread };
    let clock = if clock > CLOCKS { CLOCKS } else { clock };
    (thread << CLOCK_BITS) | clock
}

/// Which thread a stamp says wrote the word.
#[must_use]
pub const fn thread(of: Stamp) -> u64 {
    of >> CLOCK_BITS
}

/// What that thread's clock was when it did.
#[must_use]
pub const fn clock(of: Stamp) -> u64 {
    of & CLOCKS
}

/// Whether nothing a reader has done orders what `found` records.
///
/// The test section 9.5 describes, which is a store or a load finding an epoch from another thread
/// that is not older than its own. It is one comparison and it is the whole of the race detection.
///
/// Both directions of the answer are worth being clear about. A stamp from the asking thread's own
/// number is ordered by that thread having written it, and a stamp whose clock is behind the
/// asker's is taken as ordered, which is where the incompleteness lives: a Lamport clock says that
/// an ordered pair has an increasing clock and does not say that an increasing clock is an ordered
/// pair. [`NONE`] is nothing rather than something concurrent, because a word this plane has not
/// watched is a word it has nothing to say about.
#[must_use]
pub const fn unordered(found: Stamp, mine: Stamp) -> bool {
    found != NONE && thread(found) != thread(mine) && clock(found) >= clock(mine)
}

/// Whether a pointer word and the aux slot beside it came from different stores.
///
/// Judgement C1, the torn store, and it is an equality rather than an ordering. An aux slot holds
/// the stamp of the pointer word it was written with, so the two agreeing means one store wrote
/// both halves and the two disagreeing means a reader has a pointer from one store and a capability
/// from another. Fil-C accepts that pairing and is memory safe under it, because the capability is
/// a real one with real bounds. It is still the program following a pointer to an object it never
/// had a pointer to, which is a wrong answer its author would want to know about.
///
/// [`NONE`] on either side is not a tear. It is the plane not having watched one of the two halves,
/// and a monitor that reported on what it did not watch would be reporting on correct programs.
#[must_use]
pub const fn torn(word: Stamp, paired: Stamp) -> bool {
    word != NONE && paired != NONE && word != paired
}

/// Thread numbers, handed out once per thread and never taken back.
///
/// Never taken back because taking one back needs a hook on the thread ending, and the thread local
/// of [`crate::tls`] is a `pthread` key with no destructor on it for the reason written down there.
/// So this counts threads made rather than threads running, and a program that makes and joins them
/// in a loop walks through the range. What happens at the end of the range is in [`Threads::next`],
/// and it is a loss of reports rather than of soundness, which is what makes counting acceptable
/// until there is somewhere to put a destructor.
#[derive(Debug)]
pub struct Threads(AtomicU64);

impl Threads {
    /// A source whose first number is 1, because 0 is the thread of [`NONE`].
    #[must_use]
    pub const fn new() -> Self {
        Self(AtomicU64::new(1))
    }

    /// This thread's number.
    ///
    /// Every thread past [`THREADS`] gets that number, so they are all one thread as far as
    /// [`unordered`] can tell and a race between two of them is not reported. That is the right
    /// failure: the alternative is to start the numbers again, and a recycled number makes two
    /// threads that are running at the same time look like one, which loses the same reports and
    /// also makes a stale stamp from a dead thread look like a live one.
    ///
    /// Relaxed is enough. What is asked of this is that two threads do not get one number, and the
    /// ordering that makes a stamp mean anything is carried by the clock rather than by this.
    pub fn next(&self) -> u64 {
        let taken = self.0.fetch_add(1, Ordering::Relaxed);
        if taken > THREADS {
            // Put it back, so a program that makes threads forever does not wrap the counter round
            // to zero and start handing out the number that means nobody.
            self.0.store(THREADS + 1, Ordering::Relaxed);
            THREADS
        } else {
            taken
        }
    }
}

impl Default for Threads {
    fn default() -> Self {
        Self::new()
    }
}

/// One thread's Lamport clock, and the number it stamps with.
///
/// Not shared and not atomic. A thread's clock is read and written by that thread alone, which is
/// what makes the counting free, and the only thing another thread ever sees of it is a stamp that
/// has already been published into a slot.
#[derive(Clone, Copy, Debug)]
pub struct Clock {
    /// This thread's number, from [`Threads::next`].
    thread: u64,
    /// How many metadata stores it has counted.
    count: u64,
}

impl Clock {
    /// A clock for thread `thread`, at the start of its counting.
    #[must_use]
    pub const fn new(thread: u64) -> Self {
        Self { thread, count: 1 }
    }

    /// The stamp to write, counting this store, or [`NONE`] once the counting has stopped.
    ///
    /// Stopping rather than wrapping, and the difference matters. A clock that wrapped would put a
    /// store that happened later behind one that happened earlier, which is a pair [`unordered`]
    /// calls ordered from one side and concurrent from the other, so it would invent reports about
    /// programs with no race in them.
    ///
    /// A clock that stops has to stop stamping as well, which is why this answers [`NONE`] rather
    /// than the last stamp over and over. [`unordered`] calls an equal clock concurrent, so a thread
    /// parked at [`CLOCKS`] that went on stamping would have every word it wrote read as concurrent
    /// with every reader of it, ordered or not, which is the false positive this whole file is
    /// arranged to avoid. Saying nothing instead means a program that gets here stops being watched,
    /// which loses reports and invents none.
    ///
    /// Getting here takes two hundred and eighty one trillion metadata stores on one thread. This is
    /// written down because the degradation has to be in the right direction even where nobody will
    /// see it, not because anybody will.
    pub fn tick(&mut self) -> Stamp {
        if self.count >= CLOCKS {
            return NONE;
        }
        self.count += 1;
        stamp(self.thread, self.count)
    }

    /// The stamp this thread would write without counting anything.
    #[must_use]
    pub const fn now(&self) -> Stamp {
        stamp(self.thread, self.count)
    }

    /// Takes the ordering an acquired lock carries.
    ///
    /// The one edge between threads there is. Whoever released the lock published the clock they
    /// released it at, and everything they did before that is now ordered before everything this
    /// thread does after it.
    ///
    /// One past what was seen, which is Lamport's own rule for receiving and is not a rounding
    /// choice. [`unordered`] calls an equal clock concurrent, because two threads that reached the
    /// same count without meeting really are, so landing exactly on the releaser's count would leave
    /// that thread's last store looking concurrent with everything this one does next. That is a
    /// report about a program whose locking is correct, which is the one thing this detector is not
    /// allowed to produce.
    ///
    /// Seeing [`NONE`] is not an edge and moves nothing. A lock nobody has published under carries
    /// no ordering at all, and counting one for it would advance a brand new thread's clock off
    /// zero for every lock it ever takes that nobody had released.
    pub fn sync(&mut self, seen: Stamp) {
        if seen == NONE {
            return;
        }
        self.count = self.count.max(clock(seen).saturating_add(1)).min(CLOCKS);
    }
}

/// Where thread numbers come from, for the whole process.
#[cfg(unix)]
static NUMBERS: Threads = Threads::new();

/// Each thread's own clock, as the stamp it would write next.
///
/// The stamp is the whole of a [`Clock`], so what is kept per thread is the stamp itself and not a
/// pointer to one. That is why this can live in the slot [`crate::tls`] already has: nothing is
/// allocated, nothing has to outlive anything, and reading the clock is the one call that module
/// says it is.
#[cfg(unix)]
static CLOCK: crate::tls::Slot = crate::tls::Slot::new();

/// The stamp travels in a slot the size of a pointer, which is what makes keeping it free.
///
/// A target whose pointer is narrower would lose the top of the thread number and would start
/// calling two threads one, so it is refused here rather than at the point where the reports go
/// quietly missing. Nothing else in this crate would work on such a target either: a granule is
/// eight bytes because that is a pointer.
#[cfg(unix)]
const _: () = assert!(
    size_of::<usize>() >= size_of::<Stamp>(),
    "the epoch plane wants a pointer at least as wide as a stamp"
);

/// Whether the thread local turned out to keep nothing, so that nobody asks it twice.
///
/// A process out of `pthread` keys, which [`crate::tls`] degrades to an empty slot for. Without
/// this latch every call would find the slot empty, take a fresh thread number and look like a new
/// thread, which walks through the range in a few thousand calls and leaves behind stamps that are
/// each other's strangers.
#[cfg(unix)]
static KEYLESS: AtomicBool = AtomicBool::new(false);

/// This thread's clock, made on first use, or `None` when there is nowhere to keep one.
///
/// A thread that has not asked before takes a number here, which is the only place numbers are
/// handed out.
///
/// `None` is the whole degradation for a process with no thread local, and the callers turn it into
/// [`NONE`], which is a word this plane says nothing about. That is the right direction and it is
/// the one this file takes everywhere else: the alternative is stamps whose thread numbers mean
/// nothing, which would report on programs with no race in them.
#[cfg(unix)]
fn mine() -> Option<Clock> {
    if KEYLESS.load(Ordering::Relaxed) {
        return None;
    }
    let held = CLOCK.get() as usize as Stamp;
    if held != NONE {
        return Some(Clock { thread: thread(held), count: clock(held) });
    }
    let fresh = Clock::new(NUMBERS.next());
    keep(fresh);
    if CLOCK.get() as usize as Stamp == NONE {
        KEYLESS.store(true, Ordering::Relaxed);
        return None;
    }
    Some(fresh)
}

/// Writes `clock` back as this thread's.
#[cfg(unix)]
fn keep(clock: Clock) {
    // SAFETY: the slot holds a stamp rather than an address and nothing reads through what is
    // stored, so there is nothing for it to outlive.
    unsafe { CLOCK.set(clock.now() as usize as *mut core::ffi::c_void) };
}

/// The stamp this thread should write for a metadata store it is making.
///
/// The counting half of every judgement that records something, and the reason a store goes through
/// one function rather than reading the clock and writing the plane as two separate things.
#[cfg(unix)]
pub fn tick() -> Stamp {
    let Some(mut clock) = mine() else { return NONE };
    let stamp = clock.tick();
    keep(clock);
    stamp
}

/// The stamp this thread stands at, for a read that wants to compare rather than to record.
#[cfg(unix)]
#[must_use]
pub fn here() -> Stamp {
    mine().map_or(NONE, |clock| clock.now())
}

/// Takes the ordering an acquired lock carries, for this thread.
///
/// The process wide half of [`Clock::sync`], and what [`crate::sync`] calls once a lock has really
/// been taken. A thread with nowhere to keep a clock takes no edge, which is consistent with it
/// stamping nothing: there is no clock for an ordering to be against.
#[cfg(unix)]
pub fn sync(seen: Stamp) {
    let Some(mut clock) = mine() else { return };
    clock.sync(seen);
    keep(clock);
}

/// How many bytes of program memory one stamp covers.
///
/// Eight, which is the size of a pointer on both of the parent's sixty four bit targets and so is
/// the unit a pointer word comes in. A granule that held two pointers could not say which of them
/// was written last, and one that held half of one would need two slots read to answer about it.
pub const GRANULE: usize = 8;

/// How many bytes of shadow one granule needs.
pub const SLOT: usize = size_of::<Stamp>();

/// The direct mapped shadow the plane lives in.
///
/// `origin` is a bias rather than an address and works the way [`crate::plane::Lifetime`]'s does,
/// for the reason written down there: the shadow for a region high in the address space sits below
/// the bias by more than the bias is, so the value that makes the arithmetic come out right has
/// wrapped.
#[derive(Clone, Copy, Debug)]
pub struct Epochs {
    /// Where the stamp for address zero would be.
    origin: usize,
}

impl Epochs {
    /// A plane whose slot for address 0 would be at `origin`.
    ///
    /// # Safety
    ///
    /// Every address this plane is later asked about must land inside a mapping the caller owns and
    /// keeps for as long as the plane is used. Nothing here range checks, because the point of a
    /// direct map is that there is nothing to check.
    #[must_use]
    pub const unsafe fn new(origin: usize) -> Self {
        Self { origin }
    }

    /// Where the stamp for `addr` is kept.
    #[must_use]
    pub const fn slot(&self, addr: usize) -> *mut Stamp {
        // Modular, for the reason `new` gives. The offset cannot overflow: it is an address.
        self.origin.wrapping_add((addr / GRANULE) * SLOT) as *mut Stamp
    }

    /// What was last seen written to the granule holding `addr`.
    ///
    /// # Safety
    ///
    /// `addr` is inside the mapping this plane was built for.
    #[must_use]
    pub unsafe fn read(&self, addr: usize) -> Stamp {
        // SAFETY: the caller says `addr` is mapped, so its slot is inside the shadow reservation
        // this plane was built over, and it is aligned by construction.
        unsafe { self.cell(addr).load(Ordering::Relaxed) }
    }

    /// Records that `stamp` wrote the granule holding `addr`.
    ///
    /// # Safety
    ///
    /// As [`Epochs::read`].
    pub unsafe fn write(&self, addr: usize, stamp: Stamp) {
        // SAFETY: as in `read`.
        unsafe { self.cell(addr).store(stamp, Ordering::Relaxed) }
    }

    /// Records that `stamp` wrote every granule `[at, at + len)` touches.
    ///
    /// A granule at either end that the range only covers part of is stamped whole, and that is the
    /// plane's granularity rather than a rounding error. What keeps it from reporting on two threads
    /// writing neighbouring bytes is which stores reach here at all: this plane is asked about
    /// pointer shaped words, an aligned pointer word is a granule, and a granule two threads share is
    /// one holding no pointer and so one no judgement asks about.
    ///
    /// # Safety
    ///
    /// The range is inside the mapping this plane was built for.
    pub unsafe fn fill(&self, at: usize, len: usize, stamp: Stamp) {
        if len == 0 {
            return;
        }
        let lo = at - at % GRANULE;
        for granule in 0..(at - lo + len).div_ceil(GRANULE) {
            // SAFETY: the caller says the range is mapped, so every granule it covers has a slot,
            // and the walk stops at the last of them.
            unsafe { self.write(lo + granule * GRANULE, stamp) }
        }
    }

    /// The first stamp in `[at, at + len)` that nothing a thread holding `mine` has done orders, or
    /// [`NONE`] when every granule of the range is one this thread may look at.
    ///
    /// The reading half of section 9.5, and the whole of judgements C2 and C3. It answers a stamp
    /// rather than a yes or a no because what makes the report worth having is naming the other
    /// thread, and the stamp is where that name is.
    ///
    /// The first rather than the worst. There is no ordering among the strangers a range holds that
    /// would make one of them the one to report, every one of them is a race on its own, and a walk
    /// that carried on to compare them would be doing work on the path an access takes to pick
    /// between two answers that say the same thing.
    ///
    /// # Safety
    ///
    /// The range is inside the mapping this plane was built for.
    #[must_use]
    pub unsafe fn stranger(&self, at: usize, len: usize, mine: Stamp) -> Stamp {
        if len == 0 {
            return NONE;
        }
        let lo = at - at % GRANULE;
        for granule in 0..(at - lo + len).div_ceil(GRANULE) {
            // SAFETY: the caller says the range is mapped, so every granule it covers has a slot,
            // and the walk stops at the last of them.
            let found = unsafe { self.read(lo + granule * GRANULE) };
            if unordered(found, mine) {
                return found;
            }
        }
        NONE
    }

    /// Forgets everything about `[lo, lo + len)`, which is what an instance beginning there means.
    ///
    /// # Safety
    ///
    /// `lo` is granule aligned and the range is inside the mapping this plane was built for.
    pub unsafe fn clear(&self, lo: usize, len: usize) {
        debug_assert!(lo % GRANULE == 0, "a storage instance starts on a granule");
        // SAFETY: the caller's, passed straight on.
        unsafe { self.fill(lo, len, NONE) }
    }

    /// The slot for `addr` as something two threads may touch at once.
    ///
    /// This is the one plane that is read while another thread is writing it, which is not an
    /// accident of the implementation but the entire reason it exists. The other three are read
    /// and written under whatever the program's own synchronization is, so a plain load is what
    /// they use. Here a plain load would be a data race in the language's own sense, and a compiler
    /// given one is entitled to assume it does not happen and to act on that everywhere around it.
    /// Relaxed is the weakest ordering that is still defined, and it is enough: what a stamp means
    /// is carried by the clock inside it rather than by the ordering of the load that found it.
    ///
    /// # Safety
    ///
    /// As [`Epochs::read`].
    const unsafe fn cell(&self, addr: usize) -> &AtomicU64 {
        // SAFETY: the caller says `addr` is mapped, the slot is inside the shadow reservation, and
        // it is eight byte aligned because the origin was chosen so and the stride is eight.
        unsafe { AtomicU64::from_ptr(self.slot(addr)) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand in for the shadow, and a stand in for the program memory it shadows.
    ///
    /// The same fake [`crate::plane`]'s tests use and for the same reason: nothing in this file
    /// dereferences a program address, it only divides one, so inventing them keeps the tests away
    /// from whatever the harness's own allocator is doing.
    struct Fake {
        /// Never read through this name. It is here so the buffer the plane's arithmetic lands in
        /// lives exactly as long as the plane does.
        _shadow: std::vec::Vec<Stamp>,
        plane: Epochs,
        base: usize,
    }

    impl Fake {
        /// A plane over `granules` granules of pretend memory starting at a pretend address.
        fn new(granules: usize) -> Self {
            let base = 0x1_0000;
            let mut shadow = std::vec![NONE; granules];
            let origin = (shadow.as_mut_ptr() as usize) - (base / GRANULE) * SLOT;
            // SAFETY: the buffer covers exactly the granules the tests ask about, it outlives the
            // plane because both are fields of this struct, and the offset above is what makes the
            // plane's arithmetic land inside it.
            let plane = unsafe { Epochs::new(origin) };
            Self { _shadow: shadow, plane, base }
        }

        fn read(&self, offset: usize) -> Stamp {
            // SAFETY: within the buffer, by the caller keeping to the granules it asked for.
            unsafe { self.plane.read(self.base + offset) }
        }

        fn write(&self, offset: usize, stamp: Stamp) {
            // SAFETY: as above.
            unsafe { self.plane.write(self.base + offset, stamp) }
        }

        fn clear(&self, offset: usize, len: usize) {
            // SAFETY: as above.
            unsafe { self.plane.clear(self.base + offset, len) }
        }

        fn fill(&self, offset: usize, len: usize, stamp: Stamp) {
            // SAFETY: as above.
            unsafe { self.plane.fill(self.base + offset, len, stamp) }
        }

        fn slot(&self, offset: usize) -> usize {
            self.plane.slot(self.base + offset) as usize
        }

        fn stranger(&self, offset: usize, len: usize, mine: Stamp) -> Stamp {
            // SAFETY: as above.
            unsafe { self.plane.stranger(self.base + offset, len, mine) }
        }
    }

    #[test]
    fn a_stamp_says_which_thread_and_which_tick_and_reads_back_as_both() {
        // The packing on its own, because everything else in the file is a comparison between two
        // of these and a field that bled into its neighbour would make every comparison wrong in a
        // way that still looked plausible.
        let of = stamp(3, 9);
        assert_eq!(thread(of), 3);
        assert_eq!(clock(of), 9);
        assert_ne!(of, NONE);

        // And the ends of both fields, which is where a shift that is off by one shows up.
        let last = stamp(THREADS, CLOCKS);
        assert_eq!(thread(last), THREADS);
        assert_eq!(clock(last), CLOCKS);
        assert_eq!(last, u64::MAX);
    }

    #[test]
    fn a_stamp_from_another_thread_that_nothing_orders_is_the_report() {
        // The whole of the detection, in the four cases it has to tell apart.
        let mine = stamp(1, 10);

        assert!(unordered(stamp(2, 10), mine), "another thread, at the same count");
        assert!(unordered(stamp(2, 11), mine), "another thread, further along");
        assert!(!unordered(stamp(2, 9), mine), "another thread this one has got past");
        assert!(!unordered(stamp(1, 99), mine), "this thread's own writing");
        assert!(!unordered(NONE, mine), "a word the plane never watched");
    }

    #[test]
    fn taking_a_locks_ordering_puts_the_other_threads_writing_behind_this_one() {
        // The one edge between threads there is. Without it every store another thread made would
        // stay concurrent with everything this one does for the rest of the program, which is the
        // false positive the whole design is arranged to avoid.
        let mut clock = Clock::new(1);
        let theirs = stamp(2, 40);
        assert!(unordered(theirs, clock.now()));

        clock.sync(theirs);
        clock.tick();
        assert!(!unordered(theirs, clock.now()));
    }

    #[test]
    fn a_lock_that_carries_nothing_does_not_advance_the_clock_of_whoever_takes_it() {
        // Most locks, most of the time. Counting one for an edge nobody published would walk a
        // thread's clock up for every lock it takes that nothing has ever released, which is a
        // thread drifting ahead of its peers for no reason anybody could point at.
        let mut clock = Clock::new(1);
        let start = clock.now();
        clock.sync(NONE);
        assert_eq!(clock.now(), start);
    }

    #[test]
    fn a_clock_that_has_stopped_says_nothing_rather_than_the_same_thing_forever() {
        // A wrapped clock puts a later store behind an earlier one, and a pair like that is
        // concurrent looked at from one side and ordered from the other, so it is a report about a
        // program with no race in it. Stopping avoids that, and stopping while still stamping trades
        // it for a different one: every reader of a parked thread's word finds an equal clock, which
        // `unordered` calls concurrent whatever really happened.
        let mut clock = Clock::new(1);
        clock.sync(stamp(1, CLOCKS));
        assert_eq!(clock.tick(), NONE);
        assert_eq!(clock.tick(), NONE);
        assert_eq!(thread(clock.now()), 1, "and the thread is still this one");
        assert!(!unordered(stamp(2, 5), clock.now()), "and nothing it reads is reported either");
    }

    #[test]
    fn two_halves_of_one_store_agree_and_two_halves_of_two_stores_do_not() {
        // Judgement C1. The tear is the thing Fil-C accepts and stays memory safe under, and it is
        // still a program following a pointer to an object it never had a pointer to.
        let one = stamp(1, 4);
        let other = stamp(2, 4);

        assert!(!torn(one, one));
        assert!(torn(one, other));
        assert!(!torn(one, NONE), "half of it was never watched");
        assert!(!torn(NONE, one));
    }

    #[test]
    fn no_two_threads_get_one_number_until_the_numbers_run_out() {
        // What the source has to do, and what it does at the end of the range, which is hand out
        // the last number over and over rather than starting again. A recycled number makes two
        // threads that are running at the same time look like one.
        let threads = Threads::new();
        let mut seen = std::collections::HashSet::new();
        for _ in 0..1000 {
            let number = threads.next();
            assert!(seen.insert(number));
            assert_ne!(number, 0, "zero is the thread of NONE");
        }

        let ended = Threads(AtomicU64::new(THREADS));
        assert_eq!(ended.next(), THREADS);
        assert_eq!(ended.next(), THREADS);
    }

    #[test]
    fn what_a_thread_writes_is_what_the_next_reader_of_that_granule_finds() {
        // The plane doing its job, and the granule being eight bytes, which is what makes the slot
        // for a pointer word the slot for that word alone.
        let fake = Fake::new(64);
        let of = stamp(2, 5);

        fake.write(0, of);
        for offset in 0..GRANULE {
            assert_eq!(fake.read(offset), of, "byte {offset} is in the same granule");
        }
        assert_eq!(fake.read(GRANULE), NONE, "and the word after it is somebody else's");
    }

    #[test]
    fn an_instance_beginning_forgets_what_the_last_one_left() {
        // Storage that has just begun has been written by nobody, whatever the thread that owned
        // the address before it was doing. Leaving the stamps there would report a race between a
        // dead instance's writer and this one's.
        let fake = Fake::new(64);
        fake.write(0, stamp(1, 2));
        fake.write(64, stamp(2, 3));

        fake.clear(0, 40);

        for offset in (0..48).step_by(GRANULE) {
            assert_eq!(fake.read(offset), NONE, "granule at {offset} still remembers");
        }
        assert_eq!(fake.read(64), stamp(2, 3), "and the instance beside it is untouched");
    }

    #[test]
    fn a_store_that_covers_part_of_a_granule_stamps_it_whole() {
        // The plane's granularity, said out loud. What keeps this from reporting on two threads
        // writing neighbouring bytes is which stores reach the plane at all, which is the note on
        // `Epochs::fill`, and not the arithmetic here.
        let fake = Fake::new(64);
        let of = stamp(1, 3);

        fake.fill(3, 2, of);
        assert_eq!(fake.read(0), of, "the bytes below the store are in the same granule");
        assert_eq!(fake.read(GRANULE - 1), of, "and so are the bytes above it");
        assert_eq!(fake.read(GRANULE), NONE, "and the next granule is nobody's");

        fake.fill(GRANULE - 1, 2, of);
        assert_eq!(fake.read(GRANULE), of, "a store across the line stamps both sides");

        fake.fill(GRANULE * 4, 0, of);
        assert_eq!(fake.read(GRANULE * 4), NONE, "and a store of nothing stamps nothing");
    }

    #[test]
    #[cfg(unix)]
    fn a_thread_counts_its_own_stores_and_another_thread_is_not_it() {
        // The clock the judgements will stamp with, over the thread local the crate already has.
        // Two threads getting one number is the failure that matters, because it is the one that
        // makes a race look like a thread writing its own memory twice.
        let first = tick();
        let second = tick();
        assert_eq!(thread(first), thread(second), "one thread keeps its number");
        assert!(clock(second) > clock(first), "and counts what it stores");
        assert_eq!(here(), second, "and asking without storing counts nothing");

        let theirs = std::thread::spawn(tick).join().expect("the thread ran");
        assert_ne!(thread(theirs), thread(first));
        assert_ne!(thread(theirs), 0, "and it is a real number rather than nobody's");
    }

    #[test]
    fn a_read_finds_the_thread_whose_writing_nothing_of_its_own_orders() {
        // The reading half, which is judgement J9 over a range rather than over a word. What comes
        // back is the stamp and not a yes, because the report is worth having for the name in it.
        let fake = Fake::new(64);
        let mine = stamp(1, 10);
        let theirs = stamp(2, 12);

        assert_eq!(fake.stranger(0, 32, mine), NONE, "nobody has written any of it");

        fake.write(16, theirs);
        assert_eq!(fake.stranger(0, 32, mine), theirs);
        assert_eq!(fake.stranger(0, 8, mine), NONE, "the granules below it are still nobody's");
        assert_eq!(fake.stranger(16, 1, mine), theirs, "one byte of the granule is enough");
    }

    #[test]
    fn a_read_says_nothing_about_its_own_writing_or_about_a_thread_it_has_got_past() {
        // The two ways an answer is ordered, and they are the two the whole detector rests on. A
        // report in either case would be a report about a program that synchronized correctly.
        let fake = Fake::new(64);
        let mine = stamp(1, 10);

        fake.write(0, stamp(1, 4));
        assert_eq!(fake.stranger(0, 8, mine), NONE, "this thread wrote it");

        fake.write(0, stamp(2, 9));
        assert_eq!(fake.stranger(0, 8, mine), NONE, "and this one has been got past");
    }

    #[test]
    fn a_range_holding_two_strangers_answers_the_first_one() {
        // There is no ordering among them that would make one the one to report, so the walk stops
        // at the first rather than carrying on to pick between answers that say the same thing.
        let fake = Fake::new(64);
        let mine = stamp(1, 1);
        fake.write(8, stamp(2, 5));
        fake.write(16, stamp(3, 5));

        assert_eq!(fake.stranger(0, 24, mine), stamp(2, 5));
        assert_eq!(fake.stranger(16, 8, mine), stamp(3, 5), "and starting past it finds the other");
        assert_eq!(fake.stranger(0, 0, mine), NONE, "and a read of nothing reads nobody's word");
    }

    #[test]
    fn the_slot_for_an_address_is_the_slot_for_its_granule() {
        // The arithmetic on its own, because a shift that is off by one is a plane that reads its
        // neighbour and answers plausibly every time.
        let fake = Fake::new(64);

        for offset in 0..GRANULE {
            assert_eq!(fake.slot(offset), fake.slot(0));
        }
        assert_eq!(fake.slot(GRANULE) - fake.slot(0), SLOT);
    }
}
