//! The IR rewrites the machine needs before a rule can be asked anything.
//!
//! Design: `spec/10-backend.md` section 10.2, which is where the ordering comes from.
//!
//! Everything else in this crate turns an instruction into instructions. There are two things a
//! rule cannot do, and what is done about each of them instead is here.
//!
//! The first is a new shape of control flow. A rule replaces a term with a term and the
//! replacement has nowhere to put a block, so a construct that becomes blocks has to be rewritten
//! before selection rather than during it. There is one such construct today and it is `switch`.
//! Every other terminator leaves a block with one successor or two, which is what the block layout
//! writes jumps for, and a `switch` leaves it with as many as the program had cases.
//!
//! The second is arithmetic on what a rule matched. A rule may name a constant and pass it along,
//! and it may not add to one or read it as something else, because the pattern language is a
//! pattern language and giving it a way to compute would make a rule set a program the solver has
//! to reason about rather than a table it can check a line of at a time. So an instruction whose
//! lowering needs a value worked out from another one is rewritten here into instructions whose
//! lowerings do not. Four of them are floats: a float constant, a negation, and the two conversions
//! between a float and an unsigned integer. The other two move a block of memory, where the
//! arithmetic is the offset of each word from the front of it.
//!
//! # Why a copy is a run of moves and not a call
//!
//! A `memcpy` in the IR is not a call to `memcpy`. It is what the front end writes for a structure
//! assigned, passed or returned by value, and a `memset` is what it writes for the part of an
//! object an initialiser left unnamed, so a program with a `struct` in it reaches one almost at
//! once and the size is a constant every time.
//!
//! A constant size is what makes the moves the right answer. A four byte copy written as a call
//! costs the call and the two arguments and gives back four bytes moved, which is more instructions
//! than the move it replaced and slower than all of them. Every real compiler writes the moves
//! under some threshold for that reason, and above the threshold writes the call, which is where
//! this stops: the call needs a `memcpy` to exist, and a statically linked program has nowhere to
//! get one from until the compiler runtime in tamnd/rucc#277 exists. So a copy larger than the
//! threshold is refused by name rather than written wrong.
//!
//! # Why the chain a `switch` becomes is the backend's and not the front end's
//!
//! What a `switch` should become is a target decision and not a language one. A chain of compares
//! is right for three cases and wrong for two hundred, where the answer is a jump table, and wrong
//! again for twenty spread over a million, where it is a binary search on the value. A front end
//! that picked one would be picking for every target at once, and the IR would no longer hold what
//! the program said. So the `switch` survives as far as here, and here is where it is given up.
//!
//! What is written today is the chain, which `spec/10-backend.md` calls the version every compiler
//! starts with. It is correct for any number of cases and it is slow for a large one. A jump table
//! wants a read only section to put the table in and a relocation to reach it, and neither exists
//! yet, so the chain is also the only one that could be written today.

use rucc_base::Interner;
use rucc_ir::{
    BlockCall, Builder, CallInfo, Def, Extra, Flags, Func, Imm, Inst, InstData, IntPred, MemInfo,
    Opcode, Signature, Type, Value,
};

/// Rewrites every `switch` in the function into branches, and leaves everything else alone.
///
/// The function is changed in place, which is what makes this the last thing that reads the IR as
/// the front end built it. `--emit=ir` prints before this runs, and nothing after this asks what
/// the program said, only what the machine has to do.
pub fn switches(func: &mut Func) {
    let found: Vec<Inst> = func
        .blocks()
        .filter_map(|block| func.terminator(block))
        .filter(|&inst| func[inst].opcode == Opcode::Switch)
        .collect();
    for inst in found {
        chain(func, inst);
    }
}

/// One `switch`, as a compare and a branch for each case in the order they were written.
///
/// The block the `switch` was in gets the first compare, and each case after the first gets a
/// block of its own that the one before it falls to when its compare failed. The last of them
/// falls to the default, so the default is not a block anything is created for and the chain costs
/// one block per case less one.
///
/// The order is the order the cases are in, which is the order the program wrote them and not a
/// sorted one. Sorting would be the first half of a binary search and the second half is not here,
/// so it would cost a reader the ability to look at the assembly and see their own `switch`, and
/// buy nothing.
fn chain(func: &mut Func, inst: Inst) {
    let block = func.block_of(inst).expect("a terminator is in a block");
    let span = func.span(inst);
    let Extra::Switch(info) = func[inst].extra else { return };
    let info = func[info];
    let value = func[func[inst].args][0];
    // The lane, because a `switch` on a vector is not a thing C can write and the immediates are
    // an integer's either way.
    let ty = func[value].ty.lane();
    let calls: Vec<BlockCall> = func[info.targets].to_vec();
    let cases: Vec<Imm> = func[info.cases].to_vec();
    let Some((default, arms)) = calls.split_first() else { return };

    // Before anything is written, because the builder appends and the `switch` is where the
    // appending has to happen.
    func.remove_inst(inst);

    // A `switch` with nothing but a default is a jump, which is worth writing down rather than
    // refusing: it is what a `switch` whose only label is `default` is, and it is also what one
    // whose cases were all folded away by a later pass would be.
    let Some((first, rest)) = arms.split_first() else {
        let args: Vec<Value> = func[default.args].to_vec();
        Builder::new(func, block).at(span).jump(default.block, &args);
        return;
    };

    let mut at = block;
    for (index, arm) in std::iter::once(first).chain(rest).enumerate() {
        let last = index + 1 == arms.len();
        let next = if last { default.block } else { func.create_block() };
        let onward: Vec<Value> = if last { func[default.args].to_vec() } else { Vec::new() };
        let taken: Vec<Value> = func[arm.args].to_vec();
        let case = cases[index].signed(ty);

        let mut build = Builder::new(func, at).at(span);
        let want = build.iconst(ty, case);
        let same = build.icmp(IntPred::Eq, value, want);
        build.br_if(same, arm.block, &taken, next, &onward);
        at = next;
    }
}

