//! What a compressed capability in the aux plane can say, and what it has to round off.
//!
//! Design: `spec/safe-memory/05-representation.md` section 5.2.2, which stores a compressed
//! `(lo, ext, meta)` in every aux slot and marks the compression scheme as a decision not yet
//! made, and `spec/safe-memory/17-open-questions.md` question 5, which is that decision.
//!
//! # The question
//!
//! An aux slot is sixteen bytes for every eight bytes of payload. Eight of them are the lifetime
//! version, which is compared against the plane and is the whole of the temporal check, and the
//! other eight have to hold the bounds and the permissions of the capability for the pointer
//! stored in that word. Bounds are two 64-bit numbers and there are 64 bits, so something is
//! compressed. The straw man is CHERI Concentrate, an exponent and two mantissas, which is well
//! studied and whose error bounds are known. The alternative question 5 names is a 32-byte slot
//! with nothing compressed, which is four bytes of aux for every byte of pointer-dense structure
//! instead of two.
//!
//! What is at stake is the representable-region error. A compressed bound is never narrower than
//! the true bound, because a narrower one would refuse a correct program, so it is wider, and the
//! bytes between the true end and the rounded end are bytes an overflow can reach without the
//! bounds check seeing it. That is a real hole and it is worth a number rather than a shrug.
//!
//! # What this measures
//!
//! The geometry, exhaustively, which is the whole of what representability is. A range is exactly
//! representable at a mantissa of `m` bits when there is an exponent `e` such that both ends land
//! on a multiple of `2^e` and the length fits in `m` bits at that scale. That condition depends on
//! nothing but the two numbers, so sweeping the ranges that can actually arise answers it outright
//! rather than estimating it.
//!
//! Three populations, because the ranges that arise are not one kind of thing. Whole heap objects
//! are what the allocator hands out, and they are swept twice, once as the allocator places them
//! today and once from an allocator that puts each block on whatever alignment its own size needs.
//! Narrowed ranges are what `-fsafety-subobject` produces, a member inside an object, where the
//! base alignment is the structure's rather than anybody's choice. Mappings are what the boundary
//! code recovers for memory no instrumented allocator owns.
//!
//! # What this does not measure
//!
//! Nothing here is a running program. Whether the slack a scheme leaves is ever reached by a real
//! overflow is a question about programs and this is a question about numbers. The number here is
//! an upper bound on the hole: bytes that no bounds check can refuse. It is also stated per range
//! rather than weighted by how often a range of that shape occurs, since weighting it needs an
//! allocation profile from a running monitor and the point of answering this now is to decide the
//! slot format before the monitor is written against it.
//!
//! CHERI's representable region, which is the range a pointer may roam over before its bounds stop
//! decoding, is not measured either. It constrains pointer arithmetic rather than the bounds, and
//! in this design the aux slot describes a pointer that has been stored, which is in bounds or one
//! past the end. A pointer that has roamed further than that is refused at the derivation check
//! before anything stores it.

use std::fmt::Write as _;

use crate::Result;

/// Bits in an aux slot, which is sixteen bytes beside every pointer-sized word of payload.
const SLOT: u32 = 128;

/// Bits the lifetime version wants, per section 5.2.1, where it is the value that must not repeat.
const VER: u32 = 64;

/// Bits of `meta` a check reads: class, permissions, state, tag bits and flags.
///
/// Section 5.2.1's `meta` is 64 bits and 43 of them are `instance_id`, which that section already
/// calls a debugging aid rather than a safety-critical field. It is in the object's header, which
/// is where a report reads it from, so an aux slot that leaves it out loses nothing a check wanted.
const META: u32 = 4 + 3 + 2 + 4 + 8;

/// Bits of exponent in a CHERI Concentrate style encoding, enough to name every scale up to the
/// whole address space.
const EXPONENT: u32 = 6;

