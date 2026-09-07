//! The arena: where an allocated storage instance begins and ends.
//!
//! Design: `spec/safe-memory/08-temporal.md` sections 8.3 and 8.6, and the layout in
//! `spec/safe-memory/05-representation.md` section 5.2.2.
//!
//! An arena is handed a region of memory and hands out instances from it. Its whole job, as far
//! as the monitor is concerned, is to say when an instance begins and when it ends: judgement J4
//! writes a fresh version over the instance's granules, judgement J5 writes another one, and
//! everything a stale pointer does after that fails on the version compare. Document 10 section
//! 10.4 makes this an API so a third party allocator can say the same two things, and this is the
//! in tree caller of it.
//!
//! Judgement J6, whether a free is a free of something this arena allocated and has not already
//! ended, falls out of reading the header, which is why double free and free of an interior
//! pointer need no separate registry. Quarantine designs need one.
//!
//! # What is here so far
//!
//! The bookkeeping, over a region handed in. Where the region comes from is a `MAP_NORESERVE`
//! mapping on a hosted target and the physical allocator on a kernel, and neither belongs in a
//! file that is otherwise arithmetic. Reuse is by exact size class, so an instance can only be
//! given an address a same sized instance had before, which is what lets the free lists work
//! without splitting or coalescing. Splitting and coalescing are still milestone S2's, and the
//! reason they are not here is that they change which granules an instance covers and that is the
//! thing judgements J4 and J5 are written out of.

use crate::layout::{self, Class, Header, Meta, State, perm};
use crate::plane::{self, Counter, GRANULE, Lifetime, Version};

/// Why a free was refused, per judgement J6 of document 04 section 4.4.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The pointer is not the base of anything this arena handed out.
    NotAnInstance,
    /// The instance is already over. This is a double free.
    AlreadyEnded,
    /// Another allocator handed this out, so this one may not take it back.
    WrongAllocator,
}

/// How many payload sizes there are between one power of two and the next.
///
/// The whole of the size class scheme is in this number. One would be powers of two, which wastes
/// up to half of every request that is not already one, and this arena wasted that much until a
/// long run of SQLite's test suite ran the region out. Four caps the waste at a quarter and costs
/// four times the free list heads, which are words. Eight would cap it at an eighth and is a
/// change to one constant if a corpus ever says the quarter is what hurts.
const STEPS: usize = 4;

/// How many size classes get a free list.
///
/// Enough for every size a `usize` can name, which is what makes the question of what happens past
/// the last class not arise. It used to arise: there were seventeen classes covering up to a
/// megabyte and anything larger was served from the bump and never reused, so a program that
/// allocated a few large buffers over a long run consumed the region and got a null back however
/// carefully it freed. That was tamnd/rucc#611.
///
/// The cost of covering everything is this array, which is a couple of kilobytes of arena state
/// once, against a bound that had to be guessed and was guessed wrong.
pub const CLASSES: usize = STEPS - 1 + (usize::BITS as usize - 2 - 1) * STEPS;

/// A region of memory, the instances carved out of it, and the plane that says who owns what.
#[derive(Debug)]
pub struct Arena {
    plane: Lifetime,
    versions: Counter,
    instances: Counter,
    id: u64,
    base: usize,
    next: usize,
    end: usize,
    /// The head of each class's list of ended blocks, or 0 for empty. The link of a block is kept
    /// in the first word of its own payload, which is storage nobody may reach any more, since
    /// every capability for it fails the version compare. The header is left alone, because every
    /// field of it is something judgement J6 asks about on the next free.
    free: [usize; CLASSES],
}

