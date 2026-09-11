//! The init plane: which bytes the monitor has seen written since they last became storage.
//!
//! Design: `spec/safe-memory/09-type-init-and-races.md` sections 9.2 and 9.3, and the layout in
//! `spec/safe-memory/05-representation.md` section 5.2.3.
//!
//! One bit per byte, which is an eighth of the program's memory and 512 bytes of shadow per four
//! kibibyte page. That buys document 03's Y6, reading a value that was never stored, which is the
//! class MSan exists for and the class the kernel infoleak of CWE-200 is an instance of.
//!
//! # The bit means uninitialized, and that is the whole design
//!
//! MSan tracks uninitialized ness and propagates it, so a byte some uninstrumented library wrote is
//! a byte MSan still believes is uninitialized, and the next read of it is a false positive. That is
//! why every library a program links has to be instrumented, libc++ included, and it is why MSan is
//! a fuzzing tool rather than something anybody ships.
//!
//! The failure mode is inverted here. A set bit means the monitor watched these bytes become
//! storage and has not watched anything write them since, so a bit nobody set says nothing is
//! wrong. A fresh reservation reads as zeroes and already says that, an adopted region full of
//! live data says it without anything walking the region, and a gap in instrumentation loses a
//! check rather than inventing a refusal. Section 9.2 makes that trade explicitly: under reporting
//! near uninstrumented code is the only direction available to a tool whose false positive rate is
//! a release blocking property.
//!
//! So the only thing that makes a byte uninitialized is an instance beginning, which is a thing the
//! allocator here does and knows the extent of, and everything else in this file clears bits.
//!
//! # Padding belongs to the compiler and not to this file
//!
//! Section 9.3's rule is that a store which writes an object as a whole initializes the object as a
//! whole, padding included, and that a member by member fill leaves the padding alone. Both of
//! those arrive here as a range, so the rule is entirely a question of which range the compiler
//! names: `sizeof` for a struct assignment, a `memcpy`, a `calloc` or an `= {0}`, and the member's
//! own width for a store through a member. Nothing here knows what a struct is, which is what keeps
//! `-fsafety-init=padding` a decision made once in the compiler rather than a mode this plane has.
//!
//! # What is here so far
//!
//! The arithmetic and the reads and writes, with the shadow handed in, which is the same division
//! [`crate::plane`] explains for the lifetime plane. Where the shadow comes from is
//! [`crate::alloc`], which reserves one for every watched region beside the other two planes, and
//! what makes a byte unwritten is an instance beginning there. `crate::check` has the question a
//! read asks and the two judgements that record what it asks about.
//!
//! What is missing is the caller. No generated code reaches those three yet, because which range a
//! store names is section 9.3's padding rule and that is the compiler's half of the same
//! milestone.

/// How many bytes of program memory one byte of this plane covers.
///
/// Eight, because the plane is a bit per byte and a byte holds eight bits. There is no granule
/// here in the sense the other two planes have one: this is exact, and a byte is the unit C gives
/// an answer about.
pub const SPAN: usize = 8;

/// How many bytes of shadow a run of `len` bytes of program needs.
///
/// Rounded up, because the last few bytes of a run share a shadow byte with whatever follows them
/// and the shadow byte has to be there.
#[must_use]
pub const fn shadow(len: usize) -> usize {
    len.div_ceil(SPAN)
}

/// The direct mapped shadow the plane lives in.
///
/// `origin` is a bias rather than an address and works exactly the way `crate::plane::Lifetime`'s
/// does, for the reason written down there: the shadow for a region high in the address space sits
/// below the bias by more than the bias is, so the value that makes the arithmetic come out right
/// has wrapped.
#[derive(Clone, Copy, Debug)]
pub struct Init {
    /// Where the bit for address zero would be.
    origin: usize,
}

impl Init {
    /// A plane whose bit for address 0 would be the bottom bit at `origin`.
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

    /// Where the bits for the run of eight bytes holding `addr` are kept.
    #[must_use]
    pub const fn slot(&self, addr: usize) -> *mut u8 {
        // Modular, for the reason `new` gives. The offset cannot overflow: it is an eighth of an
        // address.
        self.origin.wrapping_add(addr / SPAN) as *mut u8
    }

