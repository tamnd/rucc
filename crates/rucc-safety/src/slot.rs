//! Where a capability lives once it has to be a value the back end can hold.
//!
//! Design: `spec/safe-memory/05-representation.md` section 5.2.1.
//!
//! A capability is four words, and until now none of them ever reached the back end. Every check
//! [`mod@crate::lower`] emits is handed an address and works the rest out inside the runtime, so the
//! `cap_of` that fed the check was dead by the time the check was a call and the pass simply took
//! it out. That trick runs out at `cap_store`, which is a write of a capability into the aux plane
//! and so cannot be given an address and told to find one: working a capability out from an
//! interior address is the plane walk in `rucc_safe_rt::recover::run`, which is linear in the size
//! of the object, and a pointer store is not somewhere that can be paid for. tamnd/rucc#1085 is
//! where that was worked out.
//!
//! So the first thing that has to exist is a capability that is a value. This is it.
//!
//! # Four words of frame, named by their address
//!
//! A `cap` value becomes an `alloca` of thirty two bytes in the entry block, and the value that
//! stood for the capability becomes that slot's address. Everything that produced a capability
//! writes the four words, and everything that reads one is handed the address.
//!
//! Section 5.2.1 wants capabilities in registers, and a stack slot is not that. It is the first cut
//! for two reasons. The first is that the runtime's own ABI already works this way: every entry
//! point that takes or gives a capability takes or gives a `*const Cap`, because four words is over
//! the size where the C convention passes a structure in registers anyway, so the address of a slot
//! is what a call needs in hand either way. The second is that a slot needs nothing new from the
//! back end at all. An `alloca` of a size and an alignment is a thing `rucc_codegen::frame` has
//! always laid out, so a capability reaching the back end is a capability the back end already
//! knows how to keep, and the register form of section 5.2.1 becomes an optimization over this
//! rather than a prerequisite for any of it.
//!
//! # What is lowered and what is left
//!
//! `cap_null` and `cap_store`, which is the pair that makes a capability and consumes one without
//! anybody having to work one out from an address. The other producers are the boxes on
//! tamnd/rucc#1085 after this one, and each of them is a question of its own about where the
//! numbers come from rather than about where they are kept.
//!
//! And `cap_of` over a pointer an allocator just returned, which is the second of those boxes and
//! the easiest of them by a long way. Everywhere else a `cap_of` is a question with no cheap answer,
//! because an address on its own says nothing about the object around it and working the object out
//! is the plane walk. At an allocation site the address is the base of the object, the header sits
//! directly behind the base, and the header holds the extent and the version and the whole metadata
//! word the allocator wrote. So the capability is a subtract and a load, and it is exact rather than
//! recovered: the permissions and the instance identifier are the ones the allocator meant rather
//! than a guess made from the region's class. `fresh` is the shape it recognises, and
//! `rucc_safe_rt::recover`'s `made` is the load.
//!
//! And `cap_of` over anything else, which is the last box and turned out to be a fallback rather
//! than a lowering of its own. It is the general question, every other producer is a special case of
//! it that has a cheap answer, and by the time they all exist a `cap_of` this pass cannot trace is a
//! pointer something really did lose track of. So it gets the plane walk, comes back marked as
//! recovered, and is counted. Refusing it instead would leave every capability in the function where
//! it was, which is not the more conservative direction: it is the same walk once per check rather
//! than once per pointer. An interior pointer is always this answer rather than the cheap one, which
//! is what document 05 section 5.2.3 leaves open, and it falls out of `fresh` asking for an
//! allocation site's own result rather than out of a test for it.
//!
//! And `cap_load`, which is the third box and the one producer here whose numbers nobody has to work
//! out at all, because an earlier part of the same program wrote them down. A pointer that lives in
//! memory has its capability beside it in the aux plane, so reading the pointer and reading the
//! capability are one event, and the opcode already carries everything the read needs: the
//! capability of the object the word sits in, the address of the word, and the pointer that came out
//! of it. `rucc_safe_rt::cap`'s `load` is the read. It is also the only thing this pass places that
//! reads a capability as well as making one, which is what shapes [`frames`] into two walks.
//!
//! And `cap_narrow`, which is `-fsafety-subobject`'s whole mechanism and the fourth box. A pointer
//! derived from a member of a structure gets the member's bounds rather than the object's, so an
//! overflow from one member into the next is caught where the default model would let it through.
//! What that costs is written down in document 04 section 4.4 and document 09 section 9.4, and it is
//! why the flag exists rather than the behaviour being on. The lowering is another call, over the
//! same two ends `cap_load` has, and the arithmetic behind it is `rucc_safe_rt::layout::Cap`'s
//! `narrowed`: the version and the metadata word come through untouched, because a member is in the
//! instance its object is in, and a range that is not inside the one it was taken from comes back
//! permitting nothing.
//!
//! And `cap_recover`, which is the fifth box and the last of the producers that has a shape of its
//! own. It is the plane walk, which is the expensive answer the other four exist to avoid, and it is
//! also the only answer that is always available, so it is what a pointer whose provenance nothing
//! kept falls back to. Document 05 section 5.3 is where most of those pointers come from: an
//! instrumented function called from uninstrumented code finds no frame, and every pointer argument
//! it was handed is recovered here and counted. The lowering is the same two arguments `cap_of` over
//! a fresh allocation has, because `rucc_safe_rt::recover` declares the pair as one shape, and the
//! difference between them is entirely behind the name.
//!
//! And `cap_publish` and `cap_clear`, which are the pair that takes a capability out of this
//! function and are the only two here that are not about where four words are kept. A call is where
//! the slot representation runs out, because the callee has its own frame and no way to see into
//! this one, so section 5.3 hands the capabilities over out of band and the lowering is a copy into
//! a frame in thread local storage. [`mod@crate::frame`] is all of that, including why saying there
//! is no frame is an instruction rather than the absence of one.
//!
//! And `cap_arg`, which is the sixth producer and the reading end of that pair. A pointer parameter
//! of an instrumented function has a capability the caller wrote down, so the cheap answer is to
//! read it, and the expensive one is there underneath for a function nobody published to. Which of
//! the two happens is the runtime's decision rather than a branch here, and the frame it decides
//! from is taken once at the top of the function, which is the one thing about this producer that is
//! not a property of the instruction on its own.
//!
//! And `cap_yield` and `cap_result`, which are the same pair once more for the pointer a call gives
//! back, so the callee is the writer this time and the caller is the reader. The writing end is the
//! only thing in this pass that stores into a frame it did not make, which it may because that frame
//! is the caller's stack and the caller is waiting in the call. [`mod@crate::frame`] is where that is
//! argued, along with why the caller empties the slot before it publishes.
//!
//! Until the rest exist a function can still hold a capability this pass cannot place, and the
//! answer then is to leave every capability in the function alone. Placing some and not others means
//! handing a `cap_store` the address of a slot that nothing ever wrote, which is worse than not
//! lowering it: the back end refuses an opcode it has no rule for and says so, and a slot full of
//! whatever the frame held is a capability that permits whatever it happens to say.
//!
//! # Why the dead ones go first
//!
//! Because most of them are dead. Every `cap_of` in a function was put there to feed a check, and
//! by the time this runs a check is a call, which reads one only where the runtime entry point it
//! became takes one. So the walk that used to remove them by opcode removes them by nobody reading
//! them instead. Running it to a fixpoint is what handles a chain, since a `cap_narrow` of a
//! `cap_of` leaves the `cap_of` unread only once the `cap_narrow` has gone.

use std::collections::{HashMap, HashSet};

use rucc_base::Interner;
use rucc_ir::{
    Block, BlockCall, Builder, Def, Extra, Flags, Func, Imm, Inst, InstData, MemInfo, MemOrder,
    Opcode, Restrict, Type, Value,
};

/// How many bytes a capability takes, which is section 5.2.1's four words.
///
/// `rucc_safe_rt::layout::Cap` is the other end of this and the two have to agree, so the number is
/// written down in both places and tested in both. It is the size of the slot the back end lays out
/// and the size of the structure the runtime reads out of it.
pub const BYTES: u64 = 32;

/// What a capability slot is aligned to.
///
/// One word, which is what the four fields of `Cap` need and no more. The runtime reads the slot
/// with an ordinary aligned read rather than with anything vector wide, so asking for sixteen would
/// cost frame for nothing.
pub const ALIGN: u32 = 8;

/// How wide one of the four words is.
const WORD: u64 = 8;

