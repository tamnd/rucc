//! Where a function's arguments already are when it starts running, and where a call puts its own.
//!
//! Design: `spec/12-abi-and-runtime.md`.
//!
//! This is the one part of the calling convention that is not a lowering rule, and it is worth
//! saying why, because everything else in this crate is. A rule matches a term and rewrites it,
//! and which register the third argument arrives in is not a fact about any term: it depends on
//! the argument's position and on the classification of every argument before it. A pattern has
//! nowhere to put that. So the arguments are built here, by hand, out of what the convention
//! says, the same way [`crate::finish`] builds a prologue.
//!
//! The classification itself is not here either. `rucc-lower` has already run it by the time a
//! function reaches this crate, which is why the parameters read here are nearly all plain
//! scalars: an aggregate has been split into the pieces it travels in, and a return through memory
//! is an ordinary pointer parameter in front of the rest. What is left for this is the step after
//! classification, from how a value travels to which register it is actually in, which is
//! [`rucc_target::Places`].
//!
//! The one parameter that is not a scalar is an aggregate the classification put in the argument
//! area whole, which is [`rucc_ir::Abi::ByVal`]. The IR calls it a pointer, because a pointer is
//! what an instruction reading it has to have, and the convention says the bytes travel and the
//! pointer does not. So this is the one place that reads what the classification said rather than
//! only the type, on both sides of the call, and the two sides are the two halves of one copy.
//!
//! # What it writes
//!
//! One `x64.arg_val_*` per parameter that arrived in a register, at the top of the entry block,
//! each defining a fresh register constrained to the one the argument arrived in. They encode to
//! nothing. The point of them is that a parameter has to be defined somewhere for the allocator to
//! have anything to move, and the entry block cannot define it as a block parameter: there is no
//! edge into the entry block for the move to go on, which is what `rucc_regalloc::rewrite` asserts.
//!
//! What the allocator does with them is the whole of the argument sequence. A parameter that is
//! read where it arrived costs nothing, and one that is not gets a copy, which is the same
//! bargain the return already makes and is decided by the same code.
//!
//! A parameter whose bytes travelled is the exception to all of that. Its bytes are already in
//! this function, at a place in the caller's argument area the same walk gives, so nothing is
//! brought in at all: what the parameter is is where they are, and that is one `lea`. It waits on
//! the frame the way the loads below it do, and for the same reason.
//!
//! A parameter past the last register arrived in the caller's memory rather than in a register, so
//! it is a load and not a pseudo, and it is a real instruction that encodes to real bytes. How far
//! up the caller's argument area it is is a number [`rucc_target::Places`] answers here, but where
//! that area is from inside this function is a distance into a frame, and no frame exists until
//! after allocation. So the load is written with nothing in its displacement, which of the two
//! registers it reads through is left to be settled too, and both are filled in by [`crate::finish`]
//! out of [`crate::frame::Frame::incoming`]. That is the same bargain an `alloca` already makes,
//! for the same reason and in the same two places.
//!
//! # A call
//!
//! The same reasoning the other way round, and one instruction rather than several. `x64.call`
//! and `x64.call_reg` are the only opcodes in the description whose operand vector is empty
//! there, because nothing about a call's operands is the same from one call to the next, so they
//! are built here: one read per argument constrained to the register the convention passes it in,
//! one definition for the value that comes back constrained to the register it comes back in, and
//! one definition per register the convention does not preserve.
//!
//! A call through an address has one operand more, which is the address, and it is the one
//! operand of a call that is a fact about the instruction rather than about the signature. It
//! goes in front of the arguments, because the assembler has to find it and an index into a
//! vector whose length depends on the convention is not a way of finding anything.
//!
//! Those last ones are the clobbers, and they are the whole of what the allocator has to know
//! about a call besides where the values go. Each is a definition of the physical register itself
//! rather than of a value, since there is no value: it says the register is written here, which
//! is exactly what stops the allocator from leaving something in one across the call. A register
//! an argument or the result already names is not repeated, because naming it once already blocks
//! it for the length of the instruction, which is all a clobber does.
//!
//! An argument past the last register the convention has for it is a store into the outgoing area
//! rather than an operand of the call, written in front of the call in the same block. Where that
//! area is does not have to wait for the frame the way the incoming one does, because the outgoing
//! area is at the bottom of the frame and the bottom of the frame is where the stack pointer is:
//! that is the whole reason the frame puts it there, since it is where the callee will look. So the
//! offset [`rucc_target::Places`] gives back is the offset the store is written with.
//!
//! An object passed by value in memory is the same thing again and a copy rather than a store. The
//! caller owes the callee a copy it is free to write to, which is what makes a C call by value
//! different from passing a pointer the callee must not keep, and the argument area is where the
//! convention says that copy goes. So the bytes are read out of the object and written into the
//! area a word at a time, in front of the call, with the words chosen by the same function that
//! chooses them for a `memcpy`. An object with more words than that unrolls to is turned down,
//! because the copy it wants is a call to the runtime and one call cannot be built inside another.
//!
//! The call still reports how many bytes it needed, because the frame reserves as many as the
//! widest call in the function asked for and cannot know that until every call has been seen.

use rucc_base::{Interner, Symbol};
use rucc_ir::{Abi, Param, Type};
use rucc_mir as mir;
use rucc_target::x86_64;
use rucc_target::{CallRegs, Constraint, PhysReg, Places, RegClass, Where};

use crate::varargs::Area;

/// Why a parameter could not be brought in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Missing {
    /// It travels on the x87 stack, which is a `long double` and nothing else. That stack is a
    /// third register file, it is not one the allocator has, and no instruction in the
    /// description touches it.
    OnX87,
    /// It is a float passed to a callee that takes arguments beyond the ones its signature names,
    /// on a convention that puts such a float in a vector register and in the general purpose
    /// register at the same position at once. Which arguments are the ones beyond the signature is
    /// what decides whether the second copy is needed, and a call does not carry that yet.
    InBothFiles,
    /// It is a width no pseudo covers, which is anything a machine register does not hold.
    Width,
    /// It is a value that comes back in more registers than the convention returns in. A structure
    /// of at most sixteen bytes comes back in up to two, which is as many as SysV has, and a
    /// convention with fewer of them returns such a structure through a hidden pointer instead. So
    /// this is what a signature the classification did not produce would get.
    NoRoom,
    /// It is an object whose bytes travel in the argument area and there are more of them than a
    /// copy a word at a time is worth. Such a copy belongs in a call to the runtime, and the place
    /// this is decided is in the middle of building a call, where another one cannot go.
    TooBig,
}

impl Missing {
    /// What it says when a function could not be compiled because of it.
    ///
    /// Worded so that it reads the same about a value arriving and a value being passed, since
    /// the two are the same fact seen from the two ends of one call.
    #[must_use]
    pub fn why(self) -> &'static str {
        match self {
            Missing::OnX87 => "is on the x87 stack",
            Missing::InBothFiles => "is a float passed to a variadic callee on this convention",
            Missing::Width => "is a width no argument register holds",
            Missing::NoRoom => "takes more registers than this convention has for it",
            Missing::TooBig => "is more bytes than a copy into the argument area unrolls to",
        }
    }
}

/// Which register file a value of that type travels in.
///
/// The whole of what the two files mean to this module. A float is in the vector one and
/// everything else is in the general purpose one, which is what both of this machine's conventions
/// say, and the `long double` that is in neither is turned away by [`refuses`] before this is
/// asked.
fn class_of(ty: Type, conv: &CallRegs) -> RegClass {
    if ty.is_float() { conv.sse_class } else { conv.int_class }
}

