//! The float that is wider than any instruction, as the calls that do the work.
//!
//! `_Float128` is the one arithmetic type a C program writes that no machine computes with.
//! Sixteen bytes fit in a vector register, so moving one, passing one and returning one are
//! instructions this back end has, and `spec/10-backend.md` section 10.8's rule set is written at
//! this width for exactly those three. Everything else is a call: no processor anyone compiles for
//! has an instruction that adds two binary128 values, which is why `spec/12-abi-and-runtime.md`
//! section 12.8 puts the whole format in the runtime rather than in the part of the runtime that
//! exists for targets with no floating point unit. This pass is what turns the operation the
//! program wrote into the call the routine is.
//!
//! After it there is no arithmetic, no comparison and no conversion at this format left in the
//! function, which is what lets the selector go on being a table over widths the machine has. What
//! is left at the width is the three things a rule is written for, and a call is one of them in the
//! sense that matters: the value travels in the register the convention names.
//!
//! # Why a pass and not a rule
//!
//! The same reason [`crate::wide`] is a pass. A rule rewrites a term into instructions of the
//! machine, and there is no instruction here to rewrite into, so there is nothing for a rule to
//! produce. A call is not a rewrite of a term either, because what it needs first is the
//! convention's answer about where each operand goes, which is `crate::abi`'s and not a rule's.
//!
//! # The names are libgcc's, and the spelling is a target fact
//!
//! `__addtf3` and the rest, which is what `runtime/builtins/quad.c` defines and what libgcc defines
//! beside it, so a call this pass writes links against either. The `tf` in the name means binary128
//! on every target this compiler has a back end for, and on ppc64 it does not: there `long double`
//! is a pair of doubles, `__addtf3` is the arithmetic on that pair, and binary128 is spelled `kf`.
//! So the table below is right for the back ends that exist and is the first thing to look at when
//! a ppc64 one does, which is `tamnd/rucc#618`'s row rather than this pass's business today.
//!
//! # What is left alone
//!
//! An operation at this format whose routine is not in the archive is left exactly as it was and
//! refused below by name, the same way [`crate::wide`] leaves a conversion at eighty bits alone.
//! That is a conversion against a `__int128`, whose four routines are not written yet, a conversion
//! against a `_Float16` or an eighty bit float, and a `select` of two quads, which is a move the
//! rule set has no conditional form of. A refusal naming the instruction is the outcome every one
//! of those had before this pass existed and it is still the right one: the alternative is a call
//! to a routine no archive defines, which is a link that fails further from the cause.

use rucc_base::Interner;
use rucc_ir::{
    CallInfo, Extra, Flags, Float, FloatPred, Func, Imm, Inst, InstData, IntPred, MemInfo, MemOrder,
    Opcode, Restrict, Signature, Type, Value,
};

/// The format this pass is about.
const QUAD: Float = Float::F128;

/// How wide it is, in bits and then in bytes.
const BITS: u32 = 128;

/// The two widths the runtime has an integer conversion at, which are the two a C program on a
/// machine with sixty four bit registers has integers of.
const NARROW: u32 = 32;
const WORD: u32 = 64;

/// Rewrites every operation at this format into the call that performs it.
///
/// The instructions are collected before any of them is touched, because a rewrite puts
/// instructions in front of the one it replaces and the walk would otherwise see its own work.
pub fn calls(func: &mut Func, names: &mut Interner) {
    let found: Vec<Inst> =
        func.blocks().flat_map(|block| func.insts(block).collect::<Vec<_>>()).collect();
    for inst in found {
        match func[inst].opcode {
            Opcode::FAdd | Opcode::FSub | Opcode::FMul | Opcode::FDiv => {
                arithmetic(func, names, inst);
            }
            Opcode::FNeg => negate(func, names, inst),
            Opcode::FCmp => compare(func, names, inst),
            Opcode::FConst => constant(func, inst),
            Opcode::FPExt => widen(func, names, inst),
            Opcode::FPTrunc => narrow(func, names, inst),
            Opcode::SIToFP | Opcode::UIToFP => from_integer(func, names, inst),
            Opcode::FPToSI | Opcode::FPToUI => to_integer(func, names, inst),
            _ => {}
        }
    }
}

/// Whether this type is the format.
fn quad(ty: Type) -> bool {
    ty.is_scalar() && ty.format() == Some(QUAD)
}