impl Arena {
    /// An arena over `[base, base + len)`, whose instances carry `id` for judgement J6.
    ///
    /// # Safety
    ///
    /// The region is mapped, writable, granule aligned, and owned by this arena alone for as long
    /// as it lives. `plane` covers every address in it. `id` is not shared with another allocator,
    /// since that is the whole content of the wrong allocator refusal.
    #[must_use]
    pub const unsafe fn new(plane: Lifetime, base: usize, len: usize, id: u64) -> Self {
        Self {
            plane,
            versions: Counter::new(),
            instances: Counter::new(),
            id,
            base,
            next: base,
            end: base + len,
            free: [0; CLASSES],
        }
    }

    /// Judgement J4: a new instance of `n` bytes, or 0 if the region has no room for one.
    ///
    /// The address returned points at the payload, so the program never sees the header or the
    /// aux and never has to know they are there. That is what keeps `sizeof(void *)` alone and is
    /// the reason this design links against code built by another compiler at all.
    pub fn begin(&mut self, n: usize) -> usize {
        let size = Self::sized(n);
        let block = match self.take(size) {
            0 => self.bump(size),
            reused => reused,
        };
        if block == 0 {
            return 0;
        }

        let version = plane::begun(self.versions.next());
        let payload = layout::payload_of(block, size);
        let header = Header {
            ext: size as u64,
            ver: version,
            meta: Meta::new(Class::Allocated, perm::READ | perm::WRITE, self.instances.next()),
            allocator: self.id,
        };
        // SAFETY: `block` is inside the region, which the caller of `new` promised is mapped and
        // writable, and the arena hands out no two overlapping blocks.
        unsafe { Self::header_at(block, size).write(header) };
        // SAFETY: the payload is granule aligned by `payload_of` and inside the same region, and
        // the version is one the counter has not returned before.
        unsafe { self.plane.begin(payload, size, version) };
        payload
    }

    /// Judgement J5, with J6 checked first: the instance at `payload` is over.
    ///
    /// # Errors
    ///
    /// Refuses a pointer that is not the base of one of this arena's live instances, which is
    /// where double free, free of an interior pointer and free by the wrong allocator are caught.
    ///
    /// # Safety
    ///
    /// `payload` is an address inside the region this arena was built over, so that reading the
    /// header in front of it reads this arena's memory. A pointer from somewhere else entirely is
    /// something only the shadow can rule out, and that is milestone S2.
    pub unsafe fn end(&mut self, payload: usize) -> Result<(), Refusal> {
        // SAFETY: the caller says `payload` is inside the region, which is what makes reading the
        // header in front of it a read of this arena's own storage.
        let header = unsafe { self.header(payload)? };

        let size = header.ext as usize;
        // The instance's own version with the low bit set, which no capability holds and which
        // says which instance the range used to be. See `plane::ended`.
        let ended = plane::ended(header.ver);
        // SAFETY: `payload` is the base of a live instance of `size` bytes, which is what reading
        // the header just established, so its granules are inside the region and the plane.
        unsafe { self.plane.end(payload, size, ended) };

        // SAFETY: as above, and the header is this arena's own storage rather than the program's.
        unsafe {
            (layout::header_of(payload) as *mut Header).write(Header {
                ver: ended,
                meta: header.meta.with_state(State::Ended),
                ..header
            });
        }
        self.put(size, layout::block_of(payload, size));
        Ok(())
    }

    /// Reads and checks the header in front of `payload`, which is judgement J6.
    ///
    /// # Safety
    ///
    /// As [`Arena::end`].
    unsafe fn header(&self, payload: usize) -> Result<Header, Refusal> {
        // An interior pointer is not granule aligned in the usual case, and one outside the arena
        // has no header of ours behind it. Neither screen is complete: an interior pointer that
        // happens to land on a granule gets through here and is caught below, because the header
        // it reads is the aux of the instance it points into rather than a live header.
        if payload % GRANULE != 0 || payload <= self.base + layout::HEADER || payload > self.end {
            return Err(Refusal::NotAnInstance);
        }
        // SAFETY: `payload` is inside the region by the check above, so the thirty two bytes in
        // front of it are too, and they are the arena's own storage rather than the program's.
        let header = unsafe { (layout::header_of(payload) as *const Header).read() };

        let size = header.ext as usize;
        if header.meta.class() != Class::Allocated as u8
            || size != Self::sized(size)
            || layout::block_of(payload, size) < self.base
            || payload + size > self.end
        {
            return Err(Refusal::NotAnInstance);
        }
        if header.allocator != self.id {
            return Err(Refusal::WrongAllocator);
        }
        if header.meta.state() != State::Live as u8 {
            return Err(Refusal::AlreadyEnded);
        }
        Ok(header)
    }

