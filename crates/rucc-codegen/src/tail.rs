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
//! [`mark`] works on the IR, before selection. It turns a direct call whose results are exactly
//! what the block returns next into a `tail_call`, which ends the block the way a `return` did, and
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
use rucc_target::{FrameInsts, RegClass};

/// The functions a call to which comes back more than once, which is what `setjmp` is and what
/// gcc's `special_function_p` lists. A name with underscores in front of it is the same function.
///
/// Control coming back into the caller a second time needs the caller's frame, so a caller that
/// makes one of these calls anywhere makes no tail call at all.
const TWICE: &[&str] = &["setjmp", "sigsetjmp", "savectx", "vfork", "getcontext"];

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
#[must_use]
pub fn refusal(func: &Func, names: &Interner) -> Option<&'static str> {
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
                Opcode::Call if twice(func, inst, names) => {
                    return Some("the function calls something that comes back twice");
                }
                _ => (),
            }
        }
    }
    None
}

/// Whether that call is to one of [`TWICE`].
fn twice(func: &Func, inst: Inst, names: &Interner) -> bool {
    let Extra::Call(info) = func[inst].extra else { return false };
    let Some(callee) = func[info].callee else { return false };
    TWICE.contains(&names.resolve(callee).trim_start_matches('_'))
}

/// Turns every call in tail position into a `tail_call`, unless [`refusal`] has a reason not to,
/// and says how many it turned.
pub fn mark(func: &mut Func, names: &Interner) -> usize {
    if refusal(func, names).is_some() {
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
/// A direct call, because the address of an indirect one is in a register and the epilogue may put
/// something else back in it. The `return` gives back the call's results, in order, and nothing
/// else, and the two signatures say the same about them, so the callee leaves the answer where the
/// caller's caller looks and in the form it expects.
fn in_tail_position(func: &Func, call: Inst, ret: Inst) -> bool {
    if func[ret].opcode != Opcode::Return || func[call].opcode != Opcode::Call {
        return false;
    }
    let Extra::Call(info) = func[call].extra else { return false };
    let info = func[info];
    if info.callee.is_none() {
        return false;
    }
    let results: Vec<Value> = func[call].results().collect();
    if func[func[ret].args] != results[..] {
        return false;
    }
    func[info.signature].returns == func.signature().returns
}

/// Turns each [`Tail`] that can be into the epilogue and a jump, and says how many it turned.
///
/// Nothing happens on a machine with no jump to a name, which is what [`FrameInsts::away`] says.
pub fn jumps(
    func: &mut mir::Func,
    tails: &[Tail],
    insts: &FrameInsts,
    names: &mut Interner,
) -> usize {
    let Some(away) = insts.away else { return 0 };
    let ret = mir::Opcode::new(names.intern(&format!("{}{}", insts.prefix, insts.ret)));
    let away = mir::Opcode::new(names.intern(&format!("{}{away}", insts.prefix)));
    let mut jumped = 0;
    for tail in tails {
        let Some((last, callee)) = ending(func, tail, ret) else { continue };
        let span = func.span(tail.call);
        func.remove_inst(tail.call);
        for &pseudo in &tail.returns {
            func.remove_inst(pseudo);
        }
        // The same instruction rather than a new one, so the unwind rows the epilogue hung on the
        // `ret` stay where they were: the frame is in the same state at the jump as it was there.
        func[last].opcode = away;
        func[last].symbol = Some(callee);
        func.set_span(last, span);
        jumped += 1;
    }
    jumped
}

/// The `ret` the tail's block ends in and the name it calls, when everything between the call and
/// the `ret` can run before the callee does.
///
/// That is the return pseudos, which are nothing, and the epilogue, which puts back registers the
/// callee saves for itself and moves the stack pointer. Anything that reads or writes a register
/// the call names could be moving an argument or reading the answer, and anything with a name on
/// it could be a call, so either keeps the call.
fn ending(func: &mir::Func, tail: &Tail, ret: mir::Opcode) -> Option<(mir::Inst, Symbol)> {
    let block = func.block_of(tail.call)?;
    let callee = func[tail.call].symbol?;
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
    use rucc_ir::{Block, Builder, Flags, Func, InstData, Opcode, Signature, Type, Value};

    use super::{mark, refusal};

    /// `int f(int a) { return g(a); }`, and whatever `between` puts in front of the return.
    fn caller(
        names: &mut Interner,
        between: impl FnOnce(&mut Func, Block, Value) -> Value,
    ) -> Func {
        let i32 = Type::int(32);
        let mut func =
            Func::new(names.intern("f"), Signature::new().with_params(&[i32]).with_returns(&[i32]));
        let block = func.create_block();
        let arg = func.append_param(block, i32);
        let sig = func.add_signature(Signature::new().with_params(&[i32]).with_returns(&[i32]));
        let callee = names.intern("g");
        let call = Builder::new(&mut func, block).call(callee, sig, &[arg]);
        let got = func[call].first_result.expect("an integer comes back");
        let answer = between(&mut func, block, got);
        Builder::new(&mut func, block).ret(&[answer]);
        func
    }

    fn opcodes(func: &Func) -> Vec<Opcode> {
        func.blocks()
            .flat_map(|block| func.insts(block).map(|inst| func[inst].opcode).collect::<Vec<_>>())
            .collect()
    }

    #[test]
    fn a_call_whose_answer_is_returned_becomes_a_tail_call() {
        let mut names = Interner::new();
        let mut func = caller(&mut names, |_, _, got| got);
        assert_eq!(mark(&mut func, &names), 1);
        assert_eq!(opcodes(&func), [Opcode::TailCall]);
    }

    #[test]
    fn a_call_with_work_after_it_is_not_in_tail_position() {
        let mut names = Interner::new();
        let mut func = caller(&mut names, |func, block, got| {
            Builder::new(func, block).binary(Opcode::Add, got, got, Flags::default())
        });
        assert_eq!(mark(&mut func, &names), 0);
        assert_eq!(opcodes(&func), [Opcode::Call, Opcode::Add, Opcode::Return]);
    }

    #[test]
    fn a_local_in_the_frame_turns_down_the_whole_function() {
        let mut names = Interner::new();
        let mut func = caller(&mut names, |func, block, got| {
            Builder::new(func, block).value(InstData::new(Opcode::Alloca), Type::PTR);
            got
        });
        assert_eq!(refusal(&func, &names), Some("a local lives in the frame"));
        assert_eq!(mark(&mut func, &names), 0);
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
            refusal(&func, &names),
            Some("the function calls something that comes back twice")
        );
        assert_eq!(mark(&mut func, &names), 0);
    }
}