/// The type of an instruction's first result, or nothing where it has none.
fn produced(func: &Func, inst: Inst) -> Option<Type> {
    func[inst].first_result.map(|value| func[value].ty)
}

/// The four operations, each of them the routine of its name over the two operands.
///
/// The call goes in place of the instruction rather than in front of it, so the value the rest of
/// the function reads is the value it already read and nothing has to be substituted anywhere.
/// That works here and not in [`crate::wide`] because the answer is one value of the same type:
/// nothing about this format is split, it simply is not computed.
fn arithmetic(func: &mut Func, names: &mut Interner, inst: Inst) {
    let Some(ty) = produced(func, inst) else { return };
    if !quad(ty) {
        return;
    }
    let args = func[func[inst].args].to_vec();
    let [a, b] = args[..] else { return };
    let routine = match func[inst].opcode {
        Opcode::FAdd => "__addtf3",
        Opcode::FSub => "__subtf3",
        Opcode::FMul => "__multf3",
        Opcode::FDiv => "__divtf3",
        _ => return,
    };
    into_call(func, names, inst, routine, &[a, b]);
}

/// The negation, which is a call here and an exclusive or at the two narrower formats.
///
/// [`crate::expand::floats`] flips the sign bit of a narrower float in a general purpose register,
/// and the exchange that makes that worth doing runs out at this width twice over: the integer that
/// would hold the bits has no register either, and the mask would want a constant pool that nothing
/// else in this back end needs. libgcc has the routine, so the routine is what this is.
fn negate(func: &mut Func, names: &mut Interner, inst: Inst) {
    let Some(ty) = produced(func, inst) else { return };
    let Some(&arg) = func[func[inst].args].first() else { return };
    if !quad(ty) {
        return;
    }
    into_call(func, names, inst, "__negtf2", &[arg]);
}

/// A comparison, as the call that answers it and the test of that answer against zero.
///
/// Only the sign of what these routines hand back is specified, never its magnitude, so the caller
/// compares against zero and the predicate it compares with is the one the name promised. Six of
/// the sixteen predicates are a routine each, six more are one of those six read the other way
/// round, and two need both calls.
///
/// The six that are one call are the ordered comparisons, because the number a routine answers for
/// a not a number is the one that makes its own test come out false. So `__lttf2` is below zero for
/// `a < b` and above zero for a not a number, and a test for below zero is then ordered and less
/// than and nothing else. Reading that same answer as at or above zero is unordered or greater than
/// or equal, which is the negation, and that is where the other six come from: `ult` is not `oge`,
/// `ule` is not `ogt`, and so on down.
///
/// `one` and `ueq` are the two that are not a reading of one answer. Ordered and not equal is
/// neither operand a not a number and the two of them different, and there is no single routine for
/// it, so it is `__unordtf2` saying ordered and `__netf2` saying different. Unordered or equal is
/// the negation of that and is the same two calls with the other connective. gcc emits the pair for
/// them too. Neither is a shape C's operators produce, since `!(a == b)` is unordered or not equal
/// and not this, but the optimizer may fold its way to one and the back end has to have an answer.
fn compare(func: &mut Func, names: &mut Interner, inst: Inst) {
    let args = func[func[inst].args].to_vec();
    let [a, b] = args[..] else { return };
    if !quad(func[a].ty) || !quad(func[b].ty) {
        return;
    }
    let Extra::FloatPred(pred) = func[inst].extra else { return };
    if let Some((routine, test)) = single(pred) {
        let answer = call(func, names, inst, routine, &[a, b], Type::int(NARROW));
        let zero = ahead_const(func, inst, Imm::int(0, Type::int(NARROW)), Type::int(NARROW));
        let extra = Extra::IntPred(test);
        becomes(func, inst, Opcode::ICmp, extra, &[answer, zero]);
        return;
    }
    // Always false and always true are constants rather than comparisons, and they are here rather
    // than left alone because a rule at this format would be a rule about an operand it never
    // reads.
    if let FloatPred::False | FloatPred::True = pred {
        let bits = u128::from(pred == FloatPred::True);
        let extra = Extra::Imm(func.add_imm(Imm::int(bits as i128, Type::I1)));
        becomes(func, inst, Opcode::IConst, extra, &[]);
        return;
    }
    let (FloatPred::One | FloatPred::Ueq) = pred else { return };
    let ordered = pair(func, names, inst, "__unordtf2", a, b, IntPred::Eq);
    let different = pair(func, names, inst, "__netf2", a, b, IntPred::Ne);
    // Ordered and different, or the negation of it, which by De Morgan is unordered or the same.
    let (opcode, args) = if pred == FloatPred::One {
        (Opcode::And, [ordered, different])
    } else {
        let unordered = flipped(func, inst, ordered);
        let same = flipped(func, inst, different);
        (Opcode::Or, [unordered, same])
    };
    becomes(func, inst, opcode, Extra::None, &args);
}