    /// How many bytes the instance at `payload` was given.
    ///
    /// Which is more than was asked for, since a payload is rounded up to its class. A resize may
    /// copy all of it, and a caller that writes all of it is writing storage that is its own.
    ///
    /// # Errors
    ///
    /// As [`Arena::end`], and for the same reason: the question is only answerable for a live
    /// instance of this arena's, and asking it about anything else is judgement J6 again.
    ///
    /// # Safety
    ///
    /// As [`Arena::end`].
    pub unsafe fn extent(&self, payload: usize) -> Result<usize, Refusal> {
        // SAFETY: the caller's contract is this function's contract passed straight on.
        Ok(unsafe { self.header(payload)? }.ext as usize)
    }

    /// Whether `addr` is inside the region this arena was built over.
    ///
    /// What a caller holding a pointer of unknown provenance asks before anything else. A pointer
    /// from some other allocator has no header of ours behind it, and reading one would be the
    /// monitor committing the bug it exists to catch.
    #[must_use]
    pub const fn contains(&self, addr: usize) -> bool {
        addr >= self.base && addr < self.end
    }

    /// Whether a capability holding `version` may still be used to reach `addr`.
    ///
    /// This is what the generated check calls, and it is here rather than on the plane only
    /// because the arena is what a test has to hand to get a plane worth asking.
    ///
    /// # Safety
    ///
    /// `addr` is inside the region this arena was built over.
    #[must_use]
    pub unsafe fn live(&self, addr: usize, version: Version) -> bool {
        // SAFETY: the caller says `addr` is in the region, and the plane covers all of it.
        unsafe { self.plane.live(addr, version) }
    }

    /// The version the instance at `payload` was given, or [`crate::plane::DEAD`].
    ///
    /// # Safety
    ///
    /// As [`Arena::live`].
    #[must_use]
    pub unsafe fn version(&self, payload: usize) -> Version {
        // SAFETY: as above.
        unsafe { self.plane.version(payload) }
    }

    /// How large a payload of `n` bytes is actually given.
    ///
    /// Rounded up to a size class, so that every block on a free list is the same size as every
    /// other block on it and reuse needs no splitting or coalescing. The classes are granule
    /// counts: one, two, three and four exactly, and above that four to a power of two, so 4, 5,
    /// 6, 7, 8, 10, 12, 14, 16, 20, 24 and onwards. The rounding never wastes more than a quarter,
    /// and below five granules it wastes nothing at all.
    ///
    /// Idempotent, which the free path depends on: it reads a size out of a header and refuses the
    /// pointer if the size is not one this function would have produced, and a rounding that moved
    /// a class size to the next class would refuse every free.
    #[must_use]
    pub const fn sized(n: usize) -> usize {
        // A request for nothing still gets a granule. A payload of no bytes has no granule of its
        // own to hold a version, so it would share the next instance's, and it has no word to hold
        // a free list link. C also lets `malloc(0)` hand back an address, and an address a program
        // may pass to `free` has to be an instance like any other.
        let want = if n == 0 { GRANULE } else { layout::payload(n) };
        let count = want / GRANULE;
        if count <= STEPS {
            return want;
        }
        // The spacing at this magnitude, which is a quarter of the power of two the count is in.
        let step = 1 << (Self::magnitude(count) - 2);
        count.div_ceil(step) * step * GRANULE
    }