    /// Whether the byte at `addr` counts as written.
    ///
    /// # Safety
    ///
    /// `addr` is inside the mapping this plane was built for.
    #[must_use]
    pub unsafe fn read(&self, addr: usize) -> bool {
        // SAFETY: the caller says `addr` is mapped, and the byte for a mapped address is inside the
        // shadow reservation this plane was built over.
        let bits = unsafe { self.slot(addr).read() };
        bits & bit(addr) == 0
    }

    /// Whether every byte of `[lo, lo + len)` has been written since it became storage.
    ///
    /// The refusal this feeds is judgement J1, the same one the type plane's is, because J1's own
    /// wording is about an access the planes did not permit and this is one of the planes.
    ///
    /// A shadow byte at a time, so a run that covers whole eights is one load and one compare per
    /// eight bytes of program, and an access of four or eight bytes is one of each.
    ///
    /// # Safety
    ///
    /// `[lo, lo + len)` is inside the mapping this plane was built for.
    #[must_use]
    pub unsafe fn allows(&self, lo: usize, len: usize) -> bool {
        let mut at = lo;
        while at < lo + len {
            let end = next(at).min(lo + len);
            // SAFETY: `at` is inside the range the caller says is mapped.
            let bits = unsafe { self.slot(at).read() };
            if bits & mask(at, end) != 0 {
                return false;
            }
            at = end;
        }
        true
    }

    /// The judgement a store makes: `[lo, lo + len)` holds what it wrote.
    ///
    /// Which range that is, for a store that writes a whole object, is section 9.3's padding rule
    /// and is the caller's business. See the module comment.
    ///
    /// # Safety
    ///
    /// `[lo, lo + len)` is inside the mapping this plane was built for.
    pub unsafe fn set(&self, lo: usize, len: usize) {
        // SAFETY: the caller's contract, passed straight on.
        unsafe { self.fill(lo, len, false) }
    }

    /// Judgement J4's other half: `[lo, lo + len)` is storage nothing has written yet.
    ///
    /// The only thing in the design that makes a byte uninitialized, and the allocator is the only
    /// caller, because an instance beginning is the only moment anything knows a range has become
    /// storage. It covers the whole block rather than the request, the same way the type plane's
    /// clearing does and for the same reason: the rounding shares bytes with the request, and an
    /// overflow inside a block should be reported as the bounds bug it is.
    ///
    /// # Safety
    ///
    /// As [`Init::set`].
    pub unsafe fn forget(&self, lo: usize, len: usize) {
        // SAFETY: as above.
        unsafe { self.fill(lo, len, true) }
    }

    /// A copy carries the answer for the bytes it read to the bytes it wrote.
    ///
    /// This is what makes the infoleak visible rather than what hides it. A structure filled member
    /// by member and then handed whole to `write` or to `memcpy` carries its padding's answer along
    /// with it, so the bytes that leave the program are still the bytes nothing wrote, and the read
    /// that is refused is the one at the boundary where it matters.
    ///
    /// # Safety
    ///
    /// Both ranges are inside the mapping this plane was built for. They may overlap.
    pub unsafe fn copy(&self, dst: usize, src: usize, len: usize) {
        // Backwards when the destination is above the source, so that an overlapping `memmove`
        // reads a byte's answer before the copy has written over it.
        if dst > src {
            for i in (0..len).rev() {
                // SAFETY: both addresses are inside the ranges the caller says are mapped.
                unsafe { self.one(dst + i, !self.read(src + i)) }
            }
            return;
        }
        for i in 0..len {
            // SAFETY: as above.
            unsafe { self.one(dst + i, !self.read(src + i)) }
        }
    }

    /// Writes one answer over every byte of a range.
    ///
    /// The middle of a long range is a store of a whole shadow byte rather than a read and a write,
    /// which is what makes a fill cost an eighth of what the range does. The two ends are the only
    /// places a byte is shared with somebody the caller did not name.
    unsafe fn fill(&self, lo: usize, len: usize, fresh: bool) {
        let whole = if fresh { u8::MAX } else { 0 };
        let mut at = lo;
        while at < lo + len {
            let end = next(at).min(lo + len);
            let slot = self.slot(at);
            if at % SPAN == 0 && end == next(at) {
                // SAFETY: the whole of this shadow byte is the caller's range, so nothing else's
                // answer is being written over.
                unsafe { slot.write(whole) };
            } else {
                let bits = mask(at, end);
                // SAFETY: as above, for the part of the shadow byte the range covers.
                unsafe {
                    let was = slot.read();
                    slot.write(if fresh { was | bits } else { was & !bits });
                }
            }
            at = end;
        }
    }