/// The mantissa widths worth putting in the table.
///
/// CHERI-128 uses 14 and it is in the middle of this on purpose, so the row above and the row
/// below say what a bit either way buys.
const WIDTHS: &[u32] = &[8, 10, 12, 14, 16, 18, 20];

/// The alignment `rucc-safe-rt`'s allocator gives every block, per section 5.2.3, which rounds to
/// sixteen because the lifetime plane's granule is sixteen.
const ROUND: u64 = 16;

/// Where the sweeps put the heap. Far from zero and aligned to more than any exponent under test,
/// so that a base's low bits are the sweep's own doing rather than the constant's.
const BASE: u64 = 1 << 44;

/// The bit splits worth putting in the exact-or-recover table, as bits for the offset and bits for
/// the extent, which are the same width because the offset of a pointer into its own object is at
/// most the extent.
const SPLITS: &[u32] = &[16, 18, 20, 21, 22];

/// A range of bytes a capability names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Bounds {
    /// The first byte.
    lo: u64,
    /// How many bytes it runs for.
    ext: u64,
}

impl Bounds {
    /// One past the last byte.
    fn top(self) -> u64 {
        self.lo + self.ext
    }
}

/// The smallest range an exponent-and-mantissa encoding can name that contains this one.
///
/// Outward and never inward, which is the direction the whole question is about: a bound rounded
/// in refuses an access the program is entitled to make, and a bound rounded out permits one it is
/// not. The first is a broken compiler and the second is a hole with a size, so every encoding
/// that cannot say a range exactly says a wider one and this works out how much wider.
fn round(bounds: Bounds, mantissa: u32) -> Bounds {
    for e in 0..64 {
        let step = 1u64 << e;
        let lo = bounds.lo & !(step - 1);
        let Some(raised) = bounds.top().checked_add(step - 1) else { continue };
        let top = raised & !(step - 1);
        // A mantissa of `m` bits names a length of at most `(2^m - 1)` granules of `2^e` bytes.
        let room = u128::from((1u64 << mantissa) - 1) << e;
        if u128::from(top - lo) <= room {
            return Bounds { lo, ext: top - lo };
        }
    }
    Bounds { lo: 0, ext: u64::MAX }
}

/// How many bytes wider than the truth the encoding had to be.
///
/// Both ends, because an overflow off the front is as much a miss as one off the end, and section
/// 9.4's intra-object rows are mostly the front: a member narrowed to its own bounds is preceded
/// by the rest of the structure and a rounded-down base walks straight into it.
fn slack(bounds: Bounds, mantissa: u32) -> u64 {
    let held = round(bounds, mantissa);
    (bounds.lo - held.lo) + (held.top() - bounds.top())
}

/// What a sweep over one population found.
#[derive(Clone, Copy, Default)]
struct Found {
    /// How many ranges were tried.
    tried: u64,
    /// How many of them the encoding said exactly.
    exact: u64,
    /// The most bytes it was ever wider by.
    worst: u64,
    /// The most it was ever wider by as a share of the range itself, which is the figure that does
    /// not grow just because the ranges tried were bigger.
    worst_share: f64,
}

impl Found {
    /// Adds one range to the tally.
    fn see(&mut self, bounds: Bounds, mantissa: u32) {
        self.tried += 1;
        let over = slack(bounds, mantissa);
        if over == 0 {
            self.exact += 1;
            return;
        }
        self.worst = self.worst.max(over);
        self.worst_share = self.worst_share.max(over as f64 / bounds.ext as f64);
    }

    /// The share it said exactly, as a percentage.
    fn share(self) -> f64 {
        100.0 * self.exact as f64 / self.tried.max(1) as f64
    }
}

/// The sizes a heap object can be, at the allocator's own rounding and thinned above the sizes
/// where every step is worth taking.
fn sizes() -> Vec<u64> {
    let mut out = Vec::new();
    let mut n = ROUND;
    while n <= 64 * 1024 {
        out.push(n);
        n += ROUND;
    }
    while n <= 4 * 1024 * 1024 {
        out.push(n);
        n += 4096;
    }
    while n <= 256 * 1024 * 1024 {
        out.push(n);
        n += 1024 * 1024;
    }
    out
}

