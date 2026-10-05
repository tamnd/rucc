//! The integer widths the machine has, for the ones a program wrote that it does not.
//!
//! Design: `spec/08-ir.md` section 8.2 and `spec/10-backend.md` section 10.2.
//!
//! The IR has an integer of any width, because C does: `_BitInt(40)` is forty bits of value and
//! `unsigned long long b:40` is a bit-field whose arithmetic happens at forty bits, and an IR that
//! rounded either of those up to sixty four would have thrown away the thing that makes them
//! different from a `long long`. A machine has four integer widths and forty is not one of them.
//! This is where the gap is closed.
//!
//! Every value of a width the machine has no register for is put into the narrowest one it does,
//! which is the width rounded up to a byte and then to a power of two, or into a hundred and twenty
//! eight bits for one wider than sixty four, which [`crate::wide`] then splits into two registers.
//! That is the same width the type's own layout already has: a `_BitInt(40)` object is eight bytes, so nothing here
//! changes how wide a load or a store is against the object it reads.
//!
//! # What the spare bits hold
//!
//! Nothing, and that is the decision the rest of this file follows from. A forty bit value in a
//! sixty four bit register has twenty four bits above it, and this pass does not say what is in
//! them. The alternative is to keep the value extended and fix up every instruction that produces
//! one, and it costs more: an add, a subtract, a multiply, a shift left and the three bitwise
//! operations all give the right low forty bits whatever is above them, so an invariant would pay
//! for a mask after each of those to buy a mask before the few that need one.
//!
//! What needs one is every instruction that reads a bit the narrow value does not have. A divide,
//! a remainder, a shift right and a comparison each look at the whole register, so each gets its
//! operands put into shape first, with the sign spread for the signed ones and the spare bits
//! cleared for the unsigned ones, which is the same distinction the opcode already carries. A
//! widening reads the value it widens, so it becomes the shaping itself when the two widths land
//! in the same register. A store writes the spare bits into the object's padding, and they are
//! cleared first so that the same program run twice writes the same bytes, which C leaves
//! unspecified and a compiler should not.
//!
//! A shift count is shaped as well, which reads like an oddity and is not. The count has the type
//! of the value being shifted, so a shift by a forty bit count is a count with twenty four spare
//! bits in it, and the machine reads the low five or six bits of whatever register it is handed. A
//! count that is a constant is already in range and is left alone, which is what every shift a C
//! program writes at these widths turns out to be.
//!
//! # What crosses the boundary
//!
//! A parameter, a return value and an argument of a call are agreed with code this compilation is
//! not looking at, and the agreement is the ABI's rather than this file's. Where the ABI says the
//! bits above a `_BitInt` in its register are anything, which is [`BitInts::SpareBits`] and the
//! x86-64 psABI, the agreement is the one this pass already keeps: the value crosses in the
//! register that holds it, the side that sends it owes nothing above its own bits, and the side
//! that receives it reads only those. So the signatures are widened with everything else and
//! nothing is shaped on the way across. gcc 16.2.0 keeps the same agreement from its side, which
//! is what makes the two compilers' halves of one call agree.
//!
//! Where the ABI has not been taught, which is every other one today, a function with one of
//! these widths at its own boundary or at a call's is left exactly as it was, and the selector
//! refuses it by name. RISC-V extends a `_BitInt` to the whole register and AAPCS64 has a section
//! of its own for it, and neither is the answer above. `tamnd/rucc#425` is the issue for them.
//!
//! A `_BitInt` wider than sixty four bits reaches here at a boundary only on those ABIs as well.
//! The x86-64 psABI passes one as a structure of `long`s, which is a question about the C type and
//! is answered in `rucc-lower`, so what this pass sees there is two `i64`s and not an `i65`.

use rucc_base::Idx;
use rucc_ir::{
    CallInfo, Def, Extra, Flags, Func, Imm, Inst, InstData, IntPred, Opcode, Param, Signature,
    Type, Value,
};
use rucc_target::{BitInts, CallRegs};

/// The width a value of this type is kept in, and [`None`] when the machine has one already.
///
/// One bit is a width the rules name, since a comparison produces it and it is what a `bool`
/// lives in, so it is not one of these. Between sixty four and a hundred and twenty eight bits the
/// value is held in a hundred and twenty eight, which no register holds either but which
/// [`crate::wide`] splits into two that do, and that step runs after this one for that reason. It
/// is also the layout the type already has: a `_BitInt(65)` object is sixteen bytes, as it is in
/// gcc. Above a hundred and twenty eight there is nothing to round up into, and the front end does
/// not offer one, since that is where `BITINT_MAXWIDTH` is.
#[must_use]
fn container(ty: Type) -> Option<u32> {
    if !ty.is_int() || !ty.is_scalar() {
        return None;
    }
    let bits = ty.bits();
    if bits == 1 || bits > 128 {
        return None;
    }
    let held = bits.next_power_of_two().max(8);
    (held != bits).then_some(held)
}

