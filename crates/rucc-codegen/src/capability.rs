//! What this target can be asked to do, and what happens when it cannot.
//!
//! Design: `spec/optimizer/36-lowering-and-isel.md` section 36.4.
//!
//! GCC asks one question of a target, which is whether it has an instruction for this operation at
//! this mode, and the whole of `gcc/optabs.cc` is built on the answer. rucc asks the same question
//! and used to give three separate answers in three separate places: a list in [`crate::coverage`]
//! saying an opcode has no rule and that is on purpose, a set of `match` arms in
//! [`crate::quad`] and [`crate::wide`] turning an operation into a call to the compiler runtime,
//! and the pre-selection group in [`crate::lowering`] rewriting an operation into ones the machine
//! does have.
//!
//! Those are not three questions. They are one question with three possible answers, and three
//! places that answer it can disagree without anything noticing. An opcode named on the exception
//! list and also lowered before selection is a stale line in the list; an opcode named there that
//! has grown a libcall is the same staleness the other way round. This module is the one table, and
//! [`Row`] is the three columns.
//!
//! # A row is a name
//!
//! Section 36.4 says one row per operation and mode. A name is exactly that here, and it is already
//! how the rest of the back end talks: `add.i64` is the addition opcode at sixty four bits, and
//! [`crate::coverage`] has always said that a name is an opcode and a width together. So the rows
//! are the names, which come from three places.
//!
//! [`rucc_ir::term::heads`] gives every name the rule language can spell, which is the universe the
//! rule column is about. [`LIBCALLS`] gives the names at the two modes the rule language cannot
//! spell, which is the whole reason those calls exist. And an opcode with no mode at all, a `call`
//! or a `jump` or a `trap`, gets one row under its own name, because the question is still asked
//! about it and the answer is still one of the three.
//!
//! # Why more than one column can be filled
//!
//! An operation is not obliged to have exactly one answer, and reading it that way is the mistake
//! the old list made. `sitofp.i64.f64` has a rule, because this machine has `cvtsi2sd`, and it is
//! also named by [`crate::lowering::Step::Floats`], because the same pass handles the widths the
//! machine has no instruction for and walks away from the ones it does. Both columns are true and
//! neither is stale. What would be a contradiction is a rule at a name something rewrites by hand
//! before selection ever runs, since that rule could never fire, and that is the one overlap the
//! tests below refuse.
//!
//! # Which way the arrow points
//!
//! The lowering column is not written down here. It is read out of [`crate::lowering::Step`], which
//! is where a lowering says which opcodes it is about, so there is no second copy of the group
//! membership to go stale. That is the only direction worth having: the group is the authority on
//! what the group rewrites, and a table that repeated it would be the third mechanism again under a
//! new name.
//!
//! What is written down here is the half no pass can be asked for. [`HAND`] is the opcodes lowered
//! somewhere a rule cannot reach and a pass cannot be asked about either, because the answer is a
//! `match` arm in [`crate::lower`] or in `rucc_safety`, and [`LIBCALLS`] is the runtime function an
//! operation becomes, which was three sets of `match` arms and is now one list they read.

use rucc_ir::Opcode;

use crate::coverage::{GAPS, NAMES};
use crate::lowering::Step;
use crate::select::Table;

/// What rewrites an operation before the selector sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lowering {
    /// A member of the pre-selection group, which says for itself which opcodes it is about.
    Group(Step),
    /// A place a rule cannot reach and a pass cannot be asked about, and what it does there.
    Hand(&'static str),
}

impl Lowering {
    /// Where the rewrite happens, for a report somebody reads.
    #[must_use]
    pub const fn where_(self) -> &'static str {
        match self {
            Lowering::Group(step) => step.name(),
            Lowering::Hand(where_) => where_,
        }
    }
}

/// One operation at one mode, and the three answers.
#[derive(Debug, Clone)]
pub struct Row {
    /// The opcode the row is about.
    pub opcode: Opcode,
    /// What the operation at this mode is called, which is `add.i64` or `sitofp.i64.f64` or, for an
    /// operation with no mode, the opcode's own name.
    pub name: &'static str,
    /// Whether a rule in this target's table is written at that name.
    pub rule: bool,
    /// What rewrites it before selection, if anything does.
    pub lowering: Option<Lowering>,
    /// The runtime function it becomes, if that is what happens to it.
    pub libcall: Option<&'static str>,
    /// Why nothing does any of the three, and the issue that closes it.
    pub nothing: Option<(&'static str, &'static str)>,
}

