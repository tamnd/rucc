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
//! block returns next, or hands to a block that only returns them, into a `tail_call`, which ends
//! the block the way a `return` did, and it turns down the whole function when the callee could
//! see something of the caller's frame. Its answer is what [`refusal`] says, a reason rather than
//! a no.
//!
//! [`crate::lower`] builds a `tail_call` as the call and the return it stands for, and writes the
//! call down as a [`Tail`] when the convention put every argument in a register. A call that needs
//! the argument area is left as the call it was: the area is the bottom of this function's frame,
//! and the frame is gone by the time the callee would read it. Unless `musttail` asked for it, in
//! which case the arguments go where this function's own came, above the return address the jump
//! leaves for the callee, which is what gcc does. That is as many bytes as this function's caller
//! gave it and no more, and past that the call is refused in gcc's words.
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
use rucc_ir::{Abi, AttrSet, Block, Extra, Flags, Func, Inst, Opcode, Value};
use rucc_mir as mir;
use rucc_target::{BranchInsts, FrameInsts, RegClass};

use crate::elsewhere::Elsewhere;
use crate::lower::Unsupported;

/// One call [`crate::lower`] built for a `tail_call`, and the pseudos that leave its answer where
/// the caller's answer goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tail {
    /// The call.
    pub call: mir::Inst,
    /// The return pseudos behind it, which write nothing and go with the call.
    pub returns: Vec<mir::Inst>,
    /// The `tail_call` it was built for, when `musttail` asked for that, which makes a call that
    /// cannot become a jump an error.
    pub must: Option<Inst>,
}

/// Why no call in this function can be made in tail position, or `None` when one can.
///
/// Control coming back into the caller a second time needs the caller's frame, so a caller that
/// calls something that comes back twice anywhere makes no tail call at all. Which calls those are
/// is [`comes_back`]'s rule, and the inliner asks the same one of a callee.
#[must_use]
pub fn refusal(func: &Func, names: &Interner, elsewhere: &Elsewhere) -> Option<&'static str> {
    refused(func, names, elsewhere, true).map(|(why, _)| why)
}

/// What gcc says when a reason of its own stops a `musttail` call, and what [`mark`] says when
/// the machine or the command line does.
pub const UNABLE: &str = "target is not able to optimize the call into a sibling call";

/// [`refusal`]'s reason, and what gcc says when the same thing stops a `musttail` call.
///
/// `locals` is whether a local in the frame is a reason, which it is not for a `musttail` call.
/// gcc ends the lifetime of every local before the jump, so a callee handed a pointer to one is
/// the program's mistake. A local whose size is only known when it runs is a reason for both,
/// as in gcc.
fn refused(
    func: &Func,
    names: &Interner,
    elsewhere: &Elsewhere,
    locals: bool,
) -> Option<(&'static str, &'static str)> {
    if func.attrs.set.contains(AttrSet::NAKED) {
        return Some(("the function is naked and writes its own ending", UNABLE));
    }
    let sret =
        func.signature().params.first().is_some_and(|param| matches!(param.abi, Abi::Sret { .. }));
    if sret {
        let why = "the function gives its answer back through memory it was handed";
        return Some((why, "callee returns a structure"));
    }
    // A static chain travels in a register the tail call sequence knows nothing about, and the
    // functions that pass or take one are nested functions and the functions around them, which
    // are not ones worth teaching it to.
    let chained = func
        .signatures()
        .chain(std::iter::once(func.signature()))
        .any(|signature| signature.params.iter().any(|param| param.abi == Abi::Chain));
    if chained {
        let why = "the function passes or takes the static chain of a nested function";
        return Some((why, "nested function"));
    }
    for block in func.blocks() {
        for inst in func.insts(block) {
            match func[inst].opcode {
                Opcode::Alloca if locals || !func[func[inst].args].is_empty() => {
                    return Some(("a local lives in the frame", "caller uses alloca"));
                }
                Opcode::VaStart => {
                    let why = "the function reads its own variable arguments";
                    return Some((why, "caller uses stdargs"));
                }
                Opcode::ApplyArgs => {
                    return Some(("the function keeps its arguments in the frame", UNABLE));
                }
                Opcode::SetjmpMarker => {
                    let why = "the function saves a place to come back to";
                    return Some((why, "caller uses setjmp"));
                }
                Opcode::Call if twice(func, inst, names, elsewhere) => {
                    let why = "the function calls something that comes back twice";
                    return Some((why, "caller uses setjmp"));
                }
                _ => (),
            }
        }
    }
    None
}