/// The opcodes this pass knows how to put into a machine width.
///
/// An instruction that touches one of these widths and is not one of these is why the pass leaves
/// the whole function alone, so this list is the pass's own statement of what it has thought
/// about. Adding to it is adding an arm to [`rewrite`] as well.
#[must_use]
fn understood(opcode: Opcode) -> bool {
    matches!(
        opcode,
        Opcode::IConst
            | Opcode::Add
            | Opcode::Sub
            | Opcode::Mul
            | Opcode::SDiv
            | Opcode::UDiv
            | Opcode::SRem
            | Opcode::URem
            | Opcode::And
            | Opcode::Or
            | Opcode::Xor
            | Opcode::Shl
            | Opcode::LShr
            | Opcode::AShr
            | Opcode::ICmp
            | Opcode::Trunc
            | Opcode::SExt
            | Opcode::ZExt
            | Opcode::SIToFP
            | Opcode::UIToFP
            | Opcode::FPToSI
            | Opcode::FPToUI
            | Opcode::Load
            | Opcode::Store
            | Opcode::Jump
            | Opcode::BrIf
    )
}

/// Puts every integer of a width the machine has no register for into the width that holds it.
///
/// Gives back whether it changed anything, which is what a test asks and what tells a caller that
/// a function it is about to hand to the selector is not the one the middle end produced.
///
/// The function is left exactly as it was when there is nothing at such a width, and also when
/// something at such a width is reached by an instruction this does not understand, which is the
/// second half of why the answer is a boolean. Leaving it alone is what makes the selector's
/// refusal the thing a user sees, rather than a rewrite that guessed.
///
/// `conv` is the convention of the function, through which the convention of each call it makes
/// is found, and it is what says whether a width may cross either boundary.
pub fn integers(func: &mut Func, conv: &CallRegs) -> bool {
    widen(func, Some(conv))
}

/// [`integers`] for a target with no ABI for these widths, which is wasm today.
///
/// The wasm ABI of a `_BitInt` has not been taught, so no width may cross a boundary here, and a
/// function with one at its own boundary or at a call's is left as it was, for the selector to
/// refuse by name. What is left is the arithmetic of a bit-field wider than 32 bits, which is all
/// of what a C program writes at these widths that is not a `_BitInt`.
pub fn integers_inside(func: &mut Func) -> bool {
    widen(func, None)
}

/// [`integers`], where `conv` is nothing when no convention of the target has been taught.
fn widen(func: &mut Func, conv: Option<&CallRegs>) -> bool {
    let narrow: Vec<Option<u32>> = func
        .values()
        .map(|value| container(func[value].ty).map(|_| func[value].ty.bits()))
        .collect();
    if narrow.iter().all(Option::is_none) {
        return false;
    }
    if !every_crossing_is_taught(func, conv) {
        return false;
    }

    let insts: Vec<Inst> = func.blocks().flat_map(|block| func.insts(block)).collect();
    if !insts.iter().all(|&inst| touches_nothing_it_does_not_understand(func, &narrow, conv, inst))
    {
        return false;
    }

    let values: Vec<Value> = func.values().collect();
    for value in values {
        let ty = func[value].ty;
        if let Some(held) = container(ty) {
            func.retype(value, Type::int(held));
        }
    }
    // The constants first and all of them, because a shift count is a constant and the shift asks
    // what its value is. The instruction order is the order the blocks are laid out in, which is
    // not an order every definition comes before its uses in, so asking that question during the
    // walk below would be asking it of an immediate that may or may not have been rewritten yet.
    for &inst in &insts {
        if func[inst].opcode == Opcode::IConst {
            constant(func, &narrow, inst);
        }
    }
    for &inst in &insts {
        rewrite(func, &narrow, inst);
    }
    // The signatures last, since nothing above reads them. A call is given a signature of its own
    // rather than having the one it names changed in place, because a signature can be named by
    // more than one call and the function's own is the first of them.
    let own = widened(func.signature());
    func.set_signature(own);
    for inst in insts {
        let Extra::Call(info) = func[inst].extra else { continue };
        let call = func[info];
        if !crosses(&func[call.signature]) {
            continue;
        }
        let wide = widened(&func[call.signature]);
        let signature = func.add_signature(wide);
        func[inst].extra = Extra::Call(func.add_call(CallInfo { signature, ..call }));
    }
    true
}

/// The same signature with every width the machine has no register for put into the one that
/// holds it.
fn widened(signature: &Signature) -> Signature {
    let widen = |param: &Param| match container(param.ty) {
        Some(held) => Param { ty: Type::int(held), ..*param },
        None => *param,
    };
    Signature {
        params: signature.params.iter().map(widen).collect(),
        returns: signature.returns.iter().map(widen).collect(),
        ..signature.clone()
    }
}

/// Whether a signature has one of these widths among what it takes or gives back.
fn crosses(signature: &Signature) -> bool {
    signature
        .params
        .iter()
        .chain(signature.returns.iter())
        .any(|param| container(param.ty).is_some())
}

