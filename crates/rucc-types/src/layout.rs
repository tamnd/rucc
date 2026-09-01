//! How large a type is and what it has to be aligned to, computed from the target description.
//!
//! Design: `spec/07-types-and-semantics.md` section 7.1 and `spec/18-package-layout.md`
//! section 18.2, which is the rule that none of this may be a `#[cfg]`.
//!
//! Every number here comes out of [`TargetInfo`] rather than out of the host. That is not
//! pedantry: `long` is four bytes on Windows and eight on Linux, `long double` is eight bytes
//! on Apple and sixteen on SysV x86-64, and a cross compiler that asks its own platform gets
//! both of them wrong. The widths were checked against GCC 13 on x86-64 Linux and against
//! clang on AArch64 Darwin rather than recalled.
//!
//! [`integer_info`] is here for the same reason and answers a neighbouring question: not how
//! large the object is but how wide the value in it is, which is not the same number for `bool`
//! or for a `_BitInt` and is what folding a constant depends on.
//!
//! Records are the one thing not computed here. Their layout depends on their members, on
//! bit-field packing and on attributes, so it is computed by whoever walks the members and
//! recorded with [`Types::complete_record`](crate::Types::complete_record); this module reads
//! it back.

use rucc_base::float::Format;
use rucc_target::TargetInfo;

use crate::classify::bare;
use crate::kind::{ArrayLen, FloatKind, IntKind, TypeKind};
use crate::types::{TypeId, Types};

/// The size and alignment of a complete object type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Layout {
    /// The size in bytes, which is what `sizeof` answers.
    pub size: u64,
    /// The alignment in bytes, which is what `_Alignof` answers. Always a power of two.
    pub align: u64,
}

impl Layout {
    /// A layout with the given size and alignment.
    #[must_use]
    pub const fn new(size: u64, align: u64) -> Layout {
        Layout { size, align }
    }

    /// A scalar that is as aligned as it is large, which every one on a 64-bit target is.
    #[must_use]
    const fn scalar(size: u64) -> Layout {
        Layout { size, align: size }
    }
}

/// Why a type has no layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutError {
    /// The type is incomplete: `void`, an array with no size, or a record or enumeration whose
    /// definition has not been seen. GNU C gives `sizeof(void)` the value one, and that is a
    /// dialect decision made where there is a warning to emit, not here.
    Incomplete,
    /// The type is a function type, which has no size at all. GNU C gives it the value one for
    /// the same reason it does for `void`.
    Function,
    /// The type is complete and describes an object larger than the address space, which an
    /// array declaration can ask for by multiplying two innocent looking numbers.
    TooLarge,
}

impl std::fmt::Display for LayoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            LayoutError::Incomplete => "the type is incomplete",
            LayoutError::Function => "a function type has no size",
            LayoutError::TooLarge => "the type is larger than the address space",
        };
        f.write_str(text)
    }
}

impl std::error::Error for LayoutError {}

/// The size and alignment of `id` on `target`.
///
/// # Errors
///
/// [`LayoutError`] when the type has no layout, which is a normal answer rather than a bug:
/// `sizeof` an incomplete type is a diagnostic, and the caller is the one holding the span.
pub fn layout(types: &Types, id: TypeId, target: &TargetInfo) -> Result<Layout, LayoutError> {
    // Sugar has whatever layout the type behind it has, and a typedef of an array of a typedef
    // is common enough that resolving it once here beats resolving it at every arm below.
    let id = types.canonical(id);
    match types.kind(id) {
        TypeKind::Void => Err(LayoutError::Incomplete),
        TypeKind::Bool => Ok(Layout::scalar(1)),
        TypeKind::Int(kind) => Ok(Layout::scalar(u64::from(int_width(kind, target) / 8))),
        TypeKind::Float(kind) => Ok(Layout::scalar(u64::from(float_width(kind, target) / 8))),
        TypeKind::Complex(kind) => {
            // Two of the component, adjacent, with the component's own alignment rather than
            // the pair's. `_Complex long double` on SysV x86-64 is thirty two bytes aligned to
            // sixteen, which is what both GCC and clang report.
            let part = Layout::scalar(u64::from(float_width(kind, target) / 8));
            Ok(Layout::new(part.size * 2, part.align))
        }
        TypeKind::BitInt { width, .. } => Ok(bit_int_layout(width, target)),
        TypeKind::Pointer(_) => Ok(Layout::scalar(u64::from(target.pointer_width / 8))),
        TypeKind::Function(_) => Err(LayoutError::Function),
        TypeKind::Atomic(inner) => {
            let inner = layout(types, inner, target)?;
            Ok(atomic_layout(inner))
        }
        TypeKind::Array { elem, len } => {
            let ArrayLen::Fixed(count) = len else {
                return Err(LayoutError::Incomplete);
            };
            let elem = layout(types, elem, target)?;
            let size = elem.size.checked_mul(count).ok_or(LayoutError::TooLarge)?;
            Ok(Layout::new(size, elem.align))
        }
        TypeKind::Vector { elem, len } => {
            let elem = layout(types, elem, target)?;
            let raw = elem.size.checked_mul(u64::from(len)).ok_or(LayoutError::TooLarge)?;
            Ok(vector_layout(raw))
        }
        TypeKind::Record(record) => types.record_info(record).layout.ok_or(LayoutError::Incomplete),
        TypeKind::Enum(id) => {
            let underlying = types.enum_info(id).underlying.ok_or(LayoutError::Incomplete)?;
            layout(types, underlying, target)
        }
        // Unreachable in practice: the id was canonicalised on the way in. Answering rather
        // than panicking, because a wrong size is easier to find than a crash in a compiler.
        TypeKind::Typedef { underlying, .. } => layout(types, underlying, target),
    }
}