impl Row {
    /// Whether this target can be asked to do this operation at this mode at all.
    ///
    /// The question the whole table is for. A row with no answer is an operation that reaches the
    /// selector and is refused there, which turns a hole in the back end into a user's problem.
    #[must_use]
    pub const fn answered(&self) -> bool {
        self.rule || self.lowering.is_some() || self.libcall.is_some()
    }
}

/// An opcode lowered somewhere a rule cannot reach, and what happens to it there.
///
/// Not one of these is a gap. Each is an opcode whose lowering depends on something no pattern can
/// see, so the answer lives where that something is known: where a call's operands go depends on
/// the signature, where a local lives depends on the frame, an unconditional jump is an edge and
/// edges live on the block.
///
/// This used to be twice as long, and the half that went is the half the pre-selection group can be
/// asked about directly. An entry here is one nothing can be asked about, because what does the
/// work is a `match` arm rather than a pass with a name.
pub static HAND: &[(Opcode, &str)] = &[
    // The convention. What a call's operands are is whatever the signature made them, and which
    // register each one arrives in depends on the classification of every argument before it.
    (Opcode::Call, "`crate::abi`, which builds a call out of the convention"),
    (Opcode::CallIndirect, "`crate::abi`, the same instruction with the callee in a register"),
    // The frame, which is not known until the allocator has finished running out of registers.
    (Opcode::Alloca, "`crate::lower`, as an address into a frame `crate::frame` lays out later"),
    // The stack pointer, which is not a value the program computed and so is not a value a rule
    // could bind. A scope holding a variable length array reads it as it opens and writes it back
    // as it closes, which is how the bytes are given back.
    (Opcode::StackSave, "`crate::lower`, as a move out of the stack pointer"),
    (Opcode::StackRestore, "`crate::lower`, the same move the other way round"),
    // A relocation, which is right because of what the linker does rather than because of what
    // any bitvector equals.
    (Opcode::GlobalAddr, "`crate::lower`, a `lea` off the instruction pointer with a name on it"),
    // The same instruction against a place in this function rather than a name outside it. What
    // it addresses is a block, and a block is not a value a pattern can bind.
    (Opcode::BlockAddr, "`crate::lower`, the same `lea` against a label of this function"),
    // The one thing on this machine that no ordinary instruction can work out, which is why it
    // is built here rather than matched: `%fs` is not a register a rule could name.
    (Opcode::ThreadPointer, "`crate::lower`, as the load through `%fs` at zero that reads it"),
    // What a named register holds, built there for a reason of the same shape written about any
    // register rather than about one: which register it is is a string beside the instruction and
    // a pattern matches on an opcode and a type, so no rule could name it.
    (Opcode::RegisterValue, "`crate::lower`, as one move out of the register the program named"),
    // A hint, which is built here for a reason of the same shape and one step stronger: which of
    // the four instructions it is comes out of a number in the builtin's arguments, and a pattern
    // matches on an opcode and a type and could not see it.
    (Opcode::Prefetch, "`crate::lower`, as one of the four `prefetch` instructions"),
    // Stopping, which is built here because it computes nothing for a rule to have a pattern for
    // and because what makes it right is the operating system rather than any bitvector.
    (Opcode::Trap, "`crate::lower`, as the `ud2` the program stops on"),
    // The two that walk the frames, built here because how long the walk is comes out of a number
    // beside the instruction and a pattern matches on an opcode and a type. What they start from is
    // the frame pointer, which is not a register a rule could name either, and asking for one is
    // part of building them.
    (Opcode::FrameAddress, "`crate::lower`, as the walk up the saved frame pointers"),
    (Opcode::ReturnAddress, "`crate::lower`, as the same walk with one load at the end of it"),
    // The pair that saves a place in a function and comes back to it, built here because what the
    // first of them writes down is where control comes back to, which is a place in this function
    // and not a value a pattern can bind. Each is a group of instructions rather than one, and the
    // first of them ends the block it was written in, which no rule can do.
    (
        Opcode::SetjmpMarker,
        "`crate::lower`, as the four words it writes and the block the restore comes back to",
    ),
    (
        Opcode::LongjmpMarker,
        "`crate::lower`, as the four words read back, the frame put back and the jump",
    ),
    // No instruction at all. The IR keeps the width the same and the machine has one register
    // file for both, so the value is already where it needs to be.
    (Opcode::PtrToInt, "`crate::lower`, which renames the value rather than computing anything"),
    (Opcode::IntToPtr, "`crate::lower`, the same rename the other way round"),
    // Memory SSA, which is built at -O2, read by the passes that need it, and taken back off
    // before selection. Nothing in the back end has ever seen a value of type `mem`.
    (Opcode::MemEntry, "nothing at all, since memory SSA comes off before the back end runs"),
    // The edges and the two ways of writing down that control does not arrive.
    (Opcode::Jump, "`crate::layout`, since an edge is on the block and not in the block"),
    // The one terminator selection does write, because what it reads is a value. How many arms it
    // has is not fixed, and a rule says what an instruction reads rather than where a block goes.
    (Opcode::IndirectBr, "`crate::lower`, as the jump through the register that holds the address"),
    (Opcode::Unreachable, "nothing at all, which is the answer for a place control does not reach"),
    (Opcode::UnreachableHint, "nothing at all, for the same reason"),
    // The template, which is a string and not a term. A rule set cannot be written over a string,
    // so the instructions a template names are looked up in the machine description rather than
    // matched, which is `rucc_target::x86_64::read`.
    (
        Opcode::InlineAsm,
        "`crate::lower`, as the places its operands share and the instructions its template names",
    ),
    // The barrier itself, which is one instruction or none and neither is a rewrite of anything.
    (
        Opcode::Fence,
        "`crate::lower`, as an `mfence` at the strongest ordering and nothing below it",
    ),
    // The compare and exchange, which is one instruction and produces two values, and a rule
    // replaces a term with an instruction producing one.
    (
        Opcode::Cmpxchg,
        "`crate::lower`, as a locked compare and exchange and the byte that reads its answer",
    ),
    // The read modify write, which produces one value a rule could have named and whose operation
    // is carried beside it rather than in the head a rule matches on, so one pattern would be all
    // thirteen of them. `crate::lowering::Step::Retries` is the half of this the group does and it
    // names no opcode, because the half it does not do is lowered here.
    (
        Opcode::AtomicRmw,
        "`crate::lower`, as an exchange or a locked add, and `crate::retry` for the eight with no \
         instruction, with the two on floating values refused",
    ),
    // The one of the five variable argument opcodes the group does not name, because what it writes
    // is the register save area and where that is comes out of the convention rather than the term.
    (Opcode::VaStart, "`crate::varargs`, which writes the register save area the ABI describes"),
    // Memory safety. A check is a call to the runtime, and the rewrite happens after the optimizer
    // has run so that the descriptor table only has rows for checks that survived it.
    (Opcode::CheckBounds, "`rucc_safety::lower`, into a call carrying the row that describes it"),
    (Opcode::CheckLive, "`rucc_safety::lower`, the same call over the lifetime plane"),
    (Opcode::CheckDeriv, "`rucc_safety::lower`, the same call where the pointer is computed"),
    (Opcode::CheckType, "`rucc_safety::lower`, the same call, carrying the type asked about"),
    (
        Opcode::CheckInit,
        "`rucc_safety::lower`, the same call over the init plane, carrying no type",
    ),
    (Opcode::CheckRace, "`rucc_safety::lower`, the same call over the epoch plane"),
    (
        Opcode::CheckFree,
        "`rucc_safety::lower`, the same call in front of the free rather than the access",
    ),
    // The five plane writes the same pass emits, which become calls the same way. A judgement
    // decides nothing, so none of the calls carries a descriptor row, and neither do the two
    // edges below them.
    (Opcode::MetaType, "`rucc_safety::lower`, into the call that records what a store stored"),
    (Opcode::MetaTypeCopy, "`rucc_safety::lower`, the same call over the range a copy read"),
    (Opcode::MetaInit, "`rucc_safety::lower`, into the call that says a store wrote a range"),
    (Opcode::MetaInitCopy, "`rucc_safety::lower`, the same call over the range a copy read"),
    // The aux's own copy, which the same pass emits beside those two and which is not a plane write
    // in the sense they are: what it moves is the capability beside every pointer a copy carried.
    (Opcode::CapCopy, "`rucc_safety::lower`, the same call over the slots a copy moved"),
    (Opcode::MetaEpoch, "`rucc_safety::lower`, into the call that says which thread stored"),
    // The two halves of a synchronization edge, which are the same shape of call and are not a
    // plane write at all: what they move is a thread's own clock, which lives beside the thread.
    (
        Opcode::MetaRelease,
        "`rucc_safety::lower`, into the call that publishes this thread's clock at an atomic",
    ),
    (Opcode::MetaAcquire, "`rucc_safety::lower`, into the call that takes the other end of it"),
    // The same pair for a fence, which are the same calls with no key, since a fence orders
    // against every thread rather than against an object.
    (
        Opcode::MetaFenceRelease,
        "`rucc_safety::lower`, into the call that publishes this thread's clock to everyone",
    ),
    (
        Opcode::MetaFenceAcquire,
        "`rucc_safety::lower`, into the call that takes what any release fence published",
    ),
    // The `restrict` contract, which is judgement J8 and is the one check that records as well as
    // asks. What it records goes in a slot the block owns, and the two markers are what open and
    // close that slot, so all four are calls to the runtime the same way.
    (
        Opcode::CheckRestrictRead,
        "`rucc_safety::lower`, into the call that asks what the block has already reached",
    ),
    (Opcode::CheckRestrictWrite, "`rucc_safety::lower`, the same call, saying it wrote"),
    (Opcode::RestrictEnter, "`rucc_safety::lower`, into the call that opens the block's record"),
    (Opcode::RestrictLeave, "`rucc_safety::lower`, into the call that closes it again"),
    // The two markers, and the only pair on this list that is lowered into nothing. A declared
    // region is not code, it is the reason some code carries no checks, so by the time the back end
    // sees it the whole of its effect has already happened. What it costs is the count document 10
    // section 10.2 asks for, and `rucc_safety::summary` takes that before the back end runs.
    (Opcode::SafeRegionBegin, "`rucc_safety::lower`, into nothing, once the count has been taken"),
    (Opcode::SafeRegionEnd, "`rucc_safety::lower`, the same, which is to say nothing"),
    (Opcode::CapExtent, "`rucc_safety::lower`, into a call that asks rather than one that judges"),
    (Opcode::CapExtentBack, "`rucc_safety::lower`, the same call about the bytes below an address"),
    // The capability the checks were reading, which the same pass takes out once they are calls,
    // because a call to the runtime is handed an address and finds the rest for itself. One that
    // something does read is a slot, and the only one of those the pass can fill so far is a
    // capability for a pointer an allocator just returned, which is a load out of that instance's
    // own header rather than anything worked out from the address.
    (Opcode::CapOf, "`rucc_safety::slot`, into the header read at an allocation site or the walk"),
    // The two ends of a capability that something does read. A capability is four words of frame
    // and the value that stands for one is the slot's address, so the pair below is an `alloca`
    // with four zero words written into it and a call handed the addresses of two slots.
    (Opcode::CapNull, "`rucc_safety::slot`, into a frame slot with the bottom capability in it"),
    (Opcode::CapStore, "`rucc_safety::slot`, into the call that writes one into the aux plane"),
    // The other end of that write, which is the one capability nothing has to work out, because the
    // store that put it beside the pointer already did. So this is a call too, and it is the only
    // instruction the pass rewrites that reads a slot and fills one.
    (Opcode::CapLoad, "`rucc_safety::slot`, into the call that reads one back out again"),
    // The sub-object tier's whole mechanism, which is arithmetic on the range a capability holds
    // and is a call for the same reason the rest are: where the four words sit is the runtime's to
    // know, and a second place that agreed about it would be a second place that could stop.
    (Opcode::CapNarrow, "`rucc_safety::slot`, into the call that moves the range in"),
    // The expensive producer and the only one that always has an answer, which is why it is what a
    // pointer from outside the instrumented world falls back to. Same two arguments as the fresh
    // allocation above, since the runtime declares the pair as one shape.
    (Opcode::CapRecover, "`rucc_safety::slot`, into the call that walks the planes for one"),
    // The two ends of a call, which is where a capability stops being this function's business.
    // Neither of them is a capability instruction in the sense the five above are: one copies a
    // call's worth of them into a frame in thread local storage and publishes it, and the other
    // says there is no frame at all, which is what a callee nobody can vouch for gets.
    (Opcode::CapPublish, "`rucc_safety::frame`, into the frame a call hands its callee"),
    (Opcode::CapClear, "`rucc_safety::frame`, into the call that says there is no frame"),
    // And the reading end of the first of those two, which is the one of the three that does make a
    // capability. It is in the callee rather than in the caller and it answers whether or not there
    // was a frame, because a pointer nobody described is one to be recovered from the planes.
    (Opcode::CapArg, "`rucc_safety::frame`, into the read of the frame the caller published"),
    // And the same pair for the pointer a call gives back, which is the one value crossing a call in
    // the other direction. The writing end is in the callee and is the only thing here that writes
    // into a frame it did not make, which it may because the frame is the caller's stack and the
    // caller is waiting for it.
    (
        Opcode::CapYield,
        "`rucc_safety::frame`, into the write of the frame the caller is waiting on",
    ),
    (Opcode::CapResult, "`rucc_safety::frame`, into the read of what the callee left behind"),
    // What `__builtin_expect` said, which the pass writes onto the arms of the branch it was said
    // about before taking the instruction out, so that a hint and a profile are the same thing to
    // everything downstream of the optimizer.
    (Opcode::Expect, "`rucc_opt::expect`, which moves the hint onto the branch and removes it"),
];

