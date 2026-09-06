//! Width narrowing: arithmetic redone at the width the program actually uses.
//!
//! The lowering rule set is written at an opcode and a width together, so `add.i8` and `add.i32`
//! are two rules and the machine can be asked to add two bytes as easily as two words. C never
//! asks it to. The integer promotions say the operands of an arithmetic operator go to `int`
//! first, so `char a, b; a + b` is an `int` addition of two sign extended bytes, and the front end
//! is right to write it that way because that is what the language says the expression means.
//!
//! That leaves the promoted form as the only form, and on x86-64 it is often the wrong one. A
//! byte compare against a byte is a `cmpb`, and two `movsbl` are not needed to reach it. A byte
//! add whose result is stored back into a `char` throws away every bit the promotion computed.
//! The promoted shape exists because C says so and not because the machine wants it. This is
//! issue 375.
//!
//! # The two shapes
//!
//! A truncation of arithmetic. The low bits of a sum, a difference, a product, a bitwise
//! operation or a shift by a constant depend only on the low bits of what went into it, so
//! `trunc.i8 (add.i32 (sext a) (sext b))` is `add.i8 a b` and the two extensions are left with
//! nothing reading them. That is the arithmetic half, and it is what `char c = a + b;` is.
//!
//! A comparison of extensions. Sign extension is an order isomorphism onto its image under both
//! readings of the bits, so a comparison of two of them at any predicate is the same comparison of
//! what they extended. Zero extension is one under the unsigned reading and is not one under the
//! signed reading, since it takes a negative byte to a positive word, so it carries the equalities
//! and the unsigned predicates over and not the signed ones. That is what `char a, b; a < b` is.
//!
//! Both are written so that one side may be a constant instead, because `if (c == 'x')` is the
//! common case and the constant is representable at the narrow width whenever the comparison is
//! not already decided.
//!
//! # Why it always pays
//!
//! Neither shape is applied unless every leaf it reaches narrows for nothing. A leaf is what an
//! extension extended, which is already the narrow value, or a constant, which is written down
//! again. So the rewrite replaces a wide operation, its extensions and the truncation with one
//! narrow operation and never leaves a widening behind to pay for a narrowing. Everything in
//! between is required to have exactly one reader, which is the operation above it, so the whole
//! subtree it replaces is dead the moment it is replaced.
//!
//! That is the whole profitability argument, and it is deliberately a structural one rather than
//! a cost model. A pass whose payoff has to be estimated is a pass whose payoff can be wrong.
//!
//! # What it does not narrow
//!
//! Not a divide or a remainder. `char a = -128, b = -1; char c = a / b;` is well defined in C: the
//! division happens at `int`, gives 128, and the conversion back to `char` is what makes it minus
//! 128 again. The same division at one byte is the overflow case that raises on this machine, so
//! narrowing it turns a program that works into a program that dies. It needs a range that says
//! the operands miss that one pair, and ranges are the analysis this pass does not have.
//!
//! Not a shift by a value. `char c; c <<= n;` shifts at `int`, so a count of twenty is a defined
//! shift whose low eight bits are zero, and the same count at one byte is poison. A shift by a
//! constant below the narrow width has neither problem and is narrowed.
//!
//! Not a signed operation's overflow flags. A sum that could not overflow at four bytes can
//! overflow at one, so `nsw` and `nuw` do not come along. Dropping them is a refinement in the
//! safe direction: it makes the operation more defined rather than less.
//!
//! # What is left for the analysis
//!
//! The width here is the one the truncation names. A real demanded bits analysis would let it
//! shrink further, so that `(x & 0xff) + 1` narrows on the strength of the mask rather than on the
//! strength of a truncation that is not written, and so that a value read at three widths is
//! narrowed to the widest of them rather than to none. That is the first box of issue 375 and it
//! wants the analysis manager, which wants the dominator tree, which is the next thing to build.

use rucc_ir::{Block, Def, Extra, Flags, Func, Imm, Inst, InstData, Opcode, Type, Value};

use crate::uses::count;
use crate::{Analyses, Fuel, Pass, Preserved, Stats};

/// Recorded once for each subtree redone at the narrow width.
const NARROWED: &str = "arithmetic redone at the width the program truncates it to";

