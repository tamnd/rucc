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
//! The one callee this unit does not define and still knows about is a wrapper `crate::wrap` sent a
//! call to. `rucc-safe-rt` defines every one of them and every one of them takes its frame, so a
//! call to one is sorted as a callee that still checks something, which is what it is.
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
//! It makes one only for a local, and that is the whole shape of it.
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
//! capability, and the callee recovers that one exactly the way it does today. The one exception is
//! a pointer into one of the caller's own locals, whose capability is its address and its size and
//! so costs no walk to make, and which recovery cannot describe at all. That makes this pass
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
//! # The pointer that comes back
//!
//! The same pair with the sides swapped, which [`mod@crate::frame`] is where the frame slot for it
//! is argued. A callee that holds a capability for the pointer it returns says so with a `cap_yield`
//! in front of the return, and a caller reads it with a `cap_result` behind the call.
//!
//! Both ends are a rewrite in place rather than something new, for the reason the argument side is.
//! The caller's end is the `cap_of` sitting directly behind the call, which is where
//! `crate::origin` puts a capability for a value an instruction produced, and turning that into a
//! `cap_result` swaps a plane walk for a frame read with the same walk behind it as the fallback. A
//! call whose result nothing in the caller has a capability for is left alone, so a pointer that was
//! never going to be checked does not start paying for a frame read.
//!
//! The callee's end makes nothing either. It yields a capability the function is already holding for
//! some other reason, and a returned pointer nothing in the body checks has none, so the caller's
//! fallback is what answers. That is the same restraint as everywhere else here, and it is why the
//! two ends can be put in independently: a yield with no reader writes a slot nobody looks at, and a
//! result with no yield reads the bottom capability the publish left and recovers.
//!
//! A returned pointer only travels out of a call that published, since the slot it travels in is in
//! the frame the publish filled. That rules out a callee with nothing left to check, and that is not
//! a gap for the reason the check count section gives: such a callee holds no capability at all, so
//! there was never anything for it to yield.
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

use rucc_base::hash::Map;
use rucc_base::{Interner, Symbol};
use rucc_ir::{
    BlockCall, Def, Doms, Extra, Func, Imm, Inst, InstData, Module, Opcode, Type, Value,
};

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
///
/// A wrapper `crate::wrap` sent a call to counts one too, though nothing here defines it. It is
/// `rucc-safe-rt`'s, it takes its frame, and it judges the range it was handed against the
/// capability it finds there, which for a local is the only thing that knows how big the local is:
/// no region covers the stack, so the planes the wrapper otherwise asks have nothing to say. A
/// clear in front of the call was a `memcpy` into a local that nothing judged at all.
///
/// A function that returns the address of one of its own locals counts one more, because that is
/// a capability it will yield whether or not any check is left in it, and a caller that cleared
/// instead of publishing would never read it. That is row T4 once the optimizer has taken out every
/// check in the function, which in the usual shape of the bug it has.
#[must_use]
pub fn remaining(module: &Module, names: &Interner) -> Map<Symbol, usize> {
    // Found rather than interned, since a wrapper this unit never names is one no call goes to.
    let wrappers = crate::wrap::INTERPOSED
        .iter()
        .filter_map(|name| names.find(&[crate::wrap::PREFIX, name].concat()))
        .map(|symbol| (symbol, 1));
    let defined = module.funcs().filter(|&id| !module[id].is_declaration()).map(|id| {
        let func = &module[id];
        (func.name, checks_left(func) + usize::from(returns_a_local(func)))
    });
    wrappers.chain(defined).collect()
}

/// Whether some `return` in `func` gives back the address of a local.
fn returns_a_local(func: &Func) -> bool {
    all(func).into_iter().any(|inst| {
        func[inst].opcode == Opcode::Return
            && func[func[inst].args].iter().any(|&value| local(func, value))
    })
}

/// Whether `value` is the address an `alloca` produced, which is the shape `crate::slot` builds a
/// local's capability out of, with the size it was declared with or the size the stack grew by.
fn local(func: &Func, value: Value) -> bool {
    let Def::Result { inst, .. } = func[value].def else { return false };
    func[inst].opcode == Opcode::Alloca
}