/// Why a value of that type cannot travel at all, or nothing if it can.
///
/// The width question and the file question in one place, so that the two ends of a call give the
/// same answer about the same type, and so that a `return` this cannot make says the same thing
/// about a type as the call that would have received it.
#[must_use]
pub fn refuses(ty: Type) -> Option<Missing> {
    if head_of(ty).is_some() {
        return None;
    }
    // A `long double` is the one type here that is in neither of the two files. Saying so is worth
    // more than calling it a width, because eighty bits is a width this machine computes in and
    // the file it computes in is what actually stands in the way.
    if ty.is_float() && ty.bits() == 80 {
        return Some(Missing::OnX87);
    }
    Some(Missing::Width)
}

/// What a function's parameters came to.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Arrived {
    /// The register each parameter is in, in the order the parameters were given, so the caller can
    /// bind each IR parameter to the one at its position.
    pub regs: Vec<mir::Reg>,
    /// The loads that read a parameter out of the caller's argument area, and how far up that area
    /// each of them reads.
    ///
    /// Empty for almost every function, because almost every function has few enough parameters to
    /// have been handed all of them in registers. The distance is from the bottom of the caller's
    /// argument area, which is somewhere [`crate::finish`] works out and this cannot.
    pub stack: Vec<(mir::Inst, u32)>,
    /// How many general purpose argument registers the parameters took, and how many vector ones.
    ///
    /// Nothing about an ordinary function needs this. A variadic one does: the first argument its
    /// signature does not name is the one after the last it does, so where each of the two walks
    /// stopped is where `va_start` has to say the next argument begins.
    pub took: (usize, usize),
    /// How many bytes of the caller's argument area the parameters took, which is where the first
    /// argument the signature does not name begins for the same reason.
    pub used: u32,
    /// The argument registers left over for the arguments the signature does not name, as the
    /// register each was bound into and how far up the save area its slot is.
    ///
    /// Empty unless a save area was asked for. The ones a named parameter took are not here,
    /// because their slots are behind where `va_start` sets the two offsets and nothing ever reads
    /// them, so writing them would be fourteen stores where six are wanted.
    pub spare: Vec<(mir::Reg, RegClass, u32)>,
}

/// Binds a function's parameters to where the convention says they arrive.
///
/// # Errors
///
/// The first parameter this cannot bring in, and why. A function with one is reported rather
/// than compiled, because the alternative is a function that reads an argument from wherever the
/// last one happened to leave a register.
pub fn entry(
    out: &mut mir::Func,
    block: mir::Block,
    params: &[Param],
    conv: &CallRegs,
    names: &mut Interner,
    save: Option<Area>,
) -> Result<Arrived, (usize, Missing)> {
    // Where everything is, worked out before anything is written, both so that a parameter this
    // cannot bring in stops the function before half of one is built and so that the two loops
    // below can be two loops. Asking for the place of a parameter that cannot be brought in is
    // still done, because every place after it depends on it and a reader stepping through this
    // should see the same numbers a working version would.
    let mut places = Places::new(conv);
    let mut where_from = Vec::with_capacity(params.len());
    for (index, &Param { ty, abi }) in params.iter().enumerate() {
        // A structure the classification put in the argument area arrived as bytes, and the
        // parameter the IR sees is a pointer to them. So there is nothing to bring in: the bytes
        // are already in this function's frame, and what the pointer holds is where they are.
        if let Abi::ByVal { size, align } = abi {
            let size = u32::try_from(size).map_err(|_| (index, Missing::TooBig))?;
            where_from.push((ty, places.on_stack(size, align), abi));
            continue;
        }
        let at = if ty.is_float() { places.float() } else { places.integer() };
        if let Some(missing) = refuses(ty) {
            return Err((index, missing));
        }
        where_from.push((ty, at, abi));
    }
    let mut arrived = Arrived {
        regs: Vec::with_capacity(params.len()),
        took: (places.integers(), places.floats()),
        used: places.size(),
        ..Arrived::default()
    };

    // Every pseudo first and everything else after, which is not a preference. A pseudo says a
    // register holds an argument and defines nothing before it, so as far as the allocator can see
    // the register was dead until then and is free to be used as a scratch. That is true of a
    // register no pseudo has named yet, and it stops being true the moment one does. Anything that
    // needs a scratch has to come after all of them, and a load out of the caller's stack needs one
    // for the value it loads.
    for (index, &(ty, at, _)) in where_from.iter().enumerate() {
        let class = class_of(ty, conv);
        let reg = out.new_vreg(class);
        arrived.regs.push(reg);
        let Where::Reg(arrived_in) = at else { continue };
        let head = head_of(ty).ok_or((index, Missing::Width))?;
        let opcode = mir::Opcode::new(names.intern(head));
        let operand = mir::Operand::write(reg, class).with(Constraint::Fixed(arrived_in));
        out.build(block, opcode).operand(operand).finish();
    }
    if let Some(area) = save {
        arrived.spare = spare(out, block, conv, names, area, arrived.took);
    }

    let lea = format!("{}{}", crate::lower::PREFIX, x86_64::FRAME.lea);
    // The stack pointer is written down as the register to read through because it is the one that
    // reaches the caller's stack in almost every function, and a realigned frame is the exception
    // that [`crate::finish`] rewrites. Putting something here rather than nothing keeps the
    // instruction printable and verifiable in between.
    for (index, &(ty, at, abi)) in where_from.iter().enumerate() {
        let Where::Stack(up) = at else { continue };
        let class = class_of(ty, conv);
        // The bytes of an object that travelled as bytes are read by whatever reads the parameter,
        // and what the parameter is is their address, so this takes the address rather than a
        // value out of it. Everything else about it is the load's, including the two fields
        // [`crate::finish`] fills in, because where the caller's argument area is is the same
        // question for both.
        let name = match abi {
            Abi::ByVal { .. } => lea.as_str(),
            _ => load_of(ty).ok_or((index, Missing::Width))?,
        };
        let opcode = mir::Opcode::new(names.intern(name));
        let sp = mir::Operand::read(mir::Reg::physical(conv.stack_pointer), conv.int_class);
        let made =
            out.build(block, opcode).def(arrived.regs[index], class).mem(mir::Mem::at(sp)).finish();
        arrived.stack.push((made, up));
    }
    Ok(arrived)
}

/// Binds the argument registers no parameter the signature names took, which are the ones the
/// arguments it does not name arrived in.
///
/// One pseudo each and nothing else, for the reason the loop above them gives: what these do is say
/// the register holds something, and the stores that put it in the save area are written by
/// [`crate::lower`] once it has an address to store to, which is after every pseudo in the block.
fn spare(
    out: &mut mir::Func,
    block: mir::Block,
    conv: &CallRegs,
    names: &mut Interner,
    area: Area,
    took: (usize, usize),
) -> Vec<(mir::Reg, RegClass, u32)> {
    let word = Type::int(64);
    let double = Type::float(rucc_ir::Float::F64);
    let files = [(conv.int_args, took.0, word, false), (conv.sse_args, took.1, double, true)];
    let mut spare = Vec::new();
    for (regs, taken, ty, float) in files {
        let Some(head) = head_of(ty) else { continue };
        let class = class_of(ty, conv);
        for (index, &arrived_in) in regs.iter().enumerate().skip(taken) {
            let reg = out.new_vreg(class);
            let opcode = mir::Opcode::new(names.intern(head));
            let operand = mir::Operand::write(reg, class).with(Constraint::Fixed(arrived_in));
            out.build(block, opcode).operand(operand).finish();
            let at = area.starts_at(float) + area.stride(float) * u32::try_from(index).unwrap_or(0);
            spare.push((reg, class, at));
        }
    }
    spare
}

