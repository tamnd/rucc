//! The type plane: what type the bytes at an address were last stored through.
//!
//! Design: `spec/safe-memory/09-type-init-and-races.md` section 9.1, and the layout in
//! `spec/safe-memory/05-representation.md` sections 5.2.3 and 5.2.5.
//!
//! C 6.5 says an object's effective type is set by a store through a non character lvalue, and that
//! an access through a character type is always permitted. This plane is that rule written down:
//! a store records the type it stored through, an access asks whether what it is about to read
//! agrees with what is there, and the two character clauses are [`compatible`]. Four of document
//! 03's type classes and one of its spatial classes come from it.
//!
//! The one class everybody cares about most is not here and is free. Reading a word that is not a
//! pointer and dereferencing the result is caught by the aux plane, because an aux slot whose
//! payload was never a pointer holds the bottom capability and the first access through it fails.
//! That is why document 04's plane table says pointer slot only for Tier E: the expensive plane
//! buys the other four classes and not that one.
//!
//! # The granule is eight bytes and the slot is four
//!
//! One `(homogeneous, TypeId)` pair per eight bytes of program memory, which is four bytes of
//! shadow for every eight bytes of program, and a side entry of eight `TypeId` for a granule whose
//! bytes disagree. That is the `4/g + 4h` of section 5.2.3 with `g` of eight, and the budget it is
//! being held to is 1.25 bytes per byte.
//!
//! Eight rather than sixteen is measured rather than assumed, and section 5.2.5 has the method.
//! On a sixty four bit target the unit of a distinct type is eight bytes, because a pointer, a
//! `long` and a `double` are all eight, so a sixteen byte granule holds two of them and a structure
//! that alternates two types disagrees in every one of its granules. SQLite's declarations are 13
//! percent heterogeneous at eight bytes and 65 percent at sixteen, and the plane costs 1.00 bytes
//! per byte at eight and 2.84 at sixteen.
//!
//! # A granule that has once disagreed keeps its side entry
//!
//! [`Side`] hands out entries and never takes one back. A granule that becomes heterogeneous holds
//! its entry for the life of the program, and a later store that covers the whole granule with one
//! type writes that type over all eight bytes rather than folding the granule back up.
//!
//! What that buys is an allocator that is one `fetch_add` and has no free list, no lock and no
//! ordering to get right, on a path that generated code reaches from any thread. What it costs is
//! one indirection on every later read of that granule and thirty two bytes that stay out. The
//! bound is one entry per granule that has ever disagreed, which is the same `h` the budget above
//! is written in as long as `h` is read as ever rather than as now. Reclaiming them is a free list
//! and the spin lock discipline `crate::alloc` already has, and it is not worth either until a
//! measurement says which programs fold granules back up often enough to care.
//!
//! # Running out of side entries loses checking rather than gaining false positives
//!
//! A granule that has to disagree and cannot get an entry is written as [`UNTYPED`], which
//! [`compatible`] permits everything against. That is the same direction section 9.1 chooses for
//! bytes that uninstrumented code wrote, and it is the only direction available to a tool whose
//! false positive rate is a release blocking property: the plane's coverage thins out and nothing
//! it says becomes wrong.
//!
//! # What is here so far
//!
//! The vocabulary, the compatibility rule, the arithmetic and the reads and writes, with the
//! shadow and the side table handed in. Nothing here maps either of them, for the reason
//! [`crate::plane`] gives about the lifetime plane, and nothing here is reached from generated code
//! yet: the judgement that sets a type on a store and the check that reads it are the compiler's
//! half of the same milestone.

use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

/// What the plane holds for a byte, and the compiler's own interned type universe.
///
/// `TypeId` is the parent's, from document 07, so the plane's vocabulary is exactly the compiler's
/// and a report can name a type in the spelling the program used for it. What this file adds is
/// the handful of values below, which are types the compiler has no name for.
pub type TypeId = u32;

/// The type of a byte nothing has stored through, and of a byte only uninstrumented code wrote.
///
/// A fresh shadow reservation reads as zero, so untouched address space already says this and
/// nothing has to walk it. [`compatible`] permits every access against it, which is C's rule for
/// storage with no declared type and is also the only answer that does not make every boundary a
/// false positive.
pub const UNTYPED: TypeId = 0;

