//! The brain float, as `float` arithmetic and integer work on the sixteen bits that spell it.
//!
//! `__bf16` is the top half of a `float`: the same sign, the same eight bits of exponent, and the
//! first seven bits of the significand. No machine this back end writes for computes with it, and
//! gcc does not call libgcc for most of it either. gcc writes the widening as a shift and the
//! narrowing as a few integer instructions, and this pass writes the same.
//!
//! # The widening is a shift
//!
//! The sixteen bits go to the top of a thirty two bit integer, and that integer read as a `float`
//! is the same number. That is exact for every value, a not a number included, because the two
//! formats differ only in the bits a brain float does not have.
//!
//! # The arithmetic is at `float`
//!
//! For the reason [`crate::half`] gives for the half: twenty four bits of significand are at
//! least two times eight plus two, so an addition, a subtraction, a multiplication or a division
//! computed at `float` and rounded once to the brain float is the operation rounded once. The
//! exponent range is the same in both, so nothing overflows in the middle that would not overflow
//! at the end. A comparison is the comparison of the two `float`s, since the widening is exact.
//!
//! # The narrowing from `float` rounds to nearest even on the bits
//!
//! The bits are the `float`'s bits plus `0x7fff` plus the lowest bit that stays, shifted right by
//! sixteen. An addition that carries out of the significand carries into the exponent, which is
//! the right answer: the largest finite values round up to an infinity, and a subnormal rounds up
//! to the smallest normal. A not a number is the one value this would get wrong, since its
//! significand can carry it into an infinity, so a not a number keeps its top bits and is made
//! quiet instead. That is `__truncsfbf2` in libgcc, written in place.
//!
//! # A wider source rounds to odd on the way to `float`
//!
//! Rounding twice is not rounding once, for the reason [`crate::half`] gives. The way around it
//! without a routine per width is to round to odd in the middle: truncate toward zero, and set the
//! lowest bit when anything was lost. A value rounded to odd at a format with at least two more
//! bits than the last one rounds to nearest even the same as the value itself. `float` has
//! sixteen more bits than the brain float. The truncation toward zero is the narrowing rounded the
//! machine's way, widened back and compared: when the widened value is further from zero than the
//! source, the bits step down by one, which moves a sign and magnitude value toward zero.
//!
//! An integer goes the same way. One of fifty three bits or fewer is exact in a `double`. A wider
//! one is shifted right by 11, 43 or 75 bits, by its size, and every bit shifted out is folded into
//! the lowest one that stays, which is rounding to odd again. The `double` is that times the power
//! of two the shift took away. A `_Decimal` goes to `double` first through its routine, which
//! rounds once on its own, and gcc has no single routine for that pair either.
//!
//! # What is left is a half's moves
//!
//! Every value left at the format is a load, a store, a constant, an argument, a return or a move,
//! and none of those cares what the sixteen bits mean. So the last thing this pass does is give
//! every one of them the half float type, which every convention passes exactly as gcc passes a
//! brain float, in the register a half would be in. A constant keeps its bits. A negation is left
//! to [`crate::expand::floats`], which writes one as an exclusive or with the sign bit for every
//! float of sixty four bits or narrower, and that is the right answer at this format too.

use rucc_ir::{
    CallInfo, Extra, Float, FloatPred, Func, Imm, Inst, InstData, IntPred, Opcode, Signature, Type,
    Value,
};

/// The format this pass is about.
const BRAIN: Float = Float::BF16;

/// The format its values are moved as once the arithmetic is gone, which has the same size.
const HALF: Float = Float::F16;

/// The format the arithmetic is done at, which is the one a brain float is the top half of.
const WIDE: Float = Float::F32;

/// The format an integer and a decimal go through, which holds fifty three bits exactly.
const THROUGH: Float = Float::F64;

/// How many bits of an integer a [`THROUGH`] holds exactly.
const EXACT: u32 = 53;