/// Whole heap objects, as the allocator hands them out today.
///
/// The base is a multiple of sixteen and nothing more, which is what section 5.2.3 says the
/// allocator rounds to. Every residue of sixteen up to a cache line is tried rather than one,
/// because whether a base is exactly representable at an exponent is a fact about its low bits.
fn heap(mantissa: u32) -> Found {
    let mut found = Found::default();
    for size in sizes() {
        for step in 0..4 {
            found.see(Bounds { lo: BASE + step * ROUND, ext: size }, mantissa);
        }
    }
    found
}

/// The same objects from an allocator that puts each block on the alignment its own size needs.
///
/// Which is what CHERI's allocators do and what ours could do, since the base is the allocator's
/// to choose. It is the best a compressed bound can be made to do, and the point of the row is
/// that the best is not exact: the length is the program's rather than the allocator's, so the
/// top still has to be rounded up whenever the size is not a multiple of the granule. What is
/// left over after aligning the base is therefore the floor on the error, not a transitional
/// number that more care would remove.
fn aligned(mantissa: u32) -> Found {
    let mut found = Found::default();
    for size in sizes() {
        let e = exponent(size, mantissa);
        found.see(Bounds { lo: BASE.next_multiple_of(1 << e), ext: size }, mantissa);
    }
    found
}

/// The exponent a length of this size needs, which is the alignment its ends must have.
fn exponent(size: u64, mantissa: u32) -> u32 {
    let room = (1u64 << mantissa) - 1;
    let mut e = 0;
    while e < 63 && (size >> e) > room {
        e += 1;
    }
    e
}

/// Narrowed ranges that are a scalar or a small member inside an object.
///
/// The object is sixteen-aligned and the member sits at an offset the structure decided, so
/// neither end is anybody's to align. Offsets go up in eights because a narrowed range holding a
/// pointer is pointer-aligned, and every length up to four kilobytes is tried, which covers what a
/// member of a structure is.
fn narrowed(mantissa: u32) -> Found {
    let mut found = Found::default();
    for offset in (0..4096).step_by(8) {
        for len in 1..=4096u64 {
            found.see(Bounds { lo: BASE + offset as u64, ext: len }, mantissa);
        }
    }
    found
}

/// Narrowed ranges that are a large array member inside an object.
///
/// The other half of what `-fsafety-subobject` narrows to, kept apart from the small members
/// because the two answer differently and averaging them would hide which. The stride is
/// deliberately not a power of two, so that a large member is not handed an alignment it would
/// not have in a real declaration.
fn array(mantissa: u32) -> Found {
    let mut found = Found::default();
    for offset in (0..4096).step_by(8) {
        for len in (4096..=1 << 22).step_by(9973) {
            found.see(Bounds { lo: BASE + offset as u64, ext: len }, mantissa);
        }
    }
    found
}

/// Mappings, which is what the boundary code recovers for memory no instrumented allocator owns.
///
/// Page aligned at the base by construction, so the only question is whether the length fits the
/// mantissa at the exponent that alignment allows.
fn mapping(mantissa: u32) -> Found {
    let mut found = Found::default();
    let mut pages = 1u64;
    while pages <= 1 << 22 {
        found.see(Bounds { lo: 0x7000_0000_0000, ext: pages * 4096 }, mantissa);
        pages += 1.max(pages / 16);
    }
    found
}

/// The largest object an exact offset and extent of this many bits each covers with no error.
///
/// The other half of the answer. A slot that does not compress at all says a range exactly or
/// says nothing, and saying nothing means reading the extent out of the object's header, which is
/// a load from a line `cap_of` is already on. So the number to report is not an error, there is
/// none, it is the size above which the load happens.
fn threshold(bits: u32) -> u64 {
    (1u64 << bits) - 1
}

