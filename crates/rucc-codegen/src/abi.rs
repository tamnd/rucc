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
//! One `x64.arg_val_*` per parameter, at the top of the entry block, each defining a fresh
//! register constrained to the one the argument arrived in. They encode to nothing. The point of
//! them is that a parameter has to be defined somewhere for the allocator to have anything to
//! move, and the entry block cannot define it as a block parameter: there is no edge into the
//! entry block for the move to go on, which is what `rucc_regalloc::rewrite` asserts.
//!
//! What the allocator does with them is the whole of the argument sequence. A parameter that is
//! read where it arrived costs nothing, and one that is not gets a copy, which is the same
//! bargain the return already makes and is decided by the same code.
//!
//! # A call
//!
//! The same reasoning the other way round, and one instruction rather than several. `x64.call` is
//! the only opcode in the description whose operand vector is empty there, because nothing about
//! a call's operands is the same from one call to the next, so they are built here: one read per
//! argument constrained to the register the convention passes it in, one definition for the value
//! that comes back constrained to the register it comes back in, and one definition per register
//! the convention does not preserve.
//!
//! Those last ones are the clobbers, and they are the whole of what the allocator has to know
//! about a call besides where the values go. Each is a definition of the physical register itself
//! rather than of a value, since there is no value: it says the register is written here, which
//! is exactly what stops the allocator from leaving something in one across the call. A register
//! an argument or the result already names is not repeated, because naming it once already blocks
//! it for the length of the instruction, which is all a clobber does.
//!
//! What is not here is the bytes an argument past the last register goes in. That is a place in
//! the frame and no frame exists yet, the same reason a parameter arriving there is reported
//! rather than read, so a call is asked how many bytes it would need and reports it, and a call
//! that would need any is turned down for now.

use rucc_base::{Interner, Symbol};
use rucc_ir::Type;
use rucc_mir as mir;
use rucc_target::{CallRegs, Constraint, PhysReg, Places, Where};

/// Why a parameter could not be brought in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Missing {
    /// It arrives on the stack, which is somewhere nothing reads from yet: the offset is a
    /// distance into a frame, and a frame is not worked out until after allocation.
    OnStack,
    /// It arrives in a vector register. Every rule in the set is about an integer, so a value in
    /// one has nothing downstream that could use it.
    InVector,
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
            Missing::InVector => "is in a vector register",
            Missing::Width => "is a width no argument register holds",
        }
    }
}

/// Binds a function's parameters to the registers the convention says they arrive in.
///
/// The registers come back in the order the parameters were given, so the caller can bind each
/// IR parameter to the one at its position.
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
) -> Result<Vec<mir::Reg>, (usize, Missing)> {
    let mut places = Places::new(conv);
    let mut regs = Vec::with_capacity(params.len());
    for (index, &ty) in params.iter().enumerate() {
        // Asking for the place of a parameter that cannot be brought in is still worth doing
        // before giving up, and it costs nothing, because every place after it depends on it and
        // a reader stepping through this in a debugger should see the same numbers a working
        // version would.
        let at = if ty.is_float() { places.float() } else { places.integer() };
        if ty.is_float() {
            return Err((index, Missing::InVector));
        }
        let head = head_of(ty).ok_or((index, Missing::Width))?;
        let Where::Reg(arrived) = at else { return Err((index, Missing::OnStack)) };

        let reg = out.new_vreg(conv.int_class);
        let opcode = mir::Opcode::new(names.intern(head));
        let operand = mir::Operand::write(reg, conv.int_class).with(Constraint::Fixed(arrived));
        out.build(block, opcode).operand(operand).finish();
        regs.push(reg);
    }
    Ok(regs)
}

/// What the instruction that calls a name is called.
///
/// Here rather than in a rule for the same reason the arguments are: a rule pattern sees one term
/// and a call's operands are whatever the signature made them, so no pattern could name them.
pub const CALL: &str = "x64.call";

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

