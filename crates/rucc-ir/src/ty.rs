//! The IR type system.
//!
//! Design: `spec/08-ir.md` section 8.2.
//!
//! Much smaller than C's, and deliberately so. Everything C-specific has been resolved by the
//! time lowering runs, and re-deriving any of it here would mean two answers to the same
//! question with nothing keeping them in step.
//!
//! ```text
//! i1 i8 i16 i32 i64 i128 iN     integers, by width, signless
//! f16 f32 f64 f80 f128          floating point, by width
//! ptr                           opaque, no pointee
//! i8x16 f32x4                   fixed vectors
//! void
//! ```
//!
//! Three decisions are worth restating because the rest of the crate depends on them.
//!
//! **Integers are signless.** There is no `u32` beside `i32`. The operation carries the
//! signedness, so `sdiv` and `udiv` are different opcodes over the same type. That halves the
//! type space and removes the family of bugs where the type says one thing and the operation
//! does another.
//!
//! **Pointers are opaque.** A `ptr` has no pointee. The size of an access belongs to the
//! `load` or the `store`, and the aliasing information belongs to the metadata on it, where
//! the effective-type rules can be applied precisely rather than guessed at from a static
//! pointee type that C does not license conclusions from anyway.
//!
//! **Aggregates are not values.** There is no struct type and no array type. Structs and
//! arrays live in memory, a struct assignment is a `memcpy`, and a struct passed by value has
//! been taken apart by the ABI rules before it reaches the IR.

use std::fmt;

/// An IR type.
///
/// Four bytes, packed, because a type sits on every value in a function and a function has a
/// great many values. The alternative, an enum holding a lane type and a lane count, comes out
/// at twelve bytes for the same information, and the tables this goes in are walked often
/// enough for that to show.
///
/// The packing is the low sixteen bits for the width in bits, the next fourteen for the lane
/// count biased by one, and the top two for which of the four kinds it is. That gives a
/// largest integer of [`Type::MAX_BITS`] and a widest vector of [`Type::MAX_LANES`], both of
/// which are past anything a target has.
///
/// ```
/// use rucc_ir::{Float, Type};
///
/// assert_eq!(Type::int(32).to_string(), "i32");
/// assert_eq!(Type::float(Float::F64).to_string(), "f64");
/// assert_eq!(Type::PTR.to_string(), "ptr");
/// assert_eq!(Type::vector(Type::int(8), 16).to_string(), "i8x16");
/// assert_eq!(size_of::<Type>(), 4);
/// ```
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Type(u32);

/// Which of the four kinds a [`Type`] is.
///
/// This is the discriminant on its own, for matching. It says nothing about the width or the
/// lane count, which is why it is separate from the type rather than being the type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    /// No value. The result type of a `store`, of a `call` to a `void` function, and of every
    /// terminator.
    Void,
    /// An integer of some width, with no signedness.
    Int,
    /// A floating point value in one of the formats of [`Float`].
    Float,
    /// An address, with no pointee.
    Ptr,
}

/// A floating point format, named by its width in bits.
///
/// The names are the widths because that is what the textual form uses, and a reader who sees
/// `f80` should not have to know that it occupies sixteen bytes on the stack. That is a layout
/// question and it belongs to the target, not to the type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Float {
    /// IEEE binary16, which is `_Float16` and `__fp16`.
    F16,
    /// IEEE binary32, which is `float` everywhere we care about.
    F32,
    /// IEEE binary64, which is `double`.
    F64,
    /// The x87 80-bit extended format, which is `long double` on x86 SysV.
    F80,
    /// IEEE binary128, which is `_Float128`, and `long double` on AArch64 Linux.
    F128,
}

impl Float {
    /// The width of the format in bits.
    ///
    /// This is the width of the format and not the size of the object. `F80` is eighty bits of
    /// format in a ten, twelve or sixteen byte object depending on the target.
    #[must_use]
    pub const fn bits(self) -> u32 {
        match self {
            Self::F16 => 16,
            Self::F32 => 32,
            Self::F64 => 64,
            Self::F80 => 80,
            Self::F128 => 128,
        }
    }

