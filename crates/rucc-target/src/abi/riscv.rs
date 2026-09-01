//! The RISC-V LP64D psABI, `spec/12-abi-and-runtime.md` section 12.5.
//!
//! Two eightbytes in a0 and a1 for an aggregate that fits, the address of a copy for one that
//! does not, and one rule with no analogue in the other ABIs: an aggregate of one or two
//! floating point members travels in floating point registers, and an aggregate of one floating
//! point member and one integer member travels in one of each. `struct { double re, im; }` is
//! fa0 and fa1, and `struct { double value; int tag; }` is fa0 and a0. Nothing else here looks
//! inside an aggregate at all.
//!
//! The rule stops where the registers do. A member wider than the floating point registers,
//! which is a `long double` on this ABI, is not a floating point member for this purpose and
//! the aggregate holding it is two integer registers or an address like anything else.

use super::{Arg, Call, Kind, Pass, Shape, Slot, integer_slots};

/// The width of a floating point register on LP64D, in bytes.
const FLEN: u64 = 8;

/// The width of a general purpose register, in bytes.
const XLEN: u64 = 8;

/// The registers a one or two member aggregate travels in under the floating point rule, and
/// [`None`] for one the rule does not reach.
fn flattened(shape: &Shape<'_>) -> Option<Vec<Slot>> {
    let slot = |piece: &super::Piece| match piece.scalar.kind {
        Kind::Float(format) if piece.scalar.size <= FLEN => {
            Some(Slot::Float { offset: piece.offset, format })
        }
        Kind::Integer if piece.scalar.size <= XLEN => Some(Slot::Integer {
            offset: piece.offset,
            size: u32::try_from(piece.scalar.size).expect("a size a register holds"),
        }),
        _ => None,
    };
    let float = |piece: &&super::Piece| matches!(piece.scalar.kind, Kind::Float(_));
    let floats = shape.pieces.iter().filter(float).count();
    match shape.pieces {
        // One floating point member, which is the same register the member itself would use.
        [only] if floats == 1 => Some(vec![slot(only)?]),
        // Two members with at least one floating point member between them. Two integers are
        // not this: they are the ordinary answer, and the ordinary answer for them is the same
        // two registers anyway.
        [first, second] if floats > 0 => Some(vec![slot(first)?, slot(second)?]),
        _ => None,
    }
}

/// How the return value comes back.
pub(super) fn returns(call: &mut Call, arg: &Arg<'_>) -> Pass {
    let shape = match arg {
        Arg::Void => return Pass::Ignore,
        Arg::Scalar(_) => return Pass::Direct,
        Arg::Aggregate(shape) => shape,
    };
    if shape.size == 0 {
        return Pass::Ignore;
    }
    if let Some(slots) = flattened(shape) {
        return Pass::Pieces(slots);
    }
    if shape.size <= 2 * XLEN {
        return Pass::Pieces(integer_slots(shape.size));
    }
    // The address is the first argument, in a0, the same way it is on SysV.
    call.gp = call.gp.saturating_sub(1);
    Pass::Reference
}

/// How one argument travels.
pub(super) fn argument(call: &mut Call, arg: &Arg<'_>) -> Pass {
    let shape = match arg {
        Arg::Void => return Pass::Ignore,
        Arg::Scalar(scalar) => {
            // A floating point value wider than a floating point register travels in general
            // purpose ones, which is what a `long double` does here.
            match scalar.kind {
                Kind::Float(_) if scalar.size <= FLEN => call.fp = call.fp.saturating_sub(1),
                _ => call.gp = call.gp.saturating_sub(registers(scalar.size)),
            }
            return Pass::Direct;
        }
        Arg::Aggregate(shape) => shape,
    };
    if shape.size == 0 {
        return Pass::Ignore;
    }
    if let Some(slots) = flattened(shape) {
        let floats = slots.iter().filter(|slot| matches!(slot, Slot::Float { .. })).count() as u32;
        let integers = slots.len() as u32 - floats;
        if floats <= call.fp && integers <= call.gp {
            call.fp -= floats;
            call.gp -= integers;
            return Pass::Pieces(slots);
        }
        // Not enough of one bank or the other, so the rule does not apply and the aggregate is
        // whatever the ordinary answer below makes it.
    }
    if shape.size > 2 * XLEN {
        call.gp = call.gp.saturating_sub(1);
        return Pass::Reference;
    }
    let count = registers(shape.size);
    if count > call.gp {
        call.gp = 0;
        return Pass::Memory;
    }
    call.gp -= count;
    Pass::Pieces(integer_slots(shape.size))
}

