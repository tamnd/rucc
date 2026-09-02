//! Laying out a `struct` or a `union`, bit-fields included.
//!
//! Design: `spec/07-types-and-semantics.md` section 7.1.
//!
//! This is the one layout question that cannot be answered from a type alone, because the
//! answer depends on the members, on how they are packed and on attributes the program wrote.
//! So it lives apart from [`crate::layout`]: whoever parsed the members calls [`layout_record`]
//! and hands the result to [`Types::complete_record`](crate::Types::complete_record), and from
//! then on the record has a size like any other type.
//!
//! Every rule here was measured rather than recalled, with gcc 13.3 on x86-64 Linux and clang
//! on AArch64 Darwin, over about fifty structures covering bit-field packing, zero width
//! bit-fields, `packed`, `#pragma pack`, `aligned` on a member and on the record, anonymous
//! members and flexible array members. The two compilers agreed on every one of them except
//! where `long double` differs, which is a fact about the member type rather than about the
//! record. Several of the rules below are not what a reading of the psABI documents suggests,
//! which is exactly why they were measured.

use rucc_target::TargetInfo;

use crate::kind::{ArrayLen, RecordKind, TypeKind};
use crate::layout::{Layout, LayoutError, layout};
use crate::types::{TypeId, Types};

/// One member of a record as the program wrote it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldDecl {
    /// The member name, absent for an unnamed bit-field or an anonymous member.
    pub name: Option<rucc_base::Symbol>,
    /// The member type. For an anonymous `struct` or `union` member this is that record.
    pub ty: TypeId,
    /// The bit-field width, absent when the member is an ordinary one.
    ///
    /// Zero is allowed and means the zero width bit-field, which has to be unnamed and which
    /// exists only to push the next member to the next boundary.
    pub bits: Option<u32>,
    /// An alignment the program asked for with `_Alignas` or `aligned`, in bytes.
    ///
    /// It raises the member's alignment and never lowers it, which is what both compilers do.
    /// Lowering is what `packed` is for.
    pub align: Option<u64>,
    /// Whether the member carries `packed`, which drops its alignment to one byte.
    pub packed: bool,
}

impl FieldDecl {
    /// An ordinary member with no attributes.
    #[must_use]
    pub fn new(name: Option<rucc_base::Symbol>, ty: TypeId) -> FieldDecl {
        FieldDecl { name, ty, bits: None, align: None, packed: false }
    }

    /// A bit-field member of the given width.
    #[must_use]
    pub fn bit_field(name: Option<rucc_base::Symbol>, ty: TypeId, bits: u32) -> FieldDecl {
        FieldDecl { name, ty, bits: Some(bits), align: None, packed: false }
    }
}

/// One member of a record, placed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Field {
    /// The member name, absent for an unnamed bit-field or an anonymous member.
    pub name: Option<rucc_base::Symbol>,
    /// The member type.
    pub ty: TypeId,
    /// Where the member starts, in bits from the start of the record.
    ///
    /// Bits rather than bytes because a bit-field does not start on a byte boundary, and one
    /// unit for both kinds of member beats two that have to be kept in step.
    pub offset: u64,
    /// The bit-field width, absent when the member is an ordinary one.
    pub bits: Option<u32>,
}

impl Field {
    /// Where the member starts in bytes, rounded down.
    ///
    /// For an ordinary member this is exactly where it starts. For a bit-field it is the byte
    /// the first of its bits lives in, which is a starting point for a load rather than an
    /// address the program may take.
    #[must_use]
    pub fn byte_offset(&self) -> u64 {
        self.offset / 8
    }

    /// Whether the member is a bit-field, zero width included.
    #[must_use]
    pub fn is_bit_field(&self) -> bool {
        self.bits.is_some()
    }
}

/// What the program asked for on the record itself.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RecordOptions {
    /// `packed` on the record, which is the same as `packed` on each of its members.
    pub packed: bool,
    /// An alignment asked for with `_Alignas` or `aligned`, in bytes, which raises and never
    /// lowers.
    pub align: Option<u64>,
    /// The `#pragma pack` in effect, in bytes, which caps every member's alignment.
    pub pack: Option<u64>,
}

/// A record, laid out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordLayout {
    /// The size and alignment of the record.
    pub layout: Layout,
    /// The members, one per declaration and in the same order, zero width bit-fields included.
    pub fields: Vec<Field>,
}

/// Why a record has no layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordError {
    /// A member has no layout of its own. The index is into the declarations that were passed.
    Member {
        /// Which member.
        index: usize,
        /// What is wrong with it.
        error: LayoutError,
    },
    /// A bit-field asks for more bits than its type holds.
    BitFieldTooWide {
        /// Which member.
        index: usize,
        /// The width it asked for.
        width: u32,
        /// The width its type has.
        capacity: u32,
    },
    /// The record is larger than the address space, which enough members or one large enough
    /// array can arrange.
    TooLarge,
}