/// Recorded for a subtree that would have been redone if there had been fuel for it.
const NO_FUEL: &str = "arithmetic left wide, the pass ran out of fuel";

/// How deep the walk from a truncation goes before it gives up.
///
/// A chain of arithmetic is as long as the expression somebody wrote, and generated C writes long
/// ones, so a walk with no limit is a stack overflow waiting for the right input file. Six is
/// deeper than hand written C reaches and shallow enough that the recursion cannot cost anything,
/// and an expression deeper than this narrows from whatever truncation is nearer to its leaves.
const DEPTH: u32 = 6;

/// The pass. It holds nothing, because the width it narrows to is the one the truncation names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Narrow;

impl Pass for Narrow {
    fn name(&self) -> &'static str {
        "narrow"
    }

    fn describe(&self) -> &'static str {
        "arithmetic the program truncates is redone at the width it truncates to"
    }

    fn preserves(&self) -> Preserved {
        // The arithmetic is redone at another width in the block it was already in. Widths are
        // not something the graph, the trees or the forest have an opinion about.
        Preserved::ALL
    }

    fn run(&self, func: &mut Func, _an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        let mut uses = count(func);
        for block in func.blocks().collect::<Vec<Block>>() {
            for inst in func.insts(block).collect::<Vec<Inst>>() {
                let Some(redo) = truncated_arithmetic(func, inst, &uses)
                    .or_else(|| extended_comparison(func, inst))
                else {
                    continue;
                };
                if !fuel.take() {
                    // Out of fuel, which stops the transforming rather than the looking, the
                    // same way the other three passes treat it. The walk is the same walk at
                    // every fuel setting, which is what makes bisecting over it monotonic.
                    stats.missed(NO_FUEL);
                    continue;
                }
                apply(func, inst, &redo, &mut uses);
                stats.optimized(NARROWED);
            }
        }
        stats
    }
}

/// An instruction rewritten at the narrow width, with its operands narrowed too.
struct Redo {
    /// What the instruction becomes, which is the wide operation at the narrow width.
    opcode: Opcode,
    /// The predicate, for a comparison, and nothing for arithmetic.
    extra: Extra,
    /// The width everything under this is redone at.
    ty: Type,
    /// The left operand.
    lhs: Plan,
    /// The right operand.
    rhs: Plan,
}

/// What an operand becomes at the narrow width.
enum Plan {
    /// A value that already has it, which is what an extension was extending.
    Already(Value),
    /// A constant, written down again at the narrow width.
    Constant(i128),
    /// An operation redone, which is the recursive case and the reason this is a tree.
    Nested(Box<Redo>),
}

/// Whether this is a truncation of arithmetic that can be redone narrow, and what it becomes.
///
/// The truncation is the root because it is the only place the narrow width is written down. Its
/// operand has to be read by nothing else, since a second reader would keep the wide operation
/// alive and the rewrite would be a second instruction rather than a replacement.
fn truncated_arithmetic(func: &Func, inst: Inst, uses: &[u32]) -> Option<Redo> {
    let data = &func[inst];
    if data.opcode != Opcode::Trunc {
        return None;
    }
    let ty = func[data.results().next()?].ty;
    if !narrowable(ty) {
        return None;
    }
    redo(func, *func[data.args].first()?, ty, uses, DEPTH)
}

/// Whether a width is one this pass will redo an operation at.
///
/// An integer scalar of a byte or more. The lower bound is the interesting half. One bit is an
/// integer type in the IR and a comparison against a zero extended truth is a comparison the
/// argument narrows all the way down to it, and `spec/12-instruction-selection.md` says a one bit
/// value is a truth rather than a width: `tamnd/rucc#352` is the list of what a target lowers at
/// that width and it is `and`, `or`, `xor`, a constant and the widening out of one. Narrowing an
/// `icmp` into it would be asking every target for something no target has, so the floor is the
/// narrowest width a machine holds a number in.
const fn narrowable(ty: Type) -> bool {
    ty.is_int() && ty.is_scalar() && ty.bits() >= 8
}

