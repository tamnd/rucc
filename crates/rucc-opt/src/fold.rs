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
//! The bit counting instructions are here for a related reason. Nothing in the backend selects an
//! instruction for any of them yet, so each one that survives to the end becomes the twenty odd
//! instructions of the software expansion in `rucc_codegen::expand`, inlined at the site. A
//! `__builtin_clzll` on a value the compiler can already see is the worst version of that: the
//! answer is a number between nought and sixty four and the code that computes it is the largest
//! thing in the function. Folding it costs one arm here. That is part of issue 310.
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
//! Not floating point arithmetic. Folding it means deciding what rounding mode to fold under and
//! what to do about a signalling NaN, and `rucc_base::float` has the arithmetic but the decision
//! about the environment belongs with the rest of the floating point work rather than in the first
//! pass.
//!
//! Negation is folded, and is inside that boundary rather than an exception to it. 754 says a
//! negation flips the sign bit and copies every other bit, for every input including a NaN and a
//! zero, so it is exact, it raises nothing and it never consults the rounding mode: there is no
//! decision about the environment in it to get wrong. The reason to bother is that C has no
//! negative floating constant. Every one of them is a unary minus applied to a positive one, so
//! `-1.0` arrives as an `fneg` of an `fconst`, and without this the back end makes a constant, a
//! mask and three moves through a general register out of what should be one load. That is every
//! negative floating literal in every program, and it is issue 1427.
//!
//! A bitcast of a constant is folded for the same reason and pays for the same kind of code. It is
//! the same bits read as another type of the same width, so there is nothing to decide about it
//! either, and what it unblocks is `fabs` and `copysign` of a constant: neither is a call, the
//! front end lowers both to a mask over the bits, and without this the mask and the two bitcasts
//! around it survive to the back end computing a number the compiler already has.
//!
//! A conversion from floating point to an integer is folded, and is inside that boundary rather
//! than an exception to it. C says the conversion discards the fractional part, so the rounding is
//! the language's rather than the environment's and nothing anybody sets at run time reaches it.
//! What is left is a value whose truncation does not fit the destination type, and a NaN, and both
//! of those are undefined rather than a number: `rucc_base::float::Float::to_integer` reports each
//! as `Status::INVALID` and neither folds, which is the rule below for an add that overflows under
//! `nsw` applied to the same kind of program. That is issue 1357.
//!
//! Not an operation that overflows under `nsw` or `nuw`. The result there is poison, so any
//! answer would be a valid refinement, and quietly picking the wrapping one hides a program that
//! has stepped outside the language from the sanitizer that should be reporting it.
//!
//! Not floating point comparisons, for the reason above and one more: an ordered predicate and an
//! unordered one differ only on a NaN, so the answer is the whole of what makes them two
//! predicates, and evaluating it is the floating point decision rather than a step around it.
//!
//! Integer comparisons are folded, and were not until issue 352 was closed. An `icmp` produces an
//! `i1`, and while nothing lowered one that was left standing on its own, folding one would have
//! turned working code into code that does not build. There is now a rule for a one bit constant
//! and one for a byte holding it, so the constant this leaves behind lowers wherever the
//! comparison did.

use rucc_base::float::{Float, Status};
use rucc_ir::{Block, Def, Extra, Flags, Func, Imm, Inst, InstData, IntPred, Opcode, Type, Value};

use crate::{Analyses, Analysis, Fuel, Pass, Preserved, Stats};

/// Recorded once for each instruction that became a constant.
const FOLDED: &str = "instruction with constant operands folded to a constant";

/// Recorded for an instruction that would have folded if there had been fuel for it.
///
/// Not a missed optimization in the ordinary sense, since the fuel is a person deliberately
/// stopping the pass. It is here because it is the number a bisection is searching for: the count
/// of sites past the cut is how far there is left to go.
const NO_FUEL: &str = "instruction not folded, the pass ran out of fuel";

/// The pass. It holds nothing, because folding needs to know nothing beyond the instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fold;

impl Pass for Fold {
    fn name(&self) -> &'static str {
        "fold"
    }

    fn describe(&self) -> &'static str {
        "an instruction whose operands are all constants becomes a constant"
    }

    fn preserves(&self) -> Preserved {
        // An instruction becomes a constant where it stands. No block moves, no edge moves,
        // and a terminator is not one of the instructions this folds, so every analysis in the
        // cache is about the same graph afterwards as it was before. Not the liveness, though:
        // the operands the folded instruction read are read by nobody now, and a value whose
        // last reader went is live over less of the function than it was.
        Preserved::ALL.without(Analysis::Liveness)
    }

    fn run(&self, func: &mut Func, _an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        fold_in(func, fuel)
    }
}

