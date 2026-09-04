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
//! in registers, and then the question is the one a scalar asks, with two differences. The object
//! takes a register of each file for each of its eightbytes, so the room in the save area has to be
//! there for all of them at once and the offsets step on by all of them at once. And the halves of
//! it in the save area are not next to each other, so the answer cannot be an address in the area:
//! the eightbytes are copied out into a buffer of the function's own and the answer is that.
//!
//! Which file each eightbyte came from is the classification, which is an answer about a C type and
//! not one the size and the alignment give. It arrives on the instruction, worked out by the front
//! end, which is the last thing to hold a type. An object with no slots on it is one the
//! classification sent to the argument area, and that is what tells the two halves below apart.
//!
//! `va_start` is the other way round. Three of the four fields it writes are distances into a frame
//! that does not exist yet, so it stays an instruction as far as [`crate::lower`], which builds it
//! out of the frame the way it builds an `alloca`. The spill that fills the save area is written
//! there for the same reason.

use rucc_ir::{
    Block, Builder, Extra, Flags, Func, Imm, Inst, InstData, IntPred, MemInfo, MemOrder, Opcode,
    Type, Value,
};
use rucc_target::{CallRegs, Slot};

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

    /// Where a file's slots end, which is where the vector half begins for the general purpose
    /// file and the end of the whole area for the vector one.
    ///
    /// This is what an object taking more than one register of a file is measured against: the
    /// psABI asks whether the offset is at or below the end less a slot for each register the
    /// object wants, and one register of it is the same question [`Area::last`] asks.
    #[must_use]
    pub fn ends_at(self, float: bool) -> u32 {
        if float { self.size } else { self.floats_at }
    }

    /// How many registers of a file the area holds.
    #[must_use]
    fn holds(self, float: bool) -> u32 {
        if float { self.counts.1 } else { self.counts.0 }
    }

    /// The offset of a file's last slot, which is the threshold `va_arg` compares against.
    ///
    /// The last slot's own offset and not the end of the area, because an offset equal to the end
    /// is one slot past the last argument while an offset a slot below the end is the last argument
    /// itself. An empty file has no such offset and nothing here has one.
    #[must_use]
    pub fn last(self, float: bool) -> Option<u32> {
        let last = self.holds(float).checked_sub(1)?;
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

/// One `va_object`, as the address the object can be read from.
///
/// Two shapes, and the slots on the instruction are what say which. An object with none is one the
/// classification sent to the caller's argument area, which is what everything over two eightbytes
/// is whatever its members are. There is no question to ask about that one: the overflow pointer
/// says where it is and steps on past it, and no block is needed.
///
/// An object with slots arrived in registers, and that is the branch [`next`] builds for a scalar
/// with the object's own two differences: the room has to be there for every one of its slots at
/// once, and what is answered is a buffer the slots were copied into rather than an address in the
/// save area, because two eightbytes of one object are not next to each other in there.
///
/// The address is answered rather than a copy of the object, which is what the instruction is for
/// and what gcc does with the same argument. An object in the caller's memory is already somewhere
/// addressable, and the copy the C standard describes is the assignment the caller of `va_arg`
/// wrote, which the front end has already built around this.
fn object(func: &mut Func, inst: Inst, area: Area) {
    let Extra::VaObject(at) = func[inst].extra else { return };
    let object = func[at];
    let MemInfo { size, align, .. } = func[object.mem];
    let slots: Vec<Slot> = func[object.slots].to_vec();
    let Some(&list) = func[func[inst].args].first() else { return };
    let Some(block) = func.block_of(inst) else { return };
    if func[inst].first_result.is_none() || !fits(&slots, area) {
        return;
    }
    let span = func.span(inst);

    // The buffer the register form copies into, made before anything else, because an alloca of
    // a fixed size belongs in the entry block and the walk below is built where the instruction
    // is. It is as big as the slots reach rather than as big as the object, which is more for an
    // object whose last eightbyte is a part of one: five bytes travel in a whole register and
    // come out of the area as a whole register, so the buffer has eight bytes for them to land
    // in and the three past the object are never read.
    let reach = slots.iter().map(|&slot| slot.offset() + width(slot)).max().unwrap_or(0);
    let room = buffer(func, inst, reach.max(size), align);

    // Everything below the instruction, taken out before anything is built, because a builder
    // appends to a block and the register form ends this one at a branch.
    let rest: Vec<Inst> = func.insts(block).skip_while(|&at| at != inst).skip(1).collect();
    func.remove_inst(inst);
    for &at in &rest {
        func.remove_inst(at);
    }

    let (ends, address) = match room {
        Some(room) if !slots.is_empty() => {
            registers(func, block, inst, Read { list, area, slots: &slots, size, align, room })
        }
        _ => {
            let mut build = Builder::new(func, block).at(span);
            (block, overflow(&mut build, list, area, size, align))
        }
    };

    // And the instruction itself is that address, so that everything reading it goes on reading
    // the value it already read and nothing has to be substituted anywhere.
    let args = func.push_values(&[address]);
    let data = &mut func[inst];
    data.opcode = Opcode::IntToPtr;
    data.args = args;
    data.extra = Extra::None;
    data.flags = data.flags.intersection(Flags::legal_on(Opcode::IntToPtr));
    func.append_inst(ends, inst);
    for at in rest {
        func.append_inst(ends, at);
    }
}

/// One object read off one list, which is what both halves of the walk are about.
#[derive(Clone, Copy)]
struct Read<'a> {
    /// The list it is read from.
    list: Value,
    /// The save area of the function doing the reading.
    area: Area,
    /// Which register each of the object's eightbytes arrived in, and empty for an object that
    /// arrived in the caller's memory.
    slots: &'a [Slot],
    /// How many bytes the object is.
    size: u64,
    /// What it is aligned to.
    align: u32,
    /// The buffer of the function's own the register form copies the object into.
    room: Value,
}

