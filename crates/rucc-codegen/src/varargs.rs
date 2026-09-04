//! What a function does to read the arguments its own signature does not name.
//!
//! Design: `spec/12-abi-and-runtime.md`, which is where the layout below comes from.
//!
//! A variadic callee has a problem an ordinary one does not. Six of its arguments arrived in
//! general purpose registers and eight more in vector ones, and it cannot know which of those hold
//! anything, because what it was passed is a thing only the caller knew. Registers are also not
//! addressable, and `va_arg` walks arguments one after another at run time, which is walking
//! addresses. So the convention says the callee spills all fourteen of them into a block of its own
//! frame on the way in, and from then on every argument it was passed is somewhere in memory: the
//! ones that came in registers are in that block, and the ones that did not are in the caller's
//! argument area where they were left.
//!
//! That block is the register save area, and a `va_list` is four fields saying how far into the
//! arguments the walk has got:
//!
//! ```text
//! offset  0  gp_offset          bytes into the save area of the next argument from a gpr
//! offset  4  fp_offset          bytes into the save area of the next argument from an xmm
//! offset  8  overflow_arg_area  the next argument that came in the caller's memory
//! offset 16  reg_save_area      the bottom of the save area
//! ```
//!
//! `va_start` fills all four in. The two offsets do not start at zero: the arguments the signature
//! does name took registers too, and they took the first ones, so each offset starts past them.
//! `va_arg` is then one question asked at run time, which is whether the offset for its file has run
//! off the end of the save area. If it has not, the argument is in the save area and the offset
//! steps on by a slot. If it has, the argument is in the caller's memory and the overflow pointer
//! steps on by a word instead.
//!
//! # Why the layout is exactly the psABI's and not a convenient one
//!
//! Nothing outside the function can see the save area, so its shape looks like a private decision.
//! It is not one, because a `va_list` is a thing a program hands to another function, and the
//! function it usually hands it to is `vfprintf` in the C library, which somebody else compiled and
//! which walks the list by the rules in the psABI document. So the offsets are the document's
//! offsets, the area is the document's one hundred and seventy six bytes, and the eight bytes
//! between two general purpose slots and the sixteen between two vector ones are the document's too.
//!
//! What is not the document's is what goes in the upper half of a vector slot, and the answer here
//! is nothing at all. A slot is sixteen bytes wide because the register is, and the low eight are
//! the whole of what any reader of a list looks at, since the widest thing `va_arg` names in this
//! compiler is a `double`. So the spill writes eight bytes per vector register rather than sixteen
//! and leaves the eight above them holding whatever the frame held. A reader that wanted all sixteen
//! would be reading a vector type, which is issue #200 and is not a thing yet.
//!
//! # What is here and what is next door
//!
//! `va_arg` becomes a compare and a branch, and this is where, because a rewrite that needs new
//! blocks has to happen before selection for the reason [`crate::expand`] gives. Everything it needs
//! is in the list it was handed, so it needs nothing from the frame and can run here.
//!
//! An aggregate read off a list is the same instruction under another name, because an aggregate is
//! not a value and there is nothing for one result to be, so that one answers where the object is
//! instead. Over two eightbytes it is class MEMORY whatever its members are, which means it is in
//! the caller's argument area and there is no question to ask about which half of the walk it is
//! in: the overflow pointer says where it is and steps on past it. Sixteen bytes and under arrived
//! in registers, and reading one out means knowing which register file each of its eightbytes came
//! from, which is the classification and is not something the size and the alignment say. That is
//! issue #339 and is left alone here.
//!
//! `va_start` is the other way round. Three of the four fields it writes are distances into a frame
//! that does not exist yet, so it stays an instruction as far as [`crate::lower`], which builds it
//! out of the frame the way it builds an `alloca`. The spill that fills the save area is written
//! there for the same reason.

use rucc_ir::{
    Builder, Extra, Flags, Func, Imm, Inst, InstData, IntPred, MemInfo, MemOrder, Opcode, Type,
    Value,
};
use rucc_target::CallRegs;