/// Rewrites every operation at this format, and then moves every value of it as a half.
///
/// The instructions are collected before any of them is touched, because a rewrite puts
/// instructions in front of the one it replaces and the walk would otherwise see its own work.
pub fn floats(func: &mut Func) {
    let has =
        func.values().any(|value| brain(func[value].ty.lane())) || func.signatures().any(crosses);
    if !has {
        return;
    }
    let found: Vec<Inst> = func.blocks().flat_map(|block| func.insts(block)).collect();
    for &inst in &found {
        match func[inst].opcode {
            Opcode::FAdd
            | Opcode::FSub
            | Opcode::FMul
            | Opcode::FDiv
            | Opcode::FRem
            | Opcode::Fma => arithmetic(func, inst),
            Opcode::FCmp => compare(func, inst),
            Opcode::FPExt | Opcode::FPTrunc => convert(func, inst),
            Opcode::SIToFP | Opcode::UIToFP => from_integer(func, inst),
            Opcode::FPToSI | Opcode::FPToUI => to_integer(func, inst),
            _ => {}
        }
    }
    retype(func, &found);
}

/// Whether this type is the format, as a scalar.
fn brain(ty: Type) -> bool {
    ty.is_scalar() && ty.format() == Some(BRAIN)
}

/// The type of an instruction's first result, or nothing where it has none.
fn produced(func: &Func, inst: Inst) -> Option<Type> {
    func[inst].first_result.map(|value| func[value].ty)
}

/// An operation at the format, as the same operation at `float` and the narrowing of its answer.
///
/// The flags the program wrote stay on the operation in the middle, which is the one it asked
/// for. A remainder is exact at either width, so its narrowing loses nothing.
fn arithmetic(func: &mut Func, inst: Inst) {
    let Some(ty) = produced(func, inst) else { return };
    if !brain(ty) {
        return;
    }
    let args = func[func[inst].args].to_vec();
    let wide: Vec<Value> = args.into_iter().map(|arg| widened(func, inst, arg)).collect();
    let opcode = func[inst].opcode;
    let flags = func[inst].flags;
    let args = func.push_values(&wide);
    let data = InstData { args, flags, ..InstData::new(opcode) };
    let answer = written(func, inst, data, Type::float(WIDE));
    rounded_into(func, inst, answer);
}

/// A comparison, as the same comparison of the two `float`s the operands widen to.
fn compare(func: &mut Func, inst: Inst) {
    let args = func[func[inst].args].to_vec();
    let [a, b] = args[..] else { return };
    if !brain(func[a].ty) || !brain(func[b].ty) {
        return;
    }
    let a = widened(func, inst, a);
    let b = widened(func, inst, b);
    func[inst].args = func.push_values(&[a, b]);
}

/// A conversion between this format and another float.
fn convert(func: &mut Func, inst: Inst) {
    let Some(ty) = produced(func, inst) else { return };
    let Some(&arg) = func[func[inst].args].first() else { return };
    let from = func[arg].ty;
    if brain(from) {
        let bits = widened_bits(func, inst, arg);
        if ty.format() == Some(WIDE) {
            becomes(func, inst, Opcode::Bitcast, &[bits]);
            return;
        }
        let wide = ahead(func, inst, Opcode::Bitcast, &[bits], Type::float(WIDE));
        // Toward a decimal of the same width is a widening, which is the way libgcc names it.
        let opcode = if ty.bits() > 32 || decimal(ty) { Opcode::FPExt } else { Opcode::FPTrunc };
        becomes(func, inst, opcode, &[wide]);
        return;
    }
    if !brain(ty) || !from.is_scalar() {
        return;
    }
    let wide = match from.format() {
        Some(WIDE) => arg,
        // A half is exact in a `float`, so this is the one rounding.
        Some(Float::F16) => ahead(func, inst, Opcode::FPExt, &[arg], Type::float(WIDE)),
        Some(format) if format.is_decimal() => {
            let opcode = if from.bits() < 64 { Opcode::FPExt } else { Opcode::FPTrunc };
            let binary = ahead(func, inst, opcode, &[arg], Type::float(THROUGH));
            odd(func, inst, binary)
        }
        Some(_) => odd(func, inst, arg),
        None => return,
    };
    rounded_into(func, inst, wide);
}

