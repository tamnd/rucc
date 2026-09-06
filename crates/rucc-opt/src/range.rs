//! What values an integer can hold: a few intervals, and the bits that are known.
//!
//! Design: `spec/optimizer/10-value-ranges.md`, sections 10.2, 10.4 and 10.7. This module is the
//! representation and [`ops`] is the arithmetic over it. The on-demand query that walks back
//! through branch conditions to answer what a value is at a point is the piece that comes after.
//!
//! Knowing a value is in `[0, 63]` is what removes a bounds check, narrows a sixty four bit
//! multiply to thirty two, proves a shift count is in range, folds a comparison, and tells the
//! switch lowering which of eleven cases are unreachable. Section 10.5 counts six consumers and
//! says the first two are most of the value, which is the argument for building this well rather
//! than building it large.
//!
//! # Three parts and not one
//!
//! **Intervals, plural.** A range is a union of disjoint intervals rather than one `[min, max]`,
//! because the single most useful fact in a C compiler is that a value is not zero, and that is
//! not one interval in every reading. It is what a null check produces and what a division needs.
//! Section 10.2 asks for a small fixed number of them, and this carries [`PAIRS`] with anything
//! beyond that collapsing to the hull, because an unbounded pair count is how a range
//! implementation becomes a memory problem.
//!
//! **Known bits, on the same object.** A mask says which bits are unknown and a value says what
//! the rest are. Section 10.2 says keeping this beside the interval rather than in a lattice of
//! its own is the thing a from-scratch implementation gets wrong, because the two refine each
//! other: the low three bits being zero says the value is a multiple of eight, which narrows an
//! interval, and an interval of `[0, 15]` says the top bits are zero. [`Range::narrow`] is where
//! that happens and every operation that builds a range ends by calling it.
//!
//! **Pointers are separate.** Not here. A pointer range is about null and about provenance and
//! forcing it through integer interval arithmetic produces a pointer in `[0x1000, 0x2000]` that
//! no target promised. Section 10.2 says GCC split `prange` out of `irange` in GCC 14 for this
//! reason, and rucc's split is that this module is about integers and the pointer facts live with
//! the provenance in `alias.rs`, which already has them.
//!
//! Floats have no range here at all. Section 10.2 says to skip them in M4: the interesting facts
//! about a float are whether it is a NaN and what its sign is, the consumers are few, and the
//! traps around signed zero and NaN comparison are many.
//!
//! # Bit patterns, not signed numbers
//!
//! GCC's `irange` holds bounds in the domain of its tree type, which carries a signedness. An IR
//! type here does not: `i32` is thirty two bits and the instruction says how to read them, which
//! is why there is an `icmp slt` and an `icmp ult`. So the intervals in this module are over the
//! **unsigned reading of the bit pattern**, from zero to `2^width - 1`, and the signed facts are
//! recovered from them by [`Range::signed_bounds`], which splits at the sign boundary.
//!
//! This is a departure from the document and it is worth saying why. The property section 10.2
//! cares about is that a range can say a value is not zero, and in this domain that is the one
//! interval `[1, max]` rather than the two the document's example has. What it costs is that a
//! small signed range around zero, `[-5, 5]`, is two intervals rather than one. Both fit in
//! [`PAIRS`], both are exact, and the domain that matches the IR is the one where the arithmetic
//! is exact, because every operation in the IR is defined on bit patterns modulo `2^width`.
//!
//! # How this is wrong
//!
//! Section 10.7 names three ways and [`ops`] answers two of them. Wrapping: `[100, 200] + [100,
//! 200]` in eight bits is not `[200, 400]`, and every operation there is defined modulo the
//! width. Signed overflow: in a signed type without `-fwrapv` it may be assumed not to have
//! happened, so the flags the instruction carries are an argument to every operation that can
//! overflow rather than a check somewhere upstream, because a range computed under one assumption
//! and used under the other is a miscompilation.
//!
//! The third is the one still open: precision loss is invisible. A range that fell back to
//! everything because of a missing case produces correct code that is slower, forever, with no
//! signal. There is no counter here yet because a count is only meaningful per query, and the
//! query is what comes next.

pub mod ops;
pub mod query;

use std::fmt;

use rucc_ir::Type;

/// How many disjoint intervals a range holds before it collapses to their hull.
///
/// Three, which section 10.2 says covers `x != 0`, `x != 0 && x != 1`, and the exclusions a
/// switch produces. A fourth interval is not free: every operation over ranges is quadratic in
/// this number, so the arithmetic that comes next pays for it nine times over.
pub const PAIRS: usize = 3;