impl std::fmt::Display for RecordError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RecordError::Member { index, error } => write!(f, "member {index}: {error}"),
            RecordError::BitFieldTooWide { index, width, capacity } => {
                write!(
                    f,
                    "member {index}: a bit-field of width {width} does not fit in {capacity} bits"
                )
            }
            RecordError::TooLarge => f.write_str("the record is larger than the address space"),
        }
    }
}

impl std::error::Error for RecordError {}

/// Lays out a `struct` or a `union`.
///
/// The members are in the order the program wrote them, and the result has one [`Field`] per
/// declaration in that same order, so a caller may index the two together. That includes zero
/// width bit-fields, which occupy no bits and are there only so the indices line up.
///
/// A flexible array member, meaning an array with no size as the last member of a `struct`, is
/// laid out at the offset it would have had and contributes nothing to the size, which is what
/// makes `malloc(sizeof(struct S) + n)` the idiom it is. An array with no size anywhere else is
/// an incomplete member and reported as one; whether it was allowed to be there at all is a
/// question for whoever holds the span.
///
/// # Errors
///
/// [`RecordError`] when a member has no layout, when a bit-field is wider than its type, or
/// when the whole thing does not fit in the address space.
pub fn layout_record(
    types: &Types,
    kind: RecordKind,
    fields: &[FieldDecl],
    options: &RecordOptions,
    target: &TargetInfo,
) -> Result<RecordLayout, RecordError> {
    let mut builder = Builder::new(kind, *options, fields.len());
    for (index, decl) in fields.iter().enumerate() {
        let last = index + 1 == fields.len();
        builder.place(types, target, index, decl, last)?;
    }
    Ok(builder.finish())
}

/// The state of a record being laid out.
struct Builder {
    kind: RecordKind,
    options: RecordOptions,
    /// The next free bit in a `struct`, and always zero in a `union`.
    at: u64,
    /// How many bits the record occupies so far.
    bits: u64,
    /// The alignment in bytes, before the record's own attribute is applied.
    align: u64,
    fields: Vec<Field>,
}

impl Builder {
    fn new(kind: RecordKind, options: RecordOptions, members: usize) -> Builder {
        // One byte, not zero: a record with no members at all has an alignment of one, which is
        // what both compilers report for the GNU empty structure.
        Builder { kind, options, at: 0, bits: 0, align: 1, fields: Vec::with_capacity(members) }
    }

    /// Places one member.
    fn place(
        &mut self,
        types: &Types,
        target: &TargetInfo,
        index: usize,
        decl: &FieldDecl,
        last: bool,
    ) -> Result<(), RecordError> {
        let flexible = last && self.kind == RecordKind::Struct && flexible_array(types, decl.ty);
        let member = member_layout(types, decl.ty, flexible, target)
            .map_err(|error| RecordError::Member { index, error })?;
        let align = self.member_align(decl, member.align);
        match decl.bits {
            Some(0) => self.zero_width(decl, member.align),
            Some(width) => self.bit_field(index, decl, member, align, width)?,
            None => self.ordinary(decl, member, align)?,
        }
        Ok(())
    }

    /// The alignment a member is placed at, after the attributes have had their say.
    ///
    /// `packed` drops it to one byte and an explicit `aligned` or `_Alignas` raises what is
    /// left, which is why `__attribute__((packed, aligned(4)))` gives four rather than one.
    /// `#pragma pack` then caps the result, and that is where it differs from `packed`: a
    /// member written `aligned(8)` under `pack(2)` sits on a two byte boundary, because GCC
    /// caps a field's alignment after the declaration has been laid out and the request has
    /// already had its say. An `aligned` on the record itself is not capped, since it is not a
    /// field alignment, and that part is in [`Self::finish`].
    fn member_align(&self, decl: &FieldDecl, natural: u64) -> u64 {
        let mut align = natural;
        if self.options.packed || decl.packed {
            align = 1;
        }
        if let Some(asked) = decl.align {
            align = align.max(asked);
        }
        if let Some(pack) = self.options.pack {
            align = align.min(pack);
        }
        align.max(1)
    }

    /// Places an ordinary member.
    fn ordinary(
        &mut self,
        decl: &FieldDecl,
        member: Layout,
        align: u64,
    ) -> Result<(), RecordError> {
        let offset = match self.kind {
            RecordKind::Struct => round_up(self.at, align * 8)?,
            RecordKind::Union => 0,
        };
        self.fields.push(Field { name: decl.name, ty: decl.ty, offset, bits: None });
        let size = member.size.checked_mul(8).ok_or(RecordError::TooLarge)?;
        self.advance(offset, size);
        self.align = self.align.max(align);
        Ok(())
    }

