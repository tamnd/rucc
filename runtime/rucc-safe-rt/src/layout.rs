//! How an allocated storage instance is laid out, and what its header says about it.
//!
//! Design: `spec/safe-memory/05-representation.md` section 5.2.2.
//!
//! An allocation is one block with three parts, and the pointer the program gets back points at
//! the third of them:
//!
//! ```text
//! [ aux : ceil(n / 8) * 16 bytes ][ header : 32 bytes ][ payload : n bytes ]
//! ^ the block                                            ^ what malloc returns
//! ```
//!
//! The header is the instance's own capability, so `cap_of` on a pointer into the payload is a
//! subtract by a constant and a load rather than a lookup in anything. It sits directly behind
//! the payload rather than at the front of the block for exactly that reason: the aux is as long
//! as the payload is, so a header at the front would be a different distance away for every size,
//! and finding it from the pointer the program holds would mean already knowing the size.
//!
//! The aux is where the capabilities of pointers stored *in* the payload go, sixteen bytes for
//! every eight payload bytes, since eight payload bytes is one pointer sized word and a pointer's
//! capability does not fit in one.
//!
//! That last sentence is the honest headline of the whole design, so it is written here where
//! somebody reading the arithmetic will meet it: a structure full of pointers takes about three
//! times the memory it used to, and the second and third parts are on lines the program was not
//! otherwise touching. Reasoning about this design in instruction counts will mislead. Document
//! 13 measures cache misses and memory traffic instead.
//!
//! The aux goes beside the object rather than into a global shadow because it is then in the same
//! physical page as the data and gets prefetched with it. The planes cannot do that, because the
//! lifetime plane has to be readable after the object is gone, which is why they are a shadow and
//! this is not.

use crate::plane::{GRANULE, Version};

/// What class of storage an instance is, from document 04 section 4.2.
///
/// The discriminants are ABI, in the same way the judgement numbers are: they are compiled into
/// the header of every instance and read by a runtime that may be a different build.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Class {
    /// A file scope or static local object, whose lifetime is the program's.
    Static = 0,
    /// A local, whose lifetime is its frame's.
    Automatic = 1,
    /// Something an allocator handed out, which is the only class this module lays out.
    Allocated = 2,
    /// A mapping, from `mmap` or the target's equivalent.
    Mapped = 3,
    /// Device registers, where a read is not a read.
    Mmio = 4,
    /// Storage a device owns for now, per judgement J7.
    Device = 5,
    /// A function, which is a place a pointer can name and nothing can load from.
    Function = 6,
    /// A string or compound literal, which is static storage the program may not write.
    Literal = 7,
}

/// What may be done through a capability, as a bitset.
pub mod perm {
    /// Loads are permitted.
    pub const READ: u8 = 1;
    /// Stores are permitted.
    pub const WRITE: u8 = 2;
    /// The bytes may be jumped to.
    pub const EXEC: u8 = 4;
}

/// Where an instance is in its life, from document 04 section 4.2.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum State {
    /// Accesses are permitted, subject to everything else.
    Live = 0,
    /// The instance is over. Judgement J5 has run and every capability for it fails.
    Ended = 1,
    /// Over, and the address is being held back from reuse.
    Quarantined = 2,
    /// Handed to a device. Judgement J7.
    DeviceOwned = 3,
}

/// The packed word of section 5.2.1: `class:4, perm:3, state:2, tag_bits:4, flags:8, instance_id:43`.
///
/// One word rather than five fields because it travels with the capability through registers and
/// spill slots, and four registers per live pointer is already as much as x86-64 will bear.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct Meta(pub u64);

impl Meta {
    const CLASS: u32 = 0;
    const PERM: u32 = 4;
    const STATE: u32 = 7;
    const TAG_BITS: u32 = 9;
    const FLAGS: u32 = 13;
    const INSTANCE: u32 = 21;

    /// How many instances can be told apart before the identifier repeats.
    ///
    /// Forty three bits is 8.8 x 10^12, which at a million allocations a second is a hundred
    /// days. It repeating is not a safety problem: [`Version`] is the field that must not repeat
    /// and it has all sixty four of its bits. This one is for reports to quote.
    pub const INSTANCES: u64 = 1 << 43;

    /// Flag: this capability was recovered at a boundary rather than handed over in a call frame.
    ///
    /// Every one of these is a weakening and document 10 section 10.2 counts them, so the bit has
    /// to travel with the capability rather than live in a table beside it. A capability copied
    /// into a structure and loaded back weeks of program time later is still a recovered one.
    pub const RECOVERED: u8 = 1;