/// The widest integer this reasons about.
///
/// A wider one gets [`Range::full`] and that is correct rather than a gap, because a range that
/// says nothing is always true. The consumers in section 10.5 all ask about values that fit in a
/// machine register, and carrying arbitrary precision through the arithmetic to serve a
/// `_BitInt(256)` nobody has asked about would be paid for on every query.
pub const MAX_BITS: u32 = 128;

/// Which bits of a value are known, and what they are.
///
/// A set bit in `unknown` means that bit could be either. A clear one means `value` has it. The
/// two are kept canonical, so a bit that is unknown is zero in `value`, which makes equality mean
/// what it looks like.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Bits {
    value: u128,
    unknown: u128,
}

impl Bits {
    /// Nothing known at this width.
    #[must_use]
    pub const fn unknown(width: u32) -> Self {
        Self { value: 0, unknown: mask(width) }
    }

    /// Every bit known, and these are they.
    #[must_use]
    pub const fn exactly(value: u128, width: u32) -> Self {
        Self { value: value & mask(width), unknown: 0 }
    }

    /// The bits that are known, as a mask.
    #[must_use]
    pub const fn known(self, width: u32) -> u128 {
        !self.unknown & mask(width)
    }

    /// What the known bits are, with the unknown ones zero.
    #[must_use]
    pub const fn value(self) -> u128 {
        self.value
    }

    /// The smallest value these bits allow, which is the unknown ones all zero.
    #[must_use]
    pub const fn min(self) -> u128 {
        self.value
    }

    /// The largest, which is the unknown ones all one.
    #[must_use]
    pub const fn max(self) -> u128 {
        self.value | self.unknown
    }

    /// Whether this value is one these bits allow.
    #[must_use]
    pub const fn allows(self, value: u128) -> bool {
        value & !self.unknown == self.value
    }

    /// Bits from a value and a mask of which of them mean nothing.
    ///
    /// This is the way in for an operation that worked out its answer a bit at a time, which is
    /// every bitwise operation. The value is canonicalized, so a bit that is unknown comes back
    /// zero however it went in.
    #[must_use]
    pub const fn from_parts(value: u128, unknown: u128, width: u32) -> Self {
        let unknown = unknown & mask(width);
        Self { value: value & mask(width) & !unknown, unknown }
    }

    /// The bits that mean nothing, as a mask.
    #[must_use]
    pub const fn unknown_bits(self) -> u128 {
        self.unknown
    }

    /// How many low bits are known to be zero, which is what says a value is a multiple of a
    /// power of two and so what an alignment fact is made of.
    #[must_use]
    pub const fn low_zeros(self) -> u32 {
        (self.value | self.unknown).trailing_zeros()
    }

    /// Everything both of these know, or `None` when they contradict each other.
    ///
    /// A contradiction is not a failure. It is the proof that whatever produced the two facts is
    /// on a path that is never taken, and the caller turns it into the empty range.
    #[must_use]
    pub fn meet(self, other: Self) -> Option<Self> {
        let both = self.known(MAX_BITS) & other.known(MAX_BITS);
        if self.value & both != other.value & both {
            return None;
        }
        let unknown = self.unknown & other.unknown;
        Some(Self { value: (self.value | other.value) & !unknown, unknown })
    }

    /// Only what both of these agree on, which is what a value from either place is known to be.
    #[must_use]
    pub fn join(self, other: Self) -> Self {
        let differ = self.value ^ other.value;
        let unknown = self.unknown | other.unknown | differ;
        Self { value: self.value & !unknown, unknown }
    }

    /// The bits every value in this interval agrees on, which is the prefix the two ends share.
    fn of_interval(lo: u128, hi: u128, width: u32) -> Self {
        // Above the highest bit where the two ends differ, every value between them agrees with
        // both. At and below it, some value in between has each way, since the interval is the
        // whole run of numbers from one end to the other.
        let differ = lo ^ hi;
        let below = if differ == 0 { 0 } else { u128::MAX >> differ.leading_zeros() };
        let unknown = below & mask(width);
        Self { value: lo & !unknown, unknown }
    }
}

impl fmt::Debug for Bits {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.unknown == 0 {
            return write!(f, "{:#x}", self.value);
        }
        write!(f, "{:#x}/{:#x}", self.value, self.unknown)
    }
}

/// What an integer value can be.
///
/// A few disjoint intervals over the unsigned reading of the bit pattern, and the bits that are
/// known, at a width. The two halves are kept consistent with each other by [`Range::narrow`], so
/// a range that came out of any constructor here has intervals no wider than its bits allow and
/// bits no vaguer than its intervals prove.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Range {
    /// The intervals, ascending, disjoint and not touching. Empty when the range is.
    pairs: [(u128, u128); PAIRS],
    count: u8,
    width: u32,
    bits: Bits,
}