/// What the instruction that calls a name is called.
///
/// Here rather than in a rule for the same reason the arguments are: a rule pattern sees one term
/// and a call's operands are whatever the signature made them, so no pattern could name them.
pub const CALL: &str = "x64.call";

/// What the instruction that calls an address in a register is called.
///
/// A different instruction rather than the same one with a different operand, which is what the
/// machine says too: one carries the distance to somewhere in the program and takes a relocation,
/// and the other carries the register the address is in and takes none. Sharing an opcode would
/// mean an instruction whose bytes depend on whether a field beside it happens to be set.
pub const CALL_REG: &str = "x64.call_reg";

/// One value a call passes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Passing {
    /// The type it travels as, which for an object travelling as bytes is the pointer's rather
    /// than the object's, because the pointer is what the machine IR has.
    pub ty: Type,
    /// The register holding it, or holding its address when the bytes are what travel.
    pub reg: mir::Reg,
    /// What the classification asked of it. The one thing read here is whether the object behind
    /// the pointer is the argument, since everything else it can say is about a value that is
    /// already in a register in the form it travels in.
    pub abi: Abi,
}

/// What one call came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Made {
    /// The registers the value came back in, in the order the signature returns them, which is
    /// empty for a call that gives nothing back and holds two for a structure that comes back in a
    /// pair. Which register each of them is is the classification's answer and is worked out here
    /// rather than in a table, for the reason the second half of [`Calling::returns`] gives.
    pub results: Vec<mir::Reg>,
    /// How many bytes below the stack pointer this call needs for the arguments it passes there.
    ///
    /// Not always zero for a call that passes everything in registers: a Windows caller reserves
    /// thirty two bytes for the callee to spill its register arguments into whether it uses them
    /// or not, and that reservation is this.
    pub outgoing: u32,
}

/// Which of a call's values could not be passed, and why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refused {
    /// Its position among the arguments, or `None` for the value that comes back.
    pub argument: Option<usize>,
    /// What is wrong with where it travels.
    pub missing: Missing,
}

/// What a call goes to.
///
/// The whole of the difference between the two calls. Everything else about them, which is what
/// they pass and what comes back and which registers they destroy, is the signature's answer and
/// is the same answer either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Callee {
    /// A name, which the linker resolves.
    Named(Symbol),
    /// An address in a register, which nothing resolves because there is nothing to resolve: the
    /// value is not known until the program runs.
    ///
    /// The register is unconstrained, and it has to be, because every register the convention
    /// does not preserve is one this instruction writes and every register an argument travels in
    /// is spoken for. What is left is the registers the callee has to put back, which is where
    /// the allocator will put the address, and it is the right answer for the same reason it is
    /// the only one.
    Through(mir::Reg),
}

/// One call, as everything about it that is not the function it is being built into.
#[derive(Debug, Clone, Copy)]
pub struct Calling<'a> {
    /// What it calls.
    pub callee: Callee,
    /// What it passes, in the order the signature holds them, which is the order the convention
    /// places them in.
    pub args: &'a [Passing],
    /// What comes back, which is empty for a call that gives nothing back, one type for a value,
    /// and two for a structure small enough to come back in a pair of registers.
    ///
    /// A pair is placed here rather than named by a rule for the reason the arguments are: which
    /// register each half goes in depends on the halves before it, since the two files are walked
    /// separately, and a pattern over a term cannot see them.
    pub returns: &'a [Type],
    /// Whether the callee takes arguments beyond the ones its signature names, which is what says
    /// whether it reads the count of vector registers the call passed arguments in.
    pub variadic: bool,
}

