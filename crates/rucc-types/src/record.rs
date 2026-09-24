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
//!
//! Two of them turned out to be a fact about the target rather than a fact about C, and they were
//! found the way the first fifty were: by compiling the same source for a target nobody had
//! compiled it for. Windows runs Microsoft's bit-field allocation rather than the Itanium one, on
//! mingw as well as on MSVC, and AAPCS64 lets an unnamed bit-field raise the record's alignment
//! where nothing else in the table does. Both are read out of [`TargetInfo`] here rather than
//! decided here, and `tests/abi-corpus` is where the numbers they change are written down.

use rucc_target::{BitFieldStyle, TargetInfo};
use rucc_tuple::Env;

use crate::kind::{ArrayLen, RecordKind, TypeKind};
use crate::layout::{Layout, LayoutError, align, layout};
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
    /// Where the member starts, in bytes from the start of the record, rounded down.
    ///
    /// For an ordinary member this is exactly where it starts. For a bit-field it is the byte
    /// the first of its bits lives in, which is a starting point for a load rather than an
    /// address the program may take.
    ///
    /// Bytes and not bits, with [`Field::bit`] holding the rest. A record may be `PTRDIFF_MAX`
    /// bytes and that many bits is more than a `u64` holds, so a single bit offset would make
    /// the largest record this compiler can lay out an eighth of the largest one C allows.
    pub offset: u64,
    /// Which bit of that byte the member starts at, from zero to seven.
    ///
    /// Always zero for an ordinary member, since every one of those starts on a byte boundary.
    pub bit: u32,
    /// What the member's offset is a multiple of, which is the alignment it was placed at.
    ///
    /// The same as the alignment of its type for most members, one byte under `packed`, and more
    /// than either where `aligned` asked for more. It is here because it is the only thing known
    /// about where a member sits once the offset stops being a number: an address is as aligned
    /// as the record is and as the offset into it is, and this is that second half.
    pub align: u64,
    /// The bit-field width, absent when the member is an ordinary one.
    pub bits: Option<u32>,
}

impl Field {
    /// Where the member starts, in bits from the start of the record.
    ///
    /// A `u128` for the reason [`Field::offset`] is bytes. Nothing that generates code wants
    /// this number, which is why it is not what is stored: it is here because the layout rules
    /// were measured in bits and the tests that check them read in bits.
    #[must_use]
    pub fn bit_offset(&self) -> u128 {
        u128::from(self.offset) * 8 + u128::from(self.bit)
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

/// How many bytes something is, where the answer is not a number here.
///
/// A member of a structure declared inside a function may be a variable length array, and then
/// the size of the structure and the offsets of the members after that one are worked out where
/// the declaration is reached rather than where it is written. This is the recipe for working one
/// out: a small tree over the sizes of the members, which whoever generates code walks once it
/// has a value for each of those.
///
/// A recipe rather than an expression because this crate has no expressions in it. What it names
/// is a member by index, and the one thing a reader has to be able to do with that is ask how
/// large the member's type is, which is a question it already answers for every other type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Extent {
    /// A number of bytes, known here.
    Bytes(u64),
    /// How large the type of the member at this index is.
    Member(u32),
    /// The sum of these, which is where a member sits once what is in front of it is counted.
    Sum(Vec<Extent>),
    /// The first rounded up to a multiple of the second, which is an alignment.
    RoundUp(Box<Extent>, u64),
    /// The largest of these, which is how long a `union` is.
    Max(Vec<Extent>),
}

impl Extent {
    /// The sum of the parts, folded where the parts are numbers.
    ///
    /// Folding here rather than in the reader because nearly every one of these has a constant in
    /// it and most of them are nothing else: a member two members past the variable one is at the
    /// same place as the member before it plus a number, and adding the two numbers here is what
    /// keeps the recipe the size of the thing that varies rather than the size of the record.
    #[must_use]
    pub fn sum(parts: Vec<Extent>) -> Extent {
        let mut bytes = 0u64;
        let mut rest = Vec::with_capacity(parts.len());
        for part in parts {
            match part {
                Extent::Bytes(count) => bytes = bytes.saturating_add(count),
                Extent::Sum(inner) => {
                    for part in inner {
                        match part {
                            Extent::Bytes(count) => bytes = bytes.saturating_add(count),
                            part => rest.push(part),
                        }
                    }
                }
                part => rest.push(part),
            }
        }
        if rest.is_empty() {
            return Extent::Bytes(bytes);
        }
        if bytes != 0 {
            rest.push(Extent::Bytes(bytes));
        }
        if rest.len() > 1 {
            return Extent::Sum(rest);
        }
        // The one part left, which is the whole answer: a list of one adds nothing to it.
        rest.pop().unwrap_or(Extent::Bytes(bytes))
    }