impl Range {
    /// Nothing at all, which is the range of a value on a path that is never taken.
    #[must_use]
    pub const fn empty(width: u32) -> Self {
        Self {
            pairs: [(0, 0); PAIRS],
            count: 0,
            width: clamp(width),
            bits: Bits { value: 0, unknown: 0 },
        }
    }

    /// Every value of this width, which is what is known about a value nothing has said anything
    /// about.
    #[must_use]
    pub const fn full(width: u32) -> Self {
        let width = clamp(width);
        Self {
            pairs: [(0, mask(width)), (0, 0), (0, 0)],
            count: 1,
            width,
            bits: Bits::unknown(width),
        }
    }

    /// Everything a value of this type can be.
    ///
    /// A type that is not a scalar integer gets the widest full range, because saying nothing
    /// about a vector or a pointer is always true and this module is about integers.
    #[must_use]
    pub fn of(ty: Type) -> Self {
        if ty.is_int() && ty.is_scalar() {
            return Self::full(ty.bits());
        }
        Self::full(MAX_BITS)
    }

    /// One value.
    #[must_use]
    pub fn exactly(value: u128, width: u32) -> Self {
        let width = clamp(width);
        let value = value & mask(width);
        Self {
            pairs: [(value, value), (0, 0), (0, 0)],
            count: 1,
            width,
            bits: Bits::exactly(value, width),
        }
    }

    /// Every bit pattern from one bound to the other, inclusive, wrapping if the low bound is
    /// above the high one.
    ///
    /// The wrapping case is what a signed interval becomes here: `[-5, 5]` in eight bits is
    /// `[0xfb, 0x05]`, which is the two intervals `[0, 5]` and `[0xfb, 0xff]`, and taking the
    /// bounds in that order is how a caller says so without having to split it itself.
    #[must_use]
    pub fn between(lo: u128, hi: u128, width: u32) -> Self {
        let width = clamp(width);
        let (lo, hi) = (lo & mask(width), hi & mask(width));
        if lo <= hi {
            return Self::from_pairs(&[(lo, hi)], width);
        }
        Self::from_pairs(&[(0, hi), (lo, mask(width))], width)
    }

    /// Every value from one signed bound to the other, inclusive.
    ///
    /// The bounds are read as signed numbers of that width and the intervals come out over bit
    /// patterns, so `[-5, 5]` in eight bits becomes `[0, 5]` and `[0xfb, 0xff]` on its own. A
    /// signed interval is always one wrapping interval in the unsigned domain, so nothing is lost
    /// on the way through.
    #[must_use]
    pub fn signed_between(lo: i128, hi: i128, width: u32) -> Self {
        let width = clamp(width);
        let (low, high) = signed_limits(width);
        if lo > hi || lo > high || hi < low {
            return Self::empty(width);
        }
        let (lo, hi) = (lo.max(low), hi.min(high));
        Self::between(lo as u128, hi as u128, width)
    }

    /// Every value except this one.
    ///
    /// `Range::other_than(0, width)` is the non-zero range, which section 10.2 calls the single
    /// most useful range fact in a C compiler.
    #[must_use]
    pub fn other_than(value: u128, width: u32) -> Self {
        let width = clamp(width);
        let value = value & mask(width);
        let mut pairs: Vec<(u128, u128)> = Vec::with_capacity(2);
        if value > 0 {
            pairs.push((0, value - 1));
        }
        if value < mask(width) {
            pairs.push((value + 1, mask(width)));
        }
        Self::from_pairs(&pairs, width)
    }