/// Builds one call: what it passes, what comes back, and what it destroys.
///
/// # Errors
///
/// The first value this cannot pass, and why, before anything is written. A call with one is
/// reported rather than compiled, because the alternative is a call that leaves an argument
/// wherever the last one happened to put a register.
///
/// # Panics
///
/// If a call passes two gigabytes of arguments on the stack, which is a distance no offset in a
/// frame can hold and a call no program makes.
pub fn call(
    out: &mut mir::Func,
    block: mir::Block,
    made: &Calling<'_>,
    conv: &CallRegs,
    names: &mut Interner,
) -> Result<Made, Refused> {
    let &Calling { callee, args, returns, variadic } = made;
    // Where everything goes, worked out before anything is built, so that a call this cannot make
    // leaves no half of one behind.
    let mut places = Places::new(conv);
    let mut passed = Vec::with_capacity(args.len());
    // The ones with no register left for them, as the store each of them becomes and how far up
    // the outgoing area it writes. Almost always empty.
    let mut on_stack = Vec::new();
    // How many of them went in vector registers, which is what a SysV variadic callee is told.
    let mut vectors = 0u32;
    // The ones whose bytes travel rather than their address, as the register that address is in,
    // how far up the outgoing area they go and which words the copy is made of. Almost always
    // empty too, and never at the same time as a register: an object in the argument area is in
    // the argument area whatever is left of the register files.
    let mut as_bytes = Vec::new();
    for (index, &Passing { ty, reg, abi }) in args.iter().enumerate() {
        let refused = |missing| Refused { argument: Some(index), missing };
        if let Abi::ByVal { size, align } = abi {
            let size = u32::try_from(size).map_err(|_| refused(Missing::TooBig))?;
            let Where::Stack(up) = places.on_stack(size, align) else {
                unreachable!("an object in the argument area is in the argument area")
            };
            let plan = crate::expand::plan(u64::from(size), align, conv.word)
                .ok_or(refused(Missing::TooBig))?;
            as_bytes.push((reg, up, plan));
            continue;
        }
        let at = if ty.is_float() { places.float() } else { places.integer() };
        if let Some(missing) = refuses(ty) {
            return Err(refused(missing));
        }
        // Windows passes a float to a variadic callee in the vector register and in the general
        // purpose register at the same position, both at once, because the callee has no
        // prototype to tell it which file to look in. Doing that needs to know which arguments are
        // the ones the signature does not name, and a call carries whether the callee is variadic
        // rather than how many arguments it names, so this is turned down rather than passed in
        // one file and read from the other.
        if ty.is_float() && variadic && conv.shared_positions {
            return Err(refused(Missing::InBothFiles));
        }
        let class = class_of(ty, conv);
        match at {
            Where::Reg(at) => {
                // Only a register counts, because the count is of registers. An argument that went
                // to memory is one the callee reads from memory whatever this says.
                if class == conv.sse_class {
                    vectors += 1;
                }
                passed.push((reg, at, class));
            }
            Where::Stack(up) => {
                let store = store_of(ty).ok_or(refused(Missing::Width))?;
                on_stack.push((reg, class, names.intern(store), up));
            }
        }
    }
    let comes_back = places_back(returns, conv)?;

    // A variadic callee on SysV reads how many vector registers the call passed arguments in and
    // skips saving them when the answer is none, which is what makes `printf` with no floating
    // point argument cheap. It is an obligation rather than an optimization: leaving whatever was
    // in the register there makes the callee save a register file it was not given, and a count
    // that is too low makes it read an argument out of a register nothing put one in.
    let counted = if variadic { conv.vector_count } else { None };

    // The arguments that go to memory go there now, in front of the call and after everything this
    // could have refused, so that a call it cannot make leaves no store behind either. The offset
    // is written straight in rather than left for [`crate::finish`]: the outgoing area is at the
    // bottom of the frame because that is where the callee looks for it, and the bottom of the
    // frame is where the stack pointer already is.
    for (reg, class, store, up) in on_stack {
        let sp = mir::Operand::read(mir::Reg::physical(conv.stack_pointer), conv.int_class);
        let up = i32::try_from(up).expect("an argument area under two gigabytes");
        let build = out.build(block, mir::Opcode::new(store));
        build.uses(reg, class).mem(mir::Mem::at(sp).plus(up)).finish();
    }

    // And the objects whose bytes go there, as a load and a store for each word of each of them.
    // This is the copy the caller owes a callee that takes a structure by value: the callee is
    // free to write to what it was handed, so what it was handed cannot be the caller's own copy,
    // and the argument area is where the convention says the caller's copy goes. The words are the
    // same words `crate::expand` would have chosen for a `memcpy` of the same block, because they
    // are chosen by the same function.
    for (from, up, plan) in as_bytes {
        let up = i32::try_from(up).expect("an argument area under two gigabytes");
        for (at, width) in plan {
            let ty = Type::int(width * 8);
            let at = i32::try_from(at).expect("an object under two gigabytes");
            let word = out.new_vreg(conv.int_class);
            let load = names
                .intern(load_of(ty).ok_or(Refused { argument: None, missing: Missing::Width })?);
            let there = mir::Operand::read(from, conv.int_class);
            let build = out.build(block, mir::Opcode::new(load));
            build.def(word, conv.int_class).mem(mir::Mem::at(there).plus(at)).finish();
            let store = names
                .intern(store_of(ty).ok_or(Refused { argument: None, missing: Missing::Width })?);
            let sp = mir::Operand::read(mir::Reg::physical(conv.stack_pointer), conv.int_class);
            let build = out.build(block, mir::Opcode::new(store));
            build.uses(word, conv.int_class).mem(mir::Mem::at(sp).plus(up + at)).finish();
        }
    }

    // The definitions first and the reads after, which is the order every operand vector in the
    // machine IR is in and the order `rucc_mir::defs` counts.
    let mut operands = Vec::with_capacity(args.len() + conv.int_order.len() + 2);
    let results: Vec<mir::Reg> = comes_back
        .iter()
        .map(|&(at, class)| {
            let reg = out.new_vreg(class);
            operands.push(mir::Operand::write(reg, class).with(Constraint::Fixed(at)));
            reg
        })
        .collect();
    // One list per file, because a physical register is a number and the class is what says which
    // file it is a number in. One list would have `xmm0` blocking `rax`.
    let spoken_for = |class: RegClass| -> Vec<PhysReg> {
        comes_back
            .iter()
            .filter(|&&(_, at)| at == class)
            .map(|&(reg, _)| reg)
            .chain(counted.filter(|_| class == conv.int_class))
            .chain(passed.iter().filter(|&&(_, _, at)| at == class).map(|&(_, reg, _)| reg))
            .collect()
    };
    let named = spoken_for(conv.int_class);
    for &reg in conv.int_order {
        if !conv.preserves_int(reg) && !named.contains(&reg) {
            operands.push(mir::Operand::write(mir::Reg::physical(reg), conv.int_class));
        }
    }
    let named = spoken_for(conv.sse_class);
    for &reg in conv.sse_order {
        if !conv.preserves_sse(reg) && !named.contains(&reg) {
            operands.push(mir::Operand::write(mir::Reg::physical(reg), conv.sse_class));
        }
    }
    // The address in front of the arguments, because a call through one is written with the
    // register it goes through and nothing in the operand vector is at a place a table could name.
    // First read is a place that does not depend on the signature, which is what
    // [`rucc_target::x86_64::Arg::Through`] is written against.
    if let Callee::Through(reg) = callee {
        operands.push(mir::Operand::read(reg, conv.int_class));
    }
    for (reg, at, class) in passed {
        operands.push(mir::Operand::read(reg, class).with(Constraint::Fixed(at)));
    }
    if let Some(at) = counted {
        let count = out.new_vreg(conv.int_class);
        let zero = mir::Opcode::new(names.intern("x64.mov_ri_32"));
        out.build(block, zero).def(count, conv.int_class).imm(i64::from(vectors)).finish();
        operands.push(mir::Operand::read(count, conv.int_class).with(Constraint::Fixed(at)));
    }

    let opcode = mir::Opcode::new(names.intern(match callee {
        Callee::Named(_) => CALL,
        Callee::Through(_) => CALL_REG,
    }));
    let mut build = out.build(block, opcode);
    if let Callee::Named(symbol) = callee {
        build = build.symbol(symbol);
    }
    for operand in operands {
        build = build.operand(operand);
    }
    build.finish();
    Ok(Made { results, outgoing: places.size() })
}

/// Which register each value comes back in, walked the way the arguments are.
///
/// The two files are counted separately, because a structure of a `double` and a `long` comes back
/// with the `double` in the first vector register and the `long` in the first integer one, and a
/// single count would put the second half one place further along a list it is not on.
///
/// # Errors
///
/// The first value that cannot come back at all, and why, so that a call this cannot make leaves
/// nothing behind. Nothing here reports which value it was, because the caller has one answer for
/// all of them: the value that comes back is not an argument and has no position among them.
fn places_back(returns: &[Type], conv: &CallRegs) -> Result<Vec<(PhysReg, RegClass)>, Refused> {
    let refused = |missing| Refused { argument: None, missing };
    let mut back = Vec::with_capacity(returns.len());
    let (mut ints, mut sses) = (0usize, 0usize);
    for &ty in returns {
        if let Some(missing) = refuses(ty) {
            return Err(refused(missing));
        }
        let class = class_of(ty, conv);
        let (file, at) = if class == conv.sse_class {
            (conv.sse_returns, &mut sses)
        } else {
            (conv.int_returns, &mut ints)
        };
        let reg = *file.get(*at).ok_or_else(|| refused(Missing::NoRoom))?;
        *at += 1;
        back.push((reg, class));
    }
    Ok(back)
}

/// What the pseudo for an argument of that type is called.
///
/// The width is in the name for the same reason it is in every other opcode here: it is what the
/// instruction is about. Nothing encodes it, so nothing depends on it being right, but a listing
/// that says an argument arrived and does not say how much of it did is a listing worth less.
///
/// Which widths there are is the question the rule set asks of a type, and not a list of its own,
/// because it has to be the same list. An argument brought in at a width the rules have no name
/// for is a register
/// nothing downstream could then read, and a width the rules cover that this refuses is a
/// function turned away for no reason. Asking one question in one place is what keeps the two
/// answers from drifting, and an address is what they used to disagree about.
#[must_use]
pub fn head_of(ty: Type) -> Option<&'static str> {
    if let Some(at) = crate::term::float_slot(ty) {
        return Some(["x64.arg_val_f32", "x64.arg_val_f64"][at]);
    }
    let names = ["x64.arg_val_8", "x64.arg_val_16", "x64.arg_val_32", "x64.arg_val_64"];
    Some(names[crate::term::slot(ty)?])
}

/// What the instruction that reads an argument of that type out of memory is called.
///
/// Keyed off the same two questions [`head_of`] asks and answering for the same set of types, so
/// that a parameter this compiler can bring in from a register is one it can bring in from the
/// caller's stack as well. A width one of them covered and the other did not would be a function
/// turned away for where its sixth argument happened to land.
///
/// Reading a narrow argument at its own width and not at a word is deliberate. The caller wrote a
/// whole word, but what it put in the part above the value is not something the convention says, so
/// the bits this reads are exactly the bits that mean anything. That is the same thing an argument
/// arriving in a register gets: `x64.arg_val_8` says the low byte of that register is the argument
/// and says nothing at all about the rest of it.
#[must_use]
pub fn load_of(ty: Type) -> Option<&'static str> {
    if let Some(at) = crate::term::float_slot(ty) {
        return Some(["x64.movss_rm", "x64.movsd_rm"][at]);
    }
    let names = ["x64.mov_rm_8", "x64.mov_rm_16", "x64.mov_rm_32", "x64.mov_rm_64"];
    Some(names[crate::term::slot(ty)?])
}

