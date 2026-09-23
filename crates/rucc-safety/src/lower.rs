//! Turning the checks that survived the optimizer into something the back end can generate.
//!
//! Design: `spec/safe-memory/06-instrumentation.md` sections 6.3.1 and 6.5.
//!
//! [`crate::insert`] puts checks in before the optimizer runs, which is the whole argument of this
//! crate. This is the other end of that: after the optimizer has discharged what it could prove,
//! every check still standing becomes a call to `rucc-safe-rt`, and each call carries the address of
//! a descriptor this pass writes into the object.
//!
//! # Why the checks are calls and not compares
//!
//! Section 6.3.1 wants a compare and a branch in the checked function with only the trap out of
//! line, and that is not what this emits. The reason is in `rucc-safe-rt`'s `check` module and is
//! the same one: the inline form needs the four word capability of document 05 section 5.2.1 live
//! in registers at the check, and it needs the aux plane to recover one for a pointer that came out
//! of memory. The capability representation is milestone S2 and the aux plane is S5. Handing the
//! runtime an address is what can be written today.
//!
//! It is slow, and S1's exit criterion asks for the overhead to be measured rather than for it to
//! be small. S4 is the milestone that makes it small, and it needs a number to improve on.
//!
//! # Why it runs after the optimizer
//!
//! Because a descriptor for a check that was deleted is data nothing will ever name. Running here
//! means there is one for exactly the checks a program will actually run, which is also what makes
//! the count a number worth reporting. The cost is that `--emit=ir` shows `check_bounds` rather than
//! the call, which is the right way round: the IR a person reads should say what the compiler
//! decided, not how it spelled it.
//!
//! # Why a check is handed an address and not a number
//!
//! Each descriptor is its own sixteen byte variable, internal and constant, and they all go in a
//! section called `.rucc_safety_desc`, so the section is still the contiguous table
//! `rucc_safe_rt::fail::Descriptor` describes and its length still divided by sixteen is still the
//! number of checks in the object.
//!
//! What a check passes is the descriptor's address rather than its index, and that is the whole
//! reason the descriptors are separate variables. An index is an index into *this object's* rows:
//! link two instrumented objects together and the sections concatenate while both sets of indices
//! still start at zero, so a runtime that read the section by index would report the wrong check.
//! The other way out is for every object to contribute a base the runtime adds, which is a table of
//! tables and a startup constructor to build it. An address needs neither. It is a relocation the
//! linker already knows how to do, it costs the same one instruction the index cost, and the
//! reporter reads it by dereferencing it.

use std::collections::{HashMap, HashSet};

use rucc_base::Interner;
use rucc_ir::{
    CallInfo, Datum, Extra, Flags, Func, Global, Imm, Inst, InstData, Linkage, Meta, Module,
    Opcode, Signature, Type, Value,
};

use crate::plane;

/// How wide one descriptor is, which `rucc_safe_rt::fail::Descriptor` fixes.
pub const WIDTH: u64 = 16;

/// The section the descriptors go in, which is how a reader finds all of them at once.
pub const SECTION: &str = ".rucc_safety_desc";

/// What each descriptor's name starts with, before the number that makes it unique.
///
/// Nothing outside the object ever resolves one, since every reference to one is inside the object
/// that defines it. The name exists because a relocation needs a symbol to be against.
const DESCRIPTOR: &str = "__rucc_safety_desc";

/// Judgement J1 of document 04 section 4.4, which is what an access check decides.
const ACCESS: u8 = 1;

/// Judgement J2, which is what a derivation check decides.
const DERIVE: u8 = 2;

/// Judgement J6, which is what the check in front of a free decides.
///
/// Numbered apart from J1 because what it is about is the free rather than an access, which is the
/// distinction document 04 section 4.4 draws between the two and which the reporter's wording for
/// each of them already reads as.
const FREE: u8 = 6;

/// Judgement J8, which is what a `restrict` check decides.
const RESTRICT: u8 = 8;

/// Judgement J9, which is what a race check decides.
///
/// Numbered apart from J1 the way J8 is, and document 04 section 4.5 gives the reason: it is a
/// relation between two operations rather than a property of one, so a report that said an access
/// was not permitted would be describing the wrong thing.
const RACE: u8 = 9;

/// One descriptor, as much of it as this pass knows.
///
/// The `pc` field of the runtime's descriptor is not here. Filling it means a relocation against
/// the enclosing function plus the offset of the call, which the IR cannot express and which
/// nothing needs while the reporter has the address the check was given, so those eight bytes are
/// written as zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Descriptor {
    /// Which judgement the check decides.
    pub judgement: u8,
    /// Which row of document 03's tables the failure is, which nothing decides yet.
    pub class: u8,
    /// How many bytes the access covers, saturating, and zero where the check is not about an
    /// access of a known width.
    pub size: u16,
}

/// Turns every check in a module into a call, and gives the module the descriptors they name.
///
/// The number of descriptors, which is the number of checks that survived the optimizer. That is
/// the numerator of everything document 13 measures and it is not recoverable afterwards, since by
/// this point a discharged check is simply not there.
///
/// Whether this runs at all is `-fsafety=`, and the driver decides it, for the reason
/// [`crate::run`] gives.
pub fn lower(module: &mut Module, names: &mut Interner) -> usize {
    // `size_t`, taken from the module rather than written as sixty four, so that the argument is
    // the one the runtime's own declaration of the entry point has.
    let word = Type::int(module.datalayout.pointer_bits);
    // The descriptors are collected rather than added as they are found, because a function is
    // borrowed out of the module while its checks are being rewritten. What the rewrite needs is
    // the name of the descriptor it is about, and a name is the position in this list, so both ends
    // agree without either holding the module.
    let mut written: Vec<Descriptor> = Vec::new();
    // The same reason the descriptors are collected: a plane write names a node of the module's
    // metadata table and the module is not reachable while one of its functions is borrowed out of
    // it. The table is a handful of nodes, so it is read once here rather than per instruction.
    let numbers = plane::numbers(module, names);
    for id in module.funcs() {
        if module[id].is_declaration() {
            continue;
        }
        calls(&mut module[id], names, word, &numbers, &mut written);
    }
    for (index, row) in written.iter().enumerate() {
        emit(module, names, index, *row);
    }
    written.len()
}

/// Rewrites every check in one function, and takes the capabilities out afterwards.
fn calls(
    func: &mut Func,
    names: &mut Interner,
    word: Type,
    numbers: &HashMap<Meta, u32>,
    table: &mut Vec<Descriptor>,
) {
    let insts: Vec<Inst> =
        func.blocks().flat_map(|block| func.insts(block).collect::<Vec<_>>()).collect();
    let pairs = pairs(func, &insts);
    let fused: HashSet<Inst> = pairs.values().copied().collect();
    for &inst in &insts {
        // The init check of a pair is lowered by its type check, which asks both questions in one
        // call, so there is nothing left here for it to be.
        if fused.contains(&inst) {
            continue;
        }
        match func[inst].opcode {
            Opcode::CheckBounds => bounds(func, names, word, table, inst),
            Opcode::CheckLive => live(func, names, table, inst),
            Opcode::CheckFree => freed(func, names, table, inst),
            Opcode::CheckDeriv => deriv(func, names, word, table, inst),
            Opcode::CheckType => {
                typed(func, names, word, numbers, table, inst, pairs.get(&inst).copied());
            }
            Opcode::CheckInit => began(func, names, word, table, inst),
            Opcode::CheckRace => raced(func, names, word, table, inst),
            Opcode::CheckRestrictRead => promised(func, names, word, table, inst, false),
            Opcode::CheckRestrictWrite => promised(func, names, word, table, inst, true),
            Opcode::RestrictEnter => opened(func, names, inst),
            Opcode::RestrictLeave => closed(func, names, inst),
            Opcode::MetaType => judgement(func, names, word, numbers, inst),
            Opcode::MetaTypeCopy => carriage(func, names, word, inst),
            Opcode::MetaInit => written(func, names, word, inst),
            Opcode::MetaInitCopy => carried(func, names, word, inst),
            Opcode::CapCopy => relocated(func, names, word, inst),
            Opcode::MetaEpoch => stamped(func, names, word, inst),
            Opcode::MetaRelease => published(func, names, inst),
            Opcode::MetaAcquire => taken(func, names, inst),
            Opcode::MetaFenceRelease => published_everywhere(func, names, inst),
            Opcode::MetaFenceAcquire => taken_everywhere(func, names, inst),
            Opcode::CapExtent => extent(func, names, word, inst, "__rucc_extent"),
            Opcode::CapExtentBack => extent(func, names, word, inst, "__rucc_extent_back"),
            Opcode::SafeRegionBegin | Opcode::SafeRegionEnd => declared(func, inst),
            _ => {}
        }
    }
    // Every `cap_of` in the function was put there to feed a check, and no check reads one any
    // more, so almost all of what this does is take them out again. The rest of it is putting the
    // capabilities that are left somewhere the back end can keep them, which is [`crate::slot`].
    crate::slot::frames(func, names, word);
}

/// `check_bounds` becomes `__rucc_check_bounds(pointer, size, align, capability, descriptor)`.
///
/// The size is the payload's for the check the front end wrote and the third operand's for the
/// hoisted check of section 7.4, which is about a range the program worked out rather than about
/// one access. The descriptor says zero bytes for that one, which is the field's own reading of a
/// check that is not about an access of a known width, because the width is not known here either.
///
/// The alignment is the payload's too, and it is the whole of judgement J1's `addr mod align = 0`
/// conjunct: the runtime tests it and nothing else here does. A hoisted check passes one, which
/// says the range it is about assumes nothing, because the range is a span of bytes a loop will
/// walk rather than one access and the accesses inside it carry their own.
///
/// The capability is the first operand, which this used to throw away the way [`live`] used to, and
/// handing it over is box 6 of tamnd/rucc#1241. It lets the runtime permit the commonest access in a
/// program with a subtraction and a compare over two words the caller has already got, where it used
/// to want a region lookup and two plane loads, and it costs nothing to produce, because the
/// lifetime check beside this one is loading the same capability for the version anyway. That is the
/// sense in which the issue says this is where the version compare pays for itself. The runtime only
/// ever permits on it and never refuses on it, and `rucc_safe_rt::check::bounds` is where that
/// restraint is argued.
///
/// Fourth rather than first, which is the other order from the lifetime check. The three in front of
/// it are the ones the access is about and they were here first, so leaving them alone keeps them in
/// the registers the caller would have used, and the descriptor stays last the way every check here
/// has it.
fn bounds(
    func: &mut Func,
    names: &mut Interner,
    word: Type,
    table: &mut Vec<Descriptor>,
    inst: Inst,
) {
    let args = &func[func[inst].args];
    let (Some(&capability), Some(&pointer), computed) =
        (args.first(), args.get(1), args.get(2).copied())
    else {
        return;
    };
    let Extra::Mem(mem) = func[inst].extra else { return };
    let size = func[mem].size;

    let row = Descriptor {
        judgement: ACCESS,
        class: 0,
        // Saturating, so that a report about a structure copy larger than a descriptor can hold
        // says sixty five thousand rather than whatever the low sixteen bits happened to be.
        size: if computed.is_some() { 0 } else { u16::try_from(size).unwrap_or(u16::MAX) },
    };
    let desc = record(func, names, table, inst, row);
    let bytes = match computed {
        Some(value) => fitted(func, inst, value, word),
        None => konst(func, inst, Imm::int(i128::from(size), word), word),
    };
    let claim = if computed.is_some() { 1 } else { i128::from(func[mem].align) };
    let align = konst(func, inst, Imm::int(claim, word), word);
    let params = &[Type::PTR, word, word, Type::PTR, Type::PTR];
    let args = &[pointer, bytes, align, capability, desc];
    call(func, names, inst, "__rucc_check_bounds", params, &[], args);
}

