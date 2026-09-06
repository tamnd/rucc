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
//! which is the width rounded up to a byte and then to a power of two, and that is the same width
//! the type's own layout already has: a `_BitInt(40)` object is eight bytes, so nothing here
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
//! # What it does not do
//!
//! A function whose signature has one of these widths in it, a call that passes or returns one,
//! and anything else that touches one is left exactly as it was, and the selector then refuses the
//! function by name the way it does today. The reason is the boundary rather than the arithmetic:
//! the psABI says a `_BitInt(40)` argument arrives extended, and which extension it is depends on
//! whether the type was signed, which is a fact the IR deliberately does not carry because the
//! signedness of an integer lives on the operation there and not on the type. That belongs in the
//! ABI lowering, where the C type is still in hand. `tamnd/rucc#425` is the issue for it.

use rucc_base::Idx;
use rucc_ir::{Def, Extra, Flags, Func, Imm, Inst, InstData, IntPred, Opcode, Type, Value};

/// The width a value of this type is kept in, and [`None`] when the machine has one already.
///
/// One bit is a width the rules name, since a comparison produces it and it is what a `bool`
/// lives in, so it is not one of these. Above sixty four bits there is no register to round up
/// into, and a hundred and twenty eight bit integer is refused by name in [`crate::coverage`]
/// rather than being pretended about here.
#[must_use]
fn container(ty: Type) -> Option<u32> {
    if !ty.is_int() || !ty.is_scalar() {
        return None;
    }
    let bits = ty.bits();
    if bits == 1 || bits > 64 {
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
pub fn integers(func: &mut Func) -> bool {
    let narrow: Vec<Option<u32>> = func
        .values()
        .map(|value| container(func[value].ty).map(|_| func[value].ty.bits()))
        .collect();
    if narrow.iter().all(Option::is_none) {
        return false;
    }
    if !every_width_is_one_the_signature_has(func) {
        return false;
    }

    let insts: Vec<Inst> =
        func.blocks().flat_map(|block| func.insts(block).collect::<Vec<_>>()).collect();
    if !insts.iter().all(|&inst| touches_nothing_it_does_not_understand(func, &narrow, inst)) {
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
    for inst in insts {
        rewrite(func, &narrow, inst);
    }
    true
}

/// Whether nothing at one of these widths crosses the function's own boundary.
///
/// A parameter and a return value are the two places a width is agreed with something this
/// compilation is not looking at, so a width the ABI has not been taught is a width this pass
/// leaves for the ABI to be taught about. The entry block's parameters are asked as well as the
/// signature's, because they are the same list said twice and this pass would rather notice the
/// day they stop being.
fn every_width_is_one_the_signature_has(func: &Func) -> bool {
    let signature = func.signature();
    let crossing = signature.params.iter().chain(signature.returns.iter());
    if crossing.map(|param| param.ty).any(|ty| container(ty).is_some()) {
        return false;
    }
    let Some(entry) = func.entry() else { return true };
    func[entry].params.iter().all(|&value| container(func[value].ty).is_none())
}

/// Whether every value at one of these widths that this instruction touches is one it can handle.
fn touches_nothing_it_does_not_understand(func: &Func, narrow: &[Option<u32>], inst: Inst) -> bool {
    let data = &func[inst];
    let touched = results(func, inst).any(|value| at(narrow, value).is_some())
        || func[data.args].iter().any(|&value| at(narrow, value).is_some());
    !touched || understood(data.opcode)
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
#[must_use]
fn low_bits(width: u32) -> i128 {
    (1i128 << width) - 1
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
    use rucc_target::{Arch, Env, Os, TargetInfo, Triple};

    use super::{container, integers};

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
        // Above sixty four there is nothing to round up into, and a vector is not a scalar.
        assert_eq!(container(Type::int(65)), None);
        assert_eq!(container(Type::int(128)), None);
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

        assert!(integers(&mut func), "there is a width to widen");
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

        assert!(integers(&mut func), "there is a width to widen");
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

            assert!(integers(&mut func), "there is a width to widen");
            let text = printed(&func, &mut names);
            assert!(!text.contains("i33"), "no thirty three bit value is left: {text}");
            assert_eq!(text.matches(" = and ").count(), 1, "{pred:?} masks once: {text}");
            assert_eq!(text.matches(" = shl ").count(), shifts, "{pred:?} shifts up: {text}");
        }
    }

    #[test]
    fn a_width_that_crosses_the_boundary_is_left_for_the_abi() {
        let mut names = Interner::new();
        let narrow = Type::int(40);
        let mut func = Func::new(
            names.intern("f"),
            Signature::new().with_params(&[narrow]).with_returns(&[narrow]),
        );
        let entry = func.create_block();
        let x = func.append_param(entry, narrow);
        let mut build = Builder::new(&mut func, entry);
        let one = build.iconst(narrow, 1);
        let sum = build.binary(Opcode::Add, x, one, Flags::NONE);
        build.ret(&[sum]);

        assert!(!integers(&mut func), "a parameter at that width is not this pass's to move");
        let text = printed(&func, &mut names);
        assert!(text.contains("i40"), "the function is exactly as it was: {text}");
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

        assert!(!integers(&mut func), "an opcode this has not thought about stops it");
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

        assert!(!integers(&mut func), "there is nothing to widen");
    }
}
