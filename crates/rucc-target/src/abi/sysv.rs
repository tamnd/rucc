//! SysV AMD64, `spec/12-abi-and-runtime.md` section 12.2.
//!
//! The classification algorithm is the intricate one. An aggregate is cut into eightbytes, each
//! eightbyte is given a class by merging the classes of the scalars that reach into it, and any
//! eightbyte that comes out MEMORY sends the whole argument to memory. The cases that catch
//! people are all here: an eightbyte holding an `int` and a `float` together is INTEGER, so the
//! float travels in a general purpose register; a member that is not aligned, which takes
//! `packed`, puts the whole thing in memory; and anything over sixteen bytes is in memory
//! whatever its members are.
//!
//! x87 is the other half of it. A `long double` is eighty bits of value in sixteen bytes and
//! there is no register file it can be passed in, so an aggregate holding one goes in memory as
//! an argument. As a return value it comes back on the x87 stack, and so does `_Complex long
//! double`, in st(0) and st(1). That last one is why [`Shape::complex`] exists: `struct { long
//! double a, b; }` is the same thirty two bytes in the same places and comes back in memory.

use rucc_base::float::Format;

use super::{Arg, Call, Kind, Pass, Piece, Shape, Slot};

/// The class of one eightbyte, section 3.2.3 of the psABI.
///
/// SSEUP and X87UP are not here. Both mean "the continuation of the eightbyte before this one",
/// and the only things that produce them are a vector wider than eight bytes, which is not an
/// aggregate and does not come through here, and a `long double`, whose two eightbytes are
/// treated as the one thing they are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    /// Nothing reaches into it, which takes padding or an empty member.
    None,
    /// A general purpose register.
    Integer,
    /// A vector register.
    Sse,
    /// The x87 stack.
    X87,
    /// Memory, which takes the whole argument with it.
    Memory,
}

/// Two classes over one eightbyte, section 3.2.3's merge rule.
fn merge(left: Class, right: Class) -> Class {
    match (left, right) {
        (a, b) if a == b => a,
        (Class::None, other) | (other, Class::None) => other,
        (Class::Memory, _) | (_, Class::Memory) => Class::Memory,
        // An x87 value shares an eightbyte with something else only in a packed record, and
        // there is no way to pass the two together.
        (Class::X87, _) | (_, Class::X87) => Class::Memory,
        // The rule that surprises people: one `int` in an eightbyte sends the `float` beside it
        // into a general purpose register.
        (Class::Integer, _) | (_, Class::Integer) => Class::Integer,
        _ => Class::Sse,
    }
}

/// The class of every eightbyte of an aggregate, and [`None`] where the answer is memory.
fn classes(shape: &Shape<'_>) -> Option<Vec<Class>> {
    // Eight eightbytes is the most anything can be classified into, and sixteen bytes is the
    // most anything that is not a vector can come back from it in registers. The second is a
    // consequence of the first: an aggregate over two eightbytes travels in registers only when
    // all of them but the first are SSEUP, which takes a vector.
    if shape.size > 16 {
        return None;
    }
    let mut classes = vec![Class::None; usize::try_from(shape.size.div_ceil(8)).ok()?];
    for piece in shape.pieces {
        // A member away from its natural alignment is what `packed` makes, and it is the second
        // of the two things section 3.2.3 sends straight to memory.
        if piece.scalar.align > 1 && piece.offset % piece.scalar.align != 0 {
            return None;
        }
        let class = match piece.scalar.kind {
            Kind::Integer => Class::Integer,
            Kind::Float(Format::X87Extended) => Class::X87,
            Kind::Float(_) => Class::Sse,
        };
        for at in piece.offset / 8..=(piece.end() - 1) / 8 {
            let slot = classes.get_mut(usize::try_from(at).ok()?)?;
            *slot = merge(*slot, class);
        }
    }
    classes.iter().all(|class| *class != Class::Memory).then_some(classes)
}