/// The first call `musttail` asked for, if there is one.
#[must_use]
pub fn asked(func: &Func) -> Option<Inst> {
    func.blocks().flat_map(|block| func.insts(block)).find(|&inst| must(func, inst))
}

/// Whether that is a call `musttail` asked for.
fn must(func: &Func, inst: Inst) -> bool {
    func[inst].flags.contains(Flags::MUST_TAIL)
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
pub(crate) fn twice(func: &Func, inst: Inst, names: &Interner, elsewhere: &Elsewhere) -> bool {
    let Extra::Call(info) = func[inst].extra else { return false };
    let Some(callee) = func[info].callee else { return false };
    elsewhere.twice(callee) || rucc_ir::twice_by_name(names.resolve(callee))
}

/// Whether that call can come back by an indirect jump rather than a `ret`, because the type it
/// was made through or its callee's said `indirect_return`.
pub(crate) fn jumps_back(func: &Func, inst: Inst, elsewhere: &Elsewhere) -> bool {
    if func[inst].flags.contains(Flags::INDIRECT_RETURN) {
        return true;
    }
    let Extra::Call(info) = func[inst].extra else { return false };
    func[info].callee.is_some_and(|callee| elsewhere.jumps_back(callee))
}

/// Turns every call in tail position into a `tail_call`, unless [`refusal`] has a reason not to,
/// and says how many it turned.
///
/// `optional` is whether to turn any but the calls `musttail` asked for, which is
/// `-foptimize-sibling-calls`. One it asked for is turned whatever that says and whatever local
/// is in the frame, and one that cannot be is an error, with gcc's reason for it.
///
/// `guarded` is `-fcf-protection=full` in a function whose own type is not `indirect_return`.
/// There a call that can come back by a jump stays a call, as in gcc: the jump would land in
/// this function's caller, past a call that has no landing pad after it, since that caller did
/// not know.
///
/// `indirect` is whether a call through a pointer may become a jump through it, which it may not
/// under AArch64 `bti`, where the callee's `bti c` lets in a jump only from `x16` or `x17`.
///
/// # Errors
///
/// A call `musttail` asked for that cannot be made in tail position.
pub fn mark(
    func: &mut Func,
    names: &Interner,
    elsewhere: &Elsewhere,
    guarded: bool,
    indirect: bool,
    optional: bool,
) -> Result<usize, Unsupported> {
    let optional = optional && refusal(func, names, elsewhere).is_none();
    let stopped = refused(func, names, elsewhere, false).map(|(_, said)| said);
    let blocks: Vec<_> = func.blocks().collect();
    let mut marked = 0;
    let mut left = Vec::new();
    for block in blocks {
        let insts: Vec<Inst> = func.insts(block).collect();
        let call = match insts.iter().copied().find(|&inst| must(func, inst)) {
            Some(call) => {
                let why =
                    stopped.or_else(|| misplaced(func, &insts, call, elsewhere, guarded, indirect));
                if let Some(reason) = why {
                    return Err(Unsupported::MustTail { inst: call, reason });
                }
                call
            }
            None if optional => {
                let [.., call, _] = insts[..] else { continue };
                if misplaced(func, &insts, call, elsewhere, guarded, indirect).is_some() {
                    continue;
                }
                call
            }
            None => continue,
        };
        let Some(&ret) = insts.last() else { continue };
        left.extend(func.successors(ret).map(|target| target.block));
        func.remove_inst(ret);
        func[call].opcode = Opcode::TailCall;
        marked += 1;
    }
    // A return every arm jumped to and none does now is a block whose parameters nothing hands
    // anything, so it goes rather than reaching selection that way.
    let reached: Vec<Block> = func
        .blocks()
        .filter_map(|block| func.insts(block).last())
        .flat_map(|last| func.successors(last).map(|target| target.block))
        .collect();
    left.sort_unstable();
    left.dedup();
    for block in left.into_iter().filter(|block| !reached.contains(block)) {
        func.remove_block(block);
    }
    Ok(marked)
}

/// Why that call cannot be made in tail position, in gcc's words, or `None` when it can.
///
/// It has to be the last thing in its block but the end of the block, and that has to give
/// back what the call did, which is what [`returned`] and [`misfit`] ask.
fn misplaced(
    func: &Func,
    insts: &[Inst],
    call: Inst,
    elsewhere: &Elsewhere,
    guarded: bool,
    indirect: bool,
) -> Option<&'static str> {
    let at = insts.iter().position(|&inst| inst == call).expect("the call is in the block");
    let after = &insts[at + 1..];
    let &[ret] = after else {
        let results: Vec<Value> = func[call].results().collect();
        let read =
            after.iter().any(|&inst| func[func[inst].args].iter().any(|arg| results.contains(arg)));
        return Some(if read {
            "return value changed after call"
        } else {
            "code between call and return"
        });
    };
    let Some(returned) = returned(func, ret) else { return Some("code between call and return") };
    if let Some(reason) = misfit(func, call, &returned) {
        return Some(reason);
    }
    let unable = guarded && jumps_back(func, call, elsewhere)
        || !indirect && func[call].opcode == Opcode::CallIndirect;
    unable.then_some(UNABLE)
}

