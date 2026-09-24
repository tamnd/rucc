//! The AAPCS64 walk, which is the SysV one counting down rather than up.
//!
//! Design: `spec/12-abi-and-runtime.md`, and the procedure call standard's appendix on `va_arg`,
//! which is where the layout and every step below come from.
//!
//! The callee spills the argument registers the same way a SysV one does, eight general purpose
//! registers and eight vector ones, and the list says how far into each the walk has got. What is
//! different is how it says it. There are two pointers to the top of the two halves of the save
//! area and two offsets that start negative and count up to zero:
//!
//! ```text
//! offset  0  __stack    the next argument that came in the caller's memory
//! offset  8  __gr_top   the end of the general purpose half of the save area
//! offset 16  __vr_top   the end of the vector half
//! offset 24  __gr_offs  minus the bytes of the general purpose half not walked yet
//! offset 28  __vr_offs  minus the bytes of the vector half not walked yet
//! ```
//!
//! So an argument is in the save area when its offset is still negative after the walk steps past
//! it, and the address it is at is the top plus the offset before the step. An offset that is zero
//! or more to start with means that half has run out, and the argument is in the caller's memory,
//! as is one the step takes past zero. The step is stored either way, which is what the standard
//! does and what keeps every argument after one that did not fit in memory as well.
//!
//! A general purpose slot is eight bytes and a vector one is sixteen, and neither changes with what
//! is in it: a `float` is the low four bytes of its slot and a `long double` is all sixteen.
//!
//! # Objects
//!
//! A structure of sixteen bytes or less that is not made of floats arrived in one or two general
//! purpose registers, and since those are next to each other in the save area the object is too, so
//! its address is an address in the area. One aligned to sixteen starts on an even register, which
//! is the offset rounded up to sixteen before the step. An `__int128` is read as one of these.
//!
//! A structure made of up to four floats of one kind arrived one member to a vector register, and
//! the members are sixteen bytes apart in the area, so it is copied out into a buffer the way a
//! SysV object split between two files is, and the answer is the buffer.
//!
//! Anything larger travelled as the address of a copy the caller made, which is one general purpose
//! slot, and the answer is what is in it.
//!
//! In the caller's memory, every argument takes a whole number of eight byte words and one aligned
//! to sixteen starts on a boundary of sixteen.

use rucc_ir::{
    Block, Builder, Extra, Flags, Float, Func, Inst, IntPred, MemInfo, Opcode, Type, Value,
};
use rucc_target::Slot;

use super::{added, buffer, info, is_float, offset, width};

/// Where the pointer to the next argument in the caller's memory is.
pub const STACK: i64 = 0;
/// Where the pointer to the end of the general purpose half of the save area is.
pub const GR_TOP: i64 = 8;
/// Where the pointer to the end of the vector half is.
pub const VR_TOP: i64 = 16;
/// Where the offset into the general purpose half is.
pub const GR_OFFS: i64 = 24;
/// Where the offset into the vector half is.
pub const VR_OFFS: i64 = 28;
/// How many bytes the list is, which is what a `va_copy` of one moves.
pub const SIZE: u64 = 32;

/// How far apart two general purpose slots are.
const WORD: u32 = 8;
/// How far apart two vector slots are.
const VECTOR: u32 = 16;
/// The largest object that travels as itself rather than as the address of a copy.
const IN_REGISTERS: u64 = 16;

/// Where one argument is, as far as the walk is concerned.
#[derive(Clone, Copy)]
struct Wants {
    /// Whether it is in the vector half.
    float: bool,
    /// How many slots of that half it takes.
    slots: u32,
    /// Whether it starts on an even general purpose register.
    even: bool,
    /// How many bytes it takes in the caller's memory, before rounding up to a word.
    size: u64,
    /// Whether it starts on a boundary of sixteen in the caller's memory.
    wide: bool,
}