/// The same number in the width the runtime's own declaration asks for.
///
/// A pass that works out how many bytes a loop covers has no target to ask, so it writes the count
/// in the width its arithmetic was in, and on a target whose `size_t` is narrower or wider than that
/// the call would be handed the wrong type. Zero extension rather than sign, because the number is a
/// count of bytes and a negative one is not a thing the caller can have meant.
///
/// `crate::slot` uses it for the same reason about a different pair of numbers: an offset and a
/// length that narrow a capability are written in whatever width the front end's arithmetic was in,
/// and the runtime declares both as `size_t`.
pub(crate) fn fitted(func: &mut Func, inst: Inst, value: Value, word: Type) -> Value {
    let ty = func[value].ty;
    if ty == word {
        return value;
    }
    let opcode = if ty.bits() > word.bits() { Opcode::Trunc } else { Opcode::ZExt };
    let span = func.span(inst);
    let args = func.push_values(&[value]);
    let made = func.create_inst(InstData { args, ..InstData::new(opcode) }, &[word], span);
    func.insert_before(made, inst);
    func[made].results().next().expect("a cast created with one result has one")
}

/// Whether an instruction that reads a capability is still reading one after this pass has run.
///
/// Three checks of the six now: [`live`], [`freed`] and [`bounds`]. The other three still become a
/// call that takes an address, so a capability whose only reader is one of them is dead the moment
/// the rewrite happens and [`crate::slot`]'s prune takes it out. That is a statement about how far
/// tamnd/rucc#1241 has got rather than about the design, and what is left of it is the type check,
/// the initialization check and the race check, none of which asks anything a capability answers.
///
/// It is a predicate somebody outside can ask rather than something left implied by the arm it
/// belongs to, and [`crate::origin::existing`] is the somebody, on behalf of [`crate::handover`].
/// What that pass hands to a callee has to be a capability the caller is paying for anyway, and a
/// producer about to be pruned is not one: handing it over is a reader, a reader keeps it alive, and
/// what it keeps alive is a walk of the lifetime plane nobody was doing. On the SQLite amalgamation
/// that is nine hundred walks and eight per cent of the text, bought for nothing.
///
/// The three that are not checks are here because they read a capability, come through this pass
/// untouched, and are not producers themselves, so nothing else decides whether they count. A
/// producer that reads one is a `cap_narrow`, and whether that keeps its operand alive depends on
/// whether anything keeps the narrow alive, which is a fixpoint rather than an answer this can give.
///
/// A whitelist and not a blacklist, because the two ways of being wrong are not the same size.
/// Leaving something out means a capability that could have travelled does not, which is a handover
/// missed. Putting something in wrongly means a dead producer resurrected, which is the regression
/// this exists to prevent.
pub(crate) fn keeps(opcode: Opcode) -> bool {
    matches!(
        opcode,
        Opcode::CheckBounds
            | Opcode::CheckLive
            | Opcode::CheckFree
            | Opcode::CapStore
            | Opcode::CapYield
            | Opcode::CapPublish
    )
}

/// `check_live` becomes `__rucc_check_live(capability, pointer, descriptor)`.
///
/// The first operand is the one this used to throw away, and handing it over is the whole of what
/// turns the lock and key rule of `spec/safe-memory/08-temporal-safety.md` section 8.3 on in
/// generated code. Without it the runtime asks whether anybody owns the address, which misses every
/// access through a stale pointer to a block the allocator has since handed out again. With it the
/// runtime compares the version the capability was taken at against the version the plane holds,
/// and that is the access a busy program's use after free is.
///
/// It is a `*const Cap` by the time the back end sees it, because [`crate::slot`] gives a capability
/// four words of frame and passes the address of them, which is what the runtime's own declaration
/// of every entry point that takes one asks for.
fn live(func: &mut Func, names: &mut Interner, table: &mut Vec<Descriptor>, inst: Inst) {
    let [capability, pointer] = func[func[inst].args] else { return };
    // No size. The check carries no payload, because whether anybody owns an address is a question
    // about the address rather than about how many bytes are read through it.
    let row = Descriptor { judgement: ACCESS, class: 0, size: 0 };
    let desc = record(func, names, table, inst, row);
    let params = &[Type::PTR; 3];
    call(func, names, inst, "__rucc_check_live", params, &[], &[capability, pointer, desc]);
}

/// `check_free` becomes `__rucc_check_free(capability, pointer, descriptor)`.
///
/// The same three arguments [`live`] passes and in the same order, because the two checks ask the
/// version question in the same words. What separates them is the descriptor: this one says J6, so a
/// refusal reads as a free of something that was not allocated, or not by that allocator, rather
/// than as an access the planes did not permit. That is the sentence the person whose program
/// stopped wants, since the line it stopped at is a `free` and nothing was being read.
///
/// Two entry points rather than one with a judgement argument, for the reason the descriptor exists
/// at all: which judgement a check decides is constant data the object file already carries, and
/// passing it would be putting a number in a register on every free to say something that never
/// changes. It also keeps the runtime's two answers apart, and they are not the same answer. The
/// lifetime check refuses an address nobody owns and this one leaves that to the allocator, which
/// has the header in front of it and more to say.
fn freed(func: &mut Func, names: &mut Interner, table: &mut Vec<Descriptor>, inst: Inst) {
    let [capability, pointer] = func[func[inst].args] else { return };
    // No size, for the reason [`live`] has none. How many bytes are at the address is the header's
    // business and the free is not an access.
    let row = Descriptor { judgement: FREE, class: 0, size: 0 };
    let desc = record(func, names, table, inst, row);
    let params = &[Type::PTR; 3];
    call(func, names, inst, "__rucc_check_free", params, &[], &[capability, pointer, desc]);
}

/// `check_deriv` becomes `__rucc_check_deriv(base, derived, stride, descriptor)`.
///
/// The stride goes through as a value rather than into the descriptor, because a walk over a
/// variable length array steps by a width the program computes and a descriptor is constant data.
fn deriv(
    func: &mut Func,
    names: &mut Interner,
    word: Type,
    table: &mut Vec<Descriptor>,
    inst: Inst,
) {
    let [_capability, base, derived, stride] = func[func[inst].args] else { return };
    let row = Descriptor { judgement: DERIVE, class: 0, size: 0 };
    let desc = record(func, names, table, inst, row);
    let params = &[Type::PTR, Type::PTR, word, Type::PTR];
    call(func, names, inst, "__rucc_check_deriv", params, &[], &[base, derived, stride, desc]);
}

/// Which type checks have an init check beside them that belongs to the same read.
///
/// The pair is what tamnd/rucc#1617's fifth box asks about. `rucc_safety::access_checks` writes a
/// type check and an init check in front of every read, they take the same address and the same
/// width, they carry the same descriptor row, and they are the same function in the runtime up to
/// which plane it ends at. So a read that needs both finds the region twice, and finding the region
/// is the expensive half of either one.
///
/// What is not assumed is that the two are still a pair. `crate::discharge` takes one out without
/// the other often enough that the second half of this file's work has to check rather than trust,
/// and a type check fused with an init check belonging to some later read would be an init check
/// asked earlier than the program asks it, which is a refusal of a correct program.
fn pairs(func: &Func, insts: &[Inst]) -> HashMap<Inst, Inst> {
    let mut found = HashMap::new();
    for &inst in insts {
        if func[inst].opcode != Opcode::CheckType {
            continue;
        }
        if let Some(partner) = partner(func, inst) {
            found.insert(inst, partner);
        }
    }
    found
}

/// The init check that belongs to the same read as this type check, if there is one.
///
/// Same address, same width, same block, and nothing between the two that writes memory or ends
/// the block. Another check of either kind in between ends the search rather than being walked
/// past, because a second one is a second read and the pair is one read's.
fn partner(func: &Func, check: Inst) -> Option<Inst> {
    let [_capability, pointer] = func[func[check].args] else { return None };
    let Extra::Mem(mem) = func[check].extra else { return None };
    let size = func[mem].size;
    let block = func.block_of(check)?;
    let mut after = false;
    for inst in func.insts(block) {
        if inst == check {
            after = true;
            continue;
        }
        if !after {
            continue;
        }
        match func[inst].opcode {
            Opcode::CheckInit => {
                let [_capability, other] = func[func[inst].args] else { return None };
                let Extra::Mem(at) = func[inst].extra else { return None };
                return (other == pointer && func[at].size == size).then_some(inst);
            }
            Opcode::CheckType => return None,
            opcode if opcode.writes_memory() || opcode.is_terminator() => return None,
            _ => {}
        }
    }
    None
}

/// How wide a check has to be before it is lowered to the `_range` form of its routine.
///
/// Sixty four bytes is one word of the runtime's init shadow and one run of its type plane, which is
/// the step the range forms read in, so a narrower check would have nothing for them to do.
const RANGE: u64 = 64;

/// `check_type` becomes `__rucc_check_type(pointer, size, type, descriptor)`.
///
/// The one check with a type number on it, and the number is the same one [`judgement`] passes for
/// the same reason: what the store wrote and what the read asks about have to be written in one
/// vocabulary or they cannot be compared.
///
/// The descriptor says J1 rather than a judgement of its own. The type plane is one of the planes
/// document 04 section 4.4's first judgement names, so a read the plane refused is an access the
/// planes did not permit, which is the sentence the reporter already prints.
///
/// With an init check beside it that [`partner`] recognised as the same read's, it becomes
/// `__rucc_check_typed_init` instead, with the same four arguments, and that check is taken out.
/// The two rows are identical, so the one descriptor recorded here serves both, and the runtime
/// asks the type plane first, which is the order the two calls ran in.
///
/// A third operand is how many bytes to ask about, which is [`bounds`]'s arrangement and is what
/// `rucc_opt::hoist` writes when one check stands for a loop's worth. The size in the row goes to
/// zero there for the reason given in [`bounds`]: what a report would name is the width of an access
/// the program wrote, and a check covering a whole walk is not one of those.
///
/// A check over a computed width, or over a constant one of at least [`RANGE`] bytes, calls the
/// `_range` form of the routine it would have called, which takes the same arguments and asks the
/// plane a run of sixty four bytes at a time. That is what a check `rucc_opt::hoist` put in front of
/// a loop is, whichever way it wrote the width: computed where the trip count is only known at run
/// time and a constant where it is known here. It is a name of its own rather than a test of the
/// length inside the plain routine, because that test cost every access's check a few
/// instructions, and here it is decided once and for nothing.
fn typed(
    func: &mut Func,
    names: &mut Interner,
    word: Type,
    numbers: &HashMap<Meta, u32>,
    table: &mut Vec<Descriptor>,
    inst: Inst,
    partner: Option<Inst>,
) {
    let args = &func[func[inst].args];
    let (Some(&_capability), Some(&pointer), computed) =
        (args.first(), args.get(1), args.get(2).copied())
    else {
        return;
    };
    let Extra::Mem(mem) = func[inst].extra else { return };
    let size = func[mem].size;
    let Some(node) = func[mem].tbaa else { return };
    let Some(&number) = numbers.get(&node) else { return };

    let row = Descriptor {
        judgement: ACCESS,
        class: 0,
        // Saturating, for the reason [`bounds`] gives about a report of a width that does not fit.
        size: if computed.is_some() { 0 } else { u16::try_from(size).unwrap_or(u16::MAX) },
    };
    let desc = record(func, names, table, inst, row);
    let bytes = match computed {
        Some(value) => fitted(func, inst, value, word),
        None => konst(func, inst, Imm::int(i128::from(size), word), word),
    };
    let small = Type::int(32);
    let ty = konst(func, inst, Imm::int(i128::from(number), small), small);
    let params = &[Type::PTR, word, small, Type::PTR];
    let ranged = computed.is_some() || size >= RANGE;
    let routine = match partner {
        Some(init) => {
            func.remove_inst(init);
            if ranged { "__rucc_check_typed_init_range" } else { "__rucc_check_typed_init" }
        }
        None if ranged => "__rucc_check_type_range",
        None => "__rucc_check_type",
    };
    call(func, names, inst, routine, params, &[], &[pointer, bytes, ty, desc]);
}

