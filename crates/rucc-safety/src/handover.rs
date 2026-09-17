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
//! has been counting them since before anything emitted a frame at all. [`arrange`] is the other,
//! and it is the pass that actually puts the `cap_publish` and the `cap_clear` in. It has to sort a
//! call the same way the census does or the number in `--emit=safety-summary` stops describing the
//! code that was built. One rule written once is the whole reason this is a module rather than a
//! loop body.
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
//!
//! # Where the capabilities come from
//!
//! [`arrange`] hands over the capabilities the caller already has and says nothing about the rest.
//! It never makes one, and that is the whole shape of it.
//!
//! The reason is that making one after the optimizer costs a `cap_recover`, which is the walk of the
//! lifetime plane that tamnd/rucc#1241 exists to stop paying for. A caller that made a capability
//! for every pointer it passes would pay one walk per pointer per call site, and a callee that
//! checks through one of three arguments would have been paying one. Handing over one the caller
//! already pays for is free, because the producer is already there and the walk already happens, and
//! it turns the callee's walk into a load. Handing over one it does not would be moving the walk
//! rather than removing it, and moving it to the side that does it more often.
//!
//! Already pays for is a narrower thing than already has, and `crate::origin::existing` is where the
//! difference is argued. A capability standing in the IR at this point is not one the build pays
//! for: the optimizer discharges checks and leaves producers with nobody reading them, and the check
//! lowering still drops the capability of every class but `check_live`. Handing one of those over
//! resurrects it, because a publish is a reader. Getting that wrong is worth nine hundred plane
//! walks and eight per cent of the text on the SQLite amalgamation, which is how it was found.
//!
//! So a pointer the caller holds a capability for travels, a pointer it does not is the bottom
//! capability, and the callee recovers that one exactly the way it does today. That makes this pass
//! a strict improvement on the code it replaces rather than a trade, which is what lets the numbers
//! in the pull request mean what they say. The other half of tamnd/rucc#1241 is what makes the
//! caller hold more of them: once a pointer read out of memory has its capability in the aux slot
//! beside it and a pointer an allocator returned has its in the header, a caller holds one for most
//! of what it passes and this pass gets better without changing.
//!
//! The callee side is the same restraint spelled the other way. A `cap_of` over a pointer parameter
//! becomes a `cap_arg`, in place, so the plane walk it was going to lower to becomes a frame read
//! with the same walk behind it as the fallback. Nothing new is made there either: a parameter no
//! check in the function is about still gets no capability.
//!
//! # Which position a capability goes in
//!
//! Its position among the pointer parameters the call's signature names, counting from zero. Both
//! ends have to agree about that number or a callee reads a capability belonging to another of its
//! own arguments, which is a monitor reporting on the wrong memory.
//!
//! Naming is what the two sides have in common, so naming is what the count goes by. The caller
//! counts the parameters its signature for the call declares as pointers, not the arguments that
//! turn out to be pointers, and the callee counts the pointer parameters of its entry block, which
//! are its signature's. A variadic call's tail is therefore not described at all, which is right for
//! a second reason: nothing gives a `va_arg` a capability, so a slot filled for one would be a slot
//! nobody reads.
//!
//! # What it leaves alone
//!
//! A tail call. The frame is an `alloca` in the caller's own stack and a tail call is the caller's
//! stack going away, so there is nowhere for it to live, and [`mod@crate::frame`]'s tie between a
//! publish and its call is written for the two call opcodes that return. A tail call to a callee
//! outside the unit therefore leaves whatever frame was current in place, which is the one hole in
//! the clearing this pass does.
//!
//! It is a small hole and it is worth saying why rather than leaving it to be found. The frame that
//! is current at that point is one somebody already took, because taking is the first thing an
//! instrumented function does and taking clears the magic word, so what an uninstrumented callee
//! calling back into this build finds is a frame that does not believe itself. The case where that
//! is not so is a function with checks and no pointer parameters, which takes nothing, and it is a
//! box on tamnd/rucc#1241 rather than something to paper over here.