    /// The format of that width, if there is one.
    #[must_use]
    pub const fn from_bits(bits: u32) -> Option<Self> {
        match bits {
            16 => Some(Self::F16),
            32 => Some(Self::F32),
            64 => Some(Self::F64),
            80 => Some(Self::F80),
            128 => Some(Self::F128),
            _ => None,
        }
    }
}

impl fmt::Display for Float {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "f{}", self.bits())
    }
}

// Where the packing lives. Changing any of these changes the meaning of every `Type` in a
// serialised module, which is why the textual form carries a version.
const BITS_SHIFT: u32 = 0;
const BITS_MASK: u32 = 0xffff;
const LANES_SHIFT: u32 = 16;
const LANES_MASK: u32 = 0x3fff;
const KIND_SHIFT: u32 = 30;

impl Type {
    /// The widest integer that can be represented, which is what limits `_BitInt`.
    ///
    /// Sixteen bits of width is more than any target's `BITINT_MAXWIDTH` and more than any
    /// vector register, and it leaves room in the same four bytes for the lane count.
    pub const MAX_BITS: u32 = BITS_MASK;

    /// The most lanes a vector can have.
    pub const MAX_LANES: u32 = LANES_MASK + 1;

    /// No value.
    pub const VOID: Self = Self::pack(Kind::Void, 0, 1);
    /// An address.
    pub const PTR: Self = Self::pack(Kind::Ptr, 0, 1);
    /// The one-bit integer every comparison produces.
    pub const I1: Self = Self::pack(Kind::Int, 1, 1);

    /// Builds a type from its parts, with no checking. Every public constructor checks first.
    const fn pack(kind: Kind, bits: u32, lanes: u32) -> Self {
        Self((kind as u32) << KIND_SHIFT | (lanes - 1) << LANES_SHIFT | bits << BITS_SHIFT)
    }

    /// An integer `bits` wide.
    ///
    /// # Panics
    ///
    /// Panics if `bits` is zero or above [`Type::MAX_BITS`]. A zero-width integer is not a
    /// thing the IR has, and a caller that computed one has a bug that gets much harder to
    /// find if it is allowed to travel.
    #[must_use]
    pub const fn int(bits: u32) -> Self {
        assert!(bits > 0 && bits <= Self::MAX_BITS, "integer width out of range");
        Self::pack(Kind::Int, bits, 1)
    }

    /// A floating point value in the given format.
    #[must_use]
    pub const fn float(format: Float) -> Self {
        Self::pack(Kind::Float, format.bits(), 1)
    }

    /// A vector of `lanes` copies of `lane`.
    ///
    /// # Panics
    ///
    /// Panics if `lane` is not an integer or a floating point type, if it is itself a vector,
    /// or if `lanes` is zero or above [`Type::MAX_LANES`]. A vector of pointers is not in the
    /// instruction set, so admitting the type would mean admitting a value nothing can be done
    /// with.
    #[must_use]
    pub const fn vector(lane: Self, lanes: u32) -> Self {
        assert!(lanes > 0 && lanes <= Self::MAX_LANES, "lane count out of range");
        assert!(lane.is_scalar(), "a vector's lane is a scalar");
        assert!(
            matches!(lane.kind(), Kind::Int | Kind::Float),
            "a vector's lane is an integer or a floating point value"
        );
        Self::pack(lane.kind(), lane.bits(), lanes)
    }

    /// Which of the four kinds this is.
    #[must_use]
    pub const fn kind(self) -> Kind {
        match self.0 >> KIND_SHIFT {
            0 => Kind::Void,
            1 => Kind::Int,
            2 => Kind::Float,
            _ => Kind::Ptr,
        }
    }