/// Where the count of general purpose register bytes already walked is.
pub const GP_OFFSET: i64 = 0;
/// Where the count of vector register bytes already walked is.
pub const FP_OFFSET: i64 = 4;
/// Where the pointer to the next argument in the caller's memory is.
pub const OVERFLOW: i64 = 8;
/// Where the pointer to the bottom of the register save area is.
pub const SAVE_AREA: i64 = 16;
/// How many bytes one `va_list` is, which is what a `va_copy` moves.
pub const SIZE: u64 = 24;
/// How wide the slot one vector register is saved in is, which is how wide the register is whatever
/// this actually writes into it.
pub const VECTOR_SLOT: u32 = 16;

/// How big a callee's register save area is and where its two halves are.
///
/// Worked out from the convention rather than written down, so that a convention with a different
/// number of argument registers gets an area the right size for it without anything here changing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Area {
    /// How many bytes of it the general purpose registers take, which is also where the vector half
    /// begins, since the general purpose half is first and starts at nothing.
    pub floats_at: u32,
    /// How many bytes the whole of it is.
    pub size: u32,
    /// How many registers of each file it holds, general purpose first.
    counts: (u32, u32),
    /// How far apart two general purpose slots are, which is a word.
    word: u32,
}

impl Area {
    /// The save area a variadic callee under that convention needs.
    #[must_use]
    pub fn of(conv: &CallRegs) -> Self {
        let ints = u32::try_from(conv.int_args.len()).unwrap_or(0);
        let floats = u32::try_from(conv.sse_args.len()).unwrap_or(0);
        let floats_at = conv.word * ints;
        Self {
            floats_at,
            size: floats_at + VECTOR_SLOT * floats,
            counts: (ints, floats),
            word: conv.word,
        }
    }

    /// How far apart two of a file's slots are.
    #[must_use]
    pub fn stride(self, float: bool) -> u32 {
        if float { VECTOR_SLOT } else { self.word }
    }

    /// Where a file's first slot is, which is what `va_start` writes into that file's field when
    /// the signature named no argument that file carried.
    #[must_use]
    pub fn starts_at(self, float: bool) -> u32 {
        if float { self.floats_at } else { 0 }
    }

    /// The offset of a file's last slot, which is the threshold `va_arg` compares against.
    ///
    /// The last slot's own offset and not the end of the area, because an offset equal to the end
    /// is one slot past the last argument while an offset a slot below the end is the last argument
    /// itself. An empty file has no such offset and nothing here has one.
    #[must_use]
    pub fn last(self, float: bool) -> Option<u32> {
        let count = if float { self.counts.1 } else { self.counts.0 };
        let last = count.checked_sub(1)?;
        Some(self.starts_at(float) + self.stride(float) * last)
    }
}

/// Rewrites every `va_arg`, `va_copy` and `va_end` in the function, and leaves `va_start` alone.
///
/// Those three are the ones made only of reads and writes of a list some pointer already reaches,
/// so none of them needs to know anything about the frame and all three can be done here.
/// `va_start` is the one that does need the frame, and [`crate::lower`] has it.
///
/// A convention whose list is not the four field one is left alone entirely, and a function using
/// one is then refused further down with the message about a rule that does not exist. Windows is
/// the one such convention here: its list is a plain pointer, its callee spills its four register
/// arguments into the shadow space the caller already reserved rather than into an area of its own,
/// and none of the three rewrites below is right for any of that.
pub fn lists(func: &mut Func, conv: &CallRegs) {
    if conv.shared_positions {
        return;
    }
    let area = Area::of(conv);
    let found: Vec<Inst> =
        func.blocks().flat_map(|block| func.insts(block).collect::<Vec<_>>()).collect();
    for inst in found {
        match func[inst].opcode {
            Opcode::VaArg => next(func, inst, area),
            Opcode::VaObject => object(func, inst, area),
            Opcode::VaCopy => copy(func, inst),
            // Nothing at all, which is what the psABI says it is. The instruction was still worth
            // emitting, because it says the list stops being read here, and here is where that
            // stops being worth saying.
            Opcode::VaEnd => func.remove_inst(inst),
            _ => {}
        }
    }
}