    /// A range from intervals that need not be sorted, disjoint or in bounds.
    ///
    /// This is the way in from an operation that produced a handful of intervals and does not
    /// want to think about their order. Anything beyond [`PAIRS`] of them after merging collapses
    /// to the hull of the ones that did not fit, which loses precision and never soundness.
    #[must_use]
    pub fn from_pairs(pairs: &[(u128, u128)], width: u32) -> Self {
        let width = clamp(width);
        let mut sorted: Vec<(u128, u128)> = pairs
            .iter()
            .map(|&(lo, hi)| (lo & mask(width), hi & mask(width)))
            .filter(|&(lo, hi)| lo <= hi)
            .collect();
        sorted.sort_unstable();

        // Merge what overlaps or touches. Two intervals that touch are one interval, and leaving
        // them apart would spend a pair on a boundary that says nothing.
        let mut merged: Vec<(u128, u128)> = Vec::with_capacity(sorted.len());
        for (lo, hi) in sorted {
            match merged.last_mut() {
                Some(last) if lo <= last.1.saturating_add(1) => last.1 = last.1.max(hi),
                _ => merged.push((lo, hi)),
            }
        }

        // Too many, so the tail becomes its hull. The tail rather than the head because the
        // intervals are sorted, so this keeps the low bound exact and gives up the shape in the
        // middle, which is the half a consumer asks about less often.
        if merged.len() > PAIRS {
            let tail = merged.get(PAIRS - 1..).unwrap_or_default().to_vec();
            let lo = tail.iter().map(|pair| pair.0).min().unwrap_or(0);
            let hi = tail.iter().map(|pair| pair.1).max().unwrap_or(0);
            merged.truncate(PAIRS - 1);
            merged.push((lo, hi));
        }

        let mut range = Self::empty(width);
        for (index, &pair) in merged.iter().enumerate() {
            range.pairs[index] = pair;
        }
        range.count = u8::try_from(merged.len().min(PAIRS)).unwrap_or(0);
        range.bits = range.bits_of_pairs();
        range
    }

    /// The same intervals with these bits also known.
    ///
    /// The two refine each other here and nowhere else, which is what section 10.2 asks for. An
    /// interval whose ends the bits rule out is pulled in to the nearest value the bits allow,
    /// the bits are then recomputed from what survived, and a range whose halves contradict each
    /// other comes back empty.
    #[must_use]
    pub fn narrow(self, bits: Bits) -> Self {
        let Some(bits) = self.bits.meet(bits) else {
            return Self::empty(self.width);
        };
        if self.is_empty() {
            return self;
        }

        // Everything the bits allow is between their least and their greatest, so an interval
        // outside that is empty and one that straddles the edge shrinks to fit. Then the ends
        // move again to the nearest value with the right low zeroes, which is what turns "this is
        // a multiple of four" and "this is somewhere in `[1, 3]`" into the one value it can be.
        let (low, high) = (bits.min(), bits.max());
        let step = match bits.low_zeros() {
            zeros if zeros == 0 || zeros >= self.width => 1,
            zeros => 1u128 << zeros,
        };
        let kept: Vec<(u128, u128)> = self
            .pairs()
            .iter()
            .map(|&(lo, hi)| (lo.max(low), hi.min(high)))
            .filter(|&(lo, hi)| lo <= hi)
            .filter_map(|(lo, hi)| {
                Some((lo.checked_add(step - 1)? & !(step - 1), hi & !(step - 1)))
            })
            .filter(|&(lo, hi)| lo <= hi)
            .collect();

        let mut range = Self::from_pairs(&kept, self.width);
        range.bits = match range.bits.meet(bits) {
            Some(bits) => bits,
            None => return Self::empty(self.width),
        };
        range
    }

    /// The width the values are, in bits.
    #[must_use]
    pub const fn width(self) -> u32 {
        self.width
    }

    /// The intervals, ascending and disjoint.
    #[must_use]
    pub fn pairs(&self) -> &[(u128, u128)] {
        &self.pairs[..self.count as usize]
    }

    /// The bits that are known about every value in it.
    #[must_use]
    pub const fn bits(self) -> Bits {
        self.bits
    }

    /// Every value in it, or `None` when there are more than that many.
    ///
    /// For walking a shift count or a switch selector, where the range is usually a handful of
    /// values and enumerating them gives an exact answer that reasoning about the bounds would
    /// round off. The limit is what stops that turning into a walk over four billion of them.
    #[must_use]
    pub fn list(self, limit: usize) -> Option<Vec<u128>> {
        let mut values = Vec::new();
        for &(lo, hi) in self.pairs() {
            if hi - lo >= limit as u128 {
                return None;
            }
            for value in lo..=hi {
                if values.len() == limit {
                    return None;
                }
                values.push(value);
            }
        }
        Some(values)
    }