/// Whether the classification is one this knows how to read out of the save area.
///
/// A slot wider than a register or more of them than the area holds is a classification from some
/// other machine or from a rule this has not been taught. Turning it down here leaves the
/// instruction alone, and an instruction left alone is refused by name further down, which is a
/// message about `va_arg` rather than whatever a half built walk would do at run time.
fn fits(slots: &[Slot], area: Area) -> bool {
    let mut counts = [0, 0];
    for &slot in slots {
        if width(slot) > u64::from(area.word) {
            return false;
        }
        counts[usize::from(is_float(slot))] += 1;
    }
    counts[0] <= area.holds(false) && counts[1] <= area.holds(true)
}

/// The register form: the room in the save area is asked about once per file, and the object is
/// copied out of the area into a buffer when it is there and read from the caller's memory when it
/// is not.
///
/// The question is asked once per file the object takes a register of, and both have to say yes,
/// because the psABI puts the whole object in the caller's memory when there is not room in the
/// area for all of it. A file the object takes nothing of has room by definition and is not asked
/// about, which is every object of one class and is most of them.
///
/// Gives back the block the walk ends in and the address, as an integer, that the two paths agreed
/// on.
fn registers(func: &mut Func, block: Block, inst: Inst, read: Read<'_>) -> (Block, Value) {
    let span = func.span(inst);
    let area = read.area;
    let counts = [taken_of(read.slots, false), taken_of(read.slots, true)];
    let saved = func.create_block();
    let overflowed = func.create_block();
    let join = func.create_block();
    let address = func.append_param(join, Type::int(64));

    // The questions, each in its own block, because two of them are two branches and the second
    // is only asked when the first said yes.
    let asked: Vec<bool> =
        [false, true].into_iter().filter(|&float| counts[usize::from(float)] > 0).collect();
    let mut at = block;
    for (index, &float) in asked.iter().enumerate() {
        let next = if index + 1 == asked.len() { saved } else { func.create_block() };
        // The psABI's own threshold: the end of the file's half of the area, less a slot for each
        // register the object wants, so that an offset at it leaves room for all of them.
        let room =
            area.ends_at(float).saturating_sub(area.stride(float) * counts[usize::from(float)]);
        let mut build = Builder::new(func, at).at(span);
        let counter = offset(&mut build, read.list, field_of(float));
        let walked = build.load(Type::int(32), counter, info(4, 4), Flags::default());
        let end = build.iconst(Type::int(32), i128::from(room));
        let inside = build.icmp(IntPred::Ule, walked, end);
        build.br_if(inside, next, &[], overflowed, &[]);
        at = next;
    }

    let mut build = Builder::new(func, saved).at(span);
    let found = copied(&mut build, read, counts);
    build.jump(join, &[found]);

    let mut build = Builder::new(func, overflowed).at(span);
    let here = overflow(&mut build, read.list, area, read.size, read.align);
    build.jump(join, &[here]);

    (join, address)
}