    /// The width of one lane in bits, which for a scalar is the width of the type.
    ///
    /// Zero for `void` and for `ptr`, since the width of an address is a property of the
    /// target and not of the type. Ask the target for it.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0 >> BITS_SHIFT & BITS_MASK
    }

    /// How many lanes this has, which is one unless it is a vector.
    #[must_use]
    pub const fn lanes(self) -> u32 {
        (self.0 >> LANES_SHIFT & LANES_MASK) + 1
    }

    /// Whether this has exactly one lane.
    #[must_use]
    pub const fn is_scalar(self) -> bool {
        self.lanes() == 1
    }

    /// Whether this has more than one lane.
    #[must_use]
    pub const fn is_vector(self) -> bool {
        self.lanes() > 1
    }

    /// The type of one lane, which for a scalar is the type itself.
    #[must_use]
    pub const fn lane(self) -> Self {
        Self::pack(self.kind(), self.bits(), 1)
    }

    /// The same shape as this, with the lane type replaced.
    ///
    /// This is what a comparison does: `icmp` over `i32x4` produces `i1x4`, and the rule that
    /// the lane count is carried across is easier to get right in one place than at every
    /// instruction that needs it.
    ///
    /// # Panics
    ///
    /// Panics under the same conditions as [`Type::vector`].
    #[must_use]
    pub const fn with_lane(self, lane: Self) -> Self {
        Self::vector(lane, self.lanes())
    }

    /// Whether this is an integer, of any width, scalar or vector.
    #[must_use]
    pub const fn is_int(self) -> bool {
        matches!(self.kind(), Kind::Int)
    }

    /// Whether this is a floating point value, scalar or vector.
    #[must_use]
    pub const fn is_float(self) -> bool {
        matches!(self.kind(), Kind::Float)
    }

    /// Whether this is an address. A vector of pointers cannot be built, so this is scalar.
    #[must_use]
    pub const fn is_ptr(self) -> bool {
        matches!(self.kind(), Kind::Ptr)
    }

    /// Whether this is the absence of a value.
    #[must_use]
    pub const fn is_void(self) -> bool {
        matches!(self.kind(), Kind::Void)
    }

    /// The floating point format, if this is one.
    #[must_use]
    pub const fn format(self) -> Option<Float> {
        match self.kind() {
            Kind::Float => Float::from_bits(self.bits()),
            _ => None,
        }
    }

    /// Parses the textual form, which is what the printer writes.
    ///
    /// ```
    /// use rucc_ir::Type;
    ///
    /// assert_eq!(Type::parse("i32"), Some(Type::int(32)));
    /// assert_eq!(Type::parse("f32x4"), Some(Type::vector(Type::float(rucc_ir::Float::F32), 4)));
    /// assert_eq!(Type::parse("i0"), None);
    /// assert_eq!(Type::parse("i32 "), None);
    /// ```
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        if text == "void" {
            return Some(Self::VOID);
        }
        if text == "ptr" {
            return Some(Self::PTR);
        }
        let (head, lanes) = match text.split_once('x') {
            // A lane count of one is not written, so `i8x1` is not a spelling of anything and
            // accepting it would give two texts for one type and break the round trip.
            Some((head, lanes)) => (head, parse_u32(lanes).filter(|&n| n > 1)?),
            None => (text, 1),
        };
        let bits = parse_u32(head.strip_prefix(['i', 'f'])?)?;
        let lane = match head.as_bytes()[0] {
            b'i' if bits > 0 && bits <= Self::MAX_BITS => Self::int(bits),
            b'f' => Self::float(Float::from_bits(bits)?),
            _ => return None,
        };
        if lanes > Self::MAX_LANES {
            return None;
        }
        Some(if lanes == 1 { lane } else { Self::vector(lane, lanes) })
    }
}

/// A decimal `u32` with no sign, no underscores, and no leading zero on a non-zero number.
///
/// `str::parse` would take `+4` and `0004`, and either one would be a second spelling of a
/// type that already has one, which is what breaks a byte for byte round trip.
fn parse_u32(text: &str) -> Option<u32> {
    if text.is_empty() || (text.starts_with('0') && text.len() > 1) {
        return None;
    }
    text.bytes().all(|b| b.is_ascii_digit()).then(|| text.parse().ok())?
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind() {
            Kind::Void => return f.write_str("void"),
            Kind::Ptr => return f.write_str("ptr"),
            Kind::Int => write!(f, "i{}", self.bits())?,
            Kind::Float => write!(f, "f{}", self.bits())?,
        }
        if self.is_vector() {
            write!(f, "x{}", self.lanes())?;
        }
        Ok(())
    }
}

