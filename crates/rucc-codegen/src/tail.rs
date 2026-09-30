//! A call in tail position, as a jump once the frame is given back.
//!
//! Design: `spec/optimizer/25-tail-calls.md` section 25.2, the sibling call.
//!
//! `return f(x)` needs nothing of the caller once `f` is running, so the caller can give its frame
//! back first and jump to `f`, and `f` returns straight to whoever called the caller. That saves a
//! call and a return, and it makes a chain of calls like that run in the same stack however long
//! it is, which is what a state machine written as functions calling each other relies on. gcc does
//! it at `-O2` and `-Os`, and so does this.
//!
//! # Three places
//!
//! [`mark`] works on the IR, before selection. It turns a call whose results are exactly what the
//! block returns next into a `tail_call`, which ends the block the way a `return` did, and
//! it turns down the whole function when the callee could see something of the caller's frame. Its
//! answer is what [`refusal`] says, a reason rather than a no.
//!
//! [`crate::lower`] builds a `tail_call` as the call and the return it stands for, and writes the
//! call down as a [`Tail`] when the convention put every argument in a register. A call that needs
//! the argument area is left as the call it was: the area is the bottom of this function's frame,
//! and the frame is gone by the time the callee would read it.
//!
//! [`jumps`] runs last, on machine code with every register handed out and the epilogue written.
//! Each [`Tail`] whose block goes straight from the call to the epilogue, with nothing in between
//! touching a register the call reads or writes, loses its call, and the `ret` at the end becomes a
//! `jmp` to the callee. Anything else stays a call and a `ret`, which is right, just not as short.
//!
//! # Through a pointer
//!
//! `return ops->read(file, buf)` is the shape a kernel is full of, and it is a tail call like any
//! other. The `tail_call` it becomes names nobody and takes the address as its first operand, the
//! way a `call_indirect` does. What is different is that the address is in a register until the
//! jump, and the epilogue puts back the registers a callee keeps. So [`crate::lower`] asks for the
//! address in a register the callee may destroy and no argument is in, and the `ret` becomes a
//! jump through it, which `-mindirect-branch=thunk-extern` then sends through the thunk the way it
//! does any other jump through a register.
//!
//! # Why the frame check is this strict
//!
//! The one thing that makes a tail call wrong is a pointer into the frame that is given back, and
//! the section above asks for "provably not reachable" rather than "not known to be". The only
//! addresses into a frame the IR has are the ones an `alloca` makes, so a function with no `alloca`
//! has nothing a pointer could point at, and that is the rule. It turns down functions an escape
//! analysis would let through, which costs a few calls and nothing else.

use rucc_base::{Interner, Symbol};
use rucc_ir::{Abi, AttrSet, Extra, Func, Inst, Opcode, Value};
use rucc_mir as mir;
use rucc_target::{BranchInsts, FrameInsts, RegClass};

use crate::elsewhere::Elsewhere;

/// One call [`crate::lower`] built for a `tail_call`, and the pseudos that leave its answer where
/// the caller's answer goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tail {
    /// The call.
    pub call: mir::Inst,
    /// The return pseudos behind it, which write nothing and go with the call.
    pub returns: Vec<mir::Inst>,
}

/// Why no call in this function can be made in tail position, or `None` when one can.
///
/// Control coming back into the caller a second time needs the caller's frame, so a caller that
/// calls something that comes back twice anywhere makes no tail call at all. Which calls those are
/// is [`comes_back`]'s rule, and the inliner asks the same one of a callee.
#[must_use]
pub fn refusal(func: &Func, names: &Interner, elsewhere: &Elsewhere) -> Option<&'static str> {
    if func.attrs.set.contains(AttrSet::NAKED) {
        return Some("the function is naked and writes its own ending");
    }
    let sret =
        func.signature().params.first().is_some_and(|param| matches!(param.abi, Abi::Sret { .. }));
    if sret {
        return Some("the function gives its answer back through memory it was handed");
    }
    for block in func.blocks() {
        for inst in func.insts(block) {
            match func[inst].opcode {
                Opcode::Alloca => return Some("a local lives in the frame"),
                Opcode::VaStart => return Some("the function reads its own variable arguments"),
                Opcode::ApplyArgs => return Some("the function keeps its arguments in the frame"),
                Opcode::SetjmpMarker => return Some("the function saves a place to come back to"),
                Opcode::Call if twice(func, inst, names, elsewhere) => {
                    return Some("the function calls something that comes back twice");
                }
                _ => (),
            }
        }
    }
    None
}