/// The registers the classified eightbytes take, as integers and then as vectors.
fn cost(classes: &[Class]) -> (u32, u32) {
    let count = |want: Class| classes.iter().filter(|class| **class == want).count() as u32;
    // An eightbyte nothing reaches into still travels, and it travels in a general purpose
    // register, because leaving a hole in the middle of an argument is not a thing an ABI does.
    (count(Class::Integer) + count(Class::None), count(Class::Sse))
}

/// What each eightbyte is read as.
fn slots(shape: &Shape<'_>, classes: &[Class]) -> Vec<Slot> {
    classes
        .iter()
        .enumerate()
        .map(|(index, class)| {
            let offset = index as u64 * 8;
            let bytes = (shape.size - offset).min(8);
            match class {
                // Four bytes or fewer of floating point is one `float`. More is a `double` or
                // two `float`s, which arrive in the same register either way.
                Class::Sse if bytes <= 4 => Slot::Float { offset, format: Format::Single },
                Class::Sse => Slot::Float { offset, format: Format::Double },
                _ => Slot::Integer { offset, size: u32::try_from(bytes).unwrap_or(8) },
            }
        })
        .collect()
}

/// Whether every scalar in the shape is an x87 `long double`.
fn all_x87(shape: &Shape<'_>) -> bool {
    let x87 = |piece: &Piece| piece.scalar.kind == Kind::Float(Format::X87Extended);
    !shape.pieces.is_empty() && shape.pieces.iter().all(x87)
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
    // The x87 stack, which is st(0) for a `long double` and st(0) with st(1) for a complex one.
    // A record holding two of them is not this and is the ordinary answer below, which is
    // memory, and telling the two apart is the only thing the complex flag is for.
    if all_x87(shape) && (shape.pieces.len() == 1 || (shape.pieces.len() == 2 && shape.complex)) {
        let stack = shape
            .pieces
            .iter()
            .map(|piece| Slot::Float { offset: piece.offset, format: Format::X87Extended });
        return Pass::Pieces(stack.collect());
    }
    let Some(classes) = classes(shape) else { return sret(call) };
    if classes.contains(&Class::X87) {
        return sret(call);
    }
    Pass::Pieces(slots(shape, &classes))
}

/// A return value that comes back in memory, which spends the register its address arrives in.
fn sret(call: &mut Call) -> Pass {
    call.gp = call.gp.saturating_sub(1);
    Pass::Reference
}

/// How one argument travels.
pub(super) fn argument(call: &mut Call, arg: &Arg<'_>) -> Pass {
    let shape = match arg {
        Arg::Void => return Pass::Ignore,
        Arg::Scalar(scalar) => {
            match scalar.kind {
                // An `__int128` is two consecutive general purpose registers.
                Kind::Integer => call.gp = call.gp.saturating_sub(registers(scalar.size)),
                // A `long double` argument is on the stack and spends nothing.
                Kind::Float(Format::X87Extended) => {}
                Kind::Float(_) => call.fp = call.fp.saturating_sub(1),
            }
            return Pass::Direct;
        }
        Arg::Aggregate(shape) => shape,
    };
    if shape.size == 0 {
        return Pass::Ignore;
    }
    let Some(classes) = classes(shape) else { return Pass::Memory };
    if classes.contains(&Class::X87) {
        return Pass::Memory;
    }
    let (gp, fp) = cost(&classes);
    // An aggregate takes all the registers it needs or none of them. The ones that are left are
    // still there for the arguments after it, which is what makes the eighth argument of a call
    // depend on the first seven.
    if gp > call.gp || fp > call.fp {
        return Pass::Memory;
    }
    call.gp -= gp;
    call.fp -= fp;
    Pass::Pieces(slots(shape, &classes))
}

/// How many general purpose registers a scalar of this size takes.
fn registers(size: u64) -> u32 {
    u32::try_from(size.div_ceil(8)).unwrap_or(1).max(1)
}

#[cfg(test)]
mod tests {
    use super::super::tests::{float, fpr, gpr, int, packed, record, target};
    use super::super::{Arg, Kind, Pass, Piece, Scalar, Shape};
    use super::*;