/// The runtime function an operation becomes, by opcode and by mode.
///
/// The third answer, and the one GCC's fallback ladder ends at. An operation with no instruction
/// and no way of being built out of instructions is a call to the compiler runtime, and which call
/// it is is a fact about the operation and the mode and nothing else. It used to be three sets of
/// `match` arms, in [`crate::quad`], in [`crate::wide`] and in [`crate::expand`], and it is one
/// list they read.
///
/// The mode is spelled the way the rule language spells one, which is the width for an operation on
/// one type and the two widths for a conversion. Every mode here is a mode the rule language cannot
/// spell, since an operation the machine has does not become a call, which is why these names are
/// not in [`rucc_ir::term::heads`] and are rows of their own.
///
/// The bulk operations are the exception that proves it. A copy becomes a run of moves at a size
/// the lowering will take on and a call to the C library above it, so the mode is the size rather
/// than a width, and both answers are true of the same row.
pub static LIBCALLS: &[(Opcode, &str, &str)] = &[
    // The quad format, which no x86-64 instruction touches. `crate::quad` is the pass.
    (Opcode::FAdd, "f128", "__addtf3"),
    (Opcode::FSub, "f128", "__subtf3"),
    (Opcode::FMul, "f128", "__multf3"),
    (Opcode::FDiv, "f128", "__divtf3"),
    (Opcode::FNeg, "f128", "__negtf2"),
    // A comparison is one call per predicate and a test of the integer it gives back, which is why
    // the pass keeps the integer predicate beside the name and this list holds only the name.
    (Opcode::FCmp, "oeq.f128", "__eqtf2"),
    (Opcode::FCmp, "une.f128", "__netf2"),
    (Opcode::FCmp, "olt.f128", "__lttf2"),
    (Opcode::FCmp, "ole.f128", "__letf2"),
    (Opcode::FCmp, "ogt.f128", "__gttf2"),
    (Opcode::FCmp, "oge.f128", "__getf2"),
    (Opcode::FCmp, "uno.f128", "__unordtf2"),
    (Opcode::FPExt, "f32.f128", "__extendsftf2"),
    (Opcode::FPExt, "f64.f128", "__extenddftf2"),
    (Opcode::FPTrunc, "f128.f32", "__trunctfsf2"),
    (Opcode::FPTrunc, "f128.f64", "__trunctfdf2"),
    (Opcode::SIToFP, "i32.f128", "__floatsitf"),
    (Opcode::SIToFP, "i64.f128", "__floatditf"),
    (Opcode::UIToFP, "i32.f128", "__floatunsitf"),
    (Opcode::UIToFP, "i64.f128", "__floatunditf"),
    (Opcode::FPToSI, "f128.i32", "__fixtfsi"),
    (Opcode::FPToSI, "f128.i64", "__fixtfdi"),
    (Opcode::FPToUI, "f128.i32", "__fixunstfsi"),
    (Opcode::FPToUI, "f128.i64", "__fixunstfdi"),
    // The half format, which no x86-64 instruction computes in either. `crate::half` is the pass,
    // and it needs fewer rows than the quad does because it has somewhere to go: a half widens to
    // a `float` exactly, so every operation is the `float` one with a widening in front of it and
    // a narrowing behind it, and only the widening and the narrowings are calls.
    //
    // The three narrowings are three rows and not one, and that is the part worth reading twice.
    // Each of them rounds once, and rounding twice is a different answer: a `double` that sits
    // just above the halfway point between two halves rounds down to that halfway point in a
    // `float` and then to even from there, which is the wrong neighbour. libgcc has a routine per
    // source width for exactly this reason and gcc calls the one that matches, so this table has a
    // row per source width too.
    (Opcode::FPExt, "f16.f32", "__extendhfsf2"),
    (Opcode::FPTrunc, "f32.f16", "__truncsfhf2"),
    (Opcode::FPTrunc, "f64.f16", "__truncdfhf2"),
    (Opcode::FPTrunc, "f128.f16", "__trunctfhf2"),
    // An integer wider than a register. `crate::wide` splits what it can into halves and calls for
    // what it cannot, which is the four that need the whole value at once and the conversions.
    (Opcode::UDiv, "i128", "__udivti3"),
    (Opcode::SDiv, "i128", "__divti3"),
    (Opcode::URem, "i128", "__umodti3"),
    (Opcode::SRem, "i128", "__modti3"),
    (Opcode::SIToFP, "i128.f32", "__floattisf"),
    (Opcode::SIToFP, "i128.f64", "__floattidf"),
    (Opcode::SIToFP, "i128.f128", "__floattitf"),
    (Opcode::UIToFP, "i128.f32", "__floatuntisf"),
    (Opcode::UIToFP, "i128.f64", "__floatuntidf"),
    (Opcode::UIToFP, "i128.f128", "__floatuntitf"),
    (Opcode::FPToSI, "f32.i128", "__fixsfti"),
    (Opcode::FPToSI, "f64.i128", "__fixdfti"),
    (Opcode::FPToSI, "f128.i128", "__fixtfti"),
    (Opcode::FPToUI, "f32.i128", "__fixunssfti"),
    (Opcode::FPToUI, "f64.i128", "__fixunsdfti"),
    (Opcode::FPToUI, "f128.i128", "__fixunstfti"),
    // The bulk operations, which are the C library rather than the compiler runtime. A copy the
    // lowering will not take on is one whose size is not a constant or is above the threshold, and
    // a move is always a call because the two regions may overlap.
    (Opcode::Memcpy, "big", "memcpy"),
    (Opcode::Memset, "big", "memset"),
    (Opcode::Memmove, "any", "memmove"),
];

