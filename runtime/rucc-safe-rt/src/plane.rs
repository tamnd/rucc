//! The lifetime plane: which storage instance owns an address right now.
//!
//! Design: `spec/safe-memory/08-temporal.md` section 8.3, and the layout in
//! `spec/safe-memory/05-representation.md` section 5.2.3.
//!
//! This is the lock and key. A storage instance gets a version when it begins, every capability
//! made from it carries that version, and the plane holds the version of whoever owns each
//! sixteen byte granule of address space. An access is permitted when the capability's version
//! and the plane's version for the address are the same. Ending an instance writes a fresh
//! version over its range, so every capability anybody still holds for it fails from then on,
//! including after the address has been handed to something else. That last clause is the whole
//! reason for the design: quarantine catches use after reallocation and calls it use after free,
//! and this catches use after free.
//!
//! Sixty four bits of version at a billion allocations a second is five hundred and eighty four
//! years, so there is no wraparound case and no code here to handle one. The schemes that use ten
//! or sixteen bit keys have that hole, and it is what they buy their lower overhead with.
//!
//! # What is here so far
//!
//! The arithmetic and the reads and writes, with the shadow handed in. Nothing here maps the
//! shadow: on a hosted target that is a `MAP_NORESERVE` reservation made at startup and in the
//! kernel it comes from the physical allocator during early boot, and both of those belong with
//! the code that knows which target it is. Keeping the mapping out means the part that generated
//! code actually runs can be tested against an ordinary buffer, which is what the tests below do.

use core::sync::atomic::{AtomicU64, Ordering};

/// The version an instance is given, and the value the plane holds for its granules.
pub type Version = u64;

/// The version of a granule nobody owns, which is the encoding of the bottom capability.
///
/// A fresh shadow reservation reads as zero, so untouched address space is already spelled
/// correctly and nothing has to walk it to say so.
pub const DEAD: Version = 0;

/// The version of storage that is real but that nothing here versions.
///
/// What a capability recovered at the boundary carries when all that could be recovered is the
/// mapping the address sits in, per document 05 section 5.3. Even, so [`owned`] agrees the storage
/// exists, and reserved, so it can never be an instance's: [`Counter`] answers `begun(n)` for a
/// strictly increasing `n` from one, so reaching this would take 2^63 allocations, which at a
/// billion a second is two hundred and ninety two years.
pub const FOREIGN: Version = Version::MAX - 1;

/// The version the counter's `n`th answer names.
///
/// Even, always, and that is the whole of the encoding: the low bit of a slot says whether the
/// range is owned right now or was owned and has been given back. It costs one bit of the sixty
/// four, which takes the wraparound argument in the module comment from five hundred and eighty
/// four years down to two hundred and ninety two, and it buys a plane that can be read on its own.
///
/// Without it a slot holding a version says nothing about whether anybody owns the range: a freed
/// range carries a version too, and the only way to tell the two apart is to hold the capability
/// and compare. That is fine once capabilities exist, and until then a check that is handed only an
/// address has no question it can ask. Milestone S1 is exactly that situation, and this is what
/// lets [`crate::check::live`] answer at all.
#[must_use]
pub const fn begun(n: u64) -> Version {
    n << 1
}

/// What the plane holds for a range that `version` used to own.
///
/// The same version with the low bit set, rather than a fresh one off the counter. Every
/// capability for the range still fails, because an odd value equals no version anybody holds, and
/// the value left behind says which instance it was, which is something a report can use.
#[must_use]
pub const fn ended(version: Version) -> Version {
    version | 1
}

/// Whether a slot says somebody owns that granule right now.
///
/// Untouched address space reads [`DEAD`] and a range that has been given back reads odd, so this
/// is false for both.
#[must_use]
pub const fn owned(slot: Version) -> bool {
    slot != DEAD && slot % 2 == 0
}

/// How many bytes of program memory one version covers.
///
/// Sixteen, which is why allocations round to sixteen bytes and to sixteen byte alignment. That
/// is the natural malloc alignment on both of the parent's sixty four bit targets anyway, so the
/// rounding costs nothing that was not already being paid.
pub const GRANULE: usize = 16;