    /// Writes one answer over one byte.
    unsafe fn one(&self, addr: usize, fresh: bool) {
        let slot = self.slot(addr);
        // SAFETY: `addr` is mapped, which is this function's own contract passed straight on.
        unsafe {
            let was = slot.read();
            slot.write(if fresh { was | bit(addr) } else { was & !bit(addr) });
        }
    }
}

/// The bit the byte at `addr` gets, which is its position in the run of eight.
const fn bit(addr: usize) -> u8 {
    1 << (addr % SPAN)
}

/// The first address past the run of eight holding `addr`.
const fn next(addr: usize) -> usize {
    (addr / SPAN + 1) * SPAN
}

/// The bits of one shadow byte that `[at, end)` covers, which is a run inside a single eight.
const fn mask(at: usize, end: usize) -> u8 {
    let first = at % SPAN;
    // `end` is the first address past the run, so the run ends on the byte boundary when it lands
    // exactly on the next eight, and a shift of eight is not a shift this width has.
    let past = end - at + first;
    let above = if past >= SPAN { 0 } else { u8::MAX << past };
    let below = (1u8 << first) - 1;
    !(above | below)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand in for the shadow and the program memory it shadows.
    ///
    /// The addresses are invented, because nothing in this file dereferences them: it only divides
    /// them. That is the same arrangement `crate::types`'s tests use and for the same reason.
    struct Fake {
        /// Never read through this name. It is here so the buffer the plane's arithmetic lands in
        /// lives exactly as long as the plane does.
        _shadow: std::vec::Vec<u8>,
        origin: usize,
        base: usize,
    }

    impl Fake {
        /// A plane over `len` bytes of pretend memory, with nothing written yet.
        fn new(len: usize) -> Self {
            let base = 0x1_0000;
            let mut shadow = std::vec![0u8; shadow(len)];
            // Solve origin + base / SPAN = the buffer, which is the same arithmetic the startup
            // code will do once it knows where it put the reservation.
            let origin = (shadow.as_mut_ptr() as usize) - base / SPAN;
            Self { _shadow: shadow, origin, base }
        }

        /// The plane itself, borrowed for the length of one call.
        fn plane(&self) -> Init {
            // SAFETY: the buffer covers the bytes the tests ask about and outlives the borrow.
            unsafe { Init::new(self.origin) }
        }

        fn read(&self, offset: usize) -> bool {
            // SAFETY: within the buffer, by the caller keeping to the bytes it asked for.
            unsafe { self.plane().read(self.base + offset) }
        }

        fn allows(&self, offset: usize, len: usize) -> bool {
            // SAFETY: as above.
            unsafe { self.plane().allows(self.base + offset, len) }
        }

        fn set(&self, offset: usize, len: usize) {
            // SAFETY: as above.
            unsafe { self.plane().set(self.base + offset, len) }
        }

        fn forget(&self, offset: usize, len: usize) {
            // SAFETY: as above.
            unsafe { self.plane().forget(self.base + offset, len) }
        }

        fn copy(&self, dst: usize, src: usize, len: usize) {
            // SAFETY: as above.
            unsafe { self.plane().copy(self.base + dst, self.base + src, len) }
        }
    }

    #[test]
    fn a_byte_nobody_has_said_anything_about_counts_as_written() {
        // The inversion the module comment is about, and the one property that keeps this plane
        // deployable. Untouched shadow reads as zeroes and zeroes are the permissive answer, so
        // memory some other allocator handed out is memory this says nothing about.
        let fake = Fake::new(64);

        assert!(fake.read(0));
        assert!(fake.allows(0, 64));
    }

    #[test]
    fn a_fresh_instance_has_written_none_of_its_bytes() {
        // The other half of judgement J4. Allocated storage holds whatever was there before, so an
        // instance beginning is the moment its bytes stop counting as written.
        let fake = Fake::new(64);

        fake.forget(0, 32);

        assert!(!fake.read(0));
        assert!(!fake.allows(0, 32));
        assert!(fake.allows(32, 32), "the bytes past it were not the instance's");
    }

    #[test]
    fn a_store_is_what_makes_the_bytes_it_wrote_readable() {
        // The judgement a store makes, which is the whole of the maintenance traffic this plane
        // costs.
        let fake = Fake::new(64);

        fake.forget(0, 64);
        fake.set(8, 4);

        assert!(fake.allows(8, 4));
        assert!(!fake.allows(7, 4), "the byte below it was not written");
        assert!(!fake.allows(9, 4), "the byte above it was not written");
    }

    #[test]
    fn a_store_inside_one_run_of_eight_leaves_the_rest_of_the_run_alone() {
        // Where a bitmap goes wrong if the masks are wrong. Four bytes of a struct written and the
        // other four not is the ordinary case rather than a corner: it is what a member by member
        // fill of a structure with padding in it looks like.
        let fake = Fake::new(64);

        fake.forget(0, 64);
        fake.set(2, 3);

        assert!(!fake.read(1));
        assert!(fake.read(2));
        assert!(fake.read(3));
        assert!(fake.read(4));
        assert!(!fake.read(5));
    }

    #[test]
    fn a_range_that_covers_whole_runs_and_parts_of_two_more_is_exact() {
        // The three cases of a fill in one call: a partial byte at the bottom, whole bytes in the
        // middle and a partial byte at the top.
        let fake = Fake::new(128);

        fake.forget(0, 128);
        fake.set(5, 30);

        assert!(!fake.read(4));
        assert!(fake.allows(5, 30));
        assert!(!fake.read(35));
    }

    #[test]
    fn a_read_of_no_bytes_is_permitted() {
        // A width nothing states reads no bytes anybody can name, and the walk has to terminate on
        // it rather than reading the byte it starts at.
        let fake = Fake::new(64);

        fake.forget(0, 64);

        assert!(fake.allows(0, 0));
    }

    #[test]
    fn a_copy_carries_the_padding_a_member_by_member_fill_left_behind() {
        // The infoleak, written out. Two members of a structure are filled, the two bytes of
        // padding between them are not, and the structure is copied whole into a buffer that was
        // fully written. The copy has to make the destination's padding unreadable again, because
        // those are the bytes that would leave the program.
        let fake = Fake::new(128);

        fake.forget(0, 128);
        fake.set(0, 2);
        fake.set(4, 4);
        fake.set(64, 8);
        assert!(fake.allows(64, 8));

        fake.copy(64, 0, 8);

        assert!(fake.allows(64, 2));
        assert!(!fake.allows(66, 2), "the padding came across as padding");
        assert!(fake.allows(68, 4));
    }

    #[test]
    fn a_copy_of_overlapping_ranges_reads_before_it_writes() {
        // What a `memmove` of a buffer over itself does, which is a thing string handling code does
        // on purpose. A forward walk would read a byte's answer after the copy had written over it.
        let fake = Fake::new(64);

        fake.forget(0, 64);
        fake.set(0, 4);

        fake.copy(2, 0, 4);

        assert!(fake.allows(2, 4), "every byte came from a byte that was written");
        assert!(!fake.read(6));
    }

    #[test]
    fn a_run_written_and_then_made_fresh_again_is_unwritten() {
        // A block handed out, written, freed and handed out again, which is the path every
        // allocator busy enough to matter spends its time on.
        let fake = Fake::new(64);

        fake.forget(0, 16);
        fake.set(0, 16);
        assert!(fake.allows(0, 16));

        fake.forget(0, 16);

        assert!(!fake.allows(0, 16));
    }

    #[test]
    fn the_shadow_a_run_needs_rounds_up() {
        // The last few bytes of a run share a shadow byte with whatever follows them, and the
        // shadow byte has to be there or the plane's last write lands outside the reservation.
        assert_eq!(shadow(0), 0);
        assert_eq!(shadow(1), 1);
        assert_eq!(shadow(8), 1);
        assert_eq!(shadow(9), 2);
        assert_eq!(shadow(4096), 512);
    }

    #[test]
    fn the_mask_for_a_run_is_the_bits_that_run_covers() {
        // The arithmetic every other test here rests on, checked on its own so that a failure says
        // which of the two is wrong.
        assert_eq!(mask(0, 8), 0b1111_1111);
        assert_eq!(mask(0, 1), 0b0000_0001);
        assert_eq!(mask(3, 8), 0b1111_1000);
        assert_eq!(mask(2, 5), 0b0001_1100);
        assert_eq!(mask(7, 8), 0b1000_0000);
    }
}
