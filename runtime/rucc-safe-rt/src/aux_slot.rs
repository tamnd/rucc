//! The aux slot: the sixteen bytes that sit beside a pointer in memory.
//!
//! Design: `spec/safe-memory/05-representation.md` sections 5.2.2 and 5.2.7, and
//! `spec/safe-memory/17-open-questions.md` question 5, which is the decision about what goes in
//! here and was answered in tamnd/rucc#1068.
//!
//! The file is not called `aux.rs` because it cannot be. `AUX` is a reserved device name on Windows
//! whatever extension follows it, so git refuses to write such a path and a tree holding one cannot
//! be checked out there at all, which is a failure before any crate is compiled. `xtask/src/aux.rs`
//! was renamed for the same reason once already. The word itself is the spec's and stays.
//!
//! A capability in flight is four words, and there are two here, so something has to give. What
//! gives is not the numbers. [`Slot`] keeps the lifetime version at its full width, keeps the part
//! of [`Meta`] a check reads, and says where the object starts and how far it runs exactly, as a
//! displacement and an extent of twenty one bits each, which is the largest pair that fits and
//! fills the sixteen bytes to the bit. An object too long for that sets a flag instead, and its
//! reader goes to the object's header for the two numbers, which is a load from a line
//! `cap_of` has already touched.
//!
//! # Why exact rather than compressed
//!
//! The straw man was CHERI Concentrate, an exponent and two mantissas, which rounds a range
//! outward until it fits. `cargo xtask compress` measured what that rounds off and section 5.2.7
//! has the tables. The short of it is that the rounding lands in the wrong place. A member inside
//! a structure is exact from twelve bits of mantissa up, so intra-object overflow, which is the
//! class this design claims over Fil-C, was never at risk. A whole heap object is the one that is
//! rounded, at fourteen bits only 77 percent of them are exact, and no allocator repairs it: over
//! aligning the block settles where the object starts and the top still has to round up, because
//! the length is the program's rather than the allocator's. What is left is a slack of about one
//! part in two to the mantissa, which at fourteen bits is up to sixty four bytes past a one
//! megabyte buffer. Those are bytes no bounds check could refuse, and a redzone allocator refuses
//! them today, so the compressed scheme would have been a monitor that is weaker than ASan on the
//! largest objects in the program.
//!
//! # Both numbers are relative to the pointer beside them
//!
//! `lo` is a full address and there is nowhere to put one, so it is not stored. The slot sits
//! beside the word it describes, and that word holds the pointer, which is in bounds or one past
//! the end because the derivation check refuses anything else before a store. So the displacement
//! from the pointer back to the base of its object is the only thing that has to be written down,
//! and the reader brings the pointer it just loaded. That is what makes twenty one bits enough for
//! a number that would otherwise be sixty four.
//!
//! # Where a slot is
//!
//! [`address_of`] is the other half of the format: not what the sixteen bytes say but which
//! sixteen bytes they are. Section 5.2.2's block puts the aux in front of the header in payload
//! order, so the slot for the word at `off` bytes into an object is `lo - 32 - aux(ext) + off / 8 *
//! 16`, and every term of that is either a constant or a field of the capability the check in front
//! of the store already holds. A store through a pointer whose object and offset are both known,
//! which is most of them, folds the whole expression to one displacement off the base.
//!
//! Only storage the allocator laid out has an aux. Automatic and static storage is where the design
//! says the aux goes beside the frame and in a section of the image, and neither exists yet, so
//! [`address_of`] answers nothing for them rather than pointing at whatever is in front of a stack
//! object. A caller that cannot tell the difference between no slot and a slot saying nothing would
//! refuse every pointer loaded out of a local, which is why [`load()`] reports the two separately.
//!
//! # What is not here
//!
//! `instance_id` is left in the header. Section 5.2.1 already calls it a debugging aid rather than
//! a safety critical field, a report is read out of the header anyway, and it is forty three bits
//! that a check never looks at, so an aux slot that carries it would be paying most of its budget
//! for something no judgement asks about.
//!
//! The second epoch stamp is not here either. The slot is full, so it lives in the epoch plane at
//! the slot's own address, which is a stamp the plane already has room for because it is mapped over
//! a whole region and the aux is inside one. `spec/safe-memory/09-type-init-and-races.md` section
//! 9.5 has the reasoning and [`crate::epoch`] has the plane.
//!
//! Nothing writes one of these yet. What is missing is the compiler's half, which is a `cap_store`
//! beside every store of a pointer, and that is milestone S5 work on tamnd/rucc#856.