/// The width of a standard integer type in bits.
#[must_use]
pub fn int_width(kind: IntKind, target: &TargetInfo) -> u32 {
    match kind {
        IntKind::Char | IntKind::SChar | IntKind::UChar => 8,
        IntKind::Short | IntKind::UShort => 16,
        IntKind::Int | IntKind::UInt => 32,
        IntKind::Long | IntKind::ULong => target.long_width,
        IntKind::LongLong | IntKind::ULongLong => 64,
        IntKind::Int128 | IntKind::UInt128 => 128,
    }
}

/// What an integer type is once it no longer matters how it was spelled.
///
/// A width and a signedness, which between them are everything the value of an integer constant
/// depends on. `int`, an enumeration represented in `int`, and `_BitInt(32)` are three different
/// types with one [`IntegerInfo`], and every question about what a constant of any of them holds
/// has the same answer for all three.
///
/// The width is the value's and not the object's. `bool` is one bit here and one byte in
/// [`layout`], and `_BitInt(37)` is thirty seven bits here and eight bytes there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IntegerInfo {
    /// Whether the type can hold a negative value.
    pub signed: bool,
    /// How many bits of a value the type keeps.
    pub width: u32,
}

impl IntegerInfo {
    /// An integer type of the given signedness and width.
    #[must_use]
    pub const fn new(signed: bool, width: u32) -> IntegerInfo {
        IntegerInfo { signed, width }
    }

    /// The value `raw` becomes once it is stored in a type of this shape.
    ///
    /// The low `width` bits of it, extended into the rest by the signedness. That is the form a
    /// folded constant is held in, so `300` wrapped by a `char` is `44`, `-1` wrapped by an
    /// `unsigned int` is `4294967295`, and a value of a hundred and twenty eight bit type is
    /// itself, because there is nothing wider left to extend it into.
    #[must_use]
    pub const fn wrap(self, raw: i128) -> i128 {
        if self.width == 0 {
            return 0;
        }
        if self.width >= 128 {
            return raw;
        }
        let unused = 128 - self.width;
        if self.signed {
            (raw << unused) >> unused
        } else {
            (((raw as u128) << unused) >> unused) as i128
        }
    }

    /// Whether `raw` is a value a type of this shape can hold.
    ///
    /// Every hundred and twenty eight bit pattern is a value of a hundred and twenty eight bit
    /// type, of either signedness, which is why this is a question about the width rather than
    /// a comparison against a pair of bounds: `unsigned __int128` has a greatest value that no
    /// [`i128`] can be handed to ask about.
    #[must_use]
    pub const fn holds(self, raw: i128) -> bool {
        self.wrap(raw) == raw
    }
}

/// The signedness and width of an integer type, and [`None`] when `id` is not one.
///
/// Every integer type C has. `bool` is one bit and unsigned, an enumeration answers as whatever
/// it is represented in, a `_BitInt` answers with the width it was written with, and `_Atomic`
/// and a typedef name answer as the type underneath. The coverage is the point: the shape used
/// by the conversion ranks in `convert.rs` deliberately covers only the two the ranks are
/// defined over, and folding a constant with that one would get `bool` and every enumeration
/// wrong rather than refusing them.
#[must_use]
pub fn integer_info(types: &Types, id: TypeId, target: &TargetInfo) -> Option<IntegerInfo> {
    match bare(types, id) {
        TypeKind::Bool => Some(IntegerInfo::new(false, 1)),
        TypeKind::Int(kind) => {
            Some(IntegerInfo::new(kind.is_signed(target.char_is_signed), int_width(kind, target)))
        }
        TypeKind::BitInt { signed, width } => Some(IntegerInfo::new(signed, width)),
        // An enumeration is represented in some integer type, and until its definition has been
        // seen there is no answer to give. Saying so beats picking `int`, because a caller that
        // folds a constant in a width the type does not have folds it wrongly and silently.
        TypeKind::Enum(id) => {
            let underlying = types.enum_info(id).underlying?;
            integer_info(types, underlying, target)
        }
        _ => None,
    }
}

