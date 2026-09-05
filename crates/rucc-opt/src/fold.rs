//! Constant folding: an instruction whose operands are all constants becomes a constant.
//!
//! The smallest transformation there is, and the one the rest of the middle end leans on. Every
//! later pass produces constants where the source had none, and none of them should have to
//! evaluate the arithmetic itself.
//!
//! It is worth having before any of them because the lowering walk produces constant arithmetic
//! that nothing in the C asked for. The usual arithmetic conversions widen a literal to the type
//! of the other operand, so `long y; y + 7` lowers to a 32 bit constant, a `sext` of it and an
//! add, and nothing downstream can see that the operand of the add is a number. On x86-64 that
//! costs two instructions and a register on every operation between a wide integer and a
//! literal, which is most address arithmetic and most loop bounds in real code. That is issue
//! 378.
//!
//! # How it rewrites
//!
//! In place. An instruction that folds keeps its result value and becomes an `iconst`, because
//! the value it produced already has the right type and every use of it is already correct. So
//! there is no rewriting of uses, no new value, and nothing for a later pass to have to know
//! about. What is left behind is the old operand, now used by nothing, which costs nothing in
//! the output because the backend materializes a constant where it is wanted rather than where
//! the IR wrote it, and which dead code elimination will take out of the printed IR when there
//! is one.
//!
//! # What it does not fold
//!
//! Not the divides and the remainders. Both have two cases the language leaves undefined, a zero
//! divisor and the most negative value divided by minus one, and both want guarding rather than
//! evaluating. They belong with the strength reduction that turns a division by a constant into
//! a multiply, which is where somebody looking for division arithmetic will look.
//!
//! Not floating point. Folding it means deciding what rounding mode to fold under and what to do
//! about a signalling NaN, and `rucc_base::float` has the arithmetic but the decision about the
//! environment belongs with the rest of the floating point work rather than in the first pass.
//!
//! Not an operation that overflows under `nsw` or `nuw`. The result there is poison, so any
//! answer would be a valid refinement, and quietly picking the wrapping one hides a program that
//! has stepped outside the language from the sanitizer that should be reporting it.
//!
//! Not comparisons. An `icmp` produces an `i1`, the backend folds one that feeds a branch into
//! the branch, and nothing lowers an `i1` that is left standing on its own, which is issue 352.
//! Turning a comparison into a constant before that is fixed would turn working code into code
//! that does not build.

use rucc_ir::{Block, Def, Extra, Flags, Func, Imm, Inst, Opcode, Type, Value};

use crate::{Fuel, Pass};

/// The pass. It holds nothing, because folding needs to know nothing beyond the instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fold;

impl Pass for Fold {
    fn name(&self) -> &'static str {
        "fold"
    }

    fn describe(&self) -> &'static str {
        "an integer instruction whose operands are all constants becomes a constant"
    }

    fn run(&self, func: &mut Func, fuel: &mut Fuel) -> bool {
        let blocks: Vec<Block> = func.blocks().collect();
        let mut changed = false;
        for block in blocks {
            let insts: Vec<Inst> = func.insts(block).collect();
            for inst in insts {
                let Some(folded) = evaluate(func, inst) else { continue };
                if !fuel.take() {
                    // Out of fuel, which is a request to stop transforming rather than to stop
                    // looking. Continuing the walk costs nothing and keeps the count of what
                    // could have been folded the same at every fuel setting, which is what makes
                    // a bisection over it monotonic.
                    continue;
                }
                let ty = func[result_of(func, inst)].ty;
                let at = func.add_imm(folded);
                let data = &mut func[inst];
                data.opcode = Opcode::IConst;
                data.flags = Flags::NONE;
                data.args = rucc_ir::ValueList::EMPTY;
                data.extra = Extra::Imm(at);
                debug_assert!(ty.is_int(), "only an integer instruction folds");
                changed = true;
            }
        }
        changed
    }
}

/// The single result of an instruction that folded.
fn result_of(func: &Func, inst: Inst) -> Value {
    func[inst].results().next().expect("an instruction that folds produces a value")
}