/// One `va_arg`, as the branch on whether the argument it wants is still in the save area.
///
/// The block the instruction was in is cut in two at the instruction. What was above it stays where
/// it is and gets the compare and the branch, what was below it moves into a new block that takes
/// the address as a parameter, and the `va_arg` itself becomes the load at the top of that block.
/// Turning it into the load rather than replacing it keeps the value the rest of the function reads
/// the value it already read, so nothing has to be substituted anywhere, and the two paths meet at a
/// block parameter because the IR has no variables for them to meet at.
fn next(func: &mut Func, inst: Inst, area: Area) {
    let Some(result) = func[inst].first_result else { return };
    let Some(&list) = func[func[inst].args].first() else { return };
    let ty = func[result].ty;
    let Some(block) = func.block_of(inst) else { return };
    let span = func.span(inst);
    // A scalar of a width a register holds, which is every type the algorithm below is right about.
    // A `long double` is on the x87 stack, and an `__int128` takes two slots with an alignment rule
    // of its own. Both of those are a second algorithm rather than a wider reading of this one, so
    // both are left alone here and refused by name further down.
    if !ty.is_scalar() || ty.bits() > 64 || !(ty.is_int() || ty.is_float() || ty.is_ptr()) {
        return;
    }
    let float = ty.is_float();
    let Some(last) = area.last(float) else { return };
    let field = if float { FP_OFFSET } else { GP_OFFSET };

    // Everything below the instruction, taken out before anything is built, because the builder
    // appends to a block and this block has to end at the branch.
    let rest: Vec<Inst> = func.insts(block).skip_while(|&at| at != inst).skip(1).collect();
    let taken = func.create_block();
    let overflowed = func.create_block();
    let join = func.create_block();
    let addr = func.append_param(join, Type::PTR);
    func.remove_inst(inst);
    for &at in &rest {
        func.remove_inst(at);
    }

    // The question, in the block the `va_arg` used to be in. Unsigned, because an offset into the
    // save area counts bytes and is never negative, and because what the field holds once the
    // register arguments have all been walked is a number past the end rather than a small one.
    let mut build = Builder::new(func, block).at(span);
    let counter = offset(&mut build, list, field);
    let walked = build.load(Type::int(32), counter, info(4, 4), Flags::default());
    let end = build.iconst(Type::int(32), i128::from(last));
    let inside = build.icmp(IntPred::Ule, walked, end);
    build.br_if(inside, taken, &[], overflowed, &[]);

    // The register path: the argument is in the save area at the offset the field holds, and the
    // field steps on by one slot of its file.
    let mut build = Builder::new(func, taken).at(span);
    let base = offset(&mut build, list, SAVE_AREA);
    let save = build.load(Type::PTR, base, info(8, 8), Flags::default());
    let wide = build.unary(Opcode::ZExt, walked, Type::int(64));
    let found = added(&mut build, save, wide);
    let stride = build.iconst(Type::int(32), i128::from(area.stride(float)));
    let stepped = build.binary(Opcode::Add, walked, stride, Flags::default());
    let counter = offset(&mut build, list, field);
    build.store(stepped, counter, info(4, 4), Flags::default());
    build.jump(join, &[found]);

    // The memory path: the argument is where the caller left it, and the pointer steps on by a
    // word, because the caller's argument area is a run of whole words whatever is in them.
    let mut build = Builder::new(func, overflowed).at(span);
    let pointer = offset(&mut build, list, OVERFLOW);
    let here = build.load(Type::PTR, pointer, info(8, 8), Flags::default());
    let word = build.iconst(Type::int(64), i128::from(area.word));
    let onward = added(&mut build, here, word);
    build.store(onward, pointer, info(8, 8), Flags::default());
    build.jump(join, &[here]);

    // And the load the program actually wrote, over the address the two paths agreed on, with
    // everything that used to follow it behind it in the order it was written.
    let bytes = ty.bits() / 8;
    let mem = func.add_mem(info(u64::from(bytes), bytes));
    let args = func.push_values(&[addr]);
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

/// One `va_object` whose argument is in the caller's memory, as the address of it.
///
/// Over two eightbytes is class MEMORY whatever the members are, and a MEMORY class argument is in
/// the caller's argument area, which is what the overflow pointer of the list points at. So there
/// is no question to ask and no branch to build: the argument is where that pointer says, and the
/// pointer steps on past it. Sixteen bytes and under arrived in registers and is issue #339, and
/// one of those is left alone here and refused by name further down.
///
/// The address is answered rather than a copy of the object, which is what the instruction is for
/// and what gcc does with the same argument. An object in the caller's memory is already somewhere
/// addressable, and the copy the C standard describes is the assignment the caller of `va_arg`
/// wrote, which the front end has already built around this.
///
/// The pointer is rounded up first for an object that wants more alignment than a word. The
/// argument area is a run of words, so anything asking for eight or less is where it is already,
/// and anything asking for more was put at the next multiple of what it asked for by whoever
/// passed it.
fn object(func: &mut Func, inst: Inst, area: Area) {
    let Extra::Mem(mem) = func[inst].extra else { return };
    let MemInfo { size, align, .. } = func[mem];
    let Some(&list) = func[func[inst].args].first() else { return };
    if size <= 16 || func[inst].first_result.is_none() {
        return;
    }
    let word = u64::from(area.word);
    let wide = Type::int(64);

    let pointer = field(func, inst, list, OVERFLOW);
    let args = func.push_values(&[pointer]);
    let read = func.add_mem(info(word, area.word));
    let data = InstData { args, extra: Extra::Mem(read), ..InstData::new(Opcode::Load) };
    let here = ahead(func, inst, data, Type::PTR);

    // As an integer, because rounding up is an add and a mask and neither is a thing to do to a
    // pointer. Both casts are free: the two are the same bits on this machine and nothing is
    // written for either.
    let args = func.push_values(&[here]);
    let mut at = ahead(func, inst, InstData { args, ..InstData::new(Opcode::PtrToInt) }, wide);
    if u64::from(align) > word {
        // Up to the next multiple of a power of two, which is the round up every alignment is.
        // The mask is the negative of the alignment because that is what the complement of one
        // less than it comes to, and writing it that way keeps it inside a signed sixty four bit
        // constant.
        let bump = constant(func, inst, i128::from(align) - 1, wide);
        at = binary(func, inst, Opcode::Add, at, bump, wide);
        let mask = constant(func, inst, -i128::from(align), wide);
        at = binary(func, inst, Opcode::And, at, mask, wide);
    }

    // Past it, rounded up to a whole number of words, because the argument area holds words and
    // the argument behind this one starts at one of them.
    let step = i128::from(size.next_multiple_of(word));
    let by = constant(func, inst, step, wide);
    let onward = binary(func, inst, Opcode::Add, at, by, wide);
    let args = func.push_values(&[onward]);
    let data = InstData { args, ..InstData::new(Opcode::IntToPtr) };
    let onward = ahead(func, inst, data, Type::PTR);
    let args = func.push_values(&[onward, pointer]);
    let written = func.add_mem(info(word, area.word));
    let data = InstData { args, extra: Extra::Mem(written), ..InstData::new(Opcode::Store) };
    let span = func.span(inst);
    let made = func.create_inst(data, &[], span);
    func.insert_before(made, inst);

    // And the instruction itself is the address, so that everything reading it goes on reading the
    // value it already read and nothing has to be substituted anywhere.
    let args = func.push_values(&[at]);
    let data = &mut func[inst];
    data.opcode = Opcode::IntToPtr;
    data.args = args;
    data.extra = Extra::None;
    data.flags = data.flags.intersection(Flags::legal_on(Opcode::IntToPtr));
}

/// A constant in front of an instruction.
fn constant(func: &mut Func, inst: Inst, value: i128, ty: Type) -> Value {
    let extra = Extra::Imm(func.add_imm(Imm::int(value, ty)));
    ahead(func, inst, InstData { extra, ..InstData::new(Opcode::IConst) }, ty)
}

/// Two values through an arithmetic instruction, in front of another one.
fn binary(func: &mut Func, inst: Inst, opcode: Opcode, lhs: Value, rhs: Value, ty: Type) -> Value {
    let args = func.push_values(&[lhs, rhs]);
    ahead(func, inst, InstData { args, ..InstData::new(opcode) }, ty)
}

/// One `va_copy`, as the fields of one list moved into another.
///
/// A list is those fields and holds nothing anywhere else, so copying it is copying them, and three
/// words move as three words rather than as a call to `memcpy`, which is a name this compiler
/// cannot emit yet and would be the wrong answer for three words in any case.
///
/// Every read is built before any write, so that a list copied onto itself, which is legal and
/// useless, moves what it held rather than what it has just been given.
fn copy(func: &mut Func, inst: Inst) {
    let [into, from] = func[func[inst].args] else { return };
    let mut moved = Vec::new();
    for word in 0..SIZE / 8 {
        let step = i64::try_from(word * 8).unwrap_or(0);
        let there = field(func, inst, from, step);
        let mem = func.add_mem(info(8, 8));
        let args = func.push_values(&[there]);
        let data = InstData { args, extra: Extra::Mem(mem), ..InstData::new(Opcode::Load) };
        moved.push((ahead(func, inst, data, Type::int(64)), step));
    }
    for (read, step) in moved {
        let here = field(func, inst, into, step);
        let mem = func.add_mem(info(8, 8));
        let args = func.push_values(&[read, here]);
        let data = InstData { args, extra: Extra::Mem(mem), ..InstData::new(Opcode::Store) };
        let span = func.span(inst);
        let made = func.create_inst(data, &[], span);
        func.insert_before(made, inst);
    }
    func.remove_inst(inst);
}

/// The address of a field of a list, written in front of an instruction, or the list itself for the
/// field at the front of it.
fn field(func: &mut Func, inst: Inst, list: Value, at: i64) -> Value {
    if at == 0 {
        return list;
    }
    let extra = Extra::Imm(func.add_imm(Imm::int(i128::from(at), Type::int(64))));
    let step =
        ahead(func, inst, InstData { extra, ..InstData::new(Opcode::IConst) }, Type::int(64));
    let args = func.push_values(&[list, step]);
    ahead(func, inst, InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR)
}

/// Puts an instruction in front of another one and gives back the value it produces.
fn ahead(func: &mut Func, inst: Inst, data: InstData, ty: Type) -> Value {
    let span = func.span(inst);
    let made = func.create_inst(data, &[ty], span);
    func.insert_before(made, inst);
    func[made].first_result.expect("an instruction created with one result has one")
}

/// The address of a field of a list in a block being filled, or the list itself for the field at
/// the front of it.
fn offset(build: &mut Builder<'_>, list: Value, at: i64) -> Value {
    if at == 0 {
        return list;
    }
    let step = build.iconst(Type::int(64), i128::from(at));
    added(build, list, step)
}

/// A pointer with an integer added to it.
fn added(build: &mut Builder<'_>, pointer: Value, by: Value) -> Value {
    let args = build.func().push_values(&[pointer, by]);
    build.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR)
}

