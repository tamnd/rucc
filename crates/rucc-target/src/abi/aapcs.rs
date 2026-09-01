//! AAPCS64, and Apple's variant of it, `spec/12-abi-and-runtime.md` section 12.3.
//!
//! Cleaner than SysV and with one idea SysV does not have. An aggregate of at most sixteen
//! bytes travels in general purpose registers, one per eightbyte, and anything larger travels
//! as the address of a copy the caller made. The exception is the homogeneous floating point
//! aggregate: up to four members, all of them the same floating point type and nothing else in
//! it, which travels in consecutive vector registers. `struct { float x, y, z; }` is three
//! vector registers, and adding one `int` to it makes it eight bytes in one general purpose
//! register instead.
//!
//! An aggregate that wants more registers than are left does not fall back to a smaller number
//! of them. It goes on the stack, and it takes the rest of that bank of registers with it, so
//! nothing after it can use them either. That is what makes the rule easy to get wrong: the
//! ninth argument of a call is not classified the way the first one is.
//!
//! Apple's divergences are in section 12.3 and none of them are visible here. Arguments packed
//! at their natural size on the stack, a variadic argument never in a register, and `long
//! double` being a `double` are all facts about where a value sits or what type it is rather
//! than about what form it travels in.

use super::{Arg, Call, Kind, Pass, Shape, Slot, integer_slots};

/// The most members a homogeneous floating point aggregate can have.
const HFA_LIMIT: usize = 4;

/// The vector registers of a homogeneous floating point aggregate, and [`None`] for anything
/// else.
///
/// Homogeneous means every member is the same floating point type once arrays and nested
/// records are flattened out, and that they fill the aggregate. The second half is what rules
/// out `struct { float a; char pad[8]; }` and anything a zero width bit-field has stretched.
fn hfa(shape: &Shape<'_>) -> Option<Vec<Slot>> {
    let first = shape.pieces.first()?;
    let Kind::Float(format) = first.scalar.kind else { return None };
    let count = shape.pieces.len();
    if count > HFA_LIMIT || shape.pieces.iter().any(|piece| piece.scalar != first.scalar) {
        return None;
    }
    (first.scalar.size * count as u64 == shape.size).then(|| vec![Slot::Float(format); count])
}

/// How the return value comes back.
///
/// Nothing here spends a register. The address of a return value that comes back in memory
/// goes in x8, which is not one of the eight argument registers, so unlike SysV a function
/// returning a large structure still has all eight for what it was called with.
pub(super) fn returns(_call: &mut Call, arg: &Arg<'_>) -> Pass {
    let shape = match arg {
        Arg::Void => return Pass::Ignore,
        Arg::Scalar(_) => return Pass::Direct,
        Arg::Aggregate(shape) => shape,
    };
    if shape.size == 0 {
        return Pass::Ignore;
    }
    if let Some(slots) = hfa(shape) {
        return Pass::Pieces(slots);
    }
    if shape.size <= 16 {
        return Pass::Pieces(integer_slots(shape.size));
    }
    Pass::Reference
}