/// `check_init` becomes `__rucc_check_init(pointer, size, descriptor)`.
///
/// No type number, because the plane it asks holds no types: one bit per byte, and the bit says
/// whether anything was ever stored there. So the call is the shape [`bounds`] has rather than the
/// shape [`typed`] has, and the size is the access's own width for the reason given there.
///
/// The descriptor says J1, as the type plane's does. The init plane is one of the planes document
/// 04 section 4.4's first judgement names, so a read the plane refused is an access the planes did
/// not permit, and that is already the sentence the reporter prints.
///
/// A third operand is how many bytes to ask about, as on [`typed`] and [`bounds`], and the call is
/// to `__rucc_check_init_range` for a range the way [`typed`] says.
fn began(
    func: &mut Func,
    names: &mut Interner,
    word: Type,
    table: &mut Vec<Descriptor>,
    inst: Inst,
) {
    let args = &func[func[inst].args];
    let (Some(&_capability), Some(&pointer), computed) =
        (args.first(), args.get(1), args.get(2).copied())
    else {
        return;
    };
    let Extra::Mem(mem) = func[inst].extra else { return };
    let size = func[mem].size;

    let row = Descriptor {
        judgement: ACCESS,
        class: 0,
        // Saturating, for the reason [`bounds`] gives about a report of a width that does not fit.
        size: if computed.is_some() { 0 } else { u16::try_from(size).unwrap_or(u16::MAX) },
    };
    let desc = record(func, names, table, inst, row);
    let bytes = match computed {
        Some(value) => fitted(func, inst, value, word),
        None => konst(func, inst, Imm::int(i128::from(size), word), word),
    };
    let ranged = computed.is_some() || size >= RANGE;
    let routine = if ranged { "__rucc_check_init_range" } else { "__rucc_check_init" };
    let params = &[Type::PTR, word, Type::PTR];
    call(func, names, inst, routine, params, &[], &[pointer, bytes, desc]);
}

/// `check_race` becomes `__rucc_check_race(pointer, size, descriptor)`.
///
/// The same shape [`began`] has, because the plane it asks holds one stamp per granule rather than
/// anything about a type, so what the runtime needs is a range and nothing else. The size is the
/// access's own width, which covers the case of an access that straddles two granules: either of
/// them carrying a stranger's stamp is a race this access is in.
///
/// The descriptor says J9 rather than J1. The reporter reads the number out of the row it was
/// handed, so this is the whole of what makes a race read as a race, and the line naming both
/// threads is added by the runtime from the two stamps rather than from anything here.
fn raced(
    func: &mut Func,
    names: &mut Interner,
    word: Type,
    table: &mut Vec<Descriptor>,
    inst: Inst,
) {
    let [_capability, pointer] = func[func[inst].args] else { return };
    let Extra::Mem(mem) = func[inst].extra else { return };
    let size = func[mem].size;

    let row = Descriptor {
        judgement: RACE,
        class: 0,
        // Saturating, for the reason [`bounds`] gives about a report of a width that does not fit.
        size: u16::try_from(size).unwrap_or(u16::MAX),
    };
    let desc = record(func, names, table, inst, row);
    let bytes = konst(func, inst, Imm::int(i128::from(size), word), word);
    let params = &[Type::PTR, word, Type::PTR];
    call(func, names, inst, "__rucc_check_race", params, &[], &[pointer, bytes, desc]);
}

/// The two numbers in the one word the runtime reads them out of.
///
/// `rucc_safe_rt::restrict::tag` is the other half of this and the two have to agree, so the
/// packing is written down in both places and tested in both. The clique is the high half and the
/// base is the low one, which puts the number that identifies the scope where a reader of a hex
/// dump will see it first.
fn tag(clique: u16, base: u16) -> u32 {
    (u32::from(clique) << 16) | u32::from(base)
}

/// `check_restrict_read` and `check_restrict_write` become
/// `__rucc_check_restrict(pointer, size, tag, write, descriptor)`.
///
/// One function for both, because which of them it was is the fourth argument and nothing else.
/// The runtime needs to know whether the access wrote because two reads of one byte through two
/// `restrict` pointers are not a violation of anything: the contract is about modification, so the
/// pair is refused only when at least one half of it wrote.
///
/// The scope is not an argument. The runtime finds it from the clique in the tag, walking the
/// blocks this thread is inside until it reaches the innermost one with that clique, which is what
/// makes a recursive function's second activation ask about its own promise and not its caller's.
fn promised(
    func: &mut Func,
    names: &mut Interner,
    word: Type,
    table: &mut Vec<Descriptor>,
    inst: Inst,
    write: bool,
) {
    let [pointer] = func[func[inst].args] else { return };
    let Extra::Mem(mem) = func[inst].extra else { return };
    let size = func[mem].size;
    let named = func[mem].restrict;

    let row = Descriptor {
        judgement: RESTRICT,
        class: 0,
        // Saturating, for the reason [`bounds`] gives about a report of a width that does not fit.
        size: u16::try_from(size).unwrap_or(u16::MAX),
    };
    let desc = record(func, names, table, inst, row);
    let bytes = konst(func, inst, Imm::int(i128::from(size), word), word);
    let small = Type::int(32);
    let which =
        konst(func, inst, Imm::int(i128::from(tag(named.clique, named.base)), small), small);
    let wrote = konst(func, inst, Imm::int(i128::from(u8::from(write)), small), small);
    let params = &[Type::PTR, word, small, small, Type::PTR];
    let args = &[pointer, bytes, which, wrote, desc];
    call(func, names, inst, "__rucc_check_restrict", params, &[], args);
}

/// `restrict_enter` becomes `__rucc_restrict_enter(scope, tag)`.
///
/// No descriptor, for the reason [`judgement`] gives about a plane write: opening a block refuses
/// nothing, so there is no failure to describe. The base half of the tag is how many pointers the
/// block declares rather than which of them this is, since the runtime has to know how much of the
/// slot to clear before the block starts recording into it.
fn opened(func: &mut Func, names: &mut Interner, inst: Inst) {
    let [scope] = func[func[inst].args] else { return };
    let Extra::Mem(mem) = func[inst].extra else { return };
    let named = func[mem].restrict;
    let small = Type::int(32);
    let which =
        konst(func, inst, Imm::int(i128::from(tag(named.clique, named.base)), small), small);
    call(func, names, inst, "__rucc_restrict_enter", &[Type::PTR, small], &[], &[scope, which]);
}

/// `restrict_leave` becomes `__rucc_restrict_leave(scope)`.
///
/// The slot alone, and no numbers, because closing a block is a matter of putting back whatever it
/// was inside and the slot already says what that was.
fn closed(func: &mut Func, names: &mut Interner, inst: Inst) {
    let [scope] = func[func[inst].args] else { return };
    call(func, names, inst, "__rucc_restrict_leave", &[Type::PTR], &[], &[scope]);
}

/// `safe_region_begin` and `safe_region_end` become nothing at all.
///
/// The only pair here that lowers to no call, and the reason is that a declared region is a fact
/// about the build rather than a thing the program does. Everything between the two markers is code
/// the monitor was told not to judge, so there is no check to emit, no plane to write and no state
/// for the runtime to keep: the region has already had its effect by the time this runs, which was
/// to keep the checks from being written in the first place.
///
/// What a region does cost is the row `spec/safe-memory/10-boundaries.md` section 10.2 asks for,
/// and [`crate::summary`] has already taken it. That pass runs on the front end's IR, before the
/// back end and so before this, which is the order that makes the count possible at all: after this
/// the object file has no trace that a region was ever declared.
fn declared(func: &mut Func, inst: Inst) {
    func.remove_inst(inst);
}

/// `meta_type` becomes `__rucc_meta_type(pointer, size, type)`.
///
/// No descriptor, and it is the only thing here with a payload that has none. A plane write refuses
/// nothing and reports nothing: it records the fact that the check of the same name will later ask
/// about, so there is no failure for a descriptor to describe.
///
/// The type is a number rather than a node, and `crate::plane` is where the number comes from and
/// why it is a hash of the type's name. It travels in thirty two bits because the plane holds
/// thirty two bits per byte of program memory, which is document 05 section 5.2.3's measurement and
/// not a choice made here.
fn judgement(
    func: &mut Func,
    names: &mut Interner,
    word: Type,
    numbers: &HashMap<Meta, u32>,
    inst: Inst,
) {
    let [pointer, length] = func[func[inst].args] else { return };
    let Extra::Node(node) = func[inst].extra else { return };
    let Some(&number) = numbers.get(&node) else { return };
    let bytes = fitted(func, inst, length, word);
    let small = Type::int(32);
    let ty = konst(func, inst, Imm::int(i128::from(number), small), small);
    let params = &[Type::PTR, word, small];
    call(func, names, inst, "__rucc_meta_type", params, &[], &[pointer, bytes, ty]);
}

/// `meta_type_copy` becomes `__rucc_meta_type_copy(destination, source, length)`.
///
/// No descriptor and no type number, for the two reasons [`judgement`] gives: a plane write refuses
/// nothing, and what the copied bytes are is not something the compiler knows. The runtime reads the
/// entries over the source and writes them over the destination, so the type travels without
/// anybody here having to name it.
fn carriage(func: &mut Func, names: &mut Interner, word: Type, inst: Inst) {
    let [to, from, length] = func[func[inst].args] else { return };
    let bytes = fitted(func, inst, length, word);
    let params = &[Type::PTR, Type::PTR, word];
    call(func, names, inst, "__rucc_meta_type_copy", params, &[], &[to, from, bytes]);
}

/// `meta_init` becomes `__rucc_meta_init(pointer, size)`.
///
/// No descriptor, for the reason [`judgement`] has none, and no type either. The init plane holds
/// one bit per byte and the bit says whether anything was ever stored there, so a range is the whole
/// of what a store has to say about it and there is nothing else to pass.
fn written(func: &mut Func, names: &mut Interner, word: Type, inst: Inst) {
    let [pointer, length] = func[func[inst].args] else { return };
    let bytes = fitted(func, inst, length, word);
    let params = &[Type::PTR, word];
    call(func, names, inst, "__rucc_meta_init", params, &[], &[pointer, bytes]);
}

/// `meta_init_copy` becomes `__rucc_meta_init_copy(destination, source, length)`.
///
/// The same shape as [`carriage`] and for the same reason: a copy writes no values of its own, so
/// whether a destination byte holds anything is whether the byte it came from did, and the plane
/// over the source is the only place that is written down.
fn carried(func: &mut Func, names: &mut Interner, word: Type, inst: Inst) {
    let [to, from, length] = func[func[inst].args] else { return };
    let bytes = fitted(func, inst, length, word);
    let params = &[Type::PTR, Type::PTR, word];
    call(func, names, inst, "__rucc_meta_init_copy", params, &[], &[to, from, bytes]);
}

/// `cap_copy` becomes `__rucc_cap_copy(destination, source, length)`.
///
/// The same shape as [`carried`] again, and the same argument for why it names nothing else: the
/// capability of a pointer in memory is in the slot beside it, so the slots over the source are
/// where the answer already is and the runtime moves them across. It is the one of the three that
/// is not a plane write, and the call it becomes is the one `__rucc_wrap_memcpy` has always made.
fn relocated(func: &mut Func, names: &mut Interner, word: Type, inst: Inst) {
    let [to, from, length] = func[func[inst].args] else { return };
    let bytes = fitted(func, inst, length, word);
    let params = &[Type::PTR, Type::PTR, word];
    call(func, names, inst, "__rucc_cap_copy", params, &[], &[to, from, bytes]);
}