    /// The extent rounded up to a multiple of `to`, folded where it is a number.
    #[must_use]
    pub fn round_up(self, to: u64) -> Extent {
        if to <= 1 {
            return self;
        }
        match self {
            Extent::Bytes(count) => Extent::Bytes(count.next_multiple_of(to)),
            extent => Extent::RoundUp(Box::new(extent), to),
        }
    }

    /// The largest of the parts, folded where the parts are numbers.
    #[must_use]
    pub fn max(parts: Vec<Extent>) -> Extent {
        let mut bytes = 0u64;
        let mut rest = Vec::with_capacity(parts.len());
        for part in parts {
            match part {
                Extent::Bytes(count) => bytes = bytes.max(count),
                part => rest.push(part),
            }
        }
        if rest.is_empty() {
            return Extent::Bytes(bytes);
        }
        if bytes != 0 {
            rest.push(Extent::Bytes(bytes));
        }
        if rest.len() > 1 {
            return Extent::Max(rest);
        }
        // The one part left, which is the whole answer: a list of one adds nothing to it.
        rest.pop().unwrap_or(Extent::Bytes(bytes))
    }
}

/// What is left of a record's layout when it depends on something the program computes.
///
/// Present on exactly the records that have a variable length array somewhere in their members,
/// which C calls variably modified and which may only be declared inside a function. The
/// alignment is not in here because an alignment never varies: it is decided by the members
/// rather than by where they land, so it is in the [`Layout`] beside this with a size of zero.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariableLayout {
    /// How large the record is, padded to its alignment the way a fixed size one is.
    pub size: Extent,
    /// Where each member sits, in bytes, one entry per member and in the same order.
    ///
    /// Absent for a member that sits where the number in its [`Field`] says, which is every
    /// member in front of the first one of no fixed size and is the common case even here.
    pub offsets: Vec<Option<Extent>>,
}

/// A record, laid out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordLayout {
    /// The size and alignment of the record.
    ///
    /// For a record whose size is not known here the alignment is still the right one and the
    /// size is zero, with [`RecordLayout::variable`] holding the recipe that says how long it
    /// really is.
    pub layout: Layout,
    /// The members, one per declaration and in the same order, zero width bit-fields included.
    pub fields: Vec<Field>,
    /// What the record's size and its members' offsets are, where they are not numbers.
    pub variable: Option<VariableLayout>,
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
    /// The record is larger than an object may be, which enough members or one large enough
    /// array can arrange.
    TooLarge,
    /// A bit-field sits after a member whose length the program computes, which this compiler
    /// does not lay out yet. The index is into the declarations that were passed.
    VariableBitField {
        /// Which member.
        index: usize,
    },
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
            RecordError::TooLarge => f.write_str("the record is larger than an object may be"),
            RecordError::VariableBitField { index } => {
                write!(f, "member {index}: a bit-field after a member of no fixed size")
            }
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
/// A member whose length is a variable is not an error either. It is placed by the same rules as
/// any other member, over a position that is a recipe rather than a number once the first of them
/// has gone by, and the result carries a [`VariableLayout`] saying what that recipe is. Whether
/// the program was allowed to write one there is again a question for whoever holds the span,
/// since the answer depends on the scope the record was declared in.
///
/// # Errors
///
/// [`RecordError`] when a member has no layout, when a bit-field is wider than its type, or
/// when the whole thing is larger than an object may be.
pub fn layout_record(
    types: &Types,
    kind: RecordKind,
    fields: &[FieldDecl],
    options: &RecordOptions,
    target: &TargetInfo,
) -> Result<RecordLayout, RecordError> {
    let mut builder = Builder::new(kind, *options, fields.len(), target);
    for (index, decl) in fields.iter().enumerate() {
        let last = index + 1 == fields.len();
        builder.place(types, target, index, decl, last)?;
    }
    builder.finish()
}

