//! The aux slot: the sixteen bytes that sit beside a pointer in memory.
//!
//! Design: `spec/safe-memory/05-representation.md` sections 5.2.2 and 5.2.7, and
//! `spec/safe-memory/17-open-questions.md` question 5, which is the decision about what goes in
//! here and was answered in tamnd/rucc#1068.
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
//! # What is not here
//!
//! `instance_id` is left in the header. Section 5.2.1 already calls it a debugging aid rather than
//! a safety critical field, a report is read out of the header anyway, and it is forty three bits
//! that a check never looks at, so an aux slot that carries it would be paying most of its budget
//! for something no judgement asks about.
//!
//! The second epoch stamp is not here either, which is judgement C1's problem rather than a
//! decision this module gets to make. The slot is full, so where that stamp lives is tamnd/rucc#1069.
//!
//! Nothing writes one of these yet. What is missing is the compiler's half, which is a `cap_store`
//! beside every store of a pointer, and that is milestone S5 work on tamnd/rucc#856.

use crate::layout::{Cap, Meta};
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
const HEADER: u64 = 1 << (META + 2 * BOUND);

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
            return Self { ver: cap.ver, packed: meta | HEADER };
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
        if self.packed & HEADER != 0 {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{AUX_PER_WORD, Class, perm};

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
        // Halving it is the candidate #1069 rejects for the temporal check's sake, so the full
        // width is a property worth a test rather than an accident of the packing.
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
}