/// Whether control can come back into this function a second time from one call, through a
/// `__builtin_setjmp` or a call to one of the names [`rucc_ir::twice_by_name`] knows or to a
/// function declared `returns_twice`.
///
/// The frame of such a function is laid out with nothing sharing anything. A value computed before
/// the `setjmp` and read after the `longjmp` is live across the call on the arm that reads it, and
/// dead on the arm that ran first, so liveness lets that arm reuse its spill slot. Registers are
/// safe, since the jump puts back the callee saved registers the `setjmp` wrote down, but a spill
/// slot is only memory and comes back holding whatever the first arm left in it. 7.13.2.1p3 only
/// lets a local that changed after the `setjmp` come back indeterminate, and this one did not.
/// Postgres's `PG_TRY` is `sigsetjmp` with exactly this shape, and gcc answers the same way by
/// giving nothing live across such a call a slot it shares.
#[must_use]
pub fn comes_back(func: &Func, names: &Interner, elsewhere: &Elsewhere) -> bool {
    func.blocks().any(|block| {
        func.insts(block).any(|inst| match func[inst].opcode {
            Opcode::SetjmpMarker => true,
            Opcode::Call => twice(func, inst, names, elsewhere),
            _ => false,
        })
    })
}

/// Whether that call is to one of the names [`rucc_ir::twice_by_name`] knows or to a function
/// declared to be like them.
fn twice(func: &Func, inst: Inst, names: &Interner, elsewhere: &Elsewhere) -> bool {
    let Extra::Call(info) = func[inst].extra else { return false };
    let Some(callee) = func[info].callee else { return false };
    elsewhere.twice(callee) || rucc_ir::twice_by_name(names.resolve(callee))
}

/// Turns every call in tail position into a `tail_call`, unless [`refusal`] has a reason not to,
/// and says how many it turned.
pub fn mark(func: &mut Func, names: &Interner, elsewhere: &Elsewhere) -> usize {
    if refusal(func, names, elsewhere).is_some() {
        return 0;
    }
    let blocks: Vec<_> = func.blocks().collect();
    let mut marked = 0;
    for block in blocks {
        let insts: Vec<Inst> = func.insts(block).collect();
        let [.., call, ret] = insts[..] else { continue };
        if !in_tail_position(func, call, ret) {
            continue;
        }
        func.remove_inst(ret);
        func[call].opcode = Opcode::TailCall;
        marked += 1;
    }
    marked
}

/// Whether that call, straight in front of that instruction, is one the caller could jump to.
///
/// A call to a name or through a pointer, and the `return` gives back the call's results, in
/// order, and nothing else, and the two signatures say the same about them, so the callee leaves the answer where the
/// caller's caller looks and in the form it expects.
fn in_tail_position(func: &Func, call: Inst, ret: Inst) -> bool {
    if func[ret].opcode != Opcode::Return {
        return false;
    }
    let Extra::Call(info) = func[call].extra else { return false };
    let info = func[info];
    // A `call` names its callee and a `call_indirect` never does, and a `tail_call` tells the two
    // apart by the same thing.
    match func[call].opcode {
        Opcode::Call if info.callee.is_some() => (),
        Opcode::CallIndirect if info.callee.is_none() => (),
        _ => return false,
    }
    let results: Vec<Value> = func[call].results().collect();
    if func[func[ret].args] != results[..] {
        return false;
    }
    // And the two are of one convention. A jump leaves the callee to return straight to this
    // function's caller, who restores what its own convention says the callee kept. An `ms_abi`
    // function jumping to an ordinary one on Linux would hand back `rsi`, `rdi` and the upper
    // vector registers as the SysV callee left them, which its caller counted on it keeping, and
    // the arguments would be in the other registers besides. gcc makes no sibling call across
    // the difference either.
    let callee = &func[info.signature];
    callee.convention == func.signature().convention && callee.returns == func.signature().returns
}