    /// Flag: the bounds are wider than the object's, because the object's could not be found.
    ///
    /// Always set with [`Meta::RECOVERED`], never on its own. This is the difference between a
    /// capability recovered from the planes, whose bounds are the instance's, and one recovered
    /// from the containing mapping, whose bounds are the mapping's and which therefore permits
    /// running from one object in that mapping into the next.
    pub const WIDE: u8 = 2;

    /// A live instance of `class` with `perm`, whose identifier is `instance`.
    #[must_use]
    pub const fn new(class: Class, perm: u8, instance: u64) -> Self {
        Self(
            (class as u64) << Self::CLASS
                | ((perm & 0b111) as u64) << Self::PERM
                | (State::Live as u64) << Self::STATE
                | (instance % Self::INSTANCES) << Self::INSTANCE,
        )
    }

    /// The class field.
    #[must_use]
    pub const fn class(self) -> u8 {
        (self.0 >> Self::CLASS) as u8 & 0b1111
    }

    /// The permission bitset.
    #[must_use]
    pub const fn perm(self) -> u8 {
        (self.0 >> Self::PERM) as u8 & 0b111
    }

    /// The state field.
    #[must_use]
    pub const fn state(self) -> u8 {
        (self.0 >> Self::STATE) as u8 & 0b11
    }

    /// The tag bits, which are where an accelerator such as MTE puts its tag.
    #[must_use]
    pub const fn tag_bits(self) -> u8 {
        (self.0 >> Self::TAG_BITS) as u8 & 0b1111
    }

    /// The eight spare bits, unassigned so far.
    #[must_use]
    pub const fn flags(self) -> u8 {
        (self.0 >> Self::FLAGS) as u8
    }

    /// The instance identifier.
    #[must_use]
    pub const fn instance(self) -> u64 {
        self.0 >> Self::INSTANCE
    }

    /// The same word with a different state, which is what J5 and J7 do.
    #[must_use]
    pub const fn with_state(self, state: State) -> Self {
        Self((self.0 & !(0b11 << Self::STATE)) | (state as u64) << Self::STATE)
    }

    /// The same word with the flags byte replaced.
    ///
    /// Replaced rather than merged, so that a caller building a word says all eight bits at once
    /// and there is no way to end up with a flag nobody meant to set.
    #[must_use]
    pub const fn with_flags(self, flags: u8) -> Self {
        Self((self.0 & !(0xff << Self::FLAGS)) | (flags as u64) << Self::FLAGS)
    }
}

/// What sits in front of an allocated instance.
///
/// `lo` is not in here, because it is this header's own address plus [`HEADER`], and a field that
/// can be computed is a field that can disagree.
///
/// `#[repr(C)]` and a fixed size because the backend reads it inline: `cap_of` on a pointer whose
/// instance the compiler knows is allocated is a subtract and three loads off the payload, with
/// the offsets baked in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C, align(16))]
pub struct Header {
    /// `hi - lo`, so that the bounds check is one subtract and one unsigned compare rather than
    /// two compares. This is why the capability carries an extent and not an upper bound.
    pub ext: u64,
    /// The lifetime version, the same one this instance's granules hold in the plane.
    pub ver: Version,
    /// The packed word of [`Meta`].
    pub meta: Meta,
    /// Which allocator handed this out, so that judgement J6 can refuse a free by the wrong one.
    pub allocator: u64,
}

/// How many bytes the header takes, and therefore where the aux starts.
pub const HEADER: usize = size_of::<Header>();

/// How many bytes of aux one pointer sized word of payload needs.
///
/// A capability is four words in flight and is squeezed into two here, with `lo` and `ext`
/// compressed the way CHERI compresses them. The compression scheme is question 5 of document 17
/// and is not decided, so nothing here writes an aux entry yet. What is decided is how much room
/// it gets, because that is what fixes the memory overhead and the memory overhead is the number
/// this design has to defend.
pub const AUX_PER_WORD: usize = 16;

/// How many payload bytes one aux entry covers.
pub const WORD: usize = 8;

/// The size a payload of `n` bytes actually occupies.
///
/// Rounded up to a granule, because the lifetime plane is per granule and an instance that shared
/// a granule with its neighbour would share the neighbour's version, which is to say it would
/// stop being checkable the moment the neighbour was freed.
#[must_use]
pub const fn payload(n: usize) -> usize {
    n.div_ceil(GRANULE) * GRANULE
}

/// How many bytes of aux a payload of `n` bytes needs.
#[must_use]
pub const fn aux(n: usize) -> usize {
    payload(n).div_ceil(WORD) * AUX_PER_WORD
}