impl fmt::Debug for Type {
    // The `Display` form is the one anybody wants to read, and a derived `Debug` would print
    // the packed integer, which is not information anybody can use.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_type_is_four_bytes() {
        assert_eq!(size_of::<Type>(), 4);
    }

    #[test]
    fn the_parts_come_back_out() {
        let v = Type::vector(Type::int(8), 16);
        assert_eq!(v.kind(), Kind::Int);
        assert_eq!(v.bits(), 8);
        assert_eq!(v.lanes(), 16);
        assert_eq!(v.lane(), Type::int(8));
        assert!(v.is_vector());
        assert!(!v.is_scalar());
    }

    #[test]
    fn a_scalar_has_one_lane_and_is_its_own_lane() {
        let i32_ = Type::int(32);
        assert_eq!(i32_.lanes(), 1);
        assert_eq!(i32_.lane(), i32_);
        assert!(i32_.is_scalar());
    }

    #[test]
    fn void_and_ptr_have_no_width_of_their_own() {
        assert_eq!(Type::VOID.bits(), 0);
        assert_eq!(Type::PTR.bits(), 0);
        assert!(Type::VOID.is_void());
        assert!(Type::PTR.is_ptr());
    }

    #[test]
    fn a_comparison_keeps_the_lane_count() {
        assert_eq!(Type::vector(Type::int(32), 4).with_lane(Type::I1), Type::vector(Type::I1, 4));
        assert_eq!(Type::int(32).with_lane(Type::I1), Type::I1);
    }

    #[test]
    fn the_extremes_are_representable() {
        let widest = Type::int(Type::MAX_BITS);
        assert_eq!(widest.bits(), Type::MAX_BITS);
        let longest = Type::vector(Type::I1, Type::MAX_LANES);
        assert_eq!(longest.lanes(), Type::MAX_LANES);
        assert_eq!(longest.lane(), Type::I1);
    }

    #[test]
    fn every_type_round_trips_through_its_text() {
        let mut types = vec![Type::VOID, Type::PTR];
        for bits in [1, 8, 16, 32, 64, 128, 3, 12, Type::MAX_BITS] {
            types.push(Type::int(bits));
        }
        for format in [Float::F16, Float::F32, Float::F64, Float::F80, Float::F128] {
            types.push(Type::float(format));
        }
        for lanes in [2, 4, 16, Type::MAX_LANES] {
            types.push(Type::vector(Type::int(8), lanes));
            types.push(Type::vector(Type::float(Float::F32), lanes));
        }
        for ty in types {
            let text = ty.to_string();
            assert_eq!(Type::parse(&text), Some(ty), "{text}");
        }
    }

    #[test]
    fn the_texts_that_are_not_types_are_refused() {
        for text in [
            "", "i", "f", "i0", "i8x0", "i8x1", "f24", "f0", "i-1", "i+1", "i08", "i8x01", "int",
            "i32 ", " i32", "i8x", "x4", "i8x4x4", "i65536", "i8x16385", "voidx2", "ptrx2",
        ] {
            assert_eq!(Type::parse(text), None, "{text}");
        }
    }

    #[test]
    fn a_format_knows_its_width_both_ways() {
        for format in [Float::F16, Float::F32, Float::F64, Float::F80, Float::F128] {
            assert_eq!(Float::from_bits(format.bits()), Some(format));
            assert_eq!(Type::float(format).format(), Some(format));
        }
        assert_eq!(Float::from_bits(24), None);
        assert_eq!(Type::int(32).format(), None);
    }

    #[test]
    #[should_panic(expected = "integer width out of range")]
    fn a_zero_width_integer_is_refused() {
        let _ = Type::int(0);
    }

    #[test]
    #[should_panic(expected = "integer width out of range")]
    fn an_integer_wider_than_the_packing_is_refused() {
        let _ = Type::int(Type::MAX_BITS + 1);
    }

    #[test]
    #[should_panic(expected = "lane count out of range")]
    fn a_vector_with_no_lanes_is_refused() {
        let _ = Type::vector(Type::int(8), 0);
    }

    #[test]
    #[should_panic(expected = "a vector's lane is a scalar")]
    fn a_vector_of_vectors_is_refused() {
        let _ = Type::vector(Type::vector(Type::int(8), 2), 2);
    }

    #[test]
    #[should_panic(expected = "an integer or a floating point value")]
    fn a_vector_of_pointers_is_refused() {
        let _ = Type::vector(Type::PTR, 2);
    }
}
