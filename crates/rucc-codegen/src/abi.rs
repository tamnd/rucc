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
//! function reaches this crate, which is why the parameters read here are plain scalars: an
//! aggregate has been split into the pieces it travels in, and a return through memory is an
//! ordinary pointer parameter in front of the rest. What is left for this is the step after
//! classification, from how a value travels to which register it is actually in, which is
//! [`rucc_target::Places`].
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
//! What is not here is the bytes an argument past the last register goes in. That is the outgoing
//! half of the same job the incoming half above does, and it is harder, because a store into the
//! outgoing area has to happen before the call and the area is only reserved once every call in the
//! function has been seen. So a call is asked how many bytes it would need and reports it, and a
//! call that would need any is turned down for now.

use rucc_base::{Interner, Symbol};
use rucc_ir::Type;
use rucc_mir as mir;
use rucc_target::{CallRegs, Constraint, PhysReg, Places, RegClass, Where};

/// Why a parameter could not be brought in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Missing {
    /// It travels on the stack, which a call cannot put it on yet: the outgoing argument area is
    /// only as big as the widest call in the function, and how wide that is is not known until
    /// every call has been lowered, which is after the first of them has been written.
    ///
    /// A parameter arriving on the stack is not this. That one is read, in [`entry`].
    OnStack,
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
}