/// What the instruction that writes an argument of that type into memory is called.
///
/// The mirror of [`load_of`], keyed off the same two questions and answering for the same set of
/// types, so that the two ends of one call agree about what travels. A type a callee can read out
/// of the argument area and a caller cannot write into it would be a call turned away for a reason
/// the function it calls does not have.
///
/// Writing a narrow argument at its own width leaves whatever was already in the rest of the word.
/// That is allowed, and it is what [`load_of`] is written against: the convention does not say what
/// is above the value, so the callee reads only the bits that mean anything and neither end has to
/// agree about the rest.
#[must_use]
pub fn store_of(ty: Type) -> Option<&'static str> {
    if let Some(at) = crate::term::float_slot(ty) {
        return Some(["x64.movss_mr", "x64.movsd_mr"][at]);
    }
    let names = ["x64.mov_mr_8", "x64.mov_mr_16", "x64.mov_mr_32", "x64.mov_mr_64"];
    Some(names[crate::term::slot(ty)?])
}

/// What the instruction that leaves a returned value in its register is called, for the value at
/// that place in its own register file.
///
/// Keyed off the same two questions [`head_of`] asks, so a type this can give back is a type it can
/// take in. The place is the one a call counted to when it laid the return out, which is per file
/// rather than over the whole list: a structure of a `double` and a `long` gives both of them back
/// at place zero.
///
/// The register itself is not here. It is in the operand table in `rucc_target::x86_64`, which is
/// where the first one has always been, and the two names below are how a value says which of the
/// two it is. A convention with more than two registers to come back in would need more names, and
/// there is none, which is what the `None` at the end is about.
#[must_use]
pub fn ret_of(ty: Type, at: usize) -> Option<&'static str> {
    if let Some(width) = crate::term::float_slot(ty) {
        let names =
            [["x64.ret_val_f32", "x64.ret_val_f64"], ["x64.ret_val2_f32", "x64.ret_val2_f64"]];
        return Some(names.get(at)?[width]);
    }
    let names = [
        ["x64.ret_val_8", "x64.ret_val_16", "x64.ret_val_32", "x64.ret_val_64"],
        ["x64.ret_val2_8", "x64.ret_val2_16", "x64.ret_val2_32", "x64.ret_val2_64"],
    ];
    Some(names.get(at)?[crate::term::slot(ty)?])
}

#[cfg(test)]
mod tests {
    use rucc_target::x86_64::{REGS, SYSV, WIN64};

    use super::*;

    /// Those types as parameters that travel as the values they are, which is every one of them
    /// that is not a structure the classification put in the argument area.
    fn plain(params: &[Type]) -> Vec<Param> {
        params.iter().copied().map(Param::new).collect()
    }

    /// The parameters of a function under a convention, as machine IR text.
    fn bind(params: &[Type], conv: &CallRegs) -> String {
        let mut names = Interner::new();
        let mut out = mir::Func::new(names.intern("f"));
        let block = out.create_block();
        entry(&mut out, block, &plain(params), conv, &mut names, None)
            .expect("every parameter arrives");
        mir::print_func(&out, &names, &REGS)
    }

    #[test]
    fn the_first_arguments_arrive_where_the_convention_puts_them() {
        let i32 = Type::int(32);
        assert_eq!(
            bind(&[i32, i32, Type::int(64)], &SYSV),
            "mfunc @f {\nblock0:\n    %0:gpr($rdi) = x64.arg_val_32\n    \
             %1:gpr($rsi) = x64.arg_val_32\n    %2:gpr($rdx) = x64.arg_val_64\n}\n"
        );
    }

    #[test]
    fn the_other_convention_puts_the_same_arguments_somewhere_else() {
        // The first argument is in `rcx` here and in `rdi` above, which is the difference that
        // makes a SysV binary calling a Windows one read the wrong value rather than fail.
        let i64 = Type::int(64);
        assert_eq!(
            bind(&[i64, i64], &WIN64),
            "mfunc @f {\nblock0:\n    %0:gpr($rcx) = x64.arg_val_64\n    \
             %1:gpr($rdx) = x64.arg_val_64\n}\n"
        );
    }

    /// The parameters of a function under a convention, and what each of the ones that arrived in
    /// memory is waiting on.
    fn arrive(params: &[Type], conv: &CallRegs) -> (String, Vec<u32>) {
        let mut names = Interner::new();
        let mut out = mir::Func::new(names.intern("f"));
        let block = out.create_block();
        let arrived = entry(&mut out, block, &plain(params), conv, &mut names, None)
            .expect("every parameter");
        let up = arrived.stack.iter().map(|&(_, up)| up).collect();
        (mir::print_func(&out, &names, &REGS), up)
    }

    #[test]
    fn an_argument_past_the_last_register_is_read_out_of_the_caller_s_stack() {
        let (text, up) = arrive(&[Type::int(64); 7], &SYSV);

        // Six of them got registers and the seventh did not, so the seventh is a load rather than
        // a pseudo. It reads through the stack pointer with nothing in its displacement, because
        // where the caller's argument area is from in here is a distance into a frame that does
        // not exist yet, and it is at the bottom of that area because it is the first one in it.
        assert_eq!(up, [0]);
        assert!(text.contains("%6:gpr = x64.mov_rm_64 [$rsp]"), "{text}");
        assert_eq!(text.matches("x64.arg_val_64").count(), 6, "{text}");
    }

    #[test]
    fn the_other_convention_runs_out_of_registers_three_arguments_earlier() {
        let (text, up) = arrive(&[Type::int(64); 7], &WIN64);

        // Windows passes four integers in registers and reserves thirty two bytes below the call
        // whether they are used or not, so the fifth argument is not at the bottom of the argument
        // area but above the shadow space, and the three after it follow it a word at a time.
        assert_eq!(up, [32, 40, 48]);
        assert_eq!(text.matches("x64.arg_val_64").count(), 4, "{text}");
        assert!(text.contains("%4:gpr = x64.mov_rm_64 [$rsp]"), "{text}");
    }

    /// The parameters of a function under a convention, with one of them a structure whose bytes
    /// travel, and what each of the ones that arrived in memory is waiting on.
    fn arrive_with(params: &[Param], conv: &CallRegs) -> (String, Vec<u32>) {
        let mut names = Interner::new();
        let mut out = mir::Func::new(names.intern("f"));
        let block = out.create_block();
        let arrived =
            entry(&mut out, block, params, conv, &mut names, None).expect("every parameter");
        let up = arrived.stack.iter().map(|&(_, up)| up).collect();
        (mir::print_func(&out, &names, &REGS), up)
    }

    #[test]
    fn a_structure_that_arrived_as_bytes_is_an_address_and_not_a_load() {
        let byval = Param::with_abi(Type::PTR, Abi::ByVal { size: 32, align: 8 });
        let (text, up) =
            arrive_with(&[Param::new(Type::int(32)), byval, Param::new(Type::int(32))], &SYSV);

        // `int f(int a, struct Big b, int c)`. The bytes of `b` are already in this function, at
        // the bottom of the caller's argument area, so nothing is read out of them here: what the
        // parameter is is where they are, which is one address. The two integers still travel in
        // registers, because an object in the argument area takes no register and the arguments
        // behind it do not shift along.
        assert_eq!(up, [0]);
        assert_eq!(text.matches("x64.arg_val_32").count(), 2, "{text}");
        assert!(text.contains("%2:gpr = x64.lea_64 [$rsp]"), "{text}");
        assert!(!text.contains("mov_rm"), "nothing is read out of the bytes: {text}");
    }