/// The runtime function this operation at this mode becomes, or nothing where it is not a call.
///
/// What the passes that emit one read, so that the name a call is made under and the name the table
/// reports are the same string rather than two strings somebody has to keep equal.
#[must_use]
pub fn libcall(opcode: Opcode, mode: &str) -> Option<&'static str> {
    LIBCALLS
        .iter()
        .find(|&&(at, spelled, _)| at == opcode && spelled == mode)
        .map(|&(_, _, name)| name)
}

/// What rewrites this opcode before selection, if anything does.
///
/// The group is asked first and answers for itself, so the membership is not written down twice.
/// [`HAND`] is what is left, which is the opcodes no pass can be asked about.
#[must_use]
pub fn lowering(opcode: Opcode) -> Option<Lowering> {
    for &step in Step::GROUP {
        if step.opcodes().contains(&opcode) {
            return Some(Lowering::Group(step));
        }
    }
    HAND.iter().find(|&&(at, _)| at == opcode).map(|&(_, where_)| Lowering::Hand(where_))
}

/// The whole table for one target's rules.
///
/// Nothing is compiled and nothing is run. Every column is data: the rule set is a table, the group
/// says which opcodes it is about, and the two lists above are lists.
#[must_use]
pub fn rows(table: &Table) -> Vec<Row> {
    let patterns = pattern_heads(table);
    let named = rucc_ir::term::heads();
    let mut out = Vec::with_capacity(named.len() + LIBCALLS.len() + 64);

    // The names the rule language can spell, which is the universe the rule column is about.
    for &(opcode, name) in &named {
        out.push(Row {
            opcode,
            name,
            rule: patterns.contains(&name),
            lowering: lowering(opcode),
            libcall: None,
            nothing: NAMES
                .iter()
                .find(|&&(at, ..)| at == name)
                .map(|&(_, why, issue)| (why, issue)),
        });
    }

    // The modes the rule language cannot spell, which is why the calls exist.
    for &(opcode, mode, call) in LIBCALLS {
        out.push(Row {
            opcode,
            name: mode,
            rule: false,
            lowering: lowering(opcode),
            libcall: Some(call),
            nothing: None,
        });
    }

    // And the operations with no mode at all, which still have the question asked about them.
    for opcode in Opcode::all() {
        if named.iter().any(|&(at, _)| at == opcode) {
            continue;
        }
        if LIBCALLS.iter().any(|&(at, ..)| at == opcode) {
            continue;
        }
        out.push(Row {
            opcode,
            name: opcode.name(),
            rule: false,
            lowering: lowering(opcode),
            libcall: None,
            nothing: GAPS
                .iter()
                .find(|&&(at, ..)| at == opcode)
                .map(|&(_, why, issue)| (why, issue)),
        });
    }
    out
}