/// Which of the five `inst` is, or `None` when it is not a call at all.
///
/// `left` is [`remaining`] over the module this function belongs to.
#[must_use]
pub fn wanted(func: &Func, inst: Inst, left: &Map<Symbol, usize>) -> Option<Frame> {
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
pub fn arrange(module: &mut Module, names: &Interner) -> usize {
    // The whole module before any of it is touched, because a callee is allowed to be defined after
    // its caller and the answer has to be the same either way. Nothing below changes a check count:
    // the callee side rewrites capabilities and the caller side adds them, and a check is neither.
    let left = remaining(module, names);
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

/// Both ends of it for one function: the parameters, then the calls, then the returns.
///
/// The one bit of order that bites is the table, which is taken before anything here puts a reader
/// in. `origin::existing` only reports a capability something is going to read anyway, and a publish
/// and a yield are both readers, so asking after the publishes would report capabilities that are
/// only alive because this pass handed them somewhere. That is a plane walk this pass created, which
/// is the thing it exists to stop paying for. Taking the table first makes the answer describe the
/// function as the optimizer left it.
///
/// The rest is a sequence rather than a dependency. All four rewrites keep the value a capability
/// had, so a pointer that came in as a parameter, went down into a call and came back out of another
/// is the same value at each step and travels the whole way as a frame read however this is ordered.
/// Writing it in the order the values flow is for the reader.
fn one(func: &mut Func, left: &Map<Symbol, usize>, word: Type) -> usize {
    if checks_left(func) > 0 {
        from_the_frame(func, word);
    }
    let held = origin::existing(func);
    let Some(doms) = func.entry().map(|_| Doms::new(func)) else { return 0 };
    let mut joins = Map::default();
    let mut published = 0;
    for inst in all(func) {
        // A tail call is left alone for the reason the module doc gives, which is that the frame
        // would live in a stack the call is giving up.
        if func[inst].opcode == Opcode::TailCall {
            continue;
        }
        match wanted(func, inst, left) {
            Some(Frame::Checked) => {
                if over(func, inst, &held, &doms, &mut joins) {
                    published += 1;
                    from_the_call(func, inst);
                }
            }
            // A call that hands nothing over and gets a pointer back still wants a frame, because
            // the frame is where the callee leaves the capability of what it returned. Without
            // one the caller recovers a pointer the callee could have described exactly, and for
            // a pointer to one of the callee's own locals that is the whole of row T4.
            Some(Frame::Pointerless) if returning(func, inst) && reads_frame(func, inst, left) => {
                if over(func, inst, &held, &doms, &mut joins) {
                    published += 1;
                    from_the_call(func, inst);
                }
            }
            Some(Frame::Outside | Frame::Unknown) => empty(func, inst),
            Some(Frame::Elided | Frame::Pointerless) | None => {}
        }
    }
    giving_back(func, &held, &doms);
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
    let mut position: Map<Value, usize> = Map::default();
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

/// Turns the `cap_of` behind a call into the `cap_result` that reads what the callee yielded.
///
/// Answers whether it did. Only the one directly behind the call, which is not a restriction being
/// accepted so much as the shape the two things already have: `crate::origin` puts a capability for
/// a value an instruction produced immediately behind that instruction, and `crate::frame`'s tie
/// between a result and its call asks for exactly that position, because a result behind anything
/// else would be reading whatever the previous call site left in the slot.
///
/// In place and without touching the operands, since the two opcodes have the same shape: one
/// capability out and the pointer it is about in. `cap_result` keeps the pointer for the reason
/// `cap_arg` keeps one, which is that it is the answer when the callee wrote nothing.
///
/// Called only where [`over`] published, so the frame the result reads is one this call site filled.
fn from_the_call(func: &mut Func, inst: Inst) -> bool {
    let Some(result) = func[inst].results().next() else { return false };
    if !func[result].ty.is_ptr() {
        return false;
    }
    let Some(next) = behind(func, inst) else { return false };
    if func[next].opcode != Opcode::CapOf {
        return false;
    }
    if func[func[next].args].first() != Some(&result) {
        return false;
    }
    func[next].opcode = Opcode::CapResult;
    true
}

/// Puts a `cap_yield` in front of each return that gives back a pointer this function has one for.
///
/// Answers how many it put in. In front of the return rather than anywhere else, because what the
/// instruction says is about the value leaving by that one return and a yield on a path that is not
/// taken would write the caller's frame for a pointer it never receives.
///
/// The first pointer among the returned values, since the frame has one slot for the answer. C
/// returns one value, so the loop is there to make the choice visible rather than because there is
/// anything to choose between.
///
/// Nothing is made here, as everywhere else in this pass, with one exception: `held` holds the
/// capabilities the function is paying for already, and a returned pointer that is not in it leaves
/// the caller reading the bottom capability the publish wrote, which is the recovery the caller was
/// doing anyway. The exception is the address of a local, which gets a `cap_of` made for it in
/// front of the return. Recovery has nothing to say about a stack address, and the capability is
/// the only thing that can tell the caller the frame it points into is about to go, which is T4.
fn giving_back(func: &mut Func, held: &Map<Value, Value>, doms: &Doms) -> usize {
    let mut done = 0;
    for inst in all(func) {
        if func[inst].opcode != Opcode::Return {
            continue;
        }
        let returned: Vec<Value> = func[func[inst].args].to_vec();
        let Some(&pointer) = returned.iter().find(|&&value| func[value].ty.is_ptr()) else {
            continue;
        };
        let cap = match seen(func, held, doms, pointer, inst) {
            Some(cap) => cap,
            None if local(func, pointer) => {
                let args = func.push_values(&[pointer]);
                let data = InstData { args, ..InstData::new(Opcode::CapOf) };
                let made = func.create_inst(data, &[Type::CAP], func.span(inst));
                func.insert_before(made, inst);
                func[made].results().next().expect("cap_of produces one value")
            }
            None => continue,
        };
        let args = func.push_values(&[cap]);
        let data = InstData { args, ..InstData::new(Opcode::CapYield) };
        let made = func.create_inst(data, &[], func.span(inst));
        func.insert_before(made, inst);
        done += 1;
    }
    done
}

/// A capability for the local `pointer` points into, made in front of `inst`, when it points into
/// one.
///
/// The exception [`giving_back`] makes for a returned local, made for a passed one. The optimizer
/// takes out every check a caller makes on its own local when they are all in bounds, and with them
/// the capability, so at `-O2` a buffer that is written once and handed down arrives at a callee
/// that still checks with nothing in the frame. The callee then recovers, and recovery has nothing
/// to say about a stack address, so a callee reading four bytes through a pointer to a `char` was
/// refused at `-O0` and let through at `-O2`. The capability of a local is its address and its
/// size, which is no plane walk at all, so making one here costs a call and buys the callee its
/// bounds.
///
/// Over the local rather than over `pointer`, so that a pointer into the middle of a buffer carries
/// the whole buffer's bounds, which is the same reason [`origin::already`] answers by the base.
///
/// A pointer that arrives at a block parameter is a local too when every edge into the block passes
/// one or a null, and [`joined`] makes its capability. `joins` keeps the ones already made, so that
/// two calls handing over the same parameter share one.
fn made(
    func: &mut Func,
    pointer: Value,
    inst: Inst,
    joins: &mut Map<Value, Value>,
) -> Option<Value> {
    let base = origin::root(func, pointer);
    if !local(func, base) {
        if let Some(&cap) = joins.get(&base) {
            return Some(cap);
        }
        let cap = joined(func, base)?;
        joins.insert(base, cap);
        return Some(cap);
    }
    let args = func.push_values(&[base]);
    let data = InstData { args, ..InstData::new(Opcode::CapOf) };
    let cap = func.create_inst(data, &[Type::CAP], func.span(inst));
    func.insert_before(cap, inst);
    func[cap].results().next()
}

/// The capability of a block parameter that every edge into its block passes a local or a null,
/// as a capability parameter beside it.
///
/// That is what a pointer set on one side of a branch and not the other is once the optimizer has
/// run: `char *data; if (flag) data = buffer; strcpy(data, ...)` leaves `data` a parameter of the
/// block after the branch, passed the buffer on one edge and, since reading it on the other would
/// be reading nothing, a null on the other. The same goes for a pointer set to one of two buffers.
/// Each edge passes the capability of what it passes, the local's made in front of its branch and
/// a null's the bottom one, so the parameter carries the bounds of whichever buffer it arrived
/// with, and nothing on the way is a plane walk.
///
/// Not the entry block, whose parameters are the function's own, and not when any edge passes
/// something else, since its capability would be a walk this pass does not make.
fn joined(func: &mut Func, pointer: Value) -> Option<Value> {
    let Def::Param { block, index } = func[pointer].def else { return None };
    if func.entry() == Some(block) {
        return None;
    }
    let index = index as usize;
    let mut edges = Vec::new();
    for pred in func.blocks().collect::<Vec<_>>() {
        let Some(term) = func.terminator(pred) else { continue };
        for place in func.target_list(term).iter() {
            let call = func[place];
            if call.block != block {
                continue;
            }
            let base = origin::root(func, *func[call.args].get(index)?);
            if !local(func, base) && !null(func, base) {
                return None;
            }
            edges.push((term, place, base));
        }
    }
    if edges.is_empty() {
        return None;
    }
    let cap = func.append_param(block, Type::CAP);
    for (term, place, base) in edges {
        let data = if local(func, base) {
            let args = func.push_values(&[base]);
            InstData { args, ..InstData::new(Opcode::CapOf) }
        } else {
            InstData::new(Opcode::CapNull)
        };
        let made = func.create_inst(data, &[Type::CAP], func.span(term));
        func.insert_before(made, term);
        let passed = func[made].results().next()?;
        let call = func[place];
        let mut args = func[call.args].to_vec();
        args.push(passed);
        let args = func.push_values(&args);
        func.set_block_call(place, BlockCall { args, ..call });
    }
    Some(cap)
}

/// Whether `value` is a null pointer, which is a conversion of a zero because `iconst` never makes
/// a pointer.
fn null(func: &Func, value: Value) -> bool {
    let Def::Result { inst, .. } = func[value].def else { return false };
    if func[inst].opcode != Opcode::IntToPtr {
        return false;
    }
    func[func[inst].args].first().is_some_and(|&zero| crate::is_zero(func, zero))
}

/// The instruction directly behind `inst` in the block it is in.
fn behind(func: &Func, inst: Inst) -> Option<Inst> {
    let block = func.block_of(inst)?;
    let mut after = func.insts(block).skip_while(|&at| at != inst);
    after.next();
    after.next()
}

/// Puts a `cap_publish` in front of a call, or a `cap_clear` when there is nothing to publish.
///
/// Answers whether it published. The list stops at the last pointer there is a capability for rather
/// than running to the end, because a trailing bottom capability and a shorter count say the same
/// thing to the callee and the shorter one is four fewer stores. A list that would be empty is the
/// clear instead, which is the same saving taken all the way and is what the verifier asks for
/// anyway: a publish describing nothing is a clear spelled at length, and the two mean opposite
/// things. The exception is a call that gives back a pointer, whose frame is where the answer comes
/// back, so that one publishes a single bottom capability rather than clearing.
fn over(
    func: &mut Func,
    inst: Inst,
    held: &Map<Value, Value>,
    doms: &Doms,
    joins: &mut Map<Value, Value>,
) -> bool {
    let carried: Vec<Value> = pointers(func, inst).take(ARGS).collect();
    let mut found: Vec<Option<Value>> = Vec::with_capacity(carried.len());
    for &value in &carried {
        let cap = match seen(func, held, doms, value, inst) {
            Some(cap) => Some(cap),
            None => made(func, value, inst, joins),
        };
        found.push(cap);
    }
    // Nothing to hand over is still a frame worth publishing when a pointer comes back, since the
    // callee's answer goes into it. It describes one bottom capability rather than none, which says
    // the same to the callee and keeps the publish one the verifier can tell from a clear.
    let given = match found.iter().rposition(Option::is_some) {
        Some(last) => last + 1,
        None if returning(func, inst) => 0,
        None => {
            empty(func, inst);
            return false;
        }
    };
    let mut caps = Vec::with_capacity(given.max(1));
    if given == 0 {
        caps.push(nothing(func, inst));
    }
    for each in &found[..given] {
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

/// The capability [`origin::existing`] found for `pointer`, when it is one `inst` can read.
///
/// The table holds the first capability of each pointer in the order the blocks are laid out, and
/// after the optimizer that need not be one the instruction can see. A parameter's `cap_of` goes at
/// the top of the function, but sinking can move it into the blocks that check through the pointer,
/// and a call in front of those would then publish a slot nothing has written yet. The callee reads
/// whatever an earlier call left there. So a capability that does not dominate the instruction
/// counts as none, which leaves the callee to recover, the same as for a pointer nothing checked.
fn seen(
    func: &Func,
    held: &Map<Value, Value>,
    doms: &Doms,
    pointer: Value,
    inst: Inst,
) -> Option<Value> {
    let cap = origin::already(func, held, pointer)?;
    let block = func.block_of(inst)?;
    let visible = match func[cap].def {
        Def::Param { block: from, .. } => doms.dominates(from, block),
        Def::Result { inst: made, .. } => match func.block_of(made) {
            Some(from) if from == block => {
                func.insts(block).take_while(|&at| at != inst).any(|at| at == made)
            }
            Some(from) => doms.dominates(from, block),
            None => false,
        },
    };
    visible.then_some(cap)
}

/// Whether `inst` gives back a pointer the caller makes a capability for, which is what makes the
/// frame around it worth reading after it returns.
///
/// The same shape [`from_the_call`] turns into the read, so a call whose result nobody checks goes
/// on paying nothing for it.
fn returning(func: &Func, inst: Inst) -> bool {
    let Some(result) = func[inst].results().next() else { return false };
    func[result].ty.is_ptr()
        && behind(func, inst).is_some_and(|next| {
            func[next].opcode == Opcode::CapOf && func[func[next].args].first() == Some(&result)
        })
}

/// Whether `inst` calls a function this unit defines that still has a check standing, which is
/// one that takes its frame and so one that can leave a capability in it.
fn reads_frame(func: &Func, inst: Inst, left: &Map<Symbol, usize>) -> bool {
    callee(func, inst).and_then(|name| left.get(&name)).is_some_and(|&n| n > 0)
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
    func.blocks().flat_map(|block| func.insts(block)).collect()
}

#[cfg(test)]
mod tests {
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
            .flat_map(|block| func.insts(block))
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
        assert_eq!(wanted(&func, call, &Map::default()), Some(Frame::Unknown));
    }

    #[test]
    fn something_that_is_not_a_call_is_none_of_the_five() {
        let mut names = Interner::new();
        let func = through_a_pointer(&mut names);
        let ret = func
            .blocks()
            .flat_map(|block| func.insts(block))
            .find(|&inst| func[inst].opcode == Opcode::Return)
            .expect("the function returns");
        assert_eq!(wanted(&func, ret, &Map::default()), None);
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
        arrange(&mut module, &names);
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
        assert_eq!(arrange(&mut module, &names), 0);
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
        assert_eq!(arrange(&mut module, &names), 1);
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
    fn a_capability_taken_only_after_the_call_is_not_handed_to_it() {
        // The shape sinking leaves behind: the call comes first and the only `cap_of` of the
        // pointer it passes is in the block after it. Publishing that one would copy a slot nothing
        // has written yet, so the call says it has nothing, as it would for an unchecked pointer.
        let mut names = Interner::new();
        let mut module = unit(&mut names);
        let mut func = Func::new(names.intern("f"), three());
        let entry = func.create_block();
        let later = func.create_block();
        let p = func.append_param(entry, Type::PTR);
        let n = func.append_param(entry, Type::int(64));
        let q = func.append_param(entry, Type::PTR);
        let sig = func.add_signature(three());
        let varargs = func.push_abis(&[]);
        let callee = Some(names.intern("g"));
        let info = func.add_call(CallInfo { callee, signature: sig, varargs });
        let mut b = Builder::new(&mut func, entry);
        let args = b.func().push_values(&[p, n, q]);
        b.inst(InstData { args, extra: Extra::Call(info), ..InstData::new(Opcode::Call) }, &[]);
        b.jump(later, &[]);
        let mut b = Builder::new(&mut func, later);
        checked(&mut b, p);
        b.ret(&[]);
        module.add_func(func);
        module.add_func(caller(&mut names, "g", Some("h"), three(), true));
        assert_eq!(arrange(&mut module, &names), 0);
        let func = &module[module.funcs().next().expect("the module defines two")];
        assert_eq!(count(func, Opcode::CapPublish), 0);
        assert_eq!(count(func, Opcode::CapClear), 1);
        assert_eq!(count(func, Opcode::CapArg), 1);
    }

    #[test]
    fn a_caller_holding_nothing_clears_rather_than_publishing_the_bottom_capability() {
        // The two say the same thing to the callee and the clear is the cheap way to say it.
        let mut names = Interner::new();
        let mut module = unit(&mut names);
        module.add_func(caller(&mut names, "f", Some("g"), three(), false));
        module.add_func(caller(&mut names, "g", Some("h"), three(), true));
        assert_eq!(arrange(&mut module, &names), 0);
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
        assert_eq!(arrange(&mut module, &names), 0);
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
        arrange(&mut module, &names);
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

    /// The signature of everything below, which takes a pointer and gives one back.
    fn one_each() -> Signature {
        Signature::new().with_params(&[Type::PTR]).with_returns(&[Type::PTR])
    }

    /// A lifetime check of `pointer` through a `cap_of` taken for it right there.
    ///
    /// The shape the insertion pass leaves behind, and the check is the part that matters to these
    /// tests: a capability nothing reads is one [`origin::existing`] will not report, so a producer
    /// without one would make every assertion below about the wrong thing.
    fn checked(b: &mut Builder<'_>, pointer: Value) -> Value {
        let args = b.func().push_values(&[pointer]);
        let cap = b.value(InstData { args, ..InstData::new(Opcode::CapOf) }, Type::CAP);
        let args = b.func().push_values(&[cap, pointer]);
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
        cap
    }

    /// A function named `name` that asks `callee` for a pointer and hands the same one on.
    ///
    /// Both ends of the returned pointer in one body, which is how they most often turn up. The
    /// `cap_of` directly behind the call is where `crate::origin` puts a capability for a value an
    /// instruction produced. Its own parameter is checked as well, because that is what gives the
    /// call a capability to publish, and the slot a result is read out of lives in a frame a publish
    /// filled.
    fn passing_on(names: &mut Interner, name: &str, callee: Option<&str>, checks: bool) -> Func {
        let mut func = Func::new(names.intern(name), one_each());
        let entry = func.create_block();
        let p = func.append_param(entry, Type::PTR);
        let sig = func.add_signature(one_each());
        let callee = callee.map(|each| names.intern(each));
        let varargs = func.push_abis(&[]);
        let info = func.add_call(CallInfo { callee, signature: sig, varargs });
        let mut b = Builder::new(&mut func, entry);
        if checks {
            checked(&mut b, p);
        }
        let args = b.func().push_values(&[p]);
        let data = InstData { args, extra: Extra::Call(info), ..InstData::new(Opcode::Call) };
        let got = b.value(data, Type::PTR);
        if checks {
            checked(&mut b, got);
        }
        b.ret(&[got]);
        func
    }

    /// A module holding `f`, which passes a pointer on, and the `g` it asks for one.
    fn both_ends(names: &mut Interner, checks: bool) -> Module {
        let mut module = unit(names);
        module.add_func(passing_on(names, "f", Some("g"), checks));
        module.add_func(passing_on(names, "g", Some("h"), true));
        module
    }

    #[test]
    fn a_pointer_a_call_gave_back_is_read_out_of_the_frame_rather_than_recovered() {
        // The `cap_of` behind the call becomes the `cap_result`, in place, so the value it produced
        // is the value the check goes on reading. One of the two `cap_of` in the body is over the
        // parameter and becomes a `cap_arg`, which is what leaves nothing of the opcode behind.
        let mut names = Interner::new();
        let mut module = both_ends(&mut names, true);
        assert_eq!(arrange(&mut module, &names), 1);
        let func = &module[module.funcs().next().expect("the module defines two")];
        assert_eq!(count(func, Opcode::CapResult), 1);
        assert_eq!(count(func, Opcode::CapOf), 0);
        // And the pointer it names is the one the call gave back, which is what the runtime falls
        // back to when the callee wrote nothing.
        let args = operands(func, Opcode::CapResult);
        assert_eq!(args.len(), 1);
        assert_eq!(args[0], func[only(func)].results().next().expect("the call gives one back"));
    }

    #[test]
    fn the_result_sits_behind_a_call_that_was_published_to() {
        // Which is the whole of the tie the lowering asks for, and the reason it is two back rather
        // than one: a cleared call has no frame, so there would be nothing in the slot to read.
        let mut names = Interner::new();
        let mut module = both_ends(&mut names, true);
        arrange(&mut module, &names);
        let func = &module[module.funcs().next().expect("the module defines two")];
        assert!(crate::frame::given(func, the(func, Opcode::CapResult)));
    }

    #[test]
    fn a_function_yields_the_capability_of_the_pointer_it_gives_back() {
        // The one it already holds, which here is the one it read out of the frame itself, so a
        // pointer handed along a chain of functions asks the plane once however long the chain is.
        let mut names = Interner::new();
        let mut module = both_ends(&mut names, true);
        arrange(&mut module, &names);
        let func = &module[module.funcs().next().expect("the module defines two")];
        assert_eq!(count(func, Opcode::CapYield), 1);
        let args = operands(func, Opcode::CapYield);
        assert_eq!(args.len(), 1);
        assert_eq!(args[0], produced(func, Opcode::CapResult));
        // In front of the return it is about rather than anywhere else in the block.
        assert!(crate::frame::leaving(func, the(func, Opcode::CapYield)));
    }

    #[test]
    fn a_returned_pointer_nothing_holds_a_capability_for_gets_neither_end() {
        // Nothing is made here, so a call whose result the caller never checks keeps paying nothing
        // for it, and a function that returns a pointer it holds nothing for leaves the caller the
        // recovery it was doing anyway.
        let mut names = Interner::new();
        let mut module = both_ends(&mut names, false);
        arrange(&mut module, &names);
        let func = &module[module.funcs().next().expect("the module defines two")];
        assert_eq!(count(func, Opcode::CapResult), 0);
        assert_eq!(count(func, Opcode::CapYield), 0);
        assert_eq!(count(func, Opcode::CapPublish), 0);
    }

    /// A function `f` that asks `g` for a pointer, handing it nothing, and checks what comes back.
    fn asking(names: &mut Interner) -> Func {
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let sig = func.add_signature(Signature::new().with_returns(&[Type::PTR]));
        let varargs = func.push_abis(&[]);
        let callee = Some(names.intern("g"));
        let info = func.add_call(CallInfo { callee, signature: sig, varargs });
        let mut b = Builder::new(&mut func, entry);
        let args = b.func().push_values(&[]);
        let data = InstData { args, extra: Extra::Call(info), ..InstData::new(Opcode::Call) };
        let got = b.value(data, Type::PTR);
        checked(&mut b, got);
        b.ret(&[]);
        func
    }

    #[test]
    fn a_call_that_hands_nothing_over_still_gets_back_the_capability_of_what_it_returned() {
        // Row T4 needs this one: a function that returns a pointer to its own local and takes no
        // pointer, so there was nothing to publish and the caller recovered an address the callee
        // could have described exactly. The frame is published for the answer alone.
        let mut names = Interner::new();
        let mut module = unit(&mut names);
        module.add_func(asking(&mut names));
        module.add_func(passing_on(&mut names, "g", Some("h"), true));
        assert_eq!(arrange(&mut module, &names), 1);
        let func = &module[module.funcs().next().expect("the module defines two")];
        assert_eq!(count(func, Opcode::CapPublish), 1);
        assert_eq!(operands(func, Opcode::CapPublish).len(), 1);
        assert_eq!(count(func, Opcode::CapResult), 1);
        assert!(crate::frame::given(func, the(func, Opcode::CapResult)));
    }

    /// A function `g` that returns the address of a local of its own and checks nothing.
    fn escaping(names: &mut Interner) -> Func {
        let mut func = Func::new(names.intern("g"), Signature::new().with_returns(&[Type::PTR]));
        let entry = func.create_block();
        let info = MemInfo {
            size: 4,
            align: 4,
            order: MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        };
        let extra = Extra::Mem(func.add_mem(info));
        let mut b = Builder::new(&mut func, entry);
        let local = b.value(InstData { extra, ..InstData::new(Opcode::Alloca) }, Type::PTR);
        b.ret(&[local]);
        func
    }

    /// A function `f` that hands `callee` a local and a pointer into its middle, and checks nothing.
    ///
    /// The local is `alloca(8)` rather than an array of eight bytes when `grown` says so.
    fn handing_a_local(names: &mut Interner, callee: &str, grown: bool) -> (Func, Value) {
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let sig = func.add_signature(three());
        let varargs = func.push_abis(&[]);
        let callee = Some(names.intern(callee));
        let info = func.add_call(CallInfo { callee, signature: sig, varargs });
        let mem = MemInfo {
            size: if grown { 0 } else { 8 },
            align: 4,
            order: MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        };
        let extra = Extra::Mem(func.add_mem(mem));
        let mut b = Builder::new(&mut func, entry);
        let size = if grown { vec![b.iconst(Type::int(64), 8)] } else { Vec::new() };
        let args = b.func().push_values(&size);
        let local = b.value(InstData { args, extra, ..InstData::new(Opcode::Alloca) }, Type::PTR);
        let four = b.iconst(Type::int(64), 4);
        let args = b.func().push_values(&[local, four]);
        let inside = b.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR);
        let args = b.func().push_values(&[local, four, inside]);
        b.inst(InstData { args, extra: Extra::Call(info), ..InstData::new(Opcode::Call) }, &[]);
        b.ret(&[]);
        (func, local)
    }

    /// Whether the first function of `module` publishes the capability of `local` for both of the
    /// pointers it hands over, and clears nothing.
    fn publishes_the_local(module: &Module, local: Value) {
        let func = &module[module.funcs().next().expect("the module defines one")];
        assert_eq!(count(func, Opcode::CapClear), 0);
        let caps = operands(func, Opcode::CapPublish);
        assert_eq!(caps.len(), 2);
        for cap in caps {
            let Def::Result { inst, .. } = func[cap].def else { panic!("a capability is made") };
            assert_eq!(func[inst].opcode, Opcode::CapOf);
            assert_eq!(func[func[inst].args].to_vec(), [local]);
        }
    }

    #[test]
    fn a_local_handed_to_a_callee_that_checks_travels_with_no_check_left_in_the_caller() {
        // The shape -O2 leaves when the caller's only store into its buffer was in bounds and was
        // discharged: nothing in the caller holds a capability, and the callee still checks. The
        // callee cannot recover a stack address, so the caller makes the local's capability at the
        // call, over the local itself for the pointer into its middle as well.
        let mut names = Interner::new();
        let mut module = unit(&mut names);
        let (func, local) = handing_a_local(&mut names, "g", false);
        module.add_func(func);
        module.add_func(caller(&mut names, "g", Some("h"), three(), true));
        assert_eq!(arrange(&mut module, &names), 1);
        publishes_the_local(&module, local);
    }

    #[test]
    fn a_local_handed_to_a_wrapper_travels_though_nothing_here_defines_the_wrapper() {
        // `memcpy` into a local, redirected to its wrapper. The wrapper is not in this unit, and it
        // is still one that takes its frame, so the call publishes rather than clears. A clear was
        // a copy into the stack that nothing judged, since no region covers it.
        let mut names = Interner::new();
        let mut module = unit(&mut names);
        let (func, local) = handing_a_local(&mut names, "__rucc_wrap_memcpy", false);
        module.add_func(func);
        let wrapper = names.find("__rucc_wrap_memcpy").expect("the call names it");
        assert_eq!(remaining(&module, &names).get(&wrapper), Some(&1));
        assert_eq!(arrange(&mut module, &names), 1);
        publishes_the_local(&module, local);
    }

    #[test]
    fn an_alloca_handed_to_a_wrapper_travels_like_any_other_local() {
        // `__builtin_alloca` and a variable length array are an `alloca` with the size as its
        // operand. The capability is made over it all the same, and a clear here was a copy past
        // the end of one that the wrapper let through.
        let mut names = Interner::new();
        let mut module = unit(&mut names);
        let (func, local) = handing_a_local(&mut names, "__rucc_wrap_memcpy", true);
        module.add_func(func);
        assert_eq!(arrange(&mut module, &names), 1);
        publishes_the_local(&module, local);
    }

    #[test]
    fn a_function_returning_its_own_local_yields_its_capability_with_no_check_left() {
        // Row T4 at -O2, where the optimizer has taken every check out of the function that lets
        // its local escape. It still counts as one that answers, and it makes the capability it
        // answers with, so the caller's frame read is the local's and not the bottom one.
        let mut names = Interner::new();
        let mut module = unit(&mut names);
        module.add_func(asking(&mut names));
        module.add_func(escaping(&mut names));
        let g = names.intern("g");
        assert_eq!(remaining(&module, &names).get(&g), Some(&1));
        assert_eq!(arrange(&mut module, &names), 1);
        let mut funcs = module.funcs();
        let f = &module[funcs.next().expect("the module defines two")];
        assert_eq!(count(f, Opcode::CapResult), 1);
        let g = &module[funcs.next().expect("the module defines two")];
        assert_eq!(count(g, Opcode::CapOf), 1);
        assert_eq!(count(g, Opcode::CapYield), 1);
    }

    /// `body` parsed as the one function of a module, run through [`arrange`], and verified.
    fn arranged(body: &str) -> Module {
        let mut names = Interner::new();
        let text = format!(
            "; ModuleID = 't.c'\n\
             ; format 0\n\
             target triple = \"x86_64-unknown-linux-gnu\"\n\
             target datalayout = \"e-p:64:64-i64:64-f80:128-S128\"\n\
             func @__rucc_wrap_strcpy(ptr, ptr) -> ptr, linkage(external);\n{body}"
        );
        let mut module = rucc_ir::parse(&text, &mut names).expect("the fixture parses");
        assert_eq!(arrange(&mut module, &names), 1);
        if let Err(errors) = rucc_ir::verify(&module, &names) {
            panic!("the pass left invalid IR, {errors:?}\n{}", rucc_ir::print(&module, &names));
        }
        module
    }

    /// What each edge into the block the capability parameter `cap` belongs to passes for it, as
    /// the opcode that made it and what that was over, in the order the edges come.
    fn passed(func: &Func, cap: Value) -> Vec<(Opcode, Vec<Value>)> {
        let Def::Param { block, index } = func[cap].def else { panic!("a parameter") };
        let mut got = Vec::new();
        for pred in func.blocks() {
            let Some(term) = func.terminator(pred) else { continue };
            for call in func.successors(term) {
                if call.block != block {
                    continue;
                }
                let value = func[call.args][index as usize];
                let Def::Result { inst, .. } = func[value].def else { panic!("made on the edge") };
                got.push((func[inst].opcode, func[func[inst].args].to_vec()));
            }
        }
        got
    }

    #[test]
    fn a_pointer_set_on_one_side_of_a_branch_carries_the_local_it_was_set_to() {
        // Juliet's flow variants 05 and 09 to 14: the buffer is chosen under a test the optimizer
        // cannot fold, and the other side leaves the pointer unset, which arrives as a null. The
        // copy into it was let through because the wrapper was handed nothing.
        let module = arranged(
            r#"
func @f(i1), linkage(external) {
block0(%0: i1):
    %1 = alloca, size 10, align 1
    %2 = alloca, size 11, align 1
    br_if %0, block1, block2

block1:
    jump block3(%1)

block2:
    %3 = iconst.i64 0
    %4 = inttoptr.ptr %3
    jump block3(%4)

block3(%5: ptr):
    %6 = call @__rucc_wrap_strcpy(%5, %2) : (ptr, ptr) -> ptr
    return
}
"#,
        );
        let func = &module[module.funcs().find(|&id| !module[id].is_declaration()).expect("f")];
        let caps = operands(func, Opcode::CapPublish);
        assert_eq!(caps.len(), 2);
        let local = func[func.insts(func.entry().expect("an entry")).next().expect("the alloca")]
            .results()
            .next()
            .expect("an address");
        assert_eq!(
            passed(func, caps[0]),
            [(Opcode::CapOf, vec![local]), (Opcode::CapNull, vec![])]
        );
    }

    #[test]
    fn a_pointer_set_to_one_of_two_locals_carries_the_one_it_arrived_with() {
        // Variant 12, where a coin picks the buffer that is too small or the one that is not.
        let module = arranged(
            r#"
func @f(i1), linkage(external) {
block0(%0: i1):
    %1 = alloca, size 10, align 1
    %2 = alloca, size 11, align 1
    %3 = alloca, size 11, align 1
    br_if %0, block1, block2

block1:
    jump block3(%1)

block2:
    %4 = iconst.i64 1
    %5 = ptr_add %2, %4
    jump block3(%5)

block3(%6: ptr):
    %7 = call @__rucc_wrap_strcpy(%6, %3) : (ptr, ptr) -> ptr
    return
}
"#,
        );
        let func = &module[module.funcs().find(|&id| !module[id].is_declaration()).expect("f")];
        let locals: Vec<Value> = func
            .insts(func.entry().expect("an entry"))
            .take(2)
            .filter_map(|inst| func[inst].results().next())
            .collect();
        let caps = operands(func, Opcode::CapPublish);
        assert_eq!(
            passed(func, caps[0]),
            [(Opcode::CapOf, vec![locals[0]]), (Opcode::CapOf, vec![locals[1]])]
        );
    }

    #[test]
    fn a_pointer_that_may_be_anything_else_is_handed_over_as_before() {
        // A parameter of the function passed on one edge has a capability only a walk could make,
        // so the block gets no parameter beside it and the frame holds the bottom one for it.
        let module = arranged(
            r#"
func @f(i1, ptr), linkage(external) {
block0(%0: i1, %1: ptr):
    %2 = alloca, size 10, align 1
    %3 = alloca, size 11, align 1
    br_if %0, block1, block2

block1:
    jump block3(%2)

block2:
    jump block3(%1)

block3(%4: ptr):
    %5 = call @__rucc_wrap_strcpy(%4, %3) : (ptr, ptr) -> ptr
    return
}
"#,
        );
        let func = &module[module.funcs().find(|&id| !module[id].is_declaration()).expect("f")];
        let join = func.blocks().last().expect("the block the pointer arrives at");
        assert_eq!(func[join].params.len(), 1);
        let caps = operands(func, Opcode::CapPublish);
        let Def::Result { inst, .. } = func[caps[0]].def else { panic!("a capability is made") };
        assert_eq!(func[inst].opcode, Opcode::CapNull);
    }
}