/// The type of a byte stored through a character lvalue, which is compatible with everything.
///
/// This is what makes the byte at a time copy idiom work rather than being a special case bolted
/// on. `char *p = (char *)&s; p[3] = 0;` sets byte 3 of `s` to this and leaves the other bytes
/// saying what the field said, so a later read of `s.f` still sees the field's type over three of
/// its four bytes and this over one, and both are permitted.
pub const CHARACTER: TypeId = 1;

/// How many bytes of a pointer there are to name separately.
const POINTER_BYTES: u32 = 8;

/// The first of the pointer byte ids.
const POINTER_FIRST: TypeId = 2;

/// The first id this file does not spend on something of its own.
///
/// The compiler's universe is biased past the values above rather than sharing them, because the
/// compiler numbers its types from zero and zero already means something here.
pub const FIRST_INTERNED: TypeId = POINTER_FIRST + POINTER_BYTES;

/// The type of byte `k` of a pointer shaped word.
///
/// A pointer fills a granule on both of the parent's sixty four bit targets, so an aligned one
/// never needs these and a packed or misaligned one does. They are kept apart per byte rather than
/// being one pointer type so that a word assembled out of the bytes of two different pointers is a
/// word whose bytes disagree, which is the whole of class Y5.
///
/// # Panics
///
/// Never in a release build. `k` is a byte index into a pointer and a caller that hands over a
/// larger one has computed it wrong.
#[must_use]
pub const fn pointer_byte(k: u32) -> TypeId {
    debug_assert!(k < POINTER_BYTES, "a pointer has eight bytes");
    POINTER_FIRST + k
}

/// What the compiler's `n`th interned type is called here.
#[must_use]
pub const fn interned(n: u32) -> TypeId {
    FIRST_INTERNED + n
}

/// Whether reading bytes that say `stored` as a `wanted` is permitted.
///
/// Four clauses, and each of them is C 6.5 rather than a concession to it. A byte nothing has
/// stored through takes the type of the access, which is what the standard says about storage with
/// no declared type. A byte stored through a character type is compatible with everything, and an
/// access through a character type is permitted against anything, which are the two halves of the
/// character rule. What is left is the types agreeing.
#[must_use]
pub const fn compatible(wanted: TypeId, stored: TypeId) -> bool {
    stored == UNTYPED || stored == CHARACTER || wanted == CHARACTER || wanted == stored
}

/// How many bytes of program memory one slot covers.
pub const GRANULE: usize = 8;

/// What one granule's worth of plane is, which is a flag and an identifier.
type Slot = u32;

/// How many bytes of shadow one granule needs.
pub const SLOT: usize = size_of::<Slot>();

/// How many bytes one side entry is, which is one identifier per byte of a granule.
pub const ENTRY: usize = GRANULE * size_of::<TypeId>();

/// Set in a slot whose granule's bytes do not all say the same thing.
///
/// The top bit, so that a slot that is clear of it is the identifier itself with nothing to mask
/// off, which is the case the fast path is for. A `TypeId` therefore has thirty one bits, which is
/// two billion distinct types in one program.
const HETEROGENEOUS: Slot = 1 << 31;

/// The largest identifier a slot can hold, and the largest side entry index.
const LIMIT: u32 = HETEROGENEOUS - 1;

/// The largest id the compiler may number a type with.
///
/// The same number as the one above, under the name the other end of the contract knows it by. An
/// id past this has the bit that says a granule's bytes disagree, so a store of one over a whole
/// granule leaves a slot that later reads as an index into the side table, and the plane then reads
/// whatever address that index works out to. The compiler mirrors this beside [`FIRST_INTERNED`],
/// and that is the whole of what keeps it from happening.
pub const LAST_INTERNED: TypeId = LIMIT;

/// The table of per byte entries for the granules whose bytes disagree.
///
/// Separate from the plane itself because it is allocated rather than direct mapped: a direct
/// mapped side table would be the uncompressed plane, at four bytes of shadow per byte of program,
/// which is the 4x that section 5.2.3 says puts Tier D's memory budget out of reach.
#[derive(Debug)]
pub struct Side {
    /// Where entry zero is, or zero before the table is mapped.
    base: AtomicUsize,
    /// How many entries there is room for.
    room: AtomicU32,
    /// The next entry nobody has taken.
    next: AtomicU32,
}

