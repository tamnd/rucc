//! The frame a call's capabilities travel in, beside the arguments rather than inside them.
//!
//! Design: `spec/safe-memory/05-representation.md` section 5.3.
//!
//! Everything [`mod@crate::slot`] does is about one function. A capability is made where the
//! pointer is made and read where a check needs it, and both ends are instructions in the same
//! body, so a frame slot and its address are the whole of the representation. This is what happens
//! at the one place that stops being true, which is a call.
//!
//! The rule the whole design hangs on is that an instrumented function's calling convention is
//! unchanged. A pointer argument goes in the register it always went in, at the size it always
//! was, and `sizeof(void *)` is still eight, which is what lets an object this compiler built link
//! against one nobody instrumented. So the capability cannot travel in the argument, and section
//! 5.3 puts it in a small frame in thread local storage instead, written by the caller and read by
//! the callee, indexed by argument position.
//!
//! `rucc_safe_rt::frame` is the other half and it was already finished. The magic word, the take
//! that consumes the frame so that nothing further down the chain believes it twice, the outer
//! link that makes frames nest the way calls do, and the clear that a call to an unknown callee
//! needs are all there, with `__rucc_frame_publish`, `__rucc_frame_take`, `__rucc_frame_clear` and
//! `__rucc_frame_restore` as the names generated code is compiled against. What was missing was a
//! way for the IR to say which capability belongs to which argument of which call, and that is
//! [`Opcode::CapPublish`] and [`Opcode::CapClear`] on the writing side and [`Opcode::CapArg`] on the
//! reading one.
//!
//! # The pointer that goes the other way
//!
//! A returned pointer is the one value that crosses a call backwards, and it gets the same pair of
//! ends with the sides swapped. The callee says what it is returning with [`Opcode::CapYield`], in
//! front of the return it is about, and the caller reads it with [`Opcode::CapResult`], behind the
//! call it is about.
//!
//! That write is the only one in the design that goes into a frame somebody else made, and it is
//! allowed to for a reason worth saying rather than assuming. The frame is the caller's stack, the
//! caller is sitting in the call waiting for this function to come back, and this function's own
//! stack is about to stop existing, so the caller's frame is the only storage that is alive at both
//! ends. The alternative would be a second frame pointing the other way, which is a second
//! publish and a second take on every call that returns a pointer.
//!
//! The caller writes the bottom capability into that slot before it publishes, and that is what
//! makes a callee which wrote nothing tell the truth. The frame is one allocation per function
//! reused by every call site in it, so without the write the slot still holds the last call's
//! answer, and a capability belonging to some other pointer is worse than no capability at all, for
//! exactly the reason the empty frame is an instruction rather than an absence.
//!
//! # The reading end
//!
//! A callee takes the frame once, at the top of the function, and then asks it one question per
//! pointer parameter, and the whole of what makes that pair work is that neither half has to know
//! what kind of caller the function turned out to have.
//!
//! Taking consumes the frame, which is why it happens once and why it happens first. Once, because a
//! second take finds the magic word already cleared and answers null, so a function that took twice
//! would recover half its own arguments for nothing. First, because every call this function makes
//! either publishes a frame of its own or clears, and a take after one of those finds what that call
//! left rather than what this function was given.
//!
//! The question is a call rather than a branch on whether the frame is null, and the deciding is the
//! runtime's. `rucc_safe_rt::recover::argument` is the whole of it: a capability the caller carried
//! is the answer, and the bottom one is the signal to walk the planes. That is the expensive answer,
//! it is counted as the weakening it is, and it is the one that is always available, which is what
//! lets a function compiled this way be called from code that knows nothing about any of it.
//!
//! # Why the empty frame is an instruction rather than an absence
//!
//! A publish with nothing in it and no publish at all are not the same thing, which is the one
//! part of this that reads backwards until the reason is said out loud.
//!
//! Publishing leaves the frame in place until somebody takes it. A callee that was never compiled
//! by this build does not take it, so it stays there for whatever that callee calls back into, and
//! a callback entered from uninstrumented code holding capabilities that belong to a different
//! call is worse than one entered holding none: the first reports on memory it was never about,
//! and the second recovers its arguments and says so in the count. Document 10 section 10.8 is
//! where that case is written down.
//!
//! Not publishing does not fix it either, because what is live at that point is whatever the frame
//! held before, which after an instrumented function has taken its own is its caller's caller's.
//! So a call whose callee cannot be vouched for says there is nothing, out loud, and that is
//! `cap_clear`. An empty publish would say the opposite: it sets the magic, so the callee takes a
//! frame that describes no arguments and then hands the outer one back to the next reader.
//!
//! # What decides which one a call gets
//!
//! Nothing here. [`crate::handover`] is the rule: a callee defined in this unit with no checks left
//! needs no frame, one that still checks something needs the capabilities, and a callee outside the
//! unit or reached through a pointer is one nothing here can ask. That is a whole-unit question and
//! this module is a lowering, so it lives there, where the census that has been counting those
//! buckets since before anything emitted a frame reads it as well. The pass that puts the
//! instructions in is a later box on tamnd/rucc#1085.
//!
//! # The one thing this does not handle
//!
//! A callee that leaves by `longjmp` rather than by returning skips the restore, so the frame stays
//! published over a stack that has been unwound past. Nothing in section 5.3 says what to do about
//! that and nothing in the runtime does either, so it is written down here rather than papered
//! over. The same instruction is a gap on the back end's own list for the same kind of reason, and
//! the frame is one more thing that will have to be unwound when it stops being one.

