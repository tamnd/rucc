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
//! The upper half of a vector slot is the document's too, and what is in it is the top of a
//! `_Float128`. A slot is sixteen bytes wide because the register is, and a quad is the one type
//! here that fills one, so the spill writes all sixteen bytes of every vector register and a
//! `va_arg` of a quad reads all sixteen back. gcc writes the same sixteen with the same instruction,
//! which is what makes a list built here readable by a walk somebody else compiled. Anything wider
//! than a register would be a vector type, which is issue #200 and is not a thing yet.
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
//!
//! # The other kind of list
//!
//! Windows has none of that. Its convention counts the two register files as one run of positions,
//! so an argument's position says which register of either file it is in and the two walks above
//! are one walk. It also gives every argument exactly one eight byte slot whatever it is: anything
//! that is not one, two, four or eight bytes travels as the address of a copy the caller owns, and
//! a float beyond the ones the signature names travels in the general purpose register at its
//! position as well as in the vector one, because a callee with no prototype has no way to know
//! which file to look in.
//!
//! What that comes to is that every argument a variadic callee was passed is already one contiguous
//! run of words in the caller's argument area, since the first four of them are homed in the thirty
//! two bytes of shadow space the caller reserved above the return address and the rest follow.
//! There is nothing to gather and nowhere to gather it to. So a `va_list` is a `char *` pointing at
//! the next of those words, `va_start` is one `lea` and one store, and `va_arg` is a load and an
//! eight byte step with no compare, no branch and no second file. The register save area of the
//! four field list is, on this convention, the caller's shadow space, and the callee's prologue
//! writes its leftover argument registers into it rather than into a block of its own.
//!
//! An argument that travelled by reference costs one more load and that is the whole of the
//! difference: the slot holds the address of the copy rather than the copy. Which arguments those
//! are is a question about the size and nothing else, so the classification the front end put on a
//! `va_object` is not read here at all.

