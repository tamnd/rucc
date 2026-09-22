//! The float that is narrower than any instruction, as the work at a wider one.
//!
//! `_Float16` is the other arithmetic type a C program writes that this machine does not compute
//! with. It is the mirror of [`crate::quad`]: sixteen bits fit in a vector register, so moving one,
//! passing one and returning one are things this back end has, and everything else has no
//! instruction behind it, because half precision arithmetic on x86-64 arrived with AVX512FP16 and
//! that is far above this target's baseline. gcc 16 is in exactly the same position at the same
//! baseline and does exactly this, so what this pass writes is what gcc writes.
//!
//! The difference from the quad is that the half has somewhere to go. Every value of this format
//! is a value of `float` exactly, since eleven bits of significand and five of exponent fit inside
//! twenty four and eight with room to spare, so the operation the program wrote is the `float`
//! operation with a widening in front of it and a narrowing behind it. Only the widening and the
//! narrowing are calls.
//!
//! # Why `float` in the middle is the same answer and not merely a close one
//!
//! Because twenty four is at least two times eleven plus two. That is the double rounding bound:
//! an addition, a subtraction, a multiplication or a division of two values of a format with `p`
//! significant bits, computed in a format with at least `2p + 2` bits and then rounded to the first
//! one, is the same value as the operation rounded once. A half has eleven bits and a `float` has
//! twenty four, which clears the bound by one, so `a + b` at this format really is
//! `__truncsfhf2(extend(a) + extend(b))` and not an approximation of it. The exponent range is not
//! in the way either: `float` holds every product and every quotient of two halves, subnormal ones
//! included, without overflowing or losing a bit to its own subnormal range.
//!
//! This is also why the comparisons are not calls the way the quad's are. Widening is exact and
//! exactness is all a comparison needs, so two halves compare as the two `float`s they widen to,
//! and every predicate including the unordered ones comes out the same. libgcc does define
//! `__eqhf2` and `__nehf2`, and gcc calls neither of them on this target for the same reason.
//!
//! # The narrowing is where the care goes
//!
//! Rounding twice is not rounding once. A `double` that sits a hair above the midpoint between two
//! halves rounds down to exactly that midpoint in a `float`, and then to even from the midpoint,
//! which is the other neighbour from the one a single rounding gives. So there is a routine per
//! source width, `__truncsfhf2`, `__truncdfhf2` and `__trunctfhf2`, and this pass picks the one
//! that matches what the program actually had rather than going through `float` every time.
//!
//! An integer becoming a half goes through `double` for the same reason and is exact all the same.
//! Every integer a half can represent is below 65536 and is therefore exact in a `double`, and
//! every integer at or above 65520 is an infinity at this format whatever happened on the way, so
//! there is no value of any integer width where the intermediate rounding to `double` can change
//! the answer. That is why this needs no `__floatsihf` family and libgcc has none.
//!
//! # What is left alone
//!
//! A conversion against the eighty bit format, which is the one thing here with a libgcc routine
//! that this pass does not call. `__truncxfhf2` exists and would be the right answer, and what
//! stands in the way is that an eighty bit value travels in the argument area as bytes rather than
//! in a register, so a call taking one is a different shape from every other call written here. It
//! is refused by name instead, which is the position [`crate::quad`] takes on the same pair, and it
//! is `tamnd/rucc#1064`'s row rather than this pass's work today.
//!
//! A constant of the format and a negation of one are left alone as well, and those two are not
//! refusals. [`crate::expand::floats`] already writes a float constant as the integer spelling its
//! bits and a reinterpretation, and a negation as an exclusive or with the sign bit in a general
//! purpose register, and both of those are written for every float of sixty four bits or narrower
//! rather than for the two the machine computes in. So a half reaches them and comes out as the
//! bits and a `bitcast`, which is what the rule set now has an instruction for.
//!
//! A load is left alone too, because the machine has one. `pinsrw` reads sixteen bits straight into
//! the low lane of a vector register and a rule writes it. A store is not the mirror of that: the
//! form of `pextrw` that writes memory is SSE4.1, so a store is the bits out through a general
//! purpose register and an ordinary sixteen bit store after them, which is two instructions and so
//! this pass's work rather than a rule's.

use rucc_base::Interner;
use rucc_ir::{
    CallInfo, Extra, Float, Func, Inst, InstData, MemInfo, MemOrder, Opcode, Param, Restrict,
    Signature, Type, Value,
};
use rucc_target::AbiDescription;

use crate::capability;

/// The format this pass is about.
const HALF: Float = Float::F16;