/// Rewrites the float instructions no rule can be written for, and leaves the rest alone.
///
/// Each of them needs a value worked out from one the pattern matched, which is the one thing the
/// rule language deliberately cannot do. A float constant is an integer constant read as a float,
/// and reading it is arithmetic on the immediate. A negation is an exclusive or with a mask that
/// depends on the format. A conversion between a float and an integer is that conversion at a
/// width the machine has, which is a width neither the pattern nor the replacement can work out.
///
/// What is left after this is a function whose float instructions are each one machine
/// instruction, so what a rule is asked stays a table. The one thing that is not rewritten is a
/// conversion between a float and an unsigned sixty four bit integer, which is refused by name:
/// there is no signed width that holds those values, so it is not the signed conversion anywhere,
/// and what it is instead is a compare and a branch that this would have to write blocks for. No
/// program in the corpus has asked for one yet.
pub fn floats(func: &mut Func) {
    let found: Vec<Inst> =
        func.blocks().flat_map(|block| func.insts(block).collect::<Vec<_>>()).collect();
    for inst in found {
        match func[inst].opcode {
            Opcode::FConst => constant(func, inst),
            Opcode::FNeg => negate(func, inst),
            Opcode::SIToFP | Opcode::UIToFP => widen_then_convert(func, inst),
            Opcode::FPToSI | Opcode::FPToUI => convert_then_narrow(func, inst),
            _ => {}
        }
    }
}

/// A float constant, as the integer that spells it and a reading of those bits as the float.
///
/// This is the whole of what a `movsd` from a literal would be if there were a section to put the
/// literal in, and there is not one yet. Two instructions in a register beats a constant pool that
/// nothing else needs, and it is exactly what the bits of the immediate already say, since the IR
/// holds a float constant as its bit pattern rather than as a number.
fn constant(func: &mut Func, inst: Inst) {
    let ty = produced(func, inst);
    let Extra::Imm(imm) = func[inst].extra else { return };
    if !ty.is_float() || !ty.is_scalar() {
        return;
    }
    let int = Type::int(ty.bits());
    let bits = func[imm].bits();
    // The cast is the bits as they are stored, and `Imm::int` keeps the width, so a constant whose
    // top bit is set stays the negative integer that spells it rather than becoming a wider one.
    let spelled = ahead_const(func, inst, Imm::int(bits as i128, int), int);
    becomes(func, inst, Opcode::Bitcast, &[spelled]);
}

/// A negation, as an exclusive or with the sign bit.
///
/// C says negation flips the sign and says nothing else about it, which is not what subtracting
/// from zero does to a zero or to a not a number, so this is the operation the IR already calls
/// out as not being `0 - x`. Flipping the bit is the whole of it, and it is right for every value
/// a float can hold, the payload of a not a number included, because no other bit is touched.
///
/// The bit is flipped in a general purpose register rather than in the one the float is in. The
/// other way is one instruction rather than three and it wants the mask in memory aligned to the
/// register, which is the same section a constant pool would need.
fn negate(func: &mut Func, inst: Inst) {
    let ty = produced(func, inst);
    let Some(&arg) = func[func[inst].args].first() else { return };
    if !ty.is_float() || !ty.is_scalar() {
        return;
    }
    let int = Type::int(ty.bits());
    let bits = ahead(func, inst, Opcode::Bitcast, &[arg], int);
    let mask = ahead_const(func, inst, Imm::int(1i128 << (ty.bits() - 1), int), int);
    let flipped = ahead(func, inst, Opcode::Xor, &[bits, mask], int);
    becomes(func, inst, Opcode::Bitcast, &[flipped]);
}

/// An integer becoming a float, as a widening and the signed conversion at a width there is one at.
///
/// The widening is with the sign for a signed integer and with zeroes for an unsigned one, and
/// after it the value is the same number in a signed integer the machine converts from, so the
/// conversion is the same value and the same rounding. That is the whole of why the machine needs
/// no unsigned conversion and none at a width narrower than an `int`.
fn widen_then_convert(func: &mut Func, inst: Inst) {
    let signed = func[inst].opcode == Opcode::SIToFP;
    let Some(&arg) = func[func[inst].args].first() else { return };
    let from = func[arg].ty;
    if !from.is_int() || !from.is_scalar() {
        return;
    }
    let Some(width) = holder(from.bits(), signed) else { return };
    if width == from.bits() {
        return;
    }
    let widen = if signed { Opcode::SExt } else { Opcode::ZExt };
    let wide = ahead(func, inst, widen, &[arg], Type::int(width));
    becomes(func, inst, Opcode::SIToFP, &[wide]);
}

/// A float becoming an integer, as the signed conversion at such a width and a narrowing.
///
/// The same argument the other way round. A float the program says fits in the integer it asked
/// for fits in the signed one that holds every value of it, so converting there and keeping the
/// low bits is that value however it is read, and a float that does not fit is undefined in C and
/// unspecified in the model at either width.
fn convert_then_narrow(func: &mut Func, inst: Inst) {
    let signed = func[inst].opcode == Opcode::FPToSI;
    let ty = produced(func, inst);
    let Some(&arg) = func[func[inst].args].first() else { return };
    if !ty.is_int() || !ty.is_scalar() {
        return;
    }
    let Some(width) = holder(ty.bits(), signed) else { return };
    if width == ty.bits() {
        return;
    }
    let wide = ahead(func, inst, Opcode::FPToSI, &[arg], Type::int(width));
    becomes(func, inst, Opcode::Trunc, &[wide]);
}