/// Whether a value at one of these widths may cross a boundary made with this signature.
///
/// The signature names its own convention, and a function of one convention calls functions of
/// the other, so it is that convention's ABI that is asked. A convention the platform does not
/// have is one nothing is known about.
fn taught(signature: &Signature, conv: Option<&CallRegs>) -> bool {
    conv.and_then(|conv| conv.under(signature.convention))
        .is_some_and(|conv| conv.abi.bit_ints == BitInts::SpareBits)
}

/// Whether every one of these widths at a boundary is at one whose ABI has been taught.
///
/// The function's own signature and every signature its calls name are the places a width is
/// agreed with something this compilation is not looking at. The entry block's parameters are
/// asked as well as the signature's, because they are the same list said twice and this pass
/// would rather notice the day they stop being.
fn every_crossing_is_taught(func: &Func, conv: Option<&CallRegs>) -> bool {
    if !func.signatures().all(|signature| !crosses(signature) || taught(signature, conv)) {
        return false;
    }
    let Some(entry) = func.entry() else { return true };
    taught(func.signature(), conv)
        || func[entry].params.iter().all(|&value| container(func[value].ty).is_none())
}

/// Whether every value at one of these widths that this instruction touches is one it can handle.
///
/// A return, a call and a `va_arg` are a boundary rather than arithmetic, and are understood where
/// the ABI of the signature they cross has been taught. A call is asked about its own signature rather than
/// being covered by [`every_crossing_is_taught`], because an argument past a `...` is at a width
/// no signature names.
fn touches_nothing_it_does_not_understand(
    func: &Func,
    narrow: &[Option<u32>],
    conv: Option<&CallRegs>,
    inst: Inst,
) -> bool {
    let data = &func[inst];
    let touched = results(func, inst).any(|value| at(narrow, value).is_some())
        || func[data.args].iter().any(|&value| at(narrow, value).is_some());
    if !touched || understood(data.opcode) {
        return true;
    }
    match (data.opcode, data.extra) {
        // What comes off a variable argument list crossed a call to get there, and it is read
        // as the register or the word it travelled in, with whatever the caller left above it.
        // The list was made under the function's own convention, so that is the one asked.
        (Opcode::Return | Opcode::VaArg, _) => taught(func.signature(), conv),
        (Opcode::Call | Opcode::CallIndirect | Opcode::TailCall, Extra::Call(info)) => {
            taught(&func[func[info].signature], conv)
        }
        _ => false,
    }
}

/// The narrow width a value had before it was widened, and [`None`] for one that was never narrow.
///
/// A value this pass created has no entry, which is the right answer for it: it was built at a
/// width the machine has.
#[must_use]
fn at(narrow: &[Option<u32>], value: Value) -> Option<u32> {
    narrow.get(value.index()).copied().flatten()
}

/// The values an instruction produces.
fn results(func: &Func, inst: Inst) -> impl Iterator<Item = Value> + use<'_> {
    let first = func[inst].first_result.map_or(0, Idx::index);
    let count = usize::from(func[inst].results);
    (first..first + count).map(Idx::from_usize)
}

/// One instruction, now that every value it names is at a width the machine has.
fn rewrite(func: &mut Func, narrow: &[Option<u32>], inst: Inst) {
    match func[inst].opcode {
        // The low bits are the answer whatever is above them, so there is nothing to shape. The
        // flags go, because `nsw` was a promise about the narrow width and says nothing about the
        // wide one: an add of two values with rubbish in the spare bits can carry out of the
        // register while the forty bit add it stands for does not.
        Opcode::Add | Opcode::Sub | Opcode::Mul | Opcode::And | Opcode::Or | Opcode::Xor => {
            forget_flags(func, narrow, inst);
        }
        Opcode::SDiv | Opcode::SRem => shape_both(func, narrow, inst, true),
        Opcode::UDiv | Opcode::URem => shape_both(func, narrow, inst, false),
        Opcode::Shl => shape_count(func, narrow, inst),
        Opcode::LShr => {
            shape_operand(func, narrow, inst, 0, false);
            shape_count(func, narrow, inst);
        }
        Opcode::AShr => {
            shape_operand(func, narrow, inst, 0, true);
            shape_count(func, narrow, inst);
        }
        Opcode::ICmp => compare(func, narrow, inst),
        Opcode::Trunc => truncate(func, narrow, inst),
        Opcode::SExt => extend(func, narrow, inst, true),
        Opcode::ZExt => extend(func, narrow, inst, false),
        // A conversion to a float reads every bit of the integer, so the integer is put into
        // shape the way the conversion reads it. The other way needs nothing: a value in range
        // for the narrow width has the same low bits at the wide one, and a value out of range is
        // one C does not give an answer for.
        Opcode::SIToFP => shape_operand(func, narrow, inst, 0, true),
        Opcode::UIToFP => shape_operand(func, narrow, inst, 0, false),
        // The value written goes into the object's padding as well as into the object, and it is
        // cleared so that the padding is the same on every run rather than being whatever was in
        // the register. C says those bits hold nothing in particular; a compiler that writes a
        // different nothing each time is a compiler whose output cannot be compared with itself.
        Opcode::Store => shape_operand(func, narrow, inst, 0, false),
        _ => {}
    }
}