/// `meta_epoch` becomes `__rucc_meta_epoch(pointer, length)`.
///
/// The same shape as [`written`], and carrying no thread and no count for the reason the opcode
/// gives: which thread is running and how far it has counted are facts about the moment the program
/// gets here, so the runtime reads them and nothing this pass could pass in would be either.
fn stamped(func: &mut Func, names: &mut Interner, word: Type, inst: Inst) {
    let [pointer, length] = func[func[inst].args] else { return };
    let bytes = fitted(func, inst, length, word);
    let params = &[Type::PTR, word];
    call(func, names, inst, "__rucc_meta_epoch", params, &[], &[pointer, bytes]);
}

/// `meta_release` becomes `__rucc_meta_release(object)`.
///
/// One operand and no length, because an edge is about everything the thread did rather than about
/// a range of bytes, and no descriptor, because publishing a clock decides nothing and so has
/// nothing to report. The runtime entry point is the same `sync::released` the `pthread` wrappers
/// call, keyed on the address the same way, so an ordering established through an atomic and one
/// established through a mutex are the same edge to everything that reads them.
fn published(func: &mut Func, names: &mut Interner, inst: Inst) {
    let [object] = func[func[inst].args] else { return };
    call(func, names, inst, "__rucc_meta_release", &[Type::PTR], &[], &[object]);
}

/// `meta_acquire` becomes `__rucc_meta_acquire(object)`.
///
/// The other end of [`published`], and the same shape.
fn taken(func: &mut Func, names: &mut Interner, inst: Inst) {
    let [object] = func[func[inst].args] else { return };
    call(func, names, inst, "__rucc_meta_acquire", &[Type::PTR], &[], &[object]);
}

/// `meta_fence_release` becomes `__rucc_meta_fence_release()`.
///
/// No operands at all, which is the whole difference between a fence and an atomic here. A fence
/// orders against every other thread rather than against one object, so there is no address to pass
/// and the runtime keeps one cell for every fence in the program instead of a table keyed by one.
fn published_everywhere(func: &mut Func, names: &mut Interner, inst: Inst) {
    call(func, names, inst, "__rucc_meta_fence_release", &[], &[], &[]);
}

/// `meta_fence_acquire` becomes `__rucc_meta_fence_acquire()`.
///
/// The other end of [`published_everywhere`], and the same shape.
fn taken_everywhere(func: &mut Func, names: &mut Interner, inst: Inst) {
    call(func, names, inst, "__rucc_meta_fence_acquire", &[], &[], &[]);
}

/// `cap_extent` becomes `__rucc_extent(pointer, want)`, and `cap_extent_back` the backward one.
///
/// No descriptor, and these two are the only ones of these that have none. The other four are
/// judgements and a judgement that refuses has to say what it refused. These decide nothing: they
/// are the question section 7.4 asks before a loop so that the loop can be split, once for a walk
/// that goes up and once for a walk that goes down, the answer is a number, and there is no failure
/// to describe.
///
/// One function for both because the two differ in the name they call and in nothing else. The
/// operands are the same three, the result is the same count, and the width the count comes back in
/// is handled the same way.
fn extent(func: &mut Func, names: &mut Interner, word: Type, inst: Inst, called: &str) {
    let [_capability, address, want] = func[func[inst].args] else { return };
    let asked = fitted(func, inst, want, word);
    let result = func[inst].results().next().expect("an extent query produces one value");
    let ty = func[result].ty;
    let params = &[Type::PTR, word];
    if ty == word {
        call(func, names, inst, called, params, &[word], &[address, asked]);
        return;
    }
    // The count came out in a width that is not the target's, for the reason [`fitted`] gives about
    // the operand going the other way. The call is made beside the instruction in the width the
    // runtime declares and the instruction itself becomes the conversion back, so that everything
    // reading its result still reads a value of the type it had.
    let made = calling(func, names, called, params, &[word], &[address, asked]);
    let holder = func.create_inst(made, &[word], func.span(inst));
    func.insert_before(holder, inst);
    let got = func[holder].results().next().expect("a call returning one value produces one");
    let opcode = if word.bits() > ty.bits() { Opcode::Trunc } else { Opcode::ZExt };
    let args = func.push_values(&[got]);
    func[inst] = InstData { args, ..InstData::new(opcode) };
}

/// Writes a descriptor down and gives back the address the call passes.
///
/// The `global_addr` goes in front of the check rather than at the top of the function, because the
/// back end turns it into one `lea` off the instruction pointer and putting it beside its use is
/// what keeps the value from being live across everything in between.
fn record(
    func: &mut Func,
    names: &mut Interner,
    table: &mut Vec<Descriptor>,
    inst: Inst,
    row: Descriptor,
) -> Value {
    let name = names.intern(&label(table.len()));
    table.push(row);
    let span = func.span(inst);
    let data = InstData { extra: Extra::Symbol(name), ..InstData::new(Opcode::GlobalAddr) };
    let made = func.create_inst(data, &[Type::PTR], span);
    func.insert_before(made, inst);
    func[made].results().next().expect("an address created with one result has one")
}

/// What the descriptor in position `index` is called.
fn label(index: usize) -> String {
    format!("{DESCRIPTOR}_{index}")
}

/// Puts an integer constant in front of `inst` and gives back what it produced.
fn konst(func: &mut Func, inst: Inst, imm: Imm, ty: Type) -> Value {
    let span = func.span(inst);
    let extra = Extra::Imm(func.add_imm(imm));
    let made = func.create_inst(InstData { extra, ..InstData::new(Opcode::IConst) }, &[ty], span);
    func.insert_before(made, inst);
    func[made].results().next().expect("a constant created with one result has one")
}

/// Turns `inst` into a call of `routine` with those arguments, in place.
///
/// In place rather than as a new instruction beside it, because the check is already where it has
/// to be: in front of the access for the two access checks and behind the arithmetic for the
/// derivation one. Moving it would be a chance to get that wrong.
pub(crate) fn call(
    func: &mut Func,
    names: &mut Interner,
    inst: Inst,
    routine: &str,
    params: &[Type],
    returns: &[Type],
    args: &[Value],
) {
    let made = calling(func, names, routine, params, returns, args);
    let data = &mut func[inst];
    data.opcode = made.opcode;
    data.args = made.args;
    data.extra = made.extra;
    data.flags = data.flags.intersection(Flags::legal_on(Opcode::Call));
}

/// A call to `routine` with those arguments, not yet anywhere.
///
/// Separate from [`call`] because two rewrites need the call beside the instruction rather than in
/// place of it, and building the signature and the callee is the part all of them have in common.
/// The extent query is one, when the count comes back in a width that is not the instruction's, and
/// `crate::slot`'s allocation capability is the other, because there the instruction gives back a
/// value and the call does not.
pub(crate) fn calling(
    func: &mut Func,
    names: &mut Interner,
    routine: &str,
    params: &[Type],
    returns: &[Type],
    args: &[Value],
) -> InstData {
    let sig = func.add_signature(Signature::new().with_params(params).with_returns(returns));
    let callee = names.intern(routine);
    // Nothing is passed past the last named parameter, so there is nothing for the ABI to say
    // about the arguments the signature does not name.
    let varargs = func.push_abis(&[]);
    let info = func.add_call(CallInfo { callee: Some(callee), signature: sig, varargs });
    let args = func.push_values(args);
    InstData { args, extra: Extra::Call(info), ..InstData::new(Opcode::Call) }
}

/// Adds one descriptor to the module as a variable in the shared section.
///
/// Internal, so the linker never has to resolve the name and two objects in a link do not collide
/// over it. Constant, because nothing writes a descriptor after the compiler has. Eight byte
/// aligned and sixteen bytes long, because the runtime reads it as a `#[repr(C)]` structure with a
/// `u64` in it, and because that is what makes the section as a whole a packed array of them.
fn emit(module: &mut Module, names: &mut Interner, index: usize, row: Descriptor) {
    let byte = Type::int(8);
    let half = Type::int(16);
    let judgement = module.add_imm(Imm::int(i128::from(row.judgement), byte));
    let class = module.add_imm(Imm::int(i128::from(row.class), byte));
    let size = module.add_imm(Imm::int(i128::from(row.size), half));
    let image = [
        Datum::Scalar { ty: byte, value: judgement },
        Datum::Scalar { ty: byte, value: class },
        Datum::Scalar { ty: half, value: size },
        // Four bytes the C layout puts in front of the `u64`, and then the eight of the program
        // counter, which nothing fills in yet. Both are zero and both are written out rather than
        // left off, because the descriptor after this one has to start sixteen bytes along.
        Datum::Zero(4),
        Datum::Zero(8),
    ];
    let init = module.push_data(&image);
    let mut global = Global::new(names.intern(&label(index)), WIDTH, 8);
    global.linkage = Linkage::Internal;
    global.constant = true;
    global.section = Some(names.intern(SECTION));
    global.init = Some(init);
    module.add_global(global);
}

#[cfg(test)]
mod tests {
    use rucc_ir::{
        Builder, MemInfo, MemOrder, MetaNode, Restrict, RmwOp, TbaaNode, print_func, verify_func,
    };
    use rucc_target::{Arch, Env, Os, TargetInfo, Triple};

    use super::*;
    use crate::{Plane, Promise, Races, Subobject, insert};