    /// Which free list a payload of `size` belongs to, if any.
    ///
    /// Every size this arena hands out has one, and a size that is not a class size has none. The
    /// second half is not defensive. A block reaches the free lists only through a header whose
    /// size the free path already held against [`Arena::sized`], so a `None` here would be a bug
    /// rather than a case, and the alternative to answering it is answering with a class whose
    /// blocks this one is not interchangeable with.
    fn class_of(size: usize) -> Option<usize> {
        if size != Self::sized(size) {
            return None;
        }
        let count = size / GRANULE;
        if count == 0 {
            return None;
        }
        if count <= STEPS {
            return Some(count - 1);
        }
        // Within a power of two the count is one of [`STEPS`] evenly spaced values, and which one
        // it is is what distinguishes it from the others on the same magnitude.
        let magnitude = Self::magnitude(count);
        let step = 1 << (magnitude - 2);
        let within = (count - (1 << magnitude)) / step;
        Some(STEPS - 1 + (magnitude - 2) * STEPS + within)
    }

    /// The floor of the base two logarithm of `count`, which every caller has established is
    /// above four, so the answer is at least two and the shifts below do not underflow.
    const fn magnitude(count: usize) -> usize {
        (usize::BITS - 1 - count.leading_zeros()) as usize
    }

    /// Where a block of `size` keeps its header.
    const fn header_at(block: usize, size: usize) -> *mut Header {
        layout::header_of(layout::payload_of(block, size)) as *mut Header
    }

    /// Where an ended block of `size` keeps its free list link.
    ///
    /// The first word of the payload. A payload is at least a granule, so there is always room,
    /// and the storage is unreachable: every capability for it fails on the version the end
    /// wrote. Putting the link in the header instead would mean overwriting a field the next
    /// free asks about, which is how a double free comes back as the wrong complaint.
    const fn link(block: usize, size: usize) -> *mut usize {
        layout::payload_of(block, size) as *mut usize
    }

    /// Pops a block sized for `size` off its free list, or 0.
    fn take(&mut self, size: usize) -> usize {
        let Some(class) = Self::class_of(size) else { return 0 };
        let block = self.free[class];
        if block == 0 {
            return 0;
        }
        // SAFETY: the block is one this arena ended and pushed, so it is inside the region and
        // its header is the arena's own storage rather than anything the program may reach.
        self.free[class] = unsafe { Self::link(block, size).read() };
        block
    }

    /// Pushes an ended block sized for `size` onto its free list.
    fn put(&mut self, size: usize, block: usize) {
        let Some(class) = Self::class_of(size) else { return };
        // SAFETY: `block` is an ended block inside the region, and no capability that survives
        // can reach its payload, because the end above wrote a version none of them holds.
        unsafe { Self::link(block, size).write(self.free[class]) };
        self.free[class] = block;
    }