/// The whole of the pass, without the analysis cache it does not read.
///
/// Apart so that [`crate::ipcp`] can fold a function it has just put a constant into. The
/// arithmetic a constant parameter enables is what makes that propagation reach a second level, and
/// running this rather than evaluating it there is what keeps the arithmetic written down once.
pub(crate) fn fold_in(func: &mut Func, fuel: &mut Fuel) -> Stats {
    let blocks: Vec<Block> = func.blocks().collect();
    let mut stats = Stats::new();
    for block in blocks {
        let insts: Vec<Inst> = func.insts(block).collect();
        for inst in insts {
            let Some(folded) = evaluate(func, inst) else { continue };
            if !fuel.take() {
                // Out of fuel, which is a request to stop transforming rather than to stop
                // looking. Continuing the walk costs nothing and keeps the count of what
                // could have been folded the same at every fuel setting, which is what makes
                // a bisection over it monotonic.
                stats.missed(NO_FUEL);
                continue;
            }
            let ty = func[result_of(func, inst)].ty;
            let at = func.add_imm(folded);
            let data = &mut func[inst];
            // Which constant instruction holds the answer is the result type's question and
            // not the folded instruction's. An `fneg` and a bitcast out of an integer both
            // answer in a floating point type and the rest of what folds here answers in an
            // integer one, and an immediate is the same bits either way.
            data.opcode = if ty.is_int() { Opcode::IConst } else { Opcode::FConst };
            data.flags = Flags::NONE;
            data.args = rucc_ir::ValueList::EMPTY;
            data.extra = Extra::Imm(at);
            stats.optimized(FOLDED);
        }
    }
    stats
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
    // A vector constant is a `splat` rather than an `iconst` or an `fconst`, so a vector fold
    // would have to build a different instruction and would have to be right about the lane
    // count as well.
    if !ty.is_scalar() {
        return None;
    }
    let args = &func[data.args];
    // The two that are the bits and nothing else, and the only two here whose answer can have a
    // floating point type. They are above the gate below rather than inside the match under it
    // because that gate is what keeps the rest of this file about integers.
    match data.opcode {
        Opcode::FNeg => return negated(func, *args.first()?, ty),
        Opcode::Bitcast => return reinterpreted(func, *args.first()?, ty),
        _ => {}
    }
    if !ty.is_int() {
        return None;
    }
    match data.opcode {
        Opcode::FPToSI | Opcode::FPToUI => {
            let value = floating(func, *args.first()?)?;
            to_integer(value, ty, data.opcode == Opcode::FPToSI)
        }
        _ => arithmetic(data, args, ty, &|value| constant(func, value)),
    }
}

/// What an instruction of integer arithmetic works out to, given a way to read each operand as a
/// constant.
///
/// The way to read an operand is the caller's, because this pass wants an operand that is already
/// a constant and nothing more, while [`evaluated`] wants to go on looking underneath one that is
/// not. Both of them want the arithmetic itself to be this one, so that the two cannot disagree
/// about what an instruction answers.
fn arithmetic(
    data: &InstData,
    args: &[Value],
    ty: Type,
    operand: &dyn Fn(Value) -> Option<(Imm, Type)>,
) -> Option<Imm> {
    match data.opcode {
        Opcode::Trunc | Opcode::SExt | Opcode::ZExt => {
            let (value, from) = operand(*args.first()?)?;
            Some(convert(data.opcode, value, from, ty))
        }
        Opcode::Shl | Opcode::LShr | Opcode::AShr => {
            let (value, from) = operand(*args.first()?)?;
            let (count, count_ty) = operand(*args.get(1)?)?;
            shift(data.opcode, value, from, count, count_ty, ty, data.flags)
        }
        Opcode::Add | Opcode::Sub | Opcode::Mul | Opcode::And | Opcode::Or | Opcode::Xor => {
            let (lhs, lhs_ty) = operand(*args.first()?)?;
            let (rhs, _) = operand(*args.get(1)?)?;
            binary(data.opcode, lhs, rhs, lhs_ty, ty, data.flags)
        }
        Opcode::Ctlz | Opcode::Cttz | Opcode::Ctpop | Opcode::Bswap | Opcode::Bitreverse => {
            let (value, from) = operand(*args.first()?)?;
            count(data.opcode, value, from, ty)
        }
        Opcode::ICmp => {
            let Extra::IntPred(pred) = data.extra else { return None };
            let (lhs, from) = operand(*args.first()?)?;
            let (rhs, _) = operand(*args.get(1)?)?;
            Some(Imm::int(i128::from(compare(pred, lhs, rhs, from)), ty))
        }
        // A select between two constants that are the same number is that number whichever way
        // the condition goes. Unrolling makes these, when both arms worked out something from a
        // counter that is now a constant and came to the same answer, and each arm's answer is
        // its own constant instruction, so nothing that compares values sees they are equal.
        Opcode::Select => {
            let (then, _) = operand(*args.get(1)?)?;
            let (other, _) = operand(*args.get(2)?)?;
            (then.signed(ty) == other.signed(ty)).then_some(then)
        }
        _ => None,
    }
}