/// What the function gives back when that instruction ends its block, or `None` when it does not
/// end the function.
///
/// That is a `return`, or a jump to a block that does nothing but return what it was handed, in
/// the order it was handed it. The second is how `if (x) return f(a); return g(b);` comes out of
/// the front end, with both arms jumping to one return, and how `r = f(a)` in each arm of an `if`
/// does once the variable is a block parameter.
fn returned(func: &Func, ret: Inst) -> Option<Vec<Value>> {
    match func[ret].opcode {
        Opcode::Return => Some(func[func[ret].args].to_vec()),
        Opcode::Jump => {
            let [target] = func.successors(ret).collect::<Vec<_>>()[..] else { return None };
            let mut insts = func.insts(target.block);
            let (Some(only), None) = (insts.next(), insts.next()) else { return None };
            let passed = &func[target.args];
            let same = func[only].opcode == Opcode::Return
                && func[func[only].args] == func[target.block].params[..];
            same.then(|| passed.to_vec())
        }
        _ => None,
    }
}

/// Why that call, at the end of its block, is not one the caller could jump to when the function
/// then gives back what `returned` holds, in gcc's words, or `None` when it is one.
///
/// A call to a name or through a pointer, and what is given back is the call's results, in order,
/// and nothing else, and the two signatures say the same about them, so the callee leaves the
/// answer where the caller's caller looks and in the form it expects. A function that gives back
/// nothing can also drop what the callee gives back, when that is in an integer register, where
/// the caller's caller does not look. One on the x87 stack would be left there for a caller that
/// expects the stack empty.
fn misfit(func: &Func, call: Inst, returned: &[Value]) -> Option<&'static str> {
    let Extra::Call(info) = func[call].extra else { return Some(UNABLE) };
    let info = func[info];
    // A `call` names its callee and a `call_indirect` never does, and a `tail_call` tells the two
    // apart by the same thing.
    match func[call].opcode {
        Opcode::Call if info.callee.is_some() => (),
        Opcode::CallIndirect if info.callee.is_none() => (),
        _ => return Some(UNABLE),
    }
    let callee = &func[info.signature];
    let dropped = returned.is_empty()
        && func.signature().returns.is_empty()
        && callee.returns.iter().all(|param| param.ty.is_int() || param.ty.is_ptr());
    let results: Vec<Value> = func[call].results().collect();
    if returned != results.as_slice() && !dropped {
        return Some("call and return value are different");
    }
    // And the two are of one convention. A jump leaves the callee to return straight to this
    // function's caller, who restores what its own convention says the callee kept. An `ms_abi`
    // function jumping to an ordinary one on Linux would hand back `rsi`, `rdi` and the upper
    // vector registers as the SysV callee left them, which its caller counted on it keeping, and
    // the arguments would be in the other registers besides. gcc makes no sibling call across
    // the difference either.
    if callee.convention != func.signature().convention {
        return Some(UNABLE);
    }
    // Nor is one whose callee takes the arguments off the stack, which is i386 `stdcall` and
    // `fastcall`. The callee returns with `ret $n` for its own arguments, where this function's
    // caller wants this function's taken off, and the two are the same number only by chance.
    if callee.convention.callee_pops() {
        return Some("inconsistent number of popped arguments");
    }
    let same = dropped || callee.returns == func.signature().returns;
    (!same).then_some("call and return value are different")
}