/// The object copied out of the save area into the buffer, as the address of the buffer.
///
/// A buffer and not an address in the area because the eightbytes of one object are not next to
/// each other in there: two integer eightbytes are eight bytes apart and two vector ones are
/// sixteen, and an object of one of each has them in different halves of the area entirely. So
/// there is nowhere in the area the object is, and the one place it can be made to be is somewhere
/// else.
fn copied(build: &mut Builder<'_>, read: Read<'_>, counts: [u32; 2]) -> Value {
    let area = read.area;
    let base = offset(build, read.list, SAVE_AREA);
    let save = build.load(Type::PTR, base, info(8, 8), Flags::default());

    // Where each file's next slot is, which is the one thing the offsets in the list say, and the
    // counter itself, which is what steps on by every slot the object took of that file.
    let mut walked = [None, None];
    let mut nexts = [None, None];
    for float in [false, true] {
        let file = usize::from(float);
        if counts[file] == 0 {
            continue;
        }
        let counter = offset(build, read.list, field_of(float));
        let read = build.load(Type::int(32), counter, info(4, 4), Flags::default());
        let wide = build.unary(Opcode::ZExt, read, Type::int(64));
        walked[file] = Some(read);
        nexts[file] = Some(added(build, save, wide));
    }

    let mut seen = [0, 0];
    for &slot in read.slots {
        let float = is_float(slot);
        let file = usize::from(float);
        let Some(from) = nexts[file] else { continue };
        let step = i64::from(area.stride(float) * seen[file]);
        seen[file] += 1;
        // As an integer of the slot's width whatever the file it came from, because what this is
        // is a copy of the object's bytes and nothing here reads them as anything.
        let bytes = width(slot);
        let ty = Type::int(u32::try_from(bytes).unwrap_or(1) * 8);
        let at = offset(build, from, step);
        let value =
            build.load(ty, at, info(bytes, area.stride(float).min(area.word)), Flags::default());
        let into = offset(build, read.room, i64::try_from(slot.offset()).unwrap_or(0));
        let holds = info(bytes, part(read.align, slot.offset()));
        build.store(value, into, holds, Flags::default());
    }

    // And the counters step on by every slot the object took, since the whole of it came out of
    // the area and the argument behind it starts past all of it.
    for float in [false, true] {
        let file = usize::from(float);
        let Some(counter) = walked[file] else { continue };
        let by = build.iconst(Type::int(32), i128::from(area.stride(float) * counts[file]));
        let stepped = build.binary(Opcode::Add, counter, by, Flags::default());
        let at = offset(build, read.list, field_of(float));
        build.store(stepped, at, info(4, 4), Flags::default());
    }
    build.unary(Opcode::PtrToInt, read.room, Type::int(64))
}

/// A buffer at the front of the entry block, which is where an alloca of a fixed size belongs.
///
/// Not where the walk is, because a walk inside a loop would then be an alloca inside a loop,
/// which is a frame that grows every time round. One buffer per `va_arg` of an object, made once
/// and written every time the object is read, which is what the front end would have written if
/// the temporary had a name.
fn buffer(func: &mut Func, inst: Inst, size: u64, align: u32) -> Option<Value> {
    let entry = func.entry()?;
    let span = func.span(inst);
    let mem = func.add_mem(info(size, align.max(1)));
    let data = InstData { extra: Extra::Mem(mem), ..InstData::new(Opcode::Alloca) };
    let made = func.create_inst(data, &[Type::PTR], span);
    let first = func.insts(entry).next();
    match first {
        Some(first) => func.insert_before(made, first),
        None => func.append_inst(entry, made),
    }
    func[made].first_result
}

/// Where the argument the caller left in memory is, with the overflow pointer stepped on past it,
/// as an integer address.
///
/// The pointer is rounded up first for an object that wants more alignment than a word. The
/// argument area is a run of words, so anything asking for eight or less is where it is already,
/// and anything asking for more was put at the next multiple of what it asked for by whoever
/// passed it.
fn overflow(build: &mut Builder<'_>, list: Value, area: Area, size: u64, align: u32) -> Value {
    let word = u64::from(area.word);
    let wide = Type::int(64);
    let pointer = offset(build, list, OVERFLOW);
    let here = build.load(Type::PTR, pointer, info(word, area.word), Flags::default());

    // As an integer, because rounding up is an add and a mask and neither is a thing to do to a
    // pointer. Both casts are free: the two are the same bits on this machine and nothing is
    // written for either.
    let mut at = build.unary(Opcode::PtrToInt, here, wide);
    if u64::from(align) > word {
        // Up to the next multiple of a power of two, which is the round up every alignment is.
        // The mask is the negative of the alignment because that is what the complement of one
        // less than it comes to, and writing it that way keeps it inside a signed sixty four bit
        // constant.
        let bump = build.iconst(wide, i128::from(align) - 1);
        at = build.binary(Opcode::Add, at, bump, Flags::default());
        let mask = build.iconst(wide, -i128::from(align));
        at = build.binary(Opcode::And, at, mask, Flags::default());
    }

    // Past it, rounded up to a whole number of words, because the argument area holds words and
    // the argument behind this one starts at one of them.
    let by = build.iconst(wide, i128::from(size.next_multiple_of(word)));
    let onward = build.binary(Opcode::Add, at, by, Flags::default());
    let onward = build.unary(Opcode::IntToPtr, onward, Type::PTR);
    build.store(onward, pointer, info(word, area.word), Flags::default());
    at
}