    #[test]
    fn the_argument_behind_a_structure_that_travelled_as_bytes_is_above_all_of_them() {
        let byval = Param::with_abi(Type::PTR, Abi::ByVal { size: 24, align: 16 });
        let params: Vec<Param> = (0..7).map(|_| Param::new(Type::int(64))).collect();
        let (_, up) = arrive_with(&[&params[..], &[byval], &params[..1]].concat(), &SYSV);

        // Six integers take the six registers, the seventh is at the bottom of the argument area,
        // and the structure is above it at the alignment its type asks for rather than at a word.
        // The one behind the structure is above all twenty four of its bytes, rounded up to a
        // whole number of words, because the area is a run of words.
        assert_eq!(up, [0, 16, 40]);
    }

    /// A parameter narrower than a word is read at its own width rather than at a word, and one in
    /// the other register file is read with the other file's instruction. Both are the same list
    /// [`head_of`] answers from, which is what stops a function being turned away for the width of
    /// its seventh argument alone.
    #[test]
    fn what_a_stack_argument_is_read_with_is_its_own_width_and_its_own_file() {
        let f32 = Type::float(rucc_ir::Float::F32);
        let params = [Type::int(64), Type::int(64), Type::int(64), Type::int(64), Type::int(8)];
        let (text, up) = arrive(&params, &WIN64);
        assert_eq!(up, [32]);
        assert!(text.contains("x64.mov_rm_8 [$rsp]"), "{text}");

        let floats = [f32; 5];
        let (text, up) = arrive(&floats, &WIN64);
        assert_eq!(up, [32]);
        assert!(text.contains("%4:xmm = x64.movss_rm [$rsp]"), "{text}");
    }

    /// Every type a parameter can arrive in a register at is one it can be read from memory at.
    /// The two lists are keyed off the same two questions so that they cannot drift, and this is
    /// what says so: a width one covered and the other did not would be a function turned away for
    /// where its arguments happened to land rather than for anything about it.
    #[test]
    fn the_two_lists_of_widths_answer_for_the_same_types() {
        let types = [
            Type::int(1),
            Type::int(8),
            Type::int(16),
            Type::int(32),
            Type::int(64),
            Type::int(128),
            Type::PTR,
            Type::float(rucc_ir::Float::F32),
            Type::float(rucc_ir::Float::F64),
            Type::float(rucc_ir::Float::F80),
        ];
        for ty in types {
            assert_eq!(head_of(ty).is_some(), load_of(ty).is_some(), "{ty:?}");
        }
    }

    /// A float arrives in the other file, and the two files are counted apart on SysV: the
    /// integer here is the first integer argument and the float is the first float one, so they
    /// are in `rdi` and `xmm0` rather than in the first and second of anything.
    #[test]
    fn a_float_arrives_in_a_vector_register_and_is_counted_apart_from_the_integers() {
        let f32 = Type::float(rucc_ir::Float::F32);
        let f64 = Type::float(rucc_ir::Float::F64);
        assert_eq!(
            bind(&[Type::int(32), f64, f32], &SYSV),
            "mfunc @f {\nblock0:\n    %0:gpr($rdi) = x64.arg_val_32\n    \
             %1:xmm($xmm0) = x64.arg_val_f64\n    %2:xmm($xmm1) = x64.arg_val_f32\n}\n"
        );
    }

    /// Windows counts the two files together, so the same three arguments land in different
    /// registers: the float is the second argument and takes the second vector register rather
    /// than the first, which is the difference that makes a mismatched call read the wrong value.
    #[test]
    fn the_other_convention_counts_the_two_files_as_one_run_of_positions() {
        let f64 = Type::float(rucc_ir::Float::F64);
        assert_eq!(
            bind(&[Type::int(32), f64, Type::int(64)], &WIN64),
            "mfunc @f {\nblock0:\n    %0:gpr($rcx) = x64.arg_val_32\n    \
             %1:xmm($xmm1) = x64.arg_val_f64\n    %2:gpr($r8) = x64.arg_val_64\n}\n"
        );
    }

    /// A `long double` is in neither file, and what it is turned away for says so rather than
    /// calling eighty bits a width no register holds. The x87 stack is a register file this
    /// compiler does not allocate in and has no instruction for.
    #[test]
    fn a_long_double_is_reported_as_the_x87_stack_it_travels_on() {
        let mut names = Interner::new();
        let mut out = mir::Func::new(names.intern("f"));
        let block = out.create_block();
        let params = [Type::int(32), Type::float(rucc_ir::Float::F80)];
        let made = entry(&mut out, block, &plain(&params), &SYSV, &mut names, None);
        assert_eq!(made, Err((1, Missing::OnX87)));
        assert_eq!(
            make(&[], &[Type::float(rucc_ir::Float::F80)], false, &SYSV).2,
            Err(Refused { argument: None, missing: Missing::OnX87 })
        );
    }

    /// One call to `g`, with a register for each argument arriving in the block that makes it.
    fn make(
        args: &[Type],
        returns: &[Type],
        variadic: bool,
        conv: &CallRegs,
    ) -> (Interner, mir::Func, Result<Made, Refused>) {
        let mut names = Interner::new();
        let mut out = mir::Func::new(names.intern("f"));
        let block = out.create_block();
        let passed: Vec<Passing> = args
            .iter()
            .map(|&ty| Passing {
                ty,
                reg: out.append_param(block, class_of(ty, conv)),
                abi: Abi::Plain,
            })
            .collect();
        let callee = Callee::Named(names.intern("g"));
        let what = Calling { callee, args: &passed, returns, variadic };
        let made = call(&mut out, block, &what, conv, &mut names);
        (names, out, made)
    }

    /// What the call in that function reads and writes, by register name, in the order the
    /// operands are in.
    fn operands(func: &mir::Func) -> (Vec<String>, Vec<String>) {
        let block = func.entry().expect("a function with a block in it");
        let call = func.terminator(block).expect("the call is the last thing in the block");
        let name = |operand: &mir::Operand| match (operand.reg.phys(), operand.constraint) {
            (Some(reg), _) | (None, Constraint::Fixed(reg)) => {
                REGS.name(operand.class, reg).expect("a register the file describes").to_string()
            }
            _ => format!("{:?}", operand.reg),
        };
        let mut written = Vec::new();
        let mut read = Vec::new();
        for operand in &func[func[call].operands] {
            let into = if operand.role == mir::Role::Use { &mut read } else { &mut written };
            into.push(name(operand));
        }
        (written, read)
    }

    #[test]
    fn a_call_passes_its_arguments_where_the_convention_puts_them() {
        let i32 = Type::int(32);
        let (_, func, made) = make(&[i32, i32, i32], &[], false, &SYSV);
        assert_eq!(made.expect("three integers all fit in registers").results, []);
        assert_eq!(operands(&func).1, ["rdi", "rsi", "rdx"]);
    }

    #[test]
    fn the_other_convention_passes_the_same_arguments_somewhere_else() {
        let i64 = Type::int(64);
        let (_, func, made) = make(&[i64, i64], &[], false, &WIN64);
        // Thirty two bytes of stack for a call that passes nothing on the stack, which is what
        // Windows asks a caller to leave the callee whether the callee uses it or not.
        assert_eq!(made.expect("two integers fit in registers").outgoing, 32);
        assert_eq!(operands(&func).1, ["rcx", "rdx"]);
    }