/// An integer becoming the format, through a `double` that holds it rounded to odd.
fn from_integer(func: &mut Func, inst: Inst) {
    let Some(ty) = produced(func, inst) else { return };
    let Some(&arg) = func[func[inst].args].first() else { return };
    let from = func[arg].ty;
    if !brain(ty) || !from.is_int() || !from.is_scalar() {
        return;
    }
    let opcode = func[inst].opcode;
    let signed = opcode == Opcode::SIToFP;
    let through = Type::float(THROUGH);
    let double = if from.bits() <= EXACT {
        ahead(func, inst, opcode, &[arg], through)
    } else {
        let Some(double) = folded(func, inst, arg, signed) else { return };
        double
    };
    let wide = odd(func, inst, double);
    rounded_into(func, inst, wide);
}

/// An integer wider than [`EXACT`] bits as a `double` rounded to odd, or nothing for one wider
/// than a hundred and twenty eight.
///
/// The value is exact when it fits. When it does not, it is shifted right until it does, the bits
/// shifted out are folded into the lowest one, and the `double` is scaled back up by the power of
/// two the shift took. One shift does not do for every size: a shift that brings the widest value
/// under fifty three bits leaves too few bits of a smaller one. So the shift is chosen by the size
/// of the value from [`SHIFTS`], and each one leaves at least eleven bits, which is two more than
/// the brain float has and so enough for the rounding to odd. An arithmetic shift of a negative
/// value rounds toward minus infinity rather than toward zero, and setting the lowest bit after it
/// gives the same magnitude either way, so the signed case needs nothing more.
///
/// The choices are integer selects, and the scale is chosen as its bits, because not every machine
/// has a select of a `double`.
fn folded(func: &mut Func, inst: Inst, arg: Value, signed: bool) -> Option<Value> {
    let width = match func[arg].ty.bits() {
        0..=64 => 64,
        65..=128 => 128,
        _ => return None,
    };
    let held = Type::int(width);
    let x = if func[arg].ty.bits() == width {
        arg
    } else {
        let opcode = if signed { Opcode::SExt } else { Opcode::ZExt };
        ahead(func, inst, opcode, &[arg], held)
    };
    let word = Type::int(64);
    let narrowed = |func: &mut Func, value: Value| {
        if width == 64 { value } else { ahead(func, inst, Opcode::Trunc, &[value], word) }
    };
    let zero = constant(func, inst, 0, held);
    let right = if signed { Opcode::AShr } else { Opcode::LShr };
    // From the widest shift down, so that each select keeps the one below it where it fits.
    let mut chosen = None;
    let mut factor = None;
    for &(below, shift) in SHIFTS.iter().rev() {
        if below > width {
            continue;
        }
        let lost = constant(func, inst, (1i128 << shift) - 1, held);
        let low = ahead(func, inst, Opcode::And, &[x, lost], held);
        let inexact = compared(func, inst, IntPred::Ne, low, zero);
        let sticky = ahead(func, inst, Opcode::ZExt, &[inexact], held);
        let by = constant(func, inst, i128::from(shift), held);
        let top = ahead(func, inst, right, &[x, by], held);
        let kept = ahead(func, inst, Opcode::Or, &[top, sticky], held);
        let kept = narrowed(func, kept);
        let scale = constant(func, inst, power(shift), word);
        let (Some(wider), Some(scaled)) = (chosen, factor) else {
            chosen = Some(kept);
            factor = Some(scale);
            continue;
        };
        let fits = within(func, inst, x, held, below, signed);
        chosen = Some(ahead(func, inst, Opcode::Select, &[fits, kept, wider], word));
        factor = Some(ahead(func, inst, Opcode::Select, &[fits, scale, scaled], word));
    }
    let (wider, scaled) = (chosen?, factor?);
    let fits = within(func, inst, x, held, EXACT, signed);
    let exact = narrowed(func, x);
    let chosen = ahead(func, inst, Opcode::Select, &[fits, exact, wider], word);
    let one = constant(func, inst, power(0), word);
    let factor = ahead(func, inst, Opcode::Select, &[fits, one, scaled], word);
    // Every choice is below two to the fifty three in magnitude, so the signed conversion is exact
    // whichever the program wrote.
    let through = Type::float(THROUGH);
    let double = ahead(func, inst, Opcode::SIToFP, &[chosen], through);
    let factor = ahead(func, inst, Opcode::Bitcast, &[factor], through);
    Some(ahead(func, inst, Opcode::FMul, &[double, factor], through))
}