/// How many bytes of shadow one granule needs.
pub const SLOT: usize = size_of::<Version>();

/// Where the counter that hands out versions lives.
///
/// One per allocator arena rather than one for the program, so that two arenas do not contend on
/// a single line for something neither of them needs to agree about. What has to hold is that a
/// version is never handed out twice by the same counter, and the plane is only ever read against
/// a capability made by the arena that owns that address.
#[derive(Debug)]
pub struct Counter(AtomicU64);

impl Counter {
    /// A counter whose first version is 1, because 0 is [`DEAD`].
    #[must_use]
    pub const fn new() -> Self {
        Self(AtomicU64::new(1))
    }

    /// The next version, which no earlier call returned.
    ///
    /// Relaxed is enough: uniqueness is all that is asked of this, and a version orders nothing.
    /// The writes that publish it into the plane are what carry the ordering, and the allocator
    /// that calls this is already holding whatever it holds to own the range.
    pub fn next(&self) -> Version {
        self.0.fetch_add(1, Ordering::Relaxed)
    }
}

impl Default for Counter {
    fn default() -> Self {
        Self::new()
    }
}

/// The direct mapped shadow the plane lives in.
///
/// `origin` is chosen so that `origin + (addr / GRANULE) * SLOT` is the slot for `addr`, which is
/// the shift and the add of section 5.2.4 and is two instructions on both targets. It is a field
/// rather than a link time constant because AArch64 with a fifty two bit address space and sixty
/// four kilobyte pages cannot use a fixed offset, and one load on one configuration is cheaper
/// than two code paths everywhere.
#[derive(Clone, Copy, Debug)]
pub struct Lifetime {
    origin: usize,
}

impl Lifetime {
    /// A plane whose slot for address 0 would be at `origin`.
    ///
    /// `origin` is a bias rather than an address, and it is routinely not one: the shadow for a
    /// region high in the address space sits below the bias by more than the bias is, so the value
    /// that makes the arithmetic come out right has wrapped. That is why the arithmetic in
    /// [`Lifetime::slot`] is modular and why this takes a `usize` rather than a pointer.
    ///
    /// # Safety
    ///
    /// Every address this plane is later asked about must land inside a mapping the caller owns
    /// and keeps for as long as the plane is used. Nothing here range checks, because the point
    /// of a direct map is that there is nothing to check.
    #[must_use]
    pub const unsafe fn new(origin: usize) -> Self {
        Self { origin }
    }

    /// Where the version for `addr` is kept.
    ///
    /// Addresses inside a granule share a slot, which is what makes the plane affordable and is
    /// also why an instance may not start in the middle of one.
    #[must_use]
    pub const fn slot(&self, addr: usize) -> *mut Version {
        // Modular, because the origin is a bias and a bias may have wrapped. See `new`. The
        // offset itself cannot overflow: it is half of an address.
        self.origin.wrapping_add((addr / GRANULE) * SLOT) as *mut Version
    }

    /// The version that owns `addr` right now.
    ///
    /// # Safety
    ///
    /// `addr` is inside the mapping this plane was built for.
    #[must_use]
    pub unsafe fn version(&self, addr: usize) -> Version {
        // SAFETY: the caller says `addr` is mapped, and the slot for a mapped address is inside
        // the shadow reservation this plane was built over, aligned by construction.
        unsafe { self.slot(addr).read() }
    }

    /// Whether a capability holding `version` still names whoever owns `addr`.
    ///
    /// This is the check itself, and it is one shift, one load, one compare and one branch. When
    /// the bounds check survives too the two share the branch, per document 08 section 8.3.
    ///
    /// # Safety
    ///
    /// `addr` is inside the mapping this plane was built for.
    #[must_use]
    pub unsafe fn live(&self, addr: usize, version: Version) -> bool {
        // A capability that never named anything carries DEAD, and unmapped and freed granules
        // read DEAD, so this has to be spelled out rather than falling out of the compare.
        // SAFETY: `addr` is mapped, which is this function's own contract passed straight on.
        version != DEAD && unsafe { self.version(addr) } == version
    }

