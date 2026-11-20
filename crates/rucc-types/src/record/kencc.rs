//! The record layout of the Plan 9 compilers, which every Plan 9 target uses.
//!
//! Design: tamnd/rucc#4154, and 08.1 of the rucc-plan9 plan.
//!
//! An object from rucc must link with objects from `6c` and `7c`, so a record must have the
//! same size and the same offsets as kencc gives it. kencc lays a record out in `sualign` in
//! `cc/dcl.c`, with the widths of `align` in `6c/swt.c` and `7c/swt.c`. The two files agree:
//!
//! - A member that is not a bit-field goes at the next multiple of its width in `ewidth`. That
//!   is the alignment of a scalar, but a `struct` or a `union` has no entry in `ewidth`, and so
//!   it and an array of it go at the next multiple of 8 (`Ael1`).
//! - The size of every `struct` and `union` rounds up to 8 (`Asu2`), so `struct { char c; }`
//!   is 8 bytes.
//! - `#pragma pack on` sets both of those widths to 1, and `pack N` sets them to N. It does not
//!   only cap them: under `pack 2` a `char` goes at a multiple of 2.
//! - A bit-field is in a unit of `tfield`, which is a 4 byte `int` on `6c` and a 4 byte `long`
//!   on `7c`, whatever type the program wrote. Its bits go from the low bit up. The parser
//!   counts the bits in `lastbit`, and a field that does not fit in what is left of the unit
//!   starts a new one at the next multiple of 4 bytes. A member that is not a bit-field and a
//!   bit-field of width zero end the unit. An unnamed bit-field takes bits and gets no member,
//!   so an unnamed field opens no unit.
//! - In a `union` each member is at 0, and a bit-field is at bit 0.
//!
//! The parser state is copied with its quirks. `firstbit` is set when a unit must start, and it
//! is cleared only by a member with a name, so an anonymous `struct` member between two
//! bit-fields does not end their unit. That is what kencc does, and the 9front tree has no such
//! record.

use super::{Builder, FieldDecl, RecordError, round_up};
use crate::kind::{RecordKind, TypeKind};
use crate::layout::Layout;
use crate::types::{TypeId, Types};

/// The bits in the unit of a bit-field, `tfield->width * 8`.
const UNIT: u32 = 32;

/// The state of the parser of kencc that decides where a bit-field goes.
#[derive(Debug, Clone, Copy)]
pub(super) struct Bits {
    /// `lastbit`: the bits of the unit used up to the end of the last bit-field.
    last: u32,
    /// `firstbit`: the next bit-field starts a unit.
    first: bool,
    /// Where the open unit starts, in bits from the start of the record.
    unit: u128,
    /// The width that a record, and a member that is a record, rounds to. That is `SZ_VLONG` on
    /// `6c` and `7c`, and the target gives it as [`rucc_target::TargetInfo::record_align`].
    word: u64,
}

impl Bits {
    /// The state at the opening brace of a record body.
    pub(super) const fn new(word: u64) -> Bits {
        Bits { last: 0, first: true, unit: 0, word }
    }
}

/// Whether `ty`, or the element of `ty` when it is an array, is a `struct` or a `union`.
fn record_like(types: &Types, ty: TypeId) -> bool {
    let mut ty = types.canonical(ty);
    loop {
        match types.kind(ty) {
            TypeKind::Array { elem, .. } => ty = types.canonical(elem),
            TypeKind::Record(_) => return true,
            _ => return false,
        }
    }
}

/// The largest power of two that divides `bytes`, and at most `most`. A bit-field of kencc can
/// be at any byte of its unit, so its storage has that alignment and not the one of its type.
fn alignment_at(bytes: u128, most: u64) -> u64 {
    let low = if bytes == 0 { most } else { 1u64 << bytes.trailing_zeros().min(63) };
    low.min(most).max(1)
}

impl Builder {
    /// The width of `Ael1` for a member of type `ty`, which is `natural` for a scalar.
    fn kencc_align(&self, types: &Types, ty: TypeId, natural: u64) -> u64 {
        match self.options.pack {
            Some(pack) => pack.max(1),
            None if record_like(types, ty) => self.kencc.map_or(1, |bits| bits.word),
            None => natural.max(1),
        }
    }