    fn target() -> TargetInfo {
        TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu))
    }

    /// A module holding one function that loads through its parameter, with checks already in.
    fn checked(names: &mut Interner) -> Module {
        let i32_ = Type::int(32);
        let mut func = Func::new(
            names.intern("read"),
            Signature::new().with_params(&[Type::PTR]).with_returns(&[i32_]),
        );
        let entry = func.create_block();
        let p = func.append_param(entry, Type::PTR);

        let info = MemInfo {
            size: 4,
            align: 4,
            order: MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        };
        let mut b = Builder::new(&mut func, entry);
        let args = b.func().push_values(&[p]);
        let extra = Extra::Mem(b.func().add_mem(info));
        let loaded = b.value(InstData { args, extra, ..InstData::new(Opcode::Load) }, i32_);
        b.ret(&[loaded]);

        insert(&mut func, &planeless(names).0, 8, Subobject::Off, Promise::Off, Races::Off);
        let mut module = Module::new(names.intern("read.c"), &target());
        module.add_func(func);
        module
    }

    /// The same function [`checked`] builds, over an access that may assume nothing about where
    /// it starts, which is what a member of a packed record is.
    fn unaligned(names: &mut Interner) -> Module {
        let i32_ = Type::int(32);
        let mut func = Func::new(
            names.intern("read"),
            Signature::new().with_params(&[Type::PTR]).with_returns(&[i32_]),
        );
        let entry = func.create_block();
        let p = func.append_param(entry, Type::PTR);

        let info = MemInfo {
            size: 4,
            align: 1,
            order: MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        };
        let mut b = Builder::new(&mut func, entry);
        let args = b.func().push_values(&[p]);
        let extra = Extra::Mem(b.func().add_mem(info));
        let loaded = b.value(InstData { args, extra, ..InstData::new(Opcode::Load) }, i32_);
        b.ret(&[loaded]);

        insert(&mut func, &planeless(names).0, 8, Subobject::Off, Promise::Off, Races::Off);
        let mut module = Module::new(names.intern("read.c"), &target());
        module.add_func(func);
        module
    }

    /// A plane for a function that stores nothing, and the numbering that goes with it.
    ///
    /// Every function in these tests reads or derives and none of them stores, so there is nothing
    /// to record and the entries are never named. What the two are for is that [`crate::insert`]
    /// and [`calls`] take them whether or not the function has a store in it.
    fn planeless(names: &mut Interner) -> (Plane, HashMap<Meta, u32>) {
        let mut module = Module::new(names.intern("planeless.c"), &target());
        let plane = Plane::build(&mut module);
        let numbers = plane::numbers(&module, names);
        (plane, numbers)
    }

    /// A module with one function that copies a fixed number of bytes, with the plane write in.
    fn copied(names: &mut Interner) -> Module {
        let mut func =
            Func::new(names.intern("move"), Signature::new().with_params(&[Type::PTR, Type::PTR]));
        let entry = func.create_block();
        let to = func.append_param(entry, Type::PTR);
        let from = func.append_param(entry, Type::PTR);

        let info = MemInfo {
            size: 24,
            align: 8,
            order: MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        };
        let mut b = Builder::new(&mut func, entry);
        let args = b.func().push_values(&[to, from]);
        let extra = Extra::Mem(b.func().add_mem(info));
        b.inst(InstData { args, extra, ..InstData::new(Opcode::Memcpy) }, &[]);
        b.ret(&[]);

        insert(&mut func, &planeless(names).0, 8, Subobject::Off, Promise::Off, Races::Off);
        let mut module = Module::new(names.intern("move.c"), &target());
        module.add_func(func);
        module
    }

    /// A module with one function that stores through its parameter, with the plane writes in.
    ///
    /// The plane is this module's rather than [`planeless`]'s, because the store records into it
    /// and a judgement naming an entry another module holds is a judgement [`judgement`] leaves
    /// alone.
    fn stored(names: &mut Interner) -> Module {
        let mut module = Module::new(names.intern("write.c"), &target());
        let plane = Plane::build(&mut module);

        let i64_ = Type::int(64);
        let mut func =
            Func::new(names.intern("write"), Signature::new().with_params(&[Type::PTR, i64_]));
        let entry = func.create_block();
        let p = func.append_param(entry, Type::PTR);
        let v = func.append_param(entry, i64_);

        let info = MemInfo {
            size: 8,
            align: 8,
            order: MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        };
        let mut b = Builder::new(&mut func, entry);
        let args = b.func().push_values(&[v, p]);
        let extra = Extra::Mem(b.func().add_mem(info));
        b.inst(InstData { args, extra, ..InstData::new(Opcode::Store) }, &[]);
        b.ret(&[]);

        insert(&mut func, &plane, 8, Subobject::Off, Promise::Off, Races::Off);
        module.add_func(func);
        module
    }

    /// A module with one function that stores a pointer through its parameter, checks in.
    ///
    /// `-fsafety-races=metadata`, so the store carries the epoch plane's write as well as the other
    /// two. The value stored is a pointer because that is the only kind of store the epoch plane
    /// takes, which [`crate::stamped`] argues.
    fn racing(names: &mut Interner) -> Module {
        let mut module = Module::new(names.intern("stamp.c"), &target());
        let plane = Plane::build(&mut module);

        let mut func =
            Func::new(names.intern("stamp"), Signature::new().with_params(&[Type::PTR, Type::PTR]));
        let entry = func.create_block();
        let p = func.append_param(entry, Type::PTR);
        let q = func.append_param(entry, Type::PTR);

        let info = MemInfo {
            size: 8,
            align: 8,
            order: MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        };
        let mut b = Builder::new(&mut func, entry);
        let args = b.func().push_values(&[q, p]);
        let extra = Extra::Mem(b.func().add_mem(info));
        b.inst(InstData { args, extra, ..InstData::new(Opcode::Store) }, &[]);
        b.ret(&[]);

        insert(&mut func, &plane, 8, Subobject::Off, Promise::Off, Races::Metadata);
        module.add_func(func);
        module
    }

    /// A module with one function holding a `seq_cst` atomic store, edges in.
    ///
    /// Sequentially consistent because it publishes and takes both, so one atomic covers the two
    /// markers and the order they came out in is readable off one printed function.
    fn ordering(names: &mut Interner) -> Module {
        let mut module = Module::new(names.intern("edge.c"), &target());
        let plane = Plane::build(&mut module);

        let i64_ = Type::int(64);
        let mut func =
            Func::new(names.intern("publish"), Signature::new().with_params(&[Type::PTR, i64_]));
        let entry = func.create_block();
        let p = func.append_param(entry, Type::PTR);
        let v = func.append_param(entry, i64_);

        let info = MemInfo {
            size: 8,
            align: 8,
            order: MemOrder::SeqCst,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        };
        let mut b = Builder::new(&mut func, entry);
        let at = b.func().add_mem(info);
        let args = b.func().push_values(&[p, v]);
        let extra = Extra::Rmw(RmwOp::Add, at);
        b.inst(InstData { args, extra, ..InstData::new(Opcode::AtomicRmw) }, &[i64_]);
        b.ret(&[]);

        insert(&mut func, &plane, 8, Subobject::Off, Promise::Off, Races::Metadata);
        module.add_func(func);
        module
    }

    #[test]
    fn the_two_halves_of_an_edge_become_the_calls_the_interposed_locks_already_make() {
        // One operand each, no length and no descriptor. An edge is not about a range of bytes, it
        // is about everything the thread did either side of it, and publishing a clock decides
        // nothing so there is no row to point at. The runtime entry points are the same two the
        // `pthread` wrappers call, keyed on an address the same way, so an ordering established
        // through an atomic and one established through a mutex are one table and one clock.
        let mut names = Interner::new();
        let mut module = ordering(&mut names);
        lower(&mut module, &mut names);

        let id = module.funcs().next().expect("the module has one function");
        let printed = print_func(&module, &module[id], &names);
        assert!(printed.contains("call @__rucc_meta_release(%0) : (ptr)\n"), "{printed}");
        assert!(printed.contains("call @__rucc_meta_acquire(%0) : (ptr)\n"), "{printed}");

        // In front of the atomic and behind it, which is the order the ordering itself is in.
        let published = printed.find("__rucc_meta_release").expect("the publishing half lowered");
        let changed = printed.find("atomic_rmw").expect("the atomic is still there");
        let took = printed.find("__rucc_meta_acquire").expect("and the taking half lowered");
        assert!(published < changed && changed < took, "{printed}");

        if let Err(errors) = verify_func(&module, &module[id], &names) {
            panic!("that was expected to be believed: {errors:#?}");
        }
    }

    /// A module with one function holding a `seq_cst` fence, edges in.
    fn barrier(names: &mut Interner) -> Module {
        let mut module = Module::new(names.intern("fence.c"), &target());
        let plane = Plane::build(&mut module);

        let mut func = Func::new(names.intern("barrier"), Signature::new());
        let entry = func.create_block();
        let mut b = Builder::new(&mut func, entry);
        let extra = Extra::Order(MemOrder::SeqCst);
        b.inst(InstData { extra, ..InstData::new(Opcode::Fence) }, &[]);
        b.ret(&[]);

        insert(&mut func, &plane, 8, Subobject::Off, Promise::Off, Races::Metadata);
        module.add_func(func);
        module
    }

    #[test]
    fn the_edge_a_fence_carries_becomes_a_call_that_takes_nothing_at_all() {
        // No operand, which is the whole difference. A fence orders against every other thread
        // rather than against one object, so there is no address to hand the runtime and it keeps
        // one clock for every fence in the program instead of a table keyed by one.
        let mut names = Interner::new();
        let mut module = barrier(&mut names);
        lower(&mut module, &mut names);

        let id = module.funcs().next().expect("the module has one function");
        let printed = print_func(&module, &module[id], &names);
        assert!(printed.contains("call @__rucc_meta_fence_release() : ()\n"), "{printed}");
        assert!(printed.contains("call @__rucc_meta_fence_acquire() : ()\n"), "{printed}");

        let published = printed.find("fence_release").expect("the publishing half lowered");
        let barrier = printed.find("    fence ").expect("the fence is still there");
        let took = printed.find("fence_acquire").expect("and the taking half lowered");
        assert!(published < barrier && barrier < took, "{printed}");

        if let Err(errors) = verify_func(&module, &module[id], &names) {
            panic!("that was expected to be believed: {errors:#?}");
        }
    }

    /// A module with one function that reads through its parameter as an `int`, checks in.
    ///
    /// The aliasing node is built by hand rather than by the front end, since this crate cannot
    /// depend on the one that builds the tree. What matters is the shape: a root and one type under
    /// it, which is what `rucc_lower::aliasing` produces for a translation unit that reads an `int`.
    fn asking_the_plane(names: &mut Interner) -> Module {
        let mut module = Module::new(names.intern("read.c"), &target());
        let root = names.intern("char");
        let root =
            module.add_meta(MetaNode::Tbaa(TbaaNode { name: root, parent: None, offset: 0 }));
        let int = names.intern("int");
        let int =
            module.add_meta(MetaNode::Tbaa(TbaaNode { name: int, parent: Some(root), offset: 0 }));
        let plane = Plane::build(&mut module);

        let i32_ = Type::int(32);
        let mut func = Func::new(
            names.intern("read"),
            Signature::new().with_params(&[Type::PTR]).with_returns(&[i32_]),
        );
        let entry = func.create_block();
        let p = func.append_param(entry, Type::PTR);
        let info = MemInfo {
            size: 4,
            align: 4,
            order: MemOrder::NotAtomic,
            tbaa: Some(int),
            owns: 0,
            restrict: Restrict::NONE,
        };
        let mut b = Builder::new(&mut func, entry);
        let args = b.func().push_values(&[p]);
        let extra = Extra::Mem(b.func().add_mem(info));
        let loaded = b.value(InstData { args, extra, ..InstData::new(Opcode::Load) }, i32_);
        b.ret(&[loaded]);

        insert(&mut func, &plane, 8, Subobject::Off, Promise::Off, Races::Off);
        module.add_func(func);
        module
    }

    /// One instruction that says something and produces nothing, with a payload or without one.
    fn marker(b: &mut Builder<'_>, opcode: Opcode, info: Option<MemInfo>, on: &[Value]) {
        let args = b.func().push_values(on);
        let extra = match info {
            Some(info) => Extra::Mem(b.func().add_mem(info)),
            None => Extra::None,
        };
        b.inst(InstData { args, extra, ..InstData::new(opcode) }, &[]);
    }

    /// A module with one function that reaches two objects through two `restrict` pointers.
    ///
    /// Built by hand rather than by [`insert`], because nothing puts these in yet: the pass that
    /// does is the other half of this and it is not written. What this file is about is the calls,
    /// so what the function has to be is the shape the verifier believes.
    fn promising(names: &mut Interner) -> Module {
        let i32_ = Type::int(32);
        let mut func = Func::new(
            names.intern("kernel"),
            Signature::new().with_params(&[Type::PTR, Type::PTR]),
        );
        let entry = func.create_block();
        let to = func.append_param(entry, Type::PTR);
        let from = func.append_param(entry, Type::PTR);

        let empty = MemInfo {
            size: 0,
            align: 1,
            order: MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        };
        // The slot the block keeps its record in, whose size is `rucc_safe_rt::restrict::Scope`.
        let slot = MemInfo { size: 112, align: 8, ..empty };
        let mut b = Builder::new(&mut func, entry);
        let extra = Extra::Mem(b.func().add_mem(slot));
        let scope = b.value(InstData { extra, ..InstData::new(Opcode::Alloca) }, Type::PTR);

        // Two bases of one clique, which is what a function with two `restrict` parameters gets.
        let read =
            MemInfo { size: 4, align: 4, restrict: Restrict { clique: 1, base: 2 }, ..empty };
        let writ =
            MemInfo { size: 4, align: 4, restrict: Restrict { clique: 1, base: 1 }, ..empty };
        let opening = MemInfo { restrict: Restrict { clique: 1, base: 2 }, ..slot };
        marker(&mut b, Opcode::RestrictEnter, Some(opening), &[scope]);
        marker(&mut b, Opcode::CheckRestrictRead, Some(read), &[from]);
        let args = b.func().push_values(&[from]);
        let extra = Extra::Mem(b.func().add_mem(read));
        let loaded = b.value(InstData { args, extra, ..InstData::new(Opcode::Load) }, i32_);
        marker(&mut b, Opcode::CheckRestrictWrite, Some(writ), &[to]);
        let args = b.func().push_values(&[loaded, to]);
        let extra = Extra::Mem(b.func().add_mem(writ));
        b.inst(InstData { args, extra, ..InstData::new(Opcode::Store) }, &[]);
        marker(&mut b, Opcode::RestrictLeave, None, &[scope]);
        b.ret(&[]);

        let mut module = Module::new(names.intern("kernel.c"), &target());
        module.add_func(func);
        module
    }

    #[test]
    fn a_restrict_check_becomes_the_call_that_says_which_pointer_reached_where() {
        // Two descriptors, one per check, because each of them is a judgement that can refuse and a
        // judgement that refuses has to say what it refused. The two markers have none, for the
        // reason a plane write has none: opening and closing a block decides nothing.
        let mut names = Interner::new();
        let mut module = promising(&mut names);
        assert_eq!(lower(&mut module, &mut names), 2);

        let id = module.funcs().next().expect("the module has one function");
        assert_eq!(
            print_func(&module, &module[id], &names),
            "func @kernel(ptr, ptr), linkage(external) {\n\
             block0(%0: ptr, %1: ptr):\n    \
             %2 = alloca, size 112, align 8\n    \
             %3 = iconst.i32 65538\n    \
             call @__rucc_restrict_enter(%2, %3) : (ptr, i32)\n    \
             %4 = global_addr @__rucc_safety_desc_0\n    \
             %5 = iconst.i64 4\n    \
             %6 = iconst.i32 65538\n    \
             %7 = iconst.i32 0\n    \
             call @__rucc_check_restrict(%1, %5, %6, %7, %4) : (ptr, i64, i32, i32, ptr)\n    \
             %8 = load.i32 %1, size 4, align 4, restrict(1, 2)\n    \
             %9 = global_addr @__rucc_safety_desc_1\n    \
             %10 = iconst.i64 4\n    \
             %11 = iconst.i32 65537\n    \
             %12 = iconst.i32 1\n    \
             call @__rucc_check_restrict(%0, %10, %11, %12, %9) : (ptr, i64, i32, i32, ptr)\n    \
             store %8 -> %0, size 4, align 4, restrict(1, 1)\n    \
             call @__rucc_restrict_leave(%2) : (ptr)\n    \
             return\n\
             }\n"
        );

        if let Err(errors) = verify_func(&module, &module[id], &names) {
            panic!("that was expected to be believed: {errors:#?}");
        }
    }

    #[test]
    fn the_judgement_a_restrict_check_names_is_the_one_about_the_pair() {
        // J8 rather than J1. Document 04 section 4.6 keeps this judgement out of J1 on purpose,
        // because a single access is never the violation: what is refused is a pair of them, and
        // the reporter prints a different sentence for it.
        let mut names = Interner::new();
        let mut module = promising(&mut names);
        lower(&mut module, &mut names);

        let rows: Vec<u8> = module
            .globals()
            .map(|id| {
                let init = module[id].init.expect("a descriptor is a definition");
                match module[init][0] {
                    Datum::Scalar { value, .. } => {
                        u8::try_from(module[value].bits()).expect("a judgement is one byte")
                    }
                    _ => panic!("a descriptor starts with its judgement"),
                }
            })
            .collect();
        assert_eq!(rows, [RESTRICT, RESTRICT]);
    }

    #[test]
    fn the_two_numbers_are_packed_the_way_the_runtime_unpacks_them() {
        // The other half of this is `rucc_safe_rt::restrict::tag`, and the two agree by both being
        // written down rather than by one calling the other, since this crate does not depend on
        // the runtime. A clique in the low half and a base in the high one would be read as a
        // scope nobody opened, which the runtime would pass and nobody would notice.
        assert_eq!(tag(1, 2), 0x0001_0002);
        assert_eq!(tag(0xffff, 0xffff), u32::MAX);
        assert_eq!(tag(0, 0), 0);
    }

    #[test]
    fn a_read_of_the_plane_becomes_the_call_that_carries_the_type_asked_about() {
        // Three rows rather than two, because a read now asks two questions of two planes and the
        // pair of them is a judgement that has to say what it refused. The type travels as the same
        // number a store of the same type would have recorded, which is the only way the two can be
        // compared. There is no fourth row for the init question because the two questions are one
        // call here, and a type check's row and an init check's row say the same thing.
        let mut names = Interner::new();
        let mut module = asking_the_plane(&mut names);
        assert_eq!(lower(&mut module, &mut names), 3);

        // The printer writes an `i32` immediate as a signed number and the identifier is a hash
        // that uses the whole width, so what appears is the same bits read the other way round.
        let number = i32::from_ne_bytes(plane::identifier("int").to_ne_bytes());
        let id = module.funcs().next().expect("the module has one function");
        assert_eq!(
            print_func(&module, &module[id], &names),
            format!(
                "func @read(ptr) -> i32, linkage(external) {{\n\
                 block0(%0: ptr):\n    \
                 %1 = alloca, size 32, align 8\n    \
                 call @__rucc_cap_recover(%1, %0) : (ptr, ptr)\n    \
                 %2 = global_addr @__rucc_safety_desc_0\n    \
                 %3 = iconst.i64 4\n    \
                 %4 = iconst.i64 4\n    \
                 call @__rucc_check_bounds(%0, %3, %4, %1, %2) : (ptr, i64, i64, ptr, ptr)\n    \
                 %5 = global_addr @__rucc_safety_desc_1\n    \
                 call @__rucc_check_live(%1, %0, %5) : (ptr, ptr, ptr)\n    \
                 %6 = global_addr @__rucc_safety_desc_2\n    \
                 %7 = iconst.i64 4\n    \
                 %8 = iconst.i32 {number}\n    \
                 call @__rucc_check_typed_init(%0, %7, %8, %6) : (ptr, i64, i32, ptr)\n    \
                 %9 = load.i32 %0, size 4, align 4, tbaa !1\n    \
                 return %9\n\
                 }}\n"
            )
        );

        if let Err(errors) = verify_func(&module, &module[id], &names) {
            panic!("that was expected to be believed: {errors:#?}");
        }
    }

    /// What stands between the two plane checks in [`plane_checks`].
    enum Between {
        /// Nothing at all, which is what one read leaves behind.
        Nothing,
        /// Nothing, but the init check asks about twice as many bytes.
        Wider,
        /// A store, which is memory changing between the two questions.
        Store,
        /// Another type check, which is another read.
        Another,
    }

    /// A function with a type check and an init check over the same four bytes at its parameter,
    /// with `between` standing between the two, and the type check.
    ///
    /// The shape [`crate::access_checks`] writes in front of a read, cut down to the two checks
    /// [`partner`] has to decide about. Nothing here is lowered, because what is being tested is
    /// the decision rather than the call it leads to.
    fn plane_checks(names: &mut Interner, between: Between) -> (Func, Inst) {
        let mut func = Func::new(names.intern("read"), Signature::new().with_params(&[Type::PTR]));
        let entry = func.create_block();
        let p = func.append_param(entry, Type::PTR);
        let info = MemInfo {
            size: 4,
            align: 4,
            order: MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        };

        let mut b = Builder::new(&mut func, entry);
        let args = b.func().push_values(&[p]);
        let cap = b.value(InstData { args, ..InstData::new(Opcode::CapOf) }, Type::CAP);
        marker(&mut b, Opcode::CheckType, Some(info), &[cap, p]);
        let check = b.func().insts(entry).last().expect("the type check was just put in");
        match between {
            Between::Nothing | Between::Wider => {}
            Between::Store => {
                let zero = b.iconst(Type::int(32), 0);
                marker(&mut b, Opcode::Store, Some(info), &[zero, p]);
            }
            Between::Another => marker(&mut b, Opcode::CheckType, Some(info), &[cap, p]),
        }
        let asked = match between {
            Between::Wider => MemInfo { size: 8, ..info },
            _ => info,
        };
        marker(&mut b, Opcode::CheckInit, Some(asked), &[cap, p]);
        b.ret(&[]);
        (func, check)
    }

    #[test]
    fn the_init_check_beside_a_type_check_belongs_to_the_same_read() {
        let mut names = Interner::new();
        let (func, check) = plane_checks(&mut names, Between::Nothing);
        let found = partner(&func, check).expect("the two are one read's");
        assert_eq!(func[found].opcode, Opcode::CheckInit);
    }

    #[test]
    fn an_init_check_over_other_bytes_than_the_type_check_asked_about_is_not_its_partner() {
        // Same address and a different width is two reads of the same place, and fusing them would
        // ask the init question about four bytes the program has not read yet.
        let mut names = Interner::new();
        let (func, check) = plane_checks(&mut names, Between::Wider);
        assert!(partner(&func, check).is_none());
    }

    #[test]
    fn a_store_between_the_two_plane_checks_keeps_them_apart() {
        // The two calls happen either side of the store, so the init question is answered against
        // the planes the store left rather than the ones the type question saw.
        let mut names = Interner::new();
        let (func, check) = plane_checks(&mut names, Between::Store);
        assert!(partner(&func, check).is_none());
    }

    #[test]
    fn the_init_check_of_a_later_read_is_not_an_earlier_reads_partner() {
        // A second type check is a second read, and its init check is the one that follows it. The
        // search stops rather than walking past, because pairing across it would move an init
        // question earlier than the program asks it.
        let mut names = Interner::new();
        let (func, check) = plane_checks(&mut names, Between::Another);
        assert!(partner(&func, check).is_none());
    }

    #[test]
    fn the_judgement_a_type_check_names_is_the_one_about_the_planes() {
        // J1 rather than a judgement of its own. Document 04 section 4.4's first judgement is an
        // access the capability, the planes or the alignment did not permit, and both the type
        // plane and the init plane are planes, so that is the sentence the reporter should print
        // for either of them. That the two say the same thing is also why the one row the fused
        // call carries can stand for both of them.
        let mut names = Interner::new();
        let mut module = asking_the_plane(&mut names);
        lower(&mut module, &mut names);

        let rows: Vec<u8> = module
            .globals()
            .map(|id| {
                let init = module[id].init.expect("a descriptor is a definition");
                match module[init][0] {
                    Datum::Scalar { value, .. } => {
                        u8::try_from(module[value].bits()).expect("a judgement is one byte")
                    }
                    _ => panic!("a descriptor starts with its judgement"),
                }
            })
            .collect();
        assert_eq!(rows, [ACCESS, ACCESS, ACCESS]);
    }

    #[test]
    fn a_store_becomes_the_calls_that_record_what_it_wrote() {
        // Two operands and no descriptor for the init plane's call, against three for the type
        // plane's. A type number is the one thing the two writes do not have in common: what a
        // store stored through is a thing the compiler has to name, and that it stored at all is
        // not.
        let mut names = Interner::new();
        let mut module = stored(&mut names);
        assert_eq!(lower(&mut module, &mut names), 2);

        let id = module.funcs().next().expect("the module has one function");
        let printed = print_func(&module, &module[id], &names);
        assert!(
            printed.contains("call @__rucc_meta_type(%0, %7, %8) : (ptr, i64, i32)\n"),
            "{printed}"
        );
        assert!(printed.contains("call @__rucc_meta_init(%0, %9) : (ptr, i64)\n"), "{printed}");

        if let Err(errors) = verify_func(&module, &module[id], &names) {
            panic!("that was expected to be believed: {errors:#?}");
        }
    }

    #[test]
    fn a_store_of_a_pointer_becomes_the_call_that_says_which_thread_wrote_it() {
        // Two operands and no descriptor, which is the same shape the init plane's write has. A
        // plane write refuses nothing, so there is no row to point at, and neither the thread nor
        // its count is something this pass could pass in: both are facts about the moment the
        // program reaches the call, so the runtime reads them for itself.
        let mut names = Interner::new();
        let mut module = racing(&mut names);
        lower(&mut module, &mut names);

        let id = module.funcs().next().expect("the module has one function");
        let printed = print_func(&module, &module[id], &names);
        assert!(printed.contains("call @__rucc_meta_epoch(%0, %"), "{printed}");
        assert!(printed.contains(") : (ptr, i64)\n"), "{printed}");

        if let Err(errors) = verify_func(&module, &module[id], &names) {
            panic!("that was expected to be believed: {errors:#?}");
        }
    }

    #[test]
    fn a_store_of_a_pointer_also_becomes_the_call_that_asks_who_was_there_first() {
        // Three operands and a descriptor, which is the shape the init plane's check has: the plane
        // holds stamps rather than types, so a range is the whole of the question. The descriptor
        // says J9 rather than J1, and that number is the whole of what makes the report read as a
        // race, since the reporter takes the judgement out of the row it was handed.
        let mut names = Interner::new();
        let mut module = racing(&mut names);
        lower(&mut module, &mut names);

        let id = module.funcs().next().expect("the module has one function");
        let printed = print_func(&module, &module[id], &names);
        assert!(printed.contains("call @__rucc_check_race(%0, %"), "{printed}");
        assert!(printed.contains(") : (ptr, i64, ptr)\n"), "{printed}");

        // In front of the store, and the recording behind it. The recording overwrites the stamp
        // the check reads, so the two in the other order would have the check asking about the
        // write it was called for.
        let asked = printed.find("__rucc_check_race").expect("the check lowered");
        let stamp = printed.find("__rucc_meta_epoch").expect("so did the recording");
        assert!(asked < stamp, "{printed}");

        let rows: Vec<u8> = module
            .globals()
            .map(|id| {
                let init = module[id].init.expect("a descriptor is a definition");
                match module[init][0] {
                    Datum::Scalar { value, .. } => {
                        u8::try_from(module[value].bits()).expect("a judgement is one byte")
                    }
                    _ => panic!("a descriptor starts with its judgement"),
                }
            })
            .collect();
        assert_eq!(rows, [ACCESS, ACCESS, RACE]);

        if let Err(errors) = verify_func(&module, &module[id], &names) {
            panic!("that was expected to be believed: {errors:#?}");
        }
    }

    #[test]
    fn a_copy_becomes_the_calls_that_move_the_planes_across() {
        // Three operands each and no descriptor of their own. A plane write refuses nothing, and
        // neither what the copied bytes are nor whether anything ever wrote them is a thing the
        // compiler knows, so there is no type number and no length beyond the range: all three
        // calls read what is over the source and write it over the destination. The third is the
        // aux rather than a plane, and it is here because the capability of a pointer inside a
        // structure being copied whole travels the same way its type and its init do.
        //
        // The four descriptors the count reports are the range and lifetime checks in front, one
        // per end of the copy, which are the accesses the plane writes are not.
        let mut names = Interner::new();
        let mut module = copied(&mut names);
        assert_eq!(lower(&mut module, &mut names), 4);

        let id = module.funcs().next().expect("the module has one function");
        assert_eq!(
            print_func(&module, &module[id], &names),
            "func @move(ptr, ptr), linkage(external) {\n\
             block0(%0: ptr, %1: ptr):\n    \
             %2 = alloca, size 32, align 8\n    \
             %3 = alloca, size 32, align 8\n    \
             call @__rucc_cap_recover(%3, %1) : (ptr, ptr)\n    \
             call @__rucc_cap_recover(%2, %0) : (ptr, ptr)\n    \
             %4 = global_addr @__rucc_safety_desc_0\n    \
             %5 = iconst.i64 24\n    \
             %6 = iconst.i64 8\n    \
             call @__rucc_check_bounds(%0, %5, %6, %2, %4) : (ptr, i64, i64, ptr, ptr)\n    \
             %7 = global_addr @__rucc_safety_desc_1\n    \
             call @__rucc_check_live(%2, %0, %7) : (ptr, ptr, ptr)\n    \
             %8 = global_addr @__rucc_safety_desc_2\n    \
             %9 = iconst.i64 24\n    \
             %10 = iconst.i64 8\n    \
             call @__rucc_check_bounds(%1, %9, %10, %3, %8) : (ptr, i64, i64, ptr, ptr)\n    \
             %11 = global_addr @__rucc_safety_desc_3\n    \
             call @__rucc_check_live(%3, %1, %11) : (ptr, ptr, ptr)\n    \
             memcpy %0, %1, size 24, align 8\n    \
             %12 = iconst.i64 24\n    \
             call @__rucc_meta_type_copy(%0, %1, %12) : (ptr, ptr, i64)\n    \
             %13 = iconst.i64 24\n    \
             call @__rucc_meta_init_copy(%0, %1, %13) : (ptr, ptr, i64)\n    \
             %14 = iconst.i64 24\n    \
             call @__rucc_cap_copy(%0, %1, %14) : (ptr, ptr, i64)\n    \
             return\n\
             }\n"
        );

        if let Err(errors) = verify_func(&module, &module[id], &names) {
            panic!("that was expected to be believed: {errors:#?}");
        }
    }

    /// The alignment is the access's own and not its width.
    ///
    /// Two numbers that are four apiece in [`checked`] and would look alike if only one of them
    /// went through. The access here is four bytes wide and may assume nothing about where it
    /// starts, which is what the front end says about a member of a packed record, and what has
    /// to arrive at the runtime is four bytes and an alignment of one.
    #[test]
    fn the_alignment_that_goes_through_is_the_one_the_access_may_assume() {
        let mut names = Interner::new();
        let mut module = unaligned(&mut names);
        assert_eq!(lower(&mut module, &mut names), 3);

        let id = module.funcs().next().expect("the module has one function");
        let printed = print_func(&module, &module[id], &names);
        assert!(printed.contains("%3 = iconst.i64 4\n"), "{printed}");
        assert!(printed.contains("%4 = iconst.i64 1\n"), "{printed}");
        assert!(
            printed.contains(
                "call @__rucc_check_bounds(%0, %3, %4, %1, %2) : (ptr, i64, i64, ptr, ptr)\n"
            ),
            "{printed}"
        );
    }

    #[test]
    fn every_check_becomes_a_call_carrying_the_descriptor_it_is_described_by() {
        let mut names = Interner::new();
        let mut module = checked(&mut names);
        assert_eq!(lower(&mut module, &mut names), 3);

        let id = module.funcs().next().expect("the module has one function");
        assert_eq!(
            print_func(&module, &module[id], &names),
            "func @read(ptr) -> i32, linkage(external) {\n\
             block0(%0: ptr):\n    \
             %1 = alloca, size 32, align 8\n    \
             call @__rucc_cap_recover(%1, %0) : (ptr, ptr)\n    \
             %2 = global_addr @__rucc_safety_desc_0\n    \
             %3 = iconst.i64 4\n    \
             %4 = iconst.i64 4\n    \
             call @__rucc_check_bounds(%0, %3, %4, %1, %2) : (ptr, i64, i64, ptr, ptr)\n    \
             %5 = global_addr @__rucc_safety_desc_1\n    \
             call @__rucc_check_live(%1, %0, %5) : (ptr, ptr, ptr)\n    \
             %6 = global_addr @__rucc_safety_desc_2\n    \
             %7 = iconst.i64 4\n    \
             call @__rucc_check_init(%0, %7, %6) : (ptr, i64, ptr)\n    \
             %8 = load.i32 %0, size 4, align 4\n    \
             return %8\n\
             }\n"
        );
    }

    #[test]
    fn the_capabilities_the_checks_were_reading_are_taken_out() {
        // A `cap` is a type nothing in the back end has been taught, so one left behind is a
        // compilation that fails rather than a value nobody reads.
        let mut names = Interner::new();
        let mut module = checked(&mut names);
        lower(&mut module, &mut names);

        let id = module.funcs().next().expect("the module has one function");
        let func = &module[id];
        let left: Vec<Opcode> = func
            .blocks()
            .flat_map(|block| func.insts(block).collect::<Vec<_>>())
            .map(|inst| func[inst].opcode)
            .collect();
        assert!(!left.contains(&Opcode::CapOf), "{left:?}");
    }

    #[test]
    fn what_it_produces_is_a_module_the_verifier_believes() {
        let mut names = Interner::new();
        let mut module = checked(&mut names);
        lower(&mut module, &mut names);

        let id = module.funcs().next().expect("the module has one function");
        if let Err(errors) = verify_func(&module, &module[id], &names) {
            panic!("that was expected to be believed: {errors:#?}");
        }
    }

    #[test]
    fn the_section_is_one_descriptor_per_check_and_nothing_else() {
        // The runtime is handed one address and dereferences it, so what makes the section a table
        // is only that every variable in it is the same sixteen bytes long and eight aligned. That
        // is what `--emit=safety-summary` will divide by, so it is checked here rather than
        // assumed.
        let mut names = Interner::new();
        let mut module = checked(&mut names);
        let rows = lower(&mut module, &mut names);

        let globals: Vec<_> = module.globals().collect();
        assert_eq!(globals.len(), rows);
        for (index, id) in globals.iter().enumerate() {
            let desc = &module[*id];
            assert_eq!(names.resolve(desc.name), label(index));
            assert_eq!(
                names.resolve(desc.section.expect("a descriptor names its section")),
                SECTION
            );
            assert_eq!(desc.linkage, Linkage::Internal);
            assert!(desc.constant);
            assert_eq!(desc.align, 8);
            assert_eq!(desc.size, WIDTH);

            // The image has to add up to the size, or the descriptor after this one starts in the
            // middle of this one.
            let init = desc.init.expect("a descriptor is a definition");
            let written: u64 = module[init].iter().map(|datum| datum.size(&module)).sum();
            assert_eq!(written, WIDTH);
        }
    }

    #[test]
    fn the_judgement_a_descriptor_names_is_the_one_the_check_decides() {
        // A report that said J1 where the program derived a pointer would send somebody looking
        // at the wrong line, so the two rows a derivation produces are checked by hand.
        let mut names = Interner::new();
        let mut func = Func::new(
            names.intern("walk"),
            Signature::new().with_params(&[Type::PTR, Type::int(64)]).with_returns(&[Type::PTR]),
        );
        let entry = func.create_block();
        let p = func.append_param(entry, Type::PTR);
        let n = func.append_param(entry, Type::int(64));
        let mut b = Builder::new(&mut func, entry);
        let args = b.func().push_values(&[p, n]);
        let moved = b.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR);
        b.ret(&[moved]);
        let (plane, numbers) = planeless(&mut names);
        insert(&mut func, &plane, 8, Subobject::Off, Promise::Off, Races::Off);

        let mut table = Vec::new();
        calls(&mut func, &mut names, Type::int(64), &numbers, &mut table);
        assert_eq!(table, [Descriptor { judgement: DERIVE, class: 0, size: 0 }]);
    }

    #[test]
    fn a_check_over_a_length_the_program_worked_out_passes_that_length_along() {
        // Section 7.4's hoisted check. The number of bytes is the third operand rather than the
        // payload's size, so what the call is handed is the value and not a constant, and the
        // descriptor says zero because there is no one width to report.
        let mut names = Interner::new();
        let mut func = Func::new(
            names.intern("sweep"),
            Signature::new().with_params(&[Type::PTR, Type::int(64)]),
        );
        let entry = func.create_block();
        let p = func.append_param(entry, Type::PTR);
        let n = func.append_param(entry, Type::int(64));
        let info = MemInfo {
            size: 4,
            align: 4,
            order: MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        };
        let mut b = Builder::new(&mut func, entry);
        let of = b.unary(Opcode::CapOf, p, Type::CAP);
        let args = b.func().push_values(&[of, p, n]);
        let extra = Extra::Mem(b.func().add_mem(info));
        b.inst(InstData { args, extra, ..InstData::new(Opcode::CheckBounds) }, &[]);
        b.ret(&[]);

        let mut table = Vec::new();
        let numbers = planeless(&mut names).1;
        calls(&mut func, &mut names, Type::int(64), &numbers, &mut table);
        assert_eq!(table, [Descriptor { judgement: ACCESS, class: 0, size: 0 }]);

        let mut module = Module::new(names.intern("sweep.c"), &target());
        module.add_func(func);
        let id = module.funcs().next().expect("the module has one function");
        assert_eq!(
            print_func(&module, &module[id], &names),
            "func @sweep(ptr, i64), linkage(external) {\n\
             block0(%0: ptr, %1: i64):\n    \
             %2 = alloca, size 32, align 8\n    \
             call @__rucc_cap_recover(%2, %0) : (ptr, ptr)\n    \
             %3 = global_addr @__rucc_safety_desc_0\n    \
             %4 = iconst.i64 1\n    \
             call @__rucc_check_bounds(%0, %1, %4, %2, %3) : (ptr, i64, i64, ptr, ptr)\n    \
             return\n\
             }\n"
        );
    }

    #[test]
    fn a_plane_check_over_a_length_the_program_worked_out_calls_the_range_form() {
        // What `rucc_opt::hoist` writes in front of a loop, for the two planes. Each goes to the
        // `_range` entry point, which sweeps the plane a word at a time, and the two are not made
        // one call, because `partner` pairs the checks of one access and these are not that.
        let mut names = Interner::new();
        let mut module = Module::new(names.intern("sweep.c"), &target());
        let root = names.intern("char");
        let root =
            module.add_meta(MetaNode::Tbaa(TbaaNode { name: root, parent: None, offset: 0 }));
        let int = names.intern("int");
        let int =
            module.add_meta(MetaNode::Tbaa(TbaaNode { name: int, parent: Some(root), offset: 0 }));
        let plane = Plane::build(&mut module);
        let numbers = plane::numbers(&module, &names);

        let mut func = Func::new(
            names.intern("sweep"),
            Signature::new().with_params(&[Type::PTR, Type::int(64)]),
        );
        let entry = func.create_block();
        let p = func.append_param(entry, Type::PTR);
        let n = func.append_param(entry, Type::int(64));
        let info = MemInfo {
            size: 4,
            align: 4,
            order: MemOrder::NotAtomic,
            tbaa: Some(plane.entry(Some(int))),
            owns: 0,
            restrict: Restrict::NONE,
        };
        let mut b = Builder::new(&mut func, entry);
        let of = b.unary(Opcode::CapOf, p, Type::CAP);
        marker(&mut b, Opcode::CheckType, Some(info), &[of, p, n]);
        marker(&mut b, Opcode::CheckInit, Some(info), &[of, p, n]);
        b.ret(&[]);

        let mut table = Vec::new();
        calls(&mut func, &mut names, Type::int(64), &numbers, &mut table);
        assert_eq!(table.len(), 2, "one row for each of the two checks");

        module.add_func(func);
        let id = module.funcs().next().expect("the module has one function");
        let printed = print_func(&module, &module[id], &names);
        assert!(printed.contains("call @__rucc_check_type_range(%0, %1, "), "{printed}");
        assert!(printed.contains("call @__rucc_check_init_range(%0, %1, "), "{printed}");
        assert!(!printed.contains("typed_init"), "{printed}");
    }

    #[test]
    fn a_plane_check_over_a_wide_constant_calls_the_range_form_and_a_narrow_one_does_not() {
        // The other way `rucc_opt::hoist` writes a walk, with the trip count known here, so the
        // width is a constant in the payload rather than an operand. The pair is one access's as
        // far as `partner` is concerned, so it is made one call, and that call is the range form
        // once the width reaches a word of init shadow.
        for (size, routine) in
            [(RANGE, "__rucc_check_typed_init_range"), (RANGE - 1, "__rucc_check_typed_init")]
        {
            let mut names = Interner::new();
            let mut module = Module::new(names.intern("wide.c"), &target());
            let root = names.intern("char");
            let root =
                module.add_meta(MetaNode::Tbaa(TbaaNode { name: root, parent: None, offset: 0 }));
            let int = names.intern("int");
            let int = module.add_meta(MetaNode::Tbaa(TbaaNode {
                name: int,
                parent: Some(root),
                offset: 0,
            }));
            let plane = Plane::build(&mut module);
            let numbers = plane::numbers(&module, &names);

            let mut func =
                Func::new(names.intern("wide"), Signature::new().with_params(&[Type::PTR]));
            let entry = func.create_block();
            let p = func.append_param(entry, Type::PTR);
            let info = MemInfo {
                size,
                align: 4,
                order: MemOrder::NotAtomic,
                tbaa: Some(plane.entry(Some(int))),
                owns: 0,
                restrict: Restrict::NONE,
            };
            let mut b = Builder::new(&mut func, entry);
            let of = b.unary(Opcode::CapOf, p, Type::CAP);
            marker(&mut b, Opcode::CheckType, Some(info), &[of, p]);
            marker(&mut b, Opcode::CheckInit, Some(info), &[of, p]);
            b.ret(&[]);

            let mut table = Vec::new();
            calls(&mut func, &mut names, Type::int(64), &numbers, &mut table);

            module.add_func(func);
            let id = module.funcs().next().expect("the module has one function");
            let printed = print_func(&module, &module[id], &names);
            assert!(printed.contains(&format!("call @{routine}(%0, ")), "{size}: {printed}");
            assert!(!printed.contains("__rucc_check_init"), "{size}: {printed}");
        }
    }

    #[test]
    fn a_length_wider_than_the_word_is_cut_down_to_it() {
        // The pass that works out how many bytes a loop covers has no target to ask, so on a
        // thirty two bit target it hands over a number that does not fit the runtime's own
        // parameter. What comes out is a truncation rather than a call the verifier refuses.
        let mut names = Interner::new();
        let mut func = Func::new(
            names.intern("sweep"),
            Signature::new().with_params(&[Type::PTR, Type::int(64)]),
        );
        let entry = func.create_block();
        let p = func.append_param(entry, Type::PTR);
        let n = func.append_param(entry, Type::int(64));
        let info = MemInfo {
            size: 4,
            align: 4,
            order: MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        };
        let mut b = Builder::new(&mut func, entry);
        let of = b.unary(Opcode::CapOf, p, Type::CAP);
        let args = b.func().push_values(&[of, p, n]);
        let extra = Extra::Mem(b.func().add_mem(info));
        b.inst(InstData { args, extra, ..InstData::new(Opcode::CheckBounds) }, &[]);
        b.ret(&[]);

        let mut table = Vec::new();
        let numbers = planeless(&mut names).1;
        calls(&mut func, &mut names, Type::int(32), &numbers, &mut table);
        let opcodes: Vec<Opcode> = func
            .blocks()
            .flat_map(|block| func.insts(block).collect::<Vec<_>>())
            .map(|inst| func[inst].opcode)
            .collect();
        assert!(opcodes.contains(&Opcode::Trunc), "{opcodes:?}");
    }

    /// A function that asks how many bytes its parameter covers, in the width the caller names.
    ///
    /// The result is returned so that something reads it, because a query nobody reads would be
    /// removed by any pass that ran and this test is about what the value it produces turns into.
    fn asking(names: &mut Interner, ty: Type) -> Func {
        let mut func = Func::new(
            names.intern("cover"),
            Signature::new().with_params(&[Type::PTR, ty]).with_returns(&[ty]),
        );
        let entry = func.create_block();
        let p = func.append_param(entry, Type::PTR);
        let want = func.append_param(entry, ty);
        let mut b = Builder::new(&mut func, entry);
        let of = b.unary(Opcode::CapOf, p, Type::CAP);
        let args = b.func().push_values(&[of, p, want]);
        let got = b.value(InstData { args, ..InstData::new(Opcode::CapExtent) }, ty);
        b.ret(&[got]);
        func
    }

    #[test]
    fn the_extent_query_becomes_a_call_that_carries_no_descriptor() {
        // The one rewrite here that is not a judgement, so it writes no row and the table stays
        // empty. What it is for is section 7.4's split, which needs a number and not a verdict.
        let mut names = Interner::new();
        let mut func = asking(&mut names, Type::int(64));

        let mut table = Vec::new();
        let numbers = planeless(&mut names).1;
        calls(&mut func, &mut names, Type::int(64), &numbers, &mut table);
        assert!(table.is_empty(), "{table:?}");

        let mut module = Module::new(names.intern("cover.c"), &target());
        module.add_func(func);
        let id = module.funcs().next().expect("the module has one function");
        assert_eq!(
            print_func(&module, &module[id], &names),
            "func @cover(ptr, i64) -> i64, linkage(external) {\n\
             block0(%0: ptr, %1: i64):\n    \
             %2 = call @__rucc_extent(%0, %1) : (ptr, i64) -> i64\n    \
             return %2\n\
             }\n"
        );
        if let Err(errors) = verify_func(&module, &module[id], &names) {
            panic!("that was expected to be believed: {errors:#?}");
        }
    }

    #[test]
    fn an_extent_asked_for_in_a_width_the_target_does_not_have_is_converted_back() {
        // On a thirty two bit target the runtime's own parameter and return are thirty two bits
        // wide, and the pass that wrote the arithmetic worked in sixty four. So the limit is cut
        // down on the way in and the answer is widened on the way out, and everything reading the
        // query still reads a value of the type it had.
        let mut names = Interner::new();
        let mut func = asking(&mut names, Type::int(64));

        let mut table = Vec::new();
        let numbers = planeless(&mut names).1;
        calls(&mut func, &mut names, Type::int(32), &numbers, &mut table);
        let opcodes: Vec<Opcode> = func
            .blocks()
            .flat_map(|block| func.insts(block).collect::<Vec<_>>())
            .map(|inst| func[inst].opcode)
            .collect();
        assert!(opcodes.contains(&Opcode::Trunc), "the limit goes in narrowed: {opcodes:?}");
        assert!(opcodes.contains(&Opcode::ZExt), "and the answer comes back widened: {opcodes:?}");
        assert!(
            !opcodes.contains(&Opcode::CapExtent),
            "with nothing left of the query: {opcodes:?}"
        );
    }

    /// A module holding one function that declares a region and does nothing else.
    fn exempt(names: &mut Interner) -> Module {
        let reason = names.intern("hand written assembly, checked by review");
        let mut func = Func::new(names.intern("driver"), Signature::new());
        let entry = func.create_block();
        let mut b = Builder::new(&mut func, entry);
        b.inst(
            InstData { extra: Extra::Reason(reason), ..InstData::new(Opcode::SafeRegionBegin) },
            &[],
        );
        b.inst(InstData::new(Opcode::SafeRegionEnd), &[]);
        b.ret(&[]);
        let mut module = Module::new(names.intern("driver.c"), &target());
        module.add_func(func);
        module
    }

    #[test]
    fn the_markers_around_a_declared_region_lower_into_nothing_at_all() {
        // The only pair here that becomes no call. A region is the reason some code carries no
        // checks rather than something the code does, so once `crate::summary` has counted it
        // there is nothing left for the back end to be handed.
        let mut names = Interner::new();
        let mut module = exempt(&mut names);
        assert_eq!(lower(&mut module, &mut names), 0);

        let id = module.funcs().next().expect("the module has one function");
        assert_eq!(
            print_func(&module, &module[id], &names),
            "func @driver(), linkage(external) {\n\
             block0:\n    \
             return\n\
             }\n"
        );

        if let Err(errors) = verify_func(&module, &module[id], &names) {
            panic!("that was expected to be believed: {errors:#?}");
        }
    }

    #[test]
    fn a_region_costs_the_object_file_no_descriptor_either() {
        // A descriptor describes a failure and a marker cannot fail, so a build made of nothing but
        // declared regions has an empty section and gets none.
        let mut names = Interner::new();
        let mut module = exempt(&mut names);
        lower(&mut module, &mut names);
        assert_eq!(module.globals().count(), 0);
    }

    #[test]
    fn a_module_with_nothing_to_check_gets_no_section_at_all() {
        // An object with an empty section in it is an object that says the compiler had something
        // to say and did not say it.
        let mut names = Interner::new();
        let mut module = Module::new(names.intern("empty.c"), &target());
        assert_eq!(lower(&mut module, &mut names), 0);
        assert_eq!(module.globals().count(), 0);
    }
}