/// Whether this value is arithmetic that can be redone at that width, and what it becomes.
fn redo(func: &Func, value: Value, ty: Type, uses: &[u32], depth: u32) -> Option<Redo> {
    if depth == 0 || uses[value.index()] != 1 {
        return None;
    }
    let Def::Result { inst, .. } = func[value].def else { return None };
    let data = &func[inst];
    if !low_bits_only(data.opcode) {
        return None;
    }
    let args = &func[data.args];
    let (&left, &right) = (args.first()?, args.get(1)?);
    let lhs = plan(func, left, ty, uses, depth)?;
    // A shift is the one operation whose right operand is not a number of the same kind as its
    // left one, and it is the one that is unsafe to narrow when that operand is not a constant.
    let rhs = match data.opcode {
        Opcode::Shl => Plan::Constant(count_below(func, right, ty)?),
        _ => plan(func, right, ty, uses, depth)?,
    };
    Some(Redo { opcode: data.opcode, extra: Extra::None, ty, lhs, rhs })
}

/// What an operand becomes at that width, or `None` when it would cost something to get there.
fn plan(func: &Func, value: Value, ty: Type, uses: &[u32], depth: u32) -> Option<Plan> {
    if let Some(narrow) = extended(func, value, ty) {
        return Some(Plan::Already(narrow));
    }
    if let Some((imm, wide)) = constant(func, value) {
        return Some(Plan::Constant(imm.signed(wide)));
    }
    redo(func, value, ty, uses, depth - 1).map(|redo| Plan::Nested(Box::new(redo)))
}

/// Whether an operation's low bits depend only on the low bits of what went into it.
///
/// True of the four that carry left to right and of the three that work a bit at a time. Not true
/// of a divide, a remainder or a shift right, all of which read bits above the ones they produce.
const fn low_bits_only(opcode: Opcode) -> bool {
    matches!(
        opcode,
        Opcode::Add
            | Opcode::Sub
            | Opcode::Mul
            | Opcode::And
            | Opcode::Or
            | Opcode::Xor
            | Opcode::Shl
    )
}

/// Whether this is a comparison of two things extended from the same narrower width.
///
/// Sign extension keeps the order of what it extends under both readings of the bits, so every
/// predicate survives it. Zero extension keeps the unsigned order and not the signed one, since it
/// takes a negative byte to a positive word, so it carries the equalities and the unsigned
/// predicates and refuses the signed ones.
///
/// The two sides have to be the same extension as well as from the same width. `(signed char) a <
/// b` where `b` is an `unsigned char` is a sign extension against a zero extension, and comparing
/// what they extended is comparing a byte against a byte at one predicate where the wide
/// comparison had a signed byte against an unsigned one. Both readings of the narrow comparison
/// are wrong, and the wide comparison is right, which is the whole reason C promotes.
fn extended_comparison(func: &Func, inst: Inst) -> Option<Redo> {
    let data = &func[inst];
    if data.opcode != Opcode::ICmp {
        return None;
    }
    let Extra::IntPred(pred) = data.extra else { return None };
    let args = &func[data.args];
    let (&left, &right) = (args.first()?, args.get(1)?);
    let (kind, ty, narrow) = widening(func, left)?;
    if !narrowable(ty) {
        return None;
    }
    if kind == Opcode::ZExt && pred.is_signed() {
        return None;
    }
    let rhs = match widening(func, right) {
        Some((same, from, other)) if same == kind && from == ty => Plan::Already(other),
        _ => Plan::Constant(survives(func, right, kind, ty)?),
    };
    Some(Redo { opcode: Opcode::ICmp, extra: data.extra, ty, lhs: Plan::Already(narrow), rhs })
}

/// The extension this value is, as the kind, the width it came from and the value it extended.
fn widening(func: &Func, value: Value) -> Option<(Opcode, Type, Value)> {
    let Def::Result { inst, .. } = func[value].def else { return None };
    let data = &func[inst];
    if data.opcode != Opcode::SExt && data.opcode != Opcode::ZExt {
        return None;
    }
    let narrow = *func[data.args].first()?;
    Some((data.opcode, func[narrow].ty, narrow))
}