/// The constant this value works out to, looking through as many as `depth` instructions of
/// integer arithmetic over constants.
///
/// For a pass that runs before this one has had the chance to write the answer down as a constant,
/// which is [`crate::libcall`]: it runs over the module before any function pass has started, so
/// an index the source wrote as `x & 3` with `x` known is still an `and` of two constants when it
/// looks. Nothing here changes the function, and the arithmetic is [`arithmetic`], so the answer
/// is the one this pass would have written later.
pub(crate) fn evaluated(func: &Func, value: Value, depth: u32) -> Option<(Imm, Type)> {
    if let Some(found) = constant(func, value) {
        return Some(found);
    }
    let next = depth.checked_sub(1)?;
    let Def::Result { inst, .. } = func[value].def else { return None };
    let data = &func[inst];
    let ty = func[value].ty;
    if data.results != 1 || !ty.is_int() || !ty.is_scalar() {
        return None;
    }
    let found = arithmetic(data, &func[data.args], ty, &|arg| evaluated(func, arg, next))?;
    Some((found, ty))
}

/// The bits a value holds, if it is a constant of either kind.
///
/// Both kinds, because the two rewrites above this are about the bits and do not care which of
/// them they were written as. A constant of either is one instruction with one immediate, and an
/// immediate is the bits.
fn bits_of(func: &Func, value: Value) -> Option<u128> {
    let Def::Result { inst, .. } = func[value].def else { return None };
    let data = &func[inst];
    if !matches!(data.opcode, Opcode::IConst | Opcode::FConst) {
        return None;
    }
    let Extra::Imm(at) = data.extra else { return None };
    Some(func[at].bits())
}

/// A negation of a floating point constant, which is that constant with its sign bit flipped.
///
/// This is the one piece of floating point arithmetic that folds, and it is inside the boundary
/// the file header draws rather than an exception to it. Negation is not arithmetic in the sense
/// that boundary is about: 754 says it flips the sign bit and copies every other bit, for every
/// input including a NaN and a zero, so it is exact, it raises nothing and it never consults the
/// rounding mode. There is no decision about the environment to get wrong.
///
/// The reason to bother is that C has no negative floating constant. Every one of them is a unary
/// minus applied to a positive one, so `-1.0` arrives here as an `fneg` of an `fconst` and stays
/// that way, and what the back end makes of it is a constant, a mask and three moves through a
/// general register where one load would do. That is every negative floating literal in every
/// program, and it is issue 1427.
///
/// The sign bit is the top bit of the value and not of the object it is stored in. An `f80` is
/// eighty bits of value in a hundred and twenty eight of storage, and [`Type::bits`] answers
/// eighty for it, which is the bit this has to flip.
fn negated(func: &Func, operand: Value, ty: Type) -> Option<Imm> {
    if !ty.is_float() {
        return None;
    }
    let bits = bits_of(func, operand)?;
    Some(Imm::from_bits(bits ^ 1u128 << (ty.bits() - 1)))
}

/// A bitcast of a constant, which is the same bits read as another type of the same width.
///
/// It folds in both directions, and the one that pays is out of an integer, because that is what
/// `fabs` and `copysign` leave behind. Neither is a call: the front end lowers both to a mask over
/// the bits, so `fabs (1.0)` is a bitcast of an `and` of a bitcast, and without this the three
/// survive to the back end and compute a number the compiler already has.
///
/// The widths are checked rather than assumed. The verifier requires them to match and a fold that
/// quietly widened or narrowed a constant would be a wrong answer rather than a refused one, which
/// is not a thing to leave to another pass being right.
fn reinterpreted(func: &Func, operand: Value, ty: Type) -> Option<Imm> {
    let from = func[operand].ty;
    if !from.is_scalar() || from.bits() != ty.bits() {
        return None;
    }
    let bits = bits_of(func, operand)?;
    Some(Imm::from_bits(bits))
}