/// The state of a record being laid out.
struct Builder {
    kind: RecordKind,
    options: RecordOptions,
    /// The next free bit in a `struct`, and always zero in a `union`.
    ///
    /// A `u128` because a record may be `PTRDIFF_MAX` bytes, and eight times that is more than
    /// a `u64` holds. Counting bits in a `u64` is what used to refuse a record an eighth of the
    /// way to the real limit, with the multiply by eight overflowing rather than any rule
    /// saying so.
    at: u128,
    /// How many bits the record occupies so far.
    bits: u128,
    /// The alignment in bytes, before the record's own attribute is applied.
    align: u64,
    /// The largest an object may be on the target, in bytes, checked once in [`Self::finish`].
    max: u64,
    /// How large the target says a record with no storage in it is, in bytes.
    empty: u64,
    /// The Microsoft bit-field allocation unit that is open, if one is.
    ///
    /// Always `None` under the Itanium rule, which has no such thing: there a bit-field is placed
    /// against the running bit position and the storage it lands in is whatever it lands in.
    unit: Option<Unit>,
    fields: Vec<Field>,
    /// Where the running position starts from, once a member of no fixed size has gone by.
    ///
    /// `None` until then, and from then on [`Builder::at`] and [`Builder::bits`] are counted from
    /// here rather than from the start of the record. Every member is a whole number of bytes
    /// long, so this is a byte position and the bits in `at` are bits within the byte it names.
    base: Option<Extent>,
    /// What the base is known to be a multiple of, which is what says whether a member after it
    /// is already where its alignment wants it.
    ///
    /// The size of a type is always a multiple of its alignment, so the end of a member of no
    /// fixed size is a multiple of the member's own alignment even though it is not a number.
    /// That is what makes `struct { int i[n]; int j; }` cost no arithmetic at all: four times
    /// however many is a multiple of four, so `j` is where the running position already is.
    base_align: u64,
    /// Where each member sits, where that is not the number in its [`Field`].
    offsets: Vec<Option<Extent>>,
    /// How long each member of no fixed size is, which is what makes a `union` as long as it is.
    ///
    /// Only a `union` fills this, because only there is the size of the whole a question about
    /// every member at once rather than about where the last one ended.
    widest: Vec<Extent>,
}

/// A run of Microsoft bit-fields sharing one piece of storage.
#[derive(Debug, Clone, Copy)]
struct Unit {
    /// Where the storage starts, in bits from the start of the record.
    start: u128,
    /// How large the storage is, in bytes, which is the declared type's size and not the number
    /// of bits anybody asked for.
    size: u64,
    /// How many of its bits are spoken for.
    used: u32,
}

impl Builder {
    fn new(
        kind: RecordKind,
        options: RecordOptions,
        members: usize,
        target: &TargetInfo,
    ) -> Builder {
        // One byte, not zero: a record with no members at all has an alignment of one, which is
        // what both compilers report for the GNU empty structure.
        Builder {
            kind,
            options,
            at: 0,
            bits: 0,
            align: 1,
            max: target.max_object_size(),
            empty: target.empty_record_size,
            unit: None,
            fields: Vec::with_capacity(members),
            base: None,
            base_align: 1,
            offsets: Vec::with_capacity(members),
            widest: Vec::new(),
        }
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
        let member = match member_layout(types, decl.ty, flexible, target) {
            Ok(member) => member,
            // A member whose length nobody knows yet, which is laid out by a path of its own
            // because the only thing it cannot do is contribute a number to the position.
            Err(LayoutError::Variable) => return self.variable(types, target, index, decl),
            Err(error) => return Err(RecordError::Member { index, error }),
        };
        let align = self.member_align(decl, member.align);
        // Once a member of no fixed size has gone by, the bit the record has got to is counted
        // from the end of that member rather than from its start. Under the Itanium rule that
        // settles every bit-field of non-zero width, because gcc only asks whether one straddles
        // its storage while the offset is a number, and after such a member it places them one
        // after another as if packed. What is left is a field that rounds to a unit, which is a
        // zero width one or any under Microsoft's rule, and where the end of the member is not
        // known to be as aligned as that unit, which unit the field lands in depends on the
        // lengths. Those are refused rather than placed right for some lengths and wrong for
        // the others.
        if let (Some(width), Some(_)) = (decl.bits, &self.base) {
            let unit = if width == 0 { member.align } else { align };
            let loose = width != 0 && target.bit_field_style == BitFieldStyle::Itanium;
            if !loose && unit > self.base_align {
                return Err(RecordError::VariableBitField { index });
            }
        }
        match decl.bits {
            Some(0) => self.zero_width(target, decl, member.align)?,
            Some(width) => self.bit_field(target, index, decl, member, align, width)?,
            None => self.ordinary(decl, member, align)?,
        }
        Ok(())
    }