/// The most moves a copy or a fill becomes before it is left alone for a call instead.
///
/// Thirty two, which is two hundred and fifty six bytes at a word a time and is a structure larger
/// than almost every one a program writes. What the number is trading is code size against a call,
/// and the exchange rate is a machine's rather than a language's, so the number lives here next to
/// the code it bounds and not in a target description that would have to be right about it for
/// every target at once.
///
/// It is a count of moves and not a count of bytes because that is what the cost is. A copy of
/// sixty four bytes between two addresses aligned to eight is eight moves and a copy of the same
/// sixty four bytes between two addresses aligned to one is sixty four, and the second is the
/// expensive one whatever the size says.
pub const UNROLL: usize = 32;

/// Rewrites every bulk copy and bulk fill, into moves when that is worth it and into a call to the
/// runtime when it is not.
///
/// A copy of more than [`UNROLL`] moves becomes a call, and so does a fill whose byte is not a
/// constant, which the front end does not write today and which would need the byte spread across
/// a word at runtime. A `memmove` is always a call, because the two sides may overlap and a run of
/// moves in one direction is only right for one of the two ways they can.
///
/// `word` is how many bytes the widest move on this machine carries. Nothing here reads a target
/// otherwise, and a copy is the same run of loads and stores everywhere.
pub fn bulk(func: &mut Func, names: &mut Interner, word: u32) {
    let found: Vec<Inst> =
        func.blocks().flat_map(|block| func.insts(block).collect::<Vec<_>>()).collect();
    for inst in found {
        match func[inst].opcode {
            Opcode::Memcpy => copy(func, names, inst, word),
            Opcode::Memset => fill(func, names, inst, word),
            Opcode::Memmove => library(func, names, inst, "memmove", word),
            _ => {}
        }
    }
}

/// One `memcpy`, as a load and a store for each word of it.
///
/// Each word is read and then written before the next is read, rather than every read being built
/// before any write the way [`crate::varargs`] copies a list. A `memcpy` is the copy whose two
/// sides the front end promises do not overlap, so what is at the source when the last word is read
/// is what was there when the first was, and reading a word at a time costs one register where
/// reading all of them first would cost as many registers as the copy has words.
fn copy(func: &mut Func, names: &mut Interner, inst: Inst, word: u32) {
    let [into, from] = func[func[inst].args] else { return };
    let Extra::Mem(mem) = func[inst].extra else { return };
    let info = func[mem];
    let Some(plan) = chunks(info, word) else { return library(func, names, inst, "memcpy", word) };
    for (at, width) in plan {
        let ty = Type::int(width * 8);
        let access = MemInfo { size: u64::from(width), align: width.min(info.align), ..info };
        let there = stepped(func, inst, from, at);
        let word = read(func, inst, there, access, ty);
        let here = stepped(func, inst, into, at);
        write(func, inst, word, here, access);
    }
    func.remove_inst(inst);
}

/// One `memset`, as a store of the byte spread across each word of it.
///
/// The byte is a constant, so the word it spreads into is a constant too and the spreading is done
/// here rather than by the program. The front end writes a `memset` for the part of an object an
/// initialiser did not name, where the byte is always zero, and the general case is written anyway
/// because the arithmetic is the same and being right about `0xff` costs nothing.
fn fill(func: &mut Func, names: &mut Interner, inst: Inst, word: u32) {
    let [into, byte] = func[func[inst].args] else { return };
    let Extra::Mem(mem) = func[inst].extra else { return };
    let info = func[mem];
    let Some(spelled) = literal(func, byte) else {
        return library(func, names, inst, "memset", word);
    };
    let Some(plan) = chunks(info, word) else { return library(func, names, inst, "memset", word) };
    for (at, width) in plan {
        let ty = Type::int(width * 8);
        let access = MemInfo { size: u64::from(width), align: width.min(info.align), ..info };
        let value = ahead_const(func, inst, Imm::int(spread(spelled, width) as i128, ty), ty);
        let here = stepped(func, inst, into, at);
        write(func, inst, value, here, access);
    }
    func.remove_inst(inst);
}

/// One bulk operation as a call to the routine of that name in the runtime.
///
/// This is what a copy too large to unroll becomes, and what a `memmove` and a fill with a
/// computed byte become whatever their size. The routine is `rucc-builtins`' on a freestanding
/// target and the C library's on a hosted one, and the call is the same either way because the two
/// have the same names and the same signatures on purpose.
///
/// The arguments are the C ones and not the IR ones. The IR holds the size beside the instruction
/// where C passes it, and holds a fill byte as a byte where C passes an `int`, so the size becomes
/// a constant in a register and the byte is widened. The value each returns is its first argument,
/// which nothing reads, so the call is built as returning nothing rather than as returning a
/// pointer nobody looks at.
fn library(func: &mut Func, names: &mut Interner, inst: Inst, routine: &str, word: u32) {
    let [into, second] = func[func[inst].args] else { return };
    let Extra::Mem(mem) = func[inst].extra else { return };
    let size = func[mem].size;

    // `size_t`, which is as wide as a general purpose register on every target here. Taken from
    // the machine rather than written as sixty four so that a thirty two bit target gets the
    // argument its own C library declares.
    let words = Type::int(word * 8);
    let count = ahead_const(func, inst, Imm::int(i128::from(size), words), words);
    // A fill passes an `int` where the IR passes the byte itself, and the widening is a zero
    // extension because the routine looks at the low eight bits and nothing else.
    let second = match routine {
        "memset" => widened(func, inst, second),
        _ => second,
    };

    let sig = func.add_signature(Signature::new().with_params(&[
        Type::PTR,
        if routine == "memset" { Type::int(32) } else { Type::PTR },
        words,
    ]));
    let callee = names.intern(routine);
    let varargs = func.push_abis(&[]);
    let info = func.add_call(CallInfo { callee: Some(callee), signature: sig, varargs });
    let args = func.push_values(&[into, second, count]);
    let data = &mut func[inst];
    data.opcode = Opcode::Call;
    data.args = args;
    data.extra = Extra::Call(info);
    data.flags = data.flags.intersection(Flags::legal_on(Opcode::Call));
}