    /// A call on x86-64 Linux with nothing spent yet.
    fn call() -> Call {
        target("x86_64-unknown-linux-gnu").call()
    }

    #[test]
    fn a_structure_of_two_integers_travels_in_two_registers() {
        let pieces = packed(&[int(4), int(4)]);
        let shape = record(&pieces);
        assert_eq!(shape.size, 8);
        assert_eq!(call().argument(&Arg::Aggregate(shape)), Pass::Pieces(vec![gpr(0, 8)]));

        let pieces = packed(&[int(4), int(4), int(4)]);
        let shape = record(&pieces);
        assert_eq!(shape.size, 12);
        assert_eq!(
            call().argument(&Arg::Aggregate(shape)),
            // The second register holds the four bytes that are left rather than eight bytes
            // that are not there, since the object stops at twelve.
            Pass::Pieces(vec![gpr(0, 8), gpr(8, 4)])
        );
    }

    #[test]
    fn an_integer_beside_a_float_sends_the_float_into_a_general_register() {
        // The classic one. `struct { int a; float b; }` is one eightbyte holding both, the
        // merge rule says INTEGER, and the float arrives in the low half of a general purpose
        // register rather than in a vector one.
        let pieces = packed(&[int(4), float(Format::Single, 4)]);
        assert_eq!(
            call().argument(&Arg::Aggregate(record(&pieces))),
            Pass::Pieces(vec![gpr(0, 8)])
        );

        // With eight bytes between them they are in different eightbytes and each goes where it
        // belongs.
        let pieces = packed(&[int(8), float(Format::Double, 8)]);
        assert_eq!(
            call().argument(&Arg::Aggregate(record(&pieces))),
            Pass::Pieces(vec![gpr(0, 8), fpr(8, Format::Double)])
        );
    }

    #[test]
    fn two_floats_in_one_eightbyte_are_one_vector_register() {
        let pieces = packed(&[float(Format::Single, 4), float(Format::Single, 4)]);
        assert_eq!(
            call().argument(&Arg::Aggregate(record(&pieces))),
            Pass::Pieces(vec![fpr(0, Format::Double)])
        );

        // One `float` on its own is four bytes and reading eight of them would read past the
        // object, so the slot is as wide as the object is.
        let pieces = packed(&[float(Format::Single, 4)]);
        assert_eq!(
            call().argument(&Arg::Aggregate(record(&pieces))),
            Pass::Pieces(vec![fpr(0, Format::Single)])
        );
    }

    #[test]
    fn anything_over_sixteen_bytes_is_passed_in_memory() {
        let pieces = packed(&[int(8), int(8), int(8)]);
        assert_eq!(call().argument(&Arg::Aggregate(record(&pieces))), Pass::Memory);
        // And comes back through a hidden pointer rather than in the argument area.
        assert_eq!(call().returns(&Arg::Aggregate(record(&pieces))), Pass::Reference);
    }

    #[test]
    fn a_member_that_is_not_aligned_puts_the_whole_thing_in_memory() {
        // `struct __attribute__((packed)) { char c; int b; }`, where `b` is at offset one.
        let pieces = [
            Piece { offset: 0, scalar: int(1) },
            Piece { offset: 1, scalar: Scalar { kind: Kind::Integer, size: 4, align: 4 } },
        ];
        let shape = Shape { size: 5, align: 1, pieces: &pieces, complex: false };
        assert_eq!(call().argument(&Arg::Aggregate(shape)), Pass::Memory);

        // A bit-field is allowed to sit anywhere, which is what an alignment of one says, and
        // it does not send anything to memory.
        let pieces = [
            Piece { offset: 0, scalar: int(1) },
            Piece { offset: 1, scalar: Scalar { kind: Kind::Integer, size: 4, align: 1 } },
        ];
        let shape = Shape { size: 5, align: 1, pieces: &pieces, complex: false };
        assert_eq!(call().argument(&Arg::Aggregate(shape)), Pass::Pieces(vec![gpr(0, 5)]));
    }

