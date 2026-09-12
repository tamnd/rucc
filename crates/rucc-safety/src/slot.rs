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
//! Until the rest exist a function can still hold a capability this pass cannot place, and the
//! answer then is to leave every capability in the function alone. Placing some and not others means
//! handing a `cap_store` the address of a slot that nothing ever wrote, which is worse than not
//! lowering it: the back end refuses an opcode it has no rule for and says so, and a slot full of
//! whatever the frame held is a capability that permits whatever it happens to say.
//!
//! # Why the dead ones go first
//!
//! Because most of them are dead. Every `cap_of` in a function was put there to feed a check, and
//! by the time this runs every check is a call that does not read one, so the walk that used to
//! remove them by opcode removes them by nobody reading them instead. Running it to a fixpoint is
//! what handles a chain, since a `cap_narrow` of a `cap_of` leaves the `cap_of` unread only once
//! the `cap_narrow` has gone.

use std::collections::{HashMap, HashSet};

use rucc_base::Interner;
use rucc_ir::{
    Block, Def, Extra, Flags, Func, Imm, Inst, InstData, MemInfo, MemOrder, Opcode, Restrict, Type,
    Value,
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
pub fn frames(func: &mut Func, names: &mut Interner, word: Type) {
    prune(func);
    if !placeable(func) {
        return;
    }
    let mut moved: HashMap<Value, Value> = HashMap::new();
    for inst in walk(func) {
        match func[inst].opcode {
            Opcode::CapNull => nulled(func, word, inst, &mut moved),
            Opcode::CapOf => allocated(func, names, inst, &mut moved),
            _ => {}
        }
    }
    if moved.is_empty() {
        return;
    }
    substitute(func, &moved);
    for inst in walk(func) {
        if func[inst].opcode == Opcode::CapStore {
            stored(func, names, inst);
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
fn prune(func: &mut Func) {
    loop {
        let mut read: HashSet<Value> = HashSet::new();
        for inst in walk(func) {
            operands(func, inst, |value| {
                read.insert(value);
            });
        }
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
        if !again {
            return;
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
        let known = opcode == Opcode::CapNull || fresh(func, inst).is_some();
        if opcode.makes_capability() && !known {
            return false;
        }
        let reads = func[func[inst].args].iter().any(|&value| func[value].ty.is_cap());
        if reads && opcode != Opcode::CapStore {
            return false;
        }
        // A capability passed along an edge is one whose reader is a block parameter, and a block
        // parameter is not a value this pass gives a slot to. Nothing builds one today, since the
        // verifier keeps a `cap` out of every signature and the front end has no way to name one,
        // but a pass that started to would otherwise find its capability quietly replaced by an
        // address of the wrong type.
        for call in func.successors(inst) {
            if func[call.args].iter().any(|&value| func[value].ty.is_cap()) {
                return false;
            }
        }
    }
    true
}

/// Every value an instruction reads, counting the arguments it passes to the blocks it branches to.
fn operands(func: &Func, inst: Inst, mut each: impl FnMut(Value)) {
    for &value in &func[func[inst].args] {
        each(value);
    }
    for call in func.successors(inst) {
        for &value in &func[call.args] {
            each(value);
        }
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
fn nulled(func: &mut Func, word: Type, inst: Inst, moved: &mut HashMap<Value, Value>) {
    let Some(result) = func[inst].results().next() else { return };
    let Some(address) = reserve(func, inst) else { return };
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
    moved.insert(result, address);
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
/// What happens for a pointer the flag is not on is a capability this pass cannot place, which
/// leaves the whole function's capabilities where they were. That is the conservative direction and
/// it costs nothing today, since every `cap_of` that is not read is gone by the time this runs.
fn fresh(func: &Func, inst: Inst) -> Option<Value> {
    if func[inst].opcode != Opcode::CapOf {
        return None;
    }
    let &[base] = &func[func[inst].args] else { return None };
    let Def::Result { inst: call, index: 0 } = func[base].def else { return None };
    (func[call].opcode == Opcode::Call && func[call].flags.contains(Flags::HEAP)).then_some(base)
}

/// `cap_of` over a fresh allocation becomes `__rucc_cap_made(slot, base)`.
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
fn allocated(func: &mut Func, names: &mut Interner, inst: Inst, moved: &mut HashMap<Value, Value>) {
    let Some(base) = fresh(func, inst) else { return };
    let Some(result) = func[inst].results().next() else { return };
    let Some(address) = reserve(func, inst) else { return };
    let params = &[Type::PTR; 2];
    let args = &[address, base];
    let data = crate::lower::calling(func, names, "__rucc_cap_made", params, &[], args);
    let made = func.create_inst(data, &[], func.span(inst));
    func.insert_before(made, inst);
    moved.insert(result, address);
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
fn offset(func: &mut Func, inst: Inst, address: Value, bytes: u64, word: Type) -> Value {
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
fn konst(func: &mut Func, inst: Inst, imm: Imm, ty: Type) -> Value {
    let extra = Extra::Imm(func.add_imm(imm));
    let data = InstData { extra, ..InstData::new(Opcode::IConst) };
    let made = func.create_inst(data, &[ty], func.span(inst));
    func.insert_before(made, inst);
    func[made].results().next().expect("a constant created with one result has one")
}

#[cfg(test)]
mod tests {
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
    fn a_capability_for_a_pointer_nobody_vouched_for_is_left_where_it_was() {
        let mut names = Interner::new();
        let mut func = called(&mut names, false);
        frames(&mut func, &mut names, Type::int(64));
        assert_eq!(count(&func, Opcode::CapOf), 1);
        assert_eq!(count(&func, Opcode::CapStore), 1);
        assert_eq!(count(&func, Opcode::Alloca), 0);
        believed(&module(&mut names), &func, &names);
    }

    #[test]
    fn a_capability_this_pass_cannot_place_leaves_the_others_where_they_were() {
        let mut names = Interner::new();
        let mut func = built(&mut names, |b, cap, at| {
            let args = b.func().push_values(&[at]);
            let taken = b.value(InstData { args, ..InstData::new(Opcode::CapOf) }, Type::CAP);
            let args = b.func().push_values(&[taken, at, at, cap]);
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
}