/// What this value was before it was extended to that width, when that is what it is.
///
/// Which extension it was is not asked, because this is the arithmetic side and the arithmetic
/// reads the low bits only. Those are the bits the extension copied, whichever one it was.
fn extended(func: &Func, value: Value, ty: Type) -> Option<Value> {
    let (_, from, narrow) = widening(func, value)?;
    (from == ty).then_some(narrow)
}

/// The constant this value is, with the type it has.
fn constant(func: &Func, value: Value) -> Option<(Imm, Type)> {
    let Def::Result { inst, .. } = func[value].def else { return None };
    let data = &func[inst];
    let Extra::Imm(at) = data.extra else { return None };
    if data.opcode != Opcode::IConst {
        return None;
    }
    let ty = func[value].ty;
    ty.is_int().then(|| (func[at], ty))
}

/// A shift count that is a constant below the narrow width, which is the only one that narrows.
///
/// A count at or above the width is poison at the narrow width and is a defined shift to zero at
/// the wide one, so the guard is what keeps the rewrite from inventing undefined behaviour. A
/// count that is not a constant cannot be guarded, since its value is what decides.
fn count_below(func: &Func, value: Value, ty: Type) -> Option<i128> {
    let (imm, wide) = constant(func, value)?;
    let by = imm.signed(wide);
    (by >= 0 && by < i128::from(ty.bits())).then_some(by)
}

/// A constant that is the extension of a constant at the narrow width, as that narrow constant.
///
/// Both extensions are injective, so a comparison against a constant in the image of one is the
/// same comparison against what it is the image of. A constant outside the image is a comparison
/// that is already decided, which is a thing for folding to say rather than for this to guess at.
fn survives(func: &Func, value: Value, kind: Opcode, ty: Type) -> Option<i128> {
    let (imm, wide) = constant(func, value)?;
    let k = imm.signed(wide);
    let back = Imm::int(k, ty).signed(ty);
    let same = if kind == Opcode::SExt { back } else { Imm::int(k, ty).unsigned() as i128 };
    (same == k).then_some(k)
}

/// Rewrites the instruction into what the plan says it is.
///
/// In place, because the result already has the narrow type and every use of it is already
/// correct, which is the same reason folding and the peephole rewrite in place. What is left
/// behind is the wide subtree, now read by nothing, which is what dead code elimination is for.
fn apply(func: &mut Func, inst: Inst, redo: &Redo, uses: &mut Vec<u32>) {
    let lhs = build(func, inst, redo.ty, &redo.lhs, uses);
    let rhs = build(func, inst, redo.ty, &redo.rhs, uses);
    for value in func[func[inst].args].iter().copied() {
        uses[value.index()] -= 1;
    }
    let args = func.push_values(&[lhs, rhs]);
    uses[lhs.index()] += 1;
    uses[rhs.index()] += 1;
    let data = &mut func[inst];
    data.opcode = redo.opcode;
    // No flags. An operation that could not overflow at the wide width can overflow at the narrow
    // one, so `nsw` and `nuw` do not survive the narrowing, and dropping them makes the operation
    // more defined rather than less.
    data.flags = Flags::NONE;
    data.args = args;
    data.extra = redo.extra;
}

/// The value an operand's plan comes to, writing whatever it needs in front of the instruction.
fn build(func: &mut Func, before: Inst, ty: Type, plan: &Plan, uses: &mut Vec<u32>) -> Value {
    match plan {
        Plan::Already(value) => *value,
        Plan::Constant(value) => {
            let at = func.add_imm(Imm::int(*value, ty.lane()));
            let data = InstData { extra: Extra::Imm(at), ..InstData::new(Opcode::IConst) };
            written(func, before, data, ty, uses)
        }
        Plan::Nested(redo) => {
            let lhs = build(func, before, redo.ty, &redo.lhs, uses);
            let rhs = build(func, before, redo.ty, &redo.rhs, uses);
            let args = func.push_values(&[lhs, rhs]);
            uses[lhs.index()] += 1;
            uses[rhs.index()] += 1;
            let data = InstData { args, extra: redo.extra, ..InstData::new(redo.opcode) };
            written(func, before, data, redo.ty, uses)
        }
    }
}