impl Side {
    /// A table with no room in it, which answers every request with none.
    ///
    /// That is the right state for a program whose startup has not mapped the table yet rather
    /// than a state to guard against: a plane with no side table records a granule that disagrees
    /// as [`UNTYPED`] and stops saying anything about it, which is the same graceful thinning the
    /// module comment describes for a table that fills up.
    #[must_use]
    pub const fn new() -> Self {
        Self { base: AtomicUsize::new(0), room: AtomicU32::new(0), next: AtomicU32::new(0) }
    }

    /// Says where the table is and how many entries it holds.
    ///
    /// # Safety
    ///
    /// `base` is the start of `room * ENTRY` writable bytes that outlive every use of this table,
    /// and no entry has been handed out yet.
    pub unsafe fn map(&self, base: usize, room: u32) {
        self.base.store(base, Ordering::Relaxed);
        self.room.store(room.min(LIMIT), Ordering::Relaxed);
    }

    /// Takes an entry nobody else has, or none when the table is full.
    ///
    /// One `fetch_add` and a comparison, which is what the module comment trades reclaiming for.
    /// The counter saturates rather than wrapping, so a program that asks a few billion times
    /// after the table is full keeps getting none rather than being handed entry zero again.
    fn take(&self) -> Option<u32> {
        let room = self.room.load(Ordering::Relaxed);
        let index = self.next.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
            (next < room).then_some(next + 1)
        });
        index.ok()
    }

    /// Where entry `index` is, which the caller has taken and nobody else holds.
    fn entry(&self, index: u32) -> *mut TypeId {
        let base = self.base.load(Ordering::Relaxed);
        (base + (index as usize) * ENTRY) as *mut TypeId
    }
}

impl Default for Side {
    fn default() -> Self {
        Self::new()
    }
}

/// The direct mapped shadow the plane lives in, and the side table beside it.
///
/// `origin` works the way [`crate::plane::Lifetime`]'s does and is a bias rather than an address
/// for the same reason.
#[derive(Clone, Copy, Debug)]
pub struct Types<'a> {
    /// Where the slot for address zero would be.
    origin: usize,
    /// Where a granule whose bytes disagree keeps them.
    side: &'a Side,
}

impl<'a> Types<'a> {
    /// A plane whose slot for address 0 would be at `origin`.
    ///
    /// # Safety
    ///
    /// Every address this plane is later asked about must land inside a mapping the caller owns
    /// and keeps for as long as the plane is used. Nothing here range checks, because the point of
    /// a direct map is that there is nothing to check.
    #[must_use]
    pub const unsafe fn new(origin: usize, side: &'a Side) -> Self {
        Self { origin, side }
    }

    /// Where the slot for `addr` is kept.
    #[must_use]
    pub const fn slot(&self, addr: usize) -> *mut Slot {
        // Modular, because the origin is a bias and a bias may have wrapped, which is the same
        // arithmetic and the same reason as `crate::plane::Lifetime::slot`.
        self.origin.wrapping_add((addr / GRANULE) * SLOT) as *mut Slot
    }

    /// The type the byte at `addr` was last stored through.
    ///
    /// One shift, one load, and a second load only for a granule whose bytes disagree.
    ///
    /// # Safety
    ///
    /// `addr` is inside the mapping this plane was built for.
    #[must_use]
    pub unsafe fn read(&self, addr: usize) -> TypeId {
        // SAFETY: the caller says `addr` is mapped, and the slot for a mapped address is inside
        // the shadow reservation this plane was built over, aligned by construction.
        let slot = unsafe { self.slot(addr).read() };
        if slot & HETEROGENEOUS == 0 {
            return slot;
        }
        // SAFETY: the flag is only ever set beside an index this plane took from the side table,
        // and the table hands out an entry to one caller and keeps it mapped from then on.
        unsafe { self.side.entry(slot & LIMIT).add(addr % GRANULE).read() }
    }

    /// Whether an access of `len` bytes at `lo` through a `wanted` is permitted.
    ///
    /// Granule at a time, because the answer for a granule whose bytes agree is one comparison
    /// however many of its bytes the access covers, and an access that covers whole granules is
    /// what nearly every access is.
    ///
    /// # Safety
    ///
    /// `[lo, lo + len)` is inside the mapping this plane was built for.
    #[must_use]
    pub unsafe fn allows(&self, lo: usize, len: usize, wanted: TypeId) -> bool {
        // A character access is permitted against everything, so there is nothing to walk.
        if wanted == CHARACTER {
            return true;
        }
        let mut at = lo;
        while at < lo + len {
            // SAFETY: `at` is inside the range the caller says is mapped.
            let slot = unsafe { self.slot(at).read() };
            let next = (at / GRANULE + 1) * GRANULE;
            if slot & HETEROGENEOUS == 0 {
                if !compatible(wanted, slot) {
                    return false;
                }
                at = next;
                continue;
            }
            while at < next.min(lo + len) {
                // SAFETY: as in `read`, whose body this is.
                let stored = unsafe { self.side.entry(slot & LIMIT).add(at % GRANULE).read() };
                if !compatible(wanted, stored) {
                    return false;
                }
                at += 1;
            }
        }
        true
    }