/// A value widened to an `int`, or the value itself when it is one already.
fn widened(func: &mut Func, inst: Inst, value: Value) -> Value {
    let int = Type::int(32);
    let ty = func[value].ty;
    if ty == int {
        return value;
    }
    ahead(func, inst, Opcode::ZExt, &[value], int)
}

/// Where each word of a block of memory starts and how wide it is, or nothing for a block that is
/// more words than [`UNROLL`].
///
/// The widest word is the smaller of what the machine moves at once and what the block is known to
/// be aligned to, because a load wider than the alignment is a fault on a machine that checks and
/// this pass does not know whether the one it is compiling for does. That costs a copy of a
/// character array a move per byte, which is exactly the copy the threshold sends to a call.
///
/// The width halves whenever what is left is narrower than it, so a block of thirteen bytes aligned
/// to eight is eight, four and one rather than thirteen ones. Every offset is a multiple of the
/// width at it, since each width divides the sum of the wider ones in front of it, which is what
/// lets the alignment of each access be written down as the width.
fn chunks(info: MemInfo, word: u32) -> Option<Vec<(u64, u32)>> {
    let widest = word.min(info.align).max(1);
    if !widest.is_power_of_two() {
        return None;
    }
    let mut plan = Vec::new();
    let mut at = 0;
    let mut width = u64::from(widest);
    while at < info.size {
        while width > info.size - at {
            width /= 2;
        }
        plan.push((at, u32::try_from(width).ok()?));
        at += width;
        if plan.len() > UNROLL {
            return None;
        }
    }
    Some(plan)
}

/// The byte a fill writes, when the program said which one rather than working it out.
fn literal(func: &Func, value: Value) -> Option<u8> {
    let Def::Result { inst, .. } = func[value].def else { return None };
    if func[inst].opcode != Opcode::IConst {
        return None;
    }
    let Extra::Imm(imm) = func[inst].extra else { return None };
    u8::try_from(func[imm].bits() & 0xff).ok()
}

/// One byte repeated across a word of that many bytes, which is what a fill stores.
fn spread(byte: u8, width: u32) -> u64 {
    (0..width).fold(0, |word, at| word | u64::from(byte) << (at * 8))
}

/// The address that far into a block, written in front of an instruction, or the block itself for
/// the word at the front of it.
fn stepped(func: &mut Func, inst: Inst, block: Value, at: u64) -> Value {
    if at == 0 {
        return block;
    }
    let step = ahead_const(func, inst, Imm::int(i128::from(at), Type::int(64)), Type::int(64));
    ahead(func, inst, Opcode::PtrAdd, &[block, step], Type::PTR)
}