/// Which of the two counters a file's slots are walked with.
fn field_of(float: bool) -> i64 {
    if float { FP_OFFSET } else { GP_OFFSET }
}

/// Whether a slot is one of the vector file's.
fn is_float(slot: Slot) -> bool {
    matches!(slot, Slot::Float { .. })
}

/// How many registers of a file an object takes.
fn taken_of(slots: &[Slot], float: bool) -> u32 {
    u32::try_from(slots.iter().filter(|&&slot| is_float(slot) == float).count()).unwrap_or(0)
}

/// How many bytes one slot moves, which is its own width rounded up to one the machine has a load
/// for.
fn width(slot: Slot) -> u64 {
    match slot {
        Slot::Integer { size, .. } => u64::from(size.next_power_of_two().clamp(1, 8)),
        Slot::Float { format, .. } => u64::from(format.width()).div_ceil(8),
    }
}

/// What a part of an object at that offset is aligned to, which is what the object is aligned to
/// for the part at the front of it and how far into the object the part sits for every other.
fn part(align: u32, offset: u64) -> u32 {
    let align = align.max(1);
    if offset == 0 {
        return align;
    }
    u32::try_from(1_u64 << offset.trailing_zeros()).unwrap_or(align).min(align)
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
    use rucc_base::float::Format;
    use rucc_ir::{Builder, Extra, Func, InstData, Module, Opcode, Signature, Type, VaInfo};
    use rucc_target::x86_64::{SYSV, WIN64};
    use rucc_target::{Arch, Env, Os, Slot, TargetInfo, Triple};

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
    /// many bytes wanting that much alignment and arrived in those registers. The object form of
    /// the instruction rather than the value one, because an aggregate is not a value and answers
    /// where it is instead.
    ///
    /// No slots is the object the classification sent to the caller's argument area, which is what
    /// everything over two eightbytes is.
    fn object(size: u64, align: u32, slots: &[Slot]) -> (Interner, Func) {
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::PTR]).with_returns(&[Type::PTR]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let list = func.append_param(entry, Type::PTR);
        let mem = func.add_mem(super::info(size, align));
        let slots = func.push_slots(slots);
        let at = func.add_va_object(VaInfo { mem, slots });
        let mut build = Builder::new(&mut func, entry);
        let args = build.func().push_values(&[list]);
        let data = InstData { args, extra: Extra::VaObject(at), ..InstData::new(Opcode::VaObject) };
        let got = build.value(data, Type::PTR);
        build.ret(&[got]);
        (names, func)
    }

    /// One eightbyte of an object in the general purpose file, at that offset.
    fn gpr(offset: u64, size: u32) -> Slot {
        Slot::Integer { offset, size }
    }

    /// One in the vector file, holding a `double`, which is what a whole eightbyte of floating
    /// point data is read as whichever way the members divide it up.
    fn sse(offset: u64) -> Slot {
        Slot::Float { offset, format: Format::Double }
    }

    /// Over two eightbytes is class MEMORY whatever the members are, so there is one place it can
    /// be and no question to ask about which.
    #[test]
    fn an_object_too_big_for_the_registers_is_read_out_of_the_caller_s_memory() {
        let (mut names, mut func) = object(24, 8, &[]);
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
        let (mut names, mut func) = object(28, 4, &[]);
        lists(&mut func, &SYSV);
        let text = printed(&func, &mut names);
        assert!(text.contains("iconst.i64 32"), "twenty eight bytes step on by thirty two: {text}");
        valid(&func, &mut names);
    }

    /// An object wanting more than a word is at the next multiple of what it wants, and one
    /// wanting a word or less is where the pointer already is, since the area is a run of words.
    #[test]
    fn an_object_wanting_more_alignment_than_a_word_is_rounded_up_to_it() {
        let (mut names, mut func) = object(32, 16, &[]);
        lists(&mut func, &SYSV);
        let text = printed(&func, &mut names);
        assert!(text.contains("iconst.i64 15"), "up to the next sixteen: {text}");
        assert!(text.contains("iconst.i64 -16"), "and down to a multiple of it: {text}");
        assert!(text.contains(" = and "), "which is an add and a mask: {text}");
        valid(&func, &mut names);

        let (mut names, mut func) = object(24, 8, &[]);
        lists(&mut func, &SYSV);
        assert!(!printed(&func, &mut names).contains(" = and "), "a word wants no rounding");
    }

    /// An object that arrived in registers is in the save area, and reading it is the branch a
    /// scalar asks with the object's own threshold: two eightbytes want two slots, so an offset
    /// that leaves room for one is not room enough.
    #[test]
    fn an_object_that_arrived_in_registers_is_copied_out_of_the_save_area() {
        let (mut names, mut func) = object(16, 8, &[gpr(0, 8), gpr(8, 8)]);
        let before = func.blocks().count();
        lists(&mut func, &SYSV);
        assert_eq!(func.blocks().count(), before + 3, "one for each path and one they meet at");

        let text = printed(&func, &mut names);
        assert!(!text.contains("va_object"), "{text}");
        assert!(text.contains("iconst.i32 32"), "forty eight less two slots: {text}");
        assert!(text.contains("icmp ule"), "which is the threshold: {text}");
        assert!(text.contains("alloca, size 16"), "the object lands in a buffer: {text}");
        assert!(text.contains("iconst.i32 16"), "and the counter steps by both slots: {text}");
        valid(&func, &mut names);
    }

    /// An object of one eightbyte of each file has to have room in both halves of the area, and
    /// the psABI puts the whole of it in the caller's memory when either of them is out. So there
    /// are two questions, and the second is only asked when the first said yes.
    #[test]
    fn an_object_in_both_files_asks_about_both_of_them() {
        let (mut names, mut func) = object(16, 8, &[gpr(0, 8), sse(8)]);
        lists(&mut func, &SYSV);
        let text = printed(&func, &mut names);
        assert_eq!(text.matches("br_if").count(), 2, "one question per file: {text}");
        assert!(text.contains("iconst.i32 40"), "forty eight less one slot: {text}");
        assert!(text.contains("iconst.i32 160"), "and a hundred and seventy six less one: {text}");
        assert!(text.contains("iconst.i32 8"), "each counter steps by its own slot: {text}");
        valid(&func, &mut names);
    }

    /// An object whose last eightbyte is a part of one still comes out of the area as a whole
    /// register, so the buffer has room for the whole register and the bytes past the object are
    /// never read.
    #[test]
    fn the_buffer_is_as_big_as_the_registers_reach() {
        let (mut names, mut func) = object(5, 1, &[gpr(0, 5)]);
        lists(&mut func, &SYSV);
        let text = printed(&func, &mut names);
        assert!(text.contains("alloca, size 8"), "five bytes travel in a whole register: {text}");
        valid(&func, &mut names);
    }

    /// A classification this cannot read out of the area is left alone, which is what makes the
    /// function refused by name further down rather than compiled into half a walk.
    #[test]
    fn a_classification_that_does_not_fit_the_area_is_left_alone() {
        let wide = [Slot::Float { offset: 0, format: Format::Quad }];
        let (mut names, mut func) = object(16, 16, &wide);
        let before = printed(&func, &mut names);
        lists(&mut func, &SYSV);
        assert_eq!(printed(&func, &mut names), before);
    }

    /// What reads the object goes on reading the value it already read, the same way it does for a
    /// value, and for the same reason: the instruction becomes the address rather than being
    /// replaced by one, so nothing has to be substituted anywhere.
    #[test]
    fn what_reads_the_object_reads_the_same_value_it_did_before() {
        for slots in [&[][..], &[gpr(0, 8), gpr(8, 8)][..]] {
            let (mut names, mut func) = object(if slots.is_empty() { 24 } else { 16 }, 8, slots);
            let entry = func.entry().expect("an entry block");
            let inst = func.insts(entry).next().expect("the va_object is first");
            let read = func[inst].first_result.expect("it answers an address");

            lists(&mut func, &SYSV);
            assert_eq!(func[inst].opcode, Opcode::IntToPtr, "the same instruction, lowered");
            assert_eq!(func[inst].first_result, Some(read), "producing the same value");
            valid(&func, &mut names);
        }
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