/// The format every operation at it is performed at, which is the narrowest one that holds the
/// answer exactly. See the module's second section for why exactly.
const WIDE: Float = Float::F32;

/// The format an integer conversion goes through, which is wider than [`WIDE`] because an integer
/// is not a half and needs the room. See the module's fourth section.
const THROUGH: Float = Float::F64;

/// The routine the capability table names for this operation at this mode.
///
/// Every mode this pass asks about is one no instruction on this machine covers, which is the whole
/// reason the pass exists, so the table always has an answer. A missing one is the table and this
/// pass having gone out of step rather than anything a program can reach.
fn routine(opcode: Opcode, mode: &str) -> &'static str {
    capability::libcall(opcode, mode)
        .unwrap_or_else(|| panic!("no routine for `{}` at `{mode}`", opcode.name()))
}

/// Rewrites every operation at this format into the work at a wider one.
///
/// The instructions are collected before any of them is touched, because a rewrite puts
/// instructions in front of the one it replaces and the walk would otherwise see its own work.
pub fn calls(func: &mut Func, names: &mut Interner, abi: &'static AbiDescription) {
    let found: Vec<Inst> =
        func.blocks().flat_map(|block| func.insts(block).collect::<Vec<_>>()).collect();
    for inst in found {
        match func[inst].opcode {
            Opcode::FAdd | Opcode::FSub | Opcode::FMul | Opcode::FDiv => {
                arithmetic(func, names, abi, inst);
            }
            Opcode::FCmp => compare(func, names, abi, inst),
            Opcode::FPExt => widen(func, names, abi, inst),
            Opcode::FPTrunc => narrow(func, names, abi, inst),
            Opcode::SIToFP | Opcode::UIToFP => from_integer(func, names, abi, inst),
            Opcode::FPToSI | Opcode::FPToUI => to_integer(func, names, abi, inst),
            Opcode::Store => stored(func, inst),
            _ => {}
        }
    }
}

/// Whether this type is the format.
fn half(ty: Type) -> bool {
    ty.is_scalar() && ty.format() == Some(HALF)
}

/// The type of an instruction's first result, or nothing where it has none.
fn produced(func: &Func, inst: Inst) -> Option<Type> {
    func[inst].first_result.map(|value| func[value].ty)
}

/// The four operations, each of them the `float` one between a widening and a narrowing.
///
/// The flags the program wrote are carried onto the operation in the middle, because that is the
/// operation it asked for: a contraction the program allowed is still allowed of the addition, and
/// a not a number it promised there would not be is still promised of the same operands. The two
/// calls carry none, which is the same thing [`crate::quad`] does and for the same reason, since a
/// call is a call whatever the program said about rounding.
fn arithmetic(func: &mut Func, names: &mut Interner, abi: &'static AbiDescription, inst: Inst) {
    let Some(ty) = produced(func, inst) else { return };
    if !half(ty) {
        return;
    }
    let args = func[func[inst].args].to_vec();
    let [a, b] = args[..] else { return };
    let opcode = func[inst].opcode;
    let flags = func[inst].flags;
    let a = extended(func, names, abi, inst, a);
    let b = extended(func, names, abi, inst, b);
    let args = func.push_values(&[a, b]);
    let data = InstData { args, flags, ..InstData::new(opcode) };
    let answer = written(func, inst, data, Type::float(WIDE));
    into_call(func, names, abi, inst, routine(Opcode::FPTrunc, "f32.f16"), &[answer]);
}

/// A comparison, as the same comparison of the two `float`s the operands widen to.
///
/// The predicate is untouched and the instruction stays a comparison, which is the whole of what
/// makes this different from the quad's: there the answer comes back from a routine as an integer
/// and has to be tested against zero, and here the machine has the comparison already and only the
/// operands had to get to a format it has one at.
fn compare(func: &mut Func, names: &mut Interner, abi: &'static AbiDescription, inst: Inst) {
    let args = func[func[inst].args].to_vec();
    let [a, b] = args[..] else { return };
    if !half(func[a].ty) || !half(func[b].ty) {
        return;
    }
    let a = extended(func, names, abi, inst, a);
    let b = extended(func, names, abi, inst, b);
    func[inst].args = func.push_values(&[a, b]);
}