/// Puts every capability the function still holds into a frame slot.
///
/// The three steps of the module documentation in order: take out the capabilities nobody reads,
/// decide whether what is left is a shape this pass can place, and place it.
///
/// The placing is two walks with the substitution between them, and every slot has to be reserved in
/// the first of them. `cap_load` is why: it produces a capability and reads one, so the walk that
/// rewrites it needs its operand already pointed at a slot and everything reading its result already
/// pointed at another. Reserving is the half that can happen before anything has been rewritten, and
/// a slot is only an `alloca` and a name for it, so nothing is lost by deciding all of them first.
///
/// Taking the block parameters out goes between the substitution and the second walk, because a
/// capability carried along an edge is a value the first walk gives no slot to, and `parameters`
/// needs every edge already carrying an address to decide which slot each parameter reads.
pub fn frames(func: &mut Func, names: &mut Interner, word: Type) {
    prune(func);
    if !placeable(func) {
        return;
    }
    let mut moved: HashMap<Value, Value> = HashMap::new();
    for inst in walk(func) {
        if !func[inst].opcode.makes_capability() {
            continue;
        }
        let Some(result) = func[inst].results().next() else { continue };
        let Some(address) = reserve(func, inst) else { continue };
        moved.insert(result, address);
    }
    // A `cap_clear` is the one thing here that is not about a capability at all, so a function
    // whose only safety instruction is one still has something to lower.
    let clears = walk(func).into_iter().any(|inst| func[inst].opcode == Opcode::CapClear);
    if moved.is_empty() && !clears {
        return;
    }
    substitute(func, &moved);
    parameters(func, word);
    let mut frame: Option<Value> = None;
    let mut given: Option<Value> = None;
    for inst in walk(func) {
        let slot = func[inst].results().next().and_then(|value| moved.get(&value).copied());
        match (func[inst].opcode, slot) {
            (Opcode::CapNull, Some(address)) => nulled(func, word, inst, address),
            (Opcode::CapOf, Some(address)) => allocated(func, names, inst, address),
            (Opcode::CapLoad, Some(address)) => read(func, names, inst, address),
            (Opcode::CapNarrow, Some(address)) => narrowed(func, names, word, inst, address),
            (Opcode::CapRecover, Some(address)) => recovered(func, names, inst, address),
            (Opcode::CapStore, _) => stored(func, names, inst),
            (Opcode::CapPublish, _) => {
                let at = match frame {
                    Some(at) => at,
                    None => {
                        let Some(at) = crate::frame::reserve(func, inst) else { continue };
                        frame = Some(at);
                        at
                    }
                };
                crate::frame::hand_over(func, names, word, inst, at);
            }
            (Opcode::CapClear, _) => crate::frame::cleared(func, names, inst),
            // The reading end, whose frame comes from the take at the top of the function rather
            // than from a reservation. Lazily for the same reason the publish's is, and once for a
            // stronger one: taking consumes the frame, so a second take would answer null.
            (Opcode::CapArg, Some(address)) => {
                let at = match given {
                    Some(at) => at,
                    None => {
                        let Some(at) = crate::frame::taken(func, names, inst) else { continue };
                        given = Some(at);
                        at
                    }
                };
                crate::frame::argument(func, names, word, inst, address, at);
            }
            // The writing end of the returned pointer, which goes into the caller's frame and so
            // wants the same taken pointer the arguments do. A function that only yields still pays
            // for the take, which is one call at the top of a function that returns a pointer.
            (Opcode::CapYield, _) => {
                let at = match given {
                    Some(at) => at,
                    None => {
                        let Some(at) = crate::frame::taken(func, names, inst) else { continue };
                        given = Some(at);
                        at
                    }
                };
                crate::frame::yielded(func, names, inst, at);
            }
            // And its reading end, which is back in the caller and so wants the frame the publish
            // in front of the call filled in rather than the one this function was handed. There is
            // always one by now, because the publish sits two instructions in front of this.
            (Opcode::CapResult, Some(address)) => {
                let Some(at) = frame else { continue };
                crate::frame::result(func, names, inst, address, at);
            }
            _ => {}
        }
    }
}

/// Every instruction in the function, in an order that does not borrow it.
fn walk(func: &Func) -> Vec<Inst> {
    func.blocks()
        .collect::<Vec<Block>>()
        .into_iter()
        .flat_map(|block| func.insts(block).collect::<Vec<Inst>>())
        .collect()
}

/// Takes out every capability nothing reads, until there are none of those left.
///
/// A `cap` block parameter counts as a capability here, and handing one along an edge only counts
/// as reading it when the parameter on the other end is read. Otherwise a join whose every check
/// the optimizer discharged would keep a producer alive on each edge into it, and the producer on
/// one of those edges is often the plane walk.
fn prune(func: &mut Func) {
    loop {
        let read = reads(func);
        let mut again = false;
        for inst in walk(func) {
            if !func[inst].opcode.makes_capability() {
                continue;
            }
            if func[inst].results().any(|value| read.contains(&value)) {
                continue;
            }
            func.remove_inst(inst);
            again = true;
        }
        for block in func.blocks().collect::<Vec<Block>>() {
            let params = func[block].params.clone();
            let gone: HashSet<usize> = params
                .iter()
                .enumerate()
                .filter(|&(_, &param)| func[param].ty.is_cap() && !read.contains(&param))
                .map(|(index, _)| index)
                .collect();
            if gone.is_empty() {
                continue;
            }
            joined(func, block, &gone, &[], Type::int(64));
            let dropped: HashSet<Value> = gone.iter().map(|&index| params[index]).collect();
            func.retain_params(block, |value| !dropped.contains(&value));
            again = true;
        }
        if !again {
            return;
        }
    }
}

/// Every value something reads, where an edge reads what it hands a `cap` parameter only if that
/// parameter is read in turn.
fn reads(func: &Func) -> HashSet<Value> {
    let mut read: HashSet<Value> = HashSet::new();
    let mut carried: Vec<(Value, Value)> = Vec::new();
    for inst in walk(func) {
        read.extend(func[func[inst].args].iter().copied());
        for call in func.successors(inst) {
            for (&value, &param) in func[call.args].iter().zip(&func[call.block].params) {
                if func[param].ty.is_cap() {
                    carried.push((param, value));
                } else {
                    read.insert(value);
                }
            }
        }
    }
    loop {
        let before = read.len();
        for &(param, value) in &carried {
            if read.contains(&param) {
                read.insert(value);
            }
        }
        if read.len() == before {
            return read;
        }
    }
}

/// Whether every capability left in the function is one this pass knows where to put.
///
/// Both halves have to hold. A producer this pass cannot write means a slot nothing fills, and a
/// consumer it cannot rewrite means an instruction still expecting a `cap` where its operand is now
/// an address. Either one on its own is enough to leave the whole function as it was.
fn placeable(func: &Func) -> bool {
    for inst in walk(func) {
        let opcode = func[inst].opcode;
        let placed = matches!(
            opcode,
            Opcode::CapNull
                | Opcode::CapOf
                | Opcode::CapLoad
                | Opcode::CapNarrow
                | Opcode::CapRecover
                | Opcode::CapArg
                | Opcode::CapResult
        );
        // Every producer is on that list now, so nothing reaches this any more. It stays because
        // the next one added would otherwise be placed nowhere and read as an address, which is the
        // failure this whole function exists to keep out.
        if opcode.makes_capability() && !placed {
            return false;
        }
        let reads = func[func[inst].args].iter().any(|&value| func[value].ty.is_cap());
        let consumes = matches!(
            opcode,
            Opcode::CapStore
                | Opcode::CapLoad
                | Opcode::CapNarrow
                | Opcode::CapPublish
                | Opcode::CapYield
        );
        if reads && !consumes && !expecting(func, inst) {
            return false;
        }
        // And a publish has two things to be right about beyond its operands being capabilities,
        // both of which [`crate::frame::placeable`] is where they are argued: it has to be in front
        // of a call, and it cannot describe more arguments than the frame holds.
        if opcode == Opcode::CapPublish && !crate::frame::placeable(func, inst) {
            return false;
        }
        // The returned pointer's two ends have a tie of the same kind and are checked here for the
        // same reason, which is that the check has to happen before anything has been rewritten. A
        // yield away from its return would write the frame on a path that does not take it, and a
        // result behind a call with no frame would read the previous call site's answer.
        if opcode == Opcode::CapYield && !crate::frame::leaving(func, inst) {
            return false;
        }
        if opcode == Opcode::CapResult && !crate::frame::given(func, inst) {
            return false;
        }
    }
    true
}

/// Whether a call reads its capabilities in the one position this pass can leave alone.
///
/// A check the lowering has already turned into a call is the only reader of a capability here that
/// is not itself a capability instruction, and there is one because `__rucc_check_live` is handed
/// the capability rather than having it dropped. Nothing has to be rewritten for it: [`substitute`]
/// walks every instruction's operands, so the call comes out pointed at the slot like anything else
/// does, and the entry point was declared taking a `ptr` because the address of the slot is what
/// arrives. What has to hold is that the parameter standing for the operand really is that `ptr`,
/// since a signature saying `cap` would be one the back end has no calling convention for, and the
/// verifier rejects the module for it a few passes later rather than here.
///
/// A call passing a capability the signature does not name is not one of these. The list a variadic
/// call passes beyond its parameters is untyped by definition, so there is nothing to agree with,
/// and the honest answer for a shape this pass was not written for is to leave the function alone.
fn expecting(func: &Func, inst: Inst) -> bool {
    if !matches!(func[inst].opcode, Opcode::Call | Opcode::CallIndirect | Opcode::TailCall) {
        return false;
    }
    let Extra::Call(at) = func[inst].extra else { return false };
    let signature = func[at].signature;
    // An indirect call goes through an address it passes as its first operand, and no parameter
    // stands for that one.
    let indirect = usize::from(func[inst].opcode == Opcode::CallIndirect);
    for (index, &value) in func[func[inst].args].iter().enumerate() {
        if !func[value].ty.is_cap() {
            continue;
        }
        let named = index.checked_sub(indirect).and_then(|nth| func[signature].params.get(nth));
        match named {
            Some(param) if param.ty == Type::PTR => {}
            _ => return false,
        }
    }
    true
}

/// Points every `cap` typed block parameter at a slot instead.
///
/// A capability that is live across a branch is a value like any other to the optimizer, which runs
/// between the insertion pass and this one and will thread a jump or coalesce a parameter without
/// caring what the value is, so a block parameter of type `cap` is a shape that turns up on real
/// code. It used to leave the whole function unplaced. On the SQLite amalgamation that was 268
/// functions, and a function this pass gives up on keeps every `cap_of` in it, which the back end
/// has no rule for.
///
/// Every capability in the function is in a slot by the time this runs and a slot is an address, and
/// [`substitute`] has already rewritten the arguments on every edge into the block, so what arrives
/// on each edge is the address of the slot the incoming capability is in. There are two cases, and
/// what separates them is how many different slots can arrive.
///
/// One, not counting the parameter handed back to itself round a loop, is a parameter standing for a
/// single capability. Its readers are pointed at that slot and the parameter goes. The slot is an
/// `alloca` in the entry block, so it is live wherever the branch can go, and the producer wrote the
/// four words before the branch was taken.
///
/// More than one is a join, and that cannot be done the same way. Passing the incoming slot's
/// address along would leave the parameter naming a slot whose producer may run again before the
/// parameter's last reader, and inside a loop it does: in `next = cur->next; free(cur); cur = next`
/// the `cap_load` for `next` writes the slot that `cur` arrived as, and the check in front of the
/// free would then read the wrong object's capability. So a join gets a slot of its own and every
/// edge into it copies the four words across, which is [`joined`].
fn parameters(func: &mut Func, word: Type) {
    for block in func.blocks().collect::<Vec<Block>>() {
        let params = func[block].params.clone();
        let mut copied: Vec<(usize, Value)> = Vec::new();
        for (index, &param) in params.iter().enumerate() {
            if !func[param].ty.is_cap() {
                continue;
            }
            let arriving: HashSet<Value> =
                incoming(func, block, index).into_iter().filter(|&value| value != param).collect();
            let to = match arriving.iter().next() {
                Some(&only) if arriving.len() == 1 => only,
                _ => {
                    let first = func.insts(block).next().or_else(|| func.terminator(block));
                    let Some(at) = first else { continue };
                    let Some(own) = reserve(func, at) else { continue };
                    copied.push((index, own));
                    own
                }
            };
            renamed(func, param, to);
        }
        let gone: HashSet<usize> = params
            .iter()
            .enumerate()
            .filter(|&(_, &param)| func[param].ty.is_cap())
            .map(|(index, _)| index)
            .collect();
        if gone.is_empty() {
            continue;
        }
        joined(func, block, &gone, &copied, word);
        let dropped: HashSet<Value> = gone.iter().map(|&index| params[index]).collect();
        func.retain_params(block, |value| !dropped.contains(&value));
    }
}