/// The width of a real floating type in bits, including the padding `long double` carries.
///
/// The number for `long double` is storage rather than precision. Eighty bits of x87 occupy
/// sixteen bytes on SysV x86-64, and it is the sixteen that `sizeof` answers with.
#[must_use]
pub fn float_width(kind: FloatKind, target: &TargetInfo) -> u32 {
    match kind {
        FloatKind::Float16 => 16,
        FloatKind::Float | FloatKind::Float32 => 32,
        FloatKind::Double | FloatKind::Float32x | FloatKind::Float64 => 64,
        FloatKind::LongDouble => target.long_double_width,
        // The same sixteen bytes whichever of the two formats it is, for the same reason
        // `long double` is sixteen on x86-64: the x87 eighty bits are stored padded.
        FloatKind::Float64x | FloatKind::Float128 => 128,
    }
}

/// The binary format a real floating type has on `target`.
///
/// Not derivable from [`float_width`], which is why it is a separate question: the width of a
/// `long double` on SysV x86-64 is a hundred and twenty eight bits and its format is the eighty
/// bit x87 one, and a compiler that picked the format by the size would fold every `long double`
/// constant on that target with seventeen decimal digits too many.
#[must_use]
pub fn float_format(kind: FloatKind, target: &TargetInfo) -> Format {
    match kind {
        FloatKind::Float16 => Format::Half,
        FloatKind::Float | FloatKind::Float32 => Format::Single,
        FloatKind::Double | FloatKind::Float32x | FloatKind::Float64 => Format::Double,
        FloatKind::LongDouble => target.long_double_format,
        FloatKind::Float64x => target.float64x_format,
        FloatKind::Float128 => Format::Quad,
    }
}

/// The layout of `_BitInt(width)`.
///
/// Up to 64 bits a `_BitInt` is laid out like the smallest standard integer type that holds
/// it, so the size is the byte count rounded up to a power of two and the alignment is the
/// size. Above that the psABIs treat it as an array of a granule instead, and the granule is
/// not the same everywhere: it is 64 bits on x86-64 and RISC-V and 128 on AArch64, which is
/// why `_BitInt(65)` is sixteen bytes aligned to eight on the first and sixteen bytes aligned
/// to sixteen on the second. Measured with clang 18 on x86-64 Linux and clang on AArch64
/// Darwin, including the cases above 128 bits where the size keeps growing by a granule.
fn bit_int_layout(width: u32, target: &TargetInfo) -> Layout {
    let bytes = u64::from(width).div_ceil(8);
    if bytes <= 8 {
        return Layout::scalar(bytes.max(1).next_power_of_two());
    }
    let granule = u64::from(target.bit_int_granule / 8);
    Layout::new(bytes.next_multiple_of(granule), granule)
}

/// The layout of `_Atomic(T)` given the layout of `T`.
///
/// Same size, and an alignment raised to the size when the size is one of the widths the
/// target can do a lock free access at. That is why `_Atomic` is a type and not a qualifier:
/// a sixteen byte structure is aligned to eight and `_Atomic` of it is aligned to sixteen, and
/// a type system that treated the two as one type would silently disagree with itself about
/// where the object goes. Checked against GCC 13 on x86-64 Linux and clang on AArch64 Darwin,
/// which report exactly that.
fn atomic_layout(inner: Layout) -> Layout {
    if inner.size.is_power_of_two() && inner.size <= 16 {
        return Layout::new(inner.size, inner.align.max(inner.size));
    }
    inner
}

/// The layout of a GNU vector whose elements occupy `raw` bytes in total.
///
/// Rounded up to a power of two and aligned to the whole thing, which is what GCC does with a
/// `vector_size` that is not already one. GCC rejects an element count that is not a power of
/// two and clang rounds instead, so this rounds and leaves the rejecting to whoever is holding
/// the attribute and the dialect.
fn vector_layout(raw: u64) -> Layout {
    let size = raw.max(1).next_power_of_two();
    Layout::scalar(size)
}