/// Turns each [`Tail`] that can be into the epilogue and a jump, and says how many it turned.
///
/// Nothing happens on a machine with no jump to a name, which is what [`FrameInsts::away`] says.
/// A call through a pointer becomes the jump through a register that `branch` names, which is the
/// one a computed `goto` is selected as.
pub fn jumps(
    func: &mut mir::Func,
    tails: &[Tail],
    insts: &FrameInsts,
    branch: &BranchInsts,
    names: &mut Interner,
) -> usize {
    let Some(away) = insts.away else { return 0 };
    let ret = mir::Opcode::new(names.intern(&format!("{}{}", insts.prefix, insts.ret)));
    let away = mir::Opcode::new(names.intern(&format!("{}{away}", insts.prefix)));
    let through = mir::Opcode::new(names.intern(&format!("{}{}", branch.prefix, branch.indirect)));
    let mut jumped = 0;
    for tail in tails {
        let Some((last, callee)) = ending(func, tail, ret) else { continue };
        let span = func.span(tail.call);
        let operands = func[func[tail.call].operands].to_vec();
        func.remove_inst(tail.call);
        for &pseudo in &tail.returns {
            func.remove_inst(pseudo);
        }
        // The same instruction rather than a new one, so the unwind rows the epilogue hung on the
        // `ret` stay where they were: the frame is in the same state at the jump as it was there.
        match callee {
            Callee::Named(callee) => {
                func[last].opcode = away;
                func[last].symbol = Some(callee);
            }
            // The address is the first thing the call read, which is where `crate::abi` put it,
            // and it is all the jump reads.
            Callee::Through => {
                let address = operands[mir::defs(&operands)];
                func[last].opcode = through;
                func[last].symbol = None;
                func[last].operands =
                    func.push_operands(&[mir::Operand::read(address.reg, address.class)]);
            }
        }
        func.set_span(last, span);
        jumped += 1;
    }
    jumped
}

/// Where a tail call goes, which is a name or the register the call read its address from.
#[derive(Debug, Clone, Copy)]
enum Callee {
    Named(Symbol),
    Through,
}