    /// Places one member as `sualign` places it. `member` is its layout, and `bits` the state of
    /// the parser.
    pub(super) fn kencc(
        &mut self,
        types: &Types,
        index: usize,
        decl: &FieldDecl,
        member: Layout,
        mut bits: Bits,
    ) -> Result<Bits, RecordError> {
        let align = self.kencc_align(types, decl.ty, member.align);
        match decl.bits {
            None => {
                // A member with a name ends the unit (`edecor: xdecor` in `cc.y`, and then
                // `firstbit = 0` in `dodecl`). An anonymous member changes nothing.
                if decl.name.is_some() {
                    bits = Bits { last: 0, first: false, ..bits };
                }
                let offset = match self.kind {
                    RecordKind::Struct => round_up(self.at, u128::from(align) * 8)?,
                    RecordKind::Union => 0,
                };
                self.push(decl, offset, None, align)?;
                self.advance(offset, u128::from(member.size) * 8);
            }
            Some(0) => {
                bits = Bits { last: 0, first: true, ..bits };
                self.push(decl, self.at, Some(0), 1)?;
            }
            Some(width) => {
                // kencc refuses a field wider than its unit. A field wider than the type that it
                // was written with is refused too, because rucc reads the field through that
                // type. The 9front tree has neither.
                let capacity = u32::try_from(member.size.saturating_mul(8)).unwrap_or(u32::MAX);
                let capacity = capacity.min(UNIT);
                if width > capacity {
                    return Err(RecordError::BitFieldTooWide { index, width, capacity });
                }
                bits.last += width;
                if bits.last > UNIT {
                    bits.last = width;
                    bits.first = true;
                }
                let shift = bits.last - width;
                if decl.name.is_none() {
                    // Bits and no member, so nothing is stored and no unit opens.
                    self.push(decl, self.at, Some(width), 1)?;
                    return Ok(bits);
                }
                let at = match self.kind {
                    RecordKind::Union => {
                        // The field has the type of `tfield` by now, so it takes a whole unit.
                        self.bits = self.bits.max(u128::from(UNIT));
                        0
                    }
                    RecordKind::Struct => {
                        if bits.first || shift == 0 {
                            bits.unit = round_up(self.at, u128::from(UNIT))?;
                            self.at = bits.unit + u128::from(UNIT);
                            self.bits = self.bits.max(self.at);
                        }
                        bits.unit + u128::from(shift)
                    }
                };
                bits.first = false;
                self.push(decl, at, Some(width), alignment_at(at / 8, member.align))?;
            }
        }
        self.align = self.align.max(align);
        Ok(bits)
    }