/// A byte count written the way a person says it.
fn size(bytes: u64) -> String {
    for (unit, name) in [(1u64 << 30, "GiB"), (1 << 20, "MiB"), (1 << 10, "KiB")] {
        if bytes >= unit {
            return format!("{} {name}", bytes / unit);
        }
    }
    format!("{bytes} B")
}

/// One way of dividing an aux slot up.
struct Layout {
    /// What to call it.
    name: &'static str,
    /// Bytes in the slot.
    bytes: u32,
    /// Bits of lifetime version.
    ver: u32,
    /// Bits of bounds.
    bounds: u32,
    /// Bits left over, which is what judgement C1's second stamp would have to come out of.
    spare: i64,
    /// What it gives up.
    about: &'static str,
}

/// The layouts worth putting side by side.
fn layouts() -> Vec<Layout> {
    let cheri = 2 * 14 + EXPONENT;
    let exact = 1 + 21 + 21;
    let mut out = vec![
        Layout {
            name: "wide",
            bytes: 32,
            ver: VER,
            bounds: 128,
            spare: i64::from(2 * SLOT) - i64::from(VER + 128 + 64),
            about: "nothing compressed, and twice the aux plane",
        },
        Layout {
            name: "cheri-128",
            bytes: 16,
            ver: VER,
            bounds: cheri,
            spare: i64::from(SLOT) - i64::from(VER + cheri + META),
            about: "the straw man, with instance_id left in the header",
        },
        Layout {
            name: "cheri-128, short ver",
            bytes: 16,
            ver: 32,
            bounds: cheri,
            spare: i64::from(SLOT) - i64::from(32 + cheri + META),
            about: "the same with the version halved, which is what C1 would need",
        },
        Layout {
            name: "exact or recover",
            bytes: 16,
            ver: VER,
            bounds: exact,
            spare: i64::from(SLOT) - i64::from(VER + exact + META),
            about: "an exact offset and extent up to two megabytes, header lookup past that",
        },
    ];
    out.sort_by_key(|l| l.bytes);
    out
}