/// A constant, at the width it is now held in.
///
/// The immediate is stored in exactly the width of its type, so the same bits under a wider type
/// are the same non-negative number and a negative one loses its sign extension. Reading it back
/// signed at the narrow width and writing it at the wide one is what keeps `-1` a `-1`, and it is
/// also what makes the spare bits of a constant say what the value says rather than say nothing,
/// which is the one place this pass shapes something it did not have to.
fn constant(func: &mut Func, narrow: &[Option<u32>], inst: Inst) {
    let Some(ty) = produced(func, inst) else { return };
    let Some(was) = produced_narrow(func, narrow, inst) else { return };
    let Extra::Imm(imm) = func[inst].extra else { return };
    let value = func[imm].signed(Type::int(was));
    let imm = func.add_imm(Imm::int(value, ty));
    func[inst].extra = Extra::Imm(imm);
}

/// Drops the arithmetic flags from an instruction whose result was narrower than its register.
fn forget_flags(func: &mut Func, narrow: &[Option<u32>], inst: Inst) {
    if produced_narrow(func, narrow, inst).is_none() {
        return;
    }
    func[inst].flags = func[inst].flags.without(Flags::NSW.union(Flags::NUW).union(Flags::EXACT));
}

/// Both operands put into shape, for the instructions that read every bit of both.
fn shape_both(func: &mut Func, narrow: &[Option<u32>], inst: Inst, signed: bool) {
    shape_operand(func, narrow, inst, 0, signed);
    shape_operand(func, narrow, inst, 1, signed);
    if produced_narrow(func, narrow, inst).is_some() {
        func[inst].flags = func[inst].flags.without(Flags::EXACT);
    }
}

/// The count of a shift put into shape, which the machine reads the low bits of.
fn shape_count(func: &mut Func, narrow: &[Option<u32>], inst: Inst) {
    shape_operand(func, narrow, inst, 1, false);
    if produced_narrow(func, narrow, inst).is_some() {
        func[inst].flags =
            func[inst].flags.without(Flags::NSW.union(Flags::NUW).union(Flags::EXACT));
    }
}

/// A comparison, whose two operands are shaped the way its predicate reads them.
///
/// An equality reads both the same way, so either shape answers it and the cheaper one is used.
fn compare(func: &mut Func, narrow: &[Option<u32>], inst: Inst) {
    let Extra::IntPred(pred) = func[inst].extra else { return };
    let signed = matches!(pred, IntPred::Slt | IntPred::Sle | IntPred::Sgt | IntPred::Sge);
    shape_operand(func, narrow, inst, 0, signed);
    shape_operand(func, narrow, inst, 1, signed);
}

/// Keeping the low bits, where the two widths may or may not have landed in the same register.
///
/// A truncation to a width the machine has always lands in a narrower register than it started
/// in, so it stays a truncation. A truncation to one of these widths may not: forty bits down to
/// thirty three is the same register twice, and what is left of it is the clearing of the bits
/// the narrower value does not have, which is a mask.
fn truncate(func: &mut Func, narrow: &[Option<u32>], inst: Inst) {
    let Some(to) = produced_narrow(func, narrow, inst) else { return };
    let args = func[inst].args;
    let Some(&arg) = func[args].first() else { return };
    let ty = func[arg].ty;
    if produced(func, inst) != Some(ty) {
        return;
    }
    let mask = ahead_const(func, inst, Imm::int(low_bits(to), ty), ty);
    becomes(func, inst, Opcode::And, &[arg, mask]);
}

/// Widening, which reads every bit of what it widens.
///
/// The value it reads is put into shape first, and when the two widths landed in the same
/// register that shaping is the whole of the answer: a thirty three bit value widened to forty one
/// bits, both of them held in sixty four, is that value with its spare bits made into the sign or
/// into zeroes and nothing else. When they landed in different registers the machine's own
/// widening still has to happen, so the shaping goes in front of it.
fn extend(func: &mut Func, narrow: &[Option<u32>], inst: Inst, signed: bool) {
    let args = func[inst].args;
    let Some(&arg) = func[args].first() else { return };
    let Some(from) = at(narrow, arg) else { return };
    let ty = func[arg].ty;
    let Some(wide) = produced(func, inst) else { return };
    if wide != ty {
        let shaped = shaped(func, inst, arg, from, signed);
        let opcode = if signed { Opcode::SExt } else { Opcode::ZExt };
        becomes(func, inst, opcode, &[shaped]);
        return;
    }
    if signed {
        let spare = ahead_const(func, inst, Imm::int(i128::from(ty.bits() - from), ty), ty);
        let up = ahead(func, inst, Opcode::Shl, &[arg, spare], ty);
        becomes(func, inst, Opcode::AShr, &[up, spare]);
        return;
    }
    let mask = ahead_const(func, inst, Imm::int(low_bits(from), ty), ty);
    becomes(func, inst, Opcode::And, &[arg, mask]);
}