/// The shifts [`folded`] takes, each with the size of the values it is for: a value below two to
/// the first number, and not below the one before it, is shifted right by the second. Each leaves
/// a value of at least eleven bits and below two to the fifty three.
const SHIFTS: &[(u32, u32)] = &[(64, 11), (96, 43), (128, 75)];

/// Whether `x`, of type `held`, is below two to this power in magnitude.
fn within(func: &mut Func, inst: Inst, x: Value, held: Type, bits: u32, signed: bool) -> Value {
    let limit = 1i128 << bits;
    if signed {
        // The range around zero, moved to start at zero.
        let half = constant(func, inst, limit, held);
        let moved = ahead(func, inst, Opcode::Add, &[x, half], held);
        let whole = constant(func, inst, limit << 1, held);
        compared(func, inst, IntPred::Ult, moved, whole)
    } else {
        let whole = constant(func, inst, limit, held);
        compared(func, inst, IntPred::Ult, x, whole)
    }
}

/// The bits of a `double` that is two to this power.
fn power(exponent: u32) -> i128 {
    i128::from(1023 + exponent) << 52
}

/// The format becoming an integer, which is the widening and the conversion the machine has.
fn to_integer(func: &mut Func, inst: Inst) {
    let Some(ty) = produced(func, inst) else { return };
    let Some(&arg) = func[func[inst].args].first() else { return };
    if !brain(func[arg].ty) || !ty.is_int() || !ty.is_scalar() {
        return;
    }
    let opcode = func[inst].opcode;
    let wide = widened(func, inst, arg);
    becomes(func, inst, opcode, &[wide]);
}

/// Every value, signature and call left at the format, as a half.
fn retype(func: &mut Func, insts: &[Inst]) {
    let values: Vec<Value> = func.values().collect();
    for value in values {
        let ty = func[value].ty;
        if ty.lane().format() == Some(BRAIN) {
            func.retype(value, ty.with_lane(Type::float(HALF)));
        }
    }
    let own = halved(func.signature());
    func.set_signature(own);
    // A call is given a signature of its own rather than having the one it names changed in
    // place, because a signature can be named by more than one call.
    for &inst in insts {
        let Extra::Call(info) = func[inst].extra else { continue };
        let call = func[info];
        if !crosses(&func[call.signature]) {
            continue;
        }
        let half = halved(&func[call.signature]);
        let signature = func.add_signature(half);
        func[inst].extra = Extra::Call(func.add_call(CallInfo { signature, ..call }));
    }
}

/// The same signature with the format a half wherever it appears.
fn halved(signature: &Signature) -> Signature {
    let mut out = signature.clone();
    for param in out.params.iter_mut().chain(out.returns.iter_mut()) {
        if param.ty.lane().format() == Some(BRAIN) {
            param.ty = param.ty.with_lane(Type::float(HALF));
        }
    }
    out
}

/// Whether a signature takes or gives back a value at the format.
fn crosses(signature: &Signature) -> bool {
    signature
        .params
        .iter()
        .chain(signature.returns.iter())
        .any(|param| param.ty.lane().format() == Some(BRAIN))
}

/// Whether this type is a decimal float.
fn decimal(ty: Type) -> bool {
    ty.format().is_some_and(Float::is_decimal)
}

/// A value at the format as the thirty two bit integer whose bits are its `float`.
fn widened_bits(func: &mut Func, inst: Inst, value: Value) -> Value {
    let short = ahead(func, inst, Opcode::Bitcast, &[value], Type::int(16));
    let word = Type::int(32);
    let long = ahead(func, inst, Opcode::ZExt, &[short], word);
    let sixteen = constant(func, inst, 16, word);
    ahead(func, inst, Opcode::Shl, &[long, sixteen], word)
}

/// A value at the format as the `float` it is, in front of an instruction.
fn widened(func: &mut Func, inst: Inst, value: Value) -> Value {
    let bits = widened_bits(func, inst, value);
    ahead(func, inst, Opcode::Bitcast, &[bits], Type::float(WIDE))
}