/// A half becoming a wider float, which is the one routine and never rounds.
///
/// One routine for all three destinations, because the only widening libgcc has from this format is
/// the one to `float` and the rest of the way is a widening the machine does itself. Nothing is
/// lost by going in two steps here, unlike in the narrowing direction: both halves of the journey
/// are exact, so there is no second rounding to get wrong.
fn widen(func: &mut Func, names: &mut Interner, abi: &'static AbiDescription, inst: Inst) {
    let Some(ty) = produced(func, inst) else { return };
    let Some(&arg) = func[func[inst].args].first() else { return };
    if !half(func[arg].ty) {
        return;
    }
    let routine = routine(Opcode::FPExt, "f16.f32");
    if ty.format() == Some(WIDE) {
        into_call(func, names, abi, inst, routine, &[arg]);
        return;
    }
    let wide = call(func, names, abi, inst, routine, &[arg], Type::float(WIDE));
    becomes(func, inst, Opcode::FPExt, Extra::None, &[wide]);
}

/// A wider float becoming a half, which is a routine per source width because each of them rounds.
///
/// The eighty bit format is not one of them, for the reason the module's last section gives.
fn narrow(func: &mut Func, names: &mut Interner, abi: &'static AbiDescription, inst: Inst) {
    let Some(ty) = produced(func, inst) else { return };
    let Some(&arg) = func[func[inst].args].first() else { return };
    if !half(ty) {
        return;
    }
    let mode = match func[arg].ty.format() {
        Some(Float::F32) => "f32.f16",
        Some(Float::F64) => "f64.f16",
        Some(Float::F128) => "f128.f16",
        _ => return,
    };
    into_call(func, names, abi, inst, routine(Opcode::FPTrunc, mode), &[arg]);
}

/// An integer becoming a half, which is the conversion to a `double` and the narrowing of that.
///
/// The conversion in the middle is left as the opcode the program wrote, signed or unsigned, so
/// [`crate::expand::floats`] still gets to widen a narrow integer and to take the unsigned word
/// apart the way it does for every other conversion. This pass only says which format it lands in
/// on the way.
fn from_integer(func: &mut Func, names: &mut Interner, abi: &'static AbiDescription, inst: Inst) {
    let Some(ty) = produced(func, inst) else { return };
    let Some(&arg) = func[func[inst].args].first() else { return };
    let from = func[arg].ty;
    if !half(ty) || !from.is_int() || !from.is_scalar() {
        return;
    }
    let opcode = func[inst].opcode;
    let args = func.push_values(&[arg]);
    let data = InstData { args, ..InstData::new(opcode) };
    let wide = written(func, inst, data, Type::float(THROUGH));
    into_call(func, names, abi, inst, routine(Opcode::FPTrunc, "f64.f16"), &[wide]);
}

/// A half becoming an integer, which is the widening and the conversion the machine has.
///
/// `float` rather than `double` on this side, because the widening is exact and the conversion from
/// there is the same rounding toward zero at either width. The opcode is left alone for the reason
/// [`from_integer`] leaves it alone.
fn to_integer(func: &mut Func, names: &mut Interner, abi: &'static AbiDescription, inst: Inst) {
    let Some(ty) = produced(func, inst) else { return };
    let Some(&arg) = func[func[inst].args].first() else { return };
    if !half(func[arg].ty) || !ty.is_int() || !ty.is_scalar() {
        return;
    }
    let opcode = func[inst].opcode;
    let wide = extended(func, names, abi, inst, arg);
    becomes(func, inst, opcode, Extra::None, &[wide]);
}

/// A store of a half, as a store of the sixteen bits spelling it.
///
/// The address, the flags and everything the access says about itself stay exactly as they were:
/// this is the same store of the same two bytes to the same place, said about an integer so that
/// the rule set has an instruction for it. What it costs is the `pextrw` in front, which is the one
/// instruction gcc's output has here that the load does not need.
fn stored(func: &mut Func, inst: Inst) {
    let args = func[func[inst].args].to_vec();
    let [value, address] = args[..] else { return };
    if !half(func[value].ty) {
        return;
    }
    let bits = ahead(func, inst, Opcode::Bitcast, &[value], Type::int(16));
    func[inst].args = func.push_values(&[bits, address]);
}

/// One half widened to a `float` in front of an instruction, as the call that does it.
fn extended(
    func: &mut Func,
    names: &mut Interner,
    abi: &'static AbiDescription,
    inst: Inst,
    value: Value,
) -> Value {
    let routine = routine(Opcode::FPExt, "f16.f32");
    call(func, names, abi, inst, routine, &[value], Type::float(WIDE))
}

