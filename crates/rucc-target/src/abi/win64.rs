//! Windows x64, `spec/12-abi-and-runtime.md` section 12.4.
//!
//! The simplest of the four and the one with the sharpest rule: anything that is not exactly
//! one, two, four or eight bytes travels as the address of a copy the caller made. There is no
//! classification to do and no pair of registers to fill, so a three byte structure and a three
//! hundred byte structure are passed the same way.
//!
//! An aggregate that does fit travels as an integer of its size whatever is in it, so a
//! `struct { float x, y; }` arrives in a general purpose register. That is the other half of
//! the shared argument positions: rcx, rdx, r8 and r9 are the four an integer can use, xmm0 to
//! xmm3 are the four a floating point value can use, and they are the same four positions, so
//! a call taking an `int` and then a `double` uses rcx and xmm1 and never xmm0.

use super::{Arg, Call, Pass, Slot};

/// Whether an aggregate of this size travels as a value rather than as an address.
fn fits(size: u64) -> bool {
    matches!(size, 1 | 2 | 4 | 8)
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
    if fits(shape.size) {
        return Pass::Pieces(vec![Slot::Integer(u32::try_from(shape.size).unwrap_or(8))]);
    }
    // The address of somewhere to put it is the first argument, in rcx, which moves everything
    // the function was called with one position along.
    call.gp = call.gp.saturating_sub(1);
    Pass::Reference
}

/// How one argument travels.
pub(super) fn argument(call: &mut Call, arg: &Arg<'_>) -> Pass {
    let shape = match arg {
        Arg::Void => return Pass::Ignore,
        Arg::Scalar(_) => {
            call.gp = call.gp.saturating_sub(1);
            return Pass::Direct;
        }
        Arg::Aggregate(shape) => shape,
    };
    if shape.size == 0 {
        return Pass::Ignore;
    }
    // One position either way, since an address is one register and so is a value that fits in
    // one. Running out of them puts the argument on the stack and does not change its form,
    // which is why nothing here counts what is left.
    call.gp = call.gp.saturating_sub(1);
    if fits(shape.size) {
        return Pass::Pieces(vec![Slot::Integer(u32::try_from(shape.size).unwrap_or(8))]);
    }
    Pass::Reference
}

#[cfg(test)]
mod tests {
    use rucc_base::float::Format;

    use super::super::tests::{float, int, packed, record, target};
    use super::super::{Arg, Pass, Slot};
    use super::*;

    /// A call on Windows x64 with nothing spent yet.
    fn call() -> Call {
        target("x86_64-pc-windows-msvc").call()
    }

    #[test]
    fn a_size_a_register_holds_travels_as_an_integer_whatever_is_in_it() {
        for scalars in [vec![int(1)], vec![int(2)], vec![int(4)], vec![int(4), int(4)]] {
            let pieces = packed(&scalars);
            let shape = record(&pieces);
            let size = u32::try_from(shape.size).expect("a small record");
            assert_eq!(
                call().argument(&Arg::Aggregate(shape)),
                Pass::Pieces(vec![Slot::Integer(size)])
            );
        }

        // Two `float`s are eight bytes, and eight bytes go in a general purpose register here.
        let pieces = packed(&[float(Format::Single, 4), float(Format::Single, 4)]);
        assert_eq!(
            call().argument(&Arg::Aggregate(record(&pieces))),
            Pass::Pieces(vec![Slot::Integer(8)])
        );
    }

    #[test]
    fn any_other_size_travels_as_an_address() {
        // Three bytes and twenty four bytes are the same answer, which is the rule that makes
        // this ABI short.
        let pieces = packed(&[int(1), int(1), int(1)]);
        assert_eq!(call().argument(&Arg::Aggregate(record(&pieces))), Pass::Reference);
        let pieces = packed(&[int(8), int(8), int(8)]);
        assert_eq!(call().argument(&Arg::Aggregate(record(&pieces))), Pass::Reference);
        assert_eq!(call().returns(&Arg::Aggregate(record(&pieces))), Pass::Reference);
    }

    #[test]
    fn an_argument_past_the_fourth_travels_the_way_the_first_one_does() {
        let pieces = packed(&[int(4), int(4)]);
        let mut call = call();
        for _ in 0..6 {
            assert_eq!(call.argument(&Arg::Scalar(int(8))), Pass::Direct);
        }
        // On the stack by now, and still eight bytes of value rather than an address to them.
        assert_eq!(
            call.argument(&Arg::Aggregate(record(&pieces))),
            Pass::Pieces(vec![Slot::Integer(8)])
        );
    }
}