use rucc_base::Interner;
use rucc_ir::{Extra, Func, Imm, Inst, InstData, MemInfo, MemOrder, Opcode, Restrict, Type, Value};

use crate::slot;

/// How many pointer arguments a frame carries capabilities for.
///
/// Eight, which is section 5.3's number and `rucc_safe_rt::frame::ARGS` at the other end. It is
/// more pointer arguments than nearly any function takes, and a call that hands over more than this
/// describes the ones that fit and lets the callee recover the rest, which is a weakening the
/// summary counts rather than a refusal.
pub const ARGS: usize = 8;

/// How many bytes the frame takes.
///
/// The layout is `rucc_safe_rt::frame::Frame`'s and the two have to agree, so the number is written
/// down in both places and tested in both, the same way [`slot::BYTES`] is. It is the 64-bit shape:
/// the trailing link is a pointer and the count below assumes eight bytes of it, which is every
/// target this compiler has.
pub const BYTES: u64 = OUTER + WORD;

/// What the frame is aligned to, which is what its widest field needs.
pub const ALIGN: u32 = 8;

/// The magic word and the two half words beside it, which is what the capabilities start after.
const HEAD: u64 = 8;

/// How wide the trailing link is.
const WORD: u64 = 8;

/// Where the count of described arguments sits.
const ARGC: u64 = 4;

/// Where the spare half word beside the count sits.
const FLAGS: u64 = 6;

/// How wide each of those two is.
const HALF: u64 = 2;

/// Where the callee leaves the capability of the pointer it returns.
///
/// After the arguments, because it is one more capability and there was nowhere cheaper to put it.
/// The caller writes the bottom capability here before it publishes, so that a callee which wrote
/// nothing is telling the truth rather than leaving the previous call's answer behind.
const RET: u64 = HEAD + slot::BYTES * ARGS as u64;

/// Where the frame this one was published over is kept.
///
/// After the capabilities and after [`RET`], which is why the count below is one more than [`ARGS`].
const OUTER: u64 = HEAD + slot::BYTES * (ARGS as u64 + 1);

/// The call a `cap_publish` or a `cap_clear` is about, which is the instruction after it.
///
/// Adjacency is the whole of the tie between the two, the way `meta_release` is tied to the atomic
/// it goes in front of. Anything else would mean a name for a call site in the IR, and a call site
/// already has one, which is where it is.
pub(crate) fn describes(func: &Func, inst: Inst) -> Option<Inst> {
    let block = func.block_of(inst)?;
    let mut after = func.insts(block).skip_while(|&at| at != inst);
    after.next()?;
    let next = after.next()?;
    matches!(func[next].opcode, Opcode::Call | Opcode::CallIndirect).then_some(next)
}

/// Whether this is a `cap_publish` this module knows how to write.
///
/// Two things beyond the verifier's shape. It has to be in front of a call, since there is nothing
/// else for it to be about, and it has to describe no more arguments than the frame has room for.
/// The second is a refusal rather than a truncation on purpose: dropping the capabilities past the
/// eighth would be a silent weakening, where leaving the function alone is a capability the back end
/// says it cannot lower.
pub(crate) fn placeable(func: &Func, inst: Inst) -> bool {
    func[func[inst].args].len() <= ARGS && describes(func, inst).is_some()
}