use std::collections::HashMap;

use rucc_base::Symbol;
use rucc_ir::{Extra, Func, Imm, Inst, InstData, Module, Opcode, Type, Value};

use crate::frame::ARGS;
use crate::{origin, slot};

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
///
/// What counts as one is a parameter the call's signature declares as a pointer, rather than an
/// argument whose value happens to be one, because the position in the frame has to mean the same
/// thing at the other end and what the two ends share is the declaration. An argument past the end
/// of the signature is a variadic one, and those are not described: nothing gives a `va_arg` a
/// capability, so a slot filled for one would be a slot nobody reads.
pub fn pointers<'a>(func: &'a Func, inst: Inst) -> impl Iterator<Item = Value> + 'a {
    let indirect = usize::from(func[inst].opcode == Opcode::CallIndirect);
    let signature = match func[inst].extra {
        Extra::Call(at) => Some(func[at].signature),
        _ => None,
    };
    func[func[inst].args].iter().skip(indirect).enumerate().filter_map(move |(nth, &value)| {
        let named = func[signature?].params.get(nth)?;
        (named.ty == Type::PTR).then_some(value)
    })
}

/// How many checks of any class one function still has standing.
///
/// A function with none of them never reads the frame its callers set up, which is the whole
/// condition document 05 section 5.3 puts on dropping it.
pub fn checks_left(func: &Func) -> usize {
    all(func)
        .into_iter()
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

/// Puts the frame instructions in, over a whole module, and says how many calls got a publish.
///
/// After the optimizer and in front of [`crate::lower()`], which is not a preference. The rule is a
/// check count and the optimizer is what makes the count small, so running before it would give
/// every callee a frame it no longer needs and would leave the one thing this is measured on, which
/// is how many capabilities stop being a plane walk, describing a build nobody ships. Running after
/// the lowering would be later still, but by then a check is a call and a capability is a stack slot
/// and neither of them is a thing to reason about.
pub fn arrange(module: &mut Module) -> usize {
    // The whole module before any of it is touched, because a callee is allowed to be defined after
    // its caller and the answer has to be the same either way. Nothing below changes a check count:
    // the callee side rewrites capabilities and the caller side adds them, and a check is neither.
    let left = remaining(module);
    let word = Type::int(module.datalayout.pointer_bits);
    let mut published = 0;
    for id in module.funcs() {
        if module[id].is_declaration() {
            continue;
        }
        published += one(&mut module[id], &left, word);
    }
    published
}

/// Both ends of it for one function, callee side first.
///
/// The order matters and is the point rather than a detail. A parameter whose `cap_of` becomes a
/// `cap_arg` is a capability this function now holds, so a pointer it received and passes further
/// down travels the rest of the way as well, and a chain of functions passing a buffer along asks
/// the plane once at the top instead of once per call. Doing the caller side first would give the
/// same chain a bottom capability at every step.
fn one(func: &mut Func, left: &HashMap<Symbol, usize>, word: Type) -> usize {
    if checks_left(func) > 0 {
        from_the_frame(func, word);
    }
    let held = origin::existing(func);
    let mut published = 0;
    for inst in all(func) {
        // A tail call is left alone for the reason the module doc gives, which is that the frame
        // would live in a stack the call is giving up.
        if func[inst].opcode == Opcode::TailCall {
            continue;
        }
        match wanted(func, inst, left) {
            Some(Frame::Checked) => published += usize::from(over(func, inst, &held)),
            Some(Frame::Outside | Frame::Unknown) => empty(func, inst),
            Some(Frame::Elided | Frame::Pointerless) | None => {}
        }
    }
    published
}

/// Turns each pointer parameter's `cap_of` into the `cap_arg` that reads it out of the frame.
///
/// In place, so the value the capability had is the value it still has and nothing that reads it
/// has to be rewritten. The two have the same shape either side of that: one capability out, and an
/// operand naming the pointer, which `cap_arg` keeps because the pointer is the answer when there is
/// no frame to read.
///
/// Only the first [`ARGS`] of them, since a position past the end of the frame is one the runtime
/// answers by recovering, and asking it to do that through a frame read is a call in front of the
/// walk rather than instead of it.
fn from_the_frame(func: &mut Func, word: Type) -> usize {
    let Some(entry) = func.entry() else { return 0 };
    let mut position: HashMap<Value, usize> = HashMap::new();
    let mut at = 0;
    for &param in &func[entry].params {
        if !func[param].ty.is_ptr() {
            continue;
        }
        if at < ARGS {
            position.insert(param, at);
        }
        at += 1;
    }
    if position.is_empty() {
        return 0;
    }
    let mut done = 0;
    for inst in all(func) {
        if func[inst].opcode != Opcode::CapOf {
            continue;
        }
        let Some(&pointer) = func[func[inst].args].first() else { continue };
        let Some(&nth) = position.get(&pointer) else { continue };
        let Ok(nth) = i128::try_from(nth) else { continue };
        let index = slot::konst(func, inst, Imm::int(nth, word), word);
        let args = func.push_values(&[pointer, index]);
        func[inst].opcode = Opcode::CapArg;
        func[inst].args = args;
        done += 1;
    }
    done
}

/// Puts a `cap_publish` in front of a call, or a `cap_clear` when there is nothing to publish.
///
/// Answers whether it published. The list stops at the last pointer there is a capability for rather
/// than running to the end, because a trailing bottom capability and a shorter count say the same
/// thing to the callee and the shorter one is four fewer stores. A list that would be empty is the
/// clear instead, which is the same saving taken all the way and is what the verifier asks for
/// anyway: a publish describing nothing is a clear spelled at length, and the two mean opposite
/// things.
fn over(func: &mut Func, inst: Inst, held: &HashMap<Value, Value>) -> bool {
    let carried: Vec<Value> = pointers(func, inst).take(ARGS).collect();
    let found: Vec<Option<Value>> =
        carried.iter().map(|&value| origin::already(func, held, value)).collect();
    let Some(last) = found.iter().rposition(Option::is_some) else {
        empty(func, inst);
        return false;
    };
    let mut caps = Vec::with_capacity(last + 1);
    for each in &found[..=last] {
        let cap = match *each {
            Some(cap) => cap,
            None => nothing(func, inst),
        };
        caps.push(cap);
    }
    let args = func.push_values(&caps);
    let data = InstData { args, ..InstData::new(Opcode::CapPublish) };
    let made = func.create_inst(data, &[], func.span(inst));
    func.insert_before(made, inst);
    true
}

/// Puts a `cap_clear` in front of a call.
fn empty(func: &mut Func, inst: Inst) {
    let made = func.create_inst(InstData::new(Opcode::CapClear), &[], func.span(inst));
    func.insert_before(made, inst);
}

/// The bottom capability, for a position in the frame the caller has nothing to say about.
fn nothing(func: &mut Func, inst: Inst) -> Value {
    let made = func.create_inst(InstData::new(Opcode::CapNull), &[Type::CAP], func.span(inst));
    func.insert_before(made, inst);
    func[made].results().next().expect("cap_null produces one value")
}

/// Every instruction in the function, in an order that does not borrow it.
fn all(func: &Func) -> Vec<Inst> {
    func.blocks()
        .collect::<Vec<_>>()
        .into_iter()
        .flat_map(|block| func.insts(block).collect::<Vec<Inst>>())
        .collect()
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{Builder, CallInfo, InstData, MemInfo, MemOrder, Restrict, Signature, Type};
    use rucc_target::{Arch, Env, Os, TargetInfo, Triple};

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

    /// A module for the target the rest of these tests are written against.
    fn unit(names: &mut Interner) -> Module {
        let target = TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu));
        Module::new(names.intern("f.c"), &target)
    }

    /// A function named `name` that calls `callee` under `signature`, passing `pass` of its own
    /// parameters, and checks through the first of them when `checks` says so.
    ///
    /// The one shape all of the caller side tests are about. The check is what decides whether the
    /// caller holds a capability to hand over, since the `cap_of` in front of it is the only
    /// producer in the body, and it is also what decides whether a callee wants a frame, so the same
    /// flag builds both ends of a call.
    fn caller(
        names: &mut Interner,
        name: &str,
        callee: Option<&str>,
        signature: Signature,
        checks: bool,
    ) -> Func {
        let word = Type::int(64);
        let params = Signature::new().with_params(&[Type::PTR, word, Type::PTR]);
        let mut func = Func::new(names.intern(name), params);
        let entry = func.create_block();
        let p = func.append_param(entry, Type::PTR);
        let n = func.append_param(entry, word);
        let q = func.append_param(entry, Type::PTR);
        let sig = func.add_signature(signature);
        let callee = callee.map(|each| names.intern(each));
        let varargs = func.push_abis(&[]);
        let info = func.add_call(CallInfo { callee, signature: sig, varargs });
        let mut b = Builder::new(&mut func, entry);
        if checks {
            let args = b.func().push_values(&[p]);
            let cap = b.value(InstData { args, ..InstData::new(Opcode::CapOf) }, Type::CAP);
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
            b.inst(InstData { args, extra, ..InstData::new(Opcode::CheckLive) }, &[]);
        }
        let args = b.func().push_values(&[p, n, q]);
        b.inst(InstData { args, extra: Extra::Call(info), ..InstData::new(Opcode::Call) }, &[]);
        b.ret(&[]);
        func
    }

    /// The signature every caller above calls under, which names two of its three as pointers.
    fn three() -> Signature {
        Signature::new().with_params(&[Type::PTR, Type::int(64), Type::PTR])
    }

    /// How many instructions of `opcode` a function has.
    fn count(func: &Func, opcode: Opcode) -> usize {
        all(func).into_iter().filter(|&inst| func[inst].opcode == opcode).count()
    }

    /// The one instruction of `opcode`.
    fn the(func: &Func, opcode: Opcode) -> Inst {
        all(func)
            .into_iter()
            .find(|&inst| func[inst].opcode == opcode)
            .unwrap_or_else(|| panic!("there is a {opcode:?}"))
    }

    /// The operands of the one instruction of `opcode`.
    fn operands(func: &Func, opcode: Opcode) -> Vec<Value> {
        let inst = the(func, opcode);
        func[func[inst].args].to_vec()
    }

    /// What the one instruction of `opcode` gives back.
    fn produced(func: &Func, opcode: Opcode) -> Value {
        func[the(func, opcode)].results().next().expect("it gives something back")
    }

    #[test]
    fn a_callee_that_still_checks_something_reads_its_parameter_out_of_the_frame() {
        // And the position it reads is the first, because the parameter it checks through is the
        // first of the two its signature names as pointers.
        let mut names = Interner::new();
        let mut module = unit(&mut names);
        module.add_func(caller(&mut names, "f", Some("g"), three(), true));
        arrange(&mut module);
        let func = &module[module.funcs().next().expect("the module defines one")];
        assert_eq!(count(func, Opcode::CapOf), 0);
        assert_eq!(count(func, Opcode::CapArg), 1);
        let args = operands(func, Opcode::CapArg);
        assert_eq!(args.len(), 2);
        assert_eq!(args[0], func[func.entry().expect("an entry")].params[0]);
    }

    #[test]
    fn a_call_into_something_this_unit_does_not_define_says_there_is_no_frame() {
        let mut names = Interner::new();
        let mut module = unit(&mut names);
        module.add_func(caller(&mut names, "f", Some("g"), three(), true));
        assert_eq!(arrange(&mut module), 0);
        let func = &module[module.funcs().next().expect("the module defines one")];
        assert_eq!(count(func, Opcode::CapClear), 1);
        assert_eq!(count(func, Opcode::CapPublish), 0);
    }

    #[test]
    fn a_caller_hands_over_the_capability_it_already_had() {
        // The callee is defined here and checks something, so it wants the frame, and the caller
        // has a capability for the first of the two pointers because it checks through it itself.
        // The second is one it knows nothing about, and the list stops rather than describing it.
        let mut names = Interner::new();
        let mut module = unit(&mut names);
        module.add_func(caller(&mut names, "f", Some("g"), three(), true));
        module.add_func(caller(&mut names, "g", Some("h"), three(), true));
        assert_eq!(arrange(&mut module), 1);
        let func = &module[module.funcs().next().expect("the module defines two")];
        assert_eq!(count(func, Opcode::CapPublish), 1);
        assert_eq!(count(func, Opcode::CapClear), 0);
        let caps = operands(func, Opcode::CapPublish);
        assert_eq!(caps.len(), 1);
        // And what travels is the one the caller was handed itself, so a buffer passed down a chain
        // of functions asks the plane at the top of it and nowhere else.
        assert_eq!(caps[0], produced(func, Opcode::CapArg));
    }

    #[test]
    fn a_caller_holding_nothing_clears_rather_than_publishing_the_bottom_capability() {
        // The two say the same thing to the callee and the clear is the cheap way to say it.
        let mut names = Interner::new();
        let mut module = unit(&mut names);
        module.add_func(caller(&mut names, "f", Some("g"), three(), false));
        module.add_func(caller(&mut names, "g", Some("h"), three(), true));
        assert_eq!(arrange(&mut module), 0);
        let func = &module[module.funcs().next().expect("the module defines two")];
        assert_eq!(count(func, Opcode::CapPublish), 0);
        assert_eq!(count(func, Opcode::CapNull), 0);
        assert_eq!(count(func, Opcode::CapClear), 1);
    }

    #[test]
    fn a_call_into_something_with_nothing_left_to_check_gets_no_frame_at_all() {
        let mut names = Interner::new();
        let mut module = unit(&mut names);
        module.add_func(caller(&mut names, "f", Some("g"), three(), true));
        module.add_func(caller(&mut names, "g", Some("h"), three(), false));
        assert_eq!(arrange(&mut module), 0);
        let func = &module[module.funcs().next().expect("the module defines two")];
        assert_eq!(count(func, Opcode::CapPublish), 0);
        assert_eq!(count(func, Opcode::CapClear), 0);
    }

    #[test]
    fn a_publish_goes_immediately_in_front_of_the_call_it_is_about() {
        // Which is the whole of the tie between the two, so the lowering finds it.
        let mut names = Interner::new();
        let mut module = unit(&mut names);
        module.add_func(caller(&mut names, "f", Some("g"), three(), true));
        module.add_func(caller(&mut names, "g", Some("h"), three(), true));
        arrange(&mut module);
        let func = &module[module.funcs().next().expect("the module defines two")];
        let publish = all(func)
            .into_iter()
            .find(|&inst| func[inst].opcode == Opcode::CapPublish)
            .expect("the call got a frame");
        assert!(crate::frame::placeable(func, publish));
    }

    #[test]
    fn a_variadic_call_describes_only_the_pointers_its_signature_names() {
        // Nothing gives a `va_arg` a capability, so a slot filled for one is a slot nobody reads,
        // and filling it would put the next argument's capability in the wrong place besides.
        let mut names = Interner::new();
        let mut module = unit(&mut names);
        let one = Signature::new().with_params(&[Type::PTR]).variadic();
        module.add_func(caller(&mut names, "f", Some("g"), one, true));
        let func = &module[module.funcs().next().expect("the module defines one")];
        let call = only(func);
        assert_eq!(pointers(func, call).count(), 1);
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