use rucc_base::float::Format;
use rucc_ir::{
    Block, Builder, Extra, Flags, Float, Func, Imm, Inst, InstData, IntPred, MemInfo, MemOrder,
    Opcode, Restrict, Type, Value,
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
/// How many bytes the four field `va_list` is, which is what a `va_copy` of one moves.
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
    ///
    /// Two shapes, and what tells them apart is the convention's own answer about how it counts
    /// argument positions. One that counts the two files apart spills all of both into a block of
    /// the callee's own frame, which is the psABI's register save area and is what the four field
    /// list walks. One that counts them as one run homes each register argument in the word of the
    /// caller's argument area that belongs to its position, and that run of words is the area. The
    /// vector file has nothing in it there: a float beyond the ones the signature names travels in
    /// the general purpose register at its position as well, so the copy a walk reads is that one.
    #[must_use]
    pub fn of(conv: &CallRegs) -> Self {
        let ints = u32::try_from(conv.int_args.len()).unwrap_or(0);
        let floats = u32::try_from(conv.sse_args.len()).unwrap_or(0);
        if conv.shared_positions {
            let size = conv.word * ints;
            return Self { floats_at: size, size, counts: (ints, 0), word: conv.word };
        }
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
    pub fn holds(self, float: bool) -> u32 {
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
/// A convention whose list is a plain pointer gets the walk the module doc's last section
/// describes instead, which is the same three rewrites over a list of one field.
pub fn lists(func: &mut Func, conv: &CallRegs) {
    let area = Area::of(conv);
    let word = u64::from(conv.word);
    let found: Vec<Inst> =
        func.blocks().flat_map(|block| func.insts(block).collect::<Vec<_>>()).collect();
    for inst in found {
        match (func[inst].opcode, conv.shared_positions) {
            (Opcode::VaArg, false) => next(func, inst, area),
            (Opcode::VaArg, true) => value(func, inst, word),
            (Opcode::VaObject, false) => object(func, inst, area),
            (Opcode::VaObject, true) => held(func, inst, word),
            (Opcode::VaCopy, shared) => copy(func, inst, if shared { word } else { SIZE }),
            // Nothing at all, which is what the psABI says it is. The instruction was still worth
            // emitting, because it says the list stops being read here, and here is where that
            // stops being worth saying.
            (Opcode::VaEnd, _) => func.remove_inst(inst),
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
    // A `long double` is class X87, which is a class with no register in the save area, so it is
    // always in the caller's argument area and there is no question to ask about it. That is this
    // walk with the register half deleted, which is little enough to be written out separately
    // rather than folded in as a special case of a branch that is never taken.
    if ty.is_float() && ty.bits() == 80 {
        x87(func, inst, area);
        return;
    }
    // A `_Float128` is the one value wider than a general purpose register that a single register
    // still holds. It is class SSE followed by SSEUP, which name one vector register between them,
    // so it walks the vector half the way a `double` does and takes the whole of a slot instead of
    // the low half of one. Where it stops being a wider `double` is the caller's argument area,
    // which gives it two words aligned to two rather than the one word every value the machine
    // computes in gets.
    let quad = ty.is_float() && ty.bits() == 128;
    // A scalar of a width a register holds, which is every type the algorithm below is right about.
    // An `__int128` takes two slots with an alignment rule of its own, which is a second algorithm
    // rather than a wider reading of this one, so it is left alone here and refused by name further
    // down.
    if !quad
        && (!ty.is_scalar() || ty.bits() > 64 || !(ty.is_int() || ty.is_float() || ty.is_ptr()))
    {
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

    // The memory path: the argument is where the caller left it, and the pointer steps on past it.
    // By a word for everything the machine computes in, because the caller's argument area is a run
    // of whole words whatever is in them, and by two words rounded up to two for a quad, which is
    // the slot the class gets from a function that names it as well as from one that does not.
    let mut build = Builder::new(func, overflowed).at(span);
    let (slot, want) = if quad {
        (u64::from(VECTOR_SLOT), VECTOR_SLOT)
    } else {
        (u64::from(area.word), area.word)
    };
    let at = overflow(&mut build, list, area, slot, want);
    let here = build.unary(Opcode::IntToPtr, at, Type::PTR);
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

/// One `va_arg` of a `long double`, which is the memory half of the walk and nothing else.
///
/// The psABI gives a `long double` class X87 and there is no x87 register among the ones a variadic
/// callee spills, so a `long double` passed to one is in the caller's argument area whatever else
/// the call passed and however few arguments came before it. Nothing is asked, no block is made,
/// and the overflow pointer is rounded up, read and stepped on.
///
/// Sixteen bytes and sixteen byte alignment are the psABI's numbers for the class rather than the
/// type's own: the value is ten bytes of x87 and the argument slot it sits in is padded out to two
/// words, which is why the load below is ten bytes wide and the step is sixteen.
fn x87(func: &mut Func, inst: Inst, area: Area) {
    /// What one of these takes in the argument area.
    const SLOT: u64 = 16;
    /// What the argument area aligns one to.
    const ALIGN: u32 = 16;

    let Some(result) = func[inst].first_result else { return };
    let Some(&list) = func[func[inst].args].first() else { return };
    let Some(block) = func.block_of(inst) else { return };
    let bytes = func[result].ty.bits() / 8;
    let span = func.span(inst);

    // Everything below the instruction, taken out before anything is built, for the reason the
    // branching walk takes it out: a builder appends to a block, and the instruction has to end up
    // behind what is built and in front of what followed it.
    let rest: Vec<Inst> = func.insts(block).skip_while(|&at| at != inst).skip(1).collect();
    func.remove_inst(inst);
    for &at in &rest {
        func.remove_inst(at);
    }

    let mut build = Builder::new(func, block).at(span);
    let at = overflow(&mut build, list, area, SLOT, ALIGN);
    let addr = build.unary(Opcode::IntToPtr, at, Type::PTR);

    let mem = func.add_mem(info(u64::from(bytes), ALIGN));
    let args = func.push_values(&[addr]);
    let data = &mut func[inst];
    data.opcode = Opcode::Load;
    data.args = args;
    data.extra = Extra::Mem(mem);
    data.flags = data.flags.intersection(Flags::legal_on(Opcode::Load));
    func.append_inst(block, inst);
    for at in rest {
        func.append_inst(block, at);
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
    // It is also aligned to whatever the widest slot has to be stored at rather than to whatever
    // the object asked for, which is the same number for every object a C program can write and is
    // not the same statement. A slot holding a whole vector register moves as a `movaps`, and a
    // `movaps` faults on an address that is not a multiple of sixteen, so the buffer says sixteen
    // because the copy needs it and not because the type happened to ask.
    let reach = slots.iter().map(|&slot| slot.offset() + width(slot)).max().unwrap_or(0);
    let wants = slots
        .iter()
        .map(|&slot| slot_align(area, is_float(slot), width(slot)))
        .max()
        .unwrap_or(1)
        .max(align);
    let room = buffer(func, inst, reach.max(size), wants);

    // Everything below the instruction, taken out before anything is built, because a builder
    // appends to a block and the register form ends this one at a branch.
    let rest: Vec<Inst> = func.insts(block).skip_while(|&at| at != inst).skip(1).collect();
    func.remove_inst(inst);
    for &at in &rest {
        func.remove_inst(at);
    }

    let (ends, address) = match room {
        Some(room) if !slots.is_empty() => {
            let read = Read { list, area, slots: &slots, size, align, room, wants };
            registers(func, block, inst, read)
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
    /// What it is aligned to, which is what the caller's argument area put it at.
    align: u32,
    /// The buffer of the function's own the register form copies the object into.
    room: Value,
    /// What that buffer is aligned to, which is the object's alignment or what the widest slot
    /// needs, whichever is the larger.
    wants: u32,
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
        let float = is_float(slot);
        if !in_a_register(slot) || width(slot) > u64::from(area.stride(float)) {
            return false;
        }
        counts[usize::from(float)] += 1;
    }
    counts[0] <= area.holds(false) && counts[1] <= area.holds(true)
}

/// Whether a slot is one of the registers a variadic callee spills.
///
/// The width does not say on its own. A `long double` is ten bytes and would sit inside a vector
/// slot with room to spare, and it is class X87, which has no register among the fourteen, so a
/// classification carrying one is a classification this walk cannot read. The formats a vector
/// register does hold are named rather than the ones it does not, so a format added later is one
/// this leaves alone until somebody says where it travels.
fn in_a_register(slot: Slot) -> bool {
    match slot {
        Slot::Integer { .. } => true,
        Slot::Float { format, .. } => matches!(
            format,
            Format::Half | Format::BFloat16 | Format::Single | Format::Double | Format::Quad
        ),
    }
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
        let bytes = width(slot);
        let ty = moved_as(area, float, bytes);
        let aligned = slot_align(area, float, bytes);
        let at = offset(build, from, step);
        let value = build.load(ty, at, info(bytes, aligned), Flags::default());
        let into = offset(build, read.room, i64::try_from(slot.offset()).unwrap_or(0));
        let holds = info(bytes, part(read.wants, slot.offset()));
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

/// What one slot's bytes are moved as.
///
/// An integer of the slot's width whatever file it came from, because what this is is a copy of the
/// object's bytes and nothing here reads them as anything. A slot the whole width of a vector
/// register is the exception and has to be: there is no integer that wide on this machine, and the
/// file the bytes are already in is the one that moves all sixteen of them at once.
fn moved_as(area: Area, float: bool, bytes: u64) -> Type {
    if float && bytes > u64::from(area.word) {
        return Type::float(Float::F128);
    }
    Type::int(u32::try_from(bytes).unwrap_or(1) * 8)
}

/// What the address of a slot in the save area is known to be aligned to.
///
/// The area begins on a vector slot boundary and every slot in it is a whole number of its file's
/// strides along from there, so a value as wide as its file's stride sits at a multiple of the
/// stride and everything narrower sits at a multiple of a word. The wide case is the one that has
/// to be right, since what moves a whole vector register is a `movaps` and a `movaps` faults on an
/// address that is not a multiple of sixteen rather than being slow about it.
fn slot_align(area: Area, float: bool, bytes: u64) -> u32 {
    if bytes > u64::from(area.word) { area.stride(float) } else { area.word }
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

/// Whether an argument of that size travelled as the address of a copy rather than as itself.
///
/// The platform's rule stated as a size and nothing else: an argument that is not one, two, four or
/// eight bytes is passed as a pointer to a copy the caller made, whatever the argument is made of.
/// A three byte structure is one and so is a sixteen byte float, and no classification is asked
/// about either, which is why the slots the front end put on a `va_object` go unread on this side.
fn by_reference(size: u64) -> bool {
    !matches!(size, 1 | 2 | 4 | 8)
}

/// The slot the walk is at, with the list stepped on past it, written in front of an instruction.
///
/// One word whatever is in the slot, because this convention gives every argument exactly one and
/// pays for the ones that do not fit by passing their address instead. So there is nothing to round
/// up, nothing to ask and nothing to branch on.
fn slot(func: &mut Func, inst: Inst, list: Value, word: u64) -> Value {
    let here = read(func, inst, list, Type::PTR, word);
    let step = field(func, inst, here, i64::try_from(word).unwrap_or(0));
    let mem = func.add_mem(info(word, u32::try_from(word).unwrap_or(1)));
    let args = func.push_values(&[step, list]);
    let data = InstData { args, extra: Extra::Mem(mem), ..InstData::new(Opcode::Store) };
    let span = func.span(inst);
    let made = func.create_inst(data, &[], span);
    func.insert_before(made, inst);
    here
}

/// A load written in front of an instruction, at the alignment its own width gives it.
fn read(func: &mut Func, inst: Inst, from: Value, ty: Type, size: u64) -> Value {
    let mem = func.add_mem(info(size, u32::try_from(size).unwrap_or(1)));
    let args = func.push_values(&[from]);
    let data = InstData { args, extra: Extra::Mem(mem), ..InstData::new(Opcode::Load) };
    ahead(func, inst, data, ty)
}

/// How many bytes of a scalar the one field walk moves, or nothing for a type it is not right
/// about.
///
/// A pointer has no width of its own here and is as wide as the convention's word, which is the one
/// question this has to ask the target rather than the type.
///
/// Anything wider than a general purpose register is left alone, which is a `long double`, a
/// `_Float128` and an `__int128`. The convention travels all three as the address of a copy, and
/// reading one back that way would be reading through an address nobody wrote: a wide scalar is
/// passed as itself here today whether or not the signature names it, which is tamnd/rucc#1331 and
/// is wrong for a named argument first. So they stay as they are and are refused by name further
/// down, which is where the four field walk leaves the last of them too.
fn travels(ty: Type, word: u64) -> Option<u64> {
    if !ty.is_scalar() || !(ty.is_int() || ty.is_float() || ty.is_ptr()) {
        return None;
    }
    if ty.is_ptr() {
        return Some(word);
    }
    (ty.bits() <= 64).then(|| u64::from(ty.bits().div_ceil(8)))
}

/// One `va_arg` on a convention whose list is a plain pointer, as the load at the slot the walk is
/// at.
///
/// The instruction becomes that load rather than being replaced by one, for the reason the
/// branching walk gives: the value the rest of the function reads stays the value it already read,
/// so nothing has to be substituted anywhere. Everything the load needs is written in front of it,
/// and since nothing here branches the instruction does not move and the block is not cut.
///
/// Every scalar [`travels`] answers for is one the convention passes whole, so the slot holds the
/// value and not an address, and the load is the whole of it. The object walk below is where the
/// other case is.
fn value(func: &mut Func, inst: Inst, word: u64) {
    let Some(result) = func[inst].first_result else { return };
    let Some(&list) = func[func[inst].args].first() else { return };
    let ty = func[result].ty;
    let Some(bytes) = travels(ty, word) else { return };

    let from = slot(func, inst, list, word);
    let mem = func.add_mem(info(bytes, u32::try_from(bytes).unwrap_or(1)));
    let args = func.push_values(&[from]);
    let data = &mut func[inst];
    data.opcode = Opcode::Load;
    data.args = args;
    data.extra = Extra::Mem(mem);
    data.flags = data.flags.intersection(Flags::legal_on(Opcode::Load));
}

/// One `va_object` on the same convention, as the address the object can be read from.
///
/// The slot itself for an object of a width the convention passes whole, and what the slot holds
/// for every other one, which is the address of the copy the caller made. That is the same question
/// [`by_reference`] answers for a scalar and it is asked of the size alone, so an object of three
/// bytes and an object of a hundred take the two different paths for the one reason.
///
/// The address is answered rather than a copy of the object, which is what the instruction is for:
/// the object is already somewhere addressable either way, and the copy the C standard describes is
/// the assignment the caller of `va_arg` wrote.
fn held(func: &mut Func, inst: Inst, word: u64) {
    let Extra::VaObject(at) = func[inst].extra else { return };
    let MemInfo { size, .. } = func[func[at].mem];
    let Some(&list) = func[func[inst].args].first() else { return };
    if func[inst].first_result.is_none() {
        return;
    }

    let here = slot(func, inst, list, word);
    let from = if by_reference(size) { read(func, inst, here, Type::PTR, word) } else { here };
    // Through an integer and back, which is what the branching walk's answer is too and is free
    // either way: the two are the same bits on this machine and nothing is written for the pair.
    let args = func.push_values(&[from]);
    let data = InstData { args, ..InstData::new(Opcode::PtrToInt) };
    let address = ahead(func, inst, data, Type::int(64));
    let args = func.push_values(&[address]);
    let data = &mut func[inst];
    data.opcode = Opcode::IntToPtr;
    data.args = args;
    data.extra = Extra::None;
    data.flags = data.flags.intersection(Flags::legal_on(Opcode::IntToPtr));
}

/// One `va_copy`, as the fields of one list moved into another.
///
/// A list is those fields and holds nothing anywhere else, so copying it is copying them, and a
/// handful of words move as a handful of words rather than as a call to `memcpy`, which is a name
/// this compiler cannot emit yet and would be the wrong answer for three words in any case. How
/// many words there are is the convention's answer: three for the four field list, since the two
/// offsets share one, and one for the list that is a pointer.
///
/// Every read is built before any write, so that a list copied onto itself, which is legal and
/// useless, moves what it held rather than what it has just been given.
fn copy(func: &mut Func, inst: Inst, bytes: u64) {
    let [into, from] = func[func[inst].args] else { return };
    let mut moved = Vec::new();
    for word in 0..bytes / 8 {
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

/// The address that far past a pointer, written in front of an instruction, or the pointer itself
/// for no distance at all.
///
/// A field of a list for the walk that has four of them, and the slot behind this one for the walk
/// whose list is a pointer.
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
    MemInfo {
        size,
        align,
        order: MemOrder::NotAtomic,
        tbaa: None,
        owns: 0,
        restrict: Restrict::NONE,
    }
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
    ///
    /// Class X87 is the one to ask about, because the width alone would say yes: ten bytes sit
    /// inside a vector slot with room to spare, and there is no x87 register among the fourteen a
    /// variadic callee spills, so there is nothing in the area for this to read.
    #[test]
    fn a_classification_that_does_not_fit_the_area_is_left_alone() {
        let x87 = [Slot::Float { offset: 0, format: Format::X87Extended }];
        let (mut names, mut func) = object(16, 16, &x87);
        let before = printed(&func, &mut names);
        lists(&mut func, &SYSV);
        assert_eq!(printed(&func, &mut names), before);
    }

    /// A `_Float128` walks the vector half with a slot of the whole register.
    ///
    /// One register and not two, which is the same answer the classification gives a quad passed to
    /// a function that names it: the offset stops at the last slot rather than the last but one,
    /// and it steps on by sixteen. What is read is sixteen bytes of float, which is a `movaps`
    /// further down and is the instruction gcc reads the same slot with.
    #[test]
    fn a_quad_takes_a_whole_vector_slot_of_the_save_area() {
        let (mut names, mut func) = built(Opcode::VaArg, Type::float(rucc_ir::Float::F128), 1);
        lists(&mut func, &SYSV);
        valid(&func, &mut names);
        let text = printed(&func, &mut names);
        assert!(!text.contains("va_arg"), "the va_arg is gone: {text}");
        assert!(text.contains("iconst.i32 160"), "a hundred and seventy six less one slot: {text}");
        assert!(text.contains("iconst.i32 16"), "and the counter steps by a whole one: {text}");
        assert!(text.contains("load.f128"), "read as the sixteen bytes it is: {text}");
    }

    /// And the argument area gives it two words aligned to two, which is where it stops being a
    /// wider `double`. Every other value the machine computes in is where the pointer already is
    /// and steps it on by a word.
    #[test]
    fn a_quad_the_registers_ran_out_before_is_rounded_up_to_sixteen() {
        let (mut names, mut func) = built(Opcode::VaArg, Type::float(rucc_ir::Float::F128), 1);
        lists(&mut func, &SYSV);
        let text = printed(&func, &mut names);
        assert!(text.contains("iconst.i64 15"), "up to the next sixteen: {text}");
        assert!(text.contains("iconst.i64 -16"), "and down to a multiple of it: {text}");
        assert!(text.contains(" = and "), "which is an add and a mask: {text}");

        let (mut names, mut func) = built(Opcode::VaArg, Type::float(rucc_ir::Float::F64), 1);
        lists(&mut func, &SYSV);
        let text = printed(&func, &mut names);
        assert!(!text.contains(" = and "), "a double is where the pointer already is: {text}");
        assert!(text.contains("iconst.i64 8"), "and steps it on by a word: {text}");
    }

    /// An object holding a quad is one slot of the vector file, so the question is a single one
    /// and the copy moves all sixteen bytes at once.
    ///
    /// The buffer it lands in is sixteen byte aligned, which is what the store needs rather than
    /// what the object asked for, although for this object the two are the same number.
    #[test]
    fn an_object_holding_a_quad_is_copied_out_as_one_whole_register() {
        let quad = [Slot::Float { offset: 0, format: Format::Quad }];
        let (mut names, mut func) = object(16, 16, &quad);
        lists(&mut func, &SYSV);
        valid(&func, &mut names);
        let text = printed(&func, &mut names);
        assert!(!text.contains("va_object"), "{text}");
        assert_eq!(text.matches("br_if").count(), 1, "one file, so one question: {text}");
        assert!(text.contains("iconst.i32 160"), "a hundred and seventy six less one slot: {text}");
        assert!(text.contains("load.f128"), "moved as the register it is in: {text}");
        assert!(text.contains("alloca, size 16, align 16"), "a buffer a movaps accepts: {text}");
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

    /// Windows describes a list as one pointer, and its walk is that pointer stepped on, so there is
    /// nothing to compare and nowhere else to look: the argument is at the pointer, the pointer
    /// moves on by a word, and all of it is straight line.
    #[test]
    fn a_windows_va_arg_is_the_word_at_the_pointer_and_a_step() {
        let (mut names, mut func) = built(Opcode::VaArg, Type::int(32), 1);
        lists(&mut func, &WIN64);
        valid(&func, &mut names);
        let text = printed(&func, &mut names);
        assert!(!text.contains("va_arg"), "the va_arg is gone: {text}");
        assert!(!text.contains("br_if"), "and nothing was asked: {text}");
        assert_eq!(func.blocks().count(), 1, "so no block was made: {text}");
        assert!(text.contains("iconst.i64 8"), "the step is one word: {text}");
        assert_eq!(text.matches("= load").count(), 2, "the list and the argument: {text}");
        assert_eq!(text.matches("store").count(), 1, "and the list is written back: {text}");
    }

    /// A pointer is as wide as the convention says a word is, since a type carries no width for
    /// one. Reading it as no bytes at all would be every string a `printf` was handed.
    #[test]
    fn a_windows_pointer_argument_is_the_whole_word() {
        let (mut names, mut func) = built(Opcode::VaArg, Type::PTR, 1);
        lists(&mut func, &WIN64);
        valid(&func, &mut names);
        let text = printed(&func, &mut names);
        assert_eq!(text.matches("= load").count(), 2, "the list and the argument: {text}");
        assert_eq!(text.matches("size 8").count(), 3, "and all three are words: {text}");
    }

    /// The size is the whole of what says where a Windows argument is, so an object of eight bytes
    /// is in the slot and the answer is the slot's own address, and one of twenty four is elsewhere
    /// and the answer is what the slot holds. Neither of them asks about the classification.
    #[test]
    fn a_windows_object_is_in_the_slot_or_behind_it_according_to_its_size() {
        for (size, loads) in [(8, 1), (24, 2)] {
            let (mut names, mut func) = object(size, 8, &[]);
            lists(&mut func, &WIN64);
            valid(&func, &mut names);
            let text = printed(&func, &mut names);
            assert!(!text.contains("va_object"), "{text}");
            assert_eq!(func.blocks().count(), 1, "no branch, so no new block: {text}");
            assert_eq!(text.matches("= load").count(), loads, "{size} bytes: {text}");
            assert!(!text.contains("iconst.i64 24"), "the step is a word either way: {text}");
        }
    }

    /// A list that is one pointer is copied by moving one pointer, and a copy moving three words
    /// would read two the caller never wrote and write them somewhere it does not own.
    #[test]
    fn a_windows_va_copy_moves_the_one_word_a_list_is() {
        let (mut names, mut func) = built(Opcode::VaCopy, Type::VOID, 2);
        lists(&mut func, &WIN64);
        valid(&func, &mut names);
        let text = printed(&func, &mut names);
        assert!(!text.contains("va_copy"), "{text}");
        assert_eq!(text.matches("load.i64").count(), 1, "{text}");
        assert_eq!(text.matches("store").count(), 1, "{text}");
    }

    /// A scalar wider than a general purpose register is left alone, because the convention travels
    /// one as the address of a copy and nothing here writes that address yet, which is
    /// tamnd/rucc#1331. Reading the slot as the value would be reading the low eight bytes of a
    /// `long double`, and reading it as an address would be following a float.
    #[test]
    fn a_wide_scalar_is_left_alone_on_windows() {
        let wide =
            [Type::int(128), Type::float(rucc_ir::Float::F80), Type::float(rucc_ir::Float::F128)];
        for ty in wide {
            let (mut names, mut func) = built(Opcode::VaArg, ty, 1);
            let before = printed(&func, &mut names);
            lists(&mut func, &WIN64);
            assert_eq!(printed(&func, &mut names), before, "{ty:?}");
        }
    }

    /// A width the algorithm is not right about is left alone for the same reason. An `__int128`
    /// takes two slots under an alignment rule of its own, which is a second algorithm and not a
    /// wider reading of this one, so it stays exactly as it was and is refused by name later.
    #[test]
    fn a_type_that_does_not_travel_in_one_slot_is_left_alone() {
        let ty = Type::int(128);
        let (mut names, mut func) = built(Opcode::VaArg, ty, 1);
        let before = printed(&func, &mut names);
        lists(&mut func, &SYSV);
        assert_eq!(printed(&func, &mut names), before, "{ty:?}");
    }

    /// A `long double` is class X87, and the class has no register among the fourteen a variadic
    /// callee spills, so one is in the caller's argument area whether or not anything came before
    /// it. What that means for the rewrite is that the question `va_arg` usually asks has a known
    /// answer, so there is no compare, no branch and no join: one block, the overflow pointer
    /// rounded up to sixteen and stepped on by sixteen, and the load.
    #[test]
    fn a_long_double_is_read_straight_out_of_the_callers_argument_area() {
        let (mut names, mut func) = built(Opcode::VaArg, Type::float(rucc_ir::Float::F80), 1);
        lists(&mut func, &SYSV);
        valid(&func, &mut names);
        let text = printed(&func, &mut names);
        assert!(!text.contains("va_arg"), "the va_arg is gone: {text}");
        assert!(!text.contains("br_if"), "and nothing was asked: {text}");
        assert_eq!(func.blocks().count(), 1, "so no block was made: {text}");
        // The two numbers the psABI gives the class, in the rounding up and in the step.
        assert!(text.contains(" 15"), "rounded up to sixteen: {text}");
        assert!(text.contains(" 16"), "and stepped on by sixteen: {text}");
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