use crate::layout::{self, Cap, Class, Meta, WORD};
use crate::plane::{DEAD, Version};

/// Bits of [`Meta`] a slot keeps, which is everything below `instance_id`.
const META: u32 = 21;

/// Bits each of the displacement and the extent get.
///
/// Twenty one is not a preference. A full version is sixty four bits and the meta above is twenty
/// one, which leaves forty three, and forty three is two of these and the flag.
const BOUND: u32 = 21;

/// Where the displacement starts in the packed word.
const OFF: u32 = META;

/// Where the extent starts in the packed word.
const EXT: u32 = META + BOUND;

/// The flag that says the two numbers did not fit and the header has them.
const IN_HEADER: u64 = 1 << (META + 2 * BOUND);

/// The largest displacement or extent a slot can hold, which is one byte under two megabytes.
///
/// The tables of section 5.2.7 call this two megabytes, because what they are reporting is the
/// scale a number of that width reaches. This is the number itself, and an object of exactly two
/// megabytes is one byte too long for it.
pub const EXACT: u64 = (1 << BOUND) - 1;

/// The sixteen bytes beside one pointer sized word of payload.
///
/// `#[repr(C)]` and two plain words, because generated code reaches into this. It is written by
/// the compiler's `cap_store` and read by its `cap_load`, and those are two ends of a format
/// rather than two uses of a Rust type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
#[repr(C)]
pub struct Slot {
    /// The lifetime version of the object the pointer in the paired word points into.
    pub ver: Version,
    /// The extent, the displacement, the flag and the meta bits, in that order from the top.
    pub packed: u64,
}

/// What a slot says, once the pointer beside it is brought along.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Read {
    /// The word holds no pointer, or holds one whose storage is gone.
    ///
    /// Untouched memory reads as this, which is section 5.2.2's point about document 03's class Y1
    /// costing nothing: an integer read as a pointer arrives with no capability and the first
    /// access through it is refused.
    ///
    /// Not always refused, and the exception is not this module's to make. A word a foreign writer
    /// filled reads as this too, and [`crate::cap::load`] is where the two are told apart, by a bit
    /// on the instance rather than by anything in the slot.
    Nothing,
    /// The whole capability, which is every object shorter than [`EXACT`] and a byte.
    Whole(Cap),
    /// The object is too long for the slot to say where it starts and how far it runs, so those
    /// two come out of its header and these two are what the slot had.
    Header {
        /// The lifetime version, which is in the slot whatever the object's size.
        ver: Version,
        /// The meta bits, likewise.
        meta: Meta,
    },
}

impl Slot {
    /// A slot for a word that holds no pointer.
    ///
    /// The encoding is [`crate::plane::DEAD`], which is zero, so the aux of a fresh allocation
    /// says this without being written at all.
    pub const EMPTY: Self = Self { ver: DEAD, packed: 0 };

    /// The slot for `cap`, given the pointer value `addr` that is being stored in the paired word.
    ///
    /// `addr` is not optional and is not the base. It is what the program is storing, and both
    /// numbers in the slot are relative to it, which is the trick that makes them fit.
    #[must_use]
    pub const fn of(cap: Cap, addr: u64) -> Self {
        if cap.is_bottom() {
            return Self::EMPTY;
        }
        let meta = cap.meta.0 & ((1 << META) - 1);
        // Wrapping rather than checked, because an address below the base wraps to something
        // enormous and is then too large to fit, which is the answer either way. A pointer that
        // far from its object is refused at the derivation check long before this, and if one ever
        // arrives here the slot says to ask the header rather than naming a different object.
        let off = addr.wrapping_sub(cap.lo);
        if off > EXACT || cap.ext > EXACT {
            return Self { ver: cap.ver, packed: meta | IN_HEADER };
        }
        Self { ver: cap.ver, packed: meta | (off << OFF) | (cap.ext << EXT) }
    }