/// How many bytes an allocator has to obtain to hand back `n`.
///
/// This is the memory overhead of the design, in one function. For a payload of sixteen bytes it
/// is eighty, which sounds ruinous and is the small allocation case; for four kilobytes it is a
/// little over three times, which is the case that matters and is still the honest number.
#[must_use]
pub const fn block(n: usize) -> usize {
    HEADER + aux(n) + payload(n)
}

/// Where the payload is, given the block.
#[must_use]
pub const fn payload_of(block: usize, n: usize) -> usize {
    block + aux(n) + HEADER
}

/// Where the header is, given a pointer to the start of the payload.
///
/// A constant, which is the whole reason the header is behind the payload rather than in front of
/// the aux. Everything on the hot path starts here.
#[must_use]
pub const fn header_of(payload: usize) -> usize {
    payload - HEADER
}

/// Where the block is, given a pointer to the start of the payload.
#[must_use]
pub const fn block_of(payload: usize, n: usize) -> usize {
    payload - HEADER - aux(n)
}

/// Where the aux entry for the word at `offset` bytes into the payload lives, relative to the
/// start of the block.
///
/// The aux runs in payload order and starts the block, so this is a shift and a scale, and the
/// entry for a pointer at a known offset in a known structure is a constant the backend folds
/// away.
#[must_use]
pub const fn aux_at(offset: usize) -> usize {
    (offset / WORD) * AUX_PER_WORD
}

/// A capability in flight, which is section 5.2.1's four words.
///
/// What a pointer means, kept beside the pointer rather than inside it. The program's pointer is
/// still one word and still points where it always did, which is the whole design: an instrumented
/// `struct stat` is the `struct stat` the kernel writes.
///
/// `ext` rather than an upper bound because the hot check is `(addr - lo) <u ext - n`, one
/// unsigned compare that catches running off either end, which is why every implementation of this
/// idea from SoftBound onwards stores a base and a length.
///
/// `#[repr(C)]` because it is a field of the call frame of section 5.3, and that frame is written
/// by one function and read by another, possibly compiled a week apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct Cap {
    /// The lowest address the capability permits.
    pub lo: u64,
    /// How many bytes from `lo` it covers, which is `hi - lo`.
    pub ext: u64,
    /// The lifetime version, compared against the plane to catch a pointer to storage that has
    /// been given back.
    pub ver: Version,
    /// The packed word of [`Meta`].
    pub meta: Meta,
}

impl Cap {
    /// The bottom capability, which permits nothing.
    ///
    /// A version of [`crate::plane::DEAD`] is the encoding, which is the same thing an untouched
    /// shadow slot and an aux slot holding a non pointer both say. That is section 5.2.2's point
    /// about document 03's Y1 coming for free: an integer read as a pointer arrives as this, and
    /// the first access through it fails.
    pub const BOTTOM: Self = Self { lo: 0, ext: 0, ver: crate::plane::DEAD, meta: Meta(0) };

    /// A capability over `[lo, lo + ext)` of the instance `ver` names.
    #[must_use]
    pub const fn new(lo: u64, ext: u64, ver: Version, meta: Meta) -> Self {
        Self { lo, ext, ver, meta }
    }

    /// Whether this permits nothing at all.
    #[must_use]
    pub const fn is_bottom(self) -> bool {
        self.ver == crate::plane::DEAD
    }