/// What this instruction evaluates to, if it evaluates to anything.
///
/// `None` covers every reason not to fold and does not distinguish between them, because the
/// answer to all of them is the same: leave the instruction alone.
fn evaluate(func: &Func, inst: Inst) -> Option<Imm> {
    let data = &func[inst];
    if data.results != 1 {
        return None;
    }
    let result = data.results().next()?;
    let ty = func[result].ty;
    // A vector constant is a `splat` rather than an `iconst`, so a vector fold would have to
    // build a different instruction and would have to be right about the lane count as well.
    if !ty.is_int() || !ty.is_scalar() {
        return None;
    }
    let args = &func[data.args];
    match data.opcode {
        Opcode::Trunc | Opcode::SExt | Opcode::ZExt => {
            let (value, from) = constant(func, *args.first()?)?;
            Some(convert(data.opcode, value, from, ty))
        }
        Opcode::Shl | Opcode::LShr | Opcode::AShr => {
            let (value, from) = constant(func, *args.first()?)?;
            let (count, count_ty) = constant(func, *args.get(1)?)?;
            shift(data.opcode, value, from, count, count_ty, ty, data.flags)
        }
        Opcode::Add | Opcode::Sub | Opcode::Mul | Opcode::And | Opcode::Or | Opcode::Xor => {
            let (lhs, lhs_ty) = constant(func, *args.first()?)?;
            let (rhs, _) = constant(func, *args.get(1)?)?;
            binary(data.opcode, lhs, rhs, lhs_ty, ty, data.flags)
        }
        _ => None,
    }
}

/// The constant this value is, with the type it has, if it is one.
fn constant(func: &Func, value: Value) -> Option<(Imm, Type)> {
    let Def::Result { inst, .. } = func[value].def else { return None };
    if func[inst].opcode != Opcode::IConst {
        return None;
    }
    let Extra::Imm(at) = func[inst].extra else { return None };
    let ty = func[value].ty;
    ty.is_int().then(|| (func[at], ty))
}

/// A widening or a narrowing of a constant.
fn convert(opcode: Opcode, value: Imm, from: Type, to: Type) -> Imm {
    match opcode {
        // Truncation is the masking that `Imm::int` does anyway, and sign extension is reading
        // the value as signed at its own width and storing it at the wider one.
        Opcode::Trunc | Opcode::SExt => Imm::int(value.signed(from), to),
        // Zero extension reads the same bits as unsigned, which for a width below 128 is a
        // non-negative number and survives the cast to the signed type `Imm::int` takes.
        _ => Imm::int(value.unsigned() as i128, to),
    }
}

/// A shift of a constant by a constant.
///
/// `None` when the count is not one the language defines, which is a count at or above the width
/// of the value. The result there is poison and folding it would be picking an answer for a
/// program that asked for none.
fn shift(
    opcode: Opcode,
    value: Imm,
    from: Type,
    count: Imm,
    count_ty: Type,
    to: Type,
    flags: Flags,
) -> Option<Imm> {
    let by = count.unsigned();
    if by >= u128::from(to.bits()) || count.signed(count_ty) < 0 {
        return None;
    }
    let by = by as u32;
    let exact = match opcode {
        Opcode::Shl => value.signed(from).checked_shl(by)?,
        // A logical shift right is on the bits rather than on the number, so it reads unsigned
        // and the cast back cannot lose anything: the value has at most `from.bits()` bits set
        // and shifting right sets none.
        Opcode::LShr => (value.unsigned() >> by) as i128,
        _ => value.signed(from) >> by,
    };
    if opcode == Opcode::Shl && overflowed(exact, to, flags) {
        return None;
    }
    Some(Imm::int(exact, to))
}

/// An arithmetic or bitwise operation on two constants.
fn binary(opcode: Opcode, lhs: Imm, rhs: Imm, from: Type, to: Type, flags: Flags) -> Option<Imm> {
    let (a, b) = (lhs.signed(from), rhs.signed(from));
    let exact = match opcode {
        // The bitwise three cannot overflow and are the same operation whichever way the
        // operands are read, so they take the signed reading and are done.
        Opcode::And => a & b,
        Opcode::Or => a | b,
        Opcode::Xor => a ^ b,
        // The arithmetic three are computed at 128 bits and then asked whether they fit. A type
        // of 128 bits is the one case where the checked form is doing real work rather than
        // being a formality, and it is why these are checked rather than wrapping.
        Opcode::Add => a.checked_add(b)?,
        Opcode::Sub => a.checked_sub(b)?,
        _ => a.checked_mul(b)?,
    };
    if overflowed(exact, to, flags) {
        return None;
    }
    Some(Imm::int(exact, to))
}