    /// What this says about the pointer `addr` that was loaded from the paired word.
    #[must_use]
    pub const fn read(self, addr: u64) -> Read {
        if self.ver == DEAD {
            return Read::Nothing;
        }
        let meta = Meta(self.packed & ((1 << META) - 1));
        if self.packed & IN_HEADER != 0 {
            return Read::Header { ver: self.ver, meta };
        }
        let off = (self.packed >> OFF) & EXACT;
        let ext = (self.packed >> EXT) & EXACT;
        Read::Whole(Cap::new(addr.wrapping_sub(off), ext, self.ver, meta))
    }

    /// Whether this says the word holds no pointer.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.ver == DEAD
    }
}

/// Where the slot for the word at `addr` is, given the capability of the object that word is in.
///
/// `container` is the object being written into rather than the pointer being written. A store of
/// `p->next` brings the capability of `p`, and the capability of whatever `next` holds is the thing
/// that ends up in the slot this returns the address of.
///
/// Nothing for a word that has no slot, which is three cases and they are all different questions
/// the caller has already had to ask: storage that is not an allocation and so has no aux at all,
/// a word that is not inside the object, and a word that is not pointer aligned. The last is not a
/// refusal either. A pointer sized store at an odd offset is something a C program may legitimately
/// do through a packed structure, and what it means for the aux is that the capability cannot be
/// written down, not that the store is wrong.
#[must_use]
pub const fn address_of(container: Cap, addr: u64) -> Option<u64> {
    // Only the allocator lays a block out this way. A stack object's aux is beside the frame and a
    // static's is in a section of the image, and until those exist the honest answer for both is
    // that there is no slot rather than an address in front of somebody else's storage.
    if container.meta.class() != Class::Allocated as u8 {
        return None;
    }
    // The whole word has to be in the object, so a four byte tail at the end of a payload has no
    // slot, and neither does one past the end.
    if !container.covers(addr, WORD as u64) {
        return None;
    }
    let off = addr - container.lo;
    if off % WORD as u64 != 0 {
        return None;
    }
    // `layout::HEADER` rather than the `IN_HEADER` above: one is the thirty two bytes in front of
    // the payload and the other is the flag that sends a reader to them.
    let block = container.lo - layout::HEADER as u64 - layout::aux(container.ext as usize) as u64;
    Some(block + layout::aux_at(off as usize) as u64)
}

/// Writes the capability of the pointer `value` into the slot for the word at `at`.
///
/// False if that word has no slot, which is [`address_of`]'s three cases and means the capability
/// was not written down anywhere. A caller that cares has to decide what to do about it, and what
/// the compiler's half will do is not store a pointer it cannot describe into storage it cannot
/// describe. False is not a refusal on its own.
///
/// # Safety
///
/// `container` is the capability of a live instance this runtime's allocator laid out, so that the
/// aux in front of its payload is storage the runtime owns rather than the program's. Two threads
/// storing to the same word at the same time is document 09's problem and not handled here.
pub unsafe fn store(container: Cap, at: u64, value: u64, cap: Cap) -> bool {
    let Some(slot) = address_of(container, at) else {
        return false;
    };
    // SAFETY: the caller says the block is one the allocator laid out, and `address_of` returned
    // an offset inside that block's aux, which is sixteen byte aligned from a granule aligned base
    // and so is aligned for a `Slot`.
    unsafe { (slot as *mut Slot).write(Slot::of(cap, value)) };
    true
}