/// Puts one operand of an instruction into shape, in place.
fn shape_operand(func: &mut Func, narrow: &[Option<u32>], inst: Inst, index: usize, signed: bool) {
    let list = func[inst].args;
    let mut args: Vec<Value> = func[list].to_vec();
    let Some(&arg) = args.get(index) else { return };
    let Some(width) = at(narrow, arg) else { return };
    let shaped = shaped(func, inst, arg, width, signed);
    if shaped == arg {
        return;
    }
    args[index] = shaped;
    let list = func.push_values(&args);
    func[inst].args = list;
}

/// A value whose spare bits say what the narrow value says, put in front of `inst`.
///
/// The sign spread over them for a signed reading, which is a shift up and an arithmetic shift
/// back down, and zeroes for an unsigned one, which is a mask. A constant already in range is
/// itself, which is what keeps a shift by a written number one instruction.
fn shaped(func: &mut Func, inst: Inst, value: Value, width: u32, signed: bool) -> Value {
    let ty = func[value].ty;
    if already(func, value, width, signed) {
        return value;
    }
    if signed {
        let spare = ahead_const(func, inst, Imm::int(i128::from(ty.bits() - width), ty), ty);
        let up = ahead(func, inst, Opcode::Shl, &[value, spare], ty);
        return ahead(func, inst, Opcode::AShr, &[up, spare], ty);
    }
    let mask = ahead_const(func, inst, Imm::int(low_bits(width), ty), ty);
    ahead(func, inst, Opcode::And, &[value, mask], ty)
}

/// Whether a value already says what the narrow value says in every bit of its register.
///
/// Two shapes are recognised and both are ones this pass or the front end has just written, so
/// neither needs an analysis to answer. A constant is in range or it is not, and a shift count is
/// a constant in every C program that has reached this so far. A mask that keeps no more bits than
/// the width has is a value whose spare bits are already zero, which is what a widening into the
/// same register became a few instructions ago, and it is why a shifted bit-field is one `and`
/// rather than two.
fn already(func: &Func, value: Value, width: u32, signed: bool) -> bool {
    let Def::Result { inst, .. } = func[value].def else { return false };
    let ty = func[value].ty;
    match func[inst].opcode {
        Opcode::IConst => {
            let Extra::Imm(imm) = func[inst].extra else { return false };
            let held = func[imm].signed(ty);
            if signed {
                let spare = 128 - width;
                return (held << spare) >> spare == held;
            }
            held >= 0 && held == held & low_bits(width)
        }
        // A mask says nothing about the sign bit of a narrower value, so it answers the unsigned
        // question only.
        Opcode::And if !signed => {
            let args = func[inst].args;
            func[args].iter().any(|&arg| keeps_no_more_than(func, arg, width))
        }
        _ => false,
    }
}

/// Whether a value is a constant mask that keeps no bit above the low `width` of them.
fn keeps_no_more_than(func: &Func, value: Value, width: u32) -> bool {
    let Def::Result { inst, .. } = func[value].def else { return false };
    if func[inst].opcode != Opcode::IConst {
        return false;
    }
    let Extra::Imm(imm) = func[inst].extra else { return false };
    let held = func[imm].signed(func[value].ty);
    held >= 0 && held & !low_bits(width) == 0
}

/// The low `width` bits set, as an immediate's value.
///
/// The subtraction wraps because a width of a hundred and twenty seven is a one shifted into the
/// sign of the immediate, and one less than that is every bit below it, which is the answer.
#[must_use]
fn low_bits(width: u32) -> i128 {
    (1i128 << width).wrapping_sub(1)
}

/// The type of the one value an instruction produces, and [`None`] when it produces none.
fn produced(func: &Func, inst: Inst) -> Option<Type> {
    func[inst].first_result.map(|value| func[value].ty)
}

/// The narrow width the one value an instruction produces used to have.
fn produced_narrow(func: &Func, narrow: &[Option<u32>], inst: Inst) -> Option<u32> {
    at(narrow, func[inst].first_result?)
}

/// Puts an instruction over these operands in front of another one, and gives back its value.
fn ahead(func: &mut Func, inst: Inst, opcode: Opcode, args: &[Value], ty: Type) -> Value {
    let args = func.push_values(args);
    written(func, inst, InstData { args, ..InstData::new(opcode) }, ty)
}

/// The same for a constant, which carries an immediate rather than operands.
fn ahead_const(func: &mut Func, inst: Inst, imm: Imm, ty: Type) -> Value {
    let extra = Extra::Imm(func.add_imm(imm));
    written(func, inst, InstData { extra, ..InstData::new(Opcode::IConst) }, ty)
}