    /// Whether an access of `len` bytes at `addr` is inside what this permits.
    ///
    /// The one address arithmetic fact this design leans on: subtracting the base and comparing
    /// unsigned catches an address below the object as well as one above it, because below wraps
    /// to enormous. One past the end is permitted, since C permits computing it, and reading
    /// through it is refused by asking for a byte rather than none.
    #[must_use]
    pub const fn covers(self, addr: u64, len: u64) -> bool {
        if self.is_bottom() {
            return false;
        }
        match self.ext.checked_sub(len) {
            Some(room) => addr.wrapping_sub(self.lo) <= room,
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_capability_covers_its_own_object_and_nothing_either_side() {
        let cap = Cap::new(1024, 64, 2, Meta::new(Class::Allocated, perm::READ, 1));
        assert!(cap.covers(1024, 64));
        assert!(cap.covers(1024 + 63, 1));
        // One past the end, which C lets a program compute and not read through.
        assert!(cap.covers(1024 + 64, 0));
        assert!(!cap.covers(1024 + 64, 1));
        // Below the base, which is the case the unsigned compare is chosen for: the subtraction
        // wraps to something enormous rather than to something negative.
        assert!(!cap.covers(1023, 1));
        assert!(!cap.covers(1024, 65));
        // A length no object could hold, which is the overflow a signed compare would let past.
        assert!(!cap.covers(1024, u64::MAX));
    }

    #[test]
    fn the_bottom_capability_permits_nothing() {
        // An integer read as a pointer arrives as this, and so does a pointer whose storage is
        // gone, so it has to refuse even an access of no bytes at its own base.
        assert!(Cap::BOTTOM.is_bottom());
        assert!(!Cap::BOTTOM.covers(0, 0));
        assert!(!Cap::BOTTOM.covers(0, 1));
    }

    #[test]
    fn the_header_is_the_size_the_backend_will_reach_past() {
        // cap_of on an allocated instance is a subtract by a constant the compiler bakes in, so
        // this size is generated code rather than a detail. Changing it is a change to what every
        // object built with -fsafety contains, and the two halves would have to change together.
        assert_eq!(HEADER, 32);
        assert_eq!(align_of::<Header>(), 16);
    }

    #[test]
    fn a_payload_never_shares_a_granule_with_the_allocation_beside_it() {
        // Sharing one would mean sharing a version, and sharing a version means a pointer to one
        // of them keeps working after the other is freed. This is the reason allocations round.
        for n in 1..200 {
            assert_eq!(payload(n) % GRANULE, 0);
            assert!(payload(n) >= n);
            assert!(payload(n) - n < GRANULE);
        }
    }

    #[test]
    fn every_pointer_sized_word_of_a_payload_has_an_aux_entry_of_its_own() {
        // If two words shared an entry, storing a pointer would overwrite the capability of the
        // pointer beside it, and the second one would come back as something it never was. That
        // is worse than having no aux at all.
        for n in [1, 8, 9, 16, 100, 4096] {
            let entries = aux(n) / AUX_PER_WORD;
            assert_eq!(entries * WORD, payload(n));
            let mut seen = std::collections::HashSet::new();
            for offset in (0..payload(n)).step_by(WORD) {
                assert!(seen.insert(aux_at(offset)));
                assert!(aux_at(offset) + AUX_PER_WORD <= aux(n));
            }
        }
    }

    #[test]
    fn the_payload_and_the_block_find_each_other() {
        // The program holds the payload address and the runtime needs the block, on every free
        // and on every cap_of. Getting this pair wrong is a header read off the end of the
        // allocation before it, which would be a monitor with a memory bug.
        for n in [1, 16, 17, 4096] {
            let start = 0x2_0000;
            assert_eq!(block_of(payload_of(start, n), n), start);
            assert_eq!(header_of(payload_of(start, n)), start + aux(n));
            assert_eq!(payload_of(start, n) % GRANULE, 0);
        }
    }

    #[test]
    fn what_an_allocation_actually_costs_is_stated_rather_than_discovered() {
        // The number this design has to defend, pinned so that a change to the layout is a change
        // somebody has to argue for rather than one that shows up in a benchmark six months on.
        assert_eq!(block(16), 32 + 32 + 16);
        assert_eq!(block(4096), 32 + 8192 + 4096);
    }

    #[test]
    fn the_packed_word_gives_back_every_field_that_was_put_in_it() {
        // Six fields in one word, and a shift that is off by one silently turns a read only
        // literal into a writable one, or an ended instance into a live one.
        let meta = Meta::new(Class::Allocated, perm::READ | perm::WRITE, 12345);

        assert_eq!(meta.class(), Class::Allocated as u8);
        assert_eq!(meta.perm(), perm::READ | perm::WRITE);
        assert_eq!(meta.state(), State::Live as u8);
        assert_eq!(meta.tag_bits(), 0);
        assert_eq!(meta.flags(), 0);
        assert_eq!(meta.instance(), 12345);
    }

    #[test]
    fn ending_an_instance_changes_its_state_and_nothing_else() {
        // J5 writes this field and only this field. An end that quietly dropped the class would
        // turn a later double free into a report about the wrong thing.
        let live = Meta::new(Class::Allocated, perm::READ | perm::WRITE, 999);
        let ended = live.with_state(State::Ended);

        assert_eq!(ended.state(), State::Ended as u8);
        assert_eq!(ended.class(), live.class());
        assert_eq!(ended.perm(), live.perm());
        assert_eq!(ended.instance(), live.instance());
    }

    #[test]
    fn the_last_instance_identifier_before_it_repeats_still_fits() {
        // Forty three bits, and the field above it is the top of the word, so an identifier that
        // overflowed would land nowhere rather than in a neighbour. Worth checking that it does
        // not, because the identifier is what a report quotes.
        let last = Meta::INSTANCES - 1;
        let meta = Meta::new(Class::Static, perm::READ, last);

        assert_eq!(meta.instance(), last);
        assert_eq!(meta.class(), Class::Static as u8);
        assert_eq!(meta.perm(), perm::READ);
    }
}