/// A wider binary float as a `float` rounded to odd, in front of an instruction.
///
/// The machine's narrowing rounds to nearest. Where that went further from zero than the source,
/// the bits step down by one, which is the truncation, and where it lost anything, the lowest bit
/// is set. The narrowing keeps the sign, so the sign of the answer says which way is away from
/// zero. A not a number compares unequal to everything, and its bits stay a not a number.
fn odd(func: &mut Func, inst: Inst, value: Value) -> Value {
    let ty = func[value].ty;
    let wide = Type::float(WIDE);
    let near = ahead(func, inst, Opcode::FPTrunc, &[value], wide);
    let back = ahead(func, inst, Opcode::FPExt, &[near], ty);
    let lost = float_compared(func, inst, FloatPred::Une, back, value);
    let above = float_compared(func, inst, FloatPred::Ogt, back, value);
    let word = Type::int(32);
    let bits = ahead(func, inst, Opcode::Bitcast, &[near], word);
    let by = constant(func, inst, 31, word);
    let negative = ahead(func, inst, Opcode::LShr, &[bits, by], word);
    let above = ahead(func, inst, Opcode::ZExt, &[above], word);
    let lost = ahead(func, inst, Opcode::ZExt, &[lost], word);
    // Above the source is away from zero for a positive one and toward it for a negative one. Only
    // read where something was lost, so the two are never equal there.
    let away = ahead(func, inst, Opcode::Xor, &[above, negative], word);
    let away = ahead(func, inst, Opcode::And, &[away, lost], word);
    let toward = ahead(func, inst, Opcode::Sub, &[bits, away], word);
    let odd = ahead(func, inst, Opcode::Or, &[toward, lost], word);
    ahead(func, inst, Opcode::Bitcast, &[odd], wide)
}

/// Turns an instruction into the narrowing of a `float` to the format, in place.
///
/// Rounded to nearest even on the bits, and a not a number made quiet with its top bits kept,
/// which is what gcc writes.
fn rounded_into(func: &mut Func, inst: Inst, value: Value) {
    let word = Type::int(32);
    let bits = ahead(func, inst, Opcode::Bitcast, &[value], word);
    let sixteen = constant(func, inst, 16, word);
    let top = ahead(func, inst, Opcode::LShr, &[bits, sixteen], word);
    let one = constant(func, inst, 1, word);
    let kept = ahead(func, inst, Opcode::And, &[top, one], word);
    let half = constant(func, inst, 0x7fff, word);
    let bias = ahead(func, inst, Opcode::Add, &[kept, half], word);
    let sum = ahead(func, inst, Opcode::Add, &[bits, bias], word);
    let rounded = ahead(func, inst, Opcode::LShr, &[sum, sixteen], word);
    let quiet = constant(func, inst, 0x40, word);
    let quieted = ahead(func, inst, Opcode::Or, &[top, quiet], word);
    let nan = float_compared(func, inst, FloatPred::Uno, value, value);
    let chosen = ahead(func, inst, Opcode::Select, &[nan, quieted, rounded], word);
    let short = ahead(func, inst, Opcode::Trunc, &[chosen], Type::int(16));
    becomes(func, inst, Opcode::Bitcast, &[short]);
}

/// An integer comparison in front of an instruction.
fn compared(func: &mut Func, inst: Inst, pred: IntPred, a: Value, b: Value) -> Value {
    let args = func.push_values(&[a, b]);
    let data = InstData { args, extra: Extra::IntPred(pred), ..InstData::new(Opcode::ICmp) };
    written(func, inst, data, Type::I1)
}

/// A float comparison in front of an instruction.
fn float_compared(func: &mut Func, inst: Inst, pred: FloatPred, a: Value, b: Value) -> Value {
    let args = func.push_values(&[a, b]);
    let data = InstData { args, extra: Extra::FloatPred(pred), ..InstData::new(Opcode::FCmp) };
    written(func, inst, data, Type::I1)
}

/// An integer constant in front of an instruction.
fn constant(func: &mut Func, inst: Inst, value: i128, ty: Type) -> Value {
    let extra = Extra::Imm(func.add_imm(Imm::int(value, ty)));
    written(func, inst, InstData { extra, ..InstData::new(Opcode::IConst) }, ty)
}

