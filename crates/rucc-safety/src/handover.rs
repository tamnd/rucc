//! Which calls hand their capabilities over, and which say there are none.
//!
//! Design: `spec/safe-memory/05-representation.md` section 5.3.
//!
//! [`mod@crate::frame`] is the lowering of the frame a call's capabilities travel in, and the note
//! at the end of its "what decides which one a call gets" section says the deciding is not there:
//! whether a callee reads a frame is a question about the whole unit, and a lowering is about one
//! instruction. This is the answer to that question, in the one form both readers of it want.
//!
//! There are two of those readers. [`crate::summary`] counts the calls in each bucket, because the
//! rate is what says how much a build pays for the frame and how much of that is avoidable, and it
//! has been counting them since before anything emitted a frame at all. The pass that puts the
//! `cap_publish` and the `cap_clear` in is the other, and it has to sort a call the same way the
//! census does or the number in `--emit=safety-summary` stops describing the code that was built.
//! One rule written once is the whole reason this is a module rather than a loop body.
//!
//! # The rule
//!
//! A callee defined in this unit with no checks left never reads a frame, so the call needs none. A
//! callee defined here that still checks something wants the capabilities. A callee this unit does
//! not define is one nothing here can ask about, and so is a call through a pointer, and both of
//! those get the frame emptied rather than left alone, for the reason the frame module spells out:
//! what is live at that point is the previous call's, and a callee entered holding capabilities
//! belonging to some other call is worse than one entered holding none.
//!
//! A call that hands no pointer over is none of the four, because there was never a capability for
//! it to carry. It is counted separately rather than folded into the first bucket so that the four
//! that remain are all about the callee and the denominator is visible.
//!
//! # Why the check count decides it
//!
//! Because a capability is only ever read by a check. A function with none left has no `cap_arg` in
//! it, takes no frame, and cannot tell whether its caller wrote one. That its callers then leave
//! the frame alone is safe for a reason worth saying out loud rather than assuming: such a function
//! is still compiled by this build, so every call it makes gets a publish or a clear of its own,
//! and anything further down that does take a frame is reached through one of those. The induction
//! bottoms out at a function nothing in this unit defines, which is the `outside` bucket and is
//! cleared.
//!
//! The `restrict` checks are not counted, because what one reads is the scope its own block opened
//! rather than a capability somebody handed over. A function whose only checks are those still
//! wants no frame.

use std::collections::HashMap;

use rucc_base::Symbol;
use rucc_ir::{Extra, Func, Inst, Module, Opcode, Value};

/// What the frame around one call site has to be.
///
/// Five, and every call in a unit is exactly one of them, which is what lets the census add up and
/// print a denominator instead of a percentage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Frame {
    /// The callee is defined here and has no checks left, so it never reads a frame.
    Elided,
    /// The callee is defined here and still checks something, so it needs the capabilities.
    Checked,
    /// The callee is not defined here, so nothing in this unit knows what it checks.
    Outside,
    /// The call goes through a pointer, so there is no callee to ask.
    Unknown,
    /// The call hands no pointer over, so there was never a capability to pass.
    Pointerless,
}

/// How many checks of any class each function this module defines still has standing.
///
/// Its own walk because a callee is allowed to be defined after its caller and the answer has to be
/// the same either way. A name missing from the table is a name this unit does not define, which is
/// what [`wanted`] reads it as.
#[must_use]
pub fn remaining(module: &Module) -> HashMap<Symbol, usize> {
    module
        .funcs()
        .filter(|&id| !module[id].is_declaration())
        .map(|id| (module[id].name, checks_left(&module[id])))
        .collect()
}

/// Which of the five `inst` is, or `None` when it is not a call at all.
///
/// `left` is [`remaining`] over the module this function belongs to.
#[must_use]
pub fn wanted(func: &Func, inst: Inst, left: &HashMap<Symbol, usize>) -> Option<Frame> {
    if !matches!(func[inst].opcode, Opcode::Call | Opcode::TailCall | Opcode::CallIndirect) {
        return None;
    }
    if pointers(func, inst).next().is_none() {
        return Some(Frame::Pointerless);
    }
    let named = callee(func, inst);
    match named.and_then(|name| left.get(&name)) {
        Some(0) => Some(Frame::Elided),
        Some(_) => Some(Frame::Checked),
        None if named.is_none() => Some(Frame::Unknown),
        None => Some(Frame::Outside),
    }
}

/// The name a call goes to, for a call that names one.
///
/// `None` for a call through a pointer, including one that reached here as a `Call` with no callee
/// on it, since reading a name off an indirect call would put one on a list the call does not go to.
#[must_use]
pub fn callee(func: &Func, inst: Inst) -> Option<Symbol> {
    if func[inst].opcode == Opcode::CallIndirect {
        return None;
    }
    match func[inst].extra {
        Extra::Call(at) => func[at].callee,
        _ => None,
    }
}

/// The pointers a call hands over, in the order the frame would hold them.
///
/// The first operand of an indirect call is the address it jumps to, which is a pointer the callee
/// never receives, so it is not one the frame would hold and it is skipped.
pub fn pointers<'a>(func: &'a Func, inst: Inst) -> impl Iterator<Item = Value> + 'a {
    let indirect = usize::from(func[inst].opcode == Opcode::CallIndirect);
    func[func[inst].args].iter().skip(indirect).copied().filter(|&value| func[value].ty.is_ptr())
}