/// A load put in front of an instruction, and the value it reads.
fn read(func: &mut Func, inst: Inst, from: Value, info: MemInfo, ty: Type) -> Value {
    let extra = Extra::Mem(func.add_mem(info));
    let args = func.push_values(&[from]);
    written(func, inst, InstData { args, extra, ..InstData::new(Opcode::Load) }, ty)
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

/// The width the machine converts at that holds every value of an integer of this one.
///
/// The machine converts between a float and a signed integer at thirty two bits and at sixty four
/// and at no other width, so a conversion anywhere else is one of those two with a widening in
/// front of it or a narrowing behind it. Which of the two it is, is the narrower one the values
/// fit in, and an unsigned integer of `bits` bits needs one more bit than that to be signed in.
///
/// `None` is a width no signed integer here holds, which is only an unsigned sixty four bit one.
fn holder(bits: u32, signed: bool) -> Option<u32> {
    match if signed { bits } else { bits + 1 } {
        ..=32 => Some(32),
        33..=64 => Some(64),
        _ => None,
    }
}

/// The type of the one value an instruction produces.
///
/// Every opcode this pass touches produces exactly one, so an instruction that produces none is
/// one the caller has already gone wrong about and the void type says so without panicking.
fn produced(func: &Func, inst: Inst) -> Type {
    func[inst].first_result.map_or(Type::VOID, |value| func[value].ty)
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
/// The last instruction of a rewrite is the original rather than a new one, so the value the rest
/// of the function reads is the value it already read and nothing has to be substituted anywhere.
/// The type of that value does not change either, because every rewrite here ends at the type it
/// started at.
fn becomes(func: &mut Func, inst: Inst, opcode: Opcode, args: &[Value]) {
    let args = func.push_values(args);
    let data = &mut func[inst];
    data.opcode = opcode;
    data.args = args;
    data.extra = Extra::None;
    // What the program said about rounding and about not a numbers is still true of the
    // instructions it became, and what is no longer meaningful is dropped rather than carried.
    data.flags = data.flags.intersection(Flags::legal_on(opcode));
}

/// The blocks a chain of `n` cases needs beyond the ones the program already had.
///
/// Here so that a test can say the number rather than count it, and so that whoever writes the
/// jump table has one place to compare against.
#[must_use]
pub fn blocks_for(cases: usize) -> usize {
    cases.saturating_sub(1)
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{Builder, Flags, Float, Func, Module, Opcode, Signature, Type};
    use rucc_target::{Arch, Env, Os, TargetInfo, Triple};

    use rucc_ir::{Extra, InstData, MemInfo, MemOrder};

    use super::{UNROLL, blocks_for, bulk, chunks, floats, spread, switches};

    fn target() -> TargetInfo {
        TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu))
    }

    /// `int sw(int x) { switch (x) { case 1: return 10; case 2: return 20; default: return 30; } }`
    /// as the walk builds it, which is the program in issue 275.
    fn built(cases: &[i128]) -> (Interner, Func) {
        let mut names = Interner::new();
        let int = Type::int(32);
        let mut func = Func::new(
            names.intern("sw"),
            Signature::new().with_params(&[int]).with_returns(&[int]),
        );
        let entry = func.create_block();
        let x = func.append_param(entry, int);

        let default = func.create_block();
        let arms: Vec<_> = cases.iter().map(|_| func.create_block()).collect();
        let table: Vec<(i128, rucc_ir::Block)> =
            cases.iter().copied().zip(arms.iter().copied()).collect();
        Builder::new(&mut func, entry).switch(x, default, &table);

        for (index, &arm) in arms.iter().enumerate() {
            let mut build = Builder::new(&mut func, arm);
            let what = i128::try_from(index).expect("a small number of cases");
            let v = build.iconst(int, (what + 1) * 10);
            build.ret(&[v]);
        }
        let mut build = Builder::new(&mut func, default);
        let v = build.iconst(int, 30);
        build.ret(&[v]);
        (names, func)
    }

    fn count(func: &Func) -> usize {
        func.blocks().count()
    }

    fn printed(func: &Func, names: &mut Interner) -> String {
        let module = Module::new(names.intern("sw.c"), &target());
        rucc_ir::print_func(&module, func, names)
    }

    #[test]
    fn a_switch_becomes_a_compare_and_a_branch_for_each_case() {
        let (mut names, mut func) = built(&[1, 2]);
        let before = count(&func);
        switches(&mut func);
        assert_eq!(count(&func), before + blocks_for(2));

        let text = printed(&func, &mut names);
        assert!(!text.contains("switch"), "the switch is gone: {text}");
        assert_eq!(text.matches("icmp eq").count(), 2, "one compare per case: {text}");
        assert_eq!(text.matches("br_if").count(), 2, "one branch per case: {text}");
    }

    #[test]
    fn the_last_case_falls_to_the_default_rather_than_to_a_block_of_its_own() {
        let (_, mut func) = built(&[7]);
        let before = count(&func);
        switches(&mut func);
        // One case needs no chain block at all: the one compare goes to the arm or to the default.
        assert_eq!(count(&func), before);
        assert_eq!(blocks_for(1), 0);
    }

    #[test]
    fn a_switch_with_only_a_default_is_a_jump() {
        let (_, mut func) = built(&[]);
        switches(&mut func);
        let entry = func.entry().expect("an entry block");
        let term = func.terminator(entry).expect("a terminator");
        assert_eq!(func[term].opcode, Opcode::Jump);
    }

    /// The rewrite has to leave a function the verifier still accepts, since every check it makes
    /// is one the rest of the back end assumes and none of them is rechecked after this runs.
    #[test]
    fn what_comes_out_is_valid_ir() {
        let (mut names, mut func) = built(&[1, 2, 3, 4]);
        switches(&mut func);
        let module = Module::new(names.intern("sw.c"), &target());
        rucc_ir::verify_func(&module, &func, &names).expect("the rewrite builds valid IR");
    }

    /// Nothing else is touched, which matters because this runs over every function whether or not
    /// one has a `switch` in it.
    #[test]
    fn a_function_with_no_switch_is_left_exactly_as_it_was() {
        let mut names = Interner::new();
        let int = Type::int(32);
        let mut func =
            Func::new(names.intern("f"), Signature::new().with_params(&[int]).with_returns(&[int]));
        let entry = func.create_block();
        let x = func.append_param(entry, int);
        Builder::new(&mut func, entry).ret(&[x]);

        let before = printed(&func, &mut names);
        switches(&mut func);
        assert_eq!(printed(&func, &mut names), before);
    }

    /// A function of one parameter and one result, with a body somebody else writes.
    ///
    /// The float rewrites are each one instruction becoming several in the middle of a block, so
    /// what a test needs is a block with something around the instruction rather than a shape.
    fn one(
        params: &[Type],
        returns: &[Type],
        body: impl FnOnce(&mut Builder<'_>, &[rucc_ir::Value]),
    ) -> (Interner, Func) {
        let mut names = Interner::new();
        let mut func = Func::new(
            names.intern("f"),
            Signature::new().with_params(params).with_returns(returns),
        );
        let entry = func.create_block();
        let args: Vec<_> = params.iter().map(|&ty| func.append_param(entry, ty)).collect();
        let mut build = Builder::new(&mut func, entry);
        body(&mut build, &args);
        (names, func)
    }

    fn f64() -> Type {
        Type::float(Float::F64)
    }

    fn f32() -> Type {
        Type::float(Float::F32)
    }

    /// `double c(void) { return 1.5; }`, which is the constant nothing in the rule set can name.
    #[test]
    fn a_float_constant_becomes_the_integer_that_spells_it_and_a_reading_of_those_bits() {
        let (mut names, mut func) = one(&[], &[f64()], |build, _| {
            let k = build.fconst(f64(), 0x3ff8_0000_0000_0000);
            build.ret(&[k]);
        });
        floats(&mut func);

        let text = printed(&func, &mut names);
        assert!(!text.contains("fconst"), "the float constant is gone: {text}");
        assert!(text.contains("iconst.i64 4609434218613702656"), "the bits, as an integer: {text}");
        assert!(text.contains("bitcast"), "read back as the float: {text}");
    }

    /// The width follows the format rather than being the widest one, so a `float` constant is an
    /// `i32` and reaches `movd` rather than `movq`.
    #[test]
    fn a_constant_at_the_narrow_format_is_an_integer_of_the_narrow_width() {
        let (mut names, mut func) = one(&[], &[f32()], |build, _| {
            let k = build.fconst(f32(), 0x4020_0000);
            build.ret(&[k]);
        });
        floats(&mut func);
        assert!(printed(&func, &mut names).contains("iconst.i32"), "an i32, not an i64");
    }

    /// `double n(double x) { return -x; }`. Flipping the sign bit is what C means and subtracting
    /// from zero is not, so what this asserts is the exclusive or and the mask it is given.
    #[test]
    fn a_negation_flips_the_sign_bit_and_touches_no_other() {
        let (mut names, mut func) = one(&[f64()], &[f64()], |build, args| {
            let n = build.unary(Opcode::FNeg, args[0], f64());
            build.ret(&[n]);
        });
        floats(&mut func);

        let text = printed(&func, &mut names);
        assert!(!text.contains("fneg"), "the negation is gone: {text}");
        assert!(!text.contains("fsub"), "and it did not become a subtraction: {text}");
        assert!(text.contains("iconst.i64 -9223372036854775808"), "the sign bit alone: {text}");
        assert_eq!(text.matches("xor").count(), 1, "one exclusive or: {text}");
        assert_eq!(text.matches("bitcast").count(), 2, "there and back: {text}");
    }

    /// `double u(unsigned x) { return x; }`, which is a widening and the signed conversion.
    #[test]
    fn an_unsigned_integer_becoming_a_float_widens_first_and_then_converts_as_signed() {
        let (mut names, mut func) = one(&[Type::int(32)], &[f64()], |build, args| {
            let d = build.unary(Opcode::UIToFP, args[0], f64());
            build.ret(&[d]);
        });
        floats(&mut func);

        let text = printed(&func, &mut names);
        assert!(!text.contains("uitofp"), "the unsigned conversion is gone: {text}");
        assert!(text.contains("zext.i64"), "widened with zeroes: {text}");
        assert!(text.contains("sitofp.f64"), "converted as signed: {text}");
    }

    /// `unsigned t(double x) { return x; }`, which is the same argument the other way round.
    #[test]
    fn a_float_becoming_an_unsigned_integer_converts_as_signed_first_and_then_narrows() {
        let (mut names, mut func) = one(&[f64()], &[Type::int(32)], |build, args| {
            let n = build.unary(Opcode::FPToUI, args[0], Type::int(32));
            build.ret(&[n]);
        });
        floats(&mut func);

        let text = printed(&func, &mut names);
        assert!(!text.contains("fptoui"), "the unsigned conversion is gone: {text}");
        assert!(text.contains("fptosi.i64"), "converted as signed: {text}");
        assert!(text.contains("trunc.i32"), "and narrowed to what was asked: {text}");
    }

    /// `signed char a(double x) { return (signed char)x; }`, which the front end writes as a
    /// conversion straight to eight bits and the machine has no instruction for at that width.
    #[test]
    fn a_conversion_narrower_than_the_machine_has_is_one_it_has_and_a_narrowing() {
        let (mut names, mut func) = one(&[f64()], &[Type::int(8)], |build, args| {
            let n = build.unary(Opcode::FPToSI, args[0], Type::int(8));
            build.ret(&[n]);
        });
        floats(&mut func);

        let text = printed(&func, &mut names);
        assert!(text.contains("fptosi.i32"), "converted at a width there is one at: {text}");
        assert!(text.contains("trunc.i8"), "and narrowed to what was asked: {text}");
    }

    /// The same the other way, where the widening carries the sign because the value has one.
    #[test]
    fn a_signed_integer_narrower_than_the_machine_converts_from_is_widened_with_its_sign() {
        let (mut names, mut func) = one(&[Type::int(8)], &[f64()], |build, args| {
            let d = build.unary(Opcode::SIToFP, args[0], f64());
            build.ret(&[d]);
        });
        floats(&mut func);

        let text = printed(&func, &mut names);
        assert!(text.contains("sext.i32"), "widened with the sign and not with zeroes: {text}");
        assert!(!text.contains("zext"), "widened with the sign and not with zeroes: {text}");
        assert!(text.contains("sitofp.f64"), "converted at a width there is one at: {text}");
    }

    /// The table the two of them share, which is where the whole argument about widths lives.
    #[test]
    fn the_width_a_conversion_happens_at_is_the_narrowest_one_that_holds_the_values() {
        use super::holder;
        for bits in [1, 8, 16, 32] {
            assert_eq!(holder(bits, true), Some(32), "a signed {bits} bit value fits in an int");
        }
        assert_eq!(holder(64, true), Some(64));
        for bits in [1, 8, 16, 31] {
            assert_eq!(holder(bits, false), Some(32), "an unsigned {bits} bit value does too");
        }
        // The one more bit an unsigned value needs is what makes these two the wider width.
        assert_eq!(holder(32, false), Some(64));
        assert_eq!(holder(64, false), None);
    }

    /// Sixty four bits is where the argument runs out, because an unsigned value of that width is
    /// not a signed value of any width the IR has. Both are left for the lowering to refuse by
    /// name, which is a better message than one about the instructions they would have become.
    #[test]
    fn the_unsigned_conversions_at_the_widest_width_are_left_alone() {
        let (mut names, mut func) = one(&[Type::int(64)], &[f64()], |build, args| {
            let d = build.unary(Opcode::UIToFP, args[0], f64());
            build.ret(&[d]);
        });
        let before = printed(&func, &mut names);
        floats(&mut func);
        assert_eq!(printed(&func, &mut names), before);

        let (mut names, mut func) = one(&[f64()], &[Type::int(64)], |build, args| {
            let n = build.unary(Opcode::FPToUI, args[0], Type::int(64));
            build.ret(&[n]);
        });
        let before = printed(&func, &mut names);
        floats(&mut func);
        assert_eq!(printed(&func, &mut names), before);
    }

    /// The same obligation the `switch` rewrite has, for the same reason: nothing after this
    /// checks the IR again and everything after it assumes what the verifier would have said.
    #[test]
    fn what_the_float_rewrites_leave_is_valid_ir() {
        let (mut names, mut func) = one(&[Type::int(32)], &[f64()], |build, args| {
            let k = build.fconst(f64(), 0x3ff8_0000_0000_0000);
            let d = build.unary(Opcode::UIToFP, args[0], f64());
            let n = build.unary(Opcode::FNeg, d, f64());
            let s = build.binary(Opcode::FAdd, n, k, Flags::NONE);
            build.ret(&[s]);
        });
        floats(&mut func);
        let module = Module::new(names.intern("f.c"), &target());
        rucc_ir::verify_func(&module, &func, &names).expect("the rewrite builds valid IR");
    }

    /// Nothing else is touched, for the same reason the `switch` pass has that test: this runs
    /// over every function whether or not one has a float in it.
    #[test]
    fn a_function_with_no_floats_in_it_is_left_exactly_as_it_was() {
        let (mut names, mut func) = one(&[Type::int(32)], &[Type::int(32)], |build, args| {
            build.ret(&[args[0]]);
        });
        let before = printed(&func, &mut names);
        floats(&mut func);
        assert_eq!(printed(&func, &mut names), before);
    }
    fn access(size: u64, align: u32) -> MemInfo {
        MemInfo { size, align, order: MemOrder::NotAtomic, tbaa: None }
    }

    /// `void c(void *to, const void *from) { *(T *)to = *(const T *)from; }` for a `T` of that
    /// size and alignment, which is what the front end writes for a structure assignment.
    fn moving(opcode: Opcode, size: u64, align: u32, byte: Option<i128>) -> (Interner, Func) {
        one(&[Type::PTR, Type::PTR], &[], |build, args| {
            let second = match byte {
                Some(value) => build.iconst(Type::int(8), value),
                None => args[1],
            };
            let mem = build.func().add_mem(access(size, align));
            let operands = build.func().push_values(&[args[0], second]);
            let data = InstData { args: operands, extra: Extra::Mem(mem), ..InstData::new(opcode) };
            build.inst(data, &[]);
            build.ret(&[]);
        })
    }

    fn copying(size: u64, align: u32) -> (Interner, Func) {
        moving(Opcode::Memcpy, size, align, None)
    }

    fn filling(size: u64, align: u32, byte: i128) -> (Interner, Func) {
        moving(Opcode::Memset, size, align, Some(byte))
    }

    /// The plan a copy of that size and alignment becomes, as widths, which is what the offsets
    /// follow from.
    fn widths(size: u64, align: u32) -> Option<Vec<u32>> {
        Some(chunks(access(size, align), 8)?.into_iter().map(|(_, width)| width).collect())
    }

    /// `struct point { int x, y; } a, b; a = b;`, which is sixteen bytes aligned to eight.
    #[test]
    fn a_copy_becomes_a_load_and_a_store_for_each_word_of_it() {
        let (mut names, mut func) = copying(16, 8);
        bulk(&mut func, &mut names, 8);

        let text = printed(&func, &mut names);
        assert!(!text.contains("memcpy"), "the copy is gone: {text}");
        assert_eq!(text.matches("load.i64").count(), 2, "a load per word: {text}");
        assert_eq!(text.matches("store").count(), 2, "a store per word: {text}");
        assert_eq!(
            text.matches("ptr_add").count(),
            2,
            "no offset for the word at the front: {text}"
        );
    }

    /// A word is as wide as the block is known to be aligned to and no wider, because a load
    /// wider than that faults on a machine that checks and this does not know whether the one it
    /// is compiling for does.
    #[test]
    fn a_word_is_as_wide_as_the_block_is_aligned_to() {
        assert_eq!(widths(16, 8), Some(vec![8, 8]));
        assert_eq!(widths(16, 4), Some(vec![4, 4, 4, 4]));
        assert_eq!(widths(4, 1), Some(vec![1, 1, 1, 1]));
    }

    /// What is left over is narrower words rather than a run of bytes, so thirteen bytes aligned
    /// to eight is three moves and not six.
    #[test]
    fn what_is_left_over_is_narrower_words_and_not_a_run_of_bytes() {
        assert_eq!(widths(13, 8), Some(vec![8, 4, 1]));
        assert_eq!(widths(3, 8), Some(vec![2, 1]));
        assert_eq!(widths(1, 8), Some(vec![1]));
    }

    /// Every offset is a multiple of the width at it, which is what lets the alignment of each
    /// access be written down as its width.
    #[test]
    fn every_word_starts_somewhere_it_is_aligned_for() {
        for (at, width) in chunks(access(13, 8), 8).expect("a plan for thirteen bytes") {
            assert_eq!(at % u64::from(width), 0, "{at} is a multiple of {width}");
        }
    }

    /// `struct big b = { 0 };`, where the part the initialiser did not name is zeroed.
    #[test]
    fn a_fill_is_the_byte_spread_across_each_word() {
        let (mut names, mut func) = filling(16, 8, 0);
        bulk(&mut func, &mut names, 8);

        let text = printed(&func, &mut names);
        assert!(!text.contains("memset"), "the fill is gone: {text}");
        assert_eq!(text.matches("store").count(), 2, "a store per word: {text}");
        assert!(!text.contains("load"), "a fill reads nothing: {text}");
    }

    /// The spreading is arithmetic on the byte, which is the thing a rule cannot do and the
    /// reason this pass exists at all.
    #[test]
    fn the_byte_is_repeated_across_the_word_it_is_stored_as() {
        assert_eq!(spread(0, 8), 0);
        assert_eq!(spread(0xff, 1), 0xff);
        assert_eq!(spread(0xff, 4), 0xffff_ffff);
        assert_eq!(spread(0xab, 2), 0xabab);
        assert_eq!(spread(0xab, 8), 0xabab_abab_abab_abab);
    }

    /// A copy larger than the threshold is a call to the runtime rather than a run of moves.
    #[test]
    fn a_copy_too_large_to_unroll_becomes_a_call_to_the_runtime() {
        let size = u64::try_from(UNROLL).expect("a small threshold") + 1;
        let (mut names, mut func) = copying(size, 1);
        bulk(&mut func, &mut names, 8);
        let text = printed(&func, &mut names);
        assert!(text.contains("call @memcpy"), "a call and not a bulk move: {text}");

        // And the one word under it is moves, because the threshold counts moves rather than
        // bytes and the whole point of the threshold is that a small copy does not pay for a call.
        let (mut names, mut func) = copying(size - 1, 1);
        bulk(&mut func, &mut names, 8);
        assert!(!printed(&func, &mut names).contains("memcpy"), "one word under it is unrolled");
    }

    /// The call passes what C passes, which is not what the IR holds. The size lives beside the
    /// instruction in the IR and travels in a register in the call.
    #[test]
    fn the_call_passes_the_size_that_the_instruction_carried_beside_it() {
        let size = u64::try_from(UNROLL).expect("a small threshold") + 1;
        let (mut names, mut func) = copying(size, 1);
        bulk(&mut func, &mut names, 8);
        let text = printed(&func, &mut names);
        assert!(text.contains(&format!("{size}")), "the size is an argument now: {text}");
    }

    /// A `memmove` is a call whatever its size, because the two sides may overlap and a run of
    /// moves in one direction is right for only one of the two ways they can.
    #[test]
    fn a_move_is_a_call_however_small_it_is() {
        let (mut names, mut func) = moving(Opcode::Memmove, 8, 8, None);
        bulk(&mut func, &mut names, 8);
        let text = printed(&func, &mut names);
        assert!(text.contains("call @memmove"), "a call and not a run of moves: {text}");
    }

    /// A fill whose byte the program works out rather than names. Spreading a value across a
    /// word at runtime is a multiply, so this is a call rather than moves however small it is.
    #[test]
    fn a_fill_whose_byte_is_not_a_constant_becomes_a_call() {
        let (mut names, mut func) = one(&[Type::PTR, Type::int(8)], &[], |build, args| {
            let mem = build.func().add_mem(access(8, 8));
            let operands = build.func().push_values(&[args[0], args[1]]);
            let data = InstData {
                args: operands,
                extra: Extra::Mem(mem),
                ..InstData::new(Opcode::Memset)
            };
            build.inst(data, &[]);
            build.ret(&[]);
        });
        bulk(&mut func, &mut names, 8);
        let text = printed(&func, &mut names);
        assert!(text.contains("call @memset"), "a call and not a run of stores: {text}");
        // Widened, because C passes the byte as an `int` and the IR holds it as a byte.
        assert!(text.contains("zext.i32"), "the byte is widened to what C passes: {text}");
    }

    /// A machine whose widest move is four bytes gets four byte words out of an eight byte block,
    /// however well aligned the block is.
    #[test]
    fn no_word_is_wider_than_the_machine_moves_at_once() {
        assert_eq!(chunks(access(8, 8), 4).map(|plan| plan.len()), Some(2));
        assert_eq!(chunks(access(8, 8), 8).map(|plan| plan.len()), Some(1));
    }

    #[test]
    fn what_a_copy_becomes_is_ir_that_verifies() {
        let (mut names, mut func) = copying(13, 8);
        bulk(&mut func, &mut names, 8);
        let module = Module::new(names.intern("c.c"), &target());
        rucc_ir::verify_func(&module, &func, &names).expect("the rewrite builds valid IR");
    }

    #[test]
    fn what_a_fill_becomes_is_ir_that_verifies() {
        let (mut names, mut func) = filling(13, 8, 0xff);
        bulk(&mut func, &mut names, 8);
        let module = Module::new(names.intern("f.c"), &target());
        rucc_ir::verify_func(&module, &func, &names).expect("the rewrite builds valid IR");
    }

    #[test]
    fn what_a_copy_too_large_to_unroll_becomes_is_ir_that_verifies() {
        let size = u64::try_from(UNROLL).expect("a small threshold") + 1;
        let (mut names, mut func) = copying(size, 1);
        bulk(&mut func, &mut names, 8);
        let module = Module::new(names.intern("c.c"), &target());
        rucc_ir::verify_func(&module, &func, &names).expect("the call is valid IR");
    }

    /// Nothing else is touched, for the same reason the other two passes have that test.
    #[test]
    fn a_function_with_no_bulk_move_in_it_is_left_exactly_as_it_was() {
        let (mut names, mut func) = one(&[Type::int(32)], &[Type::int(32)], |build, args| {
            build.ret(&[args[0]]);
        });
        let before = printed(&func, &mut names);
        bulk(&mut func, &mut names, 8);
        assert_eq!(printed(&func, &mut names), before);
    }
}