/// Whether a `cap_result` is behind a call that was published to.
///
/// Two instructions back rather than one, because what has to be true is not only that there is a
/// call in front of this but that the call had a frame. A `cap_clear` in that position is a call
/// with no frame for anybody to have written into, so there would be nothing here to read and the
/// answer would be the previous call site's.
pub(crate) fn given(func: &Func, inst: Inst) -> bool {
    let Some(block) = func.block_of(inst) else { return false };
    let insts: Vec<Inst> = func.insts(block).collect();
    let Some(at) = insts.iter().position(|&each| each == inst) else { return false };
    let Some(call) = at.checked_sub(1).and_then(|back| insts.get(back)) else { return false };
    if !matches!(func[*call].opcode, Opcode::Call | Opcode::CallIndirect) {
        return false;
    }
    at.checked_sub(2)
        .and_then(|back| insts.get(back))
        .is_some_and(|&publish| func[publish].opcode == Opcode::CapPublish)
}

/// Whether a `cap_yield` is in front of the return it is about.
///
/// The same kind of tie a publish has with its call, and needed for the same kind of reason. What
/// the instruction says is about the value leaving by one particular return, and one sitting
/// anywhere else would be writing the frame for a path that does not take it.
pub(crate) fn leaving(func: &Func, inst: Inst) -> bool {
    let Some(block) = func.block_of(inst) else { return false };
    let mut after = func.insts(block).skip_while(|&at| at != inst);
    after.next();
    after.next().is_some_and(|next| func[next].opcode == Opcode::Return)
}

/// `cap_publish` becomes the frame written out and `__rucc_frame_publish(frame)` in front of the
/// call, with the outer frame put back after it.
///
/// The frame is one `alloca` for the whole function rather than one per call site, because no two
/// calls in a body are live at once: the publish, the call and the restore are three instructions
/// in a row, and nothing between them is another call of this function's. Recursion is not an
/// exception, since the inner call is running in its own frame.
///
/// The argument count is written here and the magic word is not. `publish` writes the magic and the
/// outer link itself, which is the thing that makes the frame believable, and that belongs on the
/// side that knows what believable means.
pub(crate) fn hand_over(
    func: &mut Func,
    names: &mut Interner,
    word: Type,
    inst: Inst,
    frame: Value,
) {
    let Some(call) = describes(func, inst) else { return };
    let caps: Vec<Value> = func[func[inst].args].to_vec();
    let Ok(argc) = i128::try_from(caps.len()) else { return };
    let half = Type::int(16);
    let count = slot::konst(func, inst, Imm::int(argc, half), half);
    record(func, inst, word, count, frame, ARGC, HALF);
    // The spare half word beside the count, which nothing reads yet. Written rather than left as
    // whatever the frame held, because the frame is reused by every call site in the function and a
    // field that means something later would mean whatever the previous call put there.
    let zero = slot::konst(func, inst, Imm::int(0, half), half);
    record(func, inst, word, zero, frame, FLAGS, HALF);
    // And the bottom capability where the callee leaves its returned pointer's, which is four zero
    // words for the reason `cap_null` is four zero words. This is what makes a callee that wrote
    // nothing tell the truth: without it the slot holds whatever the previous call site put there,
    // and the caller would read a capability belonging to some other pointer entirely.
    let empty = slot::konst(func, inst, Imm::int(0, word), word);
    for step in 0..slot::BYTES / WORD {
        record(func, inst, word, empty, frame, RET + step * WORD, WORD);
    }
    for (at, &cap) in caps.iter().enumerate() {
        let to = slot::offset(func, inst, frame, HEAD + slot::BYTES * at as u64, word);
        let info = MemInfo {
            size: slot::BYTES,
            align: slot::ALIGN,
            order: MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        };
        let extra = Extra::Mem(func.add_mem(info));
        let args = func.push_values(&[to, cap]);
        let data = InstData { args, extra, ..InstData::new(Opcode::Memcpy) };
        let made = func.create_inst(data, &[], func.span(inst));
        func.insert_before(made, inst);
    }
    let params = &[Type::PTR];
    let data = crate::lower::calling(func, names, "__rucc_frame_publish", params, &[], &[frame]);
    let made = func.create_inst(data, &[], func.span(inst));
    func.insert_before(made, inst);
    restore(func, names, word, inst, call, frame);
    func.remove_inst(inst);
}