    /// Records one member as placed at `at` bits from the start of the record.
    ///
    /// Splitting the bit offset into a byte and a bit is the only place a `u64` can be too
    /// narrow for it, and it can only happen for a record that [`Self::finish`] is going to
    /// refuse anyway, so saying so here is the same answer arriving earlier.
    fn push(
        &mut self,
        decl: &FieldDecl,
        at: u128,
        bits: Option<u32>,
        align: u64,
    ) -> Result<(), RecordError> {
        let offset = u64::try_from(at / 8).map_err(|_| RecordError::TooLarge)?;
        let bit = u32::try_from(at % 8).expect("a bit within a byte");
        self.fields.push(Field { name: decl.name, ty: decl.ty, offset, bit, bits, align });
        // Once a member of no fixed size has gone by, the number above is counted from the end of
        // that member rather than from the start of the record, so what really says where this
        // member sits is kept beside it.
        let recipe = self.base.as_ref().map(|_| self.position(offset));
        self.offsets.push(recipe);
        Ok(())
    }

    /// The position `bytes` past where the running position is counted from.
    fn position(&self, bytes: u64) -> Extent {
        match &self.base {
            Some(base) => Extent::sum(vec![base.clone(), Extent::Bytes(bytes)]),
            None => Extent::Bytes(bytes),
        }
    }

    /// Places a member whose length is not known until the program runs.
    ///
    /// The rules are the ones every other member is placed by. What is different is that the
    /// position stops being a number here: the member starts at a byte the alignment decides and
    /// ends somewhere only the program knows, so what comes after it is counted from that end.
    /// The running bit position is reset rather than carried, and it can be, because a member is
    /// a whole number of bytes long however long that is.
    fn variable(
        &mut self,
        types: &Types,
        target: &TargetInfo,
        index: usize,
        decl: &FieldDecl,
    ) -> Result<(), RecordError> {
        let natural =
            align(types, decl.ty, target).map_err(|error| RecordError::Member { index, error })?;
        let align = self.member_align(decl, natural);
        let member = u32::try_from(index).map_err(|_| RecordError::TooLarge)?;
        // A member of no fixed size is never a bit-field, so the run of them that Microsoft's
        // rule allocates ends here, and after that the position is a whole byte.
        self.close_unit();
        match self.kind {
            RecordKind::Struct => {
                let at = self.step_to(align)?;
                let bytes = u64::try_from(at / 8).map_err(|_| RecordError::TooLarge)?;
                let start = self.position(bytes);
                self.push(decl, at, None, align)?;
                self.base = Some(Extent::sum(vec![start, Extent::Member(member)]));
                // The member starts where its own alignment put it and is a whole number of its
                // natural alignment long, so the end of it is a multiple of the smaller of the
                // two. They differ when `packed` lowered where it starts or `aligned` raised it.
                self.base_align = align.min(natural).max(1);
                self.at = 0;
                self.bits = 0;
            }
            RecordKind::Union => {
                self.push(decl, 0, None, align)?;
                self.widest.push(Extent::Member(member));
            }
        }
        self.align = self.align.max(align);
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
        // An ordinary member ends a run of Microsoft bit-fields, and the storage the run reserved
        // is spent whether or not its bits were, so this is where the padding in front of the
        // `char` of `struct { unsigned m:3; char c; }` comes from on Windows. A no-op under the
        // Itanium rule, where there is never a unit open.
        self.close_unit();
        let offset = match self.kind {
            RecordKind::Struct => self.step_to(align)?,
            RecordKind::Union => 0,
        };
        self.push(decl, offset, None, align)?;
        self.advance(offset, u128::from(member.size) * 8);
        self.align = self.align.max(align);
        Ok(())
    }