impl Missing {
    /// What it says when a function could not be compiled because of it.
    ///
    /// Worded so that it reads the same about a value arriving and a value being passed, since
    /// the two are the same fact seen from the two ends of one call.
    #[must_use]
    pub fn why(self) -> &'static str {
        match self {
            Missing::OnStack => "is passed on the stack",
            Missing::OnX87 => "is on the x87 stack",
            Missing::InBothFiles => "is a float passed to a variadic callee on this convention",
            Missing::Width => "is a width no argument register holds",
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
/// same answer about the same type.
fn refuses(ty: Type) -> Option<Missing> {
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
    params: &[Type],
    conv: &CallRegs,
    names: &mut Interner,
) -> Result<Arrived, (usize, Missing)> {
    let mut places = Places::new(conv);
    let mut arrived = Arrived { regs: Vec::with_capacity(params.len()), stack: Vec::new() };
    for (index, &ty) in params.iter().enumerate() {
        // Asking for the place of a parameter that cannot be brought in is still worth doing
        // before giving up, and it costs nothing, because every place after it depends on it and
        // a reader stepping through this in a debugger should see the same numbers a working
        // version would.
        let at = if ty.is_float() { places.float() } else { places.integer() };
        if let Some(missing) = refuses(ty) {
            return Err((index, missing));
        }

        let class = class_of(ty, conv);
        let reg = out.new_vreg(class);
        match at {
            Where::Reg(arrived_in) => {
                let head = head_of(ty).ok_or((index, Missing::Width))?;
                let opcode = mir::Opcode::new(names.intern(head));
                let operand = mir::Operand::write(reg, class).with(Constraint::Fixed(arrived_in));
                out.build(block, opcode).operand(operand).finish();
            }
            // The stack pointer is written down as the register to read through because it is the
            // one that reaches the caller's stack in almost every function, and a realigned frame
            // is the exception that [`crate::finish`] rewrites. Putting something here rather than
            // nothing keeps the instruction printable and verifiable in between.
            Where::Stack(up) => {
                let load = load_of(ty).ok_or((index, Missing::Width))?;
                let opcode = mir::Opcode::new(names.intern(load));
                let sp = mir::Operand::read(mir::Reg::physical(conv.stack_pointer), conv.int_class);
                let made = out.build(block, opcode).def(reg, class).mem(mir::Mem::at(sp)).finish();
                arrived.stack.push((made, up));
            }
        }
        arrived.regs.push(reg);
    }
    Ok(arrived)
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

/// What one call came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Made {
    /// The register the value came back in, or `None` for a call that gives nothing back.
    pub result: Option<mir::Reg>,
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
    /// What it passes, as the type each value travels as and the register it is in, in the order
    /// the signature holds them, which is the order the convention places them in.
    pub args: &'a [(Type, mir::Reg)],
    /// What comes back, or `None` for a call that gives nothing back.
    pub returns: Option<Type>,
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
    // How many of them went in vector registers, which is what a SysV variadic callee is told.
    let mut vectors = 0u32;
    for (index, &(ty, reg)) in args.iter().enumerate() {
        let refused = |missing| Refused { argument: Some(index), missing };
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
        let Where::Reg(at) = at else { return Err(refused(Missing::OnStack)) };
        let class = class_of(ty, conv);
        if class == conv.sse_class {
            vectors += 1;
        }
        passed.push((reg, at, class));
    }
    let comes_back = match returns {
        None => None,
        Some(ty) if refuses(ty).is_some() => {
            return Err(Refused { argument: None, missing: refuses(ty).unwrap_or(Missing::Width) });
        }
        // Which register a value comes back in depends on nothing but the value, which is why the
        // return side of the convention is a rule and this side is not. There is no rule here
        // because the arguments are in the same instruction.
        Some(ty) => {
            let class = class_of(ty, conv);
            let file = if class == conv.sse_class { conv.sse_returns } else { conv.int_returns };
            let at = *file.first().ok_or(Refused { argument: None, missing: Missing::Width })?;
            Some((at, class))
        }
    };

    // A variadic callee on SysV reads how many vector registers the call passed arguments in and
    // skips saving them when the answer is none, which is what makes `printf` with no floating
    // point argument cheap. It is an obligation rather than an optimization: leaving whatever was
    // in the register there makes the callee save a register file it was not given, and a count
    // that is too low makes it read an argument out of a register nothing put one in.
    let counted = if variadic { conv.vector_count } else { None };

    // The definitions first and the reads after, which is the order every operand vector in the
    // machine IR is in and the order `rucc_mir::defs` counts.
    let mut operands = Vec::with_capacity(args.len() + conv.int_order.len() + 2);
    let result = comes_back.map(|(at, class)| {
        let reg = out.new_vreg(class);
        operands.push(mir::Operand::write(reg, class).with(Constraint::Fixed(at)));
        reg
    });
    // One list per file, because a physical register is a number and the class is what says which
    // file it is a number in. One list would have `xmm0` blocking `rax`.
    let spoken_for = |class: RegClass| -> Vec<PhysReg> {
        comes_back
            .filter(|&(_, at)| at == class)
            .map(|(reg, _)| reg)
            .into_iter()
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
    Ok(Made { result, outgoing: places.size() })
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

#[cfg(test)]
mod tests {
    use rucc_target::x86_64::{REGS, SYSV, WIN64};

    use super::*;

    /// The parameters of a function under a convention, as machine IR text.
    fn bind(params: &[Type], conv: &CallRegs) -> String {
        let mut names = Interner::new();
        let mut out = mir::Func::new(names.intern("f"));
        let block = out.create_block();
        entry(&mut out, block, params, conv, &mut names).expect("every parameter arrives");
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
        let arrived = entry(&mut out, block, params, conv, &mut names).expect("every parameter");
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
        assert_eq!(entry(&mut out, block, &params, &SYSV, &mut names), Err((1, Missing::OnX87)));
        assert_eq!(
            make(&[], Some(Type::float(rucc_ir::Float::F80)), false, &SYSV).2,
            Err(Refused { argument: None, missing: Missing::OnX87 })
        );
    }

    /// One call to `g`, with a register for each argument arriving in the block that makes it.
    fn make(
        args: &[Type],
        returns: Option<Type>,
        variadic: bool,
        conv: &CallRegs,
    ) -> (Interner, mir::Func, Result<Made, Refused>) {
        let mut names = Interner::new();
        let mut out = mir::Func::new(names.intern("f"));
        let block = out.create_block();
        let passed: Vec<(Type, mir::Reg)> =
            args.iter().map(|&ty| (ty, out.append_param(block, class_of(ty, conv)))).collect();
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
        let (_, func, made) = make(&[i32, i32, i32], None, false, &SYSV);
        assert_eq!(made.expect("three integers all fit in registers").result, None);
        assert_eq!(operands(&func).1, ["rdi", "rsi", "rdx"]);
    }

    #[test]
    fn the_other_convention_passes_the_same_arguments_somewhere_else() {
        let i64 = Type::int(64);
        let (_, func, made) = make(&[i64, i64], None, false, &WIN64);
        // Thirty two bytes of stack for a call that passes nothing on the stack, which is what
        // Windows asks a caller to leave the callee whether the callee uses it or not.
        assert_eq!(made.expect("two integers fit in registers").outgoing, 32);
        assert_eq!(operands(&func).1, ["rcx", "rdx"]);
    }

    #[test]
    fn what_a_call_gives_back_comes_out_of_the_register_the_convention_returns_in() {
        let (names, func, made) = make(&[], Some(Type::int(32)), false, &SYSV);
        let result = made.expect("an integer comes back").result.expect("in a register");
        // The first thing written is the result, and it is the only thing written that is a value
        // rather than a register the callee destroyed.
        assert_eq!(operands(&func).0.first().map(String::as_str), Some("rax"));
        assert_eq!(func.class_of(result), Some(SYSV.int_class));
        assert!(mir::print_func(&func, &names, &REGS).contains("x64.call"));
    }

    #[test]
    fn every_register_the_callee_may_destroy_is_written_by_the_call() {
        let (_, func, _) = make(&[Type::int(64)], Some(Type::int(64)), false, &SYSV);
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
        let (names, func, made) = make(&[Type::int(64)], None, true, &SYSV);
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
        let (names, func, made) = make(&[Type::int(64), f64, f64], None, true, &SYSV);
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
            make(&[Type::int(32), f64], None, true, &WIN64).2,
            Err(Refused { argument: Some(1), missing: Missing::InBothFiles })
        );
        // The same call to a callee whose signature names both arguments is fine, because there is
        // no second copy to make.
        assert!(make(&[Type::int(32), f64], None, false, &WIN64).2.is_ok());
    }

    #[test]
    fn a_call_through_an_address_reads_it_in_front_of_the_arguments() {
        let i32 = Type::int(32);
        let mut names = Interner::new();
        let mut out = mir::Func::new(names.intern("f"));
        let block = out.create_block();
        let address = out.append_param(block, SYSV.int_class);
        let passed = vec![(i32, out.append_param(block, SYSV.int_class))];
        let what = Calling {
            callee: Callee::Through(address),
            args: &passed,
            returns: Some(i32),
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
    fn a_call_that_would_pass_an_argument_on_the_stack_is_reported() {
        let i64 = Type::int(64);
        let seven = vec![i64; 7];
        let (_, func, made) = make(&seven, None, false, &SYSV);
        assert_eq!(made, Err(Refused { argument: Some(6), missing: Missing::OnStack }));
        // Nothing was written, so a call this cannot make leaves no half of one behind.
        let block = func.entry().expect("a function with a block in it");
        assert_eq!(func.insts(block).count(), 0);
        // Windows runs out three arguments earlier, which is the same answer at a different
        // position and the reason this is a fact about the convention rather than about the call.
        assert_eq!(
            make(&seven, None, false, &WIN64).2,
            Err(Refused { argument: Some(4), missing: Missing::OnStack })
        );
    }

    /// A float travels in the other file at both ends of a call, and the register it comes back in
    /// is the first of that file rather than the first of the other one.
    #[test]
    fn a_call_passes_and_returns_a_float_in_a_vector_register() {
        let f64 = Type::float(rucc_ir::Float::F64);
        let (_, func, made) = make(&[Type::int(32), f64], Some(f64), false, &SYSV);
        let result = made.expect("an integer and a float both fit in registers");
        let (written, read) = operands(&func);
        assert_eq!(read, ["rdi", "xmm0"]);
        assert_eq!(written.first().map(String::as_str), Some("xmm0"));
        assert_eq!(func.class_of(result.result.expect("a float comes back")), Some(SYSV.sse_class));
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
            make(&[i128], None, false, &SYSV).2,
            Err(Refused { argument: Some(0), missing: Missing::Width })
        );
        assert_eq!(
            make(&[], Some(i128), false, &SYSV).2,
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
        assert!(make(&[Type::PTR], Some(Type::PTR), false, &SYSV).2.is_ok());
    }
}