    #[test]
    fn what_a_call_gives_back_comes_out_of_the_register_the_convention_returns_in() {
        let (names, func, made) = make(&[], &[Type::int(32)], false, &SYSV);
        let made = made.expect("an integer comes back");
        let [result] = made.results[..] else { panic!("one register") };
        // The first thing written is the result, and it is the only thing written that is a value
        // rather than a register the callee destroyed.
        assert_eq!(operands(&func).0.first().map(String::as_str), Some("rax"));
        assert_eq!(func.class_of(result), Some(SYSV.int_class));
        assert!(mir::print_func(&func, &names, &REGS).contains("x64.call"));
    }

    #[test]
    fn every_register_the_callee_may_destroy_is_written_by_the_call() {
        let (_, func, _) = make(&[Type::int(64)], &[Type::int(64)], false, &SYSV);
        let (written, read) = operands(&func);
        // The callee saved registers are not here, because a value in one of those survives a
        // call and that is the whole difference between the two halves of the convention.
        for saved in ["rbx", "rbp", "r12", "r13", "r14", "r15"] {
            assert!(!written.contains(&saved.to_string()), "{saved} survives a call");
        }
        // Every other integer register is, once. The two named ones are named by the result and
        // by the argument instead, and naming one twice would be blocking it twice.
        for destroyed in ["rcx", "rdx", "rsi", "r8", "r9", "r10", "r11"] {
            let count = written.iter().filter(|name| *name == destroyed).count();
            assert_eq!(count, 1, "{destroyed} is destroyed by a call and is written {count} times");
        }
        assert_eq!(written.iter().filter(|name| *name == "rax").count(), 1);
        assert_eq!(read, ["rdi"]);
        // The vector registers are all destroyed on SysV, and they are in the other class.
        assert!(written.contains(&"xmm0".to_string()));
    }

    #[test]
    fn a_variadic_call_says_how_many_vector_registers_it_passed_arguments_in() {
        let (names, func, made) = make(&[Type::int(64)], &[], true, &SYSV);
        made.expect("an integer argument to a variadic callee");
        let (_, read) = operands(&func);
        // Zero of them here, and `al` is where a SysV callee looks for it. Leaving whatever was in
        // the register there would make a callee that saves its vector registers save ones it was
        // never given.
        assert_eq!(read, ["rdi", "rax"]);
        assert_eq!(
            mir::print_func(&func, &names, &REGS).lines().nth(2),
            Some("    %1:gpr = x64.mov_ri_32 0")
        );

        // Two of them here, which is the number that decides how much of the register save area a
        // callee like `printf` fills in. A count of zero with a float in `xmm0` would be a callee
        // reading its first `%f` out of a register nothing wrote.
        let f64 = Type::float(rucc_ir::Float::F64);
        let (names, func, made) = make(&[Type::int(64), f64, f64], &[], true, &SYSV);
        made.expect("one integer and two floats all fit in registers");
        assert_eq!(operands(&func).1, ["rdi", "xmm0", "xmm1", "rax"]);
        assert!(mir::print_func(&func, &names, &REGS).contains("x64.mov_ri_32 2"));
    }

    /// Windows passes a float to a variadic callee in both files at once, and which arguments are
    /// the ones the signature does not name is not something a call carries, so it is turned down
    /// rather than passed in one file and read from the other.
    #[test]
    fn a_float_passed_to_a_variadic_callee_on_windows_is_reported() {
        let f64 = Type::float(rucc_ir::Float::F64);
        assert_eq!(
            make(&[Type::int(32), f64], &[], true, &WIN64).2,
            Err(Refused { argument: Some(1), missing: Missing::InBothFiles })
        );
        // The same call to a callee whose signature names both arguments is fine, because there is
        // no second copy to make.
        assert!(make(&[Type::int(32), f64], &[], false, &WIN64).2.is_ok());
    }

    #[test]
    fn a_call_through_an_address_reads_it_in_front_of_the_arguments() {
        let i32 = Type::int(32);
        let mut names = Interner::new();
        let mut out = mir::Func::new(names.intern("f"));
        let block = out.create_block();
        let address = out.append_param(block, SYSV.int_class);
        let reg = out.append_param(block, SYSV.int_class);
        let passed = vec![Passing { ty: i32, reg, abi: Abi::Plain }];
        let what = Calling {
            callee: Callee::Through(address),
            args: &passed,
            returns: &[i32],
            variadic: false,
        };
        call(&mut out, block, &what, &SYSV, &mut names).expect("one integer fits in a register");

        // The address is the first thing read and the arguments follow it, which is the order the
        // assembler counts on, and it is in no particular register because every register a call
        // could insist on is one the call has already spoken for.
        let text = mir::print_func(&out, &names, &REGS);
        assert!(text.contains("= x64.call_reg %0, %1($rdi)\n"), "{text}");
        assert!(!text.contains("@g"), "a call through an address names nobody: {text}");
    }

    #[test]
    fn a_call_with_no_register_left_writes_the_argument_into_the_outgoing_area() {
        let i64 = Type::int(64);
        let (names, func, made) = make(&[i64; 7], &[], false, &SYSV);
        let made = made.expect("the seventh goes to memory");

        // At the stack pointer, because the outgoing area is at the bottom of the frame, and in
        // front of the call rather than as an operand of it.
        let text = mir::print_func(&func, &names, &REGS);
        assert!(text.contains("x64.mov_mr_64 %6, [$rsp]\n"), "{text}");
        let store = text.find("x64.mov_mr_64").expect("the store");
        assert!(store < text.find("x64.call").expect("the call"), "{text}");
        // One word of it, which is what the frame has to reserve for this call.
        assert_eq!(made.outgoing, 8);
    }

    /// One call to `g`, passing that many words, then an object of that size and alignment by
    /// value, then one more integer, which is `int g(long.., struct Big, int)` after the
    /// classification.
    fn pass_bytes(
        before: usize,
        size: u64,
        align: u32,
        conv: &CallRegs,
    ) -> (Interner, mir::Func, Result<Made, Refused>) {
        let mut names = Interner::new();
        let mut out = mir::Func::new(names.intern("f"));
        let block = out.create_block();
        let mut args: Vec<Passing> = (0..before)
            .map(|_| Passing {
                ty: Type::int(64),
                reg: out.append_param(block, conv.int_class),
                abi: Abi::Plain,
            })
            .collect();
        args.push(Passing {
            ty: Type::PTR,
            reg: out.append_param(block, conv.int_class),
            abi: Abi::ByVal { size, align },
        });
        args.push(Passing {
            ty: Type::int(32),
            reg: out.append_param(block, conv.int_class),
            abi: Abi::Plain,
        });
        let callee = Callee::Named(names.intern("g"));
        let what = Calling { callee, args: &args, returns: &[], variadic: false };
        let made = call(&mut out, block, &what, conv, &mut names);
        (names, out, made)
    }

    #[test]
    fn a_structure_passed_by_value_in_memory_is_copied_into_the_outgoing_area() {
        let (names, func, made) = pass_bytes(1, 24, 8, &SYSV);
        let made = made.expect("an object of three words is copied a word at a time");

        // The bytes travel and the address does not, so the copy is a load and a store for each
        // word of it, in front of the call, and the callee's copy is at the bottom of the outgoing
        // area. The caller owes it this copy: the callee is free to write to what it was handed,
        // so what it was handed cannot be the object itself.
        let text = mir::print_func(&func, &names, &REGS);
        assert!(text.contains("x64.mov_mr_64 %3, [$rsp]\n"), "{text}");
        assert!(text.contains("x64.mov_mr_64 %4, [$rsp + 8]\n"), "{text}");
        assert!(text.contains("x64.mov_mr_64 %5, [$rsp + 16]\n"), "{text}");
        assert_eq!(text.matches("x64.mov_rm_64").count(), 3, "{text}");
        assert!(text.find("x64.mov_mr_64") < text.find("x64.call"), "{text}");
        assert_eq!(made.outgoing, 24);
    }