    /// The bit a member of this alignment starts at, once the member before it is placed.
    ///
    /// The ordinary answer is the running position rounded up, and it stays the ordinary answer
    /// for as long as the running position is a number. After that it is not enough: the position
    /// is so many bytes past something only the program knows, and rounding that up leaves the
    /// address a multiple of the alignment past a base that is not one. So where the base is not
    /// already as aligned as the member wants, the rounding is written into the recipe instead and
    /// what follows is counted from there.
    fn step_to(&mut self, align: u64) -> Result<u128, RecordError> {
        let boundary = u128::from(align) * 8;
        let Some(base) = self.base.clone() else {
            return round_up(self.at, boundary);
        };
        if align <= self.base_align && self.at % boundary == 0 {
            return Ok(self.at);
        }
        // A bit-field may have left the position part way into a byte, and the member being
        // placed starts after all of it.
        let bytes = u64::try_from(self.at.div_ceil(8)).map_err(|_| RecordError::TooLarge)?;
        self.base = Some(Extent::sum(vec![base, Extent::Bytes(bytes)]).round_up(align));
        self.base_align = align;
        self.at = 0;
        self.bits = 0;
        Ok(0)
    }

    /// Places a bit-field of non-zero width.
    ///
    /// Which of the two rules runs is the target's answer rather than this function's, and the two
    /// are different algorithms and not one algorithm over different numbers. What they share is
    /// the width check in front of them and the alignment question behind them.
    fn bit_field(
        &mut self,
        target: &TargetInfo,
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
        let offset = match target.bit_field_style {
            BitFieldStyle::Itanium => self.itanium(decl, align, capacity, width)?,
            BitFieldStyle::Microsoft => self.microsoft(member, align, width)?,
        };
        self.push(decl, offset, Some(width), align)?;
        if self.contributes_alignment(target, decl.name.is_some()) {
            self.align = self.align.max(align);
        }
        Ok(())
    }

    /// Places a bit-field by the Itanium rule, and says where it went.
    ///
    /// A bit-field goes at the next free bit unless that would make it span more storage than its
    /// own type occupies, in which case it starts at the next boundary of its alignment. So
    /// `struct { char c; int b:30; }` puts `b` at bit 32 and is eight bytes, while
    /// `struct { char c; long long b:33; }` puts `b` at bit 8 and is eight bytes, because the
    /// second one still fits inside one unit of its type.
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
    fn itanium(
        &mut self,
        decl: &FieldDecl,
        align: u64,
        capacity: u32,
        width: u32,
    ) -> Result<u128, RecordError> {
        let offset = match self.kind {
            RecordKind::Union => 0,
            // Packed, or after a member of no fixed size, where gcc does not ask whether the
            // field straddles anything because the offset it would ask about is not a number.
            RecordKind::Struct if self.packing(decl) || self.base.is_some() => self.at,
            RecordKind::Struct => {
                let boundary = u128::from(align) * 8;
                let used = self.at % boundary + u128::from(width);
                if used > u128::from(capacity) { round_up(self.at, boundary)? } else { self.at }
            }
        };
        self.advance(offset, u128::from(width));
        Ok(offset)
    }

