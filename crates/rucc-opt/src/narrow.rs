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
//! # The three shapes
//!
//! A truncation of arithmetic. The low bits of a sum, a difference, a product, a bitwise
//! operation or a shift by a constant depend only on the low bits of what went into it, so
//! `trunc.i8 (add.i32 (sext a) (sext b))` is `add.i8 a b` and the two extensions are left with
//! nothing reading them. That is the arithmetic half, and it is what `char c = a + b;` is.
//!
//! A comparison of extensions. Sign extension is an order isomorphism onto its image under both
//! readings of the bits, so a comparison of two of them at any predicate is the same comparison of
//! what they extended. That is what `char a, b; a < b` is. Zero extension is an isomorphism under
//! the unsigned reading and is not one under the signed reading, since it takes a negative byte to
//! a positive word, so the equalities and the unsigned predicates come over as they are. A signed
//! predicate comes over as its unsigned counterpart, because what a zero extension produces has
//! its top bits clear and the two readings agree on a value like that. That is what `unsigned char
//! a, b; a < b` is, and the promotions make it the shape most C at these widths has.
//!
//! Both are written so that one side may be a constant instead, because `if (c == 'x')` is the
//! common case and the constant is representable at the narrow width whenever the comparison is
//! not already decided.
//!
//! A bitwise operation on widened bits. That narrows all the way to one bit, which the other two
//! shapes stop short of on purpose. `and`, `or` and `xor` work a bit at a time, so over two values
//! a zero extension from one bit produced, which are zero or one and nothing else, the wide result
//! is zero or one as well and the whole of it is its own bottom bit. That bit is the operation done
//! on the two bits themselves. Two things ask for it. A comparison against zero at the `ne`
//! predicate wants it as a truth, which is what `_Bool r = p & q;` is, the comparison rather than a
//! truncation being the standard speaking: a conversion to `_Bool` gives zero or one according to
//! whether the value compares equal to zero. An extension wants it back as a number of its own
//! width, which is what `(long long)(p & q)` is, and since the bits are zero or one a sign
//! extension of them and a zero extension of them are the same value. This is the shape that gives
//! the one bit rewrite rules something to match, which is `tamnd/rucc#518`. Here too one side may
//! be a constant, and here a constant is a bit when it is zero or one.
//!
//! # Why it always pays
//!
//! No shape is applied unless every leaf it reaches narrows for nothing. A leaf is what an
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

use rucc_ir::{
    Block, Def, Extra, Flags, Func, Imm, Inst, InstData, IntPred, Opcode, Type, Value, ValueList,
};