/// `cap_clear` becomes `__rucc_frame_clear()`, in place of itself.
///
/// In place rather than beside, unlike everything [`mod@crate::slot`] rewrites, because this
/// instruction gives nothing back and neither does the call. Nothing points at it and nothing has
/// to be pointed anywhere else.
pub(crate) fn cleared(func: &mut Func, names: &mut Interner, inst: Inst) {
    crate::lower::call(func, names, inst, "__rucc_frame_clear", &[], &[], &[]);
}

/// Puts the frame back to the one this call was published over, after the call has returned.
///
/// A load of the link `publish` wrote and a call with what it found, rather than a routine that
/// takes the frame and does both, because the runtime already has the entry point that takes an
/// outer frame and the load is one instruction. The link is right whether or not the callee took
/// the frame: a callee that took it restored to the same value on the way in, so this is a store of
/// what is already there, and a callee that did not is exactly the case this exists for.
fn restore(
    func: &mut Func,
    names: &mut Interner,
    word: Type,
    inst: Inst,
    call: Inst,
    frame: Value,
) {
    let at = slot::offset(func, inst, frame, OUTER, word);
    let info = MemInfo {
        size: WORD,
        align: ALIGN,
        order: MemOrder::NotAtomic,
        tbaa: None,
        owns: 0,
        restrict: Restrict::NONE,
    };
    let extra = Extra::Mem(func.add_mem(info));
    let args = func.push_values(&[at]);
    let data = InstData { args, extra, ..InstData::new(Opcode::Load) };
    let read = func.create_inst(data, &[Type::PTR], func.span(call));
    func.insert_after(read, call);
    let Some(outer) = func[read].results().next() else { return };
    let params = &[Type::PTR];
    let data = crate::lower::calling(func, names, "__rucc_frame_restore", params, &[], &[outer]);
    let made = func.create_inst(data, &[], func.span(call));
    func.insert_after(made, read);
}

/// One of the two half words at the front of the frame, written in front of `inst`.
fn record(func: &mut Func, inst: Inst, word: Type, value: Value, frame: Value, at: u64, size: u64) {
    let to = slot::offset(func, inst, frame, at, word);
    let align = u32::try_from(size).unwrap_or(1);
    let info = MemInfo {
        size,
        align,
        order: MemOrder::NotAtomic,
        tbaa: None,
        owns: 0,
        restrict: Restrict::NONE,
    };
    let extra = Extra::Mem(func.add_mem(info));
    let args = func.push_values(&[value, to]);
    let data = InstData { args, extra, ..InstData::new(Opcode::Store) };
    let made = func.create_inst(data, &[], func.span(inst));
    func.insert_before(made, inst);
}

/// Reserves the frame at the top of the entry block and gives back its address.
///
/// One per function, for the reason [`hand_over`] gives, so this is called once however many calls
/// the body hands capabilities to. At the top for the reason [`slot`]'s own reservation is: that is
/// where the verifier wants an `alloca` that is not a variable length array.
pub(crate) fn reserve(func: &mut Func, inst: Inst) -> Option<Value> {
    let entry = func.entry()?;
    let first = func.insts(entry).next()?;
    let info = MemInfo {
        size: BYTES,
        align: ALIGN,
        order: MemOrder::NotAtomic,
        tbaa: None,
        owns: 0,
        restrict: Restrict::NONE,
    };
    let extra = Extra::Mem(func.add_mem(info));
    let data = InstData { extra, ..InstData::new(Opcode::Alloca) };
    let slot = func.create_inst(data, &[Type::PTR], func.span(inst));
    func.insert_before(slot, first);
    func[slot].results().next()
}