/// How many checks of any class one function still has standing.
///
/// A function with none of them never reads the frame its callers set up, which is the whole
/// condition document 05 section 5.3 puts on dropping it.
pub fn checks_left(func: &Func) -> usize {
    func.blocks()
        .flat_map(|block| func.insts(block).collect::<Vec<Inst>>())
        .filter(|&inst| {
            matches!(
                func[inst].opcode,
                Opcode::CheckBounds
                    | Opcode::CheckLive
                    | Opcode::CheckDeriv
                    | Opcode::CheckType
                    | Opcode::CheckInit
            )
        })
        .count()
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{Builder, CallInfo, InstData, MemInfo, MemOrder, Restrict, Signature, Type};

    use super::*;

    /// A function that calls `main`'s pointer parameter through a pointer, handing it one pointer.
    ///
    /// Built by hand rather than through the builder, because a call through a pointer is the shape
    /// with no helper for it and it is the one this module has a rule of its own about.
    fn through_a_pointer(names: &mut Interner) -> Func {
        let mut func =
            Func::new(names.intern("f"), Signature::new().with_params(&[Type::PTR, Type::PTR]));
        let entry = func.create_block();
        let at = func.append_param(entry, Type::PTR);
        let p = func.append_param(entry, Type::PTR);
        let sig = func.add_signature(Signature::new().with_params(&[Type::PTR]));
        let varargs = func.push_abis(&[]);
        let info = func.add_call(CallInfo { callee: None, signature: sig, varargs });
        let args = func.push_values(&[at, p]);
        let mut b = Builder::new(&mut func, entry);
        let data =
            InstData { args, extra: Extra::Call(info), ..InstData::new(Opcode::CallIndirect) };
        b.inst(data, &[]);
        b.ret(&[]);
        func
    }

    /// The one call in that function.
    fn only(func: &Func) -> Inst {
        func.blocks()
            .flat_map(|block| func.insts(block).collect::<Vec<Inst>>())
            .find(|&inst| matches!(func[inst].opcode, Opcode::Call | Opcode::CallIndirect))
            .expect("the function calls something")
    }

    #[test]
    fn the_address_an_indirect_call_jumps_to_is_not_one_of_the_pointers_it_hands_over() {
        // Two pointer operands and one pointer argument. The callee never receives the address it
        // was reached through, so a frame that held it would put every argument in the wrong slot.
        let mut names = Interner::new();
        let func = through_a_pointer(&mut names);
        let call = only(&func);
        assert_eq!(pointers(&func, call).count(), 1);
        assert_eq!(callee(&func, call), None);
    }

    #[test]
    fn a_call_through_a_pointer_is_one_nothing_here_can_ask_about() {
        let mut names = Interner::new();
        let func = through_a_pointer(&mut names);
        let call = only(&func);
        assert_eq!(wanted(&func, call, &HashMap::new()), Some(Frame::Unknown));
    }

    #[test]
    fn something_that_is_not_a_call_is_none_of_the_five() {
        let mut names = Interner::new();
        let func = through_a_pointer(&mut names);
        let ret = func
            .blocks()
            .flat_map(|block| func.insts(block).collect::<Vec<Inst>>())
            .find(|&inst| func[inst].opcode == Opcode::Return)
            .expect("the function returns");
        assert_eq!(wanted(&func, ret, &HashMap::new()), None);
    }

    /// A function holding one check of `opcode` over its own parameter.
    fn checking(names: &mut Interner, opcode: Opcode) -> Func {
        let mut func = Func::new(names.intern("g"), Signature::new().with_params(&[Type::PTR]));
        let entry = func.create_block();
        let p = func.append_param(entry, Type::PTR);
        let mut b = Builder::new(&mut func, entry);
        let cap = b.value(InstData::new(Opcode::CapNull), Type::CAP);
        let args = b.func().push_values(&[cap, p]);
        let info = MemInfo {
            size: 4,
            align: 4,
            order: MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        };
        let extra = Extra::Mem(b.func().add_mem(info));
        b.inst(InstData { args, extra, ..InstData::new(opcode) }, &[]);
        b.ret(&[]);
        func
    }

    #[test]
    fn every_class_of_check_is_one_that_keeps_the_frame() {
        let mut names = Interner::new();
        for opcode in [
            Opcode::CheckBounds,
            Opcode::CheckLive,
            Opcode::CheckDeriv,
            Opcode::CheckType,
            Opcode::CheckInit,
        ] {
            let func = checking(&mut names, opcode);
            assert_eq!(checks_left(&func), 1, "{opcode:?}");
        }
    }

    #[test]
    fn a_function_whose_only_checks_are_restrict_promises_wants_no_frame() {
        // What one of those reads is the scope its own block opened rather than a capability
        // somebody handed over, so a caller has nothing to hand it.
        let mut names = Interner::new();
        for opcode in [Opcode::CheckRestrictRead, Opcode::CheckRestrictWrite] {
            let func = checking(&mut names, opcode);
            assert_eq!(checks_left(&func), 0, "{opcode:?}");
        }
    }
}