    /// Whether nothing is in it, which means the value is on a path that is never taken.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.count == 0
    }

    /// Whether everything is in it, which means nothing is known.
    #[must_use]
    pub fn is_full(self) -> bool {
        match self.pairs() {
            [(0, hi)] => *hi == mask(self.width),
            _ => false,
        }
    }

    /// The one value in it, if there is exactly one.
    #[must_use]
    pub fn singleton(self) -> Option<u128> {
        match self.pairs() {
            [(lo, hi)] if lo == hi => Some(*lo),
            _ => None,
        }
    }

    /// Whether this value is in it.
    ///
    /// A range is its intervals and its bits together, so this asks both. A value inside one of
    /// the intervals whose bits are wrong is not in the range, which is what makes "somewhere in
    /// `[0, 1023]` and a multiple of eight" mean the hundred and twenty eight values it says
    /// rather than the thousand and twenty four the interval alone would.
    #[must_use]
    pub fn contains(self, value: u128) -> bool {
        let value = value & mask(self.width);
        self.bits.allows(value) && self.pairs().iter().any(|&(lo, hi)| lo <= value && value <= hi)
    }

    /// The least and greatest, read as unsigned, or `None` when the range is empty.
    #[must_use]
    pub fn unsigned_bounds(self) -> Option<(u128, u128)> {
        let pairs = self.pairs();
        Some((pairs.first()?.0, pairs.last()?.1))
    }

    /// The least and greatest, read as signed at this width, or `None` when the range is empty.
    ///
    /// The intervals are over bit patterns, so the signed answer is not the first and last of
    /// them. Everything at or above the sign boundary is negative and sorts below everything
    /// under it, so the least signed value is the first pattern at or above the boundary when
    /// there is one and the first pattern otherwise. That reordering is the whole of what the
    /// unsigned domain costs, and it is eleven lines.
    #[must_use]
    pub fn signed_bounds(self) -> Option<(i128, i128)> {
        let pairs = self.pairs();
        let (first, _) = *pairs.first()?;
        let (_, last) = *pairs.last()?;
        let boundary = sign_bit(self.width);
        let negative = pairs.iter().find(|&&(_, hi)| hi >= boundary);
        let positive = pairs.iter().rev().find(|&&(lo, _)| lo < boundary);
        let min = match negative {
            Some(&(lo, _)) => signed(lo.max(boundary), self.width),
            None => signed(first, self.width),
        };
        let max = match positive {
            Some(&(_, hi)) => signed(hi.min(boundary - 1), self.width),
            None => signed(last, self.width),
        };
        Some((min, max))
    }

    /// Whether nothing in it is zero.
    ///
    /// The fact a null check produces and the fact a division needs, which is why it has a name
    /// of its own rather than being spelled out at every call.
    #[must_use]
    pub fn nonzero(self) -> bool {
        !self.is_empty() && !self.contains(0)
    }

    /// Whether every value in it fits in that many bits, read as unsigned.
    #[must_use]
    pub fn fits_unsigned(self, bits: u32) -> bool {
        match self.unsigned_bounds() {
            None => true,
            Some((_, high)) => bits >= self.width || high <= mask(bits),
        }
    }

    /// Whether every value in it fits in that many bits, read as signed.
    #[must_use]
    pub fn fits_signed(self, bits: u32) -> bool {
        let Some((low, high)) = self.signed_bounds() else {
            return true;
        };
        if bits >= self.width {
            return true;
        }
        let limit = 1i128 << (bits - 1);
        -limit <= low && high < limit
    }

    /// Everything in either of them.
    ///
    /// # Panics
    ///
    /// Panics if the two are of different widths, since a value is one width and combining the
    /// ranges of two that are not is a question with no answer.
    #[must_use]
    pub fn union(self, other: Self) -> Self {
        assert_eq!(self.width, other.width, "these are ranges of different widths");
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        let mut pairs = self.pairs().to_vec();
        pairs.extend_from_slice(other.pairs());
        let range = Self::from_pairs(&pairs, self.width);
        // The bits of a union are only what both sides agree on, and that can be sharper than
        // what the merged intervals show, since three ones and a hull have lost the shape the
        // bits still remember.
        range.narrow(self.bits.join(other.bits))
    }

    /// Everything in both of them.
    ///
    /// # Panics
    ///
    /// Panics if the two are of different widths.
    #[must_use]
    pub fn intersect(self, other: Self) -> Self {
        assert_eq!(self.width, other.width, "these are ranges of different widths");
        let mut pairs: Vec<(u128, u128)> = Vec::with_capacity(PAIRS * PAIRS);
        for &(lo, hi) in self.pairs() {
            for &(start, end) in other.pairs() {
                let (lo, hi) = (lo.max(start), hi.min(end));
                if lo <= hi {
                    pairs.push((lo, hi));
                }
            }
        }
        // Both sets of bits and not just the intervals, because a fact like "this is even" lives
        // only in the bits and intersecting the intervals alone would drop it.
        Self::from_pairs(&pairs, self.width).narrow(self.bits).narrow(other.bits)
    }

    /// Everything of this width that is not in it.
    #[must_use]
    pub fn invert(self) -> Self {
        let mut pairs: Vec<(u128, u128)> = Vec::with_capacity(PAIRS + 1);
        let mut next = 0u128;
        for &(lo, hi) in self.pairs() {
            if lo > next {
                pairs.push((next, lo - 1));
            }
            // The top interval can end at the largest value there is, and there is nothing above
            // it to start the next gap at.
            let Some(after) = hi.checked_add(1) else {
                return Self::from_pairs(&pairs, self.width);
            };
            next = after;
        }
        if next <= mask(self.width) {
            pairs.push((next, mask(self.width)));
        }
        Self::from_pairs(&pairs, self.width)
    }

    /// The bits every interval agrees on.
    fn bits_of_pairs(&self) -> Bits {
        let mut bits: Option<Bits> = None;
        for &(lo, hi) in self.pairs() {
            let one = Bits::of_interval(lo, hi, self.width);
            bits = Some(bits.map_or(one, |had: Bits| had.join(one)));
        }
        bits.unwrap_or(Bits { value: 0, unknown: 0 })
    }
}