    /// The judgement a store makes: `[lo, lo + len)` was stored through a `ty`.
    ///
    /// # Panics
    ///
    /// Never in a release build. An id past [`LAST_INTERNED`] is a compiler that did not reduce its
    /// hash into the range this plane has, and the plane it would leave behind reads as a side entry
    /// index rather than as a type.
    ///
    /// # Safety
    ///
    /// `[lo, lo + len)` is inside the mapping this plane was built for.
    pub unsafe fn set(&self, lo: usize, len: usize, ty: TypeId) {
        debug_assert!(ty <= LAST_INTERNED, "an id the plane has no room for");
        let mut at = lo;
        while at < lo + len {
            let next = (at / GRANULE + 1) * GRANULE;
            let end = next.min(lo + len);
            if at % GRANULE == 0 && end == next {
                // SAFETY: the whole granule is the caller's range, so nothing else's type is
                // being written over.
                unsafe { self.whole(at, ty) }
            } else {
                // SAFETY: as above, for the part of the granule the range covers.
                unsafe { self.part(at, end, ty) }
            }
            at = end;
        }
    }

    /// The `memcpy` rule: the bytes at `dst` now say whatever the bytes at `src` say.
    ///
    /// C says a copy through `memcpy` or through a character array carries the source's effective
    /// type, so this is what makes the punning idiom work, and it is also what makes it checked:
    /// copying a `struct A` over a `struct B` and reading it back as a `struct B` is caught,
    /// because the plane says the bytes are an `A`.
    ///
    /// # Safety
    ///
    /// Both ranges are inside the mapping this plane was built for. They may overlap, and the
    /// answer is the same either way, because the type of a byte does not depend on the type of
    /// the byte beside it.
    pub unsafe fn copy(&self, dst: usize, src: usize, len: usize) {
        for i in 0..len {
            // SAFETY: both addresses are inside the ranges the caller says are mapped.
            unsafe {
                let ty = self.read(src + i);
                self.set(dst + i, 1, ty);
            }
        }
    }

    /// Writes one type over a granule the caller owns the whole of.
    ///
    /// A granule that already disagrees keeps its side entry and has the type written over all
    /// eight bytes, rather than being folded back up, which is the trade the module comment makes.
    unsafe fn whole(&self, at: usize, ty: TypeId) {
        // SAFETY: `at` is mapped, which is this function's own contract passed straight on.
        let slot = unsafe { self.slot(at).read() };
        if slot & HETEROGENEOUS == 0 {
            // SAFETY: as above.
            unsafe { self.slot(at).write(ty) };
            return;
        }
        let entry = self.side.entry(slot & LIMIT);
        for byte in 0..GRANULE {
            // SAFETY: the entry is eight identifiers long and this walks exactly those.
            unsafe { entry.add(byte).write(ty) };
        }
    }