    /// The width that the whole record rounds to (`Asu2`), which is also its alignment.
    pub(super) fn kencc_round(&self) -> u64 {
        self.options.pack.map_or(self.kencc.map_or(1, |bits| bits.word), |pack| pack.max(1))
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_target::{TargetInfo, Triple};

    use super::alignment_at;
    use crate::kind::{ArrayLen, IntKind, RecordKind};
    use crate::record::{FieldDecl, RecordError, RecordLayout, RecordOptions, layout_record};
    use crate::types::{TypeId, Types};

    fn plan9() -> TargetInfo {
        TargetInfo::new("x86_64-plan9".parse::<Triple>().expect("a triple the compiler supports"))
    }

    fn lay_out(types: &Types, kind: RecordKind, fields: &[FieldDecl]) -> RecordLayout {
        lay_out_with(types, kind, fields, &RecordOptions::default())
    }

    fn lay_out_with(
        types: &Types,
        kind: RecordKind,
        fields: &[FieldDecl],
        options: &RecordOptions,
    ) -> RecordLayout {
        layout_record(types, kind, fields, options, &plan9()).expect("a record kencc takes")
    }

    /// The offsets of the members in bits.
    fn offsets(laid_out: &RecordLayout) -> Vec<u128> {
        laid_out.fields.iter().map(super::super::Field::bit_offset).collect()
    }

    /// A complete record of the members.
    fn record(types: &mut Types, kind: RecordKind, fields: &[FieldDecl]) -> TypeId {
        let id = types.declare_record(kind, None);
        let laid_out = lay_out(types, kind, fields);
        types.complete_record(id, laid_out);
        types.record(id)
    }

    struct Names(Interner);

    impl Names {
        fn field(&mut self, name: &str, ty: TypeId) -> FieldDecl {
            FieldDecl::new(Some(self.0.intern(name)), ty)
        }

        fn bits(&mut self, name: &str, ty: TypeId, width: u32) -> FieldDecl {
            FieldDecl::bit_field(Some(self.0.intern(name)), ty, width)
        }
    }

    fn setup() -> (Names, Types) {
        (Names(Interner::new()), Types::new())
    }

    #[test]
    fn every_record_rounds_to_eight() {
        let (mut n, types) = setup();
        let char_type = types.int(IntKind::Char);
        let short = types.int(IntKind::Short);
        let int = types.int(IntKind::Int);
        let one = lay_out(&types, RecordKind::Struct, &[n.field("c", char_type)]);
        assert_eq!((one.layout.size, one.layout.align), (8, 8));
        let fields = [
            n.field("c", char_type),
            n.field("s", short),
            n.field("i", int),
            n.field("d", char_type),
        ];
        let mixed = lay_out(&types, RecordKind::Struct, &fields);
        assert_eq!(offsets(&mixed), [0, 16, 32, 64]);
        assert_eq!(mixed.layout.size, 16);
        let union = lay_out(&types, RecordKind::Union, &[n.field("s", short)]);
        assert_eq!(union.layout.size, 8);
    }

    #[test]
    fn a_member_that_is_a_record_or_an_array_of_one_goes_at_a_multiple_of_eight() {
        let (mut n, mut types) = setup();
        let char_type = types.int(IntKind::Char);
        let inner = record(&mut types, RecordKind::Struct, &[n.field("d", char_type)]);
        let fields = [n.field("c", char_type), n.field("t", inner)];
        let outer = lay_out(&types, RecordKind::Struct, &fields);
        assert_eq!(offsets(&outer), [0, 64]);
        assert_eq!(outer.layout.size, 16);
        let pair = types.array(inner, ArrayLen::Fixed(2));
        let fields = [n.field("c", char_type), n.field("a", pair)];
        let outer = lay_out(&types, RecordKind::Struct, &fields);
        assert_eq!(offsets(&outer), [0, 64]);
        assert_eq!(outer.layout.size, 24);
        // An array of a scalar keeps the alignment of the scalar.
        let chars = types.array(char_type, ArrayLen::Fixed(3));
        let fields = [n.field("c", char_type), n.field("a", chars)];
        assert_eq!(offsets(&lay_out(&types, RecordKind::Struct, &fields)), [0, 8]);
    }

    #[test]
    fn bit_fields_share_a_unit_of_four_bytes_until_it_is_full() {
        let (mut n, types) = setup();
        let int = types.int(IntKind::Int);
        let char_type = types.int(IntKind::Char);
        let fields = [
            n.bits("a", int, 3),
            n.bits("b", int, 5),
            n.bits("c", int, 30),
            n.field("d", char_type),
            n.bits("e", int, 4),
        ];
        let laid_out = lay_out(&types, RecordKind::Struct, &fields);
        assert_eq!(offsets(&laid_out), [0, 3, 32, 64, 96]);
        assert_eq!(laid_out.layout.size, 16);
    }

    #[test]
    fn a_bit_field_of_char_still_takes_a_unit_of_four_bytes() {
        let (mut n, types) = setup();
        let char_type = types.int(IntKind::Char);
        let fields = [n.field("c", char_type), n.bits("b", char_type, 4)];
        let laid_out = lay_out(&types, RecordKind::Struct, &fields);
        assert_eq!(offsets(&laid_out), [0, 32]);
        assert_eq!(laid_out.layout.size, 8);
    }

    #[test]
    fn an_unnamed_bit_field_takes_bits_and_opens_no_unit() {
        let (mut n, types) = setup();
        let int = types.int(IntKind::Int);
        let fields = [n.bits("a", int, 3), FieldDecl::bit_field(None, int, 5), n.bits("b", int, 4)];
        let laid_out = lay_out(&types, RecordKind::Struct, &fields);
        assert_eq!(laid_out.fields[2].bit_offset(), 8);
        assert_eq!(laid_out.layout.size, 8);
        // At the start of a record the unnamed field leaves the unit to the next named one, and
        // that one goes after the skipped bits.
        let fields = [FieldDecl::bit_field(None, int, 5), n.bits("a", int, 3)];
        let laid_out = lay_out(&types, RecordKind::Struct, &fields);
        assert_eq!(laid_out.fields[1].bit_offset(), 5);
    }

    #[test]
    fn a_zero_width_bit_field_ends_the_unit() {
        let (mut n, types) = setup();
        let int = types.int(IntKind::Int);
        let fields = [n.bits("a", int, 3), FieldDecl::bit_field(None, int, 0), n.bits("b", int, 3)];
        let laid_out = lay_out(&types, RecordKind::Struct, &fields);
        assert_eq!(laid_out.fields[2].bit_offset(), 32);
        assert_eq!(laid_out.layout.size, 8);
    }

    #[test]
    fn every_member_of_a_union_is_at_zero() {
        let (mut n, mut types) = setup();
        let char_type = types.int(IntKind::Char);
        let int = types.int(IntKind::Int);
        let inner = record(&mut types, RecordKind::Struct, &[n.field("d", char_type)]);
        let fields = [
            n.field("c", char_type),
            n.bits("a", int, 3),
            n.bits("b", int, 7),
            n.field("s", inner),
        ];
        let laid_out = lay_out(&types, RecordKind::Union, &fields);
        assert_eq!(offsets(&laid_out), [0, 0, 0, 0]);
        assert_eq!(laid_out.layout.size, 8);
    }

    #[test]
    fn pack_sets_the_alignment_of_every_member_and_of_the_record() {
        let (mut n, types) = setup();
        let char_type = types.int(IntKind::Char);
        let int = types.int(IntKind::Int);
        let fields = [n.field("c", char_type), n.field("i", int), n.field("d", char_type)];
        let one = RecordOptions { pack: Some(1), ..RecordOptions::default() };
        let laid_out = lay_out_with(&types, RecordKind::Struct, &fields, &one);
        assert_eq!(offsets(&laid_out), [0, 8, 40]);
        assert_eq!((laid_out.layout.size, laid_out.layout.align), (6, 1));
        // Under `pack 2` a `char` goes at a multiple of two too.
        let two = RecordOptions { pack: Some(2), ..RecordOptions::default() };
        let laid_out = lay_out_with(&types, RecordKind::Struct, &fields, &two);
        assert_eq!(offsets(&laid_out), [0, 16, 48]);
        assert_eq!((laid_out.layout.size, laid_out.layout.align), (8, 2));
    }

    #[test]
    fn a_bit_field_wider_than_its_unit_or_its_type_is_refused() {
        let (mut n, types) = setup();
        let char_type = types.int(IntKind::Char);
        let long_long = types.int(IntKind::LongLong);
        for (ty, width, capacity) in [(long_long, 33, 32), (char_type, 9, 8)] {
            let fields = [n.bits("x", ty, width)];
            let error = layout_record(
                &types,
                RecordKind::Struct,
                &fields,
                &RecordOptions::default(),
                &plan9(),
            );
            assert_eq!(
                error.err(),
                Some(RecordError::BitFieldTooWide { index: 0, width, capacity })
            );
        }
    }

    #[test]
    fn the_flag_gives_another_target_the_layout_of_plan9() {
        let (mut n, mut types) = setup();
        let char_type = types.int(IntKind::Char);
        let short = types.int(IntKind::Short);
        let int = types.int(IntKind::Int);
        let inner = record(&mut types, RecordKind::Struct, &[n.field("d", char_type)]);
        let fields = [
            n.field("c", char_type),
            n.bits("a", int, 3),
            n.bits("b", short, 4),
            n.field("t", inner),
            n.field("s", short),
        ];
        let linux = TargetInfo::new(
            "x86_64-linux-gnu".parse::<Triple>().expect("a triple the compiler supports"),
        );
        let kencc = RecordOptions { kencc: true, ..RecordOptions::default() };
        let plan9_layout = lay_out(&types, RecordKind::Struct, &fields);
        let flagged = layout_record(&types, RecordKind::Struct, &fields, &kencc, &linux)
            .expect("a record kencc takes");
        assert_eq!(offsets(&flagged), offsets(&plan9_layout));
        assert_eq!(flagged.layout, plan9_layout.layout);
        assert_eq!(offsets(&flagged), [0, 32, 35, 64, 128]);
        assert_eq!(flagged.layout.size, 24);
        let host = layout_record(
            &types,
            RecordKind::Struct,
            &[n.field("c", char_type)],
            &RecordOptions::default(),
            &linux,
        )
        .expect("a record the host takes");
        assert_eq!(host.layout.size, 1, "without the flag the host keeps its own layout");
    }

    #[test]
    fn a_field_in_its_unit_has_the_alignment_of_its_byte() {
        assert_eq!(alignment_at(0, 4), 4);
        assert_eq!(alignment_at(8, 4), 4);
        assert_eq!(alignment_at(9, 4), 1);
        assert_eq!(alignment_at(10, 4), 2);
    }
}