/// An instruction of that opcode over those operands, put in front of another one.
fn ahead(func: &mut Func, inst: Inst, opcode: Opcode, args: &[Value], ty: Type) -> Value {
    let args = func.push_values(args);
    written(func, inst, InstData { args, ..InstData::new(opcode) }, ty)
}

/// Creates an instruction, puts it in front of another one, and reads its value back out.
fn written(func: &mut Func, inst: Inst, data: InstData, ty: Type) -> Value {
    let span = func.span(inst);
    let made = func.create_inst(data, &[ty], span);
    func.insert_before(made, inst);
    func[made].first_result.expect("an instruction created with one result has one")
}

/// Turns an instruction into a different one over different operands, in place.
fn becomes(func: &mut Func, inst: Inst, opcode: Opcode, args: &[Value]) {
    let args = func.push_values(args);
    let data = &mut func[inst];
    data.opcode = opcode;
    data.args = args;
    data.extra = Extra::None;
    data.flags = data.flags.intersection(rucc_ir::Flags::legal_on(opcode));
}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;
    use std::collections::HashMap;

    use rucc_base::Interner;
    use rucc_base::float::{Float as Number, Format};
    use rucc_ir::{
        Builder, Extra, Flags, Float, FloatPred, Func, IntPred, Module, Opcode, Signature, Type,
        Value,
    };
    use rucc_target::{Arch, Env, Os, TargetInfo, Triple};

    use super::floats;

    fn ty(format: Float) -> Type {
        Type::float(format)
    }

    /// One function of one or two parameters whose body is one instruction and its return.
    fn one(params: &[Type], result: Type, opcode: Opcode) -> (Func, Vec<Value>) {
        let mut names = Interner::new();
        let mut func = Func::new(
            names.intern("f"),
            Signature::new().with_params(params).with_returns(&[result]),
        );
        let entry = func.create_block();
        let args: Vec<_> = params.iter().map(|&ty| func.append_param(entry, ty)).collect();
        let mut build = Builder::new(&mut func, entry);
        let answer = match args[..] {
            [a] => build.unary(opcode, a, result),
            [a, b] => build.binary(opcode, a, b, Flags::NONE),
            _ => unreachable!("one or two parameters"),
        };
        build.ret(&[answer]);
        floats(&mut func);
        let module = Module::new(names.intern("f.c"), &target());
        rucc_ir::verify_func(&module, &func, &names).expect("the rewrite builds valid IR");
        (func, args)
    }

    fn target() -> TargetInfo {
        TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu))
    }

    fn mask(bits: u32) -> u128 {
        if bits >= 128 { u128::MAX } else { (1 << bits) - 1 }
    }

    fn number(ty: Type, bits: u128) -> Number {
        Number::from_bits(ty.format().expect("a float").encoding(), bits)
    }

    /// Runs a function of one block on these arguments, which is all the rewrite writes.
    fn eval((func, params): &(Func, Vec<Value>), args: &[u128]) -> u128 {
        let entry = func.entry().expect("a body");
        let mut held: HashMap<Value, u128> =
            params.iter().copied().zip(args.iter().copied()).collect();
        for inst in func.insts(entry) {
            let data = &func[inst];
            let ops: Vec<_> = func[data.args].to_vec();
            let at = |n: usize| held[&ops[n]];
            let of = |n: usize| func[ops[n]].ty;
            if data.opcode == Opcode::Return {
                return at(0);
            }
            let out = data.first_result.expect("a value");
            let to = func[out].ty;
            let width = to.bits();
            let signed = |n: usize| {
                let bits = of(n).bits();
                ((at(n) << (128 - bits)) as i128) >> (128 - bits)
            };
            let imm = || match data.extra {
                Extra::Imm(imm) => func[imm].bits(),
                _ => unreachable!("a constant"),
            };
            let value = match data.opcode {
                Opcode::IConst | Opcode::FConst => imm(),
                Opcode::Bitcast | Opcode::ZExt | Opcode::Trunc => at(0),
                Opcode::SExt => signed(0) as u128,
                Opcode::Shl => at(0) << at(1),
                Opcode::LShr => at(0) >> at(1),
                Opcode::AShr => (signed(0) >> at(1)) as u128,
                Opcode::And => at(0) & at(1),
                Opcode::Or => at(0) | at(1),
                Opcode::Xor => at(0) ^ at(1),
                Opcode::Add => at(0).wrapping_add(at(1)),
                Opcode::Sub => at(0).wrapping_sub(at(1)),
                Opcode::Select => {
                    if at(0) & 1 == 1 {
                        at(1)
                    } else {
                        at(2)
                    }
                }
                Opcode::ICmp => {
                    let Extra::IntPred(pred) = data.extra else { unreachable!() };
                    u128::from(match pred {
                        IntPred::Ne => at(0) != at(1),
                        IntPred::Ult => at(0) < at(1),
                        _ => unreachable!("{pred:?}"),
                    })
                }
                Opcode::FCmp => {
                    let Extra::FloatPred(pred) = data.extra else { unreachable!() };
                    let order = number(of(0), at(0)).compare(number(of(1), at(1)));
                    u128::from(match pred {
                        FloatPred::Une => order != Some(Ordering::Equal),
                        FloatPred::Ogt => order == Some(Ordering::Greater),
                        FloatPred::Uno => order.is_none(),
                        _ => unreachable!("{pred:?}"),
                    })
                }
                Opcode::FPTrunc | Opcode::FPExt => {
                    let format = to.format().expect("a float").encoding();
                    number(of(0), at(0)).to_format(format).0.to_bits()
                }
                Opcode::SIToFP => {
                    let format = to.format().expect("a float").encoding();
                    Number::from_signed(signed(0), format).0.to_bits()
                }
                Opcode::UIToFP => {
                    let format = to.format().expect("a float").encoding();
                    Number::from_unsigned(at(0), format).0.to_bits()
                }
                Opcode::FMul => number(of(0), at(0)).product(number(of(1), at(1))).0.to_bits(),
                Opcode::FAdd => number(of(0), at(0)).sum(number(of(1), at(1))).0.to_bits(),
                other => unreachable!("{}", other.name()),
            };
            held.insert(out, value & mask(width));
        }
        unreachable!("a return")
    }

    /// What gcc's narrowing gives for a number of this format that is not a not a number.
    fn expected(format: Format, bits: u128) -> u128 {
        Number::from_bits(format, bits).to_format(Format::BFloat16).0.to_bits()
    }

    /// Whether these are the bits of a quiet not a number of this sign at the format.
    fn quiet(bits: u128, negative: bool) -> bool {
        (bits & 0x7fc0) == 0x7fc0 && ((bits >> 15) == 1) == negative
    }

    /// A spread of sixty four bit patterns: a walk of every exponent with a few significands
    /// each, and the ones a rounding gets wrong first.
    fn patterns() -> Vec<u64> {
        let mut out = Vec::new();
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        for _ in 0..4000 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            out.push(seed);
        }
        out
    }

    /// Doubles where a narrowing to `float` first and to the brain float second rounds twice,
    /// around every boundary that matters.
    fn doubles() -> Vec<u64> {
        let mut out = patterns();
        for exponent in [0u64, 1, 0x380, 0x381, 0x36a, 0x3ff, 0x47e, 0x47f, 0x7fe, 0x7ff] {
            for significand in [
                0,
                1,
                0x0008_0000_0000_0000,
                0x0008_0000_0000_0001,
                0x0007_ffff_ffff_ffff,
                0x0010_0000_1000_0000,
                0x000f_ffff_f000_0000,
                0x0000_1000_0000_0000,
                0x0000_1000_0000_0001,
                0x0000_0fff_ffff_ffff,
                0x0000_3000_0000_0000,
                0x0000_1000_1000_0000,
            ] {
                for sign in [0, 1u64 << 63] {
                    out.push(sign | exponent << 52 | significand);
                }
            }
        }
        out
    }

    /// The narrowing of a `double` rounds once, which a narrowing through `float` does not.
    #[test]
    fn a_double_becomes_a_brain_float_rounded_once() {
        let func = one(&[ty(Float::F64)], ty(Float::BF16), Opcode::FPTrunc);
        // The tie a narrowing through `float` gets wrong, 1 + 2^-8 + 2^-30, is above the midpoint.
        let tie = 0x3ff0_1000_0400_0000u128;
        assert_eq!(eval(&func, &[tie]), 0x3f81);
        for bits in doubles() {
            let bits = u128::from(bits);
            let got = eval(&func, &[bits]);
            // The machine's own narrowing to `float` picks the bits of a not a number, and it
            // stays a quiet one of the same sign.
            if Number::from_bits(Format::Double, bits).is_nan() {
                assert!(quiet(got, bits >> 63 == 1), "{bits:#x}: {got:#x}");
                continue;
            }
            assert_eq!(got, expected(Format::Double, bits), "{bits:#x}");
        }
    }

    /// The narrowing of a `float` is to nearest even on the bits, and a not a number stays one.
    #[test]
    fn a_float_becomes_a_brain_float_to_nearest_even() {
        let func = one(&[ty(Float::F32)], ty(Float::BF16), Opcode::FPTrunc);
        let mut all: Vec<u128> = patterns().into_iter().map(|it| u128::from(it as u32)).collect();
        all.extend([0x3f80_8000, 0x3f81_8000, 0x7f7f_ffff, 0x7f80_0001, 0xffff_ffff, 0x0000_8000]);
        for bits in all {
            // A not a number keeps its top bits and is made quiet, as gcc writes it.
            let want = if Number::from_bits(Format::Single, bits).is_nan() {
                (bits >> 16) | 0x40
            } else {
                expected(Format::Single, bits)
            };
            assert_eq!(eval(&func, &[bits]), want, "{bits:#x}");
        }
    }

    /// An integer of any width rounds once, through a `double` that holds it rounded to odd.
    #[test]
    fn an_integer_becomes_a_brain_float_rounded_once() {
        for (width, signed) in [(32, true), (64, true), (64, false), (128, true), (128, false)] {
            let opcode = if signed { Opcode::SIToFP } else { Opcode::UIToFP };
            let func = one(&[Type::int(width)], ty(Float::BF16), opcode);
            let mut all: Vec<u128> = Vec::new();
            for bits in patterns() {
                let wide = u128::from(bits) << 64 | u128::from(bits.rotate_left(17));
                for shift in [0, 9, 40, 70, 100] {
                    all.push((wide >> shift) & mask(width));
                }
            }
            // Just past fifty three bits, where a `double` itself starts to round, and either side
            // of each size where the shift changes.
            all.extend([(1 << 53) + (1 << 45) + 1, (1 << 60) | (1 << 52) | 1, 1, 0]);
            for bits in [53, 64, 96] {
                all.extend([(1 << bits) - 1, 1 << bits, (1 << bits) + (1 << (bits - 9)) + 1]);
            }
            all.extend([0x1fd_b0a0_2faa_a38f, (0x1fd_b0a0_2faa_a38f << 40) | 1]);
            for bits in all {
                let bits = bits & mask(width);
                let value = ((bits << (128 - width)) as i128) >> (128 - width);
                let want = if signed {
                    Number::from_signed(value, Format::BFloat16).0.to_bits()
                } else {
                    Number::from_unsigned(bits, Format::BFloat16).0.to_bits()
                };
                assert_eq!(eval(&func, &[bits]), want, "{width} {signed} {bits:#x}");
            }
        }
    }

    /// An addition is the `float` addition rounded once, and nothing is left at the format.
    #[test]
    fn an_addition_is_at_float_and_the_values_are_moved_as_halves() {
        let brain = ty(Float::BF16);
        let made = one(&[brain, brain], brain, Opcode::FAdd);
        let func = &made.0;
        assert!(func.values().all(|value| func[value].ty.format() != Some(Float::BF16)));
        assert_eq!(func.signature().params[0].ty, ty(Float::F16));
        assert_eq!(func.signature().returns[0].ty, ty(Float::F16));
        for pair in patterns().chunks(2) {
            let [a, b] = [pair[0] as u16, pair[1] as u16].map(u128::from);
            let sum = Number::from_bits(Format::BFloat16, a)
                .sum(Number::from_bits(Format::BFloat16, b))
                .0;
            let want = if sum.is_nan() { None } else { Some(sum.to_bits()) };
            let got = eval(&made, &[a, b]);
            match want {
                Some(want) => assert_eq!(got, want, "{a:#x} + {b:#x}"),
                None => assert!(Number::from_bits(Format::BFloat16, got).is_nan()),
            }
        }
    }
}