/// Turns each [`Tail`] that can be into the epilogue and a jump, and says how many it turned.
///
/// Nothing happens on a machine with no jump to a name, which is what [`FrameInsts::away`] says.
/// A call through a pointer becomes the jump through a register that `branch` names, which is the
/// one a computed `goto` is selected as.
///
/// # Errors
///
/// A call `musttail` asked for that cannot become a jump, in gcc's words.
pub fn jumps(
    func: &mut mir::Func,
    tails: &[Tail],
    insts: &FrameInsts,
    branch: &BranchInsts,
    names: &mut Interner,
) -> Result<usize, Unsupported> {
    let failed = |tail: &Tail| match tail.must {
        Some(inst) => Err(Unsupported::MustTail { inst, reason: "tail call production failed" }),
        None => Ok(()),
    };
    let Some(away) = insts.away else {
        tails.iter().try_for_each(failed)?;
        return Ok(0);
    };
    let ret = mir::Opcode::new(names.intern(&format!("{}{}", insts.prefix, insts.ret)));
    let away = mir::Opcode::new(names.intern(&format!("{}{away}", insts.prefix)));
    let through = mir::Opcode::new(names.intern(&format!("{}{}", branch.prefix, branch.indirect)));
    // The same jump with the `notrack` prefix, for a call that had it. The name is the one
    // `crate::lower` gives the call, the plain one's with `_notrack` after it.
    let untracked =
        mir::Opcode::new(names.intern(&format!("{}{}_notrack", branch.prefix, branch.indirect)));
    let mut jumped = 0;
    for tail in tails {
        let Some((last, callee)) = ending(func, tail, ret) else {
            failed(tail)?;
            continue;
        };
        let span = func.span(tail.call);
        let operands = func[func[tail.call].operands].to_vec();
        let notrack = names.resolve(func[tail.call].opcode.name()).ends_with("_notrack");
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
                func[last].opcode = if notrack { untracked } else { through };
                func[last].symbol = None;
                func[last].operands =
                    func.push_operands(&[mir::Operand::read(address.reg, address.class)]);
            }
        }
        func.set_span(last, span);
        jumped += 1;
    }
    Ok(jumped)
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
        Block, Builder, CallInfo, Extra, Flags, Float, Func, InstData, Opcode, Signature, Type,
        Value,
    };

    use super::{UNABLE, asked, comes_back, mark, refusal};
    use crate::elsewhere::Elsewhere;
    use crate::lower::Unsupported;

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

    /// Marks the first call in the function as one `musttail` asked for, and gives it back.
    fn must(func: &mut Func) -> rucc_ir::Inst {
        let call = func
            .blocks()
            .flat_map(|block| func.insts(block))
            .find(|&inst| func[inst].opcode == Opcode::Call)
            .expect("a call");
        func[call].flags = Flags::MUST_TAIL;
        assert_eq!(asked(func), Some(call));
        call
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
        assert_eq!(mark(&mut func, &names, &Elsewhere::default(), false, true, true), Ok(0));
        assert_eq!(opcodes(&func), [Opcode::Call, Opcode::Return]);
    }

    #[test]
    fn a_call_whose_answer_is_returned_becomes_a_tail_call() {
        let mut names = Interner::new();
        let mut func = caller(&mut names, |_, _, got| got);
        assert_eq!(mark(&mut func, &names, &Elsewhere::default(), false, true, true), Ok(1));
        assert_eq!(opcodes(&func), [Opcode::TailCall]);
    }

    /// Under `-fcf-protection=full` a call that can come back by a jump stays a call, and is a
    /// tail call again without the guard.
    #[test]
    fn a_call_that_comes_back_by_a_jump_stays_a_call_when_guarded() {
        let mut names = Interner::new();
        let mut func = caller(&mut names, |_, _, got| got);
        let call = func.blocks().flat_map(|block| func.insts(block)).next().expect("the call");
        func[call].flags = Flags::INDIRECT_RETURN;
        assert_eq!(mark(&mut func, &names, &Elsewhere::default(), true, true, true), Ok(0));
        assert_eq!(mark(&mut func, &names, &Elsewhere::default(), false, true, true), Ok(1));
        // And a plain call is a tail call under the guard as well.
        let mut func = caller(&mut names, |_, _, got| got);
        assert_eq!(mark(&mut func, &names, &Elsewhere::default(), true, true, true), Ok(1));
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

        assert_eq!(mark(&mut func, &names, &Elsewhere::default(), false, true, true), Ok(1));
        assert_eq!(opcodes(&func), [Opcode::TailCall]);
        assert_eq!(func[func[call].args], [address, arg]);
    }

    /// `int f(int a) { int r = g(a); return r; }` the way the front end writes an `if` with a
    /// return in each arm: the call jumps to a block that returns what it is handed.
    fn joined(names: &mut Interner, work: bool) -> Func {
        let i32 = Type::int(32);
        let mut func =
            Func::new(names.intern("f"), Signature::new().with_params(&[i32]).with_returns(&[i32]));
        let block = func.create_block();
        let arg = func.append_param(block, i32);
        let join = func.create_block();
        let param = func.append_param(join, i32);
        let sig = func.add_signature(Signature::new().with_params(&[i32]).with_returns(&[i32]));
        let callee = names.intern("g");
        let call = Builder::new(&mut func, block).call(callee, sig, &[arg]);
        let got = func[call].first_result.expect("an integer comes back");
        Builder::new(&mut func, block).jump(join, &[got]);
        let answer = if work {
            Builder::new(&mut func, join).binary(Opcode::Add, param, param, Flags::default())
        } else {
            param
        };
        Builder::new(&mut func, join).ret(&[answer]);
        func
    }

    #[test]
    fn a_call_that_jumps_to_a_return_of_its_answer_becomes_a_tail_call() {
        let mut names = Interner::new();
        let mut func = joined(&mut names, false);
        assert_eq!(mark(&mut func, &names, &Elsewhere::default(), false, true, true), Ok(1));
        assert_eq!(opcodes(&func), [Opcode::TailCall]);
    }

    #[test]
    fn a_call_that_jumps_to_work_on_its_answer_stays_a_call() {
        let mut names = Interner::new();
        let mut func = joined(&mut names, true);
        assert_eq!(mark(&mut func, &names, &Elsewhere::default(), false, true, true), Ok(0));
        assert_eq!(opcodes(&func), [Opcode::Call, Opcode::Jump, Opcode::Add, Opcode::Return]);
    }

    /// `void f(int a) { g(a); }`, with `g` giving back what `back` is.
    fn dropping(names: &mut Interner, back: Type) -> Func {
        let i32 = Type::int(32);
        let mut func = Func::new(names.intern("f"), Signature::new().with_params(&[i32]));
        let block = func.create_block();
        let arg = func.append_param(block, i32);
        let sig = func.add_signature(Signature::new().with_params(&[i32]).with_returns(&[back]));
        let callee = names.intern("g");
        Builder::new(&mut func, block).call(callee, sig, &[arg]);
        Builder::new(&mut func, block).ret(&[]);
        func
    }

    #[test]
    fn a_void_function_drops_an_integer_answer_and_jumps() {
        let mut names = Interner::new();
        let mut func = dropping(&mut names, Type::int(32));
        assert_eq!(mark(&mut func, &names, &Elsewhere::default(), false, true, true), Ok(1));
        assert_eq!(opcodes(&func), [Opcode::TailCall]);
    }

    /// On i386 a `double` comes back on the x87 stack, and a jump would leave it there for a
    /// caller that expects the stack empty.
    #[test]
    fn a_void_function_keeps_the_call_when_the_answer_is_a_float() {
        let mut names = Interner::new();
        let mut func = dropping(&mut names, Type::float(Float::F64));
        assert_eq!(mark(&mut func, &names, &Elsewhere::default(), false, true, true), Ok(0));
        assert_eq!(opcodes(&func), [Opcode::Call, Opcode::Return]);
    }

    #[test]
    fn a_call_with_work_after_it_is_not_in_tail_position() {
        let mut names = Interner::new();
        let mut func = caller(&mut names, |func, block, got| {
            Builder::new(func, block).binary(Opcode::Add, got, got, Flags::default())
        });
        assert_eq!(mark(&mut func, &names, &Elsewhere::default(), false, true, true), Ok(0));
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
        assert_eq!(mark(&mut func, &names, &Elsewhere::default(), false, true, true), Ok(0));
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
        assert_eq!(mark(&mut func, &names, &Elsewhere::default(), false, true, true), Ok(0));
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

    /// A call `musttail` asked for is made without `-foptimize-sibling-calls` and with a local in
    /// the frame, whose lifetime gcc ends before the jump. An ordinary one in the same place is
    /// not.
    #[test]
    fn a_musttail_call_is_made_where_an_ordinary_one_is_not() {
        let mut names = Interner::new();
        let mut func = caller(&mut names, |_, _, got| got);
        assert_eq!(mark(&mut func, &names, &Elsewhere::default(), false, true, false), Ok(0));
        must(&mut func);
        assert_eq!(mark(&mut func, &names, &Elsewhere::default(), false, true, false), Ok(1));
        assert_eq!(opcodes(&func), [Opcode::TailCall]);

        let i32 = Type::int(32);
        let mut func =
            Func::new(names.intern("f"), Signature::new().with_params(&[i32]).with_returns(&[i32]));
        let block = func.create_block();
        let arg = func.append_param(block, i32);
        Builder::new(&mut func, block).value(InstData::new(Opcode::Alloca), Type::PTR);
        let sig = func.add_signature(Signature::new().with_params(&[i32]).with_returns(&[i32]));
        let callee = names.intern("g");
        let call = Builder::new(&mut func, block).call(callee, sig, &[arg]);
        let got = func[call].first_result.expect("an integer comes back");
        Builder::new(&mut func, block).ret(&[got]);
        assert_eq!(mark(&mut func, &names, &Elsewhere::default(), false, true, true), Ok(0));
        must(&mut func);
        assert_eq!(mark(&mut func, &names, &Elsewhere::default(), false, true, true), Ok(1));
        assert_eq!(opcodes(&func), [Opcode::Alloca, Opcode::TailCall]);
    }

    /// One that cannot be made is an error, with gcc's reason for it.
    #[test]
    fn a_musttail_call_that_cannot_be_made_says_why_in_gcc_s_words() {
        let mut names = Interner::new();
        let setjmp = names.intern("_setjmp");
        let cases: [(Func, &str); 3] = [
            (
                caller(&mut names, |func, block, got| {
                    Builder::new(func, block).binary(Opcode::Add, got, got, Flags::default())
                }),
                "return value changed after call",
            ),
            (caller_of(&mut names, rucc_target::Convention::Ms, |_, _, got| got), UNABLE),
            (
                caller(&mut names, |func, block, got| {
                    let sig = func.add_signature(Signature::new().with_returns(&[Type::int(32)]));
                    Builder::new(func, block).call(setjmp, sig, &[]);
                    got
                }),
                "caller uses setjmp",
            ),
        ];
        for (mut func, reason) in cases {
            let inst = must(&mut func);
            let made = mark(&mut func, &names, &Elsewhere::default(), false, true, true);
            assert_eq!(made, Err(Unsupported::MustTail { inst, reason }));
        }
    }
}