    /// Places a bit-field of non-zero width.
    ///
    /// The rule both compilers implement is that a bit-field goes at the next free bit unless
    /// that would make it span more storage than its own type occupies, in which case it starts
    /// at the next boundary of its alignment. So `struct { char c; int b:30; }` puts `b` at bit
    /// 32 and is eight bytes, while `struct { char c; long long b:33; }` puts `b` at bit 8 and
    /// is eight bytes, because the second one still fits inside one unit of its type.
    ///
    /// Packing takes that rule out entirely, and packing means any of `packed` on the record,
    /// `packed` on the member and a `#pragma pack` of any number at all. The last of those is
    /// the surprise: `#pragma pack(4)` around `struct { char c; int b:30; }` lowers nothing,
    /// since four is what an `int` wanted anyway, and it still leaves `b` at bit 8 rather than
    /// moving it to bit 32. GCC reads the pragma as saying the program knows where it wants
    /// its fields, and the rule it takes out is the one that would move them. Measured, since
    /// the opposite reading is at least as plausible from the documents, and the same measure
    /// says `char y:6` after an `int x:12` sits at bit 12 under any packing and at bit 16
    /// without it.
    fn bit_field(
        &mut self,
        index: usize,
        decl: &FieldDecl,
        member: Layout,
        align: u64,
        width: u32,
    ) -> Result<(), RecordError> {
        let capacity = u32::try_from(member.size.saturating_mul(8)).unwrap_or(u32::MAX);
        if width > capacity {
            return Err(RecordError::BitFieldTooWide { index, width, capacity });
        }
        let offset = match self.kind {
            RecordKind::Union => 0,
            RecordKind::Struct if self.packing(decl) => self.at,
            RecordKind::Struct => {
                let boundary = align * 8;
                let used = self.at % boundary + u64::from(width);
                if used > u64::from(capacity) { round_up(self.at, boundary)? } else { self.at }
            }
        };
        self.fields.push(Field { name: decl.name, ty: decl.ty, offset, bits: Some(width) });
        self.advance(offset, u64::from(width));
        // An unnamed bit-field does not raise the record's alignment, which is why
        // `struct { char c; int :20; }` is four bytes aligned to one while the same structure
        // with the field named is four bytes aligned to four.
        if decl.name.is_some() {
            self.align = self.align.max(align);
        }
        Ok(())
    }

    /// Whether packing is in play for a member, which is what takes the straddle rule out.
    ///
    /// Not the same question as whether an alignment was lowered. A `#pragma pack` above what
    /// every member already asked for lowers nothing and still counts, because what GCC looks
    /// at is whether a maximum field alignment was set at all.
    fn packing(&self, decl: &FieldDecl) -> bool {
        self.options.packed || decl.packed || self.options.pack.is_some()
    }

    /// Handles a zero width bit-field, which places nothing and moves the next member on.
    ///
    /// It rounds to the alignment of its own type rather than to the packed alignment, so it
    /// keeps working inside a `packed` record or under `#pragma pack`, which is the whole
    /// reason a program writes one. It does not raise the record's alignment.
    fn zero_width(&mut self, decl: &FieldDecl, natural: u64) {
        if self.kind == RecordKind::Struct {
            self.at = self.at.next_multiple_of(natural.max(1) * 8);
        }
        self.fields.push(Field { name: decl.name, ty: decl.ty, offset: self.at, bits: Some(0) });
    }

    /// Records that a member ending at `offset + size` has been placed.
    fn advance(&mut self, offset: u64, size: u64) {
        let end = offset.saturating_add(size);
        if self.kind == RecordKind::Struct {
            self.at = end;
        }
        self.bits = self.bits.max(end);
    }

    /// The finished record.
    fn finish(self) -> RecordLayout {
        let align = match self.options.align {
            Some(asked) => self.align.max(asked),
            None => self.align,
        };
        let size = self.bits.div_ceil(8).next_multiple_of(align);
        RecordLayout { layout: Layout::new(size, align), fields: self.fields }
    }
}

/// Whether `ty` is an array with no size, which as the last member of a `struct` is a flexible
/// array member.
fn flexible_array(types: &Types, ty: TypeId) -> bool {
    matches!(types.kind(types.canonical(ty)), TypeKind::Array { len: ArrayLen::Unknown, .. })
}

/// The layout a member occupies, which for a flexible array member is none of it.
fn member_layout(
    types: &Types,
    ty: TypeId,
    flexible: bool,
    target: &TargetInfo,
) -> Result<Layout, LayoutError> {
    if !flexible {
        return layout(types, ty, target);
    }
    let TypeKind::Array { elem, .. } = types.kind(types.canonical(ty)) else {
        return Err(LayoutError::Incomplete);
    };
    // No size, but the element's alignment, which is why `struct { char c; long long f[]; }`
    // is eight bytes rather than one.
    let elem = layout(types, elem, target)?;
    Ok(Layout::new(0, elem.align))
}

/// `value` rounded up to a multiple of `to`, or [`RecordError::TooLarge`] if that overflows.
fn round_up(value: u64, to: u64) -> Result<u64, RecordError> {
    value.checked_next_multiple_of(to).ok_or(RecordError::TooLarge)
}