/// The routine for a predicate that is one call, and the test its answer is read with.
fn single(pred: FloatPred) -> Option<(&'static str, IntPred)> {
    Some(match pred {
        FloatPred::Oeq => ("__eqtf2", IntPred::Eq),
        FloatPred::Une => ("__netf2", IntPred::Ne),
        FloatPred::Olt => ("__lttf2", IntPred::Slt),
        FloatPred::Ole => ("__letf2", IntPred::Sle),
        FloatPred::Ogt => ("__gttf2", IntPred::Sgt),
        FloatPred::Oge => ("__getf2", IntPred::Sge),
        FloatPred::Uno => ("__unordtf2", IntPred::Ne),
        FloatPred::Ord => ("__unordtf2", IntPred::Eq),
        // The four that are one of the ordered answers read as its negation.
        FloatPred::Ult => ("__getf2", IntPred::Slt),
        FloatPred::Ule => ("__gttf2", IntPred::Sle),
        FloatPred::Ugt => ("__letf2", IntPred::Sgt),
        FloatPred::Uge => ("__lttf2", IntPred::Sge),
        _ => return None,
    })
}

/// One of the two calls a `one` or a `ueq` is made of, and its answer tested against zero.
fn pair(
    func: &mut Func,
    names: &mut Interner,
    inst: Inst,
    routine: &str,
    a: Value,
    b: Value,
    test: IntPred,
) -> Value {
    let answer = call(func, names, inst, routine, &[a, b], Type::int(NARROW));
    let zero = ahead_const(func, inst, Imm::int(0, Type::int(NARROW)), Type::int(NARROW));
    let args = func.push_values(&[answer, zero]);
    let extra = Extra::IntPred(test);
    written(func, inst, InstData { args, extra, ..InstData::new(Opcode::ICmp) }, Type::I1)
}

/// A truth value with the other answer, which is an exclusive or with one.
fn flipped(func: &mut Func, inst: Inst, value: Value) -> Value {
    let one = ahead_const(func, inst, Imm::int(1, Type::I1), Type::I1);
    let args = func.push_values(&[value, one]);
    written(func, inst, InstData { args, ..InstData::new(Opcode::Xor) }, Type::I1)
}

/// A constant, as the bits written into a frame slot and read back at this format.
///
/// Every other constant in this back end is an immediate in an instruction, and this one cannot be:
/// no instruction carries sixteen bytes of immediate, the integer that would spell the bits has no
/// register either, and the constant pool a literal would otherwise go in is a section this back
/// end does not have yet. So the bits go where the value lives, which for a value this back end
/// has no other home for is the frame, and the read back is the whole register move the rule set
/// already has at this width.
///
/// The low word goes at the lower address, which is this machine's order and is the same
/// assumption [`crate::wide`] makes about the halves of an integer this wide. A back end for a big
/// endian target is where that becomes a question, and it is the same question in both passes.
///
/// The slot is a fixed size `alloca`, so it is one slot in the frame however many times control
/// reaches it, and a constant inside a loop costs two stores a time round rather than anything that
/// grows.
fn constant(func: &mut Func, inst: Inst) {
    let Some(ty) = produced(func, inst) else { return };
    let Extra::Imm(imm) = func[inst].extra else { return };
    if !quad(ty) {
        return;
    }
    let bits = func[imm].bits();
    let bytes = u64::from(BITS / 8);
    let whole = MemInfo {
        size: bytes,
        align: BITS / 8,
        order: MemOrder::NotAtomic,
        tbaa: None,
        owns: 0,
        restrict: Restrict::NONE,
    };
    let slot = {
        let extra = Extra::Mem(func.add_mem(whole));
        written(func, inst, InstData { extra, ..InstData::new(Opcode::Alloca) }, Type::PTR)
    };
    let half = u64::from(WORD / 8);
    let word = Type::int(WORD);
    let low = ahead_const(func, inst, Imm::int(bits as i128, word), word);
    write(func, inst, low, slot, MemInfo { size: half, ..whole });
    let step = ahead_const(func, inst, Imm::int(half as i128, Type::PTR), Type::PTR);
    let args = func.push_values(&[slot, step]);
    let above =
        written(func, inst, InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR);
    let high = ahead_const(func, inst, Imm::int((bits >> WORD) as i128, word), word);
    write(func, inst, high, above, MemInfo { size: half, align: WORD / 8, ..whole });
    let extra = Extra::Mem(func.add_mem(whole));
    becomes(func, inst, Opcode::Load, extra, &[slot]);
}