/// Creates the instruction, puts it where those two asked, and reads its value back out.
fn written(func: &mut Func, inst: Inst, data: InstData, ty: Type) -> Value {
    let span = func.span(inst);
    let made = func.create_inst(data, &[ty], span);
    func.insert_before(made, inst);
    func[made].first_result.expect("an instruction created with one result has one")
}

/// Turns an instruction into a different one over different operands, in place.
///
/// The value the rest of the function reads is the value it already read, so nothing has to be
/// substituted anywhere, and its type is the one this pass has already given it.
fn becomes(func: &mut Func, inst: Inst, opcode: Opcode, args: &[Value]) {
    let args = func.push_values(args);
    let data = &mut func[inst];
    data.opcode = opcode;
    data.args = args;
    data.extra = Extra::None;
    data.flags = data.flags.intersection(Flags::legal_on(opcode));
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{Builder, Flags, Func, IntPred, Module, Opcode, Signature, Type};
    use rucc_target::x86_64::{SYSV, WIN64};
    use rucc_target::{Arch, Env, Os, TargetInfo, Triple};

    use super::{container, integers, integers_inside};

    fn target() -> TargetInfo {
        TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu))
    }

    fn printed(func: &Func, names: &mut Interner) -> String {
        let module = Module::new(names.intern("w.c"), &target());
        rucc_ir::print_func(&module, func, names)
    }

    /// A function of no arguments returning an `int`, with a block to build in.
    fn shell(names: &mut Interner) -> (Func, rucc_ir::Block) {
        let int = Type::int(32);
        let mut func = Func::new(names.intern("f"), Signature::new().with_returns(&[int]));
        let entry = func.create_block();
        (func, entry)
    }

    #[test]
    fn a_width_is_held_in_the_narrowest_register_that_fits_it() {
        assert_eq!(container(Type::int(40)), Some(64));
        assert_eq!(container(Type::int(33)), Some(64));
        assert_eq!(container(Type::int(17)), Some(32));
        assert_eq!(container(Type::int(9)), Some(16));
        assert_eq!(container(Type::int(3)), Some(8));
        // The widths the machine has, which are left alone.
        for bits in [1, 8, 16, 32, 64] {
            assert_eq!(container(Type::int(bits)), None, "{bits} is a width the machine has");
        }
        // Above sixty four it is the pair of registers the next step splits a value into, and
        // above that there is nothing. A vector is not a scalar.
        assert_eq!(container(Type::int(65)), Some(128));
        assert_eq!(container(Type::int(127)), Some(128));
        assert_eq!(container(Type::int(128)), None);
        assert_eq!(container(Type::int(129)), None);
        assert_eq!(container(Type::vector(Type::int(40), 2)), None);
        assert_eq!(container(Type::PTR), None);
    }

    /// The shape `x.b << 32` has for a forty bit bit-field, which is the program in
    /// `gcc.c-torture/execute/pr32244-1.c` with the load taken out.
    #[test]
    fn a_shift_at_a_width_the_machine_lacks_keeps_the_bits_the_width_has() {
        let mut names = Interner::new();
        let (mut func, entry) = shell(&mut names);
        let narrow = Type::int(40);
        let mut build = Builder::new(&mut func, entry);
        let value = build.iconst(narrow, 0x100);
        let count = build.iconst(narrow, 32);
        let shifted = build.binary(Opcode::Shl, value, count, Flags::NONE);
        let wide = build.unary(Opcode::ZExt, shifted, Type::int(64));
        let answer = build.unary(Opcode::Trunc, wide, Type::int(32));
        build.ret(&[answer]);

        assert!(integers(&mut func, &SYSV), "there is a width to widen");
        let text = printed(&func, &mut names);
        assert!(!text.contains("i40"), "no forty bit value is left: {text}");
        // The widening became the mask, because both widths are held in the same register and
        // clearing the spare bits is the whole of what the widening meant.
        assert_eq!(text.matches(" = and ").count(), 1, "the widening became a mask: {text}");
        assert!(!text.contains("zext"), "and is no longer a widening: {text}");
    }

    /// A value the constant check has no answer for, so that shaping it is a real instruction.
    ///
    /// A truncation down to one of these widths is one, and it is also the shortest way to make
    /// one: the pass turns it into a mask, since both widths land in the same register.
    fn seed(build: &mut Builder<'_>, narrow: Type) -> rucc_ir::Value {
        let wide = build.iconst(Type::int(64), 5);
        build.unary(Opcode::Trunc, wide, narrow)
    }

    /// An arithmetic shift right reads the sign of the narrow value, which is not the sign of the
    /// register it is in.
    #[test]
    fn a_signed_shift_right_spreads_the_sign_the_narrow_value_has() {
        let mut names = Interner::new();
        let (mut func, entry) = shell(&mut names);
        let narrow = Type::int(40);
        let mut build = Builder::new(&mut func, entry);
        let value = seed(&mut build, narrow);
        let count = build.iconst(narrow, 3);
        let shifted = build.binary(Opcode::AShr, value, count, Flags::NONE);
        let answer = build.unary(Opcode::Trunc, shifted, Type::int(32));
        build.ret(&[answer]);

        assert!(integers(&mut func, &SYSV), "there is a width to widen");
        let text = printed(&func, &mut names);
        assert!(!text.contains("i40"), "no forty bit value is left: {text}");
        // A shift up by twenty four and back down, which is what putting the sign of a forty bit
        // value into the whole of a sixty four bit register is. The count itself is a constant
        // and is left as it was, since three is three at either width.
        assert!(text.contains("iconst.i64 24"), "the spare bits are counted: {text}");
        assert_eq!(text.matches(" = shl ").count(), 1, "shifted up once: {text}");
        assert_eq!(text.matches(" = ashr ").count(), 2, "and back down, then by three: {text}");
    }

    /// An unsigned comparison reads the whole register, so its operands have their spare bits
    /// cleared, and a signed one has the sign put into them instead.
    #[test]
    fn a_comparison_shapes_its_operands_the_way_its_predicate_reads_them() {
        // The seed is a mask already, which answers the unsigned question and not the signed one,
        // so only the signed predicate pays for anything. The other side is the constant seven,
        // which is seven at every width and either sign.
        for (pred, shifts) in [(IntPred::Ult, 0), (IntPred::Eq, 0), (IntPred::Slt, 1)] {
            let mut names = Interner::new();
            let (mut func, entry) = shell(&mut names);
            let narrow = Type::int(33);
            let mut build = Builder::new(&mut func, entry);
            let left = seed(&mut build, narrow);
            let right = build.iconst(narrow, 7);
            let same = build.icmp(pred, left, right);
            let answer = build.unary(Opcode::ZExt, same, Type::int(32));
            build.ret(&[answer]);

            assert!(integers(&mut func, &SYSV), "there is a width to widen");
            let text = printed(&func, &mut names);
            assert!(!text.contains("i33"), "no thirty three bit value is left: {text}");
            assert_eq!(text.matches(" = and ").count(), 1, "{pred:?} masks once: {text}");
            assert_eq!(text.matches(" = shl ").count(), shifts, "{pred:?} shifts up: {text}");
        }
    }

    /// `_BitInt(65) c = (_BitInt(65))1 << 63;`, which is the widening of a `long long` to sixty
    /// five bits and a shift, and which stopped the whole file before. It is held in a hundred and
    /// twenty eight bits, where the widening is the machine's own and the pair splitting after this
    /// step does the rest, and the signed comparison spreads the sign across sixty three spare
    /// bits, which is the widest shaping this does and the one `low_bits` has to get right.
    #[test]
    fn a_width_above_sixty_four_is_held_in_a_hundred_and_twenty_eight() {
        let mut names = Interner::new();
        let (mut func, entry) = shell(&mut names);
        let narrow = Type::int(65);
        let mut build = Builder::new(&mut func, entry);
        let one = build.iconst(Type::int(64), 1);
        let wide = build.unary(Opcode::SExt, one, narrow);
        let count = build.iconst(narrow, 63);
        let shifted = build.binary(Opcode::Shl, wide, count, Flags::NONE);
        let zero = build.iconst(narrow, 0);
        let negative = build.icmp(IntPred::Slt, shifted, zero);
        let answer = build.unary(Opcode::ZExt, negative, Type::int(32));
        build.ret(&[answer]);

        assert!(integers(&mut func, &SYSV), "there is a width to widen");
        let text = printed(&func, &mut names);
        assert!(!text.contains("i65"), "no sixty five bit value is left: {text}");
        assert!(text.contains("sext.i128"), "the widening is to the pair: {text}");
        assert!(text.contains("iconst.i128 63"), "the spare bits are counted: {text}");
        assert_eq!(text.matches(" = ashr ").count(), 1, "the sign is spread once: {text}");
        assert_eq!(super::low_bits(127), i128::MAX);
    }

    /// `(double)x` for a `_BitInt(40)` holding a negative number, which stopped the whole function
    /// before because the conversion was not on the list. The integer has its sign spread first,
    /// since the conversion reads the whole register, and the conversion back needs nothing.
    #[test]
    fn a_conversion_to_a_float_reads_the_integer_the_way_its_sign_says() {
        for (to, from, shifts) in
            [(Opcode::SIToFP, Opcode::FPToSI, 1), (Opcode::UIToFP, Opcode::FPToUI, 0)]
        {
            let mut names = Interner::new();
            let (mut func, entry) = shell(&mut names);
            let narrow = Type::int(40);
            let mut build = Builder::new(&mut func, entry);
            let value = seed(&mut build, narrow);
            let real = build.unary(to, value, Type::float(rucc_ir::Float::F64));
            let back = build.unary(from, real, narrow);
            let answer = build.unary(Opcode::Trunc, back, Type::int(32));
            build.ret(&[answer]);

            assert!(integers(&mut func, &SYSV), "{to:?} is understood");
            let text = printed(&func, &mut names);
            assert!(!text.contains("i40"), "no forty bit value is left: {text}");
            assert_eq!(
                text.matches(" = shl ").count(),
                shifts,
                "{to:?} shapes its operand: {text}"
            );
        }
    }

    /// `_BitInt(40) f(_BitInt(40) x) { return g(x) + 1; }`, with `g` defined somewhere else.
    fn crossing(names: &mut Interner) -> Func {
        let narrow = Type::int(40);
        let signature = Signature::new().with_params(&[narrow]).with_returns(&[narrow]);
        let mut func = Func::new(names.intern("f"), signature.clone());
        let g = func.add_signature(signature);
        let callee = names.intern("g");
        let entry = func.create_block();
        let x = func.append_param(entry, narrow);
        let call = Builder::new(&mut func, entry).call(callee, g, &[x]);
        let got = func[call].results().next().expect("the call gives back a value");
        let mut build = Builder::new(&mut func, entry);
        let one = build.iconst(narrow, 1);
        let sum = build.binary(Opcode::Add, got, one, Flags::NONE);
        build.ret(&[sum]);
        func
    }

    /// On x86-64 the bits above a `_BitInt` in its register are anything both ways, which is the
    /// agreement this pass keeps already, so the signatures are widened with everything else and
    /// nothing is shaped on the way across.
    #[test]
    fn a_width_that_crosses_a_boundary_crosses_in_its_register_where_the_abi_says_so() {
        let mut names = Interner::new();
        let mut func = crossing(&mut names);
        assert!(integers(&mut func, &SYSV), "the psABI has been taught");
        let text = printed(&func, &mut names);
        assert!(!text.contains("i40"), "no forty bit value is left: {text}");
        let own = func.signature();
        assert_eq!(own.params[0].ty, Type::int(64), "the parameter is held in a register");
        assert_eq!(own.returns[0].ty, Type::int(64), "and so is what comes back");
        assert!(
            func.signatures()
                .skip(1)
                .any(|sig| sig.params.first().is_some_and(|param| param.ty == Type::int(64))),
            "the call is made with the widened signature: {text}"
        );
        assert!(!text.contains(" = and "), "nothing is shaped on the way across: {text}");
    }

    /// Windows has not been taught, so the function is left for the selector to refuse.
    #[test]
    fn a_width_that_crosses_a_boundary_the_abi_was_not_taught_is_left_alone() {
        let mut names = Interner::new();
        let mut func = crossing(&mut names);
        assert!(!integers(&mut func, &WIN64), "a boundary nobody taught this is not its to move");
        let text = printed(&func, &mut names);
        assert!(text.contains("i40"), "the function is exactly as it was: {text}");
    }

    /// A target with no taught convention, which is wasm, widens the arithmetic inside a function
    /// and leaves a function with a width at a boundary as it was.
    #[test]
    fn a_target_with_no_convention_widens_only_what_stays_inside() {
        let mut names = Interner::new();
        let mut func = crossing(&mut names);
        assert!(!integers_inside(&mut func), "no boundary has been taught");
        let text = printed(&func, &mut names);
        assert!(text.contains("i40"), "the function is exactly as it was: {text}");

        let (mut func, entry) = shell(&mut names);
        let narrow = Type::int(40);
        let mut build = Builder::new(&mut func, entry);
        let value = seed(&mut build, narrow);
        let count = build.iconst(narrow, 8);
        let shifted = build.binary(Opcode::Shl, value, count, Flags::NONE);
        let answer = build.unary(Opcode::Trunc, shifted, Type::int(32));
        build.ret(&[answer]);
        assert!(integers_inside(&mut func), "the shift is inside the function");
        let text = printed(&func, &mut names);
        assert!(!text.contains("i40"), "no forty bit value is left: {text}");
    }

    #[test]
    fn a_width_reaching_an_opcode_this_does_not_understand_is_left_alone() {
        let mut names = Interner::new();
        let (mut func, entry) = shell(&mut names);
        let narrow = Type::int(40);
        let mut build = Builder::new(&mut func, entry);
        let value = build.iconst(narrow, 3);
        // A population count is not on the list, so the function goes to the selector as it is and
        // the selector refuses it by name.
        let counted = build.unary(Opcode::Ctpop, value, narrow);
        let answer = build.unary(Opcode::Trunc, counted, Type::int(32));
        build.ret(&[answer]);

        assert!(!integers(&mut func, &SYSV), "an opcode this has not thought about stops it");
        let text = printed(&func, &mut names);
        assert!(text.contains("i40"), "the function is exactly as it was: {text}");
    }

    #[test]
    fn a_function_with_nothing_at_such_a_width_is_not_touched() {
        let mut names = Interner::new();
        let (mut func, entry) = shell(&mut names);
        let mut build = Builder::new(&mut func, entry);
        let value = build.iconst(Type::int(32), 3);
        build.ret(&[value]);

        assert!(!integers(&mut func, &SYSV), "there is nothing to widen");
    }
}