impl fmt::Debug for Range {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "i{}", self.width)?;
        if self.is_empty() {
            return write!(f, " empty");
        }
        for (index, &(lo, hi)) in self.pairs().iter().enumerate() {
            let separator = if index == 0 { " " } else { " u " };
            if lo == hi {
                write!(f, "{separator}[{lo:#x}]")?;
            } else {
                write!(f, "{separator}[{lo:#x}, {hi:#x}]")?;
            }
        }
        if self.bits.unknown != mask(self.width) {
            write!(f, " bits {:?}", self.bits)?;
        }
        Ok(())
    }
}

/// Every bit of that width set.
const fn mask(width: u32) -> u128 {
    if width >= MAX_BITS { u128::MAX } else { (1u128 << width) - 1 }
}

/// The lowest bit pattern that reads as negative at that width.
const fn sign_bit(width: u32) -> u128 {
    1u128 << (width - 1)
}

/// That bit pattern read as a signed number of that width.
const fn signed(value: u128, width: u32) -> i128 {
    // Up to the top and back down again, which is sign extension without a branch on the width.
    let shift = MAX_BITS - width;
    ((value << shift) as i128) >> shift
}

/// The least and greatest signed numbers of that width.
const fn signed_limits(width: u32) -> (i128, i128) {
    if width >= MAX_BITS {
        return (i128::MIN, i128::MAX);
    }
    let high = (1i128 << (width - 1)) - 1;
    (!high, high)
}