/// How many general purpose registers this many bytes take.
fn registers(size: u64) -> u32 {
    u32::try_from(size.div_ceil(XLEN)).unwrap_or(1).max(1)
}

#[cfg(test)]
mod tests {
    use rucc_base::float::Format;

    use super::super::tests::{float, fpr, gpr, int, packed, record, target};
    use super::super::{Arg, Pass};
    use super::*;

    /// A call on RISC-V Linux with nothing spent yet.
    fn call() -> Call {
        target("riscv64-unknown-linux-gnu").call()
    }

    #[test]
    fn two_floating_point_members_travel_in_two_floating_point_registers() {
        let pieces = packed(&[float(Format::Double, 8), float(Format::Double, 8)]);
        assert_eq!(
            call().argument(&Arg::Aggregate(record(&pieces))),
            Pass::Pieces(vec![fpr(0, Format::Double), fpr(8, Format::Double)])
        );
        // The same going out, which is fa0 and fa1.
        assert_eq!(
            call().returns(&Arg::Aggregate(record(&pieces))),
            Pass::Pieces(vec![fpr(0, Format::Double), fpr(8, Format::Double)])
        );
    }

    #[test]
    fn one_of_each_travels_in_one_of_each() {
        let pieces = packed(&[float(Format::Double, 8), int(4)]);
        assert_eq!(
            call().argument(&Arg::Aggregate(record(&pieces))),
            Pass::Pieces(vec![fpr(0, Format::Double), gpr(8, 4)])
        );
        // And the other way round, since the rule is about what the members are rather than
        // about which one is written first.
        let pieces = packed(&[int(4), float(Format::Double, 8)]);
        assert_eq!(
            call().argument(&Arg::Aggregate(record(&pieces))),
            // The integer is four bytes at zero and the `double` is eight bytes at eight,
            // which is the case a run of slots without offsets on it cannot describe.
            Pass::Pieces(vec![gpr(0, 4), fpr(8, Format::Double)])
        );
    }

    #[test]
    fn three_members_are_too_many_for_the_rule() {
        let pieces = packed(&[float(Format::Single, 4); 3]);
        let shape = record(&pieces);
        assert_eq!(shape.size, 12);
        assert_eq!(
            call().argument(&Arg::Aggregate(shape)),
            Pass::Pieces(vec![gpr(0, 8), gpr(8, 4)])
        );
    }

    #[test]
    fn over_two_registers_travels_as_an_address() {
        let pieces = packed(&[int(8), int(8), int(8)]);
        assert_eq!(call().argument(&Arg::Aggregate(record(&pieces))), Pass::Reference);
        assert_eq!(call().returns(&Arg::Aggregate(record(&pieces))), Pass::Reference);
    }

    #[test]
    fn the_rule_stops_where_the_registers_do() {
        let pieces = packed(&[float(Format::Double, 8), float(Format::Double, 8)]);
        let mut call = call();
        for _ in 0..8 {
            assert_eq!(call.argument(&Arg::Scalar(float(Format::Double, 8))), Pass::Direct);
        }
        // Every floating point register is gone, so the two members are sixteen bytes in two
        // general purpose registers instead.
        assert_eq!(
            call.argument(&Arg::Aggregate(record(&pieces))),
            Pass::Pieces(vec![gpr(0, 8), gpr(8, 8)])
        );
    }

    #[test]
    fn a_long_double_member_is_not_a_floating_point_member_for_this() {
        // Sixteen bytes of binary128, which no floating point register on LP64D holds.
        let pieces = packed(&[float(Format::Quad, 16)]);
        assert_eq!(
            call().argument(&Arg::Aggregate(record(&pieces))),
            Pass::Pieces(vec![gpr(0, 8), gpr(8, 8)])
        );
    }
}