/// What every edge into `block` passes as the parameter at `index`.
fn incoming(func: &Func, block: Block, index: usize) -> Vec<Value> {
    let mut found = Vec::new();
    for pred in func.blocks() {
        let Some(term) = func.terminator(pred) else { continue };
        for call in func.successors(term) {
            if call.block == block {
                found.extend(func[call.args].get(index).copied());
            }
        }
    }
    found
}

/// Points everything that reads `from`, operands and edge arguments alike, at `to` instead.
fn renamed(func: &mut Func, from: Value, to: Value) {
    let with = |value: Value| if value == from { to } else { value };
    for inst in walk(func) {
        let args = func[inst].args;
        func.rewrite(args, with);
        for call in func.successors(inst).collect::<Vec<_>>() {
            func.rewrite(call.args, with);
        }
    }
}

/// Takes the parameters at the positions in `gone` off every edge into `block`, copying the ones in
/// `copied` into their own slots on the way.
///
/// Every word is read before any is written, because an edge round a loop can hand one join's slot
/// to another join of the same block, and writing the first before reading the second would copy
/// what this edge just put there rather than what the last iteration left. An edge out of a branch
/// with more than one target gets a block of its own to do the copying in, since copying in front of
/// the branch would overwrite the slot on the way out of the loop as well as on the way round it.
/// A computed `goto` and an `asm goto` are the exception, because a block put on one of their edges
/// is one the jump goes straight past, and those copy in front of the branch.
fn joined(
    func: &mut Func,
    block: Block,
    gone: &HashSet<usize>,
    copied: &[(usize, Value)],
    word: Type,
) {
    for pred in func.blocks().collect::<Vec<Block>>() {
        let Some(term) = func.terminator(pred) else { continue };
        let targets = func.target_list(term);
        let many = targets.as_usize_range().len() > 1;
        for at in targets.iter() {
            let call = func[at];
            if call.block != block {
                continue;
            }
            let passed = func[call.args].to_vec();
            let kept: Vec<Value> = passed
                .iter()
                .enumerate()
                .filter(|(index, _)| !gone.contains(index))
                .map(|(_, &value)| value)
                .collect();
            let pairs: Vec<(Value, Value)> = copied
                .iter()
                .filter_map(|&(index, own)| passed.get(index).map(|&from| (from, own)))
                .filter(|&(from, own)| from != own)
                .collect();
            let splittable = !matches!(func[term].opcode, Opcode::IndirectBr | Opcode::InlineAsm);
            if pairs.is_empty() || !many || !splittable {
                let args = func.push_values(&kept);
                func.set_block_call(at, BlockCall { args, ..call });
                if !pairs.is_empty() {
                    copy(func, term, &pairs, word);
                }
                continue;
            }
            let edge = func.create_block();
            let span = func.span(term);
            let jump = Builder::new(func, edge).at(span).jump(block, &kept);
            let args = func.push_values(&[]);
            func.set_block_call(at, BlockCall { block: edge, args, ..call });
            copy(func, jump, &pairs, word);
        }
    }
}

/// Copies the four words of each `(from, to)` pair of slots, in front of `inst`, reads first.
fn copy(func: &mut Func, inst: Inst, pairs: &[(Value, Value)], word: Type) {
    let span = func.span(inst);
    let mut read = Vec::new();
    for &(from, to) in pairs {
        for step in 0..BYTES / WORD {
            let at = offset(func, inst, from, step * WORD, word);
            let args = func.push_values(&[at]);
            let extra = Extra::Mem(func.add_mem(plain()));
            let data = InstData { args, extra, ..InstData::new(Opcode::Load) };
            let made = func.create_inst(data, &[word], span);
            func.insert_before(made, inst);
            let Some(value) = func[made].results().next() else { continue };
            read.push((to, step, value));
        }
    }
    for (to, step, value) in read {
        let at = offset(func, inst, to, step * WORD, word);
        let args = func.push_values(&[value, at]);
        let extra = Extra::Mem(func.add_mem(plain()));
        let data = InstData { args, extra, ..InstData::new(Opcode::Store) };
        let made = func.create_inst(data, &[], span);
        func.insert_before(made, inst);
    }
}

/// What one word of a slot is to the back end: eight bytes, aligned, and nothing more to say.
fn plain() -> MemInfo {
    MemInfo {
        size: WORD,
        align: ALIGN,
        order: MemOrder::NotAtomic,
        tbaa: None,
        owns: 0,
        restrict: Restrict::NONE,
    }
}

/// Points every reader of a capability at the address of the slot holding it.
fn substitute(func: &mut Func, moved: &HashMap<Value, Value>) {
    let with = |value: Value| moved.get(&value).copied().unwrap_or(value);
    for inst in walk(func) {
        let args = func[inst].args;
        func.rewrite(args, with);
        for call in func.successors(inst).collect::<Vec<_>>() {
            func.rewrite(call.args, with);
        }
    }
}

/// `cap_null` becomes a slot with four zero words in it.
///
/// Zero is the whole of `rucc_safe_rt::layout::Cap::BOTTOM`, because a version of zero is what the
/// lifetime plane holds for storage nobody owns and the other three fields of the bottom capability
/// are zero for want of anything to say. So this is four stores and no call, which matters because
/// a null pointer constant is common enough that a call to say so would be visible.
///
/// Written out as words rather than left to a `memset`, for the same reason: four stores of an
/// immediate is what the back end would fold a thirty two byte clear into anyway, and going through
/// the library would put a call on the path of every null.
fn nulled(func: &mut Func, word: Type, inst: Inst, address: Value) {
    let span = func.span(inst);
    let zero = konst(func, inst, Imm::int(0, word), word);
    for step in 0..BYTES / WORD {
        let at = offset(func, inst, address, step * WORD, word);
        let info = MemInfo {
            size: WORD,
            align: ALIGN,
            order: MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        };
        let extra = Extra::Mem(func.add_mem(info));
        let args = func.push_values(&[zero, at]);
        let data = InstData { args, extra, ..InstData::new(Opcode::Store) };
        let made = func.create_inst(data, &[], span);
        func.insert_before(made, inst);
    }
    func.remove_inst(inst);
}

/// The pointer a `cap_of` is asking about, when the pointer is one an allocator just returned.
///
/// Nothing for any other instruction and nothing for any other `cap_of`, which is what makes this
/// the whole of the shape this pass recognises rather than a heuristic with an outside.
///
/// [`Flags::HEAP`] on the defining call is the thing being read, and `rucc_opt::heap::annotate` is
/// what writes it: a direct call of a name on its list that the module does not define itself.
/// Believing the flag is believing the same claim the aliasing summaries already rest on, so a
/// program where it is wrong has larger problems than this. The result has to be the call's first,
/// because a call with several is not one of those names.
///
/// What happens for a pointer the flag is not on is the plane walk, which [`allocated`] is where it
/// is argued. Nothing is refused here, and the answer being no is a cost rather than a failure.
fn fresh(func: &Func, inst: Inst) -> Option<Value> {
    if func[inst].opcode != Opcode::CapOf {
        return None;
    }
    let &[base] = &func[func[inst].args] else { return None };
    let Def::Result { inst: call, index: 0 } = func[base].def else { return None };
    (func[call].opcode == Opcode::Call && func[call].flags.contains(Flags::HEAP)).then_some(base)
}

/// `cap_of` becomes `__rucc_cap_made(slot, base)` or `__rucc_cap_recover(slot, at)`.
///
/// Which of the two is not a property of the instruction, it is what [`fresh`] could find out about
/// the pointer. A pointer traced back to an allocation site gets the cheap answer, which is a
/// subtract and a load off the header the allocator wrote. Anything else gets the plane walk, which
/// is linear in the size of the object the address landed in and comes back marked as recovered so
/// that the summary counts it. The two calls take the same two arguments, for the reason
/// [`recovered`] gives, so the difference between them here is the name.
///
/// Falling back rather than refusing is the point of this being the last of the producers. Every
/// other one is a pointer whose provenance the compiler still holds, and by the time they are all
/// there a `cap_of` that cannot be traced is a pointer something really did lose track of. Refusing
/// it would leave the whole function's capabilities where they were, which is not more conservative
/// than the walk, it is the same walk once per check rather than once per pointer.
///
/// An interior pointer is always the second answer, and that is the question document 05 section
/// 5.2.3 leaves open rather than a gap in it. `__rucc_cap_made` wants the base, because the header
/// is behind the payload and finding it is a subtract by a constant. [`fresh`] hands it the call's
/// own result or nothing, so the cheap answer is never reached with a pointer into the middle of
/// something.
///
/// Beside the instruction rather than in place of it, unlike every other rewrite in this pass and in
/// [`mod@crate::lower`]. The call gives nothing back, because the capability it produced went into
/// the slot the first argument names, and the instruction it replaces gave back a capability. So the
/// call goes in front and the `cap_of` comes out, and everything that read the capability is pointed
/// at the slot by [`substitute`].
///
/// In front of the `cap_of` rather than at the top of the function, because that is where the base
/// pointer is: the call that produced it has run by then and nothing has to be kept live any longer
/// than it already was. The slot itself is in the entry block for the reason [`reserve`] gives.
fn allocated(func: &mut Func, names: &mut Interner, inst: Inst, address: Value) {
    let (routine, base) = match fresh(func, inst) {
        Some(base) => ("__rucc_cap_made", base),
        None => {
            let &[at] = &func[func[inst].args] else { return };
            ("__rucc_cap_recover", at)
        }
    };
    let params = &[Type::PTR; 2];
    let args = &[address, base];
    let data = crate::lower::calling(func, names, routine, params, &[], args);
    let made = func.create_inst(data, &[], func.span(inst));
    func.insert_before(made, inst);
    func.remove_inst(inst);
}