/// The constant this value is, with the type it has, if it is one.
///
/// Shared with [`crate::simplify_cfg`], which asks the same question about the condition of a
/// branch. Asking it in two places would be two answers about what a constant is.
pub(crate) fn constant(func: &Func, value: Value) -> Option<(Imm, Type)> {
    let Def::Result { inst, .. } = func[value].def else { return None };
    if func[inst].opcode != Opcode::IConst {
        return None;
    }
    let Extra::Imm(at) = func[inst].extra else { return None };
    let ty = func[value].ty;
    ty.is_int().then(|| (func[at], ty))
}

/// The floating point constant this value is, read in the format its own type gives it.
///
/// An `fconst` stores the bits and the type says how to read them, which is why this is one
/// function and not a pair of them: the bits of an `f80` and the bits of an `f128` are the same
/// hundred and twenty eight bits and mean different numbers.
fn floating(func: &Func, value: Value) -> Option<Float> {
    let Def::Result { inst, .. } = func[value].def else { return None };
    if func[inst].opcode != Opcode::FConst {
        return None;
    }
    let Extra::Imm(at) = func[inst].extra else { return None };
    let format = func[value].ty.format()?.encoding();
    Some(Float::from_bits(format, func[at].bits()))
}

/// A conversion of a floating point constant to an integer, and nothing when C does not say what
/// the answer is.
///
/// The two undefined cases are a number whose truncation is outside the destination type and a
/// NaN, and `to_integer` reports both as [`Status::INVALID`] rather than answering. Folding either
/// would be picking one refinement of poison and writing it into the program, which is what this
/// pass declines to do for an add that overflows under `nsw` and declines to do here for the same
/// reason.
fn to_integer(value: Float, to: Type, signed: bool) -> Option<Imm> {
    let (number, status) = value.to_integer(to.bits(), signed);
    (!status.has(Status::INVALID)).then(|| Imm::int(number, to))
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

/// What a comparison of two constants comes out as.
///
/// Shared with [`crate::simplify_cfg`], which asks the same question about the condition of a
/// branch it is deciding the direction of. Two answers about what `slt` means would be one too
/// many, and the two places would not be checked against each other by anything.
///
/// The type is the one the operands have rather than the `i1` the answer has, since that is the
/// width the comparison is at and the only thing the reading depends on. The two equalities are
/// the same question whichever way the bits are read, so they compare the immediates directly:
/// an immediate holds its value in exactly the width of its type, which is what makes that
/// equality the equality on the numbers.
pub(crate) fn compare(pred: IntPred, lhs: Imm, rhs: Imm, ty: Type) -> bool {
    match pred {
        IntPred::Eq => lhs == rhs,
        IntPred::Ne => lhs != rhs,
        IntPred::Slt => lhs.signed(ty) < rhs.signed(ty),
        IntPred::Sle => lhs.signed(ty) <= rhs.signed(ty),
        IntPred::Sgt => lhs.signed(ty) > rhs.signed(ty),
        IntPred::Sge => lhs.signed(ty) >= rhs.signed(ty),
        IntPred::Ult => lhs.unsigned() < rhs.unsigned(),
        IntPred::Ule => lhs.unsigned() <= rhs.unsigned(),
        IntPred::Ugt => lhs.unsigned() > rhs.unsigned(),
        IntPred::Uge => lhs.unsigned() >= rhs.unsigned(),
    }
}

/// One of the five bit operations on a constant.
///
/// All five are on the bits rather than on the number, so all five read the value unsigned. An
/// immediate is stored with everything above its own width cleared, so the bits of a value of a
/// narrow type are already in the low end of a 128 bit word with zeroes above them, and the whole
/// of the work here is putting the answer back at the width it was asked at.
///
/// The two searches answer the width for a zero argument. C leaves `__builtin_clz(0)` and
/// `__builtin_ctz(0)` undefined so nothing is entitled to that answer, but it is the answer the
/// software expansion in `rucc_codegen::expand` gives and `__builtin_ffs` is built on top of it, so
/// folding to anything else here would make the same program answer two different things depending
/// on whether the argument was visible. That is a worse outcome than either answer on its own.
///
/// A byte swap of a width that is not a whole number of bytes is left alone, which is what the
/// expansion does with one too. The verifier does not allow one and quietly reversing something
/// else would be worse than the instruction surviving to a selector that says it has no rule.
fn count(opcode: Opcode, value: Imm, from: Type, to: Type) -> Option<Imm> {
    let width = from.bits();
    if width == 0 || width > 128 {
        return None;
    }
    // The bits of the word that are above the value's own type, which is how far a whole word
    // answer has to come back down. Both ends of the range above are ruled out for it: a shift by
    // the width of the word is not defined and a width of nought has no bits to answer about.
    let spare = 128 - width;
    let bits = value.unsigned();
    let answer = match opcode {
        Opcode::Ctpop => i128::from(bits.count_ones()),
        // The zeroes above the type are counted by the word and are not the value's, so they come
        // off. For a zero value that leaves the width, which is the answer wanted.
        Opcode::Ctlz => i128::from(bits.leading_zeros() - spare),
        // Trailing zeroes need no correction because the zeroes above the type are above every
        // set bit, except for a zero value, where the word answers 128 and the width is wanted.
        Opcode::Cttz => i128::from(bits.trailing_zeros().min(width)),
        Opcode::Bswap if width % 8 == 0 => (bits.swap_bytes() >> spare) as i128,
        Opcode::Bitreverse => (bits.reverse_bits() >> spare) as i128,
        _ => return None,
    };
    Some(Imm::int(answer, to))
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
    use rucc_base::float::Format;
    use rucc_ir::{
        Block, Builder, Extra, Flags, Float, Func, IntPred, Module, Opcode, Signature, Type, Value,
    };
    use rucc_target::{Arch, Env, Os, TargetInfo, Triple};

    use crate::stats::Kind;
    use crate::{Fuel, Pass, fold::Fold};

    /// A function with one block, ready to have instructions appended to it.
    fn blank() -> (Interner, Func, Block) {
        let mut names = Interner::new();
        let name = names.intern("f");
        let mut func = Func::new(name, Signature::new().with_returns(&[Type::int(64)]));
        let block = func.create_block();
        (names, func, block)
    }

    /// Runs the pass over the function with as much fuel as it wants, and says whether it
    /// rewrote anything.
    fn fold(func: &mut Func) -> bool {
        Fold.run(func, &mut crate::machine::fixtures::analyses(), &mut Fuel::unlimited()).changed()
    }

    /// An `fconst` of the number this text spells, in the format the type gives it.
    ///
    /// Through `rucc_base::float` rather than through the host's `f64`, for the reason that module
    /// exists: the bits a literal means are the target's answer and not the machine running the
    /// test's.
    fn number(build: &mut Builder<'_>, text: &str, ty: Type) -> Value {
        let format = ty.format().expect("a floating point type").encoding();
        let (value, _) = super::Float::parse(text, format).expect("a number");
        build.fconst(ty, value.to_bits())
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

    /// The bits a value now holds, or `None` if it is not a floating point constant.
    fn float_bits(func: &Func, value: Value) -> Option<u128> {
        let rucc_ir::Def::Result { inst, .. } = func[value].def else { return None };
        if func[inst].opcode != Opcode::FConst {
            return None;
        }
        let Extra::Imm(at) = func[inst].extra else { return None };
        Some(func[at].bits())
    }

    /// C has no negative floating constant, so `-1.0` is a unary minus on a positive one and
    /// arrives here as two instructions. This is the fold that makes it one.
    #[test]
    fn a_negated_floating_constant_becomes_a_constant() {
        let (_, mut func, block) = blank();
        let ty = Type::float(Float::F64);
        let mut build = Builder::new(&mut func, block);
        let one = number(&mut build, "1.0", ty);
        let minus = build.unary(Opcode::FNeg, one, ty);
        build.ret(&[minus]);
        assert!(fold(&mut func));
        assert_eq!(float_bits(&func, minus), Some(0xbff0_0000_0000_0000));
    }

    /// Negation is the sign bit and nothing else, which is what lets it fold at all, and a
    /// negative zero is where that shows: the value is equal to a positive zero and the bits are
    /// not, so anything that went through a comparison would give the wrong answer here.
    #[test]
    fn a_negated_zero_keeps_its_sign_bit() {
        let (_, mut func, block) = blank();
        let ty = Type::float(Float::F64);
        let mut build = Builder::new(&mut func, block);
        let zero = number(&mut build, "0.0", ty);
        let minus = build.unary(Opcode::FNeg, zero, ty);
        build.ret(&[minus]);
        assert!(fold(&mut func));
        assert_eq!(float_bits(&func, minus), Some(1 << 63));
    }

    /// The same for a NaN, whose payload goes through untouched. 754 says negation copies every
    /// bit but the sign for every input, and a NaN is the input where a compiler that quietly did
    /// arithmetic instead would be caught.
    #[test]
    fn a_negated_nan_keeps_its_payload() {
        let (_, mut func, block) = blank();
        let ty = Type::float(Float::F64);
        let mut build = Builder::new(&mut func, block);
        let nan = build.fconst(ty, 0x7ff8_0000_dead_beef);
        let minus = build.unary(Opcode::FNeg, nan, ty);
        build.ret(&[minus]);
        assert!(fold(&mut func));
        assert_eq!(float_bits(&func, minus), Some(0xfff8_0000_dead_beef));
    }

    /// The sign bit of an `f80` is the top bit of the eighty the value has and not of the hundred
    /// and twenty eight the object is stored in, which is the one place this could be written
    /// wrong and give a number nobody asked for.
    #[test]
    fn the_sign_bit_of_an_x87_value_is_the_top_bit_of_its_width() {
        let (_, mut func, block) = blank();
        let ty = Type::float(Float::F80);
        let mut build = Builder::new(&mut func, block);
        let one = number(&mut build, "1.0", ty);
        let minus = build.unary(Opcode::FNeg, one, ty);
        build.ret(&[minus]);
        assert!(fold(&mut func));
        let bits = float_bits(&func, minus).expect("a constant");
        assert_eq!(bits >> 79 & 1, 1, "the sign bit is set");
        assert_eq!(bits >> 80, 0, "nothing above the value is touched");
    }

    /// A bitcast of a constant is the same bits read as another type, which is what `fabs` of a
    /// constant needs: the front end lowers it to a mask over the bits rather than to a call, so
    /// folding it away is three instructions rather than one.
    #[test]
    fn a_bitcast_of_a_constant_is_the_same_bits() {
        let (_, mut func, block) = blank();
        let ty = Type::float(Float::F64);
        let bits = Type::int(64);
        let mut build = Builder::new(&mut func, block);
        let value = number(&mut build, "-3.5", ty);
        let number = build.unary(Opcode::Bitcast, value, bits);
        let mask = build.iconst(bits, i128::from(i64::MAX));
        let cleared = build.binary(Opcode::And, number, mask, Flags::NONE);
        let back = build.unary(Opcode::Bitcast, cleared, ty);
        build.ret(&[back]);
        assert!(fold(&mut func));
        assert_eq!(float_bits(&func, back), Some(0x400c_0000_0000_0000));
    }

    /// A bitcast whose operand is not a constant is left alone, which is the case nearly every
    /// bitcast in a real function is.
    #[test]
    fn a_bitcast_of_something_that_is_not_a_constant_is_left_alone() {
        let mut names = Interner::new();
        let name = names.intern("f");
        let ty = Type::float(Float::F64);
        let signature = Signature::new().with_params(&[ty]).with_returns(&[Type::int(64)]);
        let mut func = Func::new(name, signature);
        let block = func.create_block();
        let x = func.append_param(block, ty);
        let mut build = Builder::new(&mut func, block);
        let number = build.unary(Opcode::Bitcast, x, Type::int(64));
        build.ret(&[number]);
        assert!(!fold(&mut func));
        assert_eq!(value_of(&func, number, Type::int(64)), None);
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

    /// A select of two constants of the same number, under a condition nothing knows, is that
    /// number, and one of two different numbers is left for the back end to choose between.
    #[test]
    fn a_select_between_two_equal_constants_is_that_constant() {
        for (other, folds) in [(2_i128, true), (3, false)] {
            let mut names = Interner::new();
            let signature = Signature::new().with_params(&[Type::int(32)]);
            let mut func = Func::new(names.intern("f"), signature.with_returns(&[Type::int(32)]));
            let block = func.create_block();
            let x = func.append_param(block, Type::int(32));
            let mut build = Builder::new(&mut func, block);
            let zero = build.iconst(Type::int(32), 0);
            let test = build.icmp(IntPred::Slt, x, zero);
            let then = build.iconst(Type::int(32), 2);
            let other = build.iconst(Type::int(32), other);
            let out = build.select(test, then, other);
            build.ret(&[out]);
            assert_eq!(fold(&mut func), folds);
            let want = folds.then_some(2);
            assert_eq!(value_of(&func, out, Type::int(32)), want);
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

    /// The one instruction under test, on one constant, folded as far as the pass takes it.
    fn one(opcode: Opcode, ty: Type, arg: i128) -> Option<i128> {
        let (_, mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let value = build.iconst(ty, arg);
        let out = build.unary(opcode, value, ty);
        build.ret(&[out]);
        fold(&mut func);
        value_of(&func, out, ty)
    }

    #[test]
    fn the_bit_counts_are_evaluated_at_the_width_they_were_asked_at() {
        let cases = [
            (Opcode::Ctlz, 64, 0x0000_1000_0000_0000_i128, 19_i128),
            (Opcode::Ctlz, 32, 0x0000_1000, 19),
            (Opcode::Cttz, 64, 0x0000_1000_0000_0000, 44),
            (Opcode::Cttz, 32, 0x0000_1000, 12),
            (Opcode::Ctpop, 64, 0x0000_1000_0000_0000, 1),
            (Opcode::Ctpop, 32, -1, 32),
            (Opcode::Ctpop, 64, -1, 64),
        ];
        for (opcode, width, arg, want) in cases {
            let ty = Type::int(width);
            assert_eq!(one(opcode, ty, arg), Some(want), "{opcode:?} at {width} of {arg:#x}");
        }
    }

    #[test]
    fn a_search_for_a_bit_in_a_zero_answers_the_width_the_expansion_answers() {
        for width in [8_u32, 16, 32, 64] {
            let ty = Type::int(width);
            let want = Some(i128::from(width));
            assert_eq!(one(Opcode::Ctlz, ty, 0), want, "leading, at {width}");
            assert_eq!(one(Opcode::Cttz, ty, 0), want, "trailing, at {width}");
            assert_eq!(one(Opcode::Ctpop, ty, 0), Some(0), "count, at {width}");
        }
    }

    #[test]
    fn the_two_reversals_are_evaluated_and_a_byte_swap_of_a_part_of_a_byte_is_not() {
        let ty = Type::int(32);
        assert_eq!(one(Opcode::Bswap, ty, 0x1234_5678), Some(0x7856_3412));
        assert_eq!(one(Opcode::Bswap, Type::int(16), 0x1234), Some(0x3412));
        assert_eq!(one(Opcode::Bitreverse, Type::int(8), 0b1010_1100), Some(0b0011_0101));
        // A width that is not a whole number of bytes has no byte swap, so there is nothing to
        // evaluate and the instruction stays for the backend to refuse.
        let (_, mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let value = build.iconst(Type::int(4), 0b1010);
        let out = build.unary(Opcode::Bswap, value, Type::int(4));
        build.ret(&[out]);
        assert!(!fold(&mut func));
    }

    #[test]
    fn a_comparison_of_two_constants_becomes_a_one_or_a_nought() {
        let cases = [
            (IntPred::Eq, 7_i128, 7_i128, true),
            (IntPred::Eq, 7, 8, false),
            (IntPred::Ne, 7, 8, true),
            (IntPred::Slt, -1, 1, true),
            (IntPred::Sle, -1, -1, true),
            (IntPred::Sgt, -1, 1, false),
            (IntPred::Sge, 1, -1, true),
            // The same pair read as bits rather than as numbers, where minus one is the largest
            // value there is and every unsigned answer is the opposite of the signed one.
            (IntPred::Ult, -1, 1, false),
            (IntPred::Ule, -1, 1, false),
            (IntPred::Ugt, -1, 1, true),
            (IntPred::Uge, -1, 1, true),
        ];
        for (pred, a, b, want) in cases {
            let (_, mut func, block) = blank();
            let mut build = Builder::new(&mut func, block);
            let lhs = build.iconst(Type::int(64), a);
            let rhs = build.iconst(Type::int(64), b);
            let out = build.icmp(pred, lhs, rhs);
            build.ret(&[out]);
            assert!(fold(&mut func), "{pred:?} {a} {b}");
            // The answer is one bit, where a set bit read as a signed number is minus one, so
            // the question is which of the two constants it is rather than what it prints as.
            let got = value_of(&func, out, Type::I1).expect("the comparison folded");
            assert_eq!(got != 0, want, "{pred:?} {a} {b}");
        }
    }

    #[test]
    fn a_comparison_at_a_narrow_width_is_read_at_that_width() {
        // Two hundred and fifty five stored in eight bits is minus one, so it is below one when
        // the comparison is signed and above it when the comparison is not.
        let ty = Type::int(8);
        for (pred, want) in [(IntPred::Slt, true), (IntPred::Ult, false)] {
            let (_, mut func, block) = blank();
            let mut build = Builder::new(&mut func, block);
            let lhs = build.iconst(ty, 255);
            let rhs = build.iconst(ty, 1);
            let out = build.icmp(pred, lhs, rhs);
            build.ret(&[out]);
            assert!(fold(&mut func), "{pred:?}");
            let got = value_of(&func, out, Type::I1).expect("the comparison folded");
            assert_eq!(got != 0, want, "{pred:?}");
        }
    }

    #[test]
    fn a_comparison_with_one_constant_operand_is_left_alone() {
        let (_, mut func, block) = blank();
        let ty = Type::int(64);
        let param = func.append_param(block, ty);
        let mut build = Builder::new(&mut func, block);
        let rhs = build.iconst(ty, 3);
        let out = build.icmp(IntPred::Eq, param, rhs);
        build.ret(&[out]);
        assert!(!fold(&mut func));
    }

    #[test]
    fn a_bit_count_of_something_that_is_not_a_constant_is_left_alone() {
        for opcode in [Opcode::Ctlz, Opcode::Cttz, Opcode::Ctpop, Opcode::Bswap] {
            let (_, mut func, block) = blank();
            let ty = Type::int(64);
            let param = func.append_param(block, ty);
            let mut build = Builder::new(&mut func, block);
            let out = build.unary(opcode, param, ty);
            build.ret(&[out]);
            assert!(!fold(&mut func), "{opcode:?}");
        }
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
    fn a_conversion_to_an_integer_truncates_toward_zero() {
        for (text, expected) in [("2.75", 2_i128), ("-2.75", -2), ("0.5", 0), ("-0.5", 0)] {
            let (_, mut func, block) = blank();
            let mut build = Builder::new(&mut func, block);
            let value = number(&mut build, text, Type::float(Float::F64));
            let out = build.unary(Opcode::FPToSI, value, Type::int(32));
            build.ret(&[out]);
            assert!(fold(&mut func), "{text}");
            assert_eq!(value_of(&func, out, Type::int(32)), Some(expected), "{text}");
        }
    }

    #[test]
    fn a_negative_number_converts_to_an_unsigned_type_only_when_truncating_lands_on_zero() {
        for (text, expected) in [("-0.5", Some(0)), ("-1.5", None)] {
            let (_, mut func, block) = blank();
            let mut build = Builder::new(&mut func, block);
            let value = number(&mut build, text, Type::float(Float::F64));
            let out = build.unary(Opcode::FPToUI, value, Type::int(32));
            build.ret(&[out]);
            assert_eq!(fold(&mut func), expected.is_some(), "{text}");
            assert_eq!(value_of(&func, out, Type::int(32)), expected, "{text}");
        }
    }

    #[test]
    fn a_number_the_destination_type_has_no_room_for_is_left_alone() {
        let (_, mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let value = number(&mut build, "1e30", Type::float(Float::F64));
        let out = build.unary(Opcode::FPToSI, value, Type::int(32));
        build.ret(&[out]);
        assert!(!fold(&mut func));
        assert_eq!(func[out_inst(&func, out)].opcode, Opcode::FPToSI);
    }

    #[test]
    fn a_nan_is_left_alone() {
        let (_, mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let value = build.fconst(Type::float(Float::F64), 0x7ff8_0000_0000_0000);
        let out = build.unary(Opcode::FPToSI, value, Type::int(32));
        build.ret(&[out]);
        assert!(!fold(&mut func));
    }

    #[test]
    fn a_constant_is_read_in_the_format_its_own_type_gives_it() {
        // The same hundred and twenty eight bits, which are an x87 three and an `f128` far too
        // small to be anything but zero once it has been truncated.
        let bits = super::Float::parse("3.0", Format::X87Extended).expect("a number").0.to_bits();
        for (float, expected) in [(Float::F80, 3_i128), (Float::F128, 0)] {
            let (_, mut func, block) = blank();
            let mut build = Builder::new(&mut func, block);
            let value = build.fconst(Type::float(float), bits);
            let out = build.unary(Opcode::FPToSI, value, Type::int(32));
            build.ret(&[out]);
            assert!(fold(&mut func), "{float}");
            assert_eq!(value_of(&func, out, Type::int(32)), Some(expected), "{float}");
        }
    }

    #[test]
    fn a_conversion_of_something_that_is_not_a_constant_is_left_alone() {
        let (_, mut func, block) = blank();
        let param = func.append_param(block, Type::float(Float::F64));
        let mut build = Builder::new(&mut func, block);
        let out = build.unary(Opcode::FPToSI, param, Type::int(32));
        build.ret(&[out]);
        assert!(!fold(&mut func));
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
        let stats =
            Fold.run(&mut none, &mut crate::machine::fixtures::analyses(), &mut Fuel::of(0));
        assert!(!stats.changed());
        assert_eq!(none[out_inst(&none, first)].opcode, Opcode::SExt);
        // Both of them looked at and neither of them folded, which is the count a bisection is
        // reading: how many sites are left past where the fuel ran out.
        assert_eq!(stats.count(Kind::Missed, super::NO_FUEL), 2);

        let (_, mut one, block) = blank();
        let (first, second) = build_two(&mut one, block);
        let mut fuel = Fuel::of(1);
        let stats = Fold.run(&mut one, &mut crate::machine::fixtures::analyses(), &mut fuel);
        assert!(stats.changed());
        assert_eq!(fuel.spent(), 1);
        assert_eq!(stats.count(Kind::Optimized, super::FOLDED), 1);
        assert_eq!(stats.count(Kind::Missed, super::NO_FUEL), 1);
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