    /// Judgement J4: `[lo, lo + len)` is now owned by `version`.
    ///
    /// # Safety
    ///
    /// `lo` is granule aligned, the range is inside the mapping this plane was built for, and
    /// `version` came from a counter that has not returned it before. A repeated version is a
    /// stale pointer that starts working again.
    pub unsafe fn begin(&self, lo: usize, len: usize, version: Version) {
        // SAFETY: the range is mapped and granule aligned, which is what `fill` asks for.
        unsafe { self.fill(lo, len, version) }
    }

    /// Judgement J5: `[lo, lo + len)` is owned by nobody, and every capability for it now fails.
    ///
    /// A version no capability holds rather than [`DEAD`] is what document 08 section 8.3 asks
    /// for. Writing `DEAD` would work for the pointers that exist, but the next instance to be
    /// given this address would then be the one deciding whether the old pointers stay broken, and
    /// it is cheaper to settle that here than to make every allocator get it right. The value to
    /// write is [`ended`] of whatever owned the range, which no capability can equal.
    ///
    /// # Safety
    ///
    /// As [`Lifetime::begin`].
    pub unsafe fn end(&self, lo: usize, len: usize, version: Version) {
        // SAFETY: as in `begin`.
        unsafe { self.fill(lo, len, version) }
    }