/// Turns an instruction into the call that performs it, in place.
///
/// In place rather than in front of, because the call produces one value of the type the
/// instruction already produced, so every reader of it goes on reading the same value.
///
/// There is no answer coming back through an address here, which is the one shape [`crate::quad`]
/// has and this does not. Everything a routine of this family gives back is a half or a `float`,
/// and no convention passes two or four bytes of anything by reference.
fn into_call(
    func: &mut Func,
    names: &mut Interner,
    abi: &'static AbiDescription,
    inst: Inst,
    routine: &str,
    args: &[Value],
) {
    let Some(ty) = produced(func, inst) else { return };
    let shape = shaped(func, abi, inst, args);
    let extra = signature(func, names, routine, &shape, ty);
    becomes(func, inst, Opcode::Call, extra, &shape.values);
}

/// A call to a runtime routine written in front of an instruction, and the value it answers.
fn call(
    func: &mut Func,
    names: &mut Interner,
    abi: &'static AbiDescription,
    inst: Inst,
    routine: &str,
    args: &[Value],
    ty: Type,
) -> Value {
    let shape = shaped(func, abi, inst, args);
    let extra = signature(func, names, routine, &shape, ty);
    let args = func.push_values(&shape.values);
    written(func, inst, InstData { args, extra, ..InstData::new(Opcode::Call) }, ty)
}

/// A call's operands once the convention has been asked about each of them.
struct Shape {
    /// What each operand is in the signature, which is `ptr` for one that became an address.
    params: Vec<Param>,
    /// The values the call instruction actually reads, in the same order.
    values: Vec<Value>,
}

/// The operands of one call, with the one that a convention may pass by address spilled to the
/// frame.
///
/// Only a `_Float128` can be that one, and only on a convention that passes sixteen bytes of
/// anything as the address of a copy, which is Windows x64. That is the rule `tamnd/rucc#1331` put
/// in the ABI description, it is a rule about a call, and a call this pass writes is a call. Every
/// other operand here is two, four or eight bytes and travels in a register on every convention.
fn shaped(func: &mut Func, abi: &'static AbiDescription, inst: Inst, args: &[Value]) -> Shape {
    let mut shape = Shape { params: Vec::new(), values: Vec::new() };
    for &value in args {
        let ty = func[value].ty;
        let size = u64::from(ty.bits().div_ceil(8));
        if quad(ty) && abi.scalar_is_by_reference(size) {
            let copy = slot(func, inst);
            write(func, inst, value, copy);
            shape.params.push(Param::new(Type::PTR));
            shape.values.push(copy);
        } else {
            shape.params.push(Param::new(ty));
            shape.values.push(value);
        }
    }
    shape
}

/// Whether this type is the format that fills a whole vector register, which is the one operand
/// here a convention may want by address.
fn quad(ty: Type) -> bool {
    ty.is_scalar() && ty.format() == Some(Float::F128)
}

/// The call this shape is, as the `Extra` an instruction carries it in.
fn signature(
    func: &mut Func,
    names: &mut Interner,
    routine: &str,
    shape: &Shape,
    ty: Type,
) -> Extra {
    let mut built = Signature::new();
    built.params = shape.params.clone();
    built.returns = vec![Param::new(ty)];
    let signature = func.add_signature(built);
    let callee = Some(names.intern(routine));
    let varargs = func.push_abis(&[]);
    Extra::Call(func.add_call(CallInfo { callee, signature, varargs }))
}

/// A frame slot the size of a quad, put in front of an instruction.
fn slot(func: &mut Func, inst: Inst) -> Value {
    let extra = Extra::Mem(func.add_mem(whole()));
    written(func, inst, InstData { extra, ..InstData::new(Opcode::Alloca) }, Type::PTR)
}

/// An access to the whole of one quad, which is the only thing this pass ever puts in a slot.
fn whole() -> MemInfo {
    MemInfo {
        size: 16,
        align: 16,
        order: MemOrder::NotAtomic,
        tbaa: None,
        owns: 0,
        restrict: Restrict::NONE,
    }
}

/// A store of a quad into a slot, put in front of an instruction.
fn write(func: &mut Func, inst: Inst, value: Value, into: Value) {
    let span = func.span(inst);
    let extra = Extra::Mem(func.add_mem(whole()));
    let args = func.push_values(&[value, into]);
    let data = InstData { args, extra, ..InstData::new(Opcode::Store) };
    let made = func.create_inst(data, &[], span);
    func.insert_before(made, inst);
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
fn becomes(func: &mut Func, inst: Inst, opcode: Opcode, extra: Extra, args: &[Value]) {
    let args = func.push_values(args);
    let data = &mut func[inst];
    data.opcode = opcode;
    data.args = args;
    data.extra = extra;
    data.flags = data.flags.intersection(rucc_ir::Flags::legal_on(opcode));
}