    /// Writes one type over `[at, end)`, which is part of one granule and not the whole of it.
    unsafe fn part(&self, at: usize, end: usize, ty: TypeId) {
        // SAFETY: `at` is mapped, which is this function's own contract passed straight on.
        let slot = unsafe { self.slot(at).read() };
        let index = if slot & HETEROGENEOUS != 0 {
            slot & LIMIT
        } else {
            // Nothing to do when the granule already says this everywhere, which is the case a
            // loop storing one type a field at a time is in after its first turn.
            if slot == ty {
                return;
            }
            let Some(index) = self.side.take() else {
                // No entry to be had, so the granule stops saying anything rather than saying
                // something wrong about the bytes the store did not cover.
                // SAFETY: as above.
                unsafe { self.slot(at).write(UNTYPED) };
                return;
            };
            let entry = self.side.entry(index);
            for byte in 0..GRANULE {
                // SAFETY: the entry is this plane's now and is eight identifiers long.
                unsafe { entry.add(byte).write(slot) };
            }
            // SAFETY: as above. Written after the entry is filled, because a reader that saw the
            // flag before the bytes were there would read whatever the table was last used for.
            unsafe { self.slot(at).write(HETEROGENEOUS | index) };
            index
        };
        let entry = self.side.entry(index);
        let first = at % GRANULE;
        for byte in first..first + (end - at) {
            // SAFETY: the range is inside one granule, so the walk stays inside the entry.
            unsafe { entry.add(byte).write(ty) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand in for the shadow, the side table, and the program memory they shadow.
    ///
    /// The addresses are invented, because nothing in this file dereferences them: it only divides
    /// them. That keeps the tests away from whatever the harness's own allocator is doing and lets
    /// the fake own the only unsafe in here, the same way `crate::plane`'s does.
    struct Fake {
        /// Never read through this name. It is here so the buffer the plane's arithmetic lands in
        /// lives exactly as long as the plane does.
        _shadow: std::vec::Vec<Slot>,
        /// As above, for the entries a granule that disagrees is given.
        _entries: std::vec::Vec<TypeId>,
        side: std::boxed::Box<Side>,
        origin: usize,
        base: usize,
    }

    impl Fake {
        /// A plane over `granules` granules of pretend memory, with room for `room` of them to
        /// disagree.
        fn new(granules: usize, room: u32) -> Self {
            let base = 0x1_0000;
            let mut shadow = std::vec![UNTYPED; granules];
            let mut entries = std::vec![UNTYPED; room as usize * GRANULE];
            // Solve origin + (base / GRANULE) * SLOT = the buffer, which is the same arithmetic
            // the startup code will do once it knows where it put the reservation.
            let origin = (shadow.as_mut_ptr() as usize) - (base / GRANULE) * SLOT;
            let side = std::boxed::Box::new(Side::new());
            // SAFETY: the buffer holds exactly `room` entries and outlives the table, because both
            // are fields of this struct and the table is behind a box that does not move.
            unsafe { side.map(entries.as_mut_ptr() as usize, room) };
            Self { _shadow: shadow, _entries: entries, side, origin, base }
        }

        /// The plane itself, borrowed for the length of one call.
        fn plane(&self) -> Types<'_> {
            // SAFETY: the buffers cover the granules the tests ask about and outlive the borrow.
            unsafe { Types::new(self.origin, &self.side) }
        }

        fn set(&self, offset: usize, len: usize, ty: TypeId) {
            // SAFETY: within the buffer, by the caller keeping to the granules it asked for.
            unsafe { self.plane().set(self.base + offset, len, ty) }
        }

        fn read(&self, offset: usize) -> TypeId {
            // SAFETY: as above.
            unsafe { self.plane().read(self.base + offset) }
        }

        fn allows(&self, offset: usize, len: usize, ty: TypeId) -> bool {
            // SAFETY: as above.
            unsafe { self.plane().allows(self.base + offset, len, ty) }
        }

        fn copy(&self, dst: usize, src: usize, len: usize) {
            // SAFETY: as above.
            unsafe { self.plane().copy(self.base + dst, self.base + src, len) }
        }
    }

    /// The compiler's first two types, in the spelling this file gives them.
    const A: TypeId = interned(0);
    const B: TypeId = interned(1);

    #[test]
    fn reading_a_struct_back_as_the_other_struct_it_was_copied_from_is_caught() {
        // The class this plane exists for, and the test everything else here is arithmetic in
        // support of. A `struct A` is copied over a `struct B` and read as a `B`, and the plane
        // says the bytes are an `A`.
        let fake = Fake::new(64, 8);

        fake.set(0, 32, A);
        fake.set(64, 32, B);
        assert!(fake.allows(64, 32, B));

        fake.copy(64, 0, 32);

        assert!(!fake.allows(64, 32, B), "the bytes are an A now");
        assert!(fake.allows(64, 32, A));
    }

    #[test]
    fn the_largest_id_there_is_still_reads_back_as_a_type() {
        // The top of the range, where a slot is all ones but for the bit that says the bytes
        // disagree. A whole granule stored through it has to read back as that type rather than as
        // a granule with a side entry, since the bit is the only thing telling the two apart.
        let fake = Fake::new(64, 8);

        fake.set(0, 8, LAST_INTERNED);

        assert_eq!(fake.read(0), LAST_INTERNED);
        assert!(fake.allows(0, 8, LAST_INTERNED));
        assert!(!fake.allows(0, 8, A));
    }

    #[test]
    fn a_byte_written_through_a_character_lvalue_leaves_the_rest_of_the_field_alone() {
        // C 6.5's character rule, which is the idiom every serialization loop in C is written in.
        // A false positive here would be the plane refusing `memcpy` open coded, which is most of
        // why nobody deploys a type checker.
        let fake = Fake::new(64, 8);

        fake.set(0, 8, A);
        fake.set(3, 1, CHARACTER);

        assert_eq!(fake.read(2), A);
        assert_eq!(fake.read(3), CHARACTER);
        assert_eq!(fake.read(4), A);
        assert!(fake.allows(0, 8, A), "one character byte does not refuse the field");
    }

    #[test]
    fn a_byte_nothing_has_stored_through_takes_the_type_of_whoever_reads_it() {
        // The direction the whole design leans, and the one that makes the plane thin out at a
        // boundary rather than shouting. Uninstrumented code leaves bytes looking like this.
        let fake = Fake::new(64, 8);

        assert_eq!(fake.read(0), UNTYPED);
        assert!(fake.allows(0, 8, A));
        assert!(fake.allows(0, 8, B));
    }

    #[test]
    fn two_types_in_one_granule_are_both_kept() {
        // The whole reason there is a side table. Without one the second store would write over
        // the first and the field it covered would then be refused.
        let fake = Fake::new(64, 8);

        fake.set(0, 4, A);
        fake.set(4, 4, B);

        for byte in 0..4 {
            assert_eq!(fake.read(byte), A, "byte {byte}");
        }
        for byte in 4..8 {
            assert_eq!(fake.read(byte), B, "byte {byte}");
        }
        assert!(fake.allows(0, 4, A));
        assert!(fake.allows(4, 4, B));
        assert!(!fake.allows(0, 8, A), "the granule is not all an A");
    }

    #[test]
    fn a_granule_that_disagreed_and_then_agreed_again_still_answers() {
        // It keeps its side entry rather than folding back up, which is the trade the module
        // comment makes, and what must not change is the answer.
        let fake = Fake::new(64, 8);

        fake.set(0, 4, A);
        fake.set(4, 4, B);
        fake.set(0, 8, A);

        for byte in 0..8 {
            assert_eq!(fake.read(byte), A, "byte {byte}");
        }
        assert!(fake.allows(0, 8, A));
        assert!(!fake.allows(0, 8, B));
    }

    #[test]
    fn a_store_that_covers_whole_granules_takes_no_side_entry_at_all() {
        // The case that has to stay cheap, since it is nearly every store. A table with no room
        // in it answers none to every request, so a plane that needed an entry here would lose
        // the types instead of keeping them, which is what this asserts against.
        let fake = Fake::new(64, 0);

        fake.set(0, 32, A);

        assert!(fake.allows(0, 32, A));
        assert!(!fake.allows(0, 32, B));
    }

    #[test]
    fn a_granule_that_cannot_have_an_entry_stops_saying_anything() {
        // Running out of side entries thins the plane out rather than making it wrong. The bytes
        // the store covered are not recorded either, which is a lost check and not a false
        // positive, and that is the direction document 09 picks on purpose.
        let fake = Fake::new(64, 0);

        fake.set(0, 8, A);
        fake.set(0, 4, B);

        assert_eq!(fake.read(0), UNTYPED);
        assert_eq!(fake.read(4), UNTYPED);
        assert!(fake.allows(0, 8, A));
        assert!(fake.allows(0, 8, B));
    }

    #[test]
    fn storing_the_same_type_over_part_of_a_granule_that_already_says_it_costs_nothing() {
        // A loop that fills a structure one field at a time reaches this on every turn after the
        // first, and an entry taken per turn would empty the table on a program that does it.
        let fake = Fake::new(64, 1);

        fake.set(0, 8, A);
        for _ in 0..100 {
            fake.set(0, 4, A);
        }

        // The one entry is still there for a granule that really does disagree.
        fake.set(8, 4, A);
        fake.set(12, 4, B);
        assert_eq!(fake.read(8), A);
        assert_eq!(fake.read(12), B);
    }

    #[test]
    fn an_access_is_refused_over_a_byte_it_does_not_reach() {
        // The plane is per byte inside a granule that disagrees, so a walk that rounded the range
        // out to the granule would refuse an access that never touched the byte it disagreed on.
        let fake = Fake::new(64, 8);

        fake.set(0, 4, A);
        fake.set(4, 4, B);

        assert!(fake.allows(0, 4, A));
        assert!(fake.allows(0, 3, A));
        assert!(!fake.allows(3, 2, A), "the fifth byte is a B");
    }

    #[test]
    fn a_copy_out_of_a_granule_that_disagrees_carries_both_types() {
        // The `memcpy` rule byte by byte, which is the only way it can be stated: the destination
        // granule disagrees in the same places the source one does.
        let fake = Fake::new(64, 8);

        fake.set(0, 4, A);
        fake.set(4, 4, B);

        fake.copy(16, 0, 8);

        for byte in 0..4 {
            assert_eq!(fake.read(16 + byte), A, "byte {byte}");
        }
        for byte in 4..8 {
            assert_eq!(fake.read(16 + byte), B, "byte {byte}");
        }
    }

    #[test]
    fn the_character_rules_are_both_of_them() {
        // A character byte is readable as anything, and a character access reads anything. The
        // first is the copy idiom and the second is every `strlen` in the program.
        assert!(compatible(A, CHARACTER));
        assert!(compatible(CHARACTER, A));
        assert!(compatible(A, UNTYPED));
        assert!(compatible(A, A));
        assert!(!compatible(A, B));
    }

    #[test]
    fn a_character_access_never_walks_the_plane() {
        // There is nothing it could be refused by, so the walk is skipped rather than run and
        // thrown away. What this asserts is the answer, since the saving is not observable.
        let fake = Fake::new(64, 8);

        fake.set(0, 4, A);
        fake.set(4, 4, B);

        assert!(fake.allows(0, 8, CHARACTER));
    }

    #[test]
    fn the_bytes_of_a_pointer_are_eight_different_types() {
        // Class Y5 is a word assembled out of the bytes of two pointers, and it is only visible
        // if the bytes are told apart. A single pointer type would say that word is a pointer.
        let mut seen = std::collections::HashSet::new();
        for k in 0..8 {
            assert!(seen.insert(pointer_byte(k)));
            assert!(!compatible(pointer_byte(k), interned(0)));
        }
        assert!(!seen.contains(&UNTYPED));
        assert!(!seen.contains(&CHARACTER));
        assert!(seen.iter().all(|id| *id < FIRST_INTERNED));
    }

    #[test]
    fn the_slot_for_an_address_is_the_slot_for_its_granule() {
        // The arithmetic on its own, because it is what the backend emits inline rather than
        // calls, and a shift that is off by one is a plane that reads its neighbour.
        let fake = Fake::new(64, 8);
        let plane = fake.plane();

        for offset in 0..GRANULE {
            assert_eq!(plane.slot(fake.base + offset), plane.slot(fake.base));
        }
        assert_eq!(plane.slot(fake.base + GRANULE) as usize - plane.slot(fake.base) as usize, SLOT);
    }

    #[test]
    fn one_granule_is_written_without_the_one_beside_it_moving() {
        // Structures are adjacent far more often than not, so a walk that ran one granule long
        // would be a bug that only showed up under load.
        let fake = Fake::new(64, 8);

        fake.set(0, 8, A);
        fake.set(8, 8, B);
        fake.set(0, 4, CHARACTER);

        assert_eq!(fake.read(7), A);
        assert_eq!(fake.read(8), B);
        assert_eq!(fake.read(15), B);
    }

    #[test]
    fn a_table_hands_out_every_entry_it_has_and_then_stops() {
        // The counter saturates rather than wrapping, because handing entry zero out twice is two
        // granules sharing eight bytes and each of them overwriting the other.
        let entries = std::vec![UNTYPED; 3 * GRANULE];
        let side = Side::new();
        // SAFETY: the buffer holds three entries and outlives this test's use of the table.
        unsafe { side.map(entries.as_ptr() as usize, 3) };

        assert_eq!(side.take(), Some(0));
        assert_eq!(side.take(), Some(1));
        assert_eq!(side.take(), Some(2));
        for _ in 0..100 {
            assert_eq!(side.take(), None);
        }
    }

    #[test]
    fn a_table_nobody_has_mapped_answers_none() {
        // Which is the state before startup has run, and the plane has to keep working through it
        // rather than writing into address zero.
        let side = Side::new();
        assert_eq!(side.take(), None);
    }
}