    #[test]
    fn a_long_double_in_a_record_is_memory_going_in_and_the_x87_stack_coming_back() {
        let pieces = packed(&[float(Format::X87Extended, 16)]);
        let shape = record(&pieces);
        assert_eq!(shape.size, 16);
        assert_eq!(call().argument(&Arg::Aggregate(shape)), Pass::Memory);
        assert_eq!(
            call().returns(&Arg::Aggregate(shape)),
            Pass::Pieces(vec![fpr(0, Format::X87Extended)])
        );
    }

    #[test]
    fn a_complex_long_double_comes_back_where_a_record_of_two_does_not() {
        let pieces = packed(&[float(Format::X87Extended, 16), float(Format::X87Extended, 16)]);
        let complex = Shape { complex: true, ..record(&pieces) };
        assert_eq!(
            call().returns(&Arg::Aggregate(complex)),
            // Two registers of the x87 stack, holding the real part and then the imaginary
            // one, which are sixteen bytes apart because that is where the members are.
            Pass::Pieces(vec![fpr(0, Format::X87Extended), fpr(16, Format::X87Extended)])
        );
        // The same thirty two bytes with the same two members in the same places.
        assert_eq!(call().returns(&Arg::Aggregate(record(&pieces))), Pass::Reference);
    }

    #[test]
    fn an_aggregate_that_runs_out_of_registers_goes_to_memory_and_a_scalar_does_not() {
        let pieces = packed(&[int(8), int(8)]);
        let shape = Arg::Aggregate(record(&pieces));
        let mut call = call();
        // Five integer arguments leave one register, and this wants two.
        for _ in 0..5 {
            assert_eq!(call.argument(&Arg::Scalar(int(4))), Pass::Direct);
        }
        assert_eq!(call.argument(&shape), Pass::Memory);
        // A scalar after it still travels as itself, in the register that is left. Where an
        // argument sits is the backend's arithmetic and does not change what it is.
        assert_eq!(call.argument(&Arg::Scalar(int(4))), Pass::Direct);
        assert_eq!(call.argument(&Arg::Scalar(int(4))), Pass::Direct);
    }

    #[test]
    fn a_returned_pointer_to_memory_spends_the_register_the_first_argument_wanted() {
        let big = packed(&[int(8), int(8), int(8)]);
        let pair = packed(&[int(8), int(8)]);
        let mut call = call();
        assert_eq!(call.returns(&Arg::Aggregate(record(&big))), Pass::Reference);
        // Five registers are left rather than six, so a two register aggregate fits after four
        // integers and not after five.
        for _ in 0..3 {
            assert_eq!(call.argument(&Arg::Scalar(int(4))), Pass::Direct);
        }
        assert_eq!(
            call.argument(&Arg::Aggregate(record(&pair))),
            Pass::Pieces(vec![gpr(0, 8), gpr(8, 8)])
        );
    }

    #[test]
    fn an_int128_takes_two_registers_and_a_long_double_takes_none() {
        let pair = packed(&[int(8), int(8)]);
        let mut call = call();
        for _ in 0..2 {
            assert_eq!(call.argument(&Arg::Scalar(int(16))), Pass::Direct);
        }
        // Four of the six are gone, so the aggregate fits, and the `long double` between them
        // spent nothing on its way past.
        assert_eq!(call.argument(&Arg::Scalar(float(Format::X87Extended, 16))), Pass::Direct);
        assert_eq!(
            call.argument(&Arg::Aggregate(record(&pair))),
            Pass::Pieces(vec![gpr(0, 8), gpr(8, 8)])
        );
    }

    #[test]
    fn an_aggregate_of_no_size_travels_nowhere() {
        let shape = Shape { size: 0, align: 1, pieces: &[], complex: false };
        assert_eq!(call().argument(&Arg::Aggregate(shape)), Pass::Ignore);
        assert_eq!(call().returns(&Arg::Aggregate(shape)), Pass::Ignore);
        assert_eq!(call().returns(&Arg::Void), Pass::Ignore);
    }
}