    #[test]
    fn the_integers_beside_it_still_travel_in_registers() {
        let (_, func, _) = pass_bytes(1, 24, 8, &SYSV);

        // An object in the argument area takes no argument register, so the integer behind it is
        // in the second one and not the third. Counting it as a register is the mistake that would
        // shift every argument after it along by one.
        let (clobbered, read) = operands(&func);
        assert_eq!(read, ["rdi", "rsi"]);
        assert!(clobbered.contains(&"rdx".to_owned()), "the third is free: {clobbered:?}");
    }

    #[test]
    fn an_object_wanting_more_alignment_than_a_word_gets_it() {
        let (names, func, made) = pass_bytes(7, 24, 16, &SYSV);
        let made = made.expect("an object of three words");

        // Six of the integers took the registers and the seventh is at the bottom of the area, so
        // the object cannot start where it left off: sixteen byte alignment moves it up to the
        // next multiple of sixteen and leaves a word of nothing behind it. The integer after it is
        // above all three of its words.
        let text = mir::print_func(&func, &names, &REGS);
        assert!(text.contains("x64.mov_mr_64 %9, [$rsp + 16]\n"), "{text}");
        assert!(text.contains("x64.mov_mr_32 %8, [$rsp + 40]\n"), "{text}");
        assert_eq!(made.outgoing, 48);
    }

    #[test]
    fn an_object_too_large_to_copy_a_word_at_a_time_is_reported_rather_than_passed() {
        let (_, _, made) = pass_bytes(1, 4096, 8, &SYSV);

        // Five hundred and twelve words is past what unrolling is worth, and the copy that size
        // wants is a call to the runtime, which cannot be built in the middle of building a call.
        // Saying so is the point: the alternative is a call that passes the address of the object
        // where the callee is going to read the object.
        assert_eq!(made, Err(Refused { argument: Some(1), missing: Missing::TooBig }));
        assert_eq!(
            Missing::TooBig.why(),
            "is more bytes than a copy into the argument area unrolls to"
        );
    }

    /// The other convention runs out three arguments earlier and starts its argument area above the
    /// shadow space it also has to reserve, and both of those are what `Places` already said.
    #[test]
    fn where_the_outgoing_area_starts_is_the_convention_s_answer() {
        let i64 = Type::int(64);
        let (names, func, made) = make(&[i64; 7], &[], false, &WIN64);
        assert_eq!(made.expect("the last three go to memory").outgoing, 56);

        // Thirty two bytes of shadow space first, which the caller writes nothing into and the
        // callee owns, and the fifth argument above it.
        let text = mir::print_func(&func, &names, &REGS);
        assert!(text.contains("x64.mov_mr_64 %4, [$rsp + 32]\n"), "{text}");
        assert!(text.contains("x64.mov_mr_64 %5, [$rsp + 40]\n"), "{text}");
        assert!(text.contains("x64.mov_mr_64 %6, [$rsp + 48]\n"), "{text}");
    }

    /// What a stack argument is written with is its own width and its own register file, matching
    /// what the callee reads it back with.
    #[test]
    fn a_narrow_or_floating_argument_keeps_its_own_store() {
        let i64 = Type::int(64);
        let narrow = [i64, i64, i64, i64, i64, i64, Type::int(8)];
        let (names, func, made) = make(&narrow, &[], false, &SYSV);
        made.expect("the seventh goes to memory");
        let text = mir::print_func(&func, &names, &REGS);
        assert!(text.contains("x64.mov_mr_8 %6, [$rsp]\n"), "{text}");

        let f32 = Type::float(rucc_ir::Float::F32);
        let (names, func, made) = make(&[f32; 9], &[], false, &SYSV);
        made.expect("the ninth goes to memory");
        let text = mir::print_func(&func, &names, &REGS);
        assert!(text.contains("x64.movss_mr %8, [$rsp]\n"), "{text}");
    }

    /// The count a SysV variadic callee reads is a count of registers, so an argument that went to
    /// memory instead is not in it.
    #[test]
    fn an_argument_in_memory_is_not_counted_as_a_vector_register() {
        let f64 = Type::float(rucc_ir::Float::F64);
        let (names, func, made) = make(&[f64; 9], &[], true, &SYSV);
        made.expect("the ninth goes to memory");
        let text = mir::print_func(&func, &names, &REGS);
        assert!(text.contains("x64.mov_ri_32 8\n"), "eight registers, not nine: {text}");
    }

    /// The two lists of widths answer for the same set of types, so that a value the callee can
    /// read out of the argument area is one the caller can write into it.
    #[test]
    fn what_can_be_read_can_be_written() {
        let types = [
            Type::int(1),
            Type::int(8),
            Type::int(16),
            Type::int(32),
            Type::int(64),
            Type::int(128),
            Type::PTR,
            Type::float(rucc_ir::Float::F32),
            Type::float(rucc_ir::Float::F64),
            Type::float(rucc_ir::Float::F80),
        ];
        for ty in types {
            assert_eq!(load_of(ty).is_some(), store_of(ty).is_some(), "{ty:?}");
        }
    }

    /// A float travels in the other file at both ends of a call, and the register it comes back in
    /// is the first of that file rather than the first of the other one.
    #[test]
    fn a_call_passes_and_returns_a_float_in_a_vector_register() {
        let f64 = Type::float(rucc_ir::Float::F64);
        let (_, func, made) = make(&[Type::int(32), f64], &[f64], false, &SYSV);
        let result = made.expect("an integer and a float both fit in registers");
        let (written, read) = operands(&func);
        assert_eq!(read, ["rdi", "xmm0"]);
        assert_eq!(written.first().map(String::as_str), Some("xmm0"));
        assert_eq!(func.class_of(result.results[0]), Some(SYSV.sse_class));
        // Written once, because the register the result comes back in is already blocked by being
        // named and a clobber that repeated it would be blocking it twice. `rax` is a clobber here
        // rather than the result, which is the same register number in the other file and is the
        // whole reason the two lists are counted apart.
        assert_eq!(written.iter().filter(|name| *name == "xmm0").count(), 1);
        assert!(written.contains(&"rax".to_string()));
    }

    #[test]
    fn a_call_at_a_width_no_register_holds_is_reported_on_either_side() {
        let i128 = Type::int(128);
        assert_eq!(
            make(&[i128], &[], false, &SYSV).2,
            Err(Refused { argument: Some(0), missing: Missing::Width })
        );
        assert_eq!(
            make(&[], &[i128], false, &SYSV).2,
            Err(Refused { argument: None, missing: Missing::Width })
        );
    }

    #[test]
    fn an_argument_wider_than_a_register_has_no_name() {
        assert_eq!(head_of(Type::int(128)), None);
        assert_eq!(head_of(Type::int(8)), Some("x64.arg_val_8"));
        assert_eq!(head_of(Type::int(64)), Some("x64.arg_val_64"));
    }

    /// An address arrives in a general purpose register like any other integer of its width, and
    /// used to be turned away here as a width no register holds, which is what issue 274 is.
    /// `int g(char *s)` is the smallest program that was.
    #[test]
    fn an_address_arrives_in_a_register_like_the_integer_it_is() {
        assert_eq!(head_of(Type::PTR), Some("x64.arg_val_64"));
        assert_eq!(
            bind(&[Type::PTR], &SYSV),
            "mfunc @f {\nblock0:\n    %0:gpr($rdi) = x64.arg_val_64\n}\n"
        );
        // And it travels the same way at a call, on both sides of one.
        assert!(make(&[Type::PTR], &[Type::PTR], false, &SYSV).2.is_ok());
    }
}