/// A width this module reasons about, which is at least one bit and at most [`MAX_BITS`].
const fn clamp(width: u32) -> u32 {
    if width == 0 {
        return 1;
    }
    if width > MAX_BITS { MAX_BITS } else { width }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every value of that width, as the set a range is being checked against.
    fn every(width: u32) -> Vec<u128> {
        (0..=mask(width)).collect()
    }

    /// The values a range says it holds, as a list.
    fn held(range: Range) -> Vec<u128> {
        every(range.width()).into_iter().filter(|&value| range.contains(value)).collect()
    }

    /// Every range this width can describe exactly, which is every subset of its values that
    /// fits in [`PAIRS`] intervals.
    ///
    /// Section 10.4 says a claim about ranges at width four is exhaustively checkable, and this
    /// is what makes that true of the representation as well as of the arithmetic to come. Width
    /// four is ten thousand ranges, which is nothing to walk once and too many to walk against
    /// each other, so the properties about one range use four and the properties about two use
    /// three.
    fn all_at(width: u32) -> Vec<Range> {
        let mut ranges = Vec::new();
        for subset in 0u64..1 << (1u64 << width) {
            let values: Vec<u128> =
                (0..=mask(width)).filter(|&value| subset & (1 << value) != 0).collect();
            let pairs = runs(&values);
            if pairs.len() > PAIRS {
                continue;
            }
            let range = Range::from_pairs(&pairs, width);
            // Only the subsets a range describes exactly, so a property that fails is a property
            // and not a rounding.
            if held(range) == values {
                ranges.push(range);
            }
        }
        ranges
    }

    /// The values grouped into runs of consecutive ones.
    fn runs(values: &[u128]) -> Vec<(u128, u128)> {
        let mut pairs: Vec<(u128, u128)> = Vec::new();
        for &value in values {
            match pairs.last_mut() {
                Some(last) if last.1 + 1 == value => last.1 = value,
                _ => pairs.push((value, value)),
            }
        }
        pairs
    }

    #[test]
    fn nothing_and_everything_are_what_they_say() {
        let empty = Range::empty(8);
        assert!(empty.is_empty());
        assert!(!empty.is_full());
        assert!(!empty.contains(0));
        assert_eq!(empty.unsigned_bounds(), None);
        assert_eq!(empty.signed_bounds(), None);

        let full = Range::full(8);
        assert!(full.is_full());
        assert!(!full.is_empty());
        assert_eq!(full.unsigned_bounds(), Some((0, 255)));
        assert_eq!(full.signed_bounds(), Some((-128, 127)));
        assert_eq!(full.bits(), Bits::unknown(8));
    }

    #[test]
    fn the_fact_a_null_check_produces_is_one_interval() {
        let nonzero = Range::other_than(0, 32);
        assert_eq!(nonzero.pairs(), [(1, 0xffff_ffff)]);
        assert!(nonzero.nonzero());
        assert!(!nonzero.contains(0));
        // And the reason the domain is bit patterns rather than signed numbers: this is the fact
        // section 10.2 calls the most useful one in a C compiler, and here it costs one pair.
        assert_eq!(nonzero.pairs().len(), 1);
    }

    #[test]
    fn a_signed_interval_around_zero_is_two_intervals_and_still_exact() {
        // What `[-5, 5]` in eight bits comes to, which is the case the unsigned domain pays for.
        let around = Range::between(0xfb, 0x05, 8);
        assert_eq!(around.pairs(), [(0x00, 0x05), (0xfb, 0xff)]);
        assert_eq!(around.signed_bounds(), Some((-5, 5)));
        assert_eq!(around.unsigned_bounds(), Some((0, 255)));
    }

    #[test]
    fn signed_bounds_are_right_wherever_the_range_sits() {
        for width in [4u32, 8, 16, 32, 64] {
            let cases: [(Range, (i128, i128)); 4] = [
                (Range::full(width), (-(1 << (width - 1)), (1 << (width - 1)) - 1)),
                (Range::exactly(mask(width), width), (-1, -1)),
                (Range::between(0, 1, width), (0, 1)),
                (Range::between(sign_bit(width), mask(width), width), (-(1 << (width - 1)), -1)),
            ];
            for (range, want) in cases {
                assert_eq!(range.signed_bounds(), Some(want), "{range:?} at {width}");
            }
        }
    }

    #[test]
    fn an_interval_says_what_bits_it_knows() {
        // Everything from 8 to 11 has the top five bits of a byte clear and the fourth set.
        let range = Range::between(8, 11, 8);
        assert_eq!(range.bits().known(8), 0b1111_1100);
        assert_eq!(range.bits().value(), 0b0000_1000);

        // And a single value knows all of them.
        assert_eq!(Range::exactly(0x5a, 8).bits(), Bits::exactly(0x5a, 8));
    }

    #[test]
    fn known_bits_pull_the_intervals_in() {
        // Multiples of eight, from anywhere in a byte, is 0, 8, 16 and so on, so the range that
        // was everything comes back with the ends it can actually reach.
        let multiples = Bits { value: 0, unknown: 0b1111_1000 };
        let range = Range::full(8).narrow(multiples);
        assert_eq!(range.unsigned_bounds(), Some((0, 0b1111_1000)));
        assert!(range.contains(0b1111_1000));
        assert!(!range.contains(0b1111_1001));
    }

    #[test]
    fn intervals_and_bits_that_contradict_each_other_come_back_empty() {
        // Nothing between 8 and 11 is odd.
        let odd = Bits { value: 1, unknown: !1 & mask(8) };
        assert!(Range::between(8, 8, 8).narrow(odd).is_empty());
        // And the same the other way about, through an intersection.
        let evens = Range::full(8).narrow(Bits { value: 0, unknown: !1 & mask(8) });
        assert!(evens.intersect(Range::exactly(7, 8)).is_empty());
    }

    #[test]
    fn more_intervals_than_there_is_room_for_lose_precision_and_not_soundness() {
        // Five separate values in four bits, which is two more than a range can hold.
        let pairs = [(0, 0), (2, 2), (4, 4), (6, 6), (8, 8)];
        let range = Range::from_pairs(&pairs, 4);
        assert_eq!(range.pairs().len(), PAIRS);
        for (value, _) in pairs {
            assert!(range.contains(value), "{range:?} lost {value}");
        }
    }

    #[test]
    fn a_range_holds_exactly_what_it_was_built_from() {
        for range in all_at(4) {
            let listed = held(range);
            assert_eq!(Range::from_pairs(&runs(&listed), 4), range, "{range:?}");
        }
    }

    #[test]
    fn union_and_intersection_are_the_set_operations_they_are_named_after() {
        // Three bits rather than four because this is quadratic in the number of ranges, and the
        // property does not get any truer with ten thousand of them on each side.
        let all = all_at(3);
        for &a in &all {
            for &b in &all {
                let (left, right) = (held(a), held(b));

                let either: Vec<u128> = every(3)
                    .into_iter()
                    .filter(|value| left.contains(value) || right.contains(value))
                    .collect();
                check(a.union(b), &either, &format!("{a:?} u {b:?}"));

                let both: Vec<u128> =
                    left.iter().copied().filter(|value| right.contains(value)).collect();
                check(a.intersect(b), &both, &format!("{a:?} n {b:?}"));
            }
        }
    }

    #[test]
    fn inverting_gives_back_everything_that_was_not_in_it() {
        for range in all_at(4) {
            let want: Vec<u128> =
                every(4).into_iter().filter(|value| !range.contains(*value)).collect();
            let flipped = range.invert();
            check(flipped, &want, &format!("not({range:?})"));
            // The complement of a complement is what it started with, and both of them fit,
            // since a range that fits is one whose complement collapsed to at most three runs.
            if runs(&want).len() <= PAIRS {
                assert_eq!(held(flipped.invert()), held(range), "not(not({range:?}))");
            }
        }
    }

    /// The result holds everything it should, and holds nothing more whenever there was room to
    /// say so.
    ///
    /// The asymmetry is the whole contract of a fixed pair count. Losing a value would be a
    /// miscompilation, so that is checked always. Gaining one is precision loss, so it is
    /// allowed, but only when the exact answer needed more than [`PAIRS`] intervals: a range that
    /// gave up with room to spare is a bug in the merging and not a limit of the representation.
    fn check(got: Range, want: &[u128], what: &str) {
        let listed = held(got);
        for value in want {
            assert!(listed.contains(value), "{what} lost {value:#x}");
        }
        if runs(want).len() <= PAIRS {
            assert_eq!(listed, want, "{what} gave up with room to spare");
        }
    }

    #[test]
    fn the_bits_of_a_range_are_true_of_every_value_in_it() {
        for range in all_at(4) {
            let bits = range.bits();
            for value in held(range) {
                assert!(bits.allows(value), "{range:?} says {value:#x} but its bits do not");
            }
        }
    }

    #[test]
    fn the_bounds_of_a_range_are_the_bounds_of_what_is_in_it() {
        for range in all_at(4) {
            let values = held(range);
            let Some(&first) = values.first() else {
                assert_eq!(range.unsigned_bounds(), None);
                continue;
            };
            let last = *values.last().expect("not empty");
            assert_eq!(range.unsigned_bounds(), Some((first, last)), "{range:?}");

            let as_signed: Vec<i128> = values.iter().map(|&v| signed(v, 4)).collect();
            let low = *as_signed.iter().min().expect("not empty");
            let high = *as_signed.iter().max().expect("not empty");
            assert_eq!(range.signed_bounds(), Some((low, high)), "{range:?}");
        }
    }

    #[test]
    fn fitting_in_a_narrower_type_means_every_value_in_it_does() {
        for range in all_at(4) {
            for bits in 1..=4u32 {
                let values = held(range);
                let unsigned = values.iter().all(|&value| value <= mask(bits));
                assert_eq!(range.fits_unsigned(bits), unsigned, "{range:?} in u{bits}");

                let limit = 1i128 << (bits - 1);
                let signed_fits =
                    values.iter().all(|&value| (-limit..limit).contains(&signed(value, 4)));
                assert_eq!(range.fits_signed(bits), signed_fits, "{range:?} in i{bits}");
            }
        }
    }

    #[test]
    fn a_type_that_is_not_a_scalar_integer_gets_a_range_that_says_nothing() {
        assert!(Range::of(Type::PTR).is_full());
        assert!(Range::of(Type::int(32)).is_full());
        assert_eq!(Range::of(Type::int(32)).width(), 32);
        assert_eq!(Range::of(Type::PTR).width(), MAX_BITS);
    }

    #[test]
    fn a_width_wider_than_this_reasons_about_is_clamped_rather_than_wrong() {
        let wide = Range::full(256);
        assert_eq!(wide.width(), MAX_BITS);
        assert!(wide.is_full());
    }

    #[test]
    fn bits_that_contradict_each_other_have_no_meet() {
        let zero = Bits::exactly(0, 8);
        let one = Bits::exactly(1, 8);
        assert_eq!(zero.meet(one), None);
        assert_eq!(zero.meet(Bits::unknown(8)), Some(zero));
        // And a join keeps only what they agree on, which for these is the seven high bits.
        assert_eq!(zero.join(one), Bits { value: 0, unknown: 1 });
    }
}