/// `cap_recover` becomes `__rucc_cap_recover(slot, addr)`.
///
/// The same two arguments [`allocated`] has and deliberately so, since the runtime declares the pair
/// as one shape and the only difference between them is which name the pass picks. What differs is
/// behind the name. `__rucc_cap_made` is a subtract and a load off a header the allocator wrote, and
/// this one is the plane walk, which is linear in the size of the object and gives back a capability
/// marked as recovered so that the summary can count it.
///
/// So this is the expensive producer and it is also the only one that always has an answer, which is
/// why it is the one every other box falls back to. Nothing here decides when it is reached. That is
/// the front end's: a pointer whose provenance the compiler still holds gets one of the cheap
/// producers, and this is what is left for an address that arrived from outside the instrumented
/// world. Where the placing is concerned it is the plainest of the five, one pointer in and one slot
/// out, with neither end of it reading a capability.
fn recovered(func: &mut Func, names: &mut Interner, inst: Inst, address: Value) {
    let &[addr] = &func[func[inst].args] else { return };
    let args = &[address, addr];
    let data = crate::lower::calling(func, names, "__rucc_cap_recover", &[Type::PTR; 2], &[], args);
    let made = func.create_inst(data, &[], func.span(inst));
    func.insert_before(made, inst);
    func.remove_inst(inst);
}

/// `cap_load` becomes `__rucc_cap_load(slot, container, at, value)`.
///
/// The slot in front and the opcode's own three operands after it, in the order they were already
/// in, because tamnd/rucc#1080 gave the opcode the shape of the call. What the runtime is handed is
/// the capability of the object the word lives in, the address of the word, and the pointer that was
/// read out of it, and the first of those three is a slot address by the time this runs rather than
/// a capability, because [`substitute`] has been over the instruction already.
///
/// Beside the instruction and not in place of it, for the reason [`allocated`] gives. The difference
/// from every other rewrite here is that this one is both ends at once: the operand it reads came
/// out of a slot some other producer filled, and the result it writes is a slot of its own that a
/// later reader will be pointed at.
fn read(func: &mut Func, names: &mut Interner, inst: Inst, address: Value) {
    let mut args = vec![address];
    args.extend_from_slice(&func[func[inst].args]);
    let data = crate::lower::calling(func, names, "__rucc_cap_load", &[Type::PTR; 4], &[], &args);
    let made = func.create_inst(data, &[], func.span(inst));
    func.insert_before(made, inst);
    func.remove_inst(inst);
}

/// `cap_narrow` becomes `__rucc_cap_narrow(slot, base, off, len)`.
///
/// The only one of these whose operands are not already the right types. The offset and the length
/// are integers in whatever width the arithmetic that produced them was in, and the runtime declares
/// both as `size_t`, so [`crate::lower::fitted`] puts them in the target's width first. The verifier
/// makes the two agree with each other, so either one being narrow means both are.
///
/// The arithmetic is a call rather than four instructions here, which is the one place this pass
/// could have written the words itself and does not. `rucc_safe_rt::layout::Cap::narrowed` is three
/// comparisons and a copy, so nothing is being hidden from the optimizer that it could have used,
/// and writing it here would put the field order of a capability in a second place. [`nulled`] is
/// the exception that shows the rule: four zero words is the whole of the bottom capability whatever
/// the field order turns out to be.
fn narrowed(func: &mut Func, names: &mut Interner, word: Type, inst: Inst, address: Value) {
    let [base, off, len] = func[func[inst].args] else { return };
    let off = crate::lower::fitted(func, inst, off, word);
    let len = crate::lower::fitted(func, inst, len, word);
    let params = &[Type::PTR, Type::PTR, word, word];
    let args = &[address, base, off, len];
    let data = crate::lower::calling(func, names, "__rucc_cap_narrow", params, &[], args);
    let made = func.create_inst(data, &[], func.span(inst));
    func.insert_before(made, inst);
    func.remove_inst(inst);
}

/// `cap_store` becomes `__rucc_cap_store(container, at, value, capability)`.
///
/// Four addresses, since both capabilities are slots by the time this runs and the other two
/// operands were addresses to begin with. That is the signature `rucc_safe_rt::cap` declares, and
/// tamnd/rucc#1080 shaped the opcode to match it, so there is nothing to compute here.
fn stored(func: &mut Func, names: &mut Interner, inst: Inst) {
    let args: Vec<Value> = func[func[inst].args].to_vec();
    crate::lower::call(func, names, inst, "__rucc_cap_store", &[Type::PTR; 4], &[], &args);
}