    /// Writes one version over every granule the range touches.
    ///
    /// The cost of this scales with the size of the allocation rather than with the number of
    /// them, which is why document 08 calls large short lived allocations the worst case for the
    /// design. It is a `memset` over an eighth of the range and the allocator that zeroes the
    /// payload is walking the same span, so the two coalesce where the allocator lets them.
    unsafe fn fill(&self, lo: usize, len: usize, version: Version) {
        debug_assert!(lo % GRANULE == 0, "a storage instance starts on a granule");
        let mut slot = self.slot(lo);
        // Round up: the last granule is partly the instance's, and it is entirely the instance's
        // as far as the plane is concerned, which is what the rounding of allocation sizes is for.
        let granules = len.div_ceil(GRANULE);
        for _ in 0..granules {
            // SAFETY: the caller says `[lo, lo + len)` is mapped, so every one of the granules it
            // covers has a slot in the shadow, and the walk stops at the last of them.
            unsafe {
                slot.write(version);
                slot = slot.add(1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand in for the shadow, and a stand in for the program memory it shadows.
    ///
    /// The addresses are invented, because nothing in this file dereferences them: it only
    /// divides them. Inventing them keeps the tests away from whatever the test harness's own
    /// allocator is doing, and lets the fake own the only unsafe in here, so that the tests
    /// below read as the plain statements about lifetime that they are.
    struct Fake {
        /// Never read through this name. It is here so that the buffer the plane's arithmetic
        /// lands in lives exactly as long as the plane does.
        _shadow: std::vec::Vec<Version>,
        plane: Lifetime,
        base: usize,
    }

    impl Fake {
        /// A plane over `granules` granules of pretend memory starting at a pretend address.
        fn new(granules: usize) -> Self {
            let base = 0x1_0000;
            let mut shadow = std::vec![DEAD; granules];
            // Solve origin + (base / GRANULE) * SLOT = the buffer, which is the same arithmetic
            // the startup code will do once it knows where it put the reservation.
            let origin = (shadow.as_mut_ptr() as usize) - (base / GRANULE) * SLOT;
            // SAFETY: the buffer covers exactly the granules the tests ask about, it outlives the
            // plane because both are fields of this struct, and the offset above is what makes
            // the plane's arithmetic land inside it.
            let plane = unsafe { Lifetime::new(origin) };
            Self { _shadow: shadow, plane, base }
        }

        fn begin(&self, offset: usize, len: usize, version: Version) {
            // SAFETY: within the buffer, by the caller keeping to the granules it asked for.
            unsafe { self.plane.begin(self.base + offset, len, version) }
        }

        fn end(&self, offset: usize, len: usize, version: Version) {
            // SAFETY: as above.
            unsafe { self.plane.end(self.base + offset, len, version) }
        }

        fn live(&self, offset: usize, version: Version) -> bool {
            // SAFETY: as above.
            unsafe { self.plane.live(self.base + offset, version) }
        }

        fn version(&self, offset: usize) -> Version {
            // SAFETY: as above.
            unsafe { self.plane.version(self.base + offset) }
        }

        fn slot(&self, offset: usize) -> usize {
            self.plane.slot(self.base + offset) as usize
        }
    }

    #[test]
    fn a_freed_range_stays_refused_after_the_address_is_handed_out_again() {
        // The property the whole mechanism exists for, and the one quarantine does not have.
        // Everything else in this file is arithmetic in support of this test.
        let fake = Fake::new(64);
        let versions = Counter::new();

        let first = versions.next();
        fake.begin(0, 128, first);
        assert!(fake.live(0, first));

        fake.end(0, 128, versions.next());
        assert!(!fake.live(0, first));

        // The same address, a new owner, and the pointer from before is still refused rather
        // than quietly reading whatever moved in.
        let second = versions.next();
        fake.begin(0, 128, second);
        assert!(fake.live(0, second));
        assert!(!fake.live(0, first));
    }

    #[test]
    fn every_byte_of_an_instance_answers_and_the_byte_after_it_does_not() {
        // The plane is per granule, so a range that ends inside a granule owns the whole of it.
        // What must not happen is the next granule answering, because that is the neighbour.
        let fake = Fake::new(64);
        let version = 7;

        fake.begin(0, 40, version);

        for offset in 0..48 {
            assert!(fake.live(offset, version), "byte {offset} is not live");
        }
        assert!(!fake.live(48, version));
    }

    #[test]
    fn a_capability_that_never_named_anything_is_refused_over_untouched_memory() {
        // A fresh reservation reads as zero and the bottom capability carries zero, so a plain
        // compare would say the two agree. They agree about nothing, which is the point of Y1:
        // reading a word that is not a pointer and dereferencing it has to fail.
        let fake = Fake::new(64);

        assert_eq!(fake.version(0), DEAD);
        assert!(!fake.live(0, DEAD));
    }

    #[test]
    fn one_instance_ending_leaves_the_one_beside_it_alone() {
        // Instances are adjacent in a real heap far more often than not, so a fill that ran one
        // granule long would be a bug that only showed up under load.
        let fake = Fake::new(64);

        let left = 11;
        let right = 22;
        fake.begin(0, 64, left);
        fake.begin(64, 64, right);

        fake.end(0, 64, 33);

        assert!(!fake.live(0, left));
        assert!(fake.live(64, right));
        assert!(fake.live(127, right));
    }

    #[test]
    fn no_version_is_handed_out_twice() {
        // The one thing the counter has to do. A repeated version is a freed pointer that starts
        // working again, which is worse than not checking at all, because it is checked and wrong.
        let versions = Counter::new();
        let mut seen = std::collections::HashSet::new();
        for _ in 0..1000 {
            assert!(seen.insert(versions.next()));
        }
        assert!(!seen.contains(&DEAD));
    }

    #[test]
    fn a_slot_says_on_its_own_whether_anybody_owns_the_granule() {
        // What the low bit is for. A check that is handed an address and no capability has no
        // other way to tell a live range from one that was given back, since both hold a version,
        // and that is the situation every check in milestone S1 is in.
        let versions = Counter::new();
        let owner = begun(versions.next());

        assert!(owned(owner));
        assert!(!owned(ended(owner)));
        assert!(!owned(DEAD));
        assert_ne!(ended(owner), owner, "a freed range still answers to the old capability");

        // And the next instance to be given the range is owned again, rather than inheriting
        // whatever the last one left.
        assert!(owned(begun(versions.next())));
    }

    #[test]
    fn the_slot_for_an_address_is_the_slot_for_its_granule() {
        // The arithmetic on its own, because it is what the backend emits inline rather than
        // calls, and a shift that is off by one is a plane that reads its neighbour.
        let fake = Fake::new(64);

        for offset in 0..GRANULE {
            assert_eq!(fake.slot(offset), fake.slot(0));
        }
        assert_eq!(fake.slot(GRANULE) - fake.slot(0), SLOT);
    }
}