/// A narrower float becoming a quad, which is one of two routines and never rounds.
///
/// Only from the two formats the runtime has a routine from. A `_Float16` widening straight to this
/// format is not one of them and neither is the eighty bit format, which this machine has no
/// register for anyway, and both are left alone and refused below.
fn widen(func: &mut Func, names: &mut Interner, inst: Inst) {
    let Some(ty) = produced(func, inst) else { return };
    let Some(&arg) = func[func[inst].args].first() else { return };
    if !quad(ty) {
        return;
    }
    let routine = match func[arg].ty.format() {
        Some(Float::F32) => "__extendsftf2",
        Some(Float::F64) => "__extenddftf2",
        _ => return,
    };
    into_call(func, names, inst, routine, &[arg]);
}

/// A quad becoming a narrower float, which is the other direction of the same pair and rounds.
fn narrow(func: &mut Func, names: &mut Interner, inst: Inst) {
    let Some(ty) = produced(func, inst) else { return };
    let Some(&arg) = func[func[inst].args].first() else { return };
    if !quad(func[arg].ty) {
        return;
    }
    let routine = match ty.format() {
        Some(Float::F32) => "__trunctfsf2",
        Some(Float::F64) => "__trunctfdf2",
        _ => return,
    };
    into_call(func, names, inst, routine, &[arg]);
}

/// An integer becoming a quad, which is a widening to a width the runtime has a routine at and then
/// that routine.
///
/// The runtime has four, a signed and an unsigned integer at thirty two bits and at sixty four, so
/// an integer narrower than that is widened first, with the sign for a signed one and with zeroes
/// for an unsigned one. That is the same move [`crate::expand::floats`] makes in front of the
/// machine's own conversion and for the same reason: after the widening the value is the same
/// number at a width there is a conversion from.
///
/// Nothing rounds in any of the four, which is the property `spec/12-abi-and-runtime.md` section
/// 12.8 measures rather than assumes, so a program that widens an integer through this format and
/// back has the integer it started with.
///
/// A `__int128` is not widened into anything and is left alone, because the routine that would take
/// one is not written. [`crate::wide`] leaves it alone as well, so what the program gets is the
/// refusal naming the conversion, which is `tamnd/rucc#1064`.
fn from_integer(func: &mut Func, names: &mut Interner, inst: Inst) {
    let Some(ty) = produced(func, inst) else { return };
    let Some(&arg) = func[func[inst].args].first() else { return };
    let from = func[arg].ty;
    if !quad(ty) || !from.is_int() || !from.is_scalar() {
        return;
    }
    let signed = func[inst].opcode == Opcode::SIToFP;
    let Some(width) = holder(from.bits()) else { return };
    let routine = match (signed, width) {
        (true, NARROW) => "__floatsitf",
        (false, NARROW) => "__floatunsitf",
        (true, _) => "__floatditf",
        (false, _) => "__floatunditf",
    };
    let value = if from.bits() == width {
        arg
    } else {
        let opcode = if signed { Opcode::SExt } else { Opcode::ZExt };
        let args = func.push_values(&[arg]);
        written(func, inst, InstData { args, ..InstData::new(opcode) }, Type::int(width))
    };
    into_call(func, names, inst, routine, &[value]);
}