/// One `va_arg` of a scalar, as the walk and then the load the program wrote.
pub(super) fn next(func: &mut Func, inst: Inst) {
    let Some(result) = func[inst].first_result else { return };
    let Some(&list) = func[func[inst].args].first() else { return };
    let ty = func[result].ty;
    let Some(block) = func.block_of(inst) else { return };
    if !ty.is_scalar() || !(ty.is_int() || ty.is_float() || ty.is_ptr()) || ty.bits() > 128 {
        return;
    }
    // An integer of two words is read as an object by the front end, so this one is not a thing.
    if ty.is_int() && ty.bits() > 64 {
        return;
    }
    let bytes = u64::from(ty.bits().div_ceil(8)).max(1);
    let float = ty.is_float();
    let wants = Wants { float, slots: 1, even: false, size: bytes, wide: bytes > 8 };

    let rest = cut(func, block, inst);
    let (join, address) = walk(func, block, inst, list, wants, |_, found| found);

    let span = func.span(inst);
    let mut build = Builder::new(func, join).at(span);
    let from = build.unary(Opcode::IntToPtr, address, Type::PTR);
    let align = u32::try_from(bytes.next_power_of_two()).unwrap_or(1);
    let mem = func.add_mem(info(bytes, align));
    let args = func.push_values(&[from]);
    let data = &mut func[inst];
    data.opcode = Opcode::Load;
    data.args = args;
    data.extra = Extra::Mem(mem);
    data.flags = data.flags.intersection(Flags::legal_on(Opcode::Load));
    func.append_inst(join, inst);
    for at in rest {
        func.append_inst(join, at);
    }
}

/// One `va_arg` of an aggregate, as the address it can be read from.
pub(super) fn object(func: &mut Func, inst: Inst) {
    let Extra::VaObject(at) = func[inst].extra else { return };
    let object = func[at];
    let MemInfo { size, align, .. } = func[object.mem];
    let slots: Vec<Slot> = func[object.slots].to_vec();
    let Some(&list) = func[func[inst].args].first() else { return };
    let Some(block) = func.block_of(inst) else { return };
    if func[inst].first_result.is_none() {
        return;
    }
    let floats = slots.iter().filter(|&&slot| is_float(slot)).count();
    let by_reference = slots.is_empty() && size > IN_REGISTERS;
    let wants = if by_reference {
        Wants { float: false, slots: 1, even: false, size: u64::from(WORD), wide: false }
    } else if floats == 0 {
        let slots = u32::try_from(size.div_ceil(u64::from(WORD))).unwrap_or(0);
        if slots == 0 || size > IN_REGISTERS {
            return;
        }
        Wants { float: false, slots, even: align >= 16, size, wide: align >= 16 }
    } else if floats == slots.len() && floats <= 4 {
        let wide = slots.iter().any(|&slot| width(slot) > u64::from(WORD));
        let slots = u32::try_from(floats).unwrap_or(0);
        Wants { float: true, slots, even: false, size, wide }
    } else {
        return;
    };
    // A buffer for the members of a structure of floats, made before anything else because an
    // alloca of a fixed size belongs in the entry block and the walk is built where the instruction
    // is. As big as the members reach, which is the object.
    let room = if wants.float && wants.slots > 1 {
        let Some(room) = buffer(func, inst, size, align.max(WORD)) else { return };
        Some(room)
    } else {
        None
    };

    let rest = cut(func, block, inst);
    let copy = |build: &mut Builder<'_>, found: Value| match room {
        Some(room) => copied(build, found, &slots, room, align.max(WORD)),
        None => found,
    };
    let (join, mut address) = walk(func, block, inst, list, wants, copy);
    if by_reference {
        let span = func.span(inst);
        let mut build = Builder::new(func, join).at(span);
        let slot = build.unary(Opcode::IntToPtr, address, Type::PTR);
        let held = build.load(Type::PTR, slot, info(8, 8), Flags::default());
        address = build.unary(Opcode::PtrToInt, held, Type::int(64));
    }

    let args = func.push_values(&[address]);
    let data = &mut func[inst];
    data.opcode = Opcode::IntToPtr;
    data.args = args;
    data.extra = Extra::None;
    data.flags = data.flags.intersection(Flags::legal_on(Opcode::IntToPtr));
    func.append_inst(join, inst);
    for at in rest {
        func.append_inst(join, at);
    }
}

/// Takes the instruction and everything below it out of its block, and gives back what was below
/// it, because a builder appends to a block and this one has to end at the first branch.
fn cut(func: &mut Func, block: Block, inst: Inst) -> Vec<Inst> {
    let rest: Vec<Inst> = func.insts(block).skip_while(|&at| at != inst).skip(1).collect();
    func.remove_inst(inst);
    for &at in &rest {
        func.remove_inst(at);
    }
    rest
}