    /// Places a bit-field by Microsoft's rule, and says where it went.
    ///
    /// A run of bit-fields is allocated into a unit the size and the alignment of the declared
    /// type. The field joins the open unit when the unit came from a type of the same size and
    /// the bits are there for it, and otherwise the open unit is closed and a new one is opened at
    /// the next boundary of this member's alignment. Both halves of that condition matter and
    /// each is measurable on its own: `struct { unsigned m0:3; unsigned short m1:5; }` opens a
    /// second unit because the sizes differ although five bits were free, and
    /// `struct { unsigned m0:30; unsigned m1:4; }` opens one because four bits were not.
    ///
    /// Packing does not take this rule out the way it takes the Itanium one out. It lowers the
    /// alignment a unit is opened at and nothing else, so a packed `struct { unsigned m:3;
    /// char c; }` is five bytes on MSVC rather than the two it is on Linux: the unit still costs
    /// its whole four bytes and the `char` still starts after it.
    ///
    /// A `union` never has a unit open. Every member of one starts at offset zero, so there is no
    /// run for a field to join and nothing to leave open for the member after it.
    fn microsoft(&mut self, member: Layout, align: u64, width: u32) -> Result<u128, RecordError> {
        if self.kind == RecordKind::Union {
            self.bits = self.bits.max(u128::from(member.size) * 8);
            return Ok(0);
        }
        let joins = match self.unit {
            Some(unit) => {
                unit.size == member.size
                    && u128::from(unit.used) + u128::from(width) <= u128::from(unit.size) * 8
            }
            None => false,
        };
        if joins {
            let unit = self.unit.as_mut().expect("the unit the condition above looked at");
            let offset = unit.start + u128::from(unit.used);
            unit.used += width;
            self.at = offset + u128::from(width);
            return Ok(offset);
        }
        self.close_unit();
        let start = round_up(self.at, u128::from(align) * 8)?;
        self.unit = Some(Unit { start, size: member.size, used: width });
        self.at = start + u128::from(width);
        // The whole unit is spent here rather than when it is closed, so that a record whose last
        // member is a bit-field is as large as the unit and not as large as the bits used.
        self.bits = self.bits.max(start + u128::from(member.size) * 8);
        Ok(start)
    }

    /// Closes the open Microsoft allocation unit, if there is one.
    fn close_unit(&mut self) {
        if let Some(unit) = self.unit.take() {
            let end = unit.start.saturating_add(u128::from(unit.size) * 8);
            if self.kind == RecordKind::Struct {
                self.at = self.at.max(end);
            }
            self.bits = self.bits.max(end);
        }
    }