/// A quad becoming an integer, which is the routine at a width the runtime has one at and then a
/// truncation to the width the program asked for.
///
/// The four routines answer an `int`, an `unsigned int`, a `long long` and an `unsigned long long`,
/// so a narrower answer is the thirty two bit routine and a truncation. Nothing is lost by that:
/// the value has to fit the type the program named or the conversion is undefined, and a value that
/// fits is a value the truncation leaves alone.
///
/// The four cases C leaves undefined, which are a value too large, a value too small, an infinity
/// and a not a number, answer zero in the routine. Section 12.8 records that as a convention the
/// differential can hold both implementations to rather than as a promise a program may read, and
/// nothing here makes it one: no test goes in front of the call, the same way nothing tests a
/// divisor for zero in front of `__divti3`.
fn to_integer(func: &mut Func, names: &mut Interner, inst: Inst) {
    let Some(ty) = produced(func, inst) else { return };
    let Some(&arg) = func[func[inst].args].first() else { return };
    if !quad(func[arg].ty) || !ty.is_int() || !ty.is_scalar() {
        return;
    }
    let signed = func[inst].opcode == Opcode::FPToSI;
    let Some(width) = holder(ty.bits()) else { return };
    let routine = match (signed, width) {
        (true, NARROW) => "__fixtfsi",
        (false, NARROW) => "__fixunstfsi",
        (true, _) => "__fixtfdi",
        (false, _) => "__fixunstfdi",
    };
    if ty.bits() == width {
        into_call(func, names, inst, routine, &[arg]);
        return;
    }
    let answer = call(func, names, inst, routine, &[arg], Type::int(width));
    becomes(func, inst, Opcode::Trunc, Extra::None, &[answer]);
}

/// The width of the routine that serves an integer of this width, where one does.
///
/// A width at or below thirty two is served by the thirty two bit routine and one above it by the
/// sixty four bit routine, and a hundred and twenty eight is served by nothing here. Every width
/// reaching this pass is one of the machine's own, because [`crate::widths`] has already rounded an
/// integer of forty bits up into one of sixty four, so the only widths this sees are one, eight,
/// sixteen, thirty two, sixty four and a hundred and twenty eight.
fn holder(bits: u32) -> Option<u32> {
    match bits {
        0..=NARROW => Some(NARROW),
        33..=WORD => Some(WORD),
        _ => None,
    }
}

/// Turns an instruction into the call that performs it, in place.
///
/// In place rather than in front of, because the call produces one value of the type the
/// instruction already produced, so every reader of it goes on reading the same value. What the
/// program said about rounding and about not a numbers is dropped, since a call carries none of it
/// and the routine has its own answers, which are libgcc's.
fn into_call(func: &mut Func, names: &mut Interner, inst: Inst, routine: &str, args: &[Value]) {
    let Some(ty) = produced(func, inst) else { return };
    let params: Vec<Type> = args.iter().map(|&value| func[value].ty).collect();
    let signature = func.add_signature(Signature::new().with_params(&params).with_returns(&[ty]));
    let callee = Some(names.intern(routine));
    let varargs = func.push_abis(&[]);
    let extra = Extra::Call(func.add_call(CallInfo { callee, signature, varargs }));
    becomes(func, inst, Opcode::Call, extra, args);
}

/// A call to a runtime routine written in front of an instruction, and the value it answers.
fn call(
    func: &mut Func,
    names: &mut Interner,
    inst: Inst,
    routine: &str,
    args: &[Value],
    ty: Type,
) -> Value {
    let params: Vec<Type> = args.iter().map(|&value| func[value].ty).collect();
    let signature = func.add_signature(Signature::new().with_params(&params).with_returns(&[ty]));
    let callee = Some(names.intern(routine));
    let varargs = func.push_abis(&[]);
    let extra = Extra::Call(func.add_call(CallInfo { callee, signature, varargs }));
    let args = func.push_values(args);
    written(func, inst, InstData { args, extra, ..InstData::new(Opcode::Call) }, ty)
}

/// A constant put in front of an instruction.
fn ahead_const(func: &mut Func, inst: Inst, imm: Imm, ty: Type) -> Value {
    let extra = Extra::Imm(func.add_imm(imm));
    written(func, inst, InstData { extra, ..InstData::new(Opcode::IConst) }, ty)
}

/// A store put in front of an instruction, which produces nothing and is only its effect.
fn write(func: &mut Func, inst: Inst, value: Value, into: Value, info: MemInfo) {
    let span = func.span(inst);
    let extra = Extra::Mem(func.add_mem(info));
    let args = func.push_values(&[value, into]);
    let data = InstData { args, extra, ..InstData::new(Opcode::Store) };
    let made = func.create_inst(data, &[], span);
    func.insert_before(made, inst);
}

/// Creates an instruction, puts it in front of another one, and reads its value back out.
fn written(func: &mut Func, inst: Inst, data: InstData, ty: Type) -> Value {
    let span = func.span(inst);
    let made = func.create_inst(data, &[ty], span);
    func.insert_before(made, inst);
    func[made].first_result.expect("an instruction created with one result has one")
}