use crate::uses::count;
use crate::{Analyses, Analysis, Fuel, Pass, Preserved, Stats};

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
        // not something the graph, the trees or the forest have an opinion about. Liveness is
        // another matter: the narrow arithmetic is new values, and the wide values it was
        // written from are read in one fewer place or in none.
        Preserved::ALL.without(Analysis::Liveness)
    }

    fn run(&self, func: &mut Func, _an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        let mut uses = count(func);
        for block in func.blocks().collect::<Vec<Block>>() {
            for inst in func.insts(block).collect::<Vec<Inst>>() {
                let Some(redo) = truncated_arithmetic(func, inst, &uses)
                    .or_else(|| extended_comparison(func, inst))
                    .or_else(|| widened_bits(func, inst, &uses))
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
    /// The left operand, or the only one when the instruction written takes one.
    lhs: Plan,
    /// The right operand, and nothing when the instruction written takes one.
    rhs: Option<Plan>,
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
///
/// That list is also why `widened_bits` is allowed below the floor and asks this nothing. What it
/// writes is one of the three operations the list has, at the one width they are on it for.
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
    Some(Redo { opcode: data.opcode, extra: Extra::None, ty, lhs, rhs: Some(rhs) })
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
/// predicate survives it and the comparison narrows as it stands.
///
/// Zero extension keeps the unsigned order and not the signed one, since it takes a negative byte
/// to a positive word. That does not stop a signed comparison of two of them narrowing: what a
/// zero extension produces is a value with its top bits clear, the two readings of the bits agree
/// on a value like that, and so the signed comparison is asking an unsigned question. It narrows
/// to the unsigned predicate rather than to the one that was written. This is the shape the
/// integer promotions give `unsigned char a, b; a < b`, which is a signed comparison of two zero
/// extensions and is most of what C produces at these widths, so refusing it would leave the rule
/// set's narrow half with nothing to match. The swap is asked for on an extension that widens,
/// because one to the width it already has is the identity and the predicate written on it is the
/// one that holds.
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
    let widens = ty.bits() < func[left].ty.bits();
    let pred = if kind == Opcode::ZExt && widens { pred.unsigned() } else { pred };
    let rhs = match widening(func, right) {
        Some((same, from, other)) if same == kind && from == ty => Plan::Already(other),
        _ => Plan::Constant(survives(func, right, kind, ty)?),
    };
    let extra = Extra::IntPred(pred);
    Some(Redo { opcode: Opcode::ICmp, extra, ty, lhs: Plan::Already(narrow), rhs: Some(rhs) })
}

/// Whether this is something asking about a bitwise operation on widened bits, and what it
/// becomes.
///
/// A value a zero extension from one bit produced is zero or one and nothing else, and `and`, `or`
/// and `xor` of two such values are again zero or one, because each works a bit at a time and
/// every bit above the bottom of both operands is clear. So the whole wide result is its own
/// bottom bit, and that bit is the operation done on the two bits themselves.
///
/// One side may be a constant instead, the way it may in the other two shapes, and here it has to
/// be zero or one, since that is what being a bit is.
///
/// This is the shape that gives the one bit rewrite rules a producer. Nothing in the front end
/// emits an `and.i1`, so `tamnd/rucc#518` is thirteen rules that no program could reach, and the
/// reason is that C has no way of writing one: every bitwise operator promotes its operands to
/// `int` first. That makes it the one narrowing whose payoff is not in the instruction it saves.
fn widened_bits(func: &Func, inst: Inst, uses: &[u32]) -> Option<Redo> {
    let (wide, back) = asked(func, inst, uses)?;
    let data = &func[wide];
    if !bit_at_a_time(data.opcode) {
        return None;
    }
    let args = &func[data.args];
    let (&left, &right) = (args.first()?, args.get(1)?);
    // An operand the operation reads twice is read twice by it and by nothing else, which is the
    // same fact about the subtree as an operand it reads once being read by nothing else. `_Bool
    // r = p & p;` is that shape, and it is one of the thirteen rules waiting for a producer.
    let readers = if left == right { 2 } else { 1 };
    let lhs = side(func, left, uses, readers)?;
    let rhs = side(func, right, uses, readers)?;
    let extra = Extra::None;
    let bit = Redo { opcode: data.opcode, extra, ty: Type::int(1), lhs, rhs: Some(rhs) };
    let Some(ty) = back else { return Some(bit) };
    let lhs = Plan::Nested(Box::new(bit));
    Some(Redo { opcode: Opcode::ZExt, extra, ty, lhs, rhs: None })
}

/// The wide operation an instruction is asking about, and the width the answer is wanted at.
///
/// Two instructions ask. A comparison against zero at the `ne` predicate wants the answer as a
/// truth, so the width it is wanted at is the one bit the operation is redone at and there is
/// nothing to say. `_Bool r = p & q;` is that: C computes the `and` at `int` because the
/// promotions say so, and the conversion of the result back to `_Bool` is a comparison against
/// zero rather than a truncation, because the standard says a conversion to `_Bool` gives zero or
/// one according to whether the value compares equal to zero.
///
/// Only the `ne` predicate. Asking whether the wide result is zero is the negation of this, and a
/// negation is a second instruction where every other shape here writes one.
///
/// An extension wants the answer back at its own width, which is the shape `(long long)(p & q)`
/// and every other use of the result as a number wider than the `int` the promotions computed it
/// at. The bits are zero or one either way, so a sign extension of them is the same value as a
/// zero extension of them and both come out as a zero extension from the one bit. That is the pass
/// writing an opcode other than the one it read, which it otherwise refuses to do, and it is
/// allowed here because the operation being rewritten is the extension rather than the bitwise
/// operation, and what an extension does is decided by what it extends.
fn asked(func: &Func, inst: Inst, uses: &[u32]) -> Option<(Inst, Option<Type>)> {
    let data = &func[inst];
    let args = &func[data.args];
    match data.opcode {
        Opcode::ICmp if data.extra == Extra::IntPred(IntPred::Ne) => {
            let (&left, &right) = (args.first()?, args.get(1)?);
            let (zero, wide) = constant(func, right)?;
            (zero.signed(wide) == 0).then_some((read_by(func, left, uses, 1)?, None))
        }
        Opcode::ZExt | Opcode::SExt => {
            let ty = func[data.results().next()?].ty;
            Some((read_by(func, *args.first()?, uses, 1)?, Some(ty)))
        }
        _ => None,
    }
}

/// What one operand of that operation is at one bit, or nothing when it is not a bit.
///
/// A constant is a bit when it is zero or one, and a constant with anything set above the bottom
/// bit is refused for the reason the whole rewrite rests on: the wide result would then be able to
/// come out nonzero with its bottom bit clear, and the nonzero question would be asking about bits
/// the narrow operation does not have. How many readers the constant has is not asked, because a
/// constant is written down again rather than kept alive.
fn side(func: &Func, value: Value, uses: &[u32], readers: u32) -> Option<Plan> {
    if let Some((imm, wide)) = constant(func, value) {
        let k = imm.signed(wide);
        return (k == 0 || k == 1).then_some(Plan::Constant(k));
    }
    Some(Plan::Already(widened_bit(func, value, uses, readers)?))
}

/// The instruction that computed this value, when the readers it has are the ones expected.
///
/// A reader beyond those keeps the wide subtree alive, and then the rewrite is an instruction
/// added rather than a subtree replaced, which is the one thing the profitability argument here
/// does not allow.
fn read_by(func: &Func, value: Value, uses: &[u32], readers: u32) -> Option<Inst> {
    if uses[value.index()] != readers {
        return None;
    }
    let Def::Result { inst, .. } = func[value].def else { return None };
    Some(inst)
}

/// Whether an operation works a bit at a time, so that its result at one bit is its result over
/// the bottom bit of what went in.
///
/// The three that do. An `add` of two widened bits is nonzero exactly when their `or` is and a
/// `mul` of two exactly when their `and` is, and neither is here, because both would be this pass
/// writing an opcode other than the one it read and that is a different claim from the one above.
const fn bit_at_a_time(opcode: Opcode) -> bool {
    matches!(opcode, Opcode::And | Opcode::Or | Opcode::Xor)
}

/// The one bit value this operand is the zero extension of, when that is what it is.
///
/// A zero extension and not a sign extension. A sign extension from one bit gives zero or minus
/// one, so the operation over two of them is again zero or minus one, and the answer to the
/// nonzero question is still the bottom bit, so the rewrite would hold. Nothing produces one: a
/// one bit value in this IR is what a comparison answers and the front end widens it with a zero
/// extension every time, which is what the language says, since a `_Bool` converted to `int` is
/// zero or one.
fn widened_bit(func: &Func, value: Value, uses: &[u32], readers: u32) -> Option<Value> {
    let inst = read_by(func, value, uses, readers)?;
    let data = &func[inst];
    if data.opcode != Opcode::ZExt {
        return None;
    }
    let narrow = *func[data.args].first()?;
    (func[narrow].ty == Type::int(1)).then_some(narrow)
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
    let operands = operands(func, inst, redo, uses);
    for value in func[func[inst].args].iter().copied() {
        uses[value.index()] -= 1;
    }
    let args = listed(func, operands, uses);
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
            let operands = operands(func, before, redo, uses);
            let args = listed(func, operands, uses);
            let data = InstData { args, extra: redo.extra, ..InstData::new(redo.opcode) };
            written(func, before, data, redo.ty, uses)
        }
    }
}