/// Puts an instruction in front of another one and gives back the value it produces.
fn written(func: &mut Func, before: Inst, data: InstData, ty: Type, uses: &mut Vec<u32>) -> Value {
    let span = func.span(before);
    let inst = func.create_inst(data, &[ty], span);
    func.insert_before(inst, before);
    uses.resize(func.counts().values, 0);
    func[inst].first_result.expect("one result was asked for")
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{Block, Builder, Flags, Func, Inst, IntPred, Opcode, Signature, Type, Value};

    use crate::narrow::Narrow;
    use crate::{Analyses, Fuel, Pass};

    /// A function with one block, ready to have instructions appended to it.
    fn blank() -> (Func, Block) {
        let mut names = Interner::new();
        let name = names.intern("f");
        let mut func = Func::new(name, Signature::new().with_returns(&[Type::int(32)]));
        let block = func.create_block();
        (func, block)
    }

    /// The opcode and the operand types of the instruction that produced a value.
    fn shape(func: &Func, value: Value) -> (Opcode, Vec<Type>) {
        let rucc_ir::Def::Result { inst, .. } = func[value].def else { panic!("a result") };
        let data = &func[inst];
        (data.opcode, func[data.args].iter().map(|&arg| func[arg].ty).collect())
    }

    /// How many instructions are in a block.
    fn left(func: &Func, block: Block) -> usize {
        func.insts(block).count()
    }

    /// The last instruction of a block, which is the one every test here returns from.
    fn last(func: &Func, block: Block) -> Inst {
        func.insts(block).last().expect("a block with something in it")
    }

    #[test]
    fn a_truncated_sum_of_two_extensions_is_the_sum_at_the_narrow_width() {
        let (mut func, block) = blank();
        let a = func.append_param(block, Type::int(8));
        let b = func.append_param(block, Type::int(8));
        let mut build = Builder::new(&mut func, block);
        let wide_a = build.unary(Opcode::SExt, a, Type::int(32));
        let wide_b = build.unary(Opcode::SExt, b, Type::int(32));
        let sum = build.binary(Opcode::Add, wide_a, wide_b, Flags::NONE);
        let narrow = build.unary(Opcode::Trunc, sum, Type::int(8));
        build.ret(&[narrow]);
        assert!(Narrow.run(&mut func, &mut Analyses::new(), &mut Fuel::unlimited()).changed());
        assert_eq!(shape(&func, narrow), (Opcode::Add, vec![Type::int(8), Type::int(8)]));
        // Nothing new was written. The two extensions and the wide add are still there, read by
        // nothing, which is what dead code elimination takes out after this.
        assert_eq!(left(&func, block), 5);
    }

    #[test]
    fn a_constant_operand_is_written_down_again_at_the_narrow_width() {
        let (mut func, block) = blank();
        let a = func.append_param(block, Type::int(8));
        let mut build = Builder::new(&mut func, block);
        let wide = build.unary(Opcode::SExt, a, Type::int(32));
        let one = build.iconst(Type::int(32), 1);
        let sum = build.binary(Opcode::Add, wide, one, Flags::NONE);
        let narrow = build.unary(Opcode::Trunc, sum, Type::int(8));
        build.ret(&[narrow]);
        assert!(Narrow.run(&mut func, &mut Analyses::new(), &mut Fuel::unlimited()).changed());
        assert_eq!(shape(&func, narrow), (Opcode::Add, vec![Type::int(8), Type::int(8)]));
    }

    #[test]
    fn a_chain_of_arithmetic_narrows_the_whole_way_down() {
        let (mut func, block) = blank();
        let a = func.append_param(block, Type::int(8));
        let b = func.append_param(block, Type::int(8));
        let c = func.append_param(block, Type::int(8));
        let mut build = Builder::new(&mut func, block);
        let wide_a = build.unary(Opcode::SExt, a, Type::int(32));
        let wide_b = build.unary(Opcode::SExt, b, Type::int(32));
        let wide_c = build.unary(Opcode::SExt, c, Type::int(32));
        let inner = build.binary(Opcode::Add, wide_a, wide_b, Flags::NONE);
        let outer = build.binary(Opcode::Mul, inner, wide_c, Flags::NONE);
        let narrow = build.unary(Opcode::Trunc, outer, Type::int(8));
        build.ret(&[narrow]);
        assert!(Narrow.run(&mut func, &mut Analyses::new(), &mut Fuel::unlimited()).changed());
        // The outer operation is the truncation rewritten, and the inner one is a new instruction
        // written in front of it, which is the recursive case and the reason a plan is a tree.
        assert_eq!(shape(&func, narrow), (Opcode::Mul, vec![Type::int(8), Type::int(8)]));
        assert_eq!(left(&func, block), 8);
    }

    #[test]
    fn an_operation_something_else_reads_stays_wide() {
        let (mut func, block) = blank();
        let a = func.append_param(block, Type::int(8));
        let b = func.append_param(block, Type::int(8));
        let mut build = Builder::new(&mut func, block);
        let wide_a = build.unary(Opcode::SExt, a, Type::int(32));
        let wide_b = build.unary(Opcode::SExt, b, Type::int(32));
        let sum = build.binary(Opcode::Add, wide_a, wide_b, Flags::NONE);
        let narrow = build.unary(Opcode::Trunc, sum, Type::int(8));
        let kept = build.unary(Opcode::SExt, narrow, Type::int(32));
        build.ret(&[sum, kept]);
        assert!(!Narrow.run(&mut func, &mut Analyses::new(), &mut Fuel::unlimited()).changed());
        // The wide sum is read by the return as well as by the truncation, so narrowing would add
        // an instruction rather than replace one.
        assert_eq!(shape(&func, narrow), (Opcode::Trunc, vec![Type::int(32)]));
    }

    #[test]
    fn a_divide_stays_wide_because_the_narrow_one_can_raise() {
        let (mut func, block) = blank();
        let a = func.append_param(block, Type::int(8));
        let b = func.append_param(block, Type::int(8));
        let mut build = Builder::new(&mut func, block);
        let wide_a = build.unary(Opcode::SExt, a, Type::int(32));
        let wide_b = build.unary(Opcode::SExt, b, Type::int(32));
        let quotient = build.binary(Opcode::SDiv, wide_a, wide_b, Flags::NONE);
        let narrow = build.unary(Opcode::Trunc, quotient, Type::int(8));
        build.ret(&[narrow]);
        assert!(!Narrow.run(&mut func, &mut Analyses::new(), &mut Fuel::unlimited()).changed());
        // The most negative byte over minus one is a hundred and twenty eight at four bytes and
        // is the overflow that raises at one, so this is the rewrite that would turn a working
        // program into one that dies.
        assert_eq!(shape(&func, narrow), (Opcode::Trunc, vec![Type::int(32)]));
    }

    #[test]
    fn a_shift_by_a_constant_below_the_width_narrows_and_one_at_it_does_not() {
        for (by, narrows) in [(3, true), (20, false)] {
            let (mut func, block) = blank();
            let a = func.append_param(block, Type::int(8));
            let mut build = Builder::new(&mut func, block);
            let wide = build.unary(Opcode::SExt, a, Type::int(32));
            let count = build.iconst(Type::int(32), by);
            let shifted = build.binary(Opcode::Shl, wide, count, Flags::NONE);
            let narrow = build.unary(Opcode::Trunc, shifted, Type::int(8));
            build.ret(&[narrow]);
            assert_eq!(
                Narrow.run(&mut func, &mut Analyses::new(), &mut Fuel::unlimited()).changed(),
                narrows,
                "shift by {by}"
            );
            // A count of twenty is a defined shift to zero at four bytes and is poison at one, so
            // narrowing it would be inventing undefined behaviour rather than removing a widening.
            let want = if narrows { Opcode::Shl } else { Opcode::Trunc };
            assert_eq!(shape(&func, narrow).0, want, "shift by {by}");
        }
    }

    #[test]
    fn a_shift_by_a_value_stays_wide() {
        let (mut func, block) = blank();
        let a = func.append_param(block, Type::int(8));
        let n = func.append_param(block, Type::int(8));
        let mut build = Builder::new(&mut func, block);
        let wide = build.unary(Opcode::SExt, a, Type::int(32));
        let by = build.unary(Opcode::SExt, n, Type::int(32));
        let shifted = build.binary(Opcode::Shl, wide, by, Flags::NONE);
        let narrow = build.unary(Opcode::Trunc, shifted, Type::int(8));
        build.ret(&[narrow]);
        assert!(!Narrow.run(&mut func, &mut Analyses::new(), &mut Fuel::unlimited()).changed());
        assert_eq!(shape(&func, narrow).0, Opcode::Trunc);
    }

    #[test]
    fn a_comparison_of_two_sign_extensions_is_the_comparison_of_what_they_extended() {
        for pred in IntPred::all() {
            let (mut func, block) = blank();
            let a = func.append_param(block, Type::int(8));
            let b = func.append_param(block, Type::int(8));
            let mut build = Builder::new(&mut func, block);
            let wide_a = build.unary(Opcode::SExt, a, Type::int(32));
            let wide_b = build.unary(Opcode::SExt, b, Type::int(32));
            let answer = build.icmp(pred, wide_a, wide_b);
            build.ret(&[answer]);
            assert!(
                Narrow.run(&mut func, &mut Analyses::new(), &mut Fuel::unlimited()).changed(),
                "{pred}"
            );
            // Every predicate, because sign extension keeps the order of what it extends under
            // the signed reading and under the unsigned one.
            assert_eq!(shape(&func, answer).1, vec![Type::int(8), Type::int(8)], "{pred}");
        }
    }

    #[test]
    fn a_comparison_of_two_zero_extensions_narrows_at_every_predicate_but_the_signed_ones() {
        for pred in IntPred::all() {
            let (mut func, block) = blank();
            let a = func.append_param(block, Type::int(8));
            let b = func.append_param(block, Type::int(8));
            let mut build = Builder::new(&mut func, block);
            let wide_a = build.unary(Opcode::ZExt, a, Type::int(32));
            let wide_b = build.unary(Opcode::ZExt, b, Type::int(32));
            let answer = build.icmp(pred, wide_a, wide_b);
            build.ret(&[answer]);
            // Zero extension takes a negative byte to a positive word, so the signed order is not
            // the order it came from and the four signed predicates do not survive it.
            assert_eq!(
                Narrow.run(&mut func, &mut Analyses::new(), &mut Fuel::unlimited()).changed(),
                !pred.is_signed(),
                "{pred}"
            );
        }
    }

    #[test]
    fn a_comparison_against_a_constant_narrows_when_the_constant_is_one_of_the_narrow_ones() {
        for (k, narrows) in [(120, true), (-1, true), (200, false)] {
            let (mut func, block) = blank();
            let a = func.append_param(block, Type::int(8));
            let mut build = Builder::new(&mut func, block);
            let wide = build.unary(Opcode::SExt, a, Type::int(32));
            let k = build.iconst(Type::int(32), k);
            let answer = build.icmp(IntPred::Eq, wide, k);
            build.ret(&[answer]);
            // Two hundred is not the sign extension of any byte, so the comparison is already
            // decided and saying so is folding's job rather than this pass's.
            assert_eq!(
                Narrow.run(&mut func, &mut Analyses::new(), &mut Fuel::unlimited()).changed(),
                narrows
            );
        }
    }

    #[test]
    fn one_extension_against_the_other_kind_is_not_a_comparison_at_the_narrow_width() {
        // `(signed char) a < b` with `b` an `unsigned char`, which is `tamnd/rucc#375`'s one
        // wrong answer over the torture suite: sixteen is less than a hundred and ninety five at
        // four bytes and is not less than minus sixty one at one, and neither is the byte
        // comparison the other reading would give.
        for pred in IntPred::all() {
            let (mut func, block) = blank();
            let a = func.append_param(block, Type::int(8));
            let b = func.append_param(block, Type::int(8));
            let mut build = Builder::new(&mut func, block);
            let wide_a = build.unary(Opcode::SExt, a, Type::int(32));
            let wide_b = build.unary(Opcode::ZExt, b, Type::int(32));
            let answer = build.icmp(pred, wide_a, wide_b);
            build.ret(&[answer]);
            assert!(
                !Narrow.run(&mut func, &mut Analyses::new(), &mut Fuel::unlimited()).changed(),
                "{pred}"
            );
        }
    }

    #[test]
    fn a_truth_is_not_a_width_to_narrow_to() {
        // `!c != 0`, which is a comparison of a widened truth against a zero that survives the
        // widening, so the argument narrows it the whole way to one bit. The answer would be
        // right and no target lowers a one bit comparison, which is `tamnd/rucc#352`.
        let (mut func, block) = blank();
        let a = func.append_param(block, Type::int(1));
        let mut build = Builder::new(&mut func, block);
        let wide = build.unary(Opcode::ZExt, a, Type::int(32));
        let zero = build.iconst(Type::int(32), 0);
        let answer = build.icmp(IntPred::Ne, wide, zero);
        build.ret(&[answer]);
        assert!(!Narrow.run(&mut func, &mut Analyses::new(), &mut Fuel::unlimited()).changed());
        assert_eq!(shape(&func, answer).1, vec![Type::int(32), Type::int(32)]);
    }

    #[test]
    fn extensions_from_different_widths_are_not_a_comparison_at_either_of_them() {
        let (mut func, block) = blank();
        let a = func.append_param(block, Type::int(8));
        let b = func.append_param(block, Type::int(16));
        let mut build = Builder::new(&mut func, block);
        let wide_a = build.unary(Opcode::SExt, a, Type::int(32));
        let wide_b = build.unary(Opcode::SExt, b, Type::int(32));
        let answer = build.icmp(IntPred::Slt, wide_a, wide_b);
        build.ret(&[answer]);
        assert!(!Narrow.run(&mut func, &mut Analyses::new(), &mut Fuel::unlimited()).changed());
    }

    #[test]
    fn the_overflow_flags_do_not_come_along() {
        let (mut func, block) = blank();
        let a = func.append_param(block, Type::int(8));
        let b = func.append_param(block, Type::int(8));
        let mut build = Builder::new(&mut func, block);
        let wide_a = build.unary(Opcode::SExt, a, Type::int(32));
        let wide_b = build.unary(Opcode::SExt, b, Type::int(32));
        let sum = build.binary(Opcode::Add, wide_a, wide_b, Flags::NSW);
        let narrow = build.unary(Opcode::Trunc, sum, Type::int(8));
        build.ret(&[narrow]);
        assert!(Narrow.run(&mut func, &mut Analyses::new(), &mut Fuel::unlimited()).changed());
        // A sum of two bytes that cannot overflow four bytes can overflow one, so a promise made
        // about the wide operation is not a promise about the narrow one.
        let rucc_ir::Def::Result { inst, .. } = func[narrow].def else { panic!("a result") };
        assert_eq!(func[inst].flags, Flags::NONE);
    }

    #[test]
    fn fuel_stops_the_narrowing_and_not_the_looking() {
        let (mut func, block) = blank();
        let a = func.append_param(block, Type::int(8));
        let b = func.append_param(block, Type::int(8));
        let mut build = Builder::new(&mut func, block);
        let wide_a = build.unary(Opcode::SExt, a, Type::int(32));
        let wide_b = build.unary(Opcode::SExt, b, Type::int(32));
        let first = build.icmp(IntPred::Slt, wide_a, wide_b);
        let second = build.icmp(IntPred::Sgt, wide_a, wide_b);
        build.ret(&[first, second]);
        let mut fuel = Fuel::of(1);
        assert!(Narrow.run(&mut func, &mut Analyses::new(), &mut fuel).changed());
        assert_eq!(shape(&func, first).1, vec![Type::int(8), Type::int(8)]);
        assert_eq!(shape(&func, second).1, vec![Type::int(32), Type::int(32)]);
    }

    #[test]
    fn a_block_that_narrows_nothing_is_left_exactly_as_it_was() {
        let (mut func, block) = blank();
        let a = func.append_param(block, Type::int(32));
        let mut build = Builder::new(&mut func, block);
        let sum = build.binary(Opcode::Add, a, a, Flags::NONE);
        build.ret(&[sum]);
        assert!(!Narrow.run(&mut func, &mut Analyses::new(), &mut Fuel::unlimited()).changed());
        assert_eq!(left(&func, block), 2);
        assert_eq!(func[last(&func, block)].opcode, Opcode::Return);
    }
}
