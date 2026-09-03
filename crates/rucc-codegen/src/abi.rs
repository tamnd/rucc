//! Where a function's arguments already are when it starts running.
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

use rucc_base::Interner;
use rucc_ir::Type;
use rucc_mir as mir;
use rucc_target::{CallRegs, Constraint, Places, Where};

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
    #[must_use]
    pub fn why(self) -> &'static str {
        match self {
            Missing::OnStack => "arrives on the stack",
            Missing::InVector => "arrives in a vector register",
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

/// What the pseudo for an argument of that type is called.
///
/// The width is in the name for the same reason it is in every other opcode here: it is what the
/// instruction is about. Nothing encodes it, so nothing depends on it being right, but a listing
/// that says an argument arrived and does not say how much of it did is a listing worth less.
///
/// An integer and nothing else, which is the same answer `crate::term` gives about a value in a
/// register, and deliberately the same: an address is not an `i64` to the rule set today, so
/// bringing one in here would produce a register no rule could then name. Whoever widens one of
/// the two should widen both.
#[must_use]
pub fn head_of(ty: Type) -> Option<&'static str> {
    let slot = match ty.is_int().then(|| ty.bits())? {
        8 => 0,
        16 => 1,
        32 => 2,
        64 => 3,
        _ => return None,
    };
    Some(["x64.arg_val_8", "x64.arg_val_16", "x64.arg_val_32", "x64.arg_val_64"][slot])
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

    #[test]
    fn an_argument_wider_than_a_register_has_no_name() {
        assert_eq!(head_of(Type::int(128)), None);
        assert_eq!(head_of(Type::int(8)), Some("x64.arg_val_8"));
        assert_eq!(head_of(Type::int(64)), Some("x64.arg_val_64"));
    }
}