/// The values the plan's operands come to, written in front of the instruction if they are new.
fn operands(
    func: &mut Func,
    before: Inst,
    redo: &Redo,
    uses: &mut Vec<u32>,
) -> (Value, Option<Value>) {
    let lhs = build(func, before, redo.ty, &redo.lhs, uses);
    let rhs = redo.rhs.as_ref().map(|plan| build(func, before, redo.ty, plan, uses));
    (lhs, rhs)
}

/// Hands back the operand list to put on an instruction, counting each one as read.
fn listed(func: &mut Func, (lhs, rhs): (Value, Option<Value>), uses: &mut [u32]) -> ValueList {
    uses[lhs.index()] += 1;
    let Some(rhs) = rhs else { return func.push_values(&[lhs]) };
    uses[rhs.index()] += 1;
    func.push_values(&[lhs, rhs])
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
    use crate::{Fuel, Pass};

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

    /// The first operand of the instruction that produced a value.
    fn under(func: &Func, value: Value) -> Value {
        let rucc_ir::Def::Result { inst, .. } = func[value].def else { panic!("a result") };
        *func[func[inst].args].first().expect("an operand")
    }

    /// The predicate of the comparison this value is the answer to.
    fn predicate(func: &Func, value: Value) -> IntPred {
        let rucc_ir::Def::Result { inst, .. } = func[value].def else { panic!("a result") };
        let rucc_ir::Extra::IntPred(pred) = func[inst].extra else { panic!("a comparison") };
        pred
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
        assert!(
            Narrow
                .run(&mut func, &mut crate::machine::fixtures::analyses(), &mut Fuel::unlimited())
                .changed()
        );
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
        assert!(
            Narrow
                .run(&mut func, &mut crate::machine::fixtures::analyses(), &mut Fuel::unlimited())
                .changed()
        );
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
        assert!(
            Narrow
                .run(&mut func, &mut crate::machine::fixtures::analyses(), &mut Fuel::unlimited())
                .changed()
        );
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
        assert!(
            !Narrow
                .run(&mut func, &mut crate::machine::fixtures::analyses(), &mut Fuel::unlimited())
                .changed()
        );
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
        assert!(
            !Narrow
                .run(&mut func, &mut crate::machine::fixtures::analyses(), &mut Fuel::unlimited())
                .changed()
        );
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
                Narrow
                    .run(
                        &mut func,
                        &mut crate::machine::fixtures::analyses(),
                        &mut Fuel::unlimited()
                    )
                    .changed(),
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
        assert!(
            !Narrow
                .run(&mut func, &mut crate::machine::fixtures::analyses(), &mut Fuel::unlimited())
                .changed()
        );
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
                Narrow
                    .run(
                        &mut func,
                        &mut crate::machine::fixtures::analyses(),
                        &mut Fuel::unlimited()
                    )
                    .changed(),
                "{pred}"
            );
            // Every predicate, because sign extension keeps the order of what it extends under
            // the signed reading and under the unsigned one.
            assert_eq!(shape(&func, answer).1, vec![Type::int(8), Type::int(8)], "{pred}");
        }
    }

    #[test]
    fn a_comparison_of_two_zero_extensions_narrows_at_every_predicate() {
        for pred in IntPred::all() {
            let (mut func, block) = blank();
            let a = func.append_param(block, Type::int(8));
            let b = func.append_param(block, Type::int(8));
            let mut build = Builder::new(&mut func, block);
            let wide_a = build.unary(Opcode::ZExt, a, Type::int(32));
            let wide_b = build.unary(Opcode::ZExt, b, Type::int(32));
            let answer = build.icmp(pred, wide_a, wide_b);
            build.ret(&[answer]);
            assert!(
                Narrow
                    .run(
                        &mut func,
                        &mut crate::machine::fixtures::analyses(),
                        &mut Fuel::unlimited()
                    )
                    .changed(),
                "{pred}"
            );
            assert_eq!(shape(&func, answer).1, vec![Type::int(8), Type::int(8)], "{pred}");
        }
    }

    #[test]
    fn a_signed_comparison_of_two_zero_extensions_narrows_to_the_unsigned_one() {
        // `unsigned char a, b; a < b`, which the promotions write as a signed comparison of two
        // zero extensions. Both sides have their top bits clear, where the two readings of the
        // bits agree, so the question the wide comparison asks is the unsigned one and that is
        // the predicate the narrow comparison is written with.
        for pred in IntPred::all() {
            let (mut func, block) = blank();
            let a = func.append_param(block, Type::int(8));
            let b = func.append_param(block, Type::int(8));
            let mut build = Builder::new(&mut func, block);
            let wide_a = build.unary(Opcode::ZExt, a, Type::int(32));
            let wide_b = build.unary(Opcode::ZExt, b, Type::int(32));
            let answer = build.icmp(pred, wide_a, wide_b);
            build.ret(&[answer]);
            Narrow.run(
                &mut func,
                &mut crate::machine::fixtures::analyses(),
                &mut Fuel::unlimited(),
            );
            assert_eq!(predicate(&func, answer), pred.unsigned(), "{pred}");
        }
    }

    #[test]
    fn a_signed_comparison_of_two_sign_extensions_keeps_the_predicate_it_was_written_with() {
        for pred in IntPred::all() {
            let (mut func, block) = blank();
            let a = func.append_param(block, Type::int(8));
            let b = func.append_param(block, Type::int(8));
            let mut build = Builder::new(&mut func, block);
            let wide_a = build.unary(Opcode::SExt, a, Type::int(32));
            let wide_b = build.unary(Opcode::SExt, b, Type::int(32));
            let answer = build.icmp(pred, wide_a, wide_b);
            build.ret(&[answer]);
            Narrow.run(
                &mut func,
                &mut crate::machine::fixtures::analyses(),
                &mut Fuel::unlimited(),
            );
            assert_eq!(predicate(&func, answer), pred, "{pred}");
        }
    }

    #[test]
    fn a_signed_comparison_of_a_zero_extension_against_a_constant_narrows_to_the_unsigned_one() {
        // `unsigned char a; a < 200`. Two hundred is the zero extension of a byte even though it
        // is not the sign extension of one, so the constant comes along and the comparison that
        // is left is the unsigned one against that byte.
        let (mut func, block) = blank();
        let a = func.append_param(block, Type::int(8));
        let mut build = Builder::new(&mut func, block);
        let wide = build.unary(Opcode::ZExt, a, Type::int(32));
        let k = build.iconst(Type::int(32), 200);
        let answer = build.icmp(IntPred::Slt, wide, k);
        build.ret(&[answer]);
        assert!(
            Narrow
                .run(&mut func, &mut crate::machine::fixtures::analyses(), &mut Fuel::unlimited())
                .changed()
        );
        assert_eq!(shape(&func, answer).1, vec![Type::int(8), Type::int(8)]);
        assert_eq!(predicate(&func, answer), IntPred::Ult);
    }

    #[test]
    fn a_signed_comparison_of_a_zero_extension_against_a_negative_constant_is_left_alone() {
        // Minus one is no byte's zero extension, so the comparison is already decided and saying
        // which way is folding's job. Narrowing it would compare a byte against minus one, which
        // is a different question under either reading.
        let (mut func, block) = blank();
        let a = func.append_param(block, Type::int(8));
        let mut build = Builder::new(&mut func, block);
        let wide = build.unary(Opcode::ZExt, a, Type::int(32));
        let k = build.iconst(Type::int(32), -1);
        let answer = build.icmp(IntPred::Sgt, wide, k);
        build.ret(&[answer]);
        assert!(
            !Narrow
                .run(&mut func, &mut crate::machine::fixtures::analyses(), &mut Fuel::unlimited())
                .changed()
        );
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
                Narrow
                    .run(
                        &mut func,
                        &mut crate::machine::fixtures::analyses(),
                        &mut Fuel::unlimited()
                    )
                    .changed(),
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
                !Narrow
                    .run(
                        &mut func,
                        &mut crate::machine::fixtures::analyses(),
                        &mut Fuel::unlimited()
                    )
                    .changed(),
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
        assert!(
            !Narrow
                .run(&mut func, &mut crate::machine::fixtures::analyses(), &mut Fuel::unlimited())
                .changed()
        );
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
        assert!(
            !Narrow
                .run(&mut func, &mut crate::machine::fixtures::analyses(), &mut Fuel::unlimited())
                .changed()
        );
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
        assert!(
            Narrow
                .run(&mut func, &mut crate::machine::fixtures::analyses(), &mut Fuel::unlimited())
                .changed()
        );
        // A sum of two bytes that cannot overflow four bytes can overflow one, so a promise made
        // about the wide operation is not a promise about the narrow one.
        let rucc_ir::Def::Result { inst, .. } = func[narrow].def else { panic!("a result") };
        assert_eq!(func[inst].flags, Flags::NONE);
    }

    /// `_Bool p, q; _Bool r = p & q;` and the same at the other two operators.
    ///
    /// The promotions widen both bits to an `int`, the operator runs there, and the conversion of
    /// the answer back to `_Bool` is the comparison against zero. All of that is the operator on
    /// the two bits.
    #[test]
    fn a_bitwise_operation_on_two_widened_bits_is_done_at_one_bit() {
        for opcode in [Opcode::And, Opcode::Or, Opcode::Xor] {
            let (mut func, block) = blank();
            let p = func.append_param(block, Type::int(1));
            let q = func.append_param(block, Type::int(1));
            let mut build = Builder::new(&mut func, block);
            let wide_p = build.unary(Opcode::ZExt, p, Type::int(32));
            let wide_q = build.unary(Opcode::ZExt, q, Type::int(32));
            let both = build.binary(opcode, wide_p, wide_q, Flags::NONE);
            let zero = build.iconst(Type::int(32), 0);
            let answer = build.icmp(IntPred::Ne, both, zero);
            build.ret(&[answer]);
            assert!(
                Narrow
                    .run(
                        &mut func,
                        &mut crate::machine::fixtures::analyses(),
                        &mut Fuel::unlimited()
                    )
                    .changed(),
                "{opcode:?}"
            );
            assert_eq!(shape(&func, answer), (opcode, vec![Type::int(1), Type::int(1)]));
        }
    }

    /// Asking whether it came out zero is the negation of asking whether it came out nonzero, and
    /// a negation is an instruction this pass has nowhere to put.
    #[test]
    fn asking_whether_a_bitwise_operation_on_widened_bits_is_zero_is_left_alone() {
        let (mut func, block) = blank();
        let p = func.append_param(block, Type::int(1));
        let q = func.append_param(block, Type::int(1));
        let mut build = Builder::new(&mut func, block);
        let wide_p = build.unary(Opcode::ZExt, p, Type::int(32));
        let wide_q = build.unary(Opcode::ZExt, q, Type::int(32));
        let both = build.binary(Opcode::And, wide_p, wide_q, Flags::NONE);
        let zero = build.iconst(Type::int(32), 0);
        let answer = build.icmp(IntPred::Eq, both, zero);
        build.ret(&[answer]);
        assert!(
            !Narrow
                .run(&mut func, &mut crate::machine::fixtures::analyses(), &mut Fuel::unlimited())
                .changed()
        );
        assert_eq!(shape(&func, answer).1, vec![Type::int(32), Type::int(32)]);
    }

    /// A sum of two widened bits is nonzero exactly when their `or` is, and that is a different
    /// claim from the one this makes, so it is not made here.
    #[test]
    fn a_sum_of_two_widened_bits_is_left_alone() {
        let (mut func, block) = blank();
        let p = func.append_param(block, Type::int(1));
        let q = func.append_param(block, Type::int(1));
        let mut build = Builder::new(&mut func, block);
        let wide_p = build.unary(Opcode::ZExt, p, Type::int(32));
        let wide_q = build.unary(Opcode::ZExt, q, Type::int(32));
        let both = build.binary(Opcode::Add, wide_p, wide_q, Flags::NONE);
        let zero = build.iconst(Type::int(32), 0);
        let answer = build.icmp(IntPred::Ne, both, zero);
        build.ret(&[answer]);
        assert!(
            !Narrow
                .run(&mut func, &mut crate::machine::fixtures::analyses(), &mut Fuel::unlimited())
                .changed()
        );
    }

    /// Two widened bytes, which are not zero or one, so the bottom bit of the `and` is not the
    /// answer to whether the whole of it is nonzero.
    #[test]
    fn a_bitwise_operation_on_something_wider_than_a_bit_is_not_this_shape() {
        let (mut func, block) = blank();
        let a = func.append_param(block, Type::int(8));
        let b = func.append_param(block, Type::int(8));
        let mut build = Builder::new(&mut func, block);
        let wide_a = build.unary(Opcode::ZExt, a, Type::int(32));
        let wide_b = build.unary(Opcode::ZExt, b, Type::int(32));
        let both = build.binary(Opcode::And, wide_a, wide_b, Flags::NONE);
        let zero = build.iconst(Type::int(32), 0);
        let answer = build.icmp(IntPred::Ne, both, zero);
        build.ret(&[answer]);
        Narrow.run(&mut func, &mut crate::machine::fixtures::analyses(), &mut Fuel::unlimited());
        // The truncated arithmetic shape does narrow the `and` to a byte, which is a different
        // rewrite and is why this asserts on the comparison rather than on nothing having moved.
        assert_eq!(shape(&func, answer).0, Opcode::ICmp);
    }

    /// `_Bool r = p & 1;` and the rest of the one bit table, which is the point of the shape.
    ///
    /// The constant comes over as the same constant at one bit, and then tier one has the rule
    /// that finishes it. Four of the thirteen are here, one per answer the table gives.
    #[test]
    fn a_bitwise_operation_on_a_widened_bit_and_a_bit_constant_is_done_at_one_bit() {
        for (opcode, k) in
            [(Opcode::And, 0), (Opcode::And, 1), (Opcode::Or, 0), (Opcode::Or, 1), (Opcode::Xor, 0)]
        {
            let (mut func, block) = blank();
            let p = func.append_param(block, Type::int(1));
            let mut build = Builder::new(&mut func, block);
            let wide_p = build.unary(Opcode::ZExt, p, Type::int(32));
            let bit = build.iconst(Type::int(32), k);
            let both = build.binary(opcode, wide_p, bit, Flags::NONE);
            let zero = build.iconst(Type::int(32), 0);
            let answer = build.icmp(IntPred::Ne, both, zero);
            build.ret(&[answer]);
            assert!(
                Narrow
                    .run(
                        &mut func,
                        &mut crate::machine::fixtures::analyses(),
                        &mut Fuel::unlimited()
                    )
                    .changed(),
                "{opcode:?} {k}"
            );
            assert_eq!(shape(&func, answer), (opcode, vec![Type::int(1), Type::int(1)]));
        }
    }

    /// A constant with a bit set above the bottom one, which is where the argument stops holding:
    /// the wide result can be nonzero with its bottom bit clear.
    #[test]
    fn a_bitwise_operation_against_a_constant_wider_than_a_bit_is_left_alone() {
        let (mut func, block) = blank();
        let p = func.append_param(block, Type::int(1));
        let mut build = Builder::new(&mut func, block);
        let wide_p = build.unary(Opcode::ZExt, p, Type::int(32));
        let two = build.iconst(Type::int(32), 2);
        let both = build.binary(Opcode::Or, wide_p, two, Flags::NONE);
        let zero = build.iconst(Type::int(32), 0);
        let answer = build.icmp(IntPred::Ne, both, zero);
        build.ret(&[answer]);
        assert!(
            !Narrow
                .run(&mut func, &mut crate::machine::fixtures::analyses(), &mut Fuel::unlimited())
                .changed()
        );
    }

    /// `_Bool r = p & p;`, where one widening is read twice by the operation above it and by
    /// nothing else, which is the same fact about the subtree as one reader is.
    #[test]
    fn a_widened_bit_the_operation_reads_twice_is_still_only_read_by_it() {
        let (mut func, block) = blank();
        let p = func.append_param(block, Type::int(1));
        let mut build = Builder::new(&mut func, block);
        let wide_p = build.unary(Opcode::ZExt, p, Type::int(32));
        let both = build.binary(Opcode::And, wide_p, wide_p, Flags::NONE);
        let zero = build.iconst(Type::int(32), 0);
        let answer = build.icmp(IntPred::Ne, both, zero);
        build.ret(&[answer]);
        assert!(
            Narrow
                .run(&mut func, &mut crate::machine::fixtures::analyses(), &mut Fuel::unlimited())
                .changed()
        );
        assert_eq!(shape(&func, answer), (Opcode::And, vec![Type::int(1), Type::int(1)]));
    }

    /// A widened bit something else reads as well, which keeps the widening alive, so the
    /// rewrite would be an instruction added rather than a subtree replaced.
    #[test]
    fn a_widened_bit_that_something_else_reads_is_left_alone() {
        let (mut func, block) = blank();
        let p = func.append_param(block, Type::int(1));
        let q = func.append_param(block, Type::int(1));
        let mut build = Builder::new(&mut func, block);
        let wide_p = build.unary(Opcode::ZExt, p, Type::int(32));
        let wide_q = build.unary(Opcode::ZExt, q, Type::int(32));
        let both = build.binary(Opcode::And, wide_p, wide_q, Flags::NONE);
        let zero = build.iconst(Type::int(32), 0);
        let answer = build.icmp(IntPred::Ne, both, zero);
        build.ret(&[answer, wide_p]);
        assert!(
            !Narrow
                .run(&mut func, &mut crate::machine::fixtures::analyses(), &mut Fuel::unlimited())
                .changed()
        );
    }

    /// Against something other than zero, which asks a question the bottom bit does not answer.
    #[test]
    fn a_bitwise_operation_on_widened_bits_compared_against_one_is_left_alone() {
        let (mut func, block) = blank();
        let p = func.append_param(block, Type::int(1));
        let q = func.append_param(block, Type::int(1));
        let mut build = Builder::new(&mut func, block);
        let wide_p = build.unary(Opcode::ZExt, p, Type::int(32));
        let wide_q = build.unary(Opcode::ZExt, q, Type::int(32));
        let both = build.binary(Opcode::Or, wide_p, wide_q, Flags::NONE);
        let one = build.iconst(Type::int(32), 1);
        let answer = build.icmp(IntPred::Ne, both, one);
        build.ret(&[answer]);
        assert!(
            !Narrow
                .run(&mut func, &mut crate::machine::fixtures::analyses(), &mut Fuel::unlimited())
                .changed()
        );
    }

    /// `(long long)(p & q)`, which asks for the result as a wider number rather than as a truth.
    ///
    /// The bits are zero or one, so the extension of what the operation came to is the extension
    /// of the one bit it came to, and a sign extension there is the same value as a zero one.
    #[test]
    fn a_bitwise_operation_on_widened_bits_taken_wider_is_done_at_one_bit() {
        for kind in [Opcode::ZExt, Opcode::SExt] {
            let (mut func, block) = blank();
            let p = func.append_param(block, Type::int(1));
            let q = func.append_param(block, Type::int(1));
            let mut build = Builder::new(&mut func, block);
            let wide_p = build.unary(Opcode::ZExt, p, Type::int(32));
            let wide_q = build.unary(Opcode::ZExt, q, Type::int(32));
            let both = build.binary(Opcode::And, wide_p, wide_q, Flags::NONE);
            let wider = build.unary(kind, both, Type::int(64));
            build.ret(&[wider]);
            assert!(
                Narrow
                    .run(
                        &mut func,
                        &mut crate::machine::fixtures::analyses(),
                        &mut Fuel::unlimited()
                    )
                    .changed(),
                "{kind:?}"
            );
            assert_eq!(shape(&func, wider), (Opcode::ZExt, vec![Type::int(1)]), "{kind:?}");
            let bit = under(&func, wider);
            let want = (Opcode::And, vec![Type::int(1), Type::int(1)]);
            assert_eq!(shape(&func, bit), want, "{kind:?}");
        }
    }

    /// `(long long)(p & 1)`, where the constant comes over at one bit the same as it does under a
    /// comparison, and then tier one has the rule that finishes it.
    #[test]
    fn a_bit_constant_comes_over_under_an_extension_too() {
        let (mut func, block) = blank();
        let p = func.append_param(block, Type::int(1));
        let mut build = Builder::new(&mut func, block);
        let wide_p = build.unary(Opcode::ZExt, p, Type::int(32));
        let one = build.iconst(Type::int(32), 1);
        let both = build.binary(Opcode::And, wide_p, one, Flags::NONE);
        let wider = build.unary(Opcode::SExt, both, Type::int(64));
        build.ret(&[wider]);
        assert!(
            Narrow
                .run(&mut func, &mut crate::machine::fixtures::analyses(), &mut Fuel::unlimited())
                .changed()
        );
        assert_eq!(shape(&func, wider), (Opcode::ZExt, vec![Type::int(1)]));
        assert_eq!(shape(&func, under(&func, wider)), (Opcode::And, vec![Type::int(1); 2]));
    }

    /// An extension of a bitwise operation on things wider than a bit, which is the ordinary
    /// promoted shape and has nothing to do with this.
    #[test]
    fn an_extension_of_a_bitwise_operation_on_bytes_is_left_alone() {
        let (mut func, block) = blank();
        let a = func.append_param(block, Type::int(8));
        let b = func.append_param(block, Type::int(8));
        let mut build = Builder::new(&mut func, block);
        let wide_a = build.unary(Opcode::ZExt, a, Type::int(32));
        let wide_b = build.unary(Opcode::ZExt, b, Type::int(32));
        let both = build.binary(Opcode::And, wide_a, wide_b, Flags::NONE);
        let wider = build.unary(Opcode::SExt, both, Type::int(64));
        build.ret(&[wider]);
        assert!(
            !Narrow
                .run(&mut func, &mut crate::machine::fixtures::analyses(), &mut Fuel::unlimited())
                .changed()
        );
    }

    /// An extension of a sum of two widened bits, which is zero, one or two, so the whole of it is
    /// not its own bottom bit and the operation the pass would write is not the one it read.
    #[test]
    fn an_extension_of_a_sum_of_two_widened_bits_is_left_alone() {
        let (mut func, block) = blank();
        let p = func.append_param(block, Type::int(1));
        let q = func.append_param(block, Type::int(1));
        let mut build = Builder::new(&mut func, block);
        let wide_p = build.unary(Opcode::ZExt, p, Type::int(32));
        let wide_q = build.unary(Opcode::ZExt, q, Type::int(32));
        let both = build.binary(Opcode::Add, wide_p, wide_q, Flags::NONE);
        let wider = build.unary(Opcode::SExt, both, Type::int(64));
        build.ret(&[wider]);
        assert!(
            !Narrow
                .run(&mut func, &mut crate::machine::fixtures::analyses(), &mut Fuel::unlimited())
                .changed()
        );
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
        assert!(
            Narrow.run(&mut func, &mut crate::machine::fixtures::analyses(), &mut fuel).changed()
        );
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
        assert!(
            !Narrow
                .run(&mut func, &mut crate::machine::fixtures::analyses(), &mut Fuel::unlimited())
                .changed()
        );
        assert_eq!(left(&func, block), 2);
        assert_eq!(func[last(&func, block)].opcode, Opcode::Return);
    }
}