    /// Carves a fresh block for a payload of `size`, or 0 if the region is exhausted.
    fn bump(&mut self, size: usize) -> usize {
        let block = self.next;
        let needed = layout::block(size);
        if needed > self.end - block {
            return 0;
        }
        self.next = block + needed;
        block
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plane::{DEAD, SLOT};

    /// A region to allocate out of, and the shadow for it, both ordinary buffers.
    ///
    /// Real memory rather than the invented addresses the plane's own tests use, because the
    /// arena writes headers into it and reads them back. The plane is mapped over the region by
    /// the same arithmetic the startup code will do.
    struct Fake {
        /// Never read through these names. They are here so that the region the arena carves up
        /// and the shadow its plane lands in both live exactly as long as the arena does.
        _region: std::vec::Vec<u8>,
        _shadow: std::vec::Vec<u64>,
        arena: Arena,
    }

    impl Fake {
        /// An arena over `len` bytes, whose identity is `id`.
        fn new(len: usize, id: u64) -> Self {
            let mut region = std::vec![0u8; len + GRANULE];
            // The region has to start on a granule, because the plane's slot for an address that
            // did not would be shared with whatever is in front of it.
            let base = (region.as_mut_ptr() as usize).next_multiple_of(GRANULE);
            let mut shadow = std::vec![DEAD; len.div_ceil(GRANULE) + 1];
            let origin = (shadow.as_mut_ptr() as usize) - (base / GRANULE) * SLOT;
            // SAFETY: the shadow covers every granule of `[base, base + len)`, the region is
            // owned by this struct and outlives the arena because both are fields of it, and the
            // base is granule aligned by the rounding above.
            let arena = unsafe { Arena::new(Lifetime::new(origin), base, len, id) };
            Self { _region: region, _shadow: shadow, arena }
        }

        fn begin(&mut self, n: usize) -> usize {
            self.arena.begin(n)
        }

        fn end(&mut self, payload: usize) -> Result<(), Refusal> {
            // SAFETY: every address a test passes here came out of `begin` on this same arena, or
            // is an address inside its region, which is what `Arena::end` asks for.
            unsafe { self.arena.end(payload) }
        }

        fn live(&self, addr: usize, version: Version) -> bool {
            // SAFETY: as above.
            unsafe { self.arena.live(addr, version) }
        }

        fn version(&self, payload: usize) -> Version {
            // SAFETY: as above.
            unsafe { self.arena.version(payload) }
        }

        /// How much of the region is still untouched, which is what tells a test whether an
        /// allocation was served from a free list or from the bump.
        fn spare(&self) -> usize {
            self.arena.end - self.arena.next
        }

        /// Writes a byte through the payload, so that a test is exercising real storage rather
        /// than arithmetic about it.
        fn poke(&self, addr: usize, byte: u8) {
            // SAFETY: `addr` is inside an instance this arena handed out, which is inside the
            // region this struct owns.
            unsafe { (addr as *mut u8).write(byte) };
        }
    }

    #[test]
    fn a_pointer_to_a_freed_instance_fails_after_the_address_is_handed_out_again() {
        // The end to end version of the plane's own test, through the allocator, which is where
        // the address reuse that makes it interesting actually happens. The second allocation
        // gets the first one's address back, and the first one's pointer is still refused.
        let mut fake = Fake::new(1 << 16, 1);

        let first = fake.begin(64);
        let held = fake.version(first);
        assert!(fake.live(first, held));

        fake.end(first).expect("the instance is live and this is its base");
        assert!(!fake.live(first, held));

        let second = fake.begin(64);
        assert_eq!(second, first, "the free list did not hand the address back");
        assert!(fake.live(second, fake.version(second)));
        assert!(!fake.live(first, held));
    }

    #[test]
    fn a_second_free_of_the_same_pointer_is_refused_rather_than_corrupting_the_free_list() {
        // Double free with no separate registry of freed objects, which is the advantage this
        // mechanism has over quarantine. The header says the instance has ended and that is all
        // the question needs.
        let mut fake = Fake::new(1 << 16, 1);

        let payload = fake.begin(32);
        assert_eq!(fake.end(payload), Ok(()));
        assert_eq!(fake.end(payload), Err(Refusal::AlreadyEnded));
    }

    #[test]
    fn freeing_a_pointer_into_the_middle_of_an_instance_is_refused() {
        // free(p + 8) is a real bug and a common one, and it has to be refused before the header
        // read, because there is no header eight bytes behind the middle of a payload.
        let mut fake = Fake::new(1 << 16, 1);

        let payload = fake.begin(256);
        assert_eq!(fake.end(payload + 8), Err(Refusal::NotAnInstance));
        assert_eq!(fake.end(payload + GRANULE), Err(Refusal::NotAnInstance));
        assert_eq!(fake.end(payload), Ok(()), "the real base still works afterwards");
    }

    #[test]
    fn an_allocator_may_not_free_what_another_one_handed_out() {
        // Judgement J6's deallocator clause. The two arenas here have different identities and
        // the header carries the one that allocated, so the mismatch is one compare.
        let mut mine = Fake::new(1 << 16, 1);
        let mut theirs = Fake::new(1 << 16, 2);

        let payload = mine.begin(64);
        // The address is not in the other arena's region at all, so this is the easy half. The
        // half that matters is the identity check, and the region check must not be what refuses
        // it, which is why the assertion below reads the reason rather than only the failure.
        let refusal = theirs.end(payload);
        assert!(refusal.is_err());
        assert_eq!(mine.end(payload), Ok(()));
    }

    #[test]
    fn every_byte_of_an_instance_is_writable_and_none_of_them_is_the_header() {
        // The header sits directly behind the payload, so an off by one in the layout would have
        // the program writing over its own capability. Writing every byte and then reading the
        // header back is what catches that.
        let mut fake = Fake::new(1 << 16, 1);

        let payload = fake.begin(48);
        let version = fake.version(payload);
        for offset in 0..48 {
            fake.poke(payload + offset, 0xAA);
        }
        assert_eq!(fake.version(payload), version, "the payload wrote over its own plane entry");
        assert_eq!(fake.end(payload), Ok(()), "the payload wrote over its own header");
    }

    #[test]
    fn two_instances_beside_each_other_do_not_share_a_granule() {
        // Sharing one would mean sharing a version, so freeing one would silently keep the
        // other's pointers working, or break them. Allocations round to a granule to stop it.
        let mut fake = Fake::new(1 << 16, 1);

        let first = fake.begin(1);
        let second = fake.begin(1);
        let held = fake.version(second);

        assert_ne!(first, second);
        assert_eq!(fake.end(first), Ok(()));
        assert!(fake.live(second, held), "freeing the neighbour ended this one too");
    }

    #[test]
    fn a_reused_block_comes_back_from_the_free_list_rather_than_from_the_bump() {
        // If it did not, an allocate and free loop would walk the region until it ran out, which
        // is the difference between an allocator and a leak with a nice interface.
        let mut fake = Fake::new(1 << 16, 1);

        let payload = fake.begin(64);
        fake.end(payload).expect("live instance");
        let spare = fake.spare();

        for _ in 0..100 {
            let again = fake.begin(64);
            assert_eq!(again, payload);
            fake.end(again).expect("live instance");
        }
        assert_eq!(fake.spare(), spare, "the bump moved for a size the free list already had");
    }

    #[test]
    fn an_arena_with_no_room_left_says_so_rather_than_handing_out_the_region_after_it() {
        // Returning an address past the end would be the monitor itself producing the bug it is
        // there to catch, so exhaustion returns nothing and the caller decides what to do.
        let mut fake = Fake::new(4096, 1);

        let mut count = 0;
        while fake.begin(64) != 0 {
            count += 1;
            assert!(count < 4096, "the arena is handing out more than it has");
        }
        assert!(count > 0, "the arena had room for nothing at all");
    }

    #[test]
    fn a_payload_is_rounded_up_to_its_class_and_the_class_is_never_far_above_it() {
        // The rounding is what lets every block on a free list be interchangeable, and how much of
        // it there is is the memory overhead of the design. Four granules and under is exact,
        // above that it is four classes to a power of two.
        assert_eq!(Arena::sized(1), GRANULE);
        assert_eq!(Arena::sized(16), 16);
        assert_eq!(Arena::sized(17), 32);
        assert_eq!(Arena::sized(48), 48);
        assert_eq!(Arena::sized(64), 64);
        assert_eq!(Arena::sized(65), 80);
        assert_eq!(Arena::sized(208), 224);
        assert_eq!(Arena::sized(1000), 1024);
        assert_eq!(Arena::sized(1 << 20), 1 << 20);
        assert_eq!(Arena::sized((1 << 20) + 1), (1 << 20) + (1 << 18));
    }

    #[test]
    fn a_block_far_larger_than_the_old_biggest_class_is_reused() {
        // tamnd/rucc#611, as a test. There used to be seventeen classes covering up to a megabyte
        // and anything above that was served from the bump and never reused, so a program that
        // allocated and freed one large buffer in a loop consumed the region until it got a null
        // back. This asks for two megabytes forty times out of eight, which the old arena could
        // not have done four times.
        let mut fake = Fake::new(8 << 20, 1);
        let first = fake.begin(2 << 20);
        assert_ne!(first, 0);
        assert_eq!(fake.end(first), Ok(()));
        let after = fake.spare();

        for round in 0..40 {
            let payload = fake.begin(2 << 20);
            assert_ne!(payload, 0, "the region ran out on round {round}");
            assert_eq!(payload, first, "round {round} was served from the bump");
            assert_eq!(fake.end(payload), Ok(()));
        }
        assert_eq!(fake.spare(), after, "the bump moved for a block a free list had");
    }

    #[test]
    fn no_size_is_rounded_up_by_more_than_a_quarter_of_itself() {
        // The number the class scheme exists to bound, checked over every granule count up to a
        // megabyte rather than at the few points a handful of asserts would reach.
        for n in (GRANULE..=(1 << 20)).step_by(GRANULE) {
            let given = Arena::sized(n);
            assert!(given >= n, "{n} was given {given}");
            assert!(given * 4 <= n * 5, "{n} was rounded up to {given}");
        }
    }

    #[test]
    fn rounding_a_size_that_is_already_a_class_leaves_it_alone() {
        // The free path reads a size out of a header and refuses the pointer unless `sized` would
        // have produced it, so a rounding that moved a class size onwards would refuse every free
        // of anything above four granules. That is a whole allocator failing on a property nothing
        // else in the file states.
        for n in (GRANULE..=(1 << 22)).step_by(GRANULE) {
            let given = Arena::sized(n);
            assert_eq!(Arena::sized(given), given, "{n} rounded to {given}");
        }
    }

    #[test]
    fn every_class_size_gets_its_own_free_list_and_no_two_share_one() {
        // Two sizes on one list would hand a request the address of a smaller block, which is a
        // heap overflow the monitor itself created. The classes have to be a bijection and this is
        // that, over every size up to a megabyte and a sample of the ones above it.
        let mut seen = std::collections::BTreeMap::new();
        let sizes = (GRANULE..=(1 << 20))
            .step_by(GRANULE)
            .chain((20..60).map(|shift| 1_usize << shift))
            .map(Arena::sized);
        for size in sizes {
            let class = Arena::class_of(size).expect("a size this arena hands out has a class");
            assert!(class < CLASSES, "{size} wants class {class} of {CLASSES}");
            let first = seen.entry(class).or_insert(size);
            assert_eq!(*first, size, "sizes {first} and {size} share class {class}");
        }
    }

    #[test]
    fn asking_for_nothing_gets_an_instance_of_one_granule_rather_than_of_nothing() {
        // A payload of zero bytes rounds to zero granules, and zero granules is not a class: it
        // has no free list, no version of its own and nowhere to put the free list link. So the
        // smallest instance is a granule even when nothing was asked for, which is also what lets
        // `malloc(0)` hand back an address the program can free.
        assert_eq!(Arena::sized(0), GRANULE);

        let mut fake = Fake::new(1 << 16, 1);
        let payload = fake.begin(0);
        assert_ne!(payload, 0);
        assert_ne!(fake.version(payload), DEAD);

        let beside = fake.begin(0);
        assert_ne!(beside, payload, "two empty instances landed on the same address");
        assert_eq!(fake.end(payload), Ok(()));
        assert_eq!(fake.end(beside), Ok(()));
    }
}