/// The two questions and the two places, ending in a block that has the address of the argument as
/// its parameter, an integer.
///
/// `found` is handed the address in the save area on the way out of the register path and gives
/// back the address the path answers with, which is the same address for everything but a
/// structure of floats, whose members are copied out of the area into a buffer first.
fn walk(
    func: &mut Func,
    block: Block,
    inst: Inst,
    list: Value,
    wants: Wants,
    found: impl FnOnce(&mut Builder<'_>, Value) -> Value,
) -> (Block, Value) {
    let span = func.span(inst);
    let wide = Type::int(64);
    let word = Type::int(32);
    let (field, top, stride) =
        if wants.float { (VR_OFFS, VR_TOP, VECTOR) } else { (GR_OFFS, GR_TOP, WORD) };
    let fits = func.create_block();
    let saved = func.create_block();
    let stack = func.create_block();
    let join = func.create_block();
    let address = func.append_param(join, wide);

    // Whether the half had run out before this argument.
    let mut build = Builder::new(func, block).at(span);
    let counter = offset(&mut build, list, field);
    let mut walked = build.load(word, counter, info(4, 4), Flags::default());
    let zero = build.iconst(word, 0);
    let out = build.icmp(IntPred::Sge, walked, zero);
    build.br_if(out, stack, &[], fits, &[]);

    // Whether this argument takes it past the end. The step is stored whichever way it goes.
    let mut build = Builder::new(func, fits).at(span);
    if wants.even {
        let bump = build.iconst(word, 15);
        walked = build.binary(Opcode::Add, walked, bump, Flags::default());
        let mask = build.iconst(word, -16);
        walked = build.binary(Opcode::And, walked, mask, Flags::default());
    }
    let by = build.iconst(word, i128::from(stride * wants.slots));
    let stepped = build.binary(Opcode::Add, walked, by, Flags::default());
    let counter = offset(&mut build, list, field);
    build.store(stepped, counter, info(4, 4), Flags::default());
    let zero = build.iconst(word, 0);
    let past = build.icmp(IntPred::Sgt, stepped, zero);
    build.br_if(past, stack, &[], saved, &[]);

    // In the save area, at the top of the half less what the offset said was left.
    let mut build = Builder::new(func, saved).at(span);
    let at = offset(&mut build, list, top);
    let top = build.load(Type::PTR, at, info(8, 8), Flags::default());
    let back = build.unary(Opcode::SExt, walked, wide);
    let here = added(&mut build, top, back);
    let here = build.unary(Opcode::PtrToInt, here, wide);
    let here = found(&mut build, here);
    build.jump(join, &[here]);

    // In the caller's memory, where the pointer is rounded up for an argument that wants sixteen
    // and then stepped on past it by whole words.
    let mut build = Builder::new(func, stack).at(span);
    let pointer = offset(&mut build, list, STACK);
    let there = build.load(Type::PTR, pointer, info(8, 8), Flags::default());
    let mut there = build.unary(Opcode::PtrToInt, there, wide);
    if wants.wide {
        let bump = build.iconst(wide, 15);
        there = build.binary(Opcode::Add, there, bump, Flags::default());
        let mask = build.iconst(wide, -16);
        there = build.binary(Opcode::And, there, mask, Flags::default());
    }
    let by = build.iconst(wide, i128::from(wants.size.next_multiple_of(u64::from(WORD))));
    let onward = build.binary(Opcode::Add, there, by, Flags::default());
    let onward = build.unary(Opcode::IntToPtr, onward, Type::PTR);
    build.store(onward, pointer, info(8, 8), Flags::default());
    build.jump(join, &[there]);

    (join, address)
}

/// The members of a structure of floats copied out of the vector half into a buffer, as the
/// address of the buffer.
fn copied(build: &mut Builder<'_>, from: Value, slots: &[Slot], room: Value, align: u32) -> Value {
    let from = build.unary(Opcode::IntToPtr, from, Type::PTR);
    for (index, &slot) in slots.iter().enumerate() {
        let bytes = width(slot);
        let ty = if bytes > u64::from(WORD) {
            Type::float(Float::F128)
        } else {
            Type::int(u32::try_from(bytes).unwrap_or(1) * 8)
        };
        let step = i64::from(VECTOR) * i64::try_from(index).unwrap_or(0);
        let at = offset(build, from, step);
        let aligned = u32::try_from(bytes).unwrap_or(1);
        let value = build.load(ty, at, info(bytes, aligned), Flags::default());
        let into = offset(build, room, i64::try_from(slot.offset()).unwrap_or(0));
        let holds = info(bytes, super::part(align, slot.offset()));
        build.store(value, into, holds, Flags::default());
    }
    build.unary(Opcode::PtrToInt, room, Type::int(64))
}