/// What the slot for the word at `at` says about the pointer `value` that was loaded from it.
///
/// Nothing at all when the word has no slot, which is a different answer from a slot that says
/// [`Read::Nothing`]. The first means the capability was never written down and something else has
/// to supply it, and the second means the word holds an integer.
///
/// # Safety
///
/// As [`store()`].
pub unsafe fn load(container: Cap, at: u64, value: u64) -> Option<Read> {
    let slot = address_of(container, at)?;
    // SAFETY: as `store`, and a slot that was never written reads as zero, which is `Slot::EMPTY`.
    let slot = unsafe { (slot as *const Slot).read() };
    Some(slot.read(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{AUX_PER_WORD, perm};
    use std::vec;
    use std::vec::Vec;

    /// A capability over `[lo, lo + ext)` with something in every field.
    fn cap(lo: u64, ext: u64) -> Cap {
        Cap::new(lo, ext, 7, Meta::new(Class::Allocated, perm::READ, 1))
    }

    #[test]
    fn a_slot_is_the_sixteen_bytes_the_layout_reserves_for_it() {
        // The layout reserves the room and this fills it, and the two would have to change
        // together, since one is what the allocator over allocates and the other is what the
        // generated code writes into that room.
        assert_eq!(size_of::<Slot>(), AUX_PER_WORD);
        assert_eq!(align_of::<Slot>(), 8);
    }

    #[test]
    fn an_untouched_slot_says_the_word_holds_no_pointer() {
        // Zeroed memory has to mean this, because an allocation's aux is not written on the way
        // out and a program that never stores a pointer there must still get a refusal rather
        // than whatever the bits happened to be.
        assert_eq!(Slot::default(), Slot::EMPTY);
        assert!(Slot::EMPTY.is_empty());
        assert_eq!(Slot::EMPTY.read(0x1000), Read::Nothing);
    }

    #[test]
    fn the_bottom_capability_is_stored_as_no_pointer_at_all() {
        // Storing bottom and reading it back has to give bottom, not a capability over nothing at
        // the address that happened to be in the word.
        let slot = Slot::of(Cap::BOTTOM, 0x2000);
        assert_eq!(slot, Slot::EMPTY);
        assert_eq!(slot.read(0x2000), Read::Nothing);
    }

    #[test]
    fn a_capability_comes_back_with_every_field_it_went_in_with() {
        let original = cap(0x4000, 96);
        for addr in [0x4000, 0x4008, 0x4000 + 95, 0x4000 + 96] {
            let Read::Whole(back) = Slot::of(original, addr).read(addr) else {
                panic!("an object of 96 bytes fits in a slot");
            };
            assert_eq!(back.lo, original.lo);
            assert_eq!(back.ext, original.ext);
            assert_eq!(back.ver, original.ver);
            assert_eq!(back.meta.class(), original.meta.class());
            assert_eq!(back.meta.perm(), original.meta.perm());
            assert_eq!(back.meta.state(), original.meta.state());
        }
    }

    #[test]
    fn a_pointer_one_past_the_end_still_names_the_object_it_came_from() {
        // C lets a program compute that address and this design lets it store it, so the
        // displacement has to be allowed to equal the extent rather than being an error.
        let original = cap(0x8000, 32);
        let addr = 0x8000 + 32;
        let Read::Whole(back) = Slot::of(original, addr).read(addr) else {
            panic!("one past the end is inside what a slot can say");
        };
        assert_eq!(back.lo, 0x8000);
        assert!(back.covers(addr, 0));
        assert!(!back.covers(addr, 1));
    }

    #[test]
    fn the_version_keeps_all_sixty_four_of_its_bits() {
        // Halving it to make room for judgement C1's second stamp is the candidate section 9.5
        // turned down for the temporal check's sake, since a thirty two bit version repeats after
        // an hour of a busy allocator and a repeat is a use after free the checker calls live. So
        // the full width is a property worth a test rather than an accident of the packing.
        let original = Cap::new(0x100, 8, u64::MAX, Meta(0));
        let Read::Whole(back) = Slot::of(original, 0x100).read(0x100) else {
            panic!("eight bytes fit");
        };
        assert_eq!(back.ver, u64::MAX);
    }

    #[test]
    fn the_instance_identifier_is_the_one_field_left_in_the_header() {
        // Forty three bits that no check reads, against forty three bits that are the whole of the
        // bounds. A report reads it out of the header, where it also lives.
        let meta = Meta::new(Class::Allocated, perm::READ | perm::WRITE, 12345);
        let original = Cap::new(0x200, 16, 3, meta);
        let Read::Whole(back) = Slot::of(original, 0x200).read(0x200) else {
            panic!("sixteen bytes fit");
        };
        assert_eq!(back.meta.perm(), meta.perm());
        assert_eq!(back.meta.instance(), 0);
        assert_ne!(meta.instance(), 0);
    }

    #[test]
    fn an_object_of_two_megabytes_is_one_byte_too_long_to_say() {
        // The exact threshold, which is what twenty one bits reaches and not what the table of
        // section 5.2.7 rounds it to.
        let fits = cap(0x10000, EXACT);
        assert!(matches!(Slot::of(fits, 0x10000).read(0x10000), Read::Whole(_)));

        let does_not = cap(0x10000, EXACT + 1);
        let Read::Header { ver, meta } = Slot::of(does_not, 0x10000).read(0x10000) else {
            panic!("one byte over the threshold goes to the header");
        };
        assert_eq!(ver, does_not.ver);
        assert_eq!(meta.class(), does_not.meta.class());
    }

    #[test]
    fn a_long_object_keeps_its_version_and_its_meta_in_the_slot() {
        // The recover path is a load for two numbers and not a load for everything. The version
        // is what the temporal check compares, and it would be a poor trade to send the hot check
        // to the header for the sake of the cold one.
        let long = cap(0x100000, 64 << 20);
        let addr = 0x100000 + 4096;
        assert_eq!(
            Slot::of(long, addr).read(addr),
            Read::Header { ver: long.ver, meta: Meta(long.meta.0 & ((1 << META) - 1)) }
        );
    }

    #[test]
    fn an_address_outside_the_capability_never_names_a_different_object() {
        // The derivation check refuses this before anything stores it, so what is being tested is
        // that the encoding degrades to a header lookup rather than to a wrong answer if that
        // check is ever wrong.
        let original = cap(0x4000000, 64);
        for addr in [0x4000000 - 1, 0x4000000 - (4 << 20), 0x4000000 + (4 << 20)] {
            match Slot::of(original, addr).read(addr) {
                Read::Header { ver, .. } => assert_eq!(ver, original.ver),
                Read::Whole(back) => panic!("named {} bytes at {}", back.ext, back.lo),
                Read::Nothing => panic!("the capability was not bottom"),
            }
        }
    }

    #[test]
    fn every_object_a_program_is_likely_to_have_is_said_exactly() {
        // The sweep in `cargo xtask compress` is the general statement and this is the part of it
        // the runtime is allowed to depend on: below the threshold there is no rounding anywhere.
        for ext in [0, 1, 7, 8, 16, 4096, 65536, 1 << 20, EXACT] {
            let original = cap(0x1000000, ext);
            for at in [0, ext / 2, ext] {
                let addr = 0x1000000 + at;
                let Read::Whole(back) = Slot::of(original, addr).read(addr) else {
                    panic!("an object of {ext} bytes fits");
                };
                assert_eq!(back.lo, original.lo);
                assert_eq!(back.ext, ext);
                assert_eq!(back.covers(addr, 0), original.covers(addr, 0));
            }
        }
    }

    #[test]
    fn the_three_fields_do_not_run_into_each_other() {
        // Every bit of the packed word is spoken for, so a displacement at its widest beside an
        // extent at its widest beside every meta bit set has to come back as all three.
        let meta = Meta(u64::MAX);
        let original = Cap::new(0, EXACT, 1, meta);
        let Read::Whole(back) = Slot::of(original, EXACT).read(EXACT) else {
            panic!("the widest pair is the pair that fits");
        };
        assert_eq!(back.lo, 0);
        assert_eq!(back.ext, EXACT);
        assert_eq!(back.meta.0, (1 << META) - 1);
    }

    /// A block laid out the way the allocator lays one out, in storage the test owns.
    ///
    /// Real memory rather than arithmetic about memory, because the point of the functions below
    /// is that they land in the aux and not in the header or the payload, and a test that computes
    /// the same addresses the code computes would not notice if both were wrong.
    struct Block {
        _store: Vec<u64>,
        base: u64,
        cap: Cap,
    }

    fn block(n: usize) -> Block {
        let size = layout::payload(n);
        let mut store = vec![0u64; layout::block(size).div_ceil(WORD)];
        let base = store.as_mut_ptr() as usize;
        let payload = layout::payload_of(base, size);
        let meta = Meta::new(Class::Allocated, perm::READ | perm::WRITE, 3);
        Block {
            _store: store,
            base: base as u64,
            cap: Cap::new(payload as u64, size as u64, 9, meta),
        }
    }

    #[test]
    fn the_slot_for_a_word_is_where_the_layout_puts_it() {
        let b = block(64);
        for word in 0..8 {
            let at = b.cap.lo + word * WORD as u64;
            assert_eq!(
                address_of(b.cap, at),
                Some(b.base + layout::aux_at(word as usize * WORD) as u64)
            );
        }
    }

    #[test]
    fn every_word_of_the_payload_has_its_own_slot_and_they_all_fit_in_the_aux() {
        // The aux is sized from the payload and indexed from the payload, so the last word's slot
        // has to end exactly where the header begins. An off by one here would have `cap_store`
        // write the instance's own bounds over the top of the object's header.
        let b = block(4096);
        let words = b.cap.ext / WORD as u64;
        let first = address_of(b.cap, b.cap.lo).expect("the first word is in the object");
        let last = address_of(b.cap, b.cap.lo + (words - 1) * WORD as u64)
            .expect("the last word is in the object");
        assert_eq!(first, b.base);
        assert_eq!(last - first, (words - 1) * AUX_PER_WORD as u64);
        assert_eq!(last + AUX_PER_WORD as u64, layout::header_of(b.cap.lo as usize) as u64);
    }

    #[test]
    fn a_word_that_is_not_where_a_pointer_goes_has_no_slot() {
        let b = block(64);
        // Not pointer aligned, which a packed structure can ask for and which is not an error.
        assert_eq!(address_of(b.cap, b.cap.lo + 4), None);
        // Past the end, and one past the end, where a whole word does not fit.
        assert_eq!(address_of(b.cap, b.cap.lo + b.cap.ext), None);
        assert_eq!(address_of(b.cap, b.cap.lo + b.cap.ext - 4), None);
        // Below the base, which wraps rather than going negative.
        assert_eq!(address_of(b.cap, b.cap.lo - WORD as u64), None);
    }

    #[test]
    fn storage_the_allocator_did_not_lay_out_has_no_slot_yet() {
        // A local and a global are the two the design owes an aux to and does not have one for.
        // Answering with an address would be reading the eight bytes in front of somebody's stack
        // frame and calling the result a capability.
        for class in [Class::Automatic, Class::Static, Class::Mapped] {
            let cap = Cap::new(0x40000, 64, 5, Meta::new(class, perm::READ, 1));
            assert_eq!(address_of(cap, 0x40000), None);
        }
        assert_eq!(address_of(Cap::BOTTOM, 0), None);
    }

    #[test]
    fn a_pointer_stored_into_a_block_comes_back_out_of_it() {
        let b = block(64);
        let other = block(128);
        let at = b.cap.lo + 16;
        let value = other.cap.lo + 8;
        // SAFETY: the block is this test's own storage, laid out the way the allocator lays one
        // out, and no other thread is touching it.
        assert!(unsafe { store(b.cap, at, value, other.cap) });
        // SAFETY: as above.
        let Some(Read::Whole(back)) = (unsafe { load(b.cap, at, value) }) else {
            panic!("an object of 128 bytes fits in a slot");
        };
        assert_eq!(back.lo, other.cap.lo);
        assert_eq!(back.ext, other.cap.ext);
        assert_eq!(back.ver, other.cap.ver);
    }

    #[test]
    fn a_word_nobody_has_stored_a_pointer_into_says_it_holds_no_pointer() {
        // Fresh storage, which the allocator does not write an aux for, so this is what every word
        // of every new allocation says until something stores a pointer there.
        let b = block(64);
        // SAFETY: as above.
        assert_eq!(unsafe { load(b.cap, b.cap.lo, 0x1234) }, Some(Read::Nothing));
    }

    #[test]
    fn a_word_with_no_slot_is_not_the_same_answer_as_a_slot_that_says_nothing() {
        // The distinction the compiler's half depends on. A pointer loaded out of a local has no
        // slot today, and a caller that read that as `Nothing` would refuse the pointer.
        let b = block(64);
        // SAFETY: as above.
        assert_eq!(unsafe { load(b.cap, b.cap.lo + 4, 0x1234) }, None);
        // SAFETY: as above.
        assert!(!unsafe { store(b.cap, b.cap.lo + 4, 0x1234, b.cap) });
    }

    #[test]
    fn storing_a_capability_leaves_the_object_itself_alone() {
        // The whole point of putting the aux beside the payload rather than in it: the program's
        // bytes are the program's bytes, which is what lets an instrumented structure be passed to
        // a kernel that knows nothing about any of this.
        let b = block(64);
        let payload = b.cap.lo as *mut u64;
        for word in 0..8 {
            let at = b.cap.lo + word * WORD as u64;
            // SAFETY: the block is this test's own storage.
            assert!(unsafe { store(b.cap, at, at, b.cap) });
        }
        for word in 0..8 {
            // SAFETY: the payload is eight words of this test's own storage.
            assert_eq!(unsafe { payload.add(word).read() }, 0);
        }
    }
}