/// Turns an instruction into a different one over different operands, in place.
fn becomes(func: &mut Func, inst: Inst, opcode: Opcode, extra: Extra, args: &[Value]) {
    let args = func.push_values(args);
    let data = &mut func[inst];
    data.opcode = opcode;
    data.args = args;
    data.extra = extra;
    data.flags = data.flags.intersection(Flags::legal_on(opcode));
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{Block, Builder, Module, Signature};
    use rucc_target::{Arch, Env, Os, TargetInfo, Triple};

    use super::{BITS, Flags, Float, FloatPred, Func, Opcode, Type, Value, calls};

    /// The format the pass is about, as a type, which is what every test builds with.
    fn quad() -> Type {
        Type::float(Float::F128)
    }

    fn target() -> TargetInfo {
        TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu))
    }

    fn printed(func: &Func, names: &mut Interner) -> String {
        let module = Module::new(names.intern("q.c"), &target());
        rucc_ir::print_func(&module, func, names)
    }

    /// A function of those parameters returning that, with its entry block and its parameters.
    fn shell(names: &mut Interner, params: &[Type], returns: &[Type]) -> (Func, Block, Vec<Value>) {
        let signature = Signature::new().with_params(params).with_returns(returns);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let values = params.iter().map(|&ty| func.append_param(entry, ty)).collect();
        (func, entry, values)
    }

    /// The pass run over a function of two quads whose one instruction is that binary operation.
    fn binary(opcode: Opcode) -> String {
        let mut names = Interner::new();
        let (mut func, entry, params) = shell(&mut names, &[quad(), quad()], &[quad()]);
        let mut build = Builder::new(&mut func, entry);
        let answer = build.binary(opcode, params[0], params[1], Flags::NONE);
        build.ret(&[answer]);
        calls(&mut func, &mut names);
        printed(&func, &mut names)
    }

    /// The pass run over a function of two quads whose one instruction is that comparison.
    fn compared(pred: FloatPred) -> String {
        let mut names = Interner::new();
        let (mut func, entry, params) = shell(&mut names, &[quad(), quad()], &[Type::I1]);
        let mut build = Builder::new(&mut func, entry);
        let answer = build.fcmp(pred, params[0], params[1], Flags::NONE);
        build.ret(&[answer]);
        calls(&mut func, &mut names);
        printed(&func, &mut names)
    }

    #[test]
    fn the_four_operations_are_the_four_routines() {
        for (opcode, routine) in [
            (Opcode::FAdd, "__addtf3"),
            (Opcode::FSub, "__subtf3"),
            (Opcode::FMul, "__multf3"),
            (Opcode::FDiv, "__divtf3"),
        ] {
            let text = binary(opcode);
            assert!(text.contains(&format!("@{routine}")), "{routine}: {text}");
            // The arithmetic is gone rather than sitting beside the call, which is the whole point:
            // the selector has no rule to match it with.
            assert_eq!(text.matches(" = f").count(), 0, "no float arithmetic left: {text}");
            assert_eq!(text.matches(" = call").count(), 1, "one call: {text}");
        }
    }

    #[test]
    fn a_negation_is_the_routine_rather_than_a_sign_flip() {
        let mut names = Interner::new();
        let (mut func, entry, params) = shell(&mut names, &[quad()], &[quad()]);
        let mut build = Builder::new(&mut func, entry);
        let answer = build.unary(Opcode::FNeg, params[0], quad());
        build.ret(&[answer]);
        calls(&mut func, &mut names);
        let text = printed(&func, &mut names);
        assert!(text.contains("@__negtf2"), "{text}");
        assert!(!text.contains("xor"), "no sign flip in a register: {text}");
    }

    /// The six ordered predicates, each the routine of its name and the test its answer is read
    /// with.
    #[test]
    fn an_ordered_comparison_is_its_own_routine_tested_against_zero() {
        for (pred, routine, test) in [
            (FloatPred::Oeq, "__eqtf2", "icmp eq"),
            (FloatPred::Une, "__netf2", "icmp ne"),
            (FloatPred::Olt, "__lttf2", "icmp slt"),
            (FloatPred::Ole, "__letf2", "icmp sle"),
            (FloatPred::Ogt, "__gttf2", "icmp sgt"),
            (FloatPred::Oge, "__getf2", "icmp sge"),
        ] {
            let text = compared(pred);
            assert!(text.contains(&format!("@{routine}")), "{routine}: {text}");
            assert!(text.contains(test), "{test}: {text}");
            assert!(!text.contains("fcmp"), "the comparison is gone: {text}");
        }
    }

    /// The four that are one of those six answers read as its negation.
    ///
    /// The routine is the opposite one and the test is the same one, which is the part worth a test
    /// of its own: a pass that took the obvious route and kept the routine while flipping the test
    /// would be wrong only for a not a number, which is the operand nothing in a corpus has.
    #[test]
    fn an_unordered_comparison_is_the_opposite_routine_read_the_same_way() {
        for (pred, routine, test) in [
            (FloatPred::Ult, "__getf2", "icmp slt"),
            (FloatPred::Ule, "__gttf2", "icmp sle"),
            (FloatPred::Ugt, "__letf2", "icmp sgt"),
            (FloatPred::Uge, "__lttf2", "icmp sge"),
        ] {
            let text = compared(pred);
            assert!(text.contains(&format!("@{routine}")), "{routine}: {text}");
            assert!(text.contains(test), "{test}: {text}");
        }
    }

    #[test]
    fn whether_two_values_can_be_ordered_at_all_is_one_routine_either_way_round() {
        let unordered = compared(FloatPred::Uno);
        assert!(unordered.contains("@__unordtf2"), "{unordered}");
        assert!(unordered.contains("icmp ne"), "{unordered}");
        let ordered = compared(FloatPred::Ord);
        assert!(ordered.contains("@__unordtf2"), "{ordered}");
        assert!(ordered.contains("icmp eq"), "{ordered}");
    }

    /// Ordered and not equal is the one predicate that needs both calls.
    #[test]
    fn ordered_and_different_is_two_calls_joined() {
        let text = compared(FloatPred::One);
        assert!(text.contains("@__unordtf2"), "{text}");
        assert!(text.contains("@__netf2"), "{text}");
        assert_eq!(text.matches(" = call").count(), 2, "both calls: {text}");
        assert_eq!(text.matches(" = and").count(), 1, "joined: {text}");
        assert!(!text.contains("xor"), "nothing is negated: {text}");
    }

    /// Unordered or equal is the negation of that, which is the same two calls the other way up.
    #[test]
    fn unordered_or_equal_is_the_negation_of_it() {
        let text = compared(FloatPred::Ueq);
        assert_eq!(text.matches(" = call").count(), 2, "both calls: {text}");
        assert_eq!(text.matches(" = or").count(), 1, "joined the other way: {text}");
        assert_eq!(text.matches(" = xor").count(), 2, "both answers negated: {text}");
    }

    #[test]
    fn the_two_comparisons_with_no_operands_to_read_are_constants() {
        let never = compared(FloatPred::False);
        assert!(never.contains("iconst.i1 0"), "{never}");
        assert!(!never.contains("call"), "nothing is called: {never}");
        let always = compared(FloatPred::True);
        assert!(always.contains("iconst.i1 1"), "{always}");
    }

    /// A constant is the bits written into a slot and read back at the format.
    #[test]
    fn a_constant_goes_through_the_frame_a_word_at_a_time() {
        let mut names = Interner::new();
        let (mut func, entry, _) = shell(&mut names, &[], &[quad()]);
        let mut build = Builder::new(&mut func, entry);
        // One in the low word and one in the high word, so a pass that wrote either word twice or
        // wrote one of them into the wrong half is a different answer rather than the same zero.
        let value = build.fconst(quad(), (3u128 << 64) | 5);
        build.ret(&[value]);
        calls(&mut func, &mut names);
        let text = printed(&func, &mut names);
        assert!(!text.contains("fconst"), "the constant is gone: {text}");
        assert_eq!(text.matches("alloca").count(), 1, "one slot: {text}");
        assert_eq!(text.matches("store").count(), 2, "a word at a time: {text}");
        assert!(text.contains("iconst.i64 5"), "the low word first: {text}");
        assert!(text.contains("iconst.i64 3"), "the high word above it: {text}");
        assert_eq!(text.matches("ptr_add").count(), 1, "the high word is eight bytes up: {text}");
        assert_eq!(text.matches(" = load").count(), 1, "read back as one value: {text}");
    }

    #[test]
    fn the_two_narrower_formats_are_a_routine_each_way() {
        for (from, to, routine) in [
            (Float::F32, Float::F128, "__extendsftf2"),
            (Float::F64, Float::F128, "__extenddftf2"),
            (Float::F128, Float::F32, "__trunctfsf2"),
            (Float::F128, Float::F64, "__trunctfdf2"),
        ] {
            let mut names = Interner::new();
            let (mut func, entry, params) =
                shell(&mut names, &[Type::float(from)], &[Type::float(to)]);
            let mut build = Builder::new(&mut func, entry);
            let opcode =
                if to == Float::F128 { Opcode::FPExt } else { Opcode::FPTrunc };
            let answer = build.unary(opcode, params[0], Type::float(to));
            build.ret(&[answer]);
            calls(&mut func, &mut names);
            let text = printed(&func, &mut names);
            assert!(text.contains(&format!("@{routine}")), "{routine}: {text}");
        }
    }

    /// An integer the runtime has no routine at is widened to one it does, with the sign the
    /// conversion has.
    #[test]
    fn a_narrow_integer_is_widened_before_the_conversion() {
        for (opcode, bits, extend, routine) in [
            (Opcode::SIToFP, 16, " = sext", "__floatsitf"),
            (Opcode::UIToFP, 16, " = zext", "__floatunsitf"),
            (Opcode::SIToFP, 32, "", "__floatsitf"),
            (Opcode::UIToFP, 64, "", "__floatunditf"),
        ] {
            let mut names = Interner::new();
            let (mut func, entry, params) = shell(&mut names, &[Type::int(bits)], &[quad()]);
            let mut build = Builder::new(&mut func, entry);
            let answer = build.unary(opcode, params[0], quad());
            build.ret(&[answer]);
            calls(&mut func, &mut names);
            let text = printed(&func, &mut names);
            assert!(text.contains(&format!("@{routine}")), "{routine}: {text}");
            if extend.is_empty() {
                assert!(!text.contains(" = sext"), "nothing to widen: {text}");
                assert!(!text.contains(" = zext"), "nothing to widen: {text}");
            } else {
                assert!(text.contains(extend), "{extend}: {text}");
            }
        }
    }

    /// Coming down, the answer is truncated to the width the program asked for.
    #[test]
    fn a_narrow_answer_is_the_wider_routine_and_a_truncation() {
        let mut names = Interner::new();
        let (mut func, entry, params) = shell(&mut names, &[quad()], &[Type::int(16)]);
        let mut build = Builder::new(&mut func, entry);
        let answer = build.unary(Opcode::FPToSI, params[0], Type::int(16));
        build.ret(&[answer]);
        calls(&mut func, &mut names);
        let text = printed(&func, &mut names);
        assert!(text.contains("@__fixtfsi"), "{text}");
        assert_eq!(text.matches(" = trunc").count(), 1, "cut down afterwards: {text}");
    }

    #[test]
    fn a_conversion_against_a_wide_integer_is_left_exactly_as_it_was() {
        let mut names = Interner::new();
        let (mut func, entry, params) = shell(&mut names, &[Type::int(BITS)], &[quad()]);
        let mut build = Builder::new(&mut func, entry);
        let answer = build.unary(Opcode::SIToFP, params[0], quad());
        build.ret(&[answer]);
        calls(&mut func, &mut names);
        let text = printed(&func, &mut names);
        assert!(!text.contains("call"), "no routine is called: {text}");
        assert!(text.contains("sitofp"), "the conversion is still there to be refused: {text}");
    }

    /// An operation at a format the machine has is not this pass's business.
    #[test]
    fn the_narrower_formats_go_past_untouched() {
        let mut names = Interner::new();
        let double = Type::float(Float::F64);
        let (mut func, entry, params) = shell(&mut names, &[double, double], &[double]);
        let mut build = Builder::new(&mut func, entry);
        let sum = build.binary(Opcode::FAdd, params[0], params[1], Flags::NONE);
        let answer = build.fcmp(FloatPred::Olt, sum, params[1], Flags::NONE);
        build.ret(&[sum]);
        let _ = answer;
        calls(&mut func, &mut names);
        let text = printed(&func, &mut names);
        assert!(!text.contains("call"), "nothing became a call: {text}");
        assert!(text.contains("fadd"), "the add is still an add: {text}");
        assert!(text.contains("fcmp"), "the comparison is still a comparison: {text}");
    }
}