/// Every name a rule in a table is written about, which is the first question the trie asks.
///
/// Node zero is the root of the trie over the patterns and the first thing any walk asks is what
/// the term in hand is called, so the branches on the head there are exactly the set of pattern
/// heads. Nothing else can be at the root: a pattern is a term with a head, so the first step of
/// every one of them is a head, there is no constant to compare and nothing bound yet to be the
/// same as. There is no wildcard there to worry about either, since a rule matching any term at
/// all is one nobody has written and one that would be an error to write, because a lowering has
/// to know what it is lowering.
///
/// The names come out sorted and without repeats because the root is sorted, which is what the
/// walk needs it to be, so there is nothing to do here but read it.
pub(crate) fn pattern_heads(table: &Table) -> Vec<&'static str> {
    let Some(root) = table.nodes.first() else { return Vec::new() };
    let mut found: Vec<&'static str> = root.heads.iter().map(|&(head, ..)| head).collect();
    found.dedup();
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::select::x86_64::TABLE;

    /// The claim the table is for, which is section 36.4's: an operation this target cannot do, that
    /// nothing rewrites and that has no runtime function, is a build failure here rather than a
    /// selection failure on somebody's program.
    #[test]
    fn every_row_has_at_least_one_answer_or_says_why_it_has_none() {
        let mut unanswered = Vec::new();
        for row in rows(&TABLE) {
            if row.answered() || row.nothing.is_some() {
                continue;
            }
            unanswered.push(row.name);
        }
        assert!(
            unanswered.is_empty(),
            "no rule lowers these, nothing rewrites them, no runtime function stands for them and \
             nothing says why: {unanswered:?}"
        );
    }

    /// Every row that has no answer names the issue that gives it one, since a hole with no issue
    /// behind it is a hole nobody has decided anything about.
    #[test]
    fn a_row_with_no_answer_names_the_issue_that_gives_it_one() {
        for row in rows(&TABLE) {
            let Some((why, issue)) = row.nothing else { continue };
            assert!(
                !row.answered(),
                "`{}` is {why} and is also answered, so the entry is stale and {issue} may be \
                 closed",
                row.name
            );
            let number = issue
                .strip_prefix("tamnd/rucc#")
                .unwrap_or_else(|| panic!("{issue} is not an issue in this project's tracker"));
            assert!(number.parse::<u32>().is_ok(), "{issue} does not name an issue number");
        }
    }

    /// The one overlap that is a contradiction. A rule at a name something rewrites by hand before
    /// selection runs is a rule that can never fire, because the instruction is gone by then. The
    /// group is not this, which is the next test.
    #[test]
    fn a_rule_at_a_name_something_rewrites_by_hand_could_never_fire() {
        for row in rows(&TABLE) {
            let Some(Lowering::Hand(where_)) = row.lowering else { continue };
            assert!(
                !row.rule,
                "`{}` is rewritten by {where_} before selection, so the rule written at it can \
                 never fire",
                row.name
            );
        }
    }

    /// And the overlap that is not a contradiction, which is the thing the old single list could
    /// not say. A member of the group is allowed to leave a construct alone, and the two it leaves
    /// alone most often are the conversions this machine has an instruction for, so those names
    /// have a rule and a lowering at once and both are true.
    #[test]
    fn an_operation_the_machine_has_and_a_lowering_names_is_allowed_both() {
        let both: Vec<&str> = rows(&TABLE)
            .iter()
            .filter(|row| row.rule && matches!(row.lowering, Some(Lowering::Group(_))))
            .map(|row| row.name)
            .collect();
        assert!(
            both.iter().any(|name| name.starts_with("sitofp.")),
            "a signed conversion is what `crate::expand` walks away from when the machine has the \
             instruction, and the table should show both answers: {both:?}"
        );
    }

    /// The group membership is read rather than repeated, which is what stops the two going out of
    /// step. Asking for a step's opcodes and asking the table for the same opcode give the same
    /// step, because there is only the one list.
    #[test]
    fn the_lowering_column_is_the_group_saying_what_it_is_about() {
        for &step in Step::GROUP {
            for &opcode in step.opcodes() {
                assert_eq!(
                    lowering(opcode),
                    Some(Lowering::Group(step)),
                    "`{}` is named by `{}` and the table says otherwise",
                    opcode.name(),
                    step.name()
                );
            }
        }
    }

    /// An opcode is answered for in one place. A member of the group that is also on the hand
    /// written list is the three mechanisms back again, with the list and the group each thinking
    /// it owns the opcode.
    #[test]
    fn nothing_the_group_names_is_also_written_down_by_hand() {
        for &(opcode, where_) in HAND {
            for &step in Step::GROUP {
                assert!(
                    !step.opcodes().contains(&opcode),
                    "`{}` is named by `{}` and the hand written list says it is lowered by {where_}",
                    opcode.name(),
                    step.name()
                );
            }
        }
    }

    /// A runtime function is named once. Two rows naming the same call at the same mode would be
    /// the same duplication in the column the passes read.
    #[test]
    fn no_two_rows_answer_for_the_same_operation_at_the_same_mode() {
        let mut seen: Vec<(Opcode, &str)> = Vec::new();
        for &(opcode, mode, call) in LIBCALLS {
            assert!(
                !seen.contains(&(opcode, mode)),
                "`{}` at `{mode}` is answered twice, and the second answer is {call}",
                opcode.name()
            );
            seen.push((opcode, mode));
        }
    }

    /// The lookup the passes make, which is the whole reason the list is data rather than `match`
    /// arms. A pass asks for the operation and the mode and gets the one string.
    #[test]
    fn the_passes_ask_for_a_call_by_the_operation_and_the_mode() {
        assert_eq!(libcall(Opcode::SDiv, "i128"), Some("__divti3"));
        assert_eq!(libcall(Opcode::FAdd, "f128"), Some("__addtf3"));
        assert_eq!(libcall(Opcode::SIToFP, "i128.f64"), Some("__floattidf"));
        assert_eq!(libcall(Opcode::Memmove, "any"), Some("memmove"));
        assert_eq!(libcall(Opcode::SDiv, "i64"), None, "a divide the machine has is not a call");
        assert_eq!(libcall(Opcode::Add, "i128"), None, "a wide add is two adds and not a call");
    }

    /// Every runtime function is one somebody can link against, which for the compiler runtime
    /// means the name gcc's own runtime uses. A misspelled one would build and fail at the link,
    /// which is the furthest away this mistake can be found.
    #[test]
    fn a_runtime_function_is_spelled_the_way_the_runtime_spells_it() {
        for &(opcode, mode, call) in LIBCALLS {
            let library = matches!(opcode, Opcode::Memcpy | Opcode::Memset | Opcode::Memmove);
            assert_eq!(
                call.starts_with("__"),
                !library,
                "`{call}` is a {} function and is not spelled like one",
                if library { "C library" } else { "compiler runtime" }
            );
            assert!(
                !mode.is_empty() && mode.is_ascii(),
                "`{call}` answers for a mode with no name"
            );
        }
    }

    /// One row per operation and mode, which is what section 36.4 asks for, and enough of them that
    /// the table is about the whole back end rather than a corner of it.
    #[test]
    fn the_table_is_one_row_per_operation_and_mode() {
        let rows = rows(&TABLE);
        assert!(
            rows.len() > Opcode::all().count(),
            "an operation with more than one mode is more than one row, so there are more rows \
             than there are opcodes: {} rows and {} opcodes",
            rows.len(),
            Opcode::all().count()
        );
        let with_rule = rows.iter().filter(|row| row.rule).count();
        let with_lowering = rows.iter().filter(|row| row.lowering.is_some()).count();
        let with_libcall = rows.iter().filter(|row| row.libcall.is_some()).count();
        assert!(with_rule > 0 && with_lowering > 0 && with_libcall > 0);
        assert_eq!(with_libcall, LIBCALLS.len());
        println!(
            "rucc-codegen: {} rows, {with_rule} by rule, {with_lowering} by lowering, \
             {with_libcall} by a call to the runtime",
            rows.len()
        );
    }

    /// Every opcode is in the table exactly once under its own name or once per mode it has, and
    /// none is left out. A new opcode with no row is the thing this whole module exists to stop.
    #[test]
    fn every_opcode_the_ir_has_is_in_the_table() {
        let rows = rows(&TABLE);
        for opcode in Opcode::all() {
            assert!(
                rows.iter().any(|row| row.opcode == opcode),
                "`{}` has no row, so nothing says what this target does about it",
                opcode.name()
            );
        }
    }

    /// What a hand written entry says, which is a place somebody can open. An entry naming nothing
    /// is an entry that excuses an opcode without saying where the answer is.
    #[test]
    fn a_hand_written_entry_names_where_the_answer_is() {
        for &(opcode, where_) in HAND {
            assert!(
                where_.contains('`') || where_.starts_with("nothing"),
                "the entry for `{}` says {where_}, which names no module",
                opcode.name()
            );
        }
        assert_eq!(Lowering::Hand("`crate::abi`, and so on").where_(), "`crate::abi`, and so on");
        assert_eq!(Lowering::Group(Step::Bytes).where_(), "bytes");
    }
}
