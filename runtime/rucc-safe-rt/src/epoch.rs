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
//! own count and whatever the lock was released at, which is the one edge that carries an ordering
//! between threads and is where document 10's interposed synchronization primitives come in.
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
//! division [`crate::plane`] explains and the other two planes follow. Nothing maps this plane yet
//! and nothing stamps anything, so no program is watched by it. Mapping it over every watched
//! region beside the other three, and the judgements that report what it finds, are the next box of
//! milestone S5.

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

    /// The stamp to write, counting this store.
    ///
    /// Saturating rather than wrapping, and the difference matters. A clock that wrapped would put
    /// a store that happened later behind one that happened earlier, which is a pair [`unordered`]
    /// calls ordered on one side and concurrent on the other, so it would invent reports about
    /// programs with no race in them. A clock that stops makes every store from then on look
    /// concurrent with every other, which loses reports and invents none.
    pub fn tick(&mut self) -> Stamp {
        self.count = self.count.saturating_add(1).min(CLOCKS);
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
    /// thread does after it, which is said by this thread's clock going at least that high.
    pub fn sync(&mut self, seen: Stamp) {
        self.count = self.count.max(clock(seen)).min(CLOCKS);
    }
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

    /// Forgets everything about `[lo, lo + len)`, which is what an instance beginning there means.
    ///
    /// # Safety
    ///
    /// `lo` is granule aligned and the range is inside the mapping this plane was built for.
    pub unsafe fn clear(&self, lo: usize, len: usize) {
        debug_assert!(lo % GRANULE == 0, "a storage instance starts on a granule");
        for granule in 0..len.div_ceil(GRANULE) {
            // SAFETY: the caller says the range is mapped, so every granule it covers has a slot,
            // and the walk stops at the last of them.
            unsafe { self.write(lo + granule * GRANULE, NONE) }
        }
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

        fn slot(&self, offset: usize) -> usize {
            self.plane.slot(self.base + offset) as usize
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
    fn a_clock_stops_rather_than_wrapping() {
        // A wrapped clock puts a later store behind an earlier one, and a pair like that is
        // concurrent looked at from one side and ordered from the other, so it is a report about a
        // program with no race in it. Stopping loses reports and invents none.
        let mut clock = Clock::new(1);
        clock.sync(stamp(1, CLOCKS));
        assert_eq!(super::clock(clock.tick()), CLOCKS);
        assert_eq!(super::clock(clock.tick()), CLOCKS);
        assert_eq!(thread(clock.now()), 1, "and the thread is still this one");
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