/// How one argument travels.
pub(super) fn argument(call: &mut Call, arg: &Arg<'_>) -> Pass {
    let shape = match arg {
        Arg::Void => return Pass::Ignore,
        Arg::Scalar(scalar) => {
            match scalar.kind {
                Kind::Integer => call.gp = call.gp.saturating_sub(registers(scalar.size)),
                Kind::Float(_) => call.fp = call.fp.saturating_sub(1),
            }
            return Pass::Direct;
        }
        Arg::Aggregate(shape) => shape,
    };
    if shape.size == 0 {
        return Pass::Ignore;
    }
    if let Some(slots) = hfa(shape) {
        let count = registers(slots.len() as u64 * 8);
        if count > call.fp {
            // On the stack, and every vector register that is left goes with it.
            call.fp = 0;
            return Pass::Memory;
        }
        call.fp -= count;
        return Pass::Pieces(slots);
    }
    if shape.size > 16 {
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

/// How many registers this many bytes take, which is one per eight of them.
fn registers(size: u64) -> u32 {
    u32::try_from(size.div_ceil(8)).unwrap_or(1).max(1)
}

#[cfg(test)]
mod tests {
    use rucc_base::float::Format;

    use super::super::tests::{float, int, packed, record, target};
    use super::super::{Arg, Pass, Shape, Slot};
    use super::*;

    /// A call on AArch64 Linux with nothing spent yet.
    fn call() -> Call {
        target("aarch64-unknown-linux-gnu").call()
    }

    #[test]
    fn three_floats_are_three_vector_registers_and_one_int_ends_that() {
        let pieces = packed(&[float(Format::Single, 4); 3]);
        assert_eq!(
            call().argument(&Arg::Aggregate(record(&pieces))),
            Pass::Pieces(vec![Slot::Float(Format::Single); 3])
        );

        // `struct { float a, b, c; int d; }` is sixteen bytes of mixed members, which is two
        // general purpose registers and no longer homogeneous.
        let pieces = packed(&[
            float(Format::Single, 4),
            float(Format::Single, 4),
            float(Format::Single, 4),
            int(4),
        ]);
        assert_eq!(
            call().argument(&Arg::Aggregate(record(&pieces))),
            Pass::Pieces(vec![Slot::Integer(8), Slot::Integer(8)])
        );
    }

    #[test]
    fn five_floats_are_too_many_to_be_homogeneous() {
        let pieces = packed(&[float(Format::Single, 4); 5]);
        let shape = record(&pieces);
        assert_eq!(shape.size, 20);
        // Twenty bytes, which is over the sixteen an aggregate can travel in registers in.
        assert_eq!(call().argument(&Arg::Aggregate(shape)), Pass::Reference);
    }

    #[test]
    fn a_float_with_padding_after_it_is_not_homogeneous() {
        // `struct { float a; char pad[12]; }` has one floating point member and is not one of
        // these, because the members do not fill it.
        let mut pieces = vec![super::super::Piece { offset: 0, scalar: float(Format::Single, 4) }];
        for at in 4..16 {
            pieces.push(super::super::Piece { offset: at, scalar: int(1) });
        }
        let shape = Shape { size: 16, align: 4, pieces: &pieces, complex: false };
        assert_eq!(
            call().argument(&Arg::Aggregate(shape)),
            Pass::Pieces(vec![Slot::Integer(8), Slot::Integer(8)])
        );
    }

    #[test]
    fn over_sixteen_bytes_travels_as_the_address_of_a_copy() {
        let pieces = packed(&[int(8), int(8), int(8)]);
        let shape = record(&pieces);
        assert_eq!(call().argument(&Arg::Aggregate(shape)), Pass::Reference);
        assert_eq!(call().returns(&Arg::Aggregate(shape)), Pass::Reference);
    }

    #[test]
    fn a_large_return_value_costs_the_arguments_nothing() {
        let big = packed(&[int(8), int(8), int(8)]);
        let pair = packed(&[int(8), int(8)]);
        let mut call = call();
        assert_eq!(call.returns(&Arg::Aggregate(record(&big))), Pass::Reference);
        // Its address is in x8, so all eight argument registers are still here: six integers
        // and then a two register aggregate.
        for _ in 0..6 {
            assert_eq!(call.argument(&Arg::Scalar(int(8))), Pass::Direct);
        }
        assert_eq!(
            call.argument(&Arg::Aggregate(record(&pair))),
            Pass::Pieces(vec![Slot::Integer(8), Slot::Integer(8)])
        );
    }

    #[test]
    fn an_aggregate_that_did_not_fit_takes_the_rest_of_the_bank_with_it() {
        let pair = packed(&[int(8), int(8)]);
        let one = packed(&[int(8)]);
        let mut call = call();
        for _ in 0..7 {
            assert_eq!(call.argument(&Arg::Scalar(int(8))), Pass::Direct);
        }
        // One register is left and this wants two, so it goes on the stack.
        assert_eq!(call.argument(&Arg::Aggregate(record(&pair))), Pass::Memory);
        // The one that is left is not usable by anything after it either, which is the rule
        // that separates this from SysV.
        assert_eq!(call.argument(&Arg::Aggregate(record(&one))), Pass::Memory);
    }

    #[test]
    fn the_two_banks_are_counted_apart() {
        let quad = packed(&[float(Format::Double, 8); 4]);
        let mut call = call();
        for _ in 0..8 {
            assert_eq!(call.argument(&Arg::Scalar(int(8))), Pass::Direct);
        }
        // Every general purpose register is gone and all eight vector registers are still
        // here, which is two of these.
        for _ in 0..2 {
            assert_eq!(
                call.argument(&Arg::Aggregate(record(&quad))),
                Pass::Pieces(vec![Slot::Float(Format::Double); 4])
            );
        }
        assert_eq!(call.argument(&Arg::Aggregate(record(&quad))), Pass::Memory);
    }

    #[test]
    fn apple_classifies_an_aggregate_the_way_the_document_does() {
        let pieces = packed(&[float(Format::Double, 8), float(Format::Double, 8)]);
        let mut apple = target("aarch64-apple-darwin").call();
        assert_eq!(
            apple.argument(&Arg::Aggregate(record(&pieces))),
            Pass::Pieces(vec![Slot::Float(Format::Double); 2])
        );
    }
}