/// Reserves thirty two bytes at the top of the entry block and gives back their address.
///
/// At the top for the reason [`mod@crate::promise`] puts a `restrict` scope there: that is where the
/// verifier wants an `alloca` that is not a variable length array, and one left in a loop would
/// take the stack down another thirty two bytes every time round.
fn reserve(func: &mut Func, inst: Inst) -> Option<Value> {
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

/// The address `bytes` along from `address`, which for the first word is the address itself.
pub(crate) fn offset(func: &mut Func, inst: Inst, address: Value, bytes: u64, word: Type) -> Value {
    if bytes == 0 {
        return address;
    }
    let step = konst(func, inst, Imm::int(i128::from(bytes), word), word);
    let args = func.push_values(&[address, step]);
    let data = InstData { args, ..InstData::new(Opcode::PtrAdd) };
    let made = func.create_inst(data, &[Type::PTR], func.span(inst));
    func.insert_before(made, inst);
    func[made].results().next().expect("an address created with one result has one")
}

/// Puts an integer constant in front of `inst` and gives back what it produced.
pub(crate) fn konst(func: &mut Func, inst: Inst, imm: Imm, ty: Type) -> Value {
    let extra = Extra::Imm(func.add_imm(imm));
    let data = InstData { extra, ..InstData::new(Opcode::IConst) };
    let made = func.create_inst(data, &[ty], func.span(inst));
    func.insert_before(made, inst);
    func[made].results().next().expect("a constant created with one result has one")
}

#[cfg(test)]
mod tests {
    use rucc_base::Symbol;
    use rucc_ir::{Builder, CallInfo, Module, Signature, print_func, verify_func};
    use rucc_target::{Arch, Env, Os, TargetInfo, Triple};

    use super::*;

    /// A module for the printer and the verifier to resolve names against.
    fn module(names: &mut Interner) -> Module {
        let target = TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu));
        Module::new(names.intern("f.c"), &target)
    }

    /// Fails the test with everything the verifier had to say, if it had anything.
    fn believed(unit: &Module, func: &Func, names: &Interner) {
        if let Err(errors) = verify_func(unit, func, names) {
            panic!("that was expected to be believed: {errors:#?}");
        }
    }

    /// A function holding one `cap_null`, with `extra` instructions built on top of it.
    ///
    /// The builder is handed the capability, so a test decides for itself whether anything reads
    /// one, which is the difference between the two things this pass does with a capability.
    fn built(names: &mut Interner, extra: impl FnOnce(&mut Builder<'_>, Value, Value)) -> Func {
        let mut func = Func::new(names.intern("f"), Signature::new().with_params(&[Type::PTR]));
        let entry = func.create_block();
        let at = func.append_param(entry, Type::PTR);
        let mut b = Builder::new(&mut func, entry);
        let cap = b.value(InstData::new(Opcode::CapNull), Type::CAP);
        extra(&mut b, cap, at);
        b.ret(&[]);
        func
    }

    /// A `cap_of` over what a call returned, stored, with the call vouched for or not.
    ///
    /// The flag is the whole of what this pass reads to tell an allocation site from any other call,
    /// so the version without it is a test of the fall through rather than of a different program.
    fn called(names: &mut Interner, vouched: bool) -> Func {
        let word = Type::int(64);
        let mut func =
            Func::new(names.intern("f"), Signature::new().with_params(&[word, Type::PTR]));
        let entry = func.create_block();
        let size = func.append_param(entry, word);
        let at = func.append_param(entry, Type::PTR);
        let sig = Signature::new().with_params(&[word]).with_returns(&[Type::PTR]);
        let sig = func.add_signature(sig);
        let callee = names.intern("malloc");
        let varargs = func.push_abis(&[]);
        let info = func.add_call(CallInfo { callee: Some(callee), signature: sig, varargs });
        let args = func.push_values(&[size]);
        let flags = if vouched { Flags::HEAP } else { Flags::default() };
        let mut b = Builder::new(&mut func, entry);
        let data =
            InstData { args, extra: Extra::Call(info), flags, ..InstData::new(Opcode::Call) };
        let base = b.value(data, Type::PTR);
        let args = b.func().push_values(&[base]);
        let cap = b.value(InstData { args, ..InstData::new(Opcode::CapOf) }, Type::CAP);
        let args = b.func().push_values(&[cap, at, at, cap]);
        b.inst(InstData { args, ..InstData::new(Opcode::CapStore) }, &[]);
        b.ret(&[]);
        func
    }

    /// Adds a call of `callee` reading those values, under that signature.
    ///
    /// What the check lowering leaves behind, which is the one reader of a capability here that is
    /// not a capability instruction. The signature is given separately from the arguments so a test
    /// can declare one that does not describe what is passed, which is the case this pass has to
    /// refuse rather than rewrite.
    fn calling(b: &mut Builder<'_>, callee: Symbol, sig: Signature, args: &[Value]) {
        let sig = b.func().add_signature(sig);
        let callee = Some(callee);
        let varargs = b.func().push_abis(&[]);
        let info = b.func().add_call(CallInfo { callee, signature: sig, varargs });
        let args = b.func().push_values(args);
        b.inst(InstData { args, extra: Extra::Call(info), ..InstData::new(Opcode::Call) }, &[]);
    }

    /// How many instructions with that opcode the function holds.
    fn count(func: &Func, opcode: Opcode) -> usize {
        walk(func).into_iter().filter(|&inst| func[inst].opcode == opcode).count()
    }

    /// Whether any value in the function is still a capability.
    fn any_capability(func: &Func) -> bool {
        walk(func).into_iter().any(|inst| func[inst].results().any(|value| func[value].ty.is_cap()))
    }

    #[test]
    fn a_capability_nothing_reads_is_taken_out() {
        let mut names = Interner::new();
        let mut func = built(&mut names, |_, _, _| {});
        frames(&mut func, &mut names, Type::int(64));
        assert_eq!(count(&func, Opcode::CapNull), 0);
        assert_eq!(count(&func, Opcode::Alloca), 0);
        believed(&module(&mut names), &func, &names);
    }

    #[test]
    fn a_capability_something_reads_becomes_four_zero_words_of_frame() {
        let mut names = Interner::new();
        let mut func = built(&mut names, |b, cap, at| {
            let args = b.func().push_values(&[cap, at, at, cap]);
            b.inst(InstData { args, ..InstData::new(Opcode::CapStore) }, &[]);
        });
        frames(&mut func, &mut names, Type::int(64));
        assert_eq!(count(&func, Opcode::Alloca), 1);
        assert_eq!(count(&func, Opcode::Store), 4);
        assert_eq!(count(&func, Opcode::CapNull), 0);
        assert_eq!(count(&func, Opcode::CapStore), 0);
        assert!(!any_capability(&func));
        let unit = module(&mut names);
        let text = print_func(&unit, &func, &names);
        assert!(text.contains("__rucc_cap_store"), "{text}");
        believed(&unit, &func, &names);
    }

    /// A check that has already become a call still gets its capability pointed at the slot.
    ///
    /// `__rucc_check_live` is the first entry point handed one, so this is the shape the rest of the
    /// checks will arrive in as they follow. Nothing special happens to it: the call is an ordinary
    /// reader as far as the substitution is concerned, and what has to hold is that the pass agrees
    /// to place the function at all rather than leaving a `cap` where the back end would meet one.
    #[test]
    fn a_call_declared_taking_the_address_is_pointed_at_the_slot() {
        let mut names = Interner::new();
        let live = names.intern("__rucc_check_live");
        let mut func = built(&mut names, |b, cap, at| {
            calling(b, live, Signature::new().with_params(&[Type::PTR; 3]), &[cap, at, at]);
        });
        frames(&mut func, &mut names, Type::int(64));
        assert_eq!(count(&func, Opcode::Alloca), 1);
        assert_eq!(count(&func, Opcode::CapNull), 0);
        assert!(!any_capability(&func));

        let call = walk(&func)
            .into_iter()
            .find(|&inst| func[inst].opcode == Opcode::Call)
            .expect("the check was already a call");
        let slot = walk(&func)
            .into_iter()
            .find(|&inst| func[inst].opcode == Opcode::Alloca)
            .and_then(|inst| func[inst].results().next())
            .expect("the capability got a slot");
        assert_eq!(func[func[call].args][0], slot);
        believed(&module(&mut names), &func, &names);
    }

    /// And a call passing one where its signature names nothing is left alone.
    ///
    /// The unnamed half of a variadic call is untyped, so there is no parameter to agree with and no
    /// way to know the callee reads an address. Leaving the function as it was keeps that out of the
    /// back end, where a `cap` is a type nothing has been taught.
    #[test]
    fn a_call_passing_a_capability_the_signature_does_not_name_leaves_the_function_alone() {
        let mut names = Interner::new();
        let odd = names.intern("printf");
        let mut func = built(&mut names, |b, cap, at| {
            let sig = Signature::new().with_params(&[Type::PTR]).variadic();
            calling(b, odd, sig, &[at, cap]);
        });
        frames(&mut func, &mut names, Type::int(64));
        assert_eq!(count(&func, Opcode::Alloca), 0);
        assert_eq!(count(&func, Opcode::CapNull), 1);
        assert!(any_capability(&func));
    }

    #[test]
    fn each_capability_gets_a_slot_of_its_own() {
        let mut names = Interner::new();
        let mut func = built(&mut names, |b, cap, at| {
            let other = b.value(InstData::new(Opcode::CapNull), Type::CAP);
            let args = b.func().push_values(&[cap, at, at, other]);
            b.inst(InstData { args, ..InstData::new(Opcode::CapStore) }, &[]);
        });
        frames(&mut func, &mut names, Type::int(64));
        assert_eq!(count(&func, Opcode::Alloca), 2);
        assert_eq!(count(&func, Opcode::Store), 8);
        believed(&module(&mut names), &func, &names);
    }

    #[test]
    fn every_slot_is_reserved_in_the_entry_block() {
        let mut names = Interner::new();
        let mut func = built(&mut names, |b, cap, at| {
            let args = b.func().push_values(&[cap, at, at, cap]);
            b.inst(InstData { args, ..InstData::new(Opcode::CapStore) }, &[]);
        });
        frames(&mut func, &mut names, Type::int(64));
        let entry = func.entry().expect("the function has a body");
        let here = func.insts(entry).filter(|&inst| func[inst].opcode == Opcode::Alloca).count();
        assert_eq!(here, count(&func, Opcode::Alloca));
    }

    #[test]
    fn a_capability_for_a_fresh_allocation_is_one_call_and_no_stores() {
        let mut names = Interner::new();
        let mut func = called(&mut names, true);
        frames(&mut func, &mut names, Type::int(64));
        assert_eq!(count(&func, Opcode::Alloca), 1);
        assert_eq!(count(&func, Opcode::CapOf), 0);
        // Nothing writes the four words here, unlike the null case. The runtime fills the slot out
        // of the instance's own header, which is the whole point of the site being cheap.
        assert_eq!(count(&func, Opcode::Store), 0);
        assert!(!any_capability(&func));
        let unit = module(&mut names);
        let text = print_func(&unit, &func, &names);
        assert!(text.contains("__rucc_cap_made"), "{text}");
        assert!(text.contains("__rucc_cap_store"), "{text}");
        believed(&unit, &func, &names);
    }

    #[test]
    fn a_capability_for_a_pointer_nobody_vouched_for_falls_back_to_the_plane_walk() {
        let mut names = Interner::new();
        let mut func = called(&mut names, false);
        frames(&mut func, &mut names, Type::int(64));
        assert_eq!(count(&func, Opcode::CapOf), 0);
        assert_eq!(count(&func, Opcode::Alloca), 1);
        assert!(!any_capability(&func));
        // The expensive answer and the only one that is always right. Refusing instead would leave
        // every capability in the function where it was, which is the same walk once per check
        // rather than once per pointer, so it is not the more conservative direction.
        let unit = module(&mut names);
        let text = print_func(&unit, &func, &names);
        assert!(text.contains("__rucc_cap_recover"), "{text}");
        assert!(!text.contains("__rucc_cap_made"), "{text}");
        believed(&unit, &func, &names);
    }

    #[test]
    fn the_cheap_answer_is_never_asked_about_a_pointer_into_the_middle_of_something() {
        let mut names = Interner::new();
        let word = Type::int(64);
        let mut func = built(&mut names, |b, _, at| {
            let step = number(b, 16, word);
            let args = b.func().push_values(&[at, step]);
            let inner = b.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR);
            let args = b.func().push_values(&[inner]);
            let mine = b.value(InstData { args, ..InstData::new(Opcode::CapOf) }, Type::CAP);
            let args = b.func().push_values(&[mine, inner, inner, mine]);
            b.inst(InstData { args, ..InstData::new(Opcode::CapStore) }, &[]);
        });
        frames(&mut func, &mut names, word);
        // `__rucc_cap_made` subtracts a constant to reach the header, which is right for a pointer
        // to the base of an object and wrong for one into the middle. This is the question document
        // 05 section 5.2.3 leaves open, and the answer is that the cheap path is never reached with
        // one, because what it asks for is an allocation site's own result.
        let unit = module(&mut names);
        let text = print_func(&unit, &func, &names);
        assert!(text.contains("__rucc_cap_recover"), "{text}");
        assert!(!text.contains("__rucc_cap_made"), "{text}");
        believed(&unit, &func, &names);
    }

    /// An integer constant of that width, in the block being built.
    fn number(b: &mut Builder<'_>, value: i128, ty: Type) -> Value {
        let imm = b.func().add_imm(Imm::int(value, ty));
        b.value(InstData { extra: Extra::Imm(imm), ..InstData::new(Opcode::IConst) }, ty)
    }

    /// Whether the value is the address an `alloca` gave back, which is what a slot looks like.
    fn slot(func: &Func, value: Value) -> bool {
        matches!(func[value].def, Def::Result { inst, .. } if func[inst].opcode == Opcode::Alloca)
    }

    #[test]
    fn a_capability_read_out_of_memory_is_one_call_with_both_slots_in_hand() {
        let mut names = Interner::new();
        let mut func = built(&mut names, |b, cap, at| {
            let args = b.func().push_values(&[cap, at, at]);
            let got = b.value(InstData { args, ..InstData::new(Opcode::CapLoad) }, Type::CAP);
            let args = b.func().push_values(&[got, at, at, got]);
            b.inst(InstData { args, ..InstData::new(Opcode::CapStore) }, &[]);
        });
        frames(&mut func, &mut names, Type::int(64));
        // Two slots, and the four stores are the null's. The one the read fills is written by the
        // runtime, so nothing in the function touches its words.
        assert_eq!(count(&func, Opcode::Alloca), 2);
        assert_eq!(count(&func, Opcode::Store), 4);
        assert_eq!(count(&func, Opcode::CapLoad), 0);
        assert!(!any_capability(&func));

        // The part the two walks are for. The first argument is the slot this read fills and the
        // second is the slot the container capability went into, so the operand it reads was
        // pointed at a slot before the instruction became a call.
        let call = walk(&func)
            .into_iter()
            .find(|&inst| func[inst].opcode == Opcode::Call)
            .expect("the read became a call");
        let args: Vec<Value> = func[func[call].args].to_vec();
        assert_eq!(args.len(), 4);
        assert_ne!(args[0], args[1]);
        assert!(slot(&func, args[0]));
        assert!(slot(&func, args[1]));

        let unit = module(&mut names);
        let text = print_func(&unit, &func, &names);
        assert!(text.contains("__rucc_cap_load"), "{text}");
        assert!(text.contains("__rucc_cap_store"), "{text}");
        believed(&unit, &func, &names);
    }

    #[test]
    fn a_capability_read_beside_one_this_pass_cannot_place_is_left_where_it_was() {
        let mut names = Interner::new();
        let mut func = built(&mut names, |b, _, at| {
            let args = b.func().push_values(&[at]);
            let taken = b.value(InstData { args, ..InstData::new(Opcode::CapOf) }, Type::CAP);
            let args = b.func().push_values(&[taken, at, at]);
            let got = b.value(InstData { args, ..InstData::new(Opcode::CapLoad) }, Type::CAP);
            // A yield away from the return it is meant to be about, which is one of the refusals.
            let args = b.func().push_values(&[got]);
            b.inst(InstData { args, ..InstData::new(Opcode::CapYield) }, &[]);
            let args = b.func().push_values(&[got, at, at, got]);
            b.inst(InstData { args, ..InstData::new(Opcode::CapStore) }, &[]);
        });
        frames(&mut func, &mut names, Type::int(64));
        // Both producers are ones this pass understands and the yield beside them is not, and
        // placing them anyway would leave the yield holding an operand that is now an address.
        assert_eq!(count(&func, Opcode::CapLoad), 1);
        assert_eq!(count(&func, Opcode::CapOf), 1);
        assert_eq!(count(&func, Opcode::Alloca), 0);
        believed(&module(&mut names), &func, &names);
    }

    #[test]
    fn a_capability_read_nobody_looks_at_is_taken_out_with_the_one_it_read_from() {
        let mut names = Interner::new();
        let mut func = built(&mut names, |b, cap, at| {
            let args = b.func().push_values(&[cap, at, at]);
            b.value(InstData { args, ..InstData::new(Opcode::CapLoad) }, Type::CAP);
        });
        frames(&mut func, &mut names, Type::int(64));
        // The fixpoint in `prune` is what takes the pair, since the null is only unread once the
        // read that was its one reader has gone.
        assert_eq!(count(&func, Opcode::CapLoad), 0);
        assert_eq!(count(&func, Opcode::CapNull), 0);
        assert_eq!(count(&func, Opcode::Alloca), 0);
        believed(&module(&mut names), &func, &names);
    }

    #[test]
    fn a_narrowed_capability_is_one_call_with_the_two_numbers_in_the_targets_width() {
        let mut names = Interner::new();
        let word = Type::int(64);
        let narrow = Type::int(32);
        let mut func = built(&mut names, |b, cap, at| {
            let off = number(b, 16, narrow);
            let len = number(b, 8, narrow);
            let args = b.func().push_values(&[cap, off, len]);
            let member = b.value(InstData { args, ..InstData::new(Opcode::CapNarrow) }, Type::CAP);
            let args = b.func().push_values(&[member, at, at, member]);
            b.inst(InstData { args, ..InstData::new(Opcode::CapStore) }, &[]);
        });
        frames(&mut func, &mut names, word);
        assert_eq!(count(&func, Opcode::Alloca), 2);
        assert_eq!(count(&func, Opcode::CapNarrow), 0);
        // Both numbers were written in a width that is not the target's, so both are extended. The
        // verifier makes the pair agree with each other, so it is never one of the two.
        assert_eq!(count(&func, Opcode::ZExt), 2);
        assert!(!any_capability(&func));

        let call = walk(&func)
            .into_iter()
            .find(|&inst| func[inst].opcode == Opcode::Call)
            .expect("the narrowing became a call");
        let args: Vec<Value> = func[func[call].args].to_vec();
        assert_eq!(args.len(), 4);
        assert!(slot(&func, args[0]));
        assert!(slot(&func, args[1]));
        assert_eq!(func[args[2]].ty, word);
        assert_eq!(func[args[3]].ty, word);

        let unit = module(&mut names);
        let text = print_func(&unit, &func, &names);
        assert!(text.contains("__rucc_cap_narrow"), "{text}");
        believed(&unit, &func, &names);
    }

    #[test]
    fn a_recovered_capability_is_one_call_with_the_address_it_was_asked_about() {
        let mut names = Interner::new();
        let mut func = built(&mut names, |b, _, at| {
            let args = b.func().push_values(&[at]);
            let got = b.value(InstData { args, ..InstData::new(Opcode::CapRecover) }, Type::CAP);
            let args = b.func().push_values(&[got, at, at, got]);
            b.inst(InstData { args, ..InstData::new(Opcode::CapStore) }, &[]);
        });
        frames(&mut func, &mut names, Type::int(64));
        // One slot and nothing writing its words, since the walk is the runtime's and its answer
        // goes straight into the slot. The null the fixture starts from is unread and went in
        // `prune`.
        assert_eq!(count(&func, Opcode::Alloca), 1);
        assert_eq!(count(&func, Opcode::Store), 0);
        assert_eq!(count(&func, Opcode::CapRecover), 0);
        assert!(!any_capability(&func));

        let call = walk(&func)
            .into_iter()
            .find(|&inst| func[inst].opcode == Opcode::Call)
            .expect("the recovery became a call");
        let args: Vec<Value> = func[func[call].args].to_vec();
        assert_eq!(args.len(), 2);
        assert!(slot(&func, args[0]));
        // The address the opcode was asked about, which is this function's own argument handed over
        // as it stands. Nothing about it is worked out here, which is the whole point of the walk
        // being where it is.
        assert!(matches!(func[args[1]].def, Def::Param { .. }));

        let unit = module(&mut names);
        let text = print_func(&unit, &func, &names);
        assert!(text.contains("__rucc_cap_recover"), "{text}");
        believed(&unit, &func, &names);
    }

    #[test]
    fn a_member_of_something_recovered_is_two_slots_and_the_narrow_reads_the_first() {
        let mut names = Interner::new();
        let word = Type::int(64);
        let mut func = built(&mut names, |b, _, at| {
            let args = b.func().push_values(&[at]);
            let whole = b.value(InstData { args, ..InstData::new(Opcode::CapRecover) }, Type::CAP);
            let off = number(b, 16, word);
            let len = number(b, 8, word);
            let args = b.func().push_values(&[whole, off, len]);
            let member = b.value(InstData { args, ..InstData::new(Opcode::CapNarrow) }, Type::CAP);
            let args = b.func().push_values(&[member, at, at, member]);
            b.inst(InstData { args, ..InstData::new(Opcode::CapStore) }, &[]);
        });
        frames(&mut func, &mut names, word);
        assert_eq!(count(&func, Opcode::Alloca), 2);
        assert_eq!(count(&func, Opcode::CapRecover), 0);
        assert_eq!(count(&func, Opcode::CapNarrow), 0);
        // Both numbers are already the target's width, so neither of them is extended.
        assert_eq!(count(&func, Opcode::ZExt), 0);
        assert!(!any_capability(&func));

        // A pointer from outside and then a member of it, which is the pair document 05 section 5.3
        // produces most often. The narrowing reads the slot the recovery filled, so the two walks
        // are doing over two producers what they already did over a producer and a reader.
        let calls: Vec<Inst> =
            walk(&func).into_iter().filter(|&inst| func[inst].opcode == Opcode::Call).collect();
        assert_eq!(calls.len(), 3);
        let recovery: Vec<Value> = func[func[calls[0]].args].to_vec();
        let narrowing: Vec<Value> = func[func[calls[1]].args].to_vec();
        assert_eq!(narrowing[1], recovery[0]);
        assert_ne!(narrowing[0], narrowing[1]);

        let unit = module(&mut names);
        let text = print_func(&unit, &func, &names);
        assert!(text.contains("__rucc_cap_recover"), "{text}");
        assert!(text.contains("__rucc_cap_narrow"), "{text}");
        believed(&unit, &func, &names);
    }

    /// A function that calls `g(at)`, with `extra` putting instructions in front of the call.
    ///
    /// The capability and the pointer are handed over the way [`built`] hands them over, so a test
    /// decides for itself what the call is told about them, which is the whole of what the frame
    /// pair is for.
    fn passing(names: &mut Interner, extra: impl FnOnce(&mut Builder<'_>, Value, Value)) -> Func {
        let mut func = Func::new(names.intern("f"), Signature::new().with_params(&[Type::PTR]));
        let entry = func.create_block();
        let at = func.append_param(entry, Type::PTR);
        let sig = func.add_signature(Signature::new().with_params(&[Type::PTR]));
        let callee = names.intern("g");
        let varargs = func.push_abis(&[]);
        let info = func.add_call(CallInfo { callee: Some(callee), signature: sig, varargs });
        let mut b = Builder::new(&mut func, entry);
        let cap = b.value(InstData::new(Opcode::CapNull), Type::CAP);
        extra(&mut b, cap, at);
        let args = b.func().push_values(&[at]);
        let data = InstData { args, extra: Extra::Call(info), ..InstData::new(Opcode::Call) };
        b.inst(data, &[]);
        b.ret(&[]);
        func
    }

    /// Where in the function that instruction is, counting from the entry block.
    fn place(func: &Func, inst: Inst) -> usize {
        walk(func).into_iter().position(|at| at == inst).expect("the instruction is in the body")
    }

    #[test]
    fn the_capabilities_a_call_hands_over_are_copied_into_one_frame() {
        let mut names = Interner::new();
        let mut func = passing(&mut names, |b, cap, _| {
            let args = b.func().push_values(&[cap]);
            b.inst(InstData { args, ..InstData::new(Opcode::CapPublish) }, &[]);
        });
        frames(&mut func, &mut names, Type::int(64));
        assert_eq!(count(&func, Opcode::CapPublish), 0);
        // Two reservations, which are the capability's own slot and the frame. The frame is the
        // function's rather than the call's, so a second call would not add a third.
        assert_eq!(count(&func, Opcode::Alloca), 2);
        // Four zero words for the null capability, then the count and the spare half word beside
        // it, then four more emptying the slot the callee writes its returned pointer's capability
        // into. The capability itself goes over as a copy rather than as four more stores, because
        // what is being moved is a slot the pass does not otherwise look inside. The last four are
        // what makes a callee that writes nothing believed: the frame is one reservation the
        // function reuses at every call site, so without them the slot holds the last call's answer.
        assert_eq!(count(&func, Opcode::Store), 10);
        assert_eq!(count(&func, Opcode::Memcpy), 1);
        assert!(!any_capability(&func));

        // The publish in front of the call and the outer frame put back after it, which is the
        // half that has to happen whether or not the callee read anything.
        let unit = module(&mut names);
        let text = print_func(&unit, &func, &names);
        let publish = text.find("__rucc_frame_publish").expect("the frame is published");
        let callee = text.find("call @g(").expect("the call is still there");
        let restore = text.find("__rucc_frame_restore").expect("the outer frame is put back");
        assert!(publish < callee, "{text}");
        assert!(callee < restore, "{text}");
        believed(&unit, &func, &names);
    }

    #[test]
    fn the_frame_is_put_back_from_the_link_the_publish_wrote() {
        let mut names = Interner::new();
        let mut func = passing(&mut names, |b, cap, _| {
            let args = b.func().push_values(&[cap]);
            b.inst(InstData { args, ..InstData::new(Opcode::CapPublish) }, &[]);
        });
        frames(&mut func, &mut names, Type::int(64));
        // One read, and it is of the frame rather than of anything the program wrote. It has to be
        // after the call, since what it is reading is the link the runtime filled in, and the call
        // it feeds has to be after that again.
        let reads: Vec<Inst> =
            walk(&func).into_iter().filter(|&inst| func[inst].opcode == Opcode::Load).collect();
        assert_eq!(reads.len(), 1);
        let calls: Vec<Inst> =
            walk(&func).into_iter().filter(|&inst| func[inst].opcode == Opcode::Call).collect();
        assert_eq!(calls.len(), 3);
        assert!(place(&func, calls[1]) < place(&func, reads[0]));
        assert!(place(&func, reads[0]) < place(&func, calls[2]));
        let read = func[reads[0]].results().next().expect("a read gives one value back");
        assert_eq!(func[func[calls[2]].args].to_vec(), vec![read]);
        believed(&module(&mut names), &func, &names);
    }

    #[test]
    fn a_call_nobody_can_vouch_for_is_told_there_is_no_frame() {
        let mut names = Interner::new();
        let mut func = passing(&mut names, |b, _, _| {
            b.inst(InstData::new(Opcode::CapClear), &[]);
        });
        frames(&mut func, &mut names, Type::int(64));
        assert_eq!(count(&func, Opcode::CapClear), 0);
        // Nothing is reserved, because saying there is no frame is not a statement about any
        // capability and the null the fixture starts from is unread and went in `prune`.
        assert_eq!(count(&func, Opcode::Alloca), 0);
        assert_eq!(count(&func, Opcode::CapNull), 0);
        let unit = module(&mut names);
        let text = print_func(&unit, &func, &names);
        assert!(text.contains("__rucc_frame_clear"), "{text}");
        assert!(!text.contains("__rucc_frame_publish"), "{text}");
        believed(&unit, &func, &names);
    }

    #[test]
    fn a_publish_that_is_in_front_of_nothing_leaves_the_function_alone() {
        let mut names = Interner::new();
        let mut func = built(&mut names, |b, cap, _| {
            let args = b.func().push_values(&[cap]);
            b.inst(InstData { args, ..InstData::new(Opcode::CapPublish) }, &[]);
        });
        frames(&mut func, &mut names, Type::int(64));
        // There is no call for it to be about, and adjacency is the whole of the tie between the
        // two, so there is nothing to hand the capability to and the conservative answer is the
        // same one a producer this pass cannot write gets.
        assert_eq!(count(&func, Opcode::CapPublish), 1);
        assert_eq!(count(&func, Opcode::CapNull), 1);
        assert_eq!(count(&func, Opcode::Alloca), 0);
        believed(&module(&mut names), &func, &names);
    }

    #[test]
    fn a_publish_describing_more_arguments_than_the_frame_holds_leaves_the_function_alone() {
        let mut names = Interner::new();
        let mut func = passing(&mut names, |b, cap, _| {
            let caps = vec![cap; crate::frame::ARGS + 1];
            let args = b.func().push_values(&caps);
            b.inst(InstData { args, ..InstData::new(Opcode::CapPublish) }, &[]);
        });
        frames(&mut func, &mut names, Type::int(64));
        // A refusal rather than a truncation. Dropping the capabilities past the eighth would be a
        // silent weakening, and this way the back end says it cannot lower the instruction.
        assert_eq!(count(&func, Opcode::CapPublish), 1);
        assert_eq!(count(&func, Opcode::Alloca), 0);
        believed(&module(&mut names), &func, &names);
    }

    /// A function that asks for the capabilities of `count` of its own pointer arguments.
    ///
    /// One parameter, asked about at as many positions as the test wants, because what is being
    /// tested is how many takes there are rather than how many parameters. Each answer is stored so
    /// that something reads it, since a capability nobody reads goes in `prune` before any of this.
    fn asking(names: &mut Interner, word: Type, count: i128) -> Func {
        built(names, |b, _, at| {
            for position in 0..count {
                let position = b.iconst(word, position);
                let args = b.func().push_values(&[at, position]);
                let mine = b.value(InstData { args, ..InstData::new(Opcode::CapArg) }, Type::CAP);
                let args = b.func().push_values(&[mine, at, at, mine]);
                b.inst(InstData { args, ..InstData::new(Opcode::CapStore) }, &[]);
            }
        })
    }

    #[test]
    fn a_pointer_parameter_gets_the_capability_its_caller_wrote_down() {
        let mut names = Interner::new();
        let word = Type::int(64);
        let mut func = asking(&mut names, word, 1);
        frames(&mut func, &mut names, word);
        assert_eq!(count(&func, Opcode::CapArg), 0);
        // One reservation, which is the answer's. The frame is not one of these, because it is the
        // caller's stack and this end is handed a pointer to it rather than making one.
        assert_eq!(count(&func, Opcode::Alloca), 1);
        assert!(!any_capability(&func));

        // The take first and the question after it, which is the order that matters: taking
        // consumes the frame, so anything that ran in between could only have consumed it first.
        let unit = module(&mut names);
        let text = print_func(&unit, &func, &names);
        let take = text.find("__rucc_frame_take").expect("the frame is taken");
        let ask = text.find("__rucc_frame_arg").expect("the argument is asked about");
        assert!(take < ask, "{text}");
        believed(&unit, &func, &names);
    }

    #[test]
    fn the_frame_is_taken_once_however_many_arguments_are_asked_about() {
        let mut names = Interner::new();
        let word = Type::int(64);
        let mut func = asking(&mut names, word, 3);
        frames(&mut func, &mut names, word);
        let unit = module(&mut names);
        let text = print_func(&unit, &func, &names);
        // Once, and this is the assertion the whole shape of the lowering is for. A second take
        // finds the magic word already cleared and answers null, so a function that took twice
        // would recover the arguments it had just been handed.
        assert_eq!(text.matches("__rucc_frame_take").count(), 1, "{text}");
        assert_eq!(text.matches("__rucc_frame_arg").count(), 3, "{text}");
        assert_eq!(count(&func, Opcode::Alloca), 3);
        believed(&unit, &func, &names);
    }

    #[test]
    fn a_position_narrower_than_a_word_is_widened_into_one() {
        let mut names = Interner::new();
        let mut func = asking(&mut names, Type::int(32), 1);
        frames(&mut func, &mut names, Type::int(64));
        // The runtime takes the position as a `size_t` and the front end is under no obligation to
        // have produced one, which is the same thing `cap_narrow`'s offset and length need.
        assert_eq!(count(&func, Opcode::ZExt), 1);
        believed(&module(&mut names), &func, &names);
    }

    #[test]
    fn the_capability_of_a_returned_pointer_is_left_in_the_frame_the_caller_waits_in() {
        let mut names = Interner::new();
        let mut func = built(&mut names, |b, cap, _| {
            let args = b.func().push_values(&[cap]);
            b.inst(InstData { args, ..InstData::new(Opcode::CapYield) }, &[]);
        });
        frames(&mut func, &mut names, Type::int(64));
        assert_eq!(count(&func, Opcode::CapYield), 0);
        // One reservation, which is the capability being handed back. The frame is not one of
        // these, for the reason the argument end's is not: it belongs to whoever called this.
        assert_eq!(count(&func, Opcode::Alloca), 1);
        assert!(!any_capability(&func));

        // Taken before it is written into, which is the same order the arguments need and for the
        // same reason, since the take is what says where the frame is.
        let unit = module(&mut names);
        let text = print_func(&unit, &func, &names);
        let take = text.find("__rucc_frame_take").expect("the frame is taken");
        let left = text.find("__rucc_frame_yield").expect("the capability is left behind");
        assert!(take < left, "{text}");
        believed(&unit, &func, &names);
    }

    #[test]
    fn a_yield_that_is_not_in_front_of_a_return_leaves_the_function_alone() {
        let mut names = Interner::new();
        let mut func = built(&mut names, |b, cap, at| {
            let args = b.func().push_values(&[cap]);
            b.inst(InstData { args, ..InstData::new(Opcode::CapYield) }, &[]);
            let args = b.func().push_values(&[cap, at, at, cap]);
            b.inst(InstData { args, ..InstData::new(Opcode::CapStore) }, &[]);
        });
        frames(&mut func, &mut names, Type::int(64));
        // What it says is about the value leaving by one particular return, so one that is not in
        // front of a return is describing nothing, and the refusal is the publish's refusal.
        assert_eq!(count(&func, Opcode::CapYield), 1);
        assert_eq!(count(&func, Opcode::Alloca), 0);
        believed(&module(&mut names), &func, &names);
    }

    /// A function that calls `g(at)` for a pointer and asks what the pointer it got back is.
    ///
    /// The shape the reading end is defined over, which is a call with something in front of it and
    /// the `cap_result` behind it. The flag picks which of the two things can be in front, and the
    /// version that clears is a call with no frame for the callee to have written into.
    fn returning(names: &mut Interner, vouched: bool) -> Func {
        let mut func = Func::new(names.intern("f"), Signature::new().with_params(&[Type::PTR]));
        let entry = func.create_block();
        let at = func.append_param(entry, Type::PTR);
        let sig = Signature::new().with_params(&[Type::PTR]).with_returns(&[Type::PTR]);
        let sig = func.add_signature(sig);
        let callee = names.intern("g");
        let varargs = func.push_abis(&[]);
        let info = func.add_call(CallInfo { callee: Some(callee), signature: sig, varargs });
        let mut b = Builder::new(&mut func, entry);
        if vouched {
            let cap = b.value(InstData::new(Opcode::CapNull), Type::CAP);
            let args = b.func().push_values(&[cap]);
            b.inst(InstData { args, ..InstData::new(Opcode::CapPublish) }, &[]);
        } else {
            b.inst(InstData::new(Opcode::CapClear), &[]);
        }
        let args = b.func().push_values(&[at]);
        let data = InstData { args, extra: Extra::Call(info), ..InstData::new(Opcode::Call) };
        let base = b.value(data, Type::PTR);
        let args = b.func().push_values(&[base]);
        let mine = b.value(InstData { args, ..InstData::new(Opcode::CapResult) }, Type::CAP);
        let args = b.func().push_values(&[mine, base, base, mine]);
        b.inst(InstData { args, ..InstData::new(Opcode::CapStore) }, &[]);
        b.ret(&[]);
        func
    }

    #[test]
    fn the_pointer_a_call_gave_back_is_read_out_of_the_frame_it_was_published_with() {
        let mut names = Interner::new();
        let mut func = returning(&mut names, true);
        frames(&mut func, &mut names, Type::int(64));
        assert_eq!(count(&func, Opcode::CapResult), 0);
        assert_eq!(count(&func, Opcode::CapPublish), 0);
        // Three reservations: the capability that went over, the frame it went over in, and the
        // slot the answer comes back into.
        assert_eq!(count(&func, Opcode::Alloca), 3);
        assert!(!any_capability(&func));

        let unit = module(&mut names);
        let text = print_func(&unit, &func, &names);
        let publish = text.find("__rucc_frame_publish").expect("the frame is published");
        let call = text.find("call @g(").expect("the call is still there");
        let back = text.find("__rucc_frame_returned").expect("the answer is read back");
        assert!(publish < call, "{text}");
        // After the call, because what is being read is what the callee wrote, and nothing here
        // takes a frame, because this end is the caller and the frame is its own.
        assert!(call < back, "{text}");
        assert!(!text.contains("__rucc_frame_take"), "{text}");
        believed(&unit, &func, &names);
    }

    #[test]
    fn a_result_behind_a_call_with_no_frame_leaves_the_function_alone() {
        let mut names = Interner::new();
        let mut func = returning(&mut names, false);
        frames(&mut func, &mut names, Type::int(64));
        // A call that says there is no frame has none for the callee to have written into, so
        // there is nothing here to read and the answer would be whatever the last call site left.
        assert_eq!(count(&func, Opcode::CapResult), 1);
        assert_eq!(count(&func, Opcode::CapClear), 1);
        assert_eq!(count(&func, Opcode::Alloca), 0);
        believed(&module(&mut names), &func, &names);
    }

    #[test]
    fn a_capability_this_pass_cannot_place_leaves_the_others_where_they_were() {
        let mut names = Interner::new();
        let mut func = built(&mut names, |b, cap, at| {
            let args = b.func().push_values(&[at]);
            let taken = b.value(InstData { args, ..InstData::new(Opcode::CapOf) }, Type::CAP);
            // A publish in front of nothing, which is the refusal every producer has stopped being.
            let args = b.func().push_values(&[taken]);
            b.inst(InstData { args, ..InstData::new(Opcode::CapPublish) }, &[]);
            let args = b.func().push_values(&[cap, at, at, cap]);
            b.inst(InstData { args, ..InstData::new(Opcode::CapStore) }, &[]);
        });
        frames(&mut func, &mut names, Type::int(64));
        // Both of them still capabilities, since placing the one this pass understands would hand
        // the store the address of a slot the other one never wrote.
        assert_eq!(count(&func, Opcode::CapOf), 1);
        assert_eq!(count(&func, Opcode::CapNull), 1);
        assert_eq!(count(&func, Opcode::Alloca), 0);
        believed(&module(&mut names), &func, &names);
    }

    /// A capability made in one block and read in another, handed along the edge between them.
    ///
    /// Written by hand rather than produced, because nothing in the insertion pass builds this and
    /// what does build it is the optimizer running in between, which is not something a unit test
    /// of this file should have to stand up.
    fn handed_along(names: &mut Interner, round: bool) -> Func {
        let mut func = Func::new(names.intern("f"), Signature::new().with_params(&[Type::PTR]));
        let entry = func.create_block();
        let next = func.create_block();
        let done = func.create_block();
        let at = func.append_param(entry, Type::PTR);
        let held = func.append_param(next, Type::CAP);

        let mut b = Builder::new(&mut func, entry);
        let args = b.func().push_values(&[at]);
        let cap = b.value(InstData { args, ..InstData::new(Opcode::CapOf) }, Type::CAP);
        b.jump(next, &[cap]);

        let mut b = Builder::new(&mut func, next);
        let args = b.func().push_values(&[held, at, at, held]);
        b.inst(InstData { args, ..InstData::new(Opcode::CapStore) }, &[]);
        if round {
            let again = b.iconst(Type::I1, 1);
            b.br_if(again, next, &[held], done, &[]);
        } else {
            b.jump(done, &[]);
        }

        Builder::new(&mut func, done).ret(&[]);
        func
    }

    #[test]
    fn a_capability_handed_along_an_edge_is_read_out_of_the_slot_it_was_made_in() {
        // One capability arrives, so the parameter stands for it and goes: what reads it reads the
        // slot the producer filled, and nothing is copied anywhere.
        let mut names = Interner::new();
        let mut func = handed_along(&mut names, false);
        frames(&mut func, &mut names, Type::int(64));
        assert_eq!(count(&func, Opcode::CapOf), 0);
        assert_eq!(count(&func, Opcode::CapStore), 0);
        assert_eq!(count(&func, Opcode::Alloca), 1, "one slot for the one capability");
        let next = func.blocks().nth(1).expect("the function has three blocks");
        assert!(func[next].params.is_empty(), "the parameter went");
        believed(&module(&mut names), &func, &names);
    }

    /// A loop whose header joins two capabilities, the one made in front of it and the one the body
    /// makes each time round, with the body reading the header's after it has made its own.
    ///
    /// `next = cur->next; free(cur); cur = next` is the program. The body's `cap_of` stands for the
    /// `cap_load` of `next` and the `cap_store` after it for the check in front of the free, so the
    /// store has to read what `cur` arrived with and not what the body just wrote. `exit` is whether
    /// the back edge is one arm of a branch that can also leave, which is the edge that has to be a
    /// block of its own.
    fn joining(names: &mut Interner, exit: bool) -> Func {
        let mut func = Func::new(names.intern("f"), Signature::new().with_params(&[Type::PTR]));
        let entry = func.create_block();
        let head = func.create_block();
        let at = func.append_param(entry, Type::PTR);
        let held = func.append_param(head, Type::CAP);

        let mut b = Builder::new(&mut func, entry);
        let args = b.func().push_values(&[at]);
        let first = b.value(InstData { args, ..InstData::new(Opcode::CapOf) }, Type::CAP);
        b.jump(head, &[first]);

        let mut b = Builder::new(&mut func, head);
        let args = b.func().push_values(&[at]);
        let next = b.value(InstData { args, ..InstData::new(Opcode::CapOf) }, Type::CAP);
        let args = b.func().push_values(&[held, at, at, held]);
        b.inst(InstData { args, ..InstData::new(Opcode::CapStore) }, &[]);
        if exit {
            let again = b.iconst(Type::I1, 1);
            let done = b.func().create_block();
            b.br_if(again, head, &[next], done, &[]);
            Builder::new(&mut func, done).ret(&[]);
        } else {
            b.jump(head, &[next]);
        }
        func
    }

    /// The slot each call in the function is handed as its first operand, in order.
    fn handed(func: &Func) -> Vec<Value> {
        walk(func)
            .into_iter()
            .filter(|&inst| func[inst].opcode == Opcode::Call)
            .filter_map(|inst| func[func[inst].args].first().copied())
            .collect()
    }

    #[test]
    fn a_join_of_two_capabilities_gets_a_slot_of_its_own_that_each_edge_copies_into() {
        // Three slots: the one in front of the loop, the one the body makes, and the header's. The
        // store reads the header's, which is what makes the body's `cap_of` running first harmless.
        let mut names = Interner::new();
        let mut func = joining(&mut names, false);
        frames(&mut func, &mut names, Type::int(64));
        assert_eq!(count(&func, Opcode::Alloca), 3);
        let head = func.blocks().nth(1).expect("the function has two blocks");
        assert!(func[head].params.is_empty(), "the parameter went");
        let calls = handed(&func);
        let (made, stored) = (calls[1], calls[2]);
        assert_ne!(made, stored, "the store reads the join's slot and not the body's");
        // Four words each way on each of the two edges in.
        assert_eq!(count(&func, Opcode::Load), 8);
        assert_eq!(count(&func, Opcode::Store), 8);
        believed(&module(&mut names), &func, &names);
    }

    #[test]
    fn a_join_nothing_reads_goes_and_takes_what_fed_it_along() {
        // Only the edges read the parameter, and the one round the loop reads it only to hand it
        // back to itself, so neither producer has a reader and the function holds no capability.
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new().with_params(&[Type::PTR]));
        let entry = func.create_block();
        let head = func.create_block();
        let at = func.append_param(entry, Type::PTR);
        let held = func.append_param(head, Type::CAP);
        let mut b = Builder::new(&mut func, entry);
        let args = b.func().push_values(&[at]);
        let first = b.value(InstData { args, ..InstData::new(Opcode::CapOf) }, Type::CAP);
        b.jump(head, &[first]);
        let mut b = Builder::new(&mut func, head);
        let again = b.iconst(Type::I1, 1);
        let done = b.func().create_block();
        b.br_if(again, head, &[held], done, &[]);
        Builder::new(&mut func, done).ret(&[]);
        frames(&mut func, &mut names, Type::int(64));
        assert_eq!(count(&func, Opcode::CapOf), 0);
        assert_eq!(count(&func, Opcode::Call), 0);
        assert!(func[head].params.is_empty(), "the parameter went");
        believed(&module(&mut names), &func, &names);
    }

    #[test]
    fn a_join_reached_from_a_branch_that_can_also_leave_copies_on_a_block_of_its_own() {
        // The back edge is one arm of a branch, so the copy goes on a block put on that arm rather
        // than in front of the branch, where it would run on the way out as well.
        let mut names = Interner::new();
        let mut func = joining(&mut names, true);
        frames(&mut func, &mut names, Type::int(64));
        assert_eq!(func.blocks().count(), 4, "one more block, for the back edge");
        let edge = func.blocks().last().expect("the edge block is the last one made");
        assert_eq!(func.insts(edge).filter(|&inst| func[inst].opcode == Opcode::Store).count(), 4);
        believed(&module(&mut names), &func, &names);
    }

    #[test]
    fn a_capability_carried_round_a_loop_is_placed_the_same_way() {
        // The edge back into the header passes the parameter itself, so the one slot the producer
        // filled in front of the loop is what every iteration reads. That is the whole of what the
        // loop adds, and it is why there is nothing to copy round.
        let mut names = Interner::new();
        let mut func = handed_along(&mut names, true);
        frames(&mut func, &mut names, Type::int(64));
        assert_eq!(count(&func, Opcode::CapOf), 0);
        assert_eq!(count(&func, Opcode::CapStore), 0);
        assert_eq!(count(&func, Opcode::Alloca), 1, "one slot for the one capability");
        believed(&module(&mut names), &func, &names);
    }
}