/// Runs the sweep and prints what it found.
pub(crate) fn compress() -> Result<()> {
    let mut out = String::new();
    out.push_str("compress: what an aux slot can hold, and what a compressed bound rounds off\n");

    out.push_str("\nthe budget, in bits of one aux slot\n\n");
    let _ = writeln!(
        out,
        "{:<22} {:>6} {:>5} {:>7} {:>5} {:>6}  gives up",
        "layout", "bytes", "ver", "bounds", "meta", "spare"
    );
    for l in layouts() {
        let meta = if l.bytes == 32 { 64 } else { META };
        let _ = writeln!(
            out,
            "{:<22} {:>6} {:>5} {:>7} {:>5} {:>6}  {}",
            l.name, l.bytes, l.ver, l.bounds, meta, l.spare, l.about
        );
    }

    out.push_str(
        "\nwhat an exponent and two mantissas of m bits say exactly, and what it rounds\n\n",
    );
    let _ = writeln!(
        out,
        "{:>3} {:>8} {:>8} {:>11} {:>10} {:>9} {:>9} {:>10} {:>8}",
        "m",
        "heap",
        "aligned",
        "algn worst",
        "algn over",
        "member",
        "array",
        "arr worst",
        "mapping"
    );
    for &m in WIDTHS {
        let h = heap(m);
        let a = aligned(m);
        let n = narrowed(m);
        let r = array(m);
        let p = mapping(m);
        let _ = writeln!(
            out,
            "{m:>3} {:>7.1}% {:>7.1}% {:>11} {:>9.3}% {:>8.1}% {:>8.1}% {:>10} {:>7.1}%",
            h.share(),
            a.share(),
            a.worst,
            100.0 * a.worst_share,
            n.share(),
            r.share(),
            r.worst,
            p.share()
        );
    }

    out.push_str(
        "\nwhat an exact offset and extent of b bits each covers before it has to ask\n\n",
    );
    let _ = writeln!(
        out,
        "{:>3} {:>12} {:>14} {:>12}",
        "b", "bits of 128", "covers up to", "left over"
    );
    for &b in SPLITS {
        let used = VER + META + 1 + 2 * b;
        let _ = writeln!(
            out,
            "{b:>3} {:>12} {:>14} {:>12}",
            used,
            size(threshold(b) + 1),
            i64::from(SLOT) - i64::from(used)
        );
    }

    out.push_str(
        "\nA narrowed range that is a scalar or a small member is exactly representable from twelve \
         bits of mantissa upward, because a member of a structure is small and the offsets inside an \
         object are pointer-aligned. That is worth knowing because intra-object overflow was the \
         obvious thing to be afraid of here. A narrowed range that is a large array member is not, \
         and it is the same failure as a whole heap object for the same reason: the length is the \
         declaration's and neither end is anybody's to align.\n",
    );
    out.push_str(
        "\nA whole heap object is the problem, and no allocator fixes it. Putting each block on the \
         alignment its own size needs is what CHERI's allocators do and it settles the base, and the \
         aligned column is what is left after that: the top is still rounded up, because the length \
         is the program's rather than the allocator's, and a length that is not a multiple of the \
         granule has nowhere else to go. So the error floor is about one part in two to the m, which \
         at fourteen bits is up to sixty four bytes past a one megabyte buffer and four kilobytes \
         past a sixty four megabyte one. Those are bytes no bounds check can refuse, and a redzone \
         allocator refuses them today.\n",
    );
    out.push_str(
        "\nThe second table is the other way. Twenty one bits of offset and twenty one of extent fit \
         beside a full version and the meta bits a check reads, cover every object up to two \
         megabytes with no error at all, and leave the flag that says the extent did not fit. Past \
         two megabytes the capability's extent comes out of the object's header, which is thirty two \
         bytes behind the payload and on a line `cap_of` is already on, so it is a load rather than \
         a walk. The error is then zero everywhere rather than small in the place the error matters \
         most, and what it costs is one load on the allocations big enough that one load is nothing.\n",
    );
    out.push_str(
        "\nBoth sixteen byte layouts are full. Judgement C1 wants the epoch the pointer word was \
         written at held beside the capability's own, and neither of these has the room for it, so \
         where that stamp lives is a decision of its own rather than a corner of this one.\n",
    );
    print!("{out}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A base aligned to more than any exponent under test, so that a low bit in a test is one the
    /// test put there.
    const FAR: u64 = 1 << 44;

    #[test]
    fn a_range_that_fits_the_mantissa_is_said_exactly() {
        let bounds = Bounds { lo: FAR + 8, ext: 24 };
        assert_eq!(round(bounds, 14), bounds);
        assert_eq!(slack(bounds, 14), 0);
    }

    #[test]
    fn a_range_too_long_for_the_mantissa_is_widened_at_both_ends() {
        // Three bits of mantissa names seven granules, so twenty one bytes from an odd address
        // needs an exponent of two and both ends go out to a multiple of four.
        let bounds = Bounds { lo: FAR + 1, ext: 20 };
        let held = round(bounds, 3);
        assert_eq!(held.lo, FAR);
        assert_eq!(held.top(), FAR + 24);
        assert_eq!(slack(bounds, 3), 1 + 3);
    }

    #[test]
    fn a_rounded_range_always_contains_the_one_it_stands_for() {
        // The invariant the whole measurement rests on. A bound rounded inward would refuse a
        // correct program, which is a broken compiler rather than a hole with a size.
        for m in [3, 8, 14] {
            for lo in [FAR, FAR + 1, FAR + 7, FAR + 4096 + 13] {
                for ext in [1u64, 3, 16, 1000, 1 << 20, 1 << 30] {
                    let bounds = Bounds { lo, ext };
                    let held = round(bounds, m);
                    assert!(held.lo <= bounds.lo, "m {m} lo {lo} ext {ext}");
                    assert!(held.top() >= bounds.top(), "m {m} lo {lo} ext {ext}");
                }
            }
        }
    }

    #[test]
    fn a_wider_mantissa_is_never_a_worse_answer() {
        for lo in [FAR + 1, FAR + 8, FAR + 4095] {
            for ext in [17u64, 4096, 1 << 17, 1 << 24] {
                let bounds = Bounds { lo, ext };
                let mut last = u64::MAX;
                for m in 4..24 {
                    let now = slack(bounds, m);
                    assert!(now <= last, "m {m} lo {lo} ext {ext}");
                    last = now;
                }
            }
        }
    }

    #[test]
    fn a_range_aligned_at_both_ends_is_exact_however_long_it_is() {
        // Which is what the aligned allocator row is testing against. If both ends sit on a
        // multiple of the exponent's granule then the mantissa only has to count granules.
        for m in [8u32, 14] {
            for e in 0..20u32 {
                let ext = 1u64 << (e + m - 1);
                let lo = FAR.next_multiple_of(1 << exponent(ext, m));
                assert_eq!(slack(Bounds { lo, ext }, m), 0, "m {m} e {e}");
            }
        }
    }

    #[test]
    fn the_exponent_a_size_needs_is_the_smallest_one_that_fits_it() {
        assert_eq!(exponent(1, 14), 0);
        assert_eq!(exponent((1 << 14) - 1, 14), 0);
        assert_eq!(exponent(1 << 14, 14), 1);
        assert_eq!(exponent(1 << 20, 14), 7);
    }

    #[test]
    fn aligning_the_base_helps_and_does_not_finish_the_job() {
        // The finding the recommendation rests on. An allocator can put a block wherever it likes
        // and it cannot change how long the program asked for it to be, so the top is rounded up
        // whatever it does and the error floor is not zero.
        for &m in WIDTHS {
            let plain = heap(m);
            let fixed = aligned(m);
            assert!(fixed.share() >= plain.share(), "m {m}");
            assert!(fixed.share() < 100.0, "m {m} came out exact, which it cannot be");
            assert!(fixed.worst > 0, "m {m}");
        }
    }

    #[test]
    fn what_aligning_the_base_leaves_over_is_about_one_part_in_two_to_the_mantissa() {
        // A length that does not divide the granule is rounded up by less than the granule, and
        // the granule is the length over two to the mantissa, so the share is bounded by that.
        for &m in WIDTHS {
            let over = aligned(m).worst_share;
            assert!(over > 0.0, "m {m}");
            assert!(over < 2.0 / (1u64 << m) as f64, "m {m} over {over}");
        }
    }

    #[test]
    fn a_small_member_is_exact_from_twelve_bits_up_and_a_large_array_is_not() {
        // The claim the report makes. A member of a structure is small, and a large array member
        // inside one fails the same way a whole heap object does and for the same reason.
        assert_eq!(narrowed(12).share(), 100.0);
        assert!(narrowed(8).share() < 100.0);
        assert!(array(14).share() < 100.0);
    }

    #[test]
    fn a_mapping_is_exact_wherever_its_length_fits() {
        // Both ends are page aligned by construction, so nothing here is about the base.
        assert!(mapping(20).share() > mapping(8).share());
    }

    #[test]
    fn an_exact_offset_and_extent_of_twenty_one_bits_covers_two_megabytes_and_fills_the_slot() {
        assert_eq!(threshold(21) + 1, 2 * 1024 * 1024);
        assert_eq!(VER + META + 1 + 2 * 21, SLOT);
    }

    #[test]
    fn a_sixteen_byte_slot_has_no_room_for_a_second_stamp_beside_a_full_version() {
        // The budget half of the question. Judgement C1 needs the epoch the pointer word was
        // written at as well as the capability's own, and the table is what says where it has to
        // come from.
        for l in layouts() {
            if l.bytes == 16 && l.ver == VER {
                assert!(l.spare < 32, "{} has room after all", l.name);
            }
        }
    }
}