/// One call, as everything about it that is not the function it is being built into.
#[derive(Debug, Clone, Copy)]
pub struct Calling<'a> {
    /// The name it calls.
    pub callee: Symbol,
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
    for (index, &(ty, reg)) in args.iter().enumerate() {
        let refused = |missing| Refused { argument: Some(index), missing };
        let at = if ty.is_float() { places.float() } else { places.integer() };
        if ty.is_float() {
            return Err(refused(Missing::InVector));
        }
        if head_of(ty).is_none() {
            return Err(refused(Missing::Width));
        }
        let Where::Reg(at) = at else { return Err(refused(Missing::OnStack)) };
        passed.push((reg, at));
    }
    let comes_back = match returns {
        None => None,
        Some(ty) if ty.is_float() => {
            return Err(Refused { argument: None, missing: Missing::InVector });
        }
        Some(ty) if head_of(ty).is_none() => {
            return Err(Refused { argument: None, missing: Missing::Width });
        }
        // Which register a value comes back in depends on nothing but the value, which is why the
        // return side of the convention is a rule and this side is not. There is no rule here
        // because the arguments are in the same instruction.
        Some(_) => Some(
            *conv.int_returns.first().ok_or(Refused { argument: None, missing: Missing::Width })?,
        ),
    };

    // A variadic callee on SysV reads how many vector registers the call passed arguments in and
    // skips saving them when the answer is none, which is what makes `printf` with no floating
    // point argument cheap. It is an obligation rather than an optimization: leaving whatever was
    // in the register there makes the callee save a register file it was not given. The answer is
    // zero because a float argument is refused above, and it will stop being a constant on the
    // day one is not.
    let counted = if variadic { conv.vector_count } else { None };

    // The definitions first and the reads after, which is the order every operand vector in the
    // machine IR is in and the order `rucc_mir::defs` counts.
    let mut operands = Vec::with_capacity(args.len() + conv.int_order.len() + 2);
    let result = comes_back.map(|at| {
        let reg = out.new_vreg(conv.int_class);
        operands.push(mir::Operand::write(reg, conv.int_class).with(Constraint::Fixed(at)));
        reg
    });
    let named: Vec<PhysReg> =
        comes_back.into_iter().chain(counted).chain(passed.iter().map(|&(_, at)| at)).collect();
    for &reg in conv.int_order {
        if !conv.preserves_int(reg) && !named.contains(&reg) {
            operands.push(mir::Operand::write(mir::Reg::physical(reg), conv.int_class));
        }
    }
    for &reg in conv.sse_order {
        if !conv.preserves_sse(reg) {
            operands.push(mir::Operand::write(mir::Reg::physical(reg), conv.sse_class));
        }
    }
    for (reg, at) in passed {
        operands.push(mir::Operand::read(reg, conv.int_class).with(Constraint::Fixed(at)));
    }
    if let Some(at) = counted {
        let count = out.new_vreg(conv.int_class);
        let zero = mir::Opcode::new(names.intern("x64.mov_ri_32"));
        out.build(block, zero).def(count, conv.int_class).imm(0).finish();
        operands.push(mir::Operand::read(count, conv.int_class).with(Constraint::Fixed(at)));
    }

    let opcode = mir::Opcode::new(names.intern(CALL));
    let mut build = out.build(block, opcode).symbol(callee);
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
    let names = ["x64.arg_val_8", "x64.arg_val_16", "x64.arg_val_32", "x64.arg_val_64"];
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

    #[test]
    fn an_argument_past_the_last_register_is_reported_rather_than_read_from_nowhere() {
        let i64 = Type::int(64);
        let mut names = Interner::new();
        let mut out = mir::Func::new(names.intern("f"));
        let block = out.create_block();
        let seven = vec![i64; 7];
        assert_eq!(entry(&mut out, block, &seven, &SYSV, &mut names), Err((6, Missing::OnStack)));
        // Six of them still got registers, and the seventh is what stopped it. Windows runs out
        // three arguments earlier, which is the same answer at a different position.
        assert_eq!(entry(&mut out, block, &seven, &WIN64, &mut names), Err((4, Missing::OnStack)));
    }

    #[test]
    fn an_argument_in_a_vector_register_is_reported_because_nothing_here_uses_one() {
        let mut names = Interner::new();
        let mut out = mir::Func::new(names.intern("f"));
        let block = out.create_block();
        let params = [Type::int(32), Type::float(rucc_ir::Float::F64)];
        assert_eq!(entry(&mut out, block, &params, &SYSV, &mut names), Err((1, Missing::InVector)));
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
            args.iter().map(|&ty| (ty, out.append_param(block, conv.int_class))).collect();
        let callee = names.intern("g");
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
        // Zero of them, which is the answer while a float argument is refused, and `al` is where
        // a SysV callee looks for it. Leaving whatever was in the register there would make a
        // callee that saves its vector registers save ones it was never given.
        assert_eq!(read, ["rdi", "rax"]);
        assert_eq!(
            mir::print_func(&func, &names, &REGS).lines().nth(2),
            Some("    %1:gpr = x64.mov_ri_32 0")
        );
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

    #[test]
    fn a_call_that_travels_in_a_vector_register_is_reported_on_either_side() {
        let f64 = Type::float(rucc_ir::Float::F64);
        assert_eq!(
            make(&[Type::int(32), f64], None, false, &SYSV).2,
            Err(Refused { argument: Some(1), missing: Missing::InVector })
        );
        assert_eq!(
            make(&[], Some(f64), false, &SYSV).2,
            Err(Refused { argument: None, missing: Missing::InVector })
        );
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