/// Whether storing `exact` at `to` would lose something the flags promised would not happen.
///
/// An operation with neither flag wraps, and wrapping is defined, so the answer there is no
/// however far outside the type the exact result is.
fn overflowed(exact: i128, to: Type, flags: Flags) -> bool {
    let stored = Imm::int(exact, to);
    if flags.contains(Flags::NSW) && stored.signed(to) != exact {
        return true;
    }
    flags.contains(Flags::NUW) && (exact < 0 || stored.unsigned() != exact as u128)
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{Block, Builder, Extra, Flags, Func, Module, Opcode, Signature, Type, Value};
    use rucc_target::{Arch, Env, Os, TargetInfo, Triple};

    use crate::{Fuel, Pass, fold::Fold};

    /// A function with one block, ready to have instructions appended to it.
    fn blank() -> (Interner, Func, Block) {
        let mut names = Interner::new();
        let name = names.intern("f");
        let mut func = Func::new(name, Signature::new().with_returns(&[Type::int(64)]));
        let block = func.create_block();
        (names, func, block)
    }

    /// Runs the pass over the function with as much fuel as it wants.
    fn fold(func: &mut Func) -> bool {
        Fold.run(func, &mut Fuel::unlimited())
    }

    /// The constant a value now holds, or `None` if it is not one.
    fn value_of(func: &Func, value: Value, ty: Type) -> Option<i128> {
        let rucc_ir::Def::Result { inst, .. } = func[value].def else { return None };
        if func[inst].opcode != Opcode::IConst {
            return None;
        }
        let Extra::Imm(at) = func[inst].extra else { return None };
        Some(func[at].signed(ty))
    }

    #[test]
    fn a_widened_constant_becomes_a_constant_of_the_wider_type() {
        let (_, mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let narrow = build.iconst(Type::int(32), 7);
        let wide = build.unary(Opcode::SExt, narrow, Type::int(64));
        build.ret(&[wide]);
        assert!(fold(&mut func));
        assert_eq!(value_of(&func, wide, Type::int(64)), Some(7));
    }

    #[test]
    fn sign_extension_copies_the_sign_and_zero_extension_does_not() {
        for (opcode, expected) in [(Opcode::SExt, -1_i128), (Opcode::ZExt, 0xffff_ffff)] {
            let (_, mut func, block) = blank();
            let mut build = Builder::new(&mut func, block);
            let narrow = build.iconst(Type::int(32), -1);
            let wide = build.unary(opcode, narrow, Type::int(64));
            build.ret(&[wide]);
            assert!(fold(&mut func));
            assert_eq!(value_of(&func, wide, Type::int(64)), Some(expected), "{opcode:?}");
        }
    }

    #[test]
    fn truncation_keeps_the_low_bits_and_reads_them_at_the_narrow_width() {
        let (_, mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let wide = build.iconst(Type::int(32), 0x1234_5680);
        let narrow = build.unary(Opcode::Trunc, wide, Type::int(8));
        build.ret(&[narrow]);
        assert!(fold(&mut func));
        assert_eq!(value_of(&func, narrow, Type::int(8)), Some(-128));
    }

    #[test]
    fn the_arithmetic_and_the_bitwise_operations_are_evaluated() {
        let cases = [
            (Opcode::Add, 6_i128, 7_i128, 13_i128),
            (Opcode::Sub, 6, 7, -1),
            (Opcode::Mul, 6, 7, 42),
            (Opcode::And, 0b1100, 0b1010, 0b1000),
            (Opcode::Or, 0b1100, 0b1010, 0b1110),
            (Opcode::Xor, 0b1100, 0b1010, 0b0110),
        ];
        for (opcode, a, b, want) in cases {
            let (_, mut func, block) = blank();
            let mut build = Builder::new(&mut func, block);
            let lhs = build.iconst(Type::int(64), a);
            let rhs = build.iconst(Type::int(64), b);
            let out = build.binary(opcode, lhs, rhs, Flags::NONE);
            build.ret(&[out]);
            assert!(fold(&mut func), "{opcode:?}");
            assert_eq!(value_of(&func, out, Type::int(64)), Some(want), "{opcode:?}");
        }
    }

    #[test]
    fn the_three_shifts_are_evaluated_and_the_two_right_ones_differ_on_the_sign() {
        let cases = [(Opcode::Shl, -8_i128, 1_i128, -16_i128), (Opcode::AShr, -8, 1, -4)];
        for (opcode, a, b, want) in cases {
            let (_, mut func, block) = blank();
            let mut build = Builder::new(&mut func, block);
            let lhs = build.iconst(Type::int(64), a);
            let rhs = build.iconst(Type::int(64), b);
            let out = build.binary(opcode, lhs, rhs, Flags::NONE);
            build.ret(&[out]);
            assert!(fold(&mut func), "{opcode:?}");
            assert_eq!(value_of(&func, out, Type::int(64)), Some(want), "{opcode:?}");
        }
        // The logical shift is the one that reads the value as bits, so minus eight shifted
        // right by one is a very large positive number rather than minus four.
        let (_, mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let lhs = build.iconst(Type::int(64), -8);
        let rhs = build.iconst(Type::int(64), 1);
        let out = build.binary(Opcode::LShr, lhs, rhs, Flags::NONE);
        build.ret(&[out]);
        assert!(fold(&mut func));
        assert_eq!(value_of(&func, out, Type::int(64)), Some(i128::from(i64::MAX) - 3));
    }

    #[test]
    fn a_shift_by_the_width_or_more_is_left_alone_because_the_language_does_not_define_it() {
        for count in [64_i128, 65, -1] {
            let (_, mut func, block) = blank();
            let mut build = Builder::new(&mut func, block);
            let lhs = build.iconst(Type::int(64), 1);
            let rhs = build.iconst(Type::int(64), count);
            let out = build.binary(Opcode::Shl, lhs, rhs, Flags::NONE);
            build.ret(&[out]);
            assert!(!fold(&mut func), "a shift by {count} was folded");
        }
    }

    #[test]
    fn an_operation_that_wraps_folds_and_the_same_one_promising_it_will_not_does_not() {
        let big = i128::from(i32::MAX);
        for (flags, folds) in [(Flags::NONE, true), (Flags::NSW, false)] {
            let (_, mut func, block) = blank();
            let mut build = Builder::new(&mut func, block);
            let lhs = build.iconst(Type::int(32), big);
            let rhs = build.iconst(Type::int(32), 1);
            let out = build.binary(Opcode::Add, lhs, rhs, flags);
            build.ret(&[out]);
            assert_eq!(fold(&mut func), folds, "{flags}");
            if folds {
                assert_eq!(value_of(&func, out, Type::int(32)), Some(i128::from(i32::MIN)));
            }
        }
    }

    #[test]
    fn an_unsigned_promise_is_broken_by_a_negative_result_as_well_as_by_a_large_one() {
        let (_, mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let lhs = build.iconst(Type::int(32), 1);
        let rhs = build.iconst(Type::int(32), 2);
        let out = build.binary(Opcode::Sub, lhs, rhs, Flags::NUW);
        build.ret(&[out]);
        assert!(!fold(&mut func));
    }

    #[test]
    fn an_operation_with_one_constant_operand_is_left_alone() {
        let (_, mut func, block) = blank();
        let param = func.append_param(block, Type::int(64));
        let mut build = Builder::new(&mut func, block);
        let rhs = build.iconst(Type::int(64), 7);
        let out = build.binary(Opcode::Add, param, rhs, Flags::NONE);
        build.ret(&[out]);
        assert!(!fold(&mut func));
        assert_eq!(func[out_inst(&func, out)].opcode, Opcode::Add);
    }

    #[test]
    fn a_divide_is_not_folded_even_when_both_operands_are_constants() {
        for opcode in [Opcode::SDiv, Opcode::UDiv, Opcode::SRem, Opcode::URem] {
            let (_, mut func, block) = blank();
            let mut build = Builder::new(&mut func, block);
            let lhs = build.iconst(Type::int(64), 42);
            let rhs = build.iconst(Type::int(64), 7);
            let out = build.binary(opcode, lhs, rhs, Flags::NONE);
            build.ret(&[out]);
            assert!(!fold(&mut func), "{opcode:?}");
        }
    }

    #[test]
    fn a_comparison_is_not_folded_because_nothing_lowers_the_bit_it_would_leave_behind() {
        let (_, mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let lhs = build.iconst(Type::int(64), 1);
        let rhs = build.iconst(Type::int(64), 2);
        let out = build.icmp(rucc_ir::IntPred::Slt, lhs, rhs);
        build.ret(&[out]);
        assert!(!fold(&mut func));
    }

    #[test]
    fn folding_leaves_the_function_something_the_verifier_accepts() {
        let mut names = Interner::new();
        let name = names.intern("f");
        let mut func = Func::new(name, Signature::new().with_returns(&[Type::int(64)]));
        let block = func.create_block();
        let mut build = Builder::new(&mut func, block);
        let narrow = build.iconst(Type::int(32), 7);
        let wide = build.unary(Opcode::SExt, narrow, Type::int(64));
        build.ret(&[wide]);
        assert!(fold(&mut func));
        let target = TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu));
        let module_name = names.intern("m");
        let mut module = Module::new(module_name, &target);
        module.add_func(func);
        rucc_ir::verify(&module, &names).expect("folding does not break the IR");
    }

    #[test]
    fn fuel_stops_the_transformation_and_not_the_walk() {
        let build_two = |func: &mut Func, block: Block| {
            let mut build = Builder::new(func, block);
            let a = build.iconst(Type::int(32), 7);
            let wide_a = build.unary(Opcode::SExt, a, Type::int(64));
            let b = build.iconst(Type::int(32), 9);
            let wide_b = build.unary(Opcode::SExt, b, Type::int(64));
            let sum = build.binary(Opcode::Add, wide_a, wide_b, Flags::NONE);
            build.ret(&[sum]);
            (wide_a, wide_b)
        };

        let (_, mut none, block) = blank();
        let (first, _) = build_two(&mut none, block);
        assert!(!Fold.run(&mut none, &mut Fuel::of(0)));
        assert_eq!(none[out_inst(&none, first)].opcode, Opcode::SExt);

        let (_, mut one, block) = blank();
        let (first, second) = build_two(&mut one, block);
        let mut fuel = Fuel::of(1);
        assert!(Fold.run(&mut one, &mut fuel));
        assert_eq!(fuel.spent(), 1);
        assert_eq!(one[out_inst(&one, first)].opcode, Opcode::IConst);
        assert_eq!(one[out_inst(&one, second)].opcode, Opcode::SExt);
    }

    #[test]
    fn folding_one_operation_uncovers_the_next() {
        let (_, mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let a = build.iconst(Type::int(32), 7);
        let wide = build.unary(Opcode::SExt, a, Type::int(64));
        let b = build.iconst(Type::int(64), 9);
        let sum = build.binary(Opcode::Add, wide, b, Flags::NONE);
        build.ret(&[sum]);
        assert!(fold(&mut func));
        // One walk in order is enough for this shape, because a constant is written before it
        // is used and the walk is in the same order.
        assert_eq!(value_of(&func, sum, Type::int(64)), Some(16));
    }

    #[test]
    fn a_constant_is_left_where_it_is_and_folding_it_again_changes_nothing() {
        let (_, mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let a = build.iconst(Type::int(32), 7);
        let wide = build.unary(Opcode::SExt, a, Type::int(64));
        build.ret(&[wide]);
        assert!(fold(&mut func));
        assert!(!fold(&mut func), "a second run found something to do");
    }

    /// The instruction that defines a value, which every value in these tests has.
    fn out_inst(func: &Func, value: Value) -> rucc_ir::Inst {
        match func[value].def {
            rucc_ir::Def::Result { inst, .. } => inst,
            rucc_ir::Def::Param { .. } => panic!("a parameter has no instruction"),
        }
    }
}