/// An ordinary read or write of that many bytes, aligned that far.
///
/// Every access this pass makes is to a field of a list or to an argument, and none of them is
/// atomic or has anything to say about aliasing.
fn info(size: u64, align: u32) -> MemInfo {
    MemInfo { size, align, order: MemOrder::NotAtomic, tbaa: None }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{Builder, Extra, Func, InstData, Module, Opcode, Signature, Type};
    use rucc_target::x86_64::{SYSV, WIN64};
    use rucc_target::{Arch, Env, Os, TargetInfo, Triple};

    use super::{Area, FP_OFFSET, GP_OFFSET, OVERFLOW, SAVE_AREA, SIZE, VECTOR_SLOT, lists};

    fn target() -> TargetInfo {
        TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu))
    }

    /// `T f(va_list *ap) { return va_arg(*ap, T); }`, or the same shape over whichever of the
    /// family is asked for, with the list arriving as the pointer it has decayed to by the time
    /// anything reads it.
    fn built(opcode: Opcode, ty: Type, lists: usize) -> (Interner, Func) {
        let mut names = Interner::new();
        let params = vec![Type::PTR; lists];
        let mut signature = Signature::new().with_params(&params);
        if !ty.is_void() {
            signature = signature.with_returns(&[ty]);
        }
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let args: Vec<_> = params.iter().map(|&ty| func.append_param(entry, ty)).collect();

        let mut build = Builder::new(&mut func, entry);
        let list = build.func().push_values(&args);
        if ty.is_void() {
            build.inst(InstData { args: list, ..InstData::new(opcode) }, &[]);
            build.ret(&[]);
        } else {
            let got = build.value(InstData { args: list, ..InstData::new(opcode) }, ty);
            build.ret(&[got]);
        }
        (names, func)
    }

    fn printed(func: &Func, names: &mut Interner) -> String {
        let module = Module::new(names.intern("va.c"), &target());
        rucc_ir::print_func(&module, func, names)
    }

    fn valid(func: &Func, names: &mut Interner) {
        let module = Module::new(names.intern("va.c"), &target());
        rucc_ir::verify_func(&module, func, names).expect("the rewrite builds valid IR");
    }

    /// The numbers in this test are the psABI's own, written out rather than computed, because the
    /// whole point of the layout is that it is the document's and not a convenient one. A version
    /// of [`Area`] that worked them out differently would agree with itself and disagree with the C
    /// library, and this is what would notice.
    #[test]
    fn the_save_area_is_the_one_the_document_describes() {
        let area = Area::of(&SYSV);
        assert_eq!(area.floats_at, 48, "six general purpose registers of eight bytes");
        assert_eq!(area.size, 176, "and eight vector ones of sixteen");
        assert_eq!(area.stride(false), 8);
        assert_eq!(area.stride(true), VECTOR_SLOT);
        assert_eq!(area.starts_at(false), 0);
        assert_eq!(area.starts_at(true), 48);
        // The last slot's own offset and not the end of the area, which is what `va_arg` compares
        // against: an offset equal to the end is one slot past the last argument.
        assert_eq!(area.last(false), Some(40));
        assert_eq!(area.last(true), Some(160));
    }

    /// And the four fields, for the same reason.
    #[test]
    fn a_list_is_the_four_fields_the_document_describes() {
        assert_eq!((GP_OFFSET, FP_OFFSET, OVERFLOW, SAVE_AREA), (0, 4, 8, 16));
        assert_eq!(SIZE, 24);
    }

    #[test]
    fn a_va_arg_becomes_the_branch_on_whether_the_argument_is_still_in_the_save_area() {
        let (mut names, mut func) = built(Opcode::VaArg, Type::int(32), 1);
        let before = func.blocks().count();
        lists(&mut func, &SYSV);
        assert_eq!(func.blocks().count(), before + 3, "one for each path and one they meet at");

        let text = printed(&func, &mut names);
        assert!(!text.contains("va_arg"), "the va_arg is gone: {text}");
        assert!(text.contains("icmp ule"), "the threshold is a comparison: {text}");
        assert!(text.contains("br_if"), "and it is branched on: {text}");
        valid(&func, &mut names);
    }

    /// Which field it walks is the whole of the difference between the two files, and getting it
    /// backwards is a program that reads its integers out of the vector half.
    #[test]
    fn which_half_of_the_area_is_walked_is_the_type_s_answer() {
        for (ty, last, stride) in
            [(Type::int(64), 40, 8), (Type::float(rucc_ir::Float::F64), 160, 16)]
        {
            let (mut names, mut func) = built(Opcode::VaArg, ty, 1);
            lists(&mut func, &SYSV);
            let text = printed(&func, &mut names);
            assert!(text.contains(&format!("iconst.i32 {last}")), "{ty:?} stops at {last}: {text}");
            assert!(text.contains(&format!("iconst.i32 {stride}")), "and steps by it: {text}");
        }
    }

    /// The value the rest of the function reads has to stay the value it already read, since the
    /// rewrite substitutes nothing anywhere. It stays it by the `va_arg` becoming the load rather
    /// than being replaced by one, so the instruction is the same instruction under a new opcode
    /// and in a new block.
    #[test]
    fn what_reads_the_argument_reads_the_same_value_it_did_before() {
        let (mut names, mut func) = built(Opcode::VaArg, Type::int(32), 1);
        let entry = func.entry().expect("an entry block");
        let inst = func.insts(entry).next().expect("the va_arg is first");
        let read = func[inst].first_result.expect("it produces the argument");

        lists(&mut func, &SYSV);
        assert_eq!(func[inst].opcode, Opcode::Load, "the same instruction, lowered");
        assert_eq!(func[inst].first_result, Some(read), "producing the same value");
        assert_ne!(func.block_of(inst), Some(entry), "in the block the two paths meet at");
        valid(&func, &mut names);
    }

    #[test]
    fn a_va_end_is_nothing_at_all() {
        let (mut names, mut func) = built(Opcode::VaEnd, Type::VOID, 1);
        lists(&mut func, &SYSV);
        let text = printed(&func, &mut names);
        assert!(!text.contains("va_end"), "{text}");
        assert_eq!(func.blocks().count(), 1, "and needs no block: {text}");
        valid(&func, &mut names);
    }

    /// Three words and no branch, because a list is three words and holds nothing anywhere else.
    #[test]
    fn a_va_copy_is_the_list_moved_a_word_at_a_time() {
        let (mut names, mut func) = built(Opcode::VaCopy, Type::VOID, 2);
        lists(&mut func, &SYSV);
        let text = printed(&func, &mut names);
        assert!(!text.contains("va_copy"), "{text}");
        assert_eq!(text.matches("load.i64").count(), 3, "{text}");
        assert_eq!(text.matches("store").count(), 3, "{text}");
        assert_eq!(func.blocks().count(), 1, "and needs no block: {text}");
        valid(&func, &mut names);
    }

    /// Every read before every write, so that `va_copy(ap, ap)` moves what the list held rather
    /// than what it has just been given. Useless and legal, which is exactly the combination that
    /// gets written once and never tested anywhere else.
    #[test]
    fn a_list_copied_onto_itself_moves_what_it_held() {
        let (mut names, mut func) = built(Opcode::VaCopy, Type::VOID, 1);
        // One parameter, so both operands of the copy are the same list. The builder above pushes
        // as many operands as there are parameters, so the second is added here.
        let entry = func.entry().expect("an entry block");
        let inst = func.insts(entry).next().expect("the copy is first");
        let list = func[func[inst].args][0];
        let args = func.push_values(&[list, list]);
        func[inst].args = args;

        lists(&mut func, &SYSV);
        let text = printed(&func, &mut names);
        let first = text.find("store").expect("a write");
        let last = text.rfind("load.i64").expect("a read");
        assert!(last < first, "every read is above every write: {text}");
        valid(&func, &mut names);
    }

    /// `struct s f(va_list *ap) { return va_arg(*ap, struct s); }`, where the structure is that
    /// many bytes wanting that much alignment. The object form of the instruction rather than the
    /// value one, because an aggregate is not a value and answers where it is instead.
    fn object(size: u64, align: u32) -> (Interner, Func) {
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::PTR]).with_returns(&[Type::PTR]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let list = func.append_param(entry, Type::PTR);
        let mem = func.add_mem(super::info(size, align));
        let mut build = Builder::new(&mut func, entry);
        let args = build.func().push_values(&[list]);
        let data = InstData { args, extra: Extra::Mem(mem), ..InstData::new(Opcode::VaObject) };
        let got = build.value(data, Type::PTR);
        build.ret(&[got]);
        (names, func)
    }

    /// Over two eightbytes is class MEMORY whatever the members are, so there is one place it can
    /// be and no question to ask about which.
    #[test]
    fn an_object_too_big_for_the_registers_is_read_out_of_the_caller_s_memory() {
        let (mut names, mut func) = object(24, 8);
        lists(&mut func, &SYSV);
        let text = printed(&func, &mut names);
        assert!(!text.contains("va_object"), "{text}");
        assert_eq!(func.blocks().count(), 1, "no branch, so no new block: {text}");
        assert!(text.contains("iconst.i64 8"), "the overflow field is at eight: {text}");
        assert!(text.contains("iconst.i64 24"), "and the pointer steps past the object: {text}");
        assert!(!text.contains("gp_offset"), "{text}");
        valid(&func, &mut names);
    }

    /// The size the pointer steps on by is the size rounded up to a word, because the argument
    /// area holds words and the argument behind this one starts at one of them.
    #[test]
    fn a_size_that_is_not_a_whole_number_of_words_steps_on_by_the_next_one() {
        let (mut names, mut func) = object(28, 4);
        lists(&mut func, &SYSV);
        let text = printed(&func, &mut names);
        assert!(text.contains("iconst.i64 32"), "twenty eight bytes step on by thirty two: {text}");
        valid(&func, &mut names);
    }

    /// An object wanting more than a word is at the next multiple of what it wants, and one
    /// wanting a word or less is where the pointer already is, since the area is a run of words.
    #[test]
    fn an_object_wanting_more_alignment_than_a_word_is_rounded_up_to_it() {
        let (mut names, mut func) = object(32, 16);
        lists(&mut func, &SYSV);
        let text = printed(&func, &mut names);
        assert!(text.contains("iconst.i64 15"), "up to the next sixteen: {text}");
        assert!(text.contains("iconst.i64 -16"), "and down to a multiple of it: {text}");
        assert!(text.contains(" = and "), "which is an add and a mask: {text}");
        valid(&func, &mut names);

        let (mut names, mut func) = object(24, 8);
        lists(&mut func, &SYSV);
        assert!(!printed(&func, &mut names).contains(" = and "), "a word wants no rounding");
    }

    /// Sixteen bytes and under arrived in registers, so it is in the save area rather than in the
    /// caller's memory and the walk above is not right about it. Issue #339 is that one, and until
    /// it lands the instruction is left alone and the function is refused by name further down.
    #[test]
    fn an_object_small_enough_to_have_arrived_in_registers_is_left_alone() {
        for size in [1, 8, 9, 16] {
            let (mut names, mut func) = object(size, 8);
            let before = printed(&func, &mut names);
            lists(&mut func, &SYSV);
            assert_eq!(printed(&func, &mut names), before, "{size} bytes");
        }
    }

    /// What reads the object goes on reading the value it already read, the same way it does for a
    /// value, and for the same reason: the instruction becomes the address rather than being
    /// replaced by one, so nothing has to be substituted anywhere.
    #[test]
    fn what_reads_the_object_reads_the_same_value_it_did_before() {
        let (mut names, mut func) = object(24, 8);
        let entry = func.entry().expect("an entry block");
        let inst = func.insts(entry).next().expect("the va_object is first");
        let read = func[inst].first_result.expect("it answers an address");

        lists(&mut func, &SYSV);
        assert_eq!(func[inst].opcode, Opcode::IntToPtr, "the same instruction, lowered");
        assert_eq!(func[inst].first_result, Some(read), "producing the same value");
        valid(&func, &mut names);
    }

    /// Windows has a list of a different shape and an algorithm to match, and none of the rewrites
    /// here is right for any of it. Leaving it alone is what makes the function refused by name
    /// further down rather than compiled into a walk over an area that was never filled in.
    #[test]
    fn a_convention_whose_list_is_not_this_one_is_left_alone() {
        let (mut names, mut func) = built(Opcode::VaArg, Type::int(32), 1);
        let before = printed(&func, &mut names);
        lists(&mut func, &WIN64);
        assert_eq!(printed(&func, &mut names), before);
    }

    /// A width the algorithm is not right about is left alone for the same reason, and the two
    /// here are the ones a program actually writes: a `long double` is on a register file this has
    /// nothing to say about, and an `__int128` takes two slots under an alignment rule of its own.
    #[test]
    fn a_type_that_does_not_travel_in_one_slot_is_left_alone() {
        for ty in [Type::float(rucc_ir::Float::F80), Type::int(128)] {
            let (mut names, mut func) = built(Opcode::VaArg, ty, 1);
            let before = printed(&func, &mut names);
            lists(&mut func, &SYSV);
            assert_eq!(printed(&func, &mut names), before, "{ty:?}");
        }
    }

    /// Nothing else is touched, which matters because this runs over every function whether or not
    /// one reads a variable argument.
    #[test]
    fn a_function_with_no_list_in_it_is_left_exactly_as_it_was() {
        let mut names = Interner::new();
        let int = Type::int(32);
        let mut func =
            Func::new(names.intern("f"), Signature::new().with_params(&[int]).with_returns(&[int]));
        let entry = func.create_block();
        let x = func.append_param(entry, int);
        Builder::new(&mut func, entry).ret(&[x]);

        let before = printed(&func, &mut names);
        lists(&mut func, &SYSV);
        assert_eq!(printed(&func, &mut names), before);
    }
}