    /// Whether a bit-field raises the record's alignment to its own.
    ///
    /// A named one does almost everywhere, which is why `struct { char c; int b:20; }` is four
    /// bytes aligned to four. An unnamed one does not, which is why the same structure with the
    /// field unnamed is four bytes aligned to one, and AAPCS64 and Windows are the two places in
    /// this table that say otherwise.
    ///
    /// MSVC's `union` is the exception to all of it: there a bit-field gets storage and no say in
    /// the alignment at all, so `union { unsigned m:3; char c; }` is four bytes aligned to one,
    /// which is an alignment smaller than any member of it would have on its own. MinGW's gcc lays
    /// the same union out aligned to four even under `-mms-bitfields`, and clang does not follow it
    /// there, so this is one of the rows where the two references disagree. Section 6.9 settles it
    /// for the incumbent, which on `windows-gnu` is gcc.
    fn contributes_alignment(&self, target: &TargetInfo, named: bool) -> bool {
        match (self.kind, target.bit_field_style) {
            (RecordKind::Union, BitFieldStyle::Microsoft) if target.tuple.env() == Env::Msvc => {
                false
            }
            _ => named || target.unnamed_bit_field_aligns,
        }
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
    /// Under the Itanium rule it rounds to the alignment of its own type rather than to the packed
    /// alignment, so it keeps working inside a `packed` record or under `#pragma pack`, which is
    /// the whole reason a program writes one.
    ///
    /// Under Microsoft's rule it closes the open allocation unit and does nothing else, so with no
    /// unit open it does nothing at all. That is a visible difference rather than a restatement:
    /// `struct { char c; unsigned :0; }` is four bytes under the first rule and one byte under the
    /// second, because there the member before it was not a bit-field and there was no run to end.
    fn zero_width(
        &mut self,
        target: &TargetInfo,
        decl: &FieldDecl,
        natural: u64,
    ) -> Result<(), RecordError> {
        match target.bit_field_style {
            BitFieldStyle::Itanium => {
                if self.kind == RecordKind::Struct {
                    self.at = round_up(self.at, u128::from(natural.max(1)) * 8)?;
                    // The padding it opened counts toward the size and not only toward where the
                    // next member goes, so `struct { char c; int :0; }` is four bytes rather than
                    // one. With a member after it this is invisible, because that member's own
                    // end is further along, which is why it took a record ending in a zero width
                    // field to find.
                    self.bits = self.bits.max(self.at);
                }
                if self.contributes_alignment(target, false) {
                    self.align = self.align.max(natural.max(1));
                }
            }
            BitFieldStyle::Microsoft => self.close_unit(),
        }
        self.push(decl, self.at, Some(0), natural.max(1))
    }

    /// Records that a member ending at `offset + size` has been placed.
    fn advance(&mut self, offset: u128, size: u128) {
        let end = offset.saturating_add(size);
        if self.kind == RecordKind::Struct {
            self.at = end;
        }
        self.bits = self.bits.max(end);
    }

    /// The finished record.
    ///
    /// The one place the size limit is applied, which is why nothing above it checks for
    /// overflow: a member may be placed anywhere the arithmetic goes, and a record too large to
    /// be an object is refused here whether it got that way through one array or a thousand
    /// members.
    fn finish(self) -> Result<RecordLayout, RecordError> {
        let align = match self.options.align {
            Some(asked) => self.align.max(asked),
            None => self.align,
        };
        if self.base.is_some() || !self.widest.is_empty() {
            return self.finish_variable(align);
        }
        let size = u64::try_from(self.bits.div_ceil(8)).map_err(|_| RecordError::TooLarge)?;
        // A record with no storage in it is the one shape C has nothing to say about, because C
        // does not have it: it is a GNU extension, and the number is whatever the target's other
        // compiler chose. Zero everywhere but MSVC, and this covers the empty record, the one
        // holding nothing but a zero width bit-field, and the one holding nothing but a flexible
        // array member, all three of which the reference gives the same answer for.
        let size = if self.bits == 0 { self.empty } else { size };
        let size = size.checked_next_multiple_of(align).ok_or(RecordError::TooLarge)?;
        if size > self.max {
            return Err(RecordError::TooLarge);
        }
        Ok(RecordLayout { layout: Layout::new(size, align), fields: self.fields, variable: None })
    }

    /// The finished record, where how long it is depends on something the program computes.
    ///
    /// The one rule the fixed size path has that this one cannot is the limit on how large an
    /// object may be, since nothing here knows how large it turned out to be. It is not lost:
    /// every object of this type has its size worked out where its declaration is reached, and
    /// that is where a length nobody can have an object of is caught.
    ///
    /// The other one it does without is the size a target gives a record with nothing in it, which
    /// cannot arise, because a record with a member of no fixed size has a member.
    fn finish_variable(self, align: u64) -> Result<RecordLayout, RecordError> {
        let bytes = u64::try_from(self.bits.div_ceil(8)).map_err(|_| RecordError::TooLarge)?;
        let size = match self.kind {
            // Counted from the end of the last member of no fixed size, which is what the base
            // is, plus whatever the members after it came to.
            RecordKind::Struct => self.position(bytes),
            // Every member of a `union` starts at the same place, so how long it is is a question
            // about the longest of them and not about where anything ended.
            RecordKind::Union => {
                let mut parts = self.widest;
                parts.push(Extent::Bytes(bytes));
                Extent::max(parts)
            }
        };
        // Rounded up to the alignment, unless the members already left it there, which is the
        // same question a member of that alignment asks about where it goes.
        let rounded = if self.base_align >= align && bytes % align == 0 {
            size
        } else {
            size.round_up(align)
        };
        let variable = VariableLayout { size: rounded, offsets: self.offsets };
        Ok(RecordLayout {
            layout: Layout::new(0, align),
            fields: self.fields,
            variable: Some(variable),
        })
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
///
/// Nothing a program can write overflows a `u128` of bits, so this is the answer to a question
/// that cannot come up rather than a limit. It stays checked because the alternative is a wrap
/// that would place a member at a plausible looking offset.
fn round_up(value: u128, to: u128) -> Result<u128, RecordError> {
    value.checked_next_multiple_of(to).ok_or(RecordError::TooLarge)
}