/// Calls `__rucc_frame_take()` at the top of the entry block and gives back what it found.
///
/// Once per function, at the very front, and both halves of that are load bearing rather than tidy.
/// Once, because taking consumes the frame: a second take in the same body finds the magic word
/// already cleared and answers null, so a function that took twice would recover half its arguments
/// for no reason. At the front, because any call this function makes either publishes a frame of its
/// own or clears, and both of those are gone by the time a take after them runs.
///
/// The answer is a pointer that may be null, and null is not an error. It is a call from code this
/// build never compiled, a call whose caller could not vouch for this one, or a call this compiler
/// decided needed no frame, and the runtime treats all three the same way, which is to work the
/// capability out of the planes instead.
pub(crate) fn taken(func: &mut Func, names: &mut Interner, inst: Inst) -> Option<Value> {
    let entry = func.entry()?;
    let first = func.insts(entry).next()?;
    let data = crate::lower::calling(func, names, "__rucc_frame_take", &[], &[Type::PTR], &[]);
    let made = func.create_inst(data, &[Type::PTR], func.span(inst));
    func.insert_before(made, first);
    func[made].results().next()
}

/// `cap_arg` becomes `__rucc_frame_arg(slot, frame, position, pointer)`.
///
/// The slot in front, which is where the capability goes, and then the three things the runtime
/// needs to decide what it is: the frame the caller published or null, which of the call's arguments
/// this one was, and the pointer itself. The last of those is the answer when the first is null, and
/// putting the fallback in the runtime rather than in a branch here is what keeps this one call
/// whatever kind of caller the function turns out to have.
///
/// Beside the instruction and not in place of it, the way every other producer here is rewritten,
/// because the call gives nothing back and the instruction did.
pub(crate) fn argument(
    func: &mut Func,
    names: &mut Interner,
    word: Type,
    inst: Inst,
    address: Value,
    frame: Value,
) {
    let [at, position] = func[func[inst].args] else { return };
    let position = crate::lower::fitted(func, inst, position, word);
    let params = &[Type::PTR, Type::PTR, word, Type::PTR];
    let args = &[address, frame, position, at];
    let data = crate::lower::calling(func, names, "__rucc_frame_arg", params, &[], args);
    let made = func.create_inst(data, &[], func.span(inst));
    func.insert_before(made, inst);
    func.remove_inst(inst);
}

/// `cap_yield` becomes `__rucc_frame_yield(frame, slot)`.
///
/// The frame is the one this function was handed, so the write goes into storage the caller owns
/// and is still sitting in, which is the only place a returned pointer's capability can live: this
/// function's own stack is gone by the time the caller looks at anything. A null frame is a caller
/// that said nothing, and the runtime drops the write rather than the compiler testing for it here.
///
/// In place of the instruction, which gives nothing back, so there is nothing to rewrite around.
pub(crate) fn yielded(func: &mut Func, names: &mut Interner, inst: Inst, frame: Value) {
    let [cap] = func[func[inst].args] else { return };
    let params = &[Type::PTR, Type::PTR];
    crate::lower::call(func, names, inst, "__rucc_frame_yield", params, &[], &[frame, cap]);
}

/// `cap_result` becomes `__rucc_frame_returned(slot, frame, pointer)`.
///
/// The frame is the one the publish in front of the call filled in, and it is read after the call
/// rather than before because what is being read is what the callee wrote. The pointer is the
/// fallback, for a callee that wrote nothing and left the bottom capability the publish put there,
/// and it is the runtime that tells those apart for the reason [`argument`] gives.
///
/// Beside the instruction rather than in place of it, because the call gives nothing back.
pub(crate) fn result(
    func: &mut Func,
    names: &mut Interner,
    inst: Inst,
    address: Value,
    frame: Value,
) {
    let [at] = func[func[inst].args] else { return };
    let params = &[Type::PTR, Type::PTR, Type::PTR];
    let args = &[address, frame, at];
    let data = crate::lower::calling(func, names, "__rucc_frame_returned", params, &[], args);
    let made = func.create_inst(data, &[], func.span(inst));
    func.insert_before(made, inst);
    func.remove_inst(inst);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_frame_is_the_shape_the_runtime_reads() {
        // An ABI, and the two halves are in different crates that cannot see each other, so this
        // is the same arithmetic `rucc_safe_rt::frame`'s own test does. A change on one side
        // without a change on the other is two halves disagreeing about where the capabilities are,
        // which is a monitor reporting on the wrong memory rather than a monitor being slow.
        assert_eq!(slot::BYTES, 32);
        assert_eq!(ARGS, 8);
        assert_eq!(HEAD, 8);
        assert_eq!(ARGC, 4);
        assert_eq!(FLAGS, 6);
        assert_eq!(OUTER, 8 + 32 * (ARGS as u64 + 1));
        assert_eq!(BYTES, 8 + 32 * (ARGS as u64 + 1) + 8);
    }
}