/// The `ret` the tail's block ends in and where it goes, when everything between the call and the
/// `ret` can run before the callee does.
///
/// That is the return pseudos, which are nothing, and the epilogue, which puts back registers the
/// callee saves for itself and moves the stack pointer. Anything that reads or writes a register
/// the call names could be moving an argument or reading the answer, and anything with a name on
/// it could be a call, so either keeps the call. A call through a register names that register,
/// so an epilogue that puts back the one the address is in keeps the call too.
fn ending(func: &mir::Func, tail: &Tail, ret: mir::Opcode) -> Option<(mir::Inst, Callee)> {
    let block = func.block_of(tail.call)?;
    let callee = func[tail.call].symbol.map_or(Callee::Through, Callee::Named);
    if !func[block].succs.is_empty() {
        return None;
    }
    let last = func.insts(block).last()?;
    if func[last].opcode != ret {
        return None;
    }
    let named: Vec<(mir::Reg, RegClass)> =
        func[func[tail.call].operands].iter().map(|operand| (operand.reg, operand.class)).collect();
    let mut at = func.next_inst(tail.call)?;
    while at != last {
        if !tail.returns.contains(&at) {
            let data = &func[at];
            let touches = func[data.operands]
                .iter()
                .any(|operand| named.contains(&(operand.reg, operand.class)));
            if touches || data.symbol.is_some() {
                return None;
            }
        }
        at = func.next_inst(at)?;
    }
    Some((last, callee))
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{
        Block, Builder, CallInfo, Extra, Flags, Func, InstData, Opcode, Signature, Type, Value,
    };

    use super::{comes_back, mark, refusal};
    use crate::elsewhere::Elsewhere;

    /// `int f(int a) { return g(a); }`, and whatever `between` puts in front of the return.
    fn caller(
        names: &mut Interner,
        between: impl FnOnce(&mut Func, Block, Value) -> Value,
    ) -> Func {
        caller_of(names, rucc_target::Convention::Target, between)
    }

    /// The same caller, with `g` declared in the convention given.
    fn caller_of(
        names: &mut Interner,
        convention: rucc_target::Convention,
        between: impl FnOnce(&mut Func, Block, Value) -> Value,
    ) -> Func {
        let i32 = Type::int(32);
        let mut func =
            Func::new(names.intern("f"), Signature::new().with_params(&[i32]).with_returns(&[i32]));
        let block = func.create_block();
        let arg = func.append_param(block, i32);
        let mut called = Signature::new().with_params(&[i32]).with_returns(&[i32]);
        called.convention = convention;
        let sig = func.add_signature(called);
        let callee = names.intern("g");
        let call = Builder::new(&mut func, block).call(callee, sig, &[arg]);
        let got = func[call].first_result.expect("an integer comes back");
        let answer = between(&mut func, block, got);
        Builder::new(&mut func, block).ret(&[answer]);
        func
    }

    fn opcodes(func: &Func) -> Vec<Opcode> {
        func.blocks().flat_map(|block| func.insts(block).map(|inst| func[inst].opcode)).collect()
    }

    /// A jump to a function of the other convention would leave the caller's caller with what
    /// the callee did not keep, so the call stays a call.
    #[test]
    fn a_call_to_the_other_convention_stays_a_call() {
        let mut names = Interner::new();
        let mut func = caller_of(&mut names, rucc_target::Convention::Ms, |_, _, got| got);
        assert_eq!(mark(&mut func, &names, &Elsewhere::default()), 0);
        assert_eq!(opcodes(&func), [Opcode::Call, Opcode::Return]);
    }

    #[test]
    fn a_call_whose_answer_is_returned_becomes_a_tail_call() {
        let mut names = Interner::new();
        let mut func = caller(&mut names, |_, _, got| got);
        assert_eq!(mark(&mut func, &names, &Elsewhere::default()), 1);
        assert_eq!(opcodes(&func), [Opcode::TailCall]);
    }

    /// `int f(int (*g)(int), int a) { return g(a); }` is a tail call too, one that names nobody
    /// and keeps the address as its first operand.
    #[test]
    fn a_call_through_a_pointer_whose_answer_is_returned_becomes_a_tail_call() {
        let mut names = Interner::new();
        let i32 = Type::int(32);
        let mut func = Func::new(
            names.intern("f"),
            Signature::new().with_params(&[Type::PTR, i32]).with_returns(&[i32]),
        );
        let block = func.create_block();
        let address = func.append_param(block, Type::PTR);
        let arg = func.append_param(block, i32);
        let sig = func.add_signature(Signature::new().with_params(&[i32]).with_returns(&[i32]));
        let varargs = func.push_abis(&[]);
        let info = func.add_call(CallInfo { callee: None, signature: sig, varargs });
        let mut build = Builder::new(&mut func, block);
        let inst = InstData {
            args: build.func().push_values(&[address, arg]),
            extra: Extra::Call(info),
            ..InstData::new(Opcode::CallIndirect)
        };
        let call = build.inst(inst, &[i32]);
        let got = func[call].first_result.expect("an integer comes back");
        Builder::new(&mut func, block).ret(&[got]);

        assert_eq!(mark(&mut func, &names, &Elsewhere::default()), 1);
        assert_eq!(opcodes(&func), [Opcode::TailCall]);
        assert_eq!(func[func[call].args], [address, arg]);
    }

    #[test]
    fn a_call_with_work_after_it_is_not_in_tail_position() {
        let mut names = Interner::new();
        let mut func = caller(&mut names, |func, block, got| {
            Builder::new(func, block).binary(Opcode::Add, got, got, Flags::default())
        });
        assert_eq!(mark(&mut func, &names, &Elsewhere::default()), 0);
        assert_eq!(opcodes(&func), [Opcode::Call, Opcode::Add, Opcode::Return]);
    }

    #[test]
    fn a_local_in_the_frame_turns_down_the_whole_function() {
        let mut names = Interner::new();
        let mut func = caller(&mut names, |func, block, got| {
            Builder::new(func, block).value(InstData::new(Opcode::Alloca), Type::PTR);
            got
        });
        assert_eq!(
            refusal(&func, &names, &Elsewhere::default()),
            Some("a local lives in the frame")
        );
        assert_eq!(mark(&mut func, &names, &Elsewhere::default()), 0);
    }

    #[test]
    fn a_call_to_setjmp_anywhere_turns_down_the_whole_function() {
        let mut names = Interner::new();
        let setjmp = names.intern("_setjmp");
        let mut func = caller(&mut names, |func, block, got| {
            let sig = func.add_signature(Signature::new().with_returns(&[Type::int(32)]));
            Builder::new(func, block).call(setjmp, sig, &[]);
            got
        });
        assert_eq!(
            refusal(&func, &names, &Elsewhere::default()),
            Some("the function calls something that comes back twice")
        );
        assert_eq!(mark(&mut func, &names, &Elsewhere::default()), 0);
    }

    #[test]
    fn glibcs_name_for_sigsetjmp_comes_back_and_an_ordinary_call_does_not() {
        let mut names = Interner::new();
        let setjmp = names.intern("__sigsetjmp");
        let func = caller(&mut names, |func, block, got| {
            let sig = func.add_signature(Signature::new().with_returns(&[Type::int(32)]));
            Builder::new(func, block).call(setjmp, sig, &[]);
            got
        });
        assert!(comes_back(&func, &names, &Elsewhere::default()));
        let plain = caller(&mut names, |_, _, got| got);
        assert!(!comes_back(&plain, &names, &Elsewhere::default()));
    }
}
