//! Taking out a safety check whose answer is already known.
//!
//! Design: `spec/safe-memory/07-check-elimination.md` section 7.3, which is the first half of the
//! Tier E budget. `rucc-safety` puts a bounds check and a lifetime check in front of every access
//! and does not try to be clever about it, on purpose: a walk that inserts everything is a walk
//! anybody can read, and every check that is not needed is meant to be taken out here instead.
//! This pass takes them out, and it does the case document 07 expects to be worth the most and to
//! be the easiest to get right, which is a second access to bytes an earlier access already had
//! checked. All three kinds `rucc-safety` emits are the pass's business, the bounds check and the
//! lifetime check in front of an access and the derivation check after a walk, because they are
//! emitted together and taking out one of three is a third of a saving.
//!
//! # The two halves
//!
//! Section 7.7 asks for the pass and the condition to be separate things, and they are. What is in
//! this file is a walk: which check runs before which, which pointer was computed from which, and
//! how far apart two addresses are. Nothing here decides whether that is enough. The condition
//! under which a check may go is a rule in `rules/safety.rules`, a solver has to agree with it
//! before this crate finishes building, and `crate::rules::safety` is the table it compiles into.
//!
//! The split is worth the trouble because the two halves fail differently. A walk that gets the
//! context wrong is a bug of the ordinary kind, and section 14.3's differential check accounting,
//! which runs the instrumented program with every check and again with the discharged ones gone,
//! is what looks for it. A removal condition that is wrong is arithmetic that is off at the ends of
//! the type. It gives the right answer on every test anybody writes and lets one access through in
//! the one case nobody thought of, and nothing observes that until somebody exploits it.
//!
//! # What it establishes and what it asks
//!
//! Walking the dominator tree from the entry, the pass carries a set of facts. A `check_bounds`
//! that stays is a fact, because a check that passes says the bytes it was about lie inside one
//! storage instance, and a check that fails does not return. A fact is remembered as the pointer's
//! base and the constant offset from it, which is what a chain of `ptr_add` over constants comes
//! to, plus how many bytes the access covers.
//!
//! At the next `check_bounds`, the pointer is normalized the same way. When a fact shares its base,
//! the distance between the two accesses is the difference of the two offsets, and that is a number
//! this pass has rather than a claim it makes: both addresses are the same value plus a constant.
//! The question of whether the later bytes are inside the earlier ones is then handed to the table,
//! which answers it in sixty four bit arithmetic rather than in the offsets, and the check goes
//! only if the answer is yes.
//!
//! A check whose extent is an operand is left out of all of this, in both directions. Section 7.4's
//! hoisted check covers as many bytes as its loop runs times, and every range compared here is a
//! pair of numbers, so such a check is neither read as a fact nor asked about. Reading its payload
//! would be worse than skipping it, since the size there is one element of the walk rather than the
//! range the check is about, and a fact recorded from it would be smaller than the truth in one
//! direction and a question asked from it smaller in the other.
//!
//! The capability operand has to be the `cap_of` of the check's own pointer, which is the shape
//! `rucc-safety` emits and the shape the argument needs. The check being removed asks whether its
//! bytes are inside the instance that owns its own pointer, its pointer is inside the range the
//! earlier check established, and that range is inside one instance, so the answer is yes. A check
//! whose capability came from somewhere else is asking about a different instance and is left
//! alone. Nothing is required of the earlier check's capability, because all that is used of it is
//! that the check passed, and a check that passed put its bytes inside one instance whatever
//! capability it named.
//!
//! # The fact nobody had to check for
//!
//! Section 7.2 lists four sources of a discharge and puts the frontend first, because the majority
//! of accesses in real C are to a local or a global at a constant offset and the bounds of either
//! are not something anybody has to find out. An `alloca` of a fixed size makes one storage
//! instance of that many bytes and says so in its payload, so the range from its address to that
//! many further along is inside one instance for exactly the reason a passing `check_bounds` says
//! its own range is. When the address a check is about normalizes to such an `alloca`, that range
//! is the fact, and the question put to the table is the same question with the same rule
//! answering it.
//!
//! Two things make it worth more than a fact a check established. It is there before anything has
//! run, so the first access to a local is discharged rather than only the second. And no call takes
//! it away: a callee cannot free a frame slot, whatever it does to whatever the slot points at, so
//! this fact is asked separately rather than kept in the set the walk throws away at the first call
//! it cannot see through.
//!
//! Only the fixed size form. A variable length array is an `alloca` with an operand and a payload
//! whose size field reads zero, and reading it anyway would discharge every check in the array.
//!
//! A global is the same fact about the other half of section 7.2's sentence, and it arrives here
//! differently for one reason: how big a global is lives on the module and this pass is given one
//! function. So `crate::extents` works it out over the module before the pipeline starts, asks the
//! same rule, and writes the answer onto the check as [`Flags::STATIC`], which is what
//! `crate::nofree` does with what a call reaches and for the same reason. What is read here is what
//! the IR says, the same way the pass reads an opcode.
//!
//! It answers a lifetime check as well as a bounds check, which a local does not. What a local
//! gives is an extent, and how long it stays alive is the block it was declared in, which is a
//! question this pass has nothing to say about. A global has static storage duration and is alive
//! wherever the question is asked.
//!
//! # The walk that stops at a step it cannot read
//!
//! Everything above needs the address to be a base and a constant, and an array index is not a
//! constant. The walk stops at the first `ptr_add` whose step is a value, and what comes out is a
//! fact about a base whose size nobody knows, which answers nothing.
//!
//! Section 7.2's third source is what gets past it. Document 10's ranges know something about the
//! step even though it is not a number: an index the program has already tested against a length,
//! or one whose low bits are all that is used, is bounded. So the walk carries on, adding the low
//! end of the step's range to the offset and the width of the range to the size, and what it ends
//! up with is the range of addresses the access can land in.
//!
//! Whether an object holding all of that range holds the one address the access actually uses is
//! its own rule, `reached.i64`, which leaves the distance opaque so that one answer covers every
//! value the step could take. It is a rule of its own rather than the containment rule asked about
//! the far end of the range, and the reason is section 7.7's: turning a range of addresses into one
//! containment question is arithmetic on the thing being proved, and a pass doing that quietly is
//! what the split between the walk and the rule exists to stop.
//!
//! What the range is asked of is the list above and not a shorter one: the local an `alloca`
//! declares, the object an allocator made where the program has tested it, and the ranges checks
//! that already ran established. The allocation was missing from that list until tamnd/rucc#880,
//! which is what left a loop walking an index into its own `malloc` with every check it started
//! with however plainly the call said how many bytes it made.
//!
//! The range is only ever asked with and never recorded. What a check proves when it runs is that
//! the address the program used was inside the object, and nothing at all about the rest of a
//! range this pass made up around it. So a check discharged this way records the narrow fact, the
//! bytes the access really wanted, which is the thing that was proved and is what a second check
//! of the same bytes is answered by.
//!
//! The ranges are built only for a function that has a walk by a value in it, because they cost a
//! copy of the control flow graph and a function without one would never ask them anything.
//!
//! # The lifetime half, and what it borrows from the other one
//!
//! A `check_live` that stays is a fact too, and a smaller one than it looks: it says the storage
//! instance holding its own address is alive, and it says nothing about the address four bytes
//! along, because that address might be in a different instance. On its own that fact discharges
//! only a second lifetime check of the very same address, and the shape `rucc-safety` emits is a
//! lifetime check per field rather than per object, so on its own it would almost never fire.
//!
//! What makes it fire is the bounds fact sitting next to it. A `check_bounds` that passed put its
//! whole range inside one instance, so if the lifetime check's address is in that range, the
//! instance that was found alive is the instance the whole range is in, and the whole range is
//! alive. So a lifetime fact is recorded as the widest checked range containing its address, and a
//! later lifetime check is asked about as a single byte. The question of whether that byte is in
//! that range is the same question the bounds half asks, put to the same rule.
//!
//! The order the two arrive in is what makes this work rather than a coincidence to be careful
//! about: `rucc-safety` emits the bounds check first and the lifetime check second, so the range is
//! established by the time there is a lifetime fact to widen. A lifetime check that arrives with no
//! range around it keeps the narrow fact, which is correct and worth little.
//!
//! # The derivation half, which is one question rather than two
//!
//! `rucc-safety` puts a `check_deriv` after every `ptr_add` off a pointer, and what it asks is not
//! about a range at all: it asks whether the pointer that came out is still in the storage instance
//! the pointer that went in belongs to. The runtime has some slack in it for a pointer that walked
//! exactly off either end, and none of that slack is used here, because the case this pass answers
//! is the one where both ends are plainly inside something.
//!
//! What answers it is one fact holding both ends. A `check_bounds` that passed put its whole range
//! inside one instance, so if the address that went in and the address that came out are both in
//! that range, the second is in the instance the first belongs to, which is the question. It has to
//! be one fact and not one for each end: two facts saying two addresses are each inside some
//! instance say nothing about whether it is the same instance, and that is the only thing being
//! asked. A local is a fact of exactly this shape and is asked the same way.
//!
//! Both ends are asked about as a single byte, the way a lifetime check is, and for the same reason.
//! Nothing here is claiming anything about how many bytes are readable at either address.
//!
//! A `check_deriv` that stays leaves no fact behind. What it establishes is that two addresses share
//! an instance, which is not a range of bytes and does not fit in what this walk carries, and the
//! `covered.i64` rule has nothing to say about it. Recording it would mean a second kind of fact and
//! a second rule, and the pointer it is about nearly always gets a `check_bounds` of its own a few
//! instructions later that establishes the range properly.
//!
//! # Why a call throws the facts away, and which calls do not
//!
//! Section 7.3 says nothing kills a bounds fact except a redefinition of the capability, which in
//! SSA is never, and this pass is stricter than that: a call, or anything else this pass cannot see
//! through, drops every fact it is carrying.
//!
//! The case is a `free` and then an allocation of something smaller at the same address. The range
//! established before the call is no longer inside one instance after it, and what document 07
//! leaves that to is the lifetime judgement rather than this one. Today's lifetime check is about
//! the address rather than about the version the capability was taken at, so it would not refuse
//! the access either, and a rate this pass reports is worth less than a hole it opens. The strict
//! version is what is written first.
//!
//! A `meta_end` and a `meta_transfer` drop the facts as well. Nothing emits either one yet, so
//! this costs nothing today and is the difference between conservative and wrong on the day the
//! instrumentation starts ending lifetimes. `crate::nofree` treats them the same way.
//!
//! The two facts nobody had to check for go across a call untouched, and neither is an exception to
//! the paragraph above because neither is in the set being thrown away. A callee cannot free a
//! frame slot and cannot free a global, so a check the declaration answers is answered on the far
//! side of any call at all.
//!
//! A call that says it reaches nothing which can free is the exception, and it is not this pass
//! being trusting. `crate::nofree` works the answer out over the whole module before the pipeline
//! starts and writes it onto the call site as [`Flags::NOFREE`], because the fact belongs to the
//! callee and a pass is given one function. Reading it here is reading what the IR says, the same
//! way the pass reads an opcode. Nothing else about a call is believed: the facts still go across
//! an unmarked call, a call through an address, and inline assembly.
//!
//! What the strictness still costs is measured rather than guessed. A check that a fact would have
//! covered if a call had not intervened is counted, so `-fopt-info-missed` says per function what
//! is left to win.

use std::collections::{HashMap, HashSet};

use rucc_ir::{Block, Def, Extra, Flags, Func, Inst, Opcode, Value};

use crate::range::query::Ranges;
use crate::rules::{Piece, Subject, Table, safety};
use crate::{Analyses, Analysis, Cfg, Fuel, Pass, Preserved, Stats, heap};

/// Recorded once for each bounds check taken out.
const REMOVED: &str = "bounds check removed, a dominating check covers the same bytes";

/// Recorded once for each bounds check taken out because it was inside a local.
const REMOVED_LOCAL: &str = "bounds check removed, its bytes are inside a local this function \
                             declares";

/// Recorded once for each bounds check taken out because it was inside a global.
const REMOVED_STATIC: &str = "bounds check removed, its bytes are inside an object of static \
                              storage duration";

/// Recorded once for each bounds check taken out because every caller hands in the object.
const REMOVED_HANDED: &str = "bounds check removed, its bytes are inside an object every call to \
                              this function hands it";

/// Recorded once for each bounds check taken out because an allocator made the object.
const REMOVED_MADE: &str = "bounds check removed, its bytes are inside an object an allocator made \
                            and this function has tested";

/// Recorded once for each bounds check taken out because a range answered the step it walked by.
const REMOVED_RANGE: &str = "bounds check removed, every address the walk can reach is inside the \
                             object it started from";

/// Recorded once for each lifetime check taken out.
const REMOVED_LIVE: &str = "lifetime check removed, a dominating check covers the same storage";

/// Recorded once for each lifetime check taken out because it was inside a global.
const REMOVED_LIVE_STATIC: &str =
    "lifetime check removed, its storage lives as long as the program does";

/// Recorded once for each lifetime check taken out because every caller hands in the object.
const REMOVED_LIVE_HANDED: &str = "lifetime check removed, its storage is an object every call to \
                                   this function hands it";

/// Recorded once for each lifetime check taken out because it was inside a frame slot.
const REMOVED_LIVE_LOCAL: &str =
    "lifetime check removed, its storage is a frame slot of this function";

/// Recorded once for each lifetime check taken out because a range answered the step it walked by.
const REMOVED_LIVE_RANGE: &str = "lifetime check removed, every address the walk can reach is in \
                                  storage a check found alive";

/// Recorded for a bounds check that would have gone if there had been fuel for it.
const NO_FUEL: &str = "bounds check kept, the pass ran out of fuel";

/// Recorded for a lifetime check that would have gone if there had been fuel for it.
const NO_FUEL_LIVE: &str = "lifetime check kept, the pass ran out of fuel";

/// Recorded once for each derivation check taken out because a range answered the step it walked by.
const REMOVED_DERIV_RANGE: &str = "derivation check removed, every address either end can reach is \
                                   inside one checked range";

/// Recorded for a bounds check a call cost, which is the honest price of the paragraph above.
///
/// This one is worth reading rather than skipping. It is the number of checks that are still being
/// paid for because `crate::nofree` could not vouch for a call, so it says per function what the
/// rest of section 7.5's summary work would be worth before anybody writes it.
const PAST_A_CALL: &str =
    "bounds check kept, a call between it and the check that covers it might free";

/// The same, for a lifetime check. Section 8.8 is about this number rather than the one above.
const PAST_A_CALL_LIVE: &str =
    "lifetime check kept, a call between it and the check that covers it might free";

/// Recorded for a bounds check whose operands this pass cannot read.
const UNKNOWN_SHAPE: &str = "bounds check left alone, its pointer is not a base and a constant";

/// Recorded for a bounds check about a range the program worked out.
const COMPUTED_EXTENT: &str =
    "bounds check left alone, how many bytes it covers is a number only the program has";

/// Recorded for a lifetime check whose operands this pass cannot read.
const UNKNOWN_SHAPE_LIVE: &str =
    "lifetime check left alone, its pointer is not a base and a constant";

/// Recorded once for each derivation check taken out.
const REMOVED_DERIV: &str =
    "derivation check removed, one checked range holds both the pointer and where it walked to";

/// Recorded once for each derivation check taken out because it walked inside a local.
const REMOVED_DERIV_LOCAL: &str =
    "derivation check removed, it walks inside a local this function declares";

/// Recorded once for each derivation check taken out because it walked inside a global.
const REMOVED_DERIV_STATIC: &str =
    "derivation check removed, it walks inside an object of static storage duration";

/// Recorded once for each derivation check taken out because every caller hands in the object.
const REMOVED_DERIV_HANDED: &str = "derivation check removed, it walks inside an object every call \
                                    to this function hands it";

/// Recorded once for each derivation check taken out because an allocator made the object.
const REMOVED_DERIV_MADE: &str = "derivation check removed, it walks inside an object an allocator \
                                  made and this function has tested";

/// Recorded for a derivation check that would have gone if there had been fuel for it.
const NO_FUEL_DERIV: &str = "derivation check kept, the pass ran out of fuel";

/// Recorded for a derivation check a call cost.
const PAST_A_CALL_DERIV: &str =
    "derivation check kept, a call between it and the range that holds both ends might free";

/// Recorded for a derivation check naming a capability that is not the one it is about.
const NOT_ITS_CAPABILITY_DERIV: &str = "derivation check left alone, the capability it names is not the one the pointer that went in \
     carries";

/// Recorded for a derivation check whose two ends are not off one value.
const TWO_BASES_DERIV: &str =
    "derivation check left alone, its two pointers are not built on one base";

/// Recorded for a derivation check whose walk can reach past the end of the local it starts in.
const OVER_THE_LOCAL_DERIV: &str =
    "derivation check left alone, the walk can reach past the end of the local it starts in";

/// Recorded for a derivation check on a pointer this function loaded out of memory.
const NO_EXTENT_LOADED: &str = "derivation check left alone, nothing here says how big the object \
                                is and the pointer to it was loaded from memory";

/// Recorded for a derivation check on a pointer this function was handed.
const NO_EXTENT_HANDED: &str = "derivation check left alone, nothing here says how big the object \
                                is and the pointer to it was handed to this function";

/// Recorded for a derivation check on a pointer into a global.
const NO_EXTENT_GLOBAL: &str = "derivation check left alone, nothing here says how big the object \
                                is and the pointer to it is into a global";

/// Recorded for a derivation check on a pointer a call handed back.
const NO_EXTENT_RETURNED: &str = "derivation check left alone, nothing here says how big the \
                                  object is and the pointer to it came back from a call";

/// Recorded for a derivation check on a pointer none of the shapes above describes.
const NO_EXTENT_OTHER: &str =
    "derivation check left alone, nothing here says how big the object its pointers are in is";

/// The pass. It holds nothing, because everything it works out is about one function.
/// Which of the places a fact comes from a run of this pass may ask.
///
/// Everything is asked normally and there is one pass in the pipeline. The others are here for the
/// measurement `spec/safe-memory/13-performance.md` section 13.5 asks for and
/// `spec/safe-memory/17-open-questions.md` question 3 is: how much each source discharges on its
/// own, and how much the same sources discharge together. A number for a source on its own cannot
/// be read off the remarks of a full run, because the rules are asked in an order and whichever one
/// answers first is the one the remark names, so the second source to be asked about a check two of
/// them could answer looks like it answered nothing.
///
/// The four are document 07 section 7.2's four, with the caveat the measurement found: the ranges
/// are not a fourth kind of fact but a way of asking the other three about a subscript instead of
/// about an address written out in the program.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Sources {
    /// How big an object is, read off whatever made it. A global's extent comes from
    /// `crate::extents`, a local's from its `alloca`, an allocation's from the call `crate::heap`
    /// marked. Section 7.2's first source.
    objects: bool,
    /// What a check that has already run established, carried down the dominator tree. Section 7.3,
    /// and the one the literature calls redundant check elimination.
    dominance: bool,
    /// What every caller of this function guarantees about what it was handed, from
    /// `crate::params`. Section 7.5.
    summaries: bool,
    /// The value ranges and the recurrences, which widen the one address a check names into the
    /// range of addresses a walk can reach so that the other three can be asked about a subscript.
    /// Section 7.4, and the half of the PICO result this pass holds. The other half is
    /// [`crate::hoist`] and [`crate::split`], which are passes of their own and have flags of their
    /// own.
    ranges: bool,
}

impl Sources {
    /// Every one of them, which is what the pipeline runs.
    pub const ALL: Self = Self { objects: true, dominance: true, summaries: true, ranges: true };
    /// What an object says about itself and nothing else.
    pub const OBJECTS: Self =
        Self { objects: true, dominance: false, summaries: false, ranges: false };
    /// What an earlier check established and nothing else.
    pub const DOMINANCE: Self =
        Self { objects: false, dominance: true, summaries: false, ranges: false };
    /// What every caller guarantees and nothing else.
    pub const SUMMARIES: Self =
        Self { objects: false, dominance: false, summaries: true, ranges: false };
    /// Every fact, asked only about addresses written out in the program.
    pub const NARROW: Self =
        Self { objects: true, dominance: true, summaries: true, ranges: false };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Discharge {
    /// What `-f<name>` and `-fno-<name>` reach this run by.
    name: &'static str,
    /// Which places it may take a fact from. See [`Sources`].
    sources: Sources,
}

/// The pass the pipeline runs, which asks everything.
pub static DISCHARGE: Discharge = Discharge { name: "discharge", sources: Sources::ALL };

/// The same pass asking an object how big it is and nothing else.
pub static OBJECTS: Discharge = Discharge { name: "discharge-objects", sources: Sources::OBJECTS };

/// The same pass asking what an earlier check established and nothing else.
pub static DOMINANCE: Discharge =
    Discharge { name: "discharge-dominance", sources: Sources::DOMINANCE };

/// The same pass asking what every caller guarantees and nothing else.
pub static SUMMARIES: Discharge =
    Discharge { name: "discharge-summaries", sources: Sources::SUMMARIES };

/// The same pass asking every fact, about addresses written out in the program only.
pub static NARROW: Discharge = Discharge { name: "discharge-narrow", sources: Sources::NARROW };

/// The same pass asking everything, under a name of its own.
///
/// [`DISCHARGE`] already asks everything, so this looks like a duplicate and is not. A pass the level
/// did not choose goes on the end of the pipeline, so a run of `-fno-discharge -fdischarge-objects`
/// asks its question in a different place from where the shipped pass asks it, and the two numbers
/// are not comparable. This one is turned on the same way as the others and lands in the same place,
/// so the sum of the parts and the whole are measured under one arrangement. What it costs against
/// [`DISCHARGE`] is what the position is worth, which is a number the measurement wants anyway.
pub static EVERY: Discharge = Discharge { name: "discharge-every", sources: Sources::ALL };

impl Pass for Discharge {
    fn name(&self) -> &'static str {
        self.name
    }

    fn describe(&self) -> &'static str {
        "a bounds, lifetime or derivation check whose answer is already known is removed"
    }

    fn preserves(&self) -> Preserved {
        // Instructions go and blocks do not. A check is not a terminator and removing one leaves
        // every edge where it was. What it does not leave where it was is the liveness, because
        // the check was reading something and now nothing is.
        Preserved::ALL.without(Analysis::Liveness)
    }

    fn run(&self, func: &mut Func, an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        let Some(entry) = func.entry() else { return stats };
        let dom = an.dominators(func).clone();

        // The graph is built for two reasons and neither is the common one, so a function with
        // neither pays for no copy of it. The ranges want it when there is a walk the constant
        // reader gives up on, and the allocation rule wants it to find where the program has tested
        // what an allocator gave it.
        let walks = self.sources.ranges && walks_by_a_value(func);
        let cfg = (walks || (self.sources.objects && heap::allocates(func)))
            .then(|| an.cfg(func).clone());
        let mut ranges = cfg.as_ref().filter(|_| walks).map(|cfg| Ranges::new(&*func, cfg, &dom));

        // One answer per allocation rather than one per check, because a function that reads twenty
        // fields of the same object asks the same question about the same pointer twenty times.
        let mut checked: HashMap<Value, HashSet<Block>> = HashMap::new();

        // Whether anything in here says a lifetime is over. Read once over the whole function
        // rather than carried down the walk, because what the frame slot rule needs is that no
        // `meta_end` runs before the check on any path, and a fact carried down the dominator
        // tree only ever says something about the paths that go through one block.
        let ends = ends_a_lifetime(func);

        // The walk is a stack rather than recursion because the dominator tree of a long chain of
        // blocks is as deep as the function is long, and a pass is not a place to find that out.
        // Each block carries its own copy of what holds at its start, which is what makes a fact a
        // call killed in one arm of a branch still hold in the other.
        let mut going: Vec<(Inst, &'static str)> = Vec::new();
        let mut work = vec![(entry, Scope::default())];
        while let Some((block, mut scope)) = work.pop() {
            for inst in func.insts(block).collect::<Vec<Inst>>() {
                if opaque(func, inst) {
                    scope.forget();
                    continue;
                }
                match func[inst].opcode {
                    Opcode::CheckBounds => {
                        if func[func[inst].args].len() > 2 {
                            stats.missed(COMPUTED_EXTENT);
                            continue;
                        }
                        let Some(asked) = about(func, inst) else {
                            stats.missed(UNKNOWN_SHAPE);
                            continue;
                        };
                        // The four objects whose extent is known without anybody having checked
                        // it. A global was worked out over the module by `crate::extents` and an
                        // object every caller hands in by `crate::params`, both of which arrive as
                        // a flag; a local is read off its `alloca` here and an allocation off the
                        // call `crate::heap` marked. All four are asked of the same rule as every
                        // other fact. The reach of a walk the constant reader could not finish is
                        // asked last, because it is the only one that costs an analysis to answer.
                        let why = if self.sources.objects
                            && func[inst].flags.contains(Flags::STATIC)
                        {
                            Some(REMOVED_STATIC)
                        } else if self.sources.summaries && func[inst].flags.contains(Flags::HANDED)
                        {
                            Some(REMOVED_HANDED)
                        } else if self.sources.objects
                            && declared(func, asked.base)
                                .is_some_and(|local| covers(&local, &asked))
                        {
                            Some(REMOVED_LOCAL)
                        } else if self.sources.objects
                            && allocated(func, cfg.as_ref(), &mut checked, block, &[&asked])
                        {
                            Some(REMOVED_MADE)
                        } else if self.sources.dominance && scope.bounds.covers(&asked) {
                            Some(REMOVED)
                        } else {
                            // The same four sources in the same order, asked of the range of
                            // addresses the walk can reach rather than of the one address the
                            // constant reader could name. A flag has already been read above and
                            // reading it again would say the same thing, so what is left is the
                            // local, the allocation and what the walk carries.
                            reach(func, ranges.as_mut(), &asked, inst).and_then(|wide| {
                                if self.sources.objects
                                    && declared(func, wide.base)
                                        .is_some_and(|local| reaches(&local, &wide))
                                {
                                    Some(REMOVED_RANGE)
                                } else if self.sources.objects
                                    && allocated_around(
                                        func,
                                        cfg.as_ref(),
                                        &mut checked,
                                        block,
                                        &[&wide],
                                    )
                                {
                                    Some(REMOVED_MADE)
                                } else if self.sources.dominance && scope.bounds.reaches(&wide) {
                                    Some(REMOVED_RANGE)
                                } else {
                                    None
                                }
                            })
                        };
                        let Some(why) = why else {
                            if scope.bounds.covered_before(&asked) {
                                stats.missed(PAST_A_CALL);
                            }
                            // A check that stays is a check that runs, and a check that runs
                            // establishes what it was about. One that was removed establishes
                            // nothing new: whatever covered it covers everything it would have.
                            scope.bounds.held.push(asked);
                            continue;
                        };
                        if !fuel.take() {
                            stats.missed(NO_FUEL);
                            scope.bounds.held.push(asked);
                            continue;
                        }
                        // A check that goes normally establishes nothing new, because whatever
                        // answered it covers everything it would have. The range is the one
                        // exception: what answered it was a fact about a made up range around the
                        // address, and the next check on these bytes has to ask for that range
                        // again and may not get the same answer. So the narrow fact goes in, which
                        // is the thing that was actually proved.
                        if why == REMOVED_RANGE {
                            scope.bounds.held.push(asked);
                        }
                        going.push((inst, why));
                    }
                    Opcode::CheckLive => {
                        let Some(asked) = alive(func, inst) else {
                            stats.missed(UNKNOWN_SHAPE_LIVE);
                            continue;
                        };
                        // A global is alive as long as the program is, and a frame slot is alive
                        // until the function returns, so both objects whose extent is known
                        // without anybody having checked it answer this as well as a bounds
                        // check. `ends` is what makes the second one true: where a local stops
                        // being alive is written into the IR as `meta_end` and not read off the
                        // shape of the source, so a function with one in it is a function this
                        // does not claim anything about.
                        let why = if self.sources.objects
                            && func[inst].flags.contains(Flags::STATIC)
                        {
                            Some(REMOVED_LIVE_STATIC)
                        } else if self.sources.summaries && func[inst].flags.contains(Flags::HANDED)
                        {
                            Some(REMOVED_LIVE_HANDED)
                        } else if self.sources.objects
                            && !ends
                            && declared(func, asked.base)
                                .is_some_and(|local| covers(&local, &asked))
                        {
                            Some(REMOVED_LIVE_LOCAL)
                        } else if self.sources.dominance && scope.alive.covers(&asked) {
                            Some(REMOVED_LIVE)
                        } else {
                            // A lifetime fact and not a bounds one, because what is being asked
                            // is whether the storage is alive and a bounds check that passed says
                            // nothing about that. The widening argument is the bounds arm's: a
                            // range known alive that holds every address the walk can reach holds
                            // the one it actually uses.
                            reach(func, ranges.as_mut(), &asked, inst)
                                .filter(|wide| {
                                    (self.sources.objects
                                        && !ends
                                        && declared(func, wide.base)
                                            .is_some_and(|local| reaches(&local, wide)))
                                        || (self.sources.dominance && scope.alive.reaches(wide))
                                })
                                .map(|_| REMOVED_LIVE_RANGE)
                        };
                        let Some(why) = why else {
                            if scope.alive.covered_before(&asked) {
                                stats.missed(PAST_A_CALL_LIVE);
                            }
                            scope.alive.held.push(widened(func, &scope.bounds, asked));
                            continue;
                        };
                        if !fuel.take() {
                            stats.missed(NO_FUEL_LIVE);
                            scope.alive.held.push(widened(func, &scope.bounds, asked));
                            continue;
                        }
                        // The bounds arm's exception, for its reason. A range answered a made up
                        // range around this address, so what was proved is about the address.
                        if why == REMOVED_LIVE_RANGE {
                            scope.alive.held.push(widened(func, &scope.bounds, asked));
                        }
                        going.push((inst, why));
                    }
                    Opcode::CheckDeriv => {
                        let narrow = derives(func, inst);
                        let why = narrow.and_then(|(from, to)| {
                            if self.sources.objects && func[inst].flags.contains(Flags::STATIC) {
                                Some(REMOVED_DERIV_STATIC)
                            } else if self.sources.summaries
                                && func[inst].flags.contains(Flags::HANDED)
                            {
                                Some(REMOVED_DERIV_HANDED)
                            } else if self.sources.objects
                                && declared(func, from.base).is_some_and(|local| {
                                    covers(&local, &from) && covers(&local, &to)
                                })
                            {
                                Some(REMOVED_DERIV_LOCAL)
                            } else if self.sources.objects
                                && allocated(func, cfg.as_ref(), &mut checked, block, &[&from, &to])
                            {
                                Some(REMOVED_DERIV_MADE)
                            } else if self.sources.dominance && scope.bounds.holds_both(&from, &to)
                            {
                                Some(REMOVED_DERIV)
                            } else {
                                None
                            }
                        });
                        // Asked last, and asked off the check's own operands rather than off what
                        // `derives` worked out, because the case it is for is the one `derives`
                        // cannot read at all: past a step the constant reader gives up on the two
                        // ends are not one base and two constants. One thing has to hold both of
                        // the ranges, for the same reason one thing has to hold both of the
                        // addresses, which is that two things saying each end is inside something
                        // say nothing about it being the same something.
                        let why = why.or_else(|| {
                            spread(func, ranges.as_mut(), inst, inst).and_then(|(near, far)| {
                                if self.sources.objects
                                    && declared(func, near.base).is_some_and(|local| {
                                        reaches(&local, &near) && reaches(&local, &far)
                                    })
                                {
                                    Some(REMOVED_DERIV_RANGE)
                                } else if self.sources.objects
                                    && allocated_around(
                                        func,
                                        cfg.as_ref(),
                                        &mut checked,
                                        block,
                                        &[&near, &far],
                                    )
                                {
                                    Some(REMOVED_DERIV_MADE)
                                } else if self.sources.dominance
                                    && scope.bounds.reaches_both(&near, &far)
                                {
                                    Some(REMOVED_DERIV_RANGE)
                                } else {
                                    None
                                }
                            })
                        });
                        let Some(why) = why else {
                            match narrow {
                                Some((from, to)) => {
                                    if scope.bounds.held_both_before(&from, &to) {
                                        stats.missed(PAST_A_CALL_DERIV);
                                    }
                                }
                                None => {
                                    stats.missed(unreadable(func, ranges.as_mut(), inst));
                                }
                            }
                            continue;
                        };
                        if !fuel.take() {
                            stats.missed(NO_FUEL_DERIV);
                            continue;
                        }
                        going.push((inst, why));
                    }
                    _ => continue,
                }
            }
            for child in dom.children(block) {
                work.push((child, scope.clone()));
            }
        }

        for (inst, why) in going {
            func.remove_inst(inst);
            stats.optimized(why);
        }
        stats
    }
}

/// A range of bytes some check has already been passed on, or is being asked about.
///
/// The address is kept as the value it was computed from and the constant distance from it, rather
/// than as the pointer itself, because that is what makes two of these comparable: the whole of
/// what this pass knows about two addresses is that they are one value plus two constants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Fact {
    /// The value the address was computed from.
    pub(crate) base: Value,
    /// How far past it the access starts.
    offset: i128,
    /// How many bytes it covers.
    size: i128,
}

impl Fact {
    /// The whole of an object whose extent is known, starting at its own address.
    ///
    /// The two sources of one of these are an `alloca` of a fixed size and a global, and what they
    /// have in common is that the size is said by something other than a check that passed.
    pub(crate) fn whole(base: Value, size: i128) -> Self {
        Self { base, offset: 0, size }
    }
}

/// A range of addresses an access can land in, and how many bytes it takes when it does.
///
/// What [`reach`] works out and the only thing it is used for. It is deliberately not a [`Fact`]:
/// a fact is something that was established and may be recorded, and this is a question and may
/// not. The address the program uses is `base` plus somewhere between `low` and `low` plus `width`
/// further along, and what a check proves when it runs is about that one address rather than about
/// the range this was made out of.
#[derive(Debug, Clone, Copy)]
struct Reach {
    /// The value the address was computed from.
    base: Value,
    /// The nearest the access can start to it.
    low: i128,
    /// How much further than that it can start.
    width: i128,
    /// How many bytes it covers.
    size: i128,
}

/// One kind of fact, and what has become of it.
#[derive(Debug, Clone, Default)]
struct Known {
    /// The ranges a check has been passed on and nothing has cast doubt on since.
    held: Vec<Fact>,
    /// The ones a call threw away, kept only so that the cost of throwing them away is a number
    /// somebody can read rather than a paragraph somebody has to believe.
    lost: Vec<Fact>,
}

impl Known {
    /// Whether something still standing answers this.
    fn covers(&self, asked: &Fact) -> bool {
        self.held.iter().any(|fact| covers(fact, asked))
    }

    /// Whether something still standing answers a range of addresses an access can land in.
    fn reaches(&self, asked: &Reach) -> bool {
        self.held.iter().any(|fact| reaches(fact, asked))
    }

    /// Whether one thing still standing answers both of these ranges.
    ///
    /// One rather than one each, for the reason [`Known::holds_both`] gives, and the reason does
    /// not change when the ends are ranges instead of addresses.
    fn reaches_both(&self, from: &Reach, to: &Reach) -> bool {
        self.held.iter().any(|fact| reaches(fact, from) && reaches(fact, to))
    }

    /// Whether something would have answered it before a call came along.
    fn covered_before(&self, asked: &Fact) -> bool {
        self.lost.iter().any(|fact| covers(fact, asked))
    }

    /// Whether one thing still standing answers both of these.
    ///
    /// One rather than one each, which is the whole point of asking it this way. Two facts saying
    /// two addresses are each inside some instance say nothing about whether it is the same
    /// instance, and that is the only thing a derivation check wants to know.
    fn holds_both(&self, from: &Fact, to: &Fact) -> bool {
        self.held.iter().any(|fact| covers(fact, from) && covers(fact, to))
    }

    /// Whether one would have answered both before a call came along.
    fn held_both_before(&self, from: &Fact, to: &Fact) -> bool {
        self.lost.iter().any(|fact| covers(fact, from) && covers(fact, to))
    }

    /// Gives up everything, because something happened that this pass cannot see through.
    fn forget(&mut self) {
        self.lost.append(&mut self.held);
    }
}

/// What holds where the walk has got to.
///
/// The two kinds are apart because they are killed together and answered separately: a range being
/// inside one instance and that instance being alive are different claims, and reporting them as
/// one number would hide which of the two a check is still being paid for.
#[derive(Debug, Clone, Default)]
struct Scope {
    /// Ranges a `check_bounds` established are inside one storage instance.
    bounds: Known,
    /// Ranges a `check_live` established are in an instance that is alive.
    alive: Known,
}

impl Scope {
    /// Gives up every fact of either kind.
    fn forget(&mut self) {
        self.bounds.forget();
        self.alive.forget();
    }
}

/// Whether this instruction could do something to memory that this pass cannot account for.
///
/// A call is the whole of it, in every spelling, and inline assembly with it. A `tail_call` ends
/// the block and there is nothing after it to protect, and it is here anyway so that the reason a
/// fact survives is never that the walk did not think of something.
///
/// A call carrying [`Flags::NOFREE`] reaches nothing that ends a lifetime, so there is nothing for
/// it to have done to the bytes an earlier check was passed on. `crate::nofree` is what put the
/// flag there and what argues for it.
///
/// A `meta_end` and a `meta_transfer` end a lifetime by saying so, which is the plainest way for a
/// fact to stop being true, and neither is emitted today.
fn opaque(func: &Func, inst: Inst) -> bool {
    match func[inst].opcode {
        Opcode::Call | Opcode::CallIndirect | Opcode::TailCall => {
            !func[inst].flags.contains(Flags::NOFREE)
        }
        Opcode::InlineAsm | Opcode::MetaEnd | Opcode::MetaTransfer => true,
        _ => false,
    }
}

/// What a `check_bounds` is about, when it is one this pass can read.
pub(crate) fn about(func: &Func, check: Inst) -> Option<Fact> {
    let (base, offset) = addressed(func, check)?;
    let Extra::Mem(info) = func[check].extra else { return None };
    Some(Fact { base, offset, size: i128::from(func[info].size) })
}

/// What a `check_live` is about, when it is one this pass can read.
///
/// One byte, because that is the whole of what the check says: the instance holding this address
/// is alive, and nothing about the address next door. The widening to a range that makes the fact
/// useful is [`widened`], and it needs a bounds fact to do it.
pub(crate) fn alive(func: &Func, check: Inst) -> Option<Fact> {
    let (base, offset) = addressed(func, check)?;
    Some(Fact { base, offset, size: 1 })
}

/// The address a check is about, as a base and a constant.
///
/// The capability has to be the `cap_of` of the check's own pointer. That is the shape
/// `rucc-safety` emits and it is what the removal argument in the module comment needs, so a check
/// that does not have it is not a check this pass has anything to say about.
fn addressed(func: &Func, check: Inst) -> Option<(Value, i128)> {
    let args = &func[func[check].args];
    let &capability = args.first()?;
    let &pointer = args.get(1)?;
    if operand_of(func, capability, Opcode::CapOf, 0) != Some(pointer) {
        return None;
    }
    Some(normal(func, pointer))
}

/// The two ends of a `check_deriv`, each as the single byte at it.
///
/// A derivation check asks whether the pointer that came out of a `ptr_add` is still in the storage
/// instance the pointer that went in belongs to, so both ends have to be readable and both have to
/// come out of the same value, which is what makes the two offsets comparable at all. One byte each
/// because that is what is being asked about: not a range, but whether an address is in an instance.
///
/// The capability has to be the `cap_of` of the pointer that went in, for the reason [`addressed`]
/// gives. The instance the check is about is the one that pointer belongs to, and a check naming
/// some other capability is about some other instance.
///
/// The width operand is not read. It matters to the runtime only for a pointer that walked off the
/// near end, where the check passes on the byte a stride further along instead of on the address
/// itself, and this pass never gets that far: it discharges nothing it has not put inside a range
/// outright.
pub(crate) fn derives(func: &Func, check: Inst) -> Option<(Fact, Fact)> {
    let args = &func[func[check].args];
    let &capability = args.first()?;
    let &from = args.get(1)?;
    let &to = args.get(2)?;
    if operand_of(func, capability, Opcode::CapOf, 0) != Some(from) {
        return None;
    }
    let (base, start) = normal(func, from);
    let (walked, end) = normal(func, to);
    if base != walked {
        return None;
    }
    Some((Fact { base, offset: start, size: 1 }, Fact { base, offset: end, size: 1 }))
}

/// The object a local is, when the address a check is about was computed from one.
///
/// This is the fact nobody had to check for, and section 7.2 puts it first of the four sources
/// because it is where most of the win is. An `alloca` of a fixed size is one storage instance of
/// that many bytes, said by the instruction that makes it rather than by a check that passed, so
/// the bytes from its address to that many further along are inside one instance for the same
/// reason a passing `check_bounds` says its own range is.
///
/// Only the fixed size form. The one that takes an operand is a variable length array, and how
/// many bytes it is is a value the program works out rather than a number in the payload, where
/// the field reads zero.
///
/// The fact holds everywhere in the function and no call takes it away, which is the other half of
/// what makes it worth having. A callee cannot free a frame slot: what it could free is whatever a
/// pointer stored in the slot points at, and that is a different instance and a different check.
/// So this is asked separately from the facts the walk carries rather than pushed into them, since
/// everything in there is thrown away at the first call this pass cannot see through.
fn declared(func: &Func, base: Value) -> Option<Fact> {
    let Def::Result { inst, .. } = func[base].def else { return None };
    if func[inst].opcode != Opcode::Alloca || !func[func[inst].args].is_empty() {
        return None;
    }
    let Extra::Mem(info) = func[inst].extra else { return None };
    Some(Fact::whole(base, i128::from(func[info].size)))
}

/// The object an allocator made, when the address a check is about was computed from one and this
/// function has already found out it is not null.
///
/// The same shape as [`declared`] one storey up, with a marked call saying the size instead of an
/// `alloca` and one more thing to establish. `crate::heap` has the argument for both halves: what a
/// call to `malloc` says is an extent and never a lifetime, and it only says it where the program
/// has looked, because a null pointer is inside no object and a check on one is a check that is
/// meant to fail.
///
/// Nothing is claimed when the graph was not built, which is a function this found no allocation in
/// and so a function where the answer would have been no anyway.
fn allocation(
    func: &Func,
    cfg: Option<&Cfg>,
    checked: &mut HashMap<Value, HashSet<Block>>,
    block: Block,
    base: Value,
) -> Option<Fact> {
    let whole = heap::made(func, base)?;
    let cfg = cfg?;
    checked
        .entry(whole.base)
        .or_insert_with(|| heap::tested(func, cfg, whole.base))
        .contains(&block)
        .then_some(whole)
}

/// Whether all of those bytes are inside one object an allocator made.
///
/// Every part has to be inside, and inside the same object, which is what asking [`covers`] with one
/// fact and several does.
fn allocated(
    func: &Func,
    cfg: Option<&Cfg>,
    checked: &mut HashMap<Value, HashSet<Block>>,
    block: Block,
    parts: &[&Fact],
) -> bool {
    let Some(first) = parts.first() else { return false };
    let Some(whole) = allocation(func, cfg, checked, block, first.base) else { return false };
    parts.iter().all(|part| covers(&whole, part))
}

/// Whether every address a walk can reach is inside one object an allocator made.
///
/// [`allocated`] for the question [`reach`] and [`spread`] ask. The object comes from the same place
/// and is believed for the same reason, and what is asked of it is [`reaches`] rather than
/// [`covers`], so a walk by a step the ranges put numbers on can be answered by a call that says how
/// many bytes it made.
///
/// The wide path used to ask a local and the facts the walk carries and nothing else, so a program
/// that walked into its own `malloc` by an index kept its checks however plainly the size was
/// written. That is the first half of tamnd/rucc#880.
fn allocated_around(
    func: &Func,
    cfg: Option<&Cfg>,
    checked: &mut HashMap<Value, HashSet<Block>>,
    block: Block,
    spans: &[&Reach],
) -> bool {
    let Some(first) = spans.first() else { return false };
    let Some(whole) = allocation(func, cfg, checked, block, first.base) else { return false };
    spans.iter().all(|span| reaches(&whole, span))
}

/// A lifetime fact grown from one address to the checked range it sits in.
///
/// The argument is in the module comment: a `check_bounds` that passed put its whole range inside
/// one instance, so the instance this lifetime check found alive is the instance that range is in.
/// With no range around the address the fact stays as it came, which is correct and answers only a
/// repeat of the very same check.
///
/// A local is asked about first, because the object it is is the widest range there can be for an
/// address computed from it and a wider fact answers more later checks. What that gives is a
/// lifetime check anywhere in a local discharging every later one in the same local, up to the
/// first call, which is the shape a function that reads several fields of a local struct has.
fn widened(func: &Func, bounds: &Known, asked: Fact) -> Fact {
    if let Some(local) = declared(func, asked.base).filter(|local| covers(local, &asked)) {
        return local;
    }
    bounds.held.iter().find(|fact| covers(fact, &asked)).copied().unwrap_or(asked)
}

/// The value an address was computed from, and how far past it the address is.
///
/// A `ptr_add` over a constant is walked through, and anything else is where the answer stops. The
/// arithmetic here is exact because it is done in `i128` over offsets that came out of the IR as
/// sixty four bit constants, and whether it is small enough to mean anything at sixty four bits is
/// the rule's question rather than this function's.
pub(crate) fn normal(func: &Func, value: Value) -> (Value, i128) {
    let mut base = value;
    let mut offset: i128 = 0;
    while let Some((from, step)) = walked(func, base) {
        let Some(sum) = offset.checked_add(step) else { break };
        base = from;
        offset = sum;
    }
    (base, offset)
}

/// Every address a walk can reach, when a step it takes is a value rather than a constant.
///
/// This is the third of the four sources section 7.2 lists, and it is the one that needs an
/// analysis. [`normal`] stops at the first `ptr_add` whose step it cannot read, and what it hands
/// back is a fact about a base nobody knows the size of. Document 10's ranges do know something
/// about the step: an index the program has already tested, or one a loop counts, is bounded even
/// though it is not constant. So the walk carries on past the step, adding the low end of its
/// range to the offset and the width of the range to the size.
///
/// What comes out is a range of addresses the access can land in, and it is a [`Reach`] rather than
/// a [`Fact`] on purpose. Whether an object holding all of that range holds the one address the
/// access actually uses is [`reaches`], which asks a rule with the distance left opaque, so one
/// answer covers every value the step could take.
///
/// It is only ever asked with. What this returns must never be recorded as established, and the
/// one place it could be is the push in the `check_bounds` arm, which happens only where this
/// returned nothing or answered nothing. The reason is that the widened range is not what a check
/// proves. A check that runs and passes proves the address the program used was inside the object,
/// and says nothing at all about the rest of the range this function made up around it.
fn reach(func: &Func, ranges: Option<&mut Ranges<'_>>, asked: &Fact, at: Inst) -> Option<Reach> {
    let wide = spanned(func, ranges?, asked.base, asked.offset, asked.size, at)?;
    // Nothing was walked past, so this is the fact that came in and asking it again is work
    // somebody already did.
    (wide.base != asked.base).then_some(wide)
}

/// Which of the reasons a derivation check this pass could not read is kept for.
///
/// The census and nothing else. Whether the check goes has already been decided by the time this
/// runs, and what it answers is the question somebody reading `-fopt-info-missed` is actually
/// asking, which is what would have to be built for this pile to move.
///
/// It walks the same ground [`spread`] walks rather than being folded into it, because the two want
/// different things. [`spread`] wants an answer or nothing, and stopping at the first step it cannot
/// read is the fastest way to nothing. This wants to get as far as it can and name where it stopped,
/// so it runs only on checks that are staying and it is allowed to be the slower of the two.
///
/// The five that begin `nothing here says how big` are one refusal counted five ways. What is missing
/// in every one of them is how many bytes belong to the object, and where the pointer came from is
/// what says which piece of work would supply it: `__counted_by` and the type plane for a pointer out
/// of memory, section 7.5's summaries for one that was handed over, `crate::extents` reaching further
/// for a global, and the allocation summaries for one a call returned.
fn unreadable(func: &Func, ranges: Option<&mut Ranges<'_>>, check: Inst) -> &'static str {
    let args = &func[func[check].args];
    let (Some(&capability), Some(&from), Some(&to)) = (args.first(), args.get(1), args.get(2))
    else {
        return NO_EXTENT_OTHER;
    };
    if operand_of(func, capability, Opcode::CapOf, 0) != Some(from) {
        return NOT_ITS_CAPABILITY_DERIV;
    }
    // No ranges is a function with no walk in it that steps by a value, so every step here was a
    // constant, so the reader that gives up on two bases gave up on two bases.
    let Some(ranges) = ranges else { return TWO_BASES_DERIV };
    let (base, offset) = normal(func, from);
    let Some(near) = spanned(func, ranges, base, offset, 1, check) else {
        return NO_EXTENT_OTHER;
    };
    let (base, offset) = normal(func, to);
    let Some(far) = spanned(func, ranges, base, offset, 1, check) else {
        return NO_EXTENT_OTHER;
    };
    if near.base != far.base {
        return TWO_BASES_DERIV;
    }
    if declared(func, near.base).is_some() {
        return OVER_THE_LOCAL_DERIV;
    }
    match func[near.base].def {
        Def::Param { .. } => NO_EXTENT_HANDED,
        Def::Result { inst, .. } => match func[inst].opcode {
            Opcode::Load => NO_EXTENT_LOADED,
            Opcode::GlobalAddr => NO_EXTENT_GLOBAL,
            Opcode::Call | Opcode::CallIndirect => NO_EXTENT_RETURNED,
            _ => NO_EXTENT_OTHER,
        },
    }
}

/// The two ends of a derivation check, each as the range of addresses it can be at.
///
/// A derivation check asks whether the pointer that came out of a walk is still in the storage
/// instance the pointer that went in belongs to. [`derives`] answers that only when both ends
/// normalize to one base over constants, and past a step the constant reader gives up on they do
/// not, which is why this reads the check's operands again rather than taking what that worked
/// out. Each end becomes a range, and the two still have to be off one base or there is nothing
/// comparable to ask about.
///
/// One byte each, for the reason [`derives`] gives. Nothing here claims anything about how many
/// bytes are readable at either address.
///
/// The capability has to be the `cap_of` of the pointer that went in, for the reason [`addressed`]
/// gives.
fn spread(
    func: &Func,
    ranges: Option<&mut Ranges<'_>>,
    check: Inst,
    at: Inst,
) -> Option<(Reach, Reach)> {
    let ranges = ranges?;
    let args = &func[func[check].args];
    let &capability = args.first()?;
    let &from = args.get(1)?;
    let &to = args.get(2)?;
    if operand_of(func, capability, Opcode::CapOf, 0) != Some(from) {
        return None;
    }
    let (base, offset) = normal(func, from);
    let near = spanned(func, ranges, base, offset, 1, at)?;
    let (base, offset) = normal(func, to);
    let far = spanned(func, ranges, base, offset, 1, at)?;
    (near.base == far.base).then_some((near, far))
}

/// Every address a walk off `base` can reach, and how many bytes it takes when it gets there.
///
/// The loop is [`normal`]'s with one more thing to try. A `ptr_add` over a constant is walked
/// through the same way, and a `ptr_add` over a value is walked through when document 10's ranges
/// put numbers on that value: the low end of the range goes on the distance and the width of it on
/// the slack. Anything else is where the walk stops.
///
/// Nothing is returned when a step is a value the ranges say nothing useful about, rather than the
/// walk stopping there and handing back what it had. What it had would be a range off a `ptr_add`
/// nobody knows the size of, which answers nothing, so stopping would be a longer way of saying no.
fn spanned(
    func: &Func,
    ranges: &mut Ranges<'_>,
    base: Value,
    offset: i128,
    size: i128,
    at: Inst,
) -> Option<Reach> {
    let mut base = base;
    let mut low = offset;
    let mut width: i128 = 0;
    loop {
        // A constant step again, because past a step that needed a range there can be more of
        // them, and the frontend leaves a field offset as a constant under an array index.
        if let Some((from, step)) = walked(func, base) {
            low = low.checked_add(step)?;
            base = from;
            continue;
        }
        let Some(from) = operand_of(func, base, Opcode::PtrAdd, 0) else { break };
        let by = operand_of(func, base, Opcode::PtrAdd, 1)?;
        let (least, most) = ranges.at_inst(by, at).signed_bounds()?;
        low = low.checked_add(least)?;
        width = width.checked_add(most.checked_sub(least)?)?;
        base = from;
    }
    Some(Reach { base, low, width, size })
}

/// Whether any walk in this function steps by a value rather than a constant.
///
/// The question the ranges are built for. A function without one of these would pay for a copy of
/// the control flow graph and never ask anything of it.
/// Whether anything in this function says a lifetime is over.
///
/// Nothing emits `meta_end` today, so this is false everywhere and the frame slot rule in
/// [`Discharge::run`] is on for every function. It is written anyway, and written over the whole
/// function rather than along the walk, because the day something does emit one the cheap reading
/// is the wrong one: a lifetime that ended in one arm of a branch has ended for a check after the
/// join, and a walk down the dominator tree would not have seen it. Turning the rule off for the
/// function is the reading that stays right when that day comes, and the finer one is a job for
/// whoever makes `meta_end` appear.
fn ends_a_lifetime(func: &Func) -> bool {
    func.blocks().any(|block| func.insts(block).any(|inst| func[inst].opcode == Opcode::MetaEnd))
}

fn walks_by_a_value(func: &Func) -> bool {
    func.blocks().any(|block| {
        func.insts(block).any(|inst| {
            func[inst].opcode == Opcode::PtrAdd
                && func[func[inst].args].get(1).is_some_and(|&by| constant(func, by).is_none())
        })
    })
}

/// The pointer one `ptr_add` over a constant was computed from, and by how much.
fn walked(func: &Func, value: Value) -> Option<(Value, i128)> {
    let from = operand_of(func, value, Opcode::PtrAdd, 0)?;
    let by = operand_of(func, value, Opcode::PtrAdd, 1)?;
    Some((from, constant(func, by)?))
}

/// Operand `index` of the instruction that produced `value`, when that instruction is `opcode`.
pub(crate) fn operand_of(func: &Func, value: Value, opcode: Opcode, index: usize) -> Option<Value> {
    let Def::Result { inst, .. } = func[value].def else { return None };
    if func[inst].opcode != opcode {
        return None;
    }
    func[func[inst].args].get(index).copied()
}

/// The value of an integer constant, read with its own sign.
pub(crate) fn constant(func: &Func, value: Value) -> Option<i128> {
    let Def::Result { inst, .. } = func[value].def else { return None };
    if func[inst].opcode != Opcode::IConst {
        return None;
    }
    let Extra::Imm(imm) = func[inst].extra else { return None };
    let ty = func[value].ty;
    ty.is_int().then(|| func[imm].signed(ty))
}

/// Whether an established fact answers the check being asked about.
///
/// This function decides nothing. It puts the two together into the term the rule file is written
/// about and asks the table, which is the whole of section 7.7's split: the paragraph above worked
/// out that the two addresses are one value a constant apart, and whether that is enough is
/// somebody's proof rather than this file's opinion.
pub(crate) fn covers(fact: &Fact, asked: &Fact) -> bool {
    if fact.base != asked.base {
        return false;
    }
    let Some(delta) = asked.offset.checked_sub(fact.offset) else { return false };
    let mut question = Question::default();
    let at = question.opaque();
    let at = question.app("value.i64", &[at]);
    let span = question.number(fact.size);
    let span = question.app("iconst.i64", &[span]);
    let far = question.number(delta);
    let far = question.app("iconst.i64", &[far]);
    let reach = question.number(asked.size);
    let reach = question.app("iconst.i64", &[reach]);
    let term = question.app("covered.i64", &[at, span, far, reach]);
    match safety::TABLE.find(&question, term) {
        Some(found) => yes(&safety::TABLE, found.rule),
        None => false,
    }
}

/// Whether an object holds every address a walk can land on.
///
/// The companion to [`covers`] for the question [`reach`] asks, and it decides nothing either. It
/// puts the object and the range of addresses into the term the rule file is written about and
/// asks the table. The distance the program actually walks is opaque in the question, which is
/// what makes one answer cover every value it could take.
fn reaches(fact: &Fact, asked: &Reach) -> bool {
    if fact.base != asked.base {
        return false;
    }
    let Some(delta) = asked.low.checked_sub(fact.offset) else { return false };
    let mut question = Question::default();
    let at = question.opaque();
    let at = question.app("value.i64", &[at]);
    let span = question.number(fact.size);
    let span = question.app("iconst.i64", &[span]);
    let delta = question.number(delta);
    let delta = question.app("iconst.i64", &[delta]);
    let width = question.number(asked.width);
    let width = question.app("iconst.i64", &[width]);
    let size = question.number(asked.size);
    let size = question.app("iconst.i64", &[size]);
    let step = question.opaque();
    let step = question.app("value.i64", &[step]);
    let term = question.app("reached.i64", &[at, span, delta, width, size, step]);
    match safety::TABLE.find(&question, term) {
        Some(found) => yes(&safety::TABLE, found.rule),
        None => false,
    }
}

/// Whether the rule that fired answers yes.
///
/// A discharge rule replaces the question with a constant, and one is yes. Every rule in the file
/// answers that today, and reading it off the rule rather than assuming it is what keeps this
/// honest on the day one of them answers something else.
pub(crate) fn yes(table: &Table, rule: usize) -> bool {
    matches!(table.rules[rule].replacement, [Piece::App { .. }, Piece::Int(1)])
}

/// A term built to be asked about, and nothing else.
///
/// The rules are matched against this rather than against the function, because what is being asked
/// about is not in the function: it is what the walk worked out about two of its instructions. So
/// the subject is a small arena of exactly the term being asked, built fresh for each question and
/// thrown away with the answer.
#[derive(Debug, Default)]
pub(crate) struct Question {
    held: Vec<Held>,
}

/// One node of that term.
#[derive(Debug)]
enum Held {
    /// A number the pattern can read and a guard can be about.
    Int(i128),
    /// A head and its arguments.
    App(&'static str, Vec<usize>),
    /// Something with no structure, which is how an address the rule only names is written.
    Opaque,
}

impl Question {
    /// Adds a constant and gives back where it went.
    ///
    /// Named for what it adds rather than for what it holds, because the arena also answers
    /// [`Subject::int`] and one name for the two would read as though building a term and asking
    /// about one were the same act.
    pub(crate) fn number(&mut self, value: i128) -> usize {
        self.held.push(Held::Int(value));
        self.held.len() - 1
    }

    /// Adds an application of `head` to what is already in the arena.
    pub(crate) fn app(&mut self, head: &'static str, args: &[usize]) -> usize {
        self.held.push(Held::App(head, args.to_vec()));
        self.held.len() - 1
    }

    /// Adds something the rule can bind and cannot look inside.
    pub(crate) fn opaque(&mut self) -> usize {
        self.held.push(Held::Opaque);
        self.held.len() - 1
    }
}

impl Subject for Question {
    type Node = usize;

    fn head(&self, node: usize) -> Option<(&str, usize)> {
        match &self.held[node] {
            Held::App(head, args) => Some((head, args.len())),
            Held::Int(_) | Held::Opaque => None,
        }
    }

    fn arg(&self, node: usize, index: usize) -> usize {
        match &self.held[node] {
            Held::App(_, args) => args[index],
            // The walk only asks for an argument `head` said was there, so this is unreachable
            // rather than a case with an answer.
            Held::Int(_) | Held::Opaque => unreachable!("only an application has arguments"),
        }
    }

    fn int(&self, node: usize) -> Option<i128> {
        match self.held[node] {
            Held::Int(value) => Some(value),
            Held::App(..) | Held::Opaque => None,
        }
    }

    fn same(&self, a: usize, b: usize) -> bool {
        // Every node of a question is written once, so two places holding one thing are one place.
        a == b
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{
        AsmInfo, Block, BlockCallList, Builder, Extra, Flags, Func, Inst, InstData, IntPred,
        MemInfo, MemOrder, Opcode, Restrict, Signature, Type, Value,
    };

    use super::{DISCHARGE, Fact};
    use crate::stats::Kind;
    use crate::{Fuel, Pass, pass};

    /// A function taking a pointer, with one block, ready to have accesses put in it.
    fn blank() -> (Interner, Func, Block, Value) {
        let mut names = Interner::new();
        let name = names.intern("f");
        let mut func = Func::new(name, Signature::new().with_params(&[Type::PTR]));
        let block = func.create_block();
        let pointer = func.append_param(block, Type::PTR);
        (names, func, block, pointer)
    }

    /// Puts `cap_of` and a `check_bounds` over `size` bytes at `pointer` into a block.
    ///
    /// The same shape `rucc-safety` emits, written out here rather than reached for, because
    /// `rucc-opt` is rank 9 alongside `rucc-safety` and cannot depend on it.
    fn check(build: &mut Builder<'_>, pointer: Value, size: u64) {
        let args = build.func().push_values(&[pointer]);
        let capability = build.value(InstData { args, ..InstData::new(Opcode::CapOf) }, Type::CAP);
        let info = MemInfo {
            size,
            align: 1,
            order: MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        };
        let args = build.func().push_values(&[capability, pointer]);
        let extra = Extra::Mem(build.func().add_mem(info));
        build.inst(InstData { args, extra, ..InstData::new(Opcode::CheckBounds) }, &[]);
    }

    /// Puts `cap_of` and a `check_live` at `pointer` into a block.
    ///
    /// `rucc-safety` emits this straight after the bounds check for the same access and shares the
    /// one `cap_of` between the two. Sharing it is not what the pass reads, so the tests build a
    /// second one, which is the harder shape for it to accept.
    fn live(build: &mut Builder<'_>, pointer: Value) {
        let args = build.func().push_values(&[pointer]);
        let capability = build.value(InstData { args, ..InstData::new(Opcode::CapOf) }, Type::CAP);
        let args = build.func().push_values(&[capability, pointer]);
        build.inst(InstData { args, ..InstData::new(Opcode::CheckLive) }, &[]);
    }

    /// Both checks in front of one access, in the order `rucc-safety` writes them.
    fn access(build: &mut Builder<'_>, pointer: Value, size: u64) {
        check(build, pointer, size);
        live(build, pointer);
    }

    /// A pointer `bytes` past another one.
    fn past(build: &mut Builder<'_>, pointer: Value, bytes: i128) -> Value {
        let offset = build.iconst(Type::int(64), bytes);
        let args = build.func().push_values(&[pointer, offset]);
        build.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR)
    }

    /// Puts the flag `crate::extents` writes onto every check in a function.
    ///
    /// The pass reads what the IR says, so what a test has to build is an IR that says it. Working
    /// out which checks deserve it is `crate::extents`, is about a module rather than a function,
    /// and has its own tests.
    fn marked(func: &mut Func) {
        flagged(func, Flags::STATIC);
    }

    /// Puts that flag on every check in the function, the way an annotator before the pipeline
    /// would have.
    fn flagged(func: &mut Func, flag: Flags) {
        let insts: Vec<Inst> =
            func.blocks().flat_map(|block| func.insts(block).collect::<Vec<_>>()).collect();
        for inst in insts {
            let check = matches!(
                func[inst].opcode,
                Opcode::CheckBounds | Opcode::CheckLive | Opcode::CheckDeriv
            );
            if check {
                func[inst].flags |= flag;
            }
        }
    }

    /// How many checks are left in a function.
    fn checks(func: &Func) -> usize {
        func.blocks()
            .flat_map(|block| func.insts(block).collect::<Vec<_>>())
            .filter(|&inst| func[inst].opcode == Opcode::CheckBounds)
            .count()
    }

    /// How many lifetime checks are left in a function.
    fn lives(func: &Func) -> usize {
        func.blocks()
            .flat_map(|block| func.insts(block).collect::<Vec<_>>())
            .filter(|&inst| func[inst].opcode == Opcode::CheckLive)
            .count()
    }

    fn run(func: &mut Func) -> crate::Stats {
        DISCHARGE.run(func, &mut crate::machine::fixtures::analyses(), &mut Fuel::unlimited())
    }

    /// The same, with one of the measurement's variants rather than the pass the pipeline runs.
    fn run_with(pass: &super::Discharge, func: &mut Func) -> crate::Stats {
        pass.run(func, &mut crate::machine::fixtures::analyses(), &mut Fuel::unlimited())
    }

    #[test]
    fn a_run_that_may_only_ask_an_object_leaves_what_dominance_would_have_taken() {
        // Two checks of the same bytes on a pointer that came from outside. Nothing here says how
        // big the object is, so the only thing that could answer the second one is the first one
        // having run, and a run that may not ask that has to keep both.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, 4);
        check(&mut build, pointer, 4);
        build.ret(&[]);
        let stats = run_with(&super::OBJECTS, &mut func);
        assert_eq!(checks(&func), 2);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED), 0);
    }

    #[test]
    fn a_run_that_may_only_ask_dominance_takes_the_second_check_of_the_same_bytes() {
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, 4);
        check(&mut build, pointer, 4);
        build.ret(&[]);
        let stats = run_with(&super::DOMINANCE, &mut func);
        assert_eq!(checks(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED), 1);
    }

    #[test]
    fn a_run_that_may_only_ask_dominance_leaves_a_check_inside_a_local() {
        // The other way round. One check, nothing in front of it, and the bytes are inside an
        // `alloca` whose size is written on it. Only the object can answer that one.
        let (_, mut func, block, _) = blank();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build, 16);
        check(&mut build, slot, 4);
        build.ret(&[]);
        let stats = run_with(&super::DOMINANCE, &mut func);
        assert_eq!(checks(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_LOCAL), 0);
        assert_eq!(
            run_with(&super::OBJECTS, &mut func).count(Kind::Optimized, super::REMOVED_LOCAL),
            1
        );
    }

    #[test]
    fn the_measurement_variants_answer_to_names_of_their_own() {
        // A run that cannot be reached by a flag is a run nobody can measure with.
        let names: Vec<&str> = [
            &DISCHARGE,
            &super::OBJECTS,
            &super::DOMINANCE,
            &super::SUMMARIES,
            &super::NARROW,
            &super::EVERY,
        ]
        .iter()
        .map(|pass| pass.name())
        .collect();
        assert_eq!(
            names,
            [
                "discharge",
                "discharge-objects",
                "discharge-dominance",
                "discharge-summaries",
                "discharge-narrow",
                "discharge-every"
            ]
        );
        for name in names {
            assert!(pass::find(name).is_some(), "`{name}` is not in the pass list");
        }
    }

    #[test]
    fn a_second_check_of_the_same_bytes_goes() {
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, 4);
        check(&mut build, pointer, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED), 1);
    }

    #[test]
    fn a_check_over_a_length_the_program_worked_out_is_not_this_pass_to_read() {
        // Section 7.4's hoisted check covers as many bytes as its loop runs times, which is a value
        // and not a number. Every range this pass compares is a pair of numbers, so it says so and
        // leaves the check alone rather than reading the payload, whose size is one element.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, 4);
        let args = build.func().push_values(&[pointer]);
        let capability = build.value(InstData { args, ..InstData::new(Opcode::CapOf) }, Type::CAP);
        let bytes = build.iconst(Type::int(64), 4);
        let info = MemInfo {
            size: 4,
            align: 1,
            order: MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        };
        let extra = Extra::Mem(build.func().add_mem(info));
        let args = build.func().push_values(&[capability, pointer, bytes]);
        build.inst(InstData { args, extra, ..InstData::new(Opcode::CheckBounds) }, &[]);
        build.ret(&[]);

        let stats = run(&mut func);
        assert_eq!(checks(&func), 2, "the second one stays");
        assert_eq!(stats.count(Kind::Missed, super::COMPUTED_EXTENT), 1);
    }

    #[test]
    fn a_check_of_bytes_inside_a_checked_range_goes() {
        // Four bytes at offset four, inside sixteen bytes at offset zero. This is the shape the
        // whole pass is for: a struct whose fields are read one after another through one pointer.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, 16);
        let field = past(&mut build, pointer, 4);
        check(&mut build, field, 4);
        build.ret(&[]);
        run(&mut func);
        assert_eq!(checks(&func), 1);
    }

    #[test]
    fn a_check_of_bytes_past_the_end_of_a_checked_range_stays() {
        // Four bytes at offset fourteen is two bytes past the end of the sixteen that were
        // checked, and those two bytes are what the check is for.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, 16);
        let over = past(&mut build, pointer, 14);
        check(&mut build, over, 4);
        build.ret(&[]);
        assert!(!run(&mut func).changed());
        assert_eq!(checks(&func), 2);
    }

    #[test]
    fn a_check_of_bytes_before_a_checked_range_stays() {
        // The guard's `delta` is not negative, and this is why. A read four bytes below what was
        // checked is a read of somebody else's memory, and it is the bug the check exists for.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, 16);
        let under = past(&mut build, pointer, -4);
        check(&mut build, under, 4);
        build.ret(&[]);
        assert!(!run(&mut func).changed());
        assert_eq!(checks(&func), 2);
    }

    #[test]
    fn a_check_through_a_pointer_nothing_relates_to_the_first_stays() {
        let mut names = Interner::new();
        let name = names.intern("two");
        let mut func = Func::new(name, Signature::new().with_params(&[Type::PTR, Type::PTR]));
        let block = func.create_block();
        let one = func.append_param(block, Type::PTR);
        let other = func.append_param(block, Type::PTR);
        let mut build = Builder::new(&mut func, block);
        check(&mut build, one, 16);
        check(&mut build, other, 4);
        build.ret(&[]);
        assert!(!run(&mut func).changed());
        assert_eq!(checks(&func), 2);
    }

    #[test]
    fn a_check_a_call_stands_between_stays_and_is_counted() {
        // The conservatism the module comment argues for, and the number that says what it costs.
        let (mut names, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, 16);
        let callee = names.intern("might_free");
        let signature = build.func().add_signature(Signature::new());
        build.call(callee, signature, &[]);
        check(&mut build, pointer, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert!(!stats.changed());
        assert_eq!(checks(&func), 2);
        assert_eq!(stats.count(Kind::Missed, super::PAST_A_CALL), 1);
    }

    #[test]
    fn a_check_a_call_that_cannot_free_stands_between_goes() {
        // The other side of the paragraph above. The summary said this call reaches nothing that
        // ends a lifetime, so the range the first check established is still one range.
        let (mut names, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, 16);
        let callee = names.intern("counts_them");
        let signature = build.func().add_signature(Signature::new());
        let call = build.call(callee, signature, &[]);
        check(&mut build, pointer, 4);
        build.ret(&[]);
        func[call].flags |= Flags::NOFREE;
        let stats = run(&mut func);
        assert_eq!(checks(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED), 1);
        assert_eq!(stats.count(Kind::Missed, super::PAST_A_CALL), 0);
    }

    #[test]
    fn inline_assembly_throws_the_facts_away_whatever_it_is_flagged() {
        // There is no flag that would make this safe. The template is text the compiler does not
        // read, so nothing worked anything out about what it reaches.
        let (mut names, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, 16);
        build.inline_asm(
            AsmInfo {
                template: names.intern("nop"),
                constraints: names.intern(""),
                clobbers: names.intern(""),
                targets: BlockCallList::EMPTY,
            },
            &[],
            &[],
            Flags::NONE,
        );
        check(&mut build, pointer, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert!(!stats.changed());
        assert_eq!(stats.count(Kind::Missed, super::PAST_A_CALL), 1);
    }

    #[test]
    fn a_check_that_only_one_path_covers_stays() {
        // The dominator tree is what makes this right. The check in the arm covers the one in the
        // join on one path and not on the other, and a check that goes has to be one that ran.
        let (_, mut func, block, pointer) = blank();
        let arm = func.create_block();
        let join = func.create_block();
        let mut build = Builder::new(&mut func, block);
        let condition = build.iconst(Type::int(32), 1);
        build.br_if(condition, arm, &[], join, &[]);
        let mut build = Builder::new(&mut func, arm);
        check(&mut build, pointer, 16);
        build.jump(join, &[]);
        let mut build = Builder::new(&mut func, join);
        check(&mut build, pointer, 4);
        build.ret(&[]);
        assert!(!run(&mut func).changed());
        assert_eq!(checks(&func), 2);
    }

    #[test]
    fn a_check_a_dominating_block_covers_goes() {
        let (_, mut func, block, pointer) = blank();
        let after = func.create_block();
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, 16);
        build.jump(after, &[]);
        let mut build = Builder::new(&mut func, after);
        let field = past(&mut build, pointer, 8);
        check(&mut build, field, 8);
        build.ret(&[]);
        run(&mut func);
        assert_eq!(checks(&func), 1);
    }

    #[test]
    fn fuel_stops_the_removing_and_not_the_looking() {
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, 4);
        check(&mut build, pointer, 4);
        check(&mut build, pointer, 4);
        build.ret(&[]);
        let mut fuel = Fuel::of(1);
        let stats = DISCHARGE.run(&mut func, &mut crate::machine::fixtures::analyses(), &mut fuel);
        assert_eq!(checks(&func), 2);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED), 1);
        assert_eq!(stats.count(Kind::Missed, super::NO_FUEL), 1);
    }

    #[test]
    fn a_second_lifetime_check_of_the_same_address_goes() {
        // The narrow fact on its own, with no range around it to widen into.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        live(&mut build, pointer);
        live(&mut build, pointer);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(lives(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_LIVE), 1);
    }

    #[test]
    fn a_lifetime_check_inside_a_checked_range_goes() {
        // The shape the pass is for, with both halves of it. Sixteen bytes are checked and found
        // alive, then a field four bytes in is read, and neither check in front of it survives.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        access(&mut build, pointer, 16);
        let field = past(&mut build, pointer, 4);
        access(&mut build, field, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 1);
        assert_eq!(lives(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_LIVE), 1);
    }

    #[test]
    fn a_lifetime_check_outside_every_checked_range_stays() {
        // Four bytes at offset twenty are past the sixteen that were checked, so nothing says the
        // address is in the instance that was found alive, and it might be in no instance at all.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        access(&mut build, pointer, 16);
        let over = past(&mut build, pointer, 20);
        live(&mut build, over);
        build.ret(&[]);
        assert!(!run(&mut func).changed());
        assert_eq!(lives(&func), 2);
    }

    #[test]
    fn a_lifetime_check_with_no_range_around_it_does_not_widen() {
        // Without the bounds check the first lifetime check speaks only for its own address, so
        // the one four bytes along is a different question and stays.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        live(&mut build, pointer);
        let field = past(&mut build, pointer, 4);
        live(&mut build, field);
        build.ret(&[]);
        assert!(!run(&mut func).changed());
        assert_eq!(lives(&func), 2);
    }

    #[test]
    fn a_lifetime_check_a_call_stands_between_stays_and_is_counted() {
        // Section 8.8's number. This is the one the summaries were written for.
        let (mut names, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        access(&mut build, pointer, 16);
        let callee = names.intern("might_free");
        let signature = build.func().add_signature(Signature::new());
        build.call(callee, signature, &[]);
        let field = past(&mut build, pointer, 4);
        live(&mut build, field);
        build.ret(&[]);
        let stats = run(&mut func);
        assert!(!stats.changed());
        assert_eq!(lives(&func), 2);
        assert_eq!(stats.count(Kind::Missed, super::PAST_A_CALL_LIVE), 1);
    }

    #[test]
    fn a_lifetime_check_a_call_that_cannot_free_stands_between_goes() {
        let (mut names, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        access(&mut build, pointer, 16);
        let callee = names.intern("counts_them");
        let signature = build.func().add_signature(Signature::new());
        let call = build.call(callee, signature, &[]);
        let field = past(&mut build, pointer, 4);
        live(&mut build, field);
        build.ret(&[]);
        func[call].flags |= Flags::NOFREE;
        let stats = run(&mut func);
        assert_eq!(lives(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_LIVE), 1);
    }

    #[test]
    fn ending_a_lifetime_throws_the_facts_away() {
        // Nothing emits `meta_end` yet, so this is the test that says what will happen when
        // something does, rather than a test of anything the compiler does today.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        access(&mut build, pointer, 16);
        let size = build.iconst(Type::int(64), 16);
        let args = build.func().push_values(&[pointer, size]);
        build.inst(InstData { args, ..InstData::new(Opcode::MetaEnd) }, &[]);
        access(&mut build, pointer, 16);
        build.ret(&[]);
        let stats = run(&mut func);
        assert!(!stats.changed());
        assert_eq!(checks(&func), 2);
        assert_eq!(lives(&func), 2);
        assert_eq!(stats.count(Kind::Missed, super::PAST_A_CALL), 1);
        assert_eq!(stats.count(Kind::Missed, super::PAST_A_CALL_LIVE), 1);
    }

    #[test]
    fn fuel_runs_out_over_both_kinds_of_check() {
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        access(&mut build, pointer, 16);
        access(&mut build, pointer, 4);
        build.ret(&[]);
        let mut fuel = Fuel::of(1);
        let stats = DISCHARGE.run(&mut func, &mut crate::machine::fixtures::analyses(), &mut fuel);
        assert_eq!(checks(&func), 1);
        assert_eq!(lives(&func), 2);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED), 1);
        assert_eq!(stats.count(Kind::Missed, super::NO_FUEL_LIVE), 1);
    }

    #[test]
    fn a_distance_too_large_to_be_a_real_access_is_not_discharged() {
        // The guard's bound. The two readings of the arithmetic agree while the numbers stay
        // small, so a rule proved at sixty four bits is not asked about anything else. Nothing
        // here is wrong, it simply is not proved, and a check that is not proved to be unnecessary
        // stays.
        let huge = i128::from(u64::MAX) * 4;
        let fact = Fact { base: Value::new(0), offset: 0, size: huge };
        let asked = Fact { base: Value::new(0), offset: huge / 2, size: 4 };
        assert!(!super::covers(&fact, &asked));
    }

    #[test]
    fn a_range_of_addresses_wider_than_the_rule_allows_is_not_discharged() {
        // The guard on `reached.i64` bounds each of the three numbers at four gigabytes, for the
        // reason the rule file gives: past there the compiler's `i128` reading of the guard and the
        // solver's sixty four bit reading part company, and a rule proved under one and run under
        // the other is a rule proved about arithmetic that is not happening. A step whose range is
        // that wide is the usual case rather than a corner, since an index nothing has bounded says
        // nothing about where the access lands.
        let base = Value::new(0);
        let whole = Fact::whole(base, i128::from(u64::MAX) * 4);
        let asked = super::Reach { base, low: 0, width: i128::from(u64::MAX), size: 4 };
        assert!(!super::reaches(&whole, &asked));
    }

    #[test]
    fn a_range_of_addresses_that_ends_where_the_object_does_is_discharged() {
        // Sixteen bytes, a step somewhere in nought to eleven, four bytes read. The last address
        // the walk can reach is the last one in the object, which is inside it.
        let base = Value::new(0);
        let whole = Fact::whole(base, 16);
        let asked = super::Reach { base, low: 0, width: 12, size: 4 };
        assert!(super::reaches(&whole, &asked));
        let over = super::Reach { base, low: 0, width: 13, size: 4 };
        assert!(!super::reaches(&whole, &over), "one byte further runs off the end");
    }

    #[test]
    fn a_walk_by_a_bounded_step_off_a_local_takes_its_derivation_check_with_it() {
        // The shape `derives` cannot read at all: the pointer that went in is the slot and the one
        // that came out is a value past it, so the two are not one base and two constants. Both
        // ends widen to the slot, the slot holds both ranges, and one thing holding both is what a
        // derivation check asks about.
        let (_, mut func, block, _, index) = indexed();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build, 16);
        let step = low_bits(&mut build, index, 7);
        let at = walk(&mut build, slot, step);
        deriv(&mut build, slot, at, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(derivs(&func), 0);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_DERIV_RANGE), 1);
    }

    #[test]
    fn a_walk_that_can_leave_the_local_keeps_its_derivation_check() {
        // Nought to fifteen off a slot of eight. Every step is bounded and the answer is still no,
        // because the question is whether the slot holds every address the walk can reach.
        let (_, mut func, block, _, index) = indexed();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build, 8);
        let step = low_bits(&mut build, index, 15);
        let at = walk(&mut build, slot, step);
        deriv(&mut build, slot, at, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(derivs(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_DERIV_RANGE), 0);
        assert_eq!(stats.count(Kind::Missed, super::OVER_THE_LOCAL_DERIV), 1);
    }

    #[test]
    fn a_lifetime_check_a_bounded_walk_lands_inside_a_checked_range_goes() {
        // An access over thirty two bytes establishes the range, and the lifetime check beside it
        // makes that range one a check found alive. The lifetime check on the walk then goes,
        // because every address the walk can reach is in the range that was found alive.
        //
        // Written off a parameter rather than a slot because a slot answers the narrow question on
        // its own. What has to answer this one is a range a check was passed on.
        let (_, mut func, block, pointer, index) = indexed();
        let mut build = Builder::new(&mut func, block);
        access(&mut build, pointer, 32);
        let step = low_bits(&mut build, index, 7);
        let at = walk(&mut build, pointer, step);
        live(&mut build, at);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(lives(&func), 1, "the one in front of the access stays");
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_LIVE_RANGE), 1);
    }

    #[test]
    fn a_lifetime_check_a_bounded_walk_can_leave_the_checked_range_keeps_it() {
        // The same over eight bytes, under a walk that can go fifteen past the start. A range of
        // eight bytes does not hold an address fifteen along from where it begins.
        let (_, mut func, block, pointer, index) = indexed();
        let mut build = Builder::new(&mut func, block);
        access(&mut build, pointer, 8);
        let step = low_bits(&mut build, index, 15);
        let at = walk(&mut build, pointer, step);
        live(&mut build, at);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(lives(&func), 2);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_LIVE_RANGE), 0);
    }

    /// A stack slot of `size` bytes, in the entry block where the verifier wants one.
    fn local(build: &mut Builder<'_>, size: u64) -> Value {
        let info = MemInfo {
            size,
            align: 8,
            order: MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        };
        let extra = Extra::Mem(build.func().add_mem(info));
        build.value(InstData { extra, ..InstData::new(Opcode::Alloca) }, Type::PTR)
    }

    /// A function taking a pointer and an index, with one block.
    fn indexed() -> (Interner, Func, Block, Value, Value) {
        let mut names = Interner::new();
        let name = names.intern("f");
        let mut func = Func::new(name, Signature::new().with_params(&[Type::PTR, Type::int(64)]));
        let block = func.create_block();
        let pointer = func.append_param(block, Type::PTR);
        let index = func.append_param(block, Type::int(64));
        (names, func, block, pointer, index)
    }

    /// A pointer a value past another one.
    fn walk(build: &mut Builder<'_>, pointer: Value, by: Value) -> Value {
        let args = build.func().push_values(&[pointer, by]);
        build.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR)
    }

    /// The low bits of a value, which is a step the ranges can put a number on.
    fn low_bits(build: &mut Builder<'_>, value: Value, mask: i128) -> Value {
        let bits = build.iconst(Type::int(64), mask);
        build.binary(Opcode::And, value, bits, Flags::NONE)
    }

    #[test]
    fn a_walk_by_a_step_the_ranges_bound_inside_a_local_goes() {
        // Section 7.2's third source. The step is not a constant, so the walk stops at the
        // `ptr_add` and the fact that comes out is about a base nobody knows the size of. What
        // the ranges say is that the step is somewhere in nought to seven, so the four bytes the
        // access wants are somewhere in nought to eleven, and all of that is inside the sixteen
        // the slot is.
        let (_, mut func, block, _, index) = indexed();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build, 16);
        let step = low_bits(&mut build, index, 7);
        let at = walk(&mut build, slot, step);
        check(&mut build, at, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 0);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_RANGE), 1);
    }

    #[test]
    fn a_walk_by_a_step_the_ranges_cannot_bound_is_left_alone() {
        // The same function with the mask taken off. A parameter can be anything, so the range of
        // addresses the walk reaches is the whole of memory and no slot covers it.
        let (_, mut func, block, _, index) = indexed();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build, 16);
        let at = walk(&mut build, slot, index);
        check(&mut build, at, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_RANGE), 0);
    }

    #[test]
    fn a_walk_a_bounded_step_can_take_off_the_end_of_a_local_is_left_alone() {
        // Nought to seven again, four bytes again, and a slot of eight this time. The step being
        // bounded is not the question. The question is whether every address it can reach is
        // inside the slot, and seven plus four is not.
        let (_, mut func, block, _, index) = indexed();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build, 8);
        let step = low_bits(&mut build, index, 7);
        let at = walk(&mut build, slot, step);
        check(&mut build, at, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_RANGE), 0);
    }

    #[test]
    fn a_constant_step_past_a_bounded_one_is_walked_too() {
        // A field of an element of an array of structs, which is the shape this is for. The array
        // index needs a range and the field offset does not, and the walk has to get through both.
        let (_, mut func, block, _, index) = indexed();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build, 32);
        let step = low_bits(&mut build, index, 15);
        let element = walk(&mut build, slot, step);
        let field = past(&mut build, element, 8);
        check(&mut build, field, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 0);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_RANGE), 1);
    }

    #[test]
    fn what_a_range_discharge_records_is_the_bytes_and_not_the_range() {
        // The second check is the same bytes as the first, and the first went because a made up
        // range around it was inside the slot. What the first one proved is that those bytes are
        // in the slot, so the second one goes on that rather than on the ranges being asked all
        // over again.
        let (_, mut func, block, _, index) = indexed();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build, 16);
        let step = low_bits(&mut build, index, 7);
        let at = walk(&mut build, slot, step);
        check(&mut build, at, 4);
        check(&mut build, at, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 0);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_RANGE), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED), 1);
    }

    /// A stack slot whose size the program works out, which is what a variable length array is.
    fn growable(build: &mut Builder<'_>, size: Value) -> Value {
        let info = MemInfo {
            size: 0,
            align: 8,
            order: MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        };
        let extra = Extra::Mem(build.func().add_mem(info));
        let args = build.func().push_values(&[size]);
        build.value(InstData { args, extra, ..InstData::new(Opcode::Alloca) }, Type::PTR)
    }

    #[test]
    fn a_check_of_bytes_inside_a_local_goes_with_nothing_in_front_of_it() {
        // Section 7.2's first source. No check established this and none had to: an `alloca` of
        // sixteen bytes is sixteen bytes of one storage instance because that is what it makes.
        let (_, mut func, block, _) = blank();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build, 16);
        let field = past(&mut build, slot, 8);
        check(&mut build, field, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 0);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_LOCAL), 1);
    }

    #[test]
    fn a_check_past_the_end_of_a_local_stays() {
        // The slot is sixteen bytes and the access runs to twenty. Nothing about it being a local
        // says anything about the four bytes after it, which belong to whatever the frame puts
        // there next.
        let (_, mut func, block, _) = blank();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build, 16);
        let field = past(&mut build, slot, 16);
        check(&mut build, field, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_LOCAL), 0);
    }

    #[test]
    fn a_check_of_bytes_inside_a_local_goes_across_a_call() {
        // The other half of what makes the fact worth having. A callee cannot free a frame slot,
        // so unlike everything the walk carries this one is not thrown away at a call.
        let (mut names, mut func, block, _) = blank();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build, 16);
        let callee = names.intern("might_free");
        let signature = build.func().add_signature(Signature::new());
        build.call(callee, signature, &[]);
        check(&mut build, slot, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 0);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_LOCAL), 1);
        assert_eq!(stats.count(Kind::Missed, super::PAST_A_CALL), 0);
    }

    #[test]
    fn a_check_inside_a_variable_length_array_stays() {
        // How many bytes it is is a value the program works out, and the payload's size field
        // reads zero. A pass that read it anyway would discharge every check in the array.
        let (_, mut func, block, _) = blank();
        let mut build = Builder::new(&mut func, block);
        let bytes = build.iconst(Type::int(64), 64);
        let slot = growable(&mut build, bytes);
        check(&mut build, slot, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_LOCAL), 0);
    }

    #[test]
    fn a_lifetime_check_in_a_local_goes_with_nothing_in_front_of_it() {
        // The frame slot rule, and the point is that neither of these has a check in front of it.
        // A slot is alive until the function returns, so a lifetime check anywhere inside one is
        // asking a question the `alloca` already answered.
        let (_, mut func, block, _) = blank();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build, 16);
        live(&mut build, slot);
        let field = past(&mut build, slot, 12);
        live(&mut build, field);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(lives(&func), 0);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_LIVE_LOCAL), 2);
    }

    #[test]
    fn a_lifetime_check_past_the_end_of_a_local_stays() {
        // The slot answers for its own bytes and no further, so an address outside it is a
        // different instance and a question nothing has answered.
        let (_, mut func, block, _) = blank();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build, 16);
        live(&mut build, slot);
        let field = past(&mut build, slot, 24);
        live(&mut build, field);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(lives(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_LIVE_LOCAL), 1);
    }

    #[test]
    fn something_ending_a_lifetime_turns_the_frame_slot_rule_off() {
        // The gate, and with it the widening the frame slot rule usually hides. With a `meta_end`
        // anywhere in the function the slot answers nothing, so the first check stays and pays,
        // and what takes the second one out is the first one widened to the whole slot.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build, 16);
        live(&mut build, slot);
        let field = past(&mut build, slot, 12);
        live(&mut build, field);
        let size = build.iconst(Type::int(64), 16);
        let args = build.func().push_values(&[pointer, size]);
        build.inst(InstData { args, ..InstData::new(Opcode::MetaEnd) }, &[]);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(lives(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_LIVE_LOCAL), 0);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_LIVE), 1);
    }

    /// Puts `cap_of` and a `check_deriv` for a walk from `from` to `to` into a block.
    ///
    /// The stride is the width of one element, which is what `rucc-safety` passes and what the
    /// runtime uses for a pointer that walked off the near end. This pass does not read it.
    fn deriv(build: &mut Builder<'_>, from: Value, to: Value, stride: i128) {
        let args = build.func().push_values(&[from]);
        let capability = build.value(InstData { args, ..InstData::new(Opcode::CapOf) }, Type::CAP);
        let width = build.iconst(Type::int(64), stride);
        let args = build.func().push_values(&[capability, from, to, width]);
        build.inst(InstData { args, ..InstData::new(Opcode::CheckDeriv) }, &[]);
    }

    /// How many derivation checks are left in a function.
    fn derivs(func: &Func) -> usize {
        func.blocks()
            .flat_map(|block| func.insts(block).collect::<Vec<_>>())
            .filter(|&inst| func[inst].opcode == Opcode::CheckDeriv)
            .count()
    }

    #[test]
    fn a_walk_inside_a_checked_range_goes() {
        // Sixteen bytes were checked, and the walk goes from the start of them to eight in. Both
        // ends are in one range, so the second address is in the instance the first belongs to.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, 16);
        let field = past(&mut build, pointer, 8);
        deriv(&mut build, pointer, field, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(derivs(&func), 0);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_DERIV), 1);
    }

    #[test]
    fn a_walk_that_leaves_the_checked_range_stays() {
        // Four bytes were checked and the walk goes eight past them. Nothing here says the two
        // addresses are in one instance, which is the whole of what the check is about.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, 4);
        let field = past(&mut build, pointer, 8);
        deriv(&mut build, pointer, field, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(derivs(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_DERIV), 0);
    }

    #[test]
    fn two_ranges_holding_one_end_each_do_not_answer_a_walk() {
        // The case the one fact rule is written for. Both addresses have been checked, so both are
        // inside some instance, and nothing says it is the same one. The walk stays.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, 4);
        let field = past(&mut build, pointer, 64);
        check(&mut build, field, 4);
        deriv(&mut build, pointer, field, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(derivs(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_DERIV), 0);
    }

    #[test]
    fn a_walk_inside_a_local_goes_with_nothing_in_front_of_it() {
        // The shape almost every derivation check in real code has: a field of a local struct.
        // `rucc-safety` emits the walk before the bounds check on what it produced, so a fact from
        // an earlier check is usually the wrong size for it and the local is what answers.
        let (_, mut func, block, _) = blank();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build, 16);
        let field = past(&mut build, slot, 8);
        deriv(&mut build, slot, field, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(derivs(&func), 0);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_DERIV_LOCAL), 1);
    }

    #[test]
    fn a_walk_off_the_end_of_a_local_stays() {
        // Where the slot stops is where the fact stops. One past the end is the case the runtime
        // has slack for and this pass does not use any of it.
        let (_, mut func, block, _) = blank();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build, 16);
        let field = past(&mut build, slot, 16);
        deriv(&mut build, slot, field, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(derivs(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_DERIV_LOCAL), 0);
    }

    #[test]
    fn a_walk_a_call_stands_between_stays_and_is_counted() {
        // The same price the other two kinds pay, reported the same way, so the cost of not
        // trusting a call is a number per function rather than a paragraph.
        let (mut names, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, 16);
        let callee = names.intern("might_free");
        let signature = build.func().add_signature(Signature::new());
        build.call(callee, signature, &[]);
        let field = past(&mut build, pointer, 8);
        deriv(&mut build, pointer, field, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(derivs(&func), 1);
        assert_eq!(stats.count(Kind::Missed, super::PAST_A_CALL_DERIV), 1);
    }

    #[test]
    fn a_check_the_module_says_is_inside_a_global_goes_with_nothing_in_front_of_it() {
        // The other half of section 7.2's first source. The size of a global lives on the module
        // and this pass is given one function, so the answer arrives as a flag `crate::extents`
        // wrote before the pipeline started, and all three kinds carry it.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        let field = past(&mut build, pointer, 8);
        deriv(&mut build, pointer, field, 1);
        access(&mut build, field, 4);
        build.ret(&[]);
        marked(&mut func);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 0);
        assert_eq!(lives(&func), 0);
        assert_eq!(derivs(&func), 0);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_STATIC), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_LIVE_STATIC), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_DERIV_STATIC), 1);
    }

    #[test]
    fn a_check_the_module_says_every_caller_hands_in_goes_with_nothing_in_front_of_it() {
        // Section 7.5's summaries, arriving the same way a global's extent does and for the same
        // reason: which object a caller passes is a fact about a different function. What the flag
        // says is an extent and a lifetime, because the objects `crate::params` believes are a
        // caller's frame slot and a global and both are alive for as long as the call runs.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        let field = past(&mut build, pointer, 8);
        deriv(&mut build, pointer, field, 1);
        access(&mut build, field, 4);
        build.ret(&[]);
        flagged(&mut func, Flags::HANDED);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 0);
        assert_eq!(lives(&func), 0);
        assert_eq!(derivs(&func), 0);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_HANDED), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_LIVE_HANDED), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_DERIV_HANDED), 1);
    }

    /// A function that takes an index, allocates `size` bytes and tests the answer against null.
    ///
    /// Gives back the block where the test has passed, the block where it has not, the pointer and
    /// the index. The flag is put on by hand, because which calls deserve it is a question about a
    /// module and `crate::heap` is what answers it.
    ///
    /// The index is there for the tests about a walk by a value. A parameter on its own is any
    /// number at all, so a test that wants a bounded one puts [`low_bits`] over it the same way the
    /// local tests do.
    fn allocation(size: i128) -> (Interner, Func, Block, Block, Value, Value) {
        let mut names = Interner::new();
        let name = names.intern("f");
        let mut func = Func::new(name, Signature::new().with_params(&[Type::int(64)]));
        let entry = func.create_block();
        let inside = func.create_block();
        let outside = func.create_block();
        let index = func.append_param(entry, Type::int(64));
        let mut build = Builder::new(&mut func, entry);
        let signature = build.func().add_signature(
            Signature::new().with_params(&[Type::int(64)]).with_returns(&[Type::PTR]),
        );
        let bytes = build.iconst(Type::int(64), size);
        let call = build.call(names.intern("malloc"), signature, &[bytes]);
        let at = build.func();
        at[call].flags |= Flags::HEAP;
        let pointer = at[call].results().next().expect("a call that gives back a pointer");
        let zero = build.iconst(Type::int(64), 0);
        let null = build.unary(Opcode::IntToPtr, zero, Type::PTR);
        let condition = build.icmp(IntPred::Ne, pointer, null);
        build.br_if(condition, inside, &[], outside, &[]);
        let mut build = Builder::new(&mut func, outside);
        build.ret(&[]);
        (names, func, inside, outside, pointer, index)
    }

    #[test]
    fn a_check_inside_an_allocation_the_program_tested_goes() {
        // The third of the objects whose extent nobody had to check for. `malloc(16)` says how
        // many bytes it made in the call, and the branch on null is what makes it true here.
        let (_, mut func, inside, _, pointer, _) = allocation(16);
        let mut build = Builder::new(&mut func, inside);
        let field = past(&mut build, pointer, 8);
        deriv(&mut build, pointer, field, 1);
        access(&mut build, field, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 0);
        assert_eq!(derivs(&func), 0);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_MADE), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_DERIV_MADE), 1);
        // The lifetime check is the one an allocation says nothing about, because a `free` in this
        // same function can end it, and it is what reports a use after free.
        assert_eq!(lives(&func), 1);
    }

    #[test]
    fn a_check_on_an_allocation_nobody_tested_stays() {
        // Down the other arm the pointer is null, a null pointer is inside no object at all, and
        // the check is one that is supposed to fail.
        let (_, mut func, _, outside, pointer, _) = allocation(16);
        let mut build = Builder::new(&mut func, outside);
        access(&mut build, pointer, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_MADE), 0);
    }

    #[test]
    fn a_check_past_the_end_of_an_allocation_stays() {
        // Four bytes at offset fourteen is two bytes past the sixteen that were asked for, and
        // those two bytes are what the check is for.
        let (_, mut func, inside, _, pointer, _) = allocation(16);
        let mut build = Builder::new(&mut func, inside);
        let field = past(&mut build, pointer, 14);
        access(&mut build, field, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_MADE), 0);
    }

    #[test]
    fn a_walk_that_leaves_an_allocation_stays() {
        // One end inside and the other past the end is a walk out of the object, which is what a
        // derivation check is there to catch, so both ends have to be inside before it goes.
        let (_, mut func, inside, _, pointer, _) = allocation(16);
        let mut build = Builder::new(&mut func, inside);
        let field = past(&mut build, pointer, 32);
        deriv(&mut build, pointer, field, 1);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(derivs(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_DERIV_MADE), 0);
    }

    #[test]
    fn a_check_inside_an_allocation_goes_across_a_call() {
        // The other reason a fact read off the instruction is worth having. How many bytes an
        // allocator made is not something a callee can change, so unlike a fact from a check that
        // ran this one is still there on the far side of a call.
        let (mut names, mut func, inside, _, pointer, _) = allocation(16);
        let mut build = Builder::new(&mut func, inside);
        access(&mut build, pointer, 4);
        let signature = build.func().add_signature(Signature::new());
        build.call(names.intern("g"), signature, &[]);
        access(&mut build, pointer, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 0);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_MADE), 2);
        // Both lifetime checks stay, and the second one is the one a `free` inside `g` would make
        // report.
        assert_eq!(lives(&func), 2);
    }

    /// A function that allocates `size` bytes and never looks at what it got back.
    ///
    /// The shape `bench/safety/a-strided-column-sum.c` has. The size on its own must not answer a
    /// check here, because reading through what `malloc` gave back without testing it is the bug
    /// this compiler is for.
    fn untested(size: i128) -> (Interner, Func, Block, Value, Value) {
        let mut names = Interner::new();
        let name = names.intern("f");
        let mut func = Func::new(name, Signature::new().with_params(&[Type::int(64)]));
        let block = func.create_block();
        let index = func.append_param(block, Type::int(64));
        let mut build = Builder::new(&mut func, block);
        let signature = build.func().add_signature(
            Signature::new().with_params(&[Type::int(64)]).with_returns(&[Type::PTR]),
        );
        let bytes = build.iconst(Type::int(64), size);
        let call = build.call(names.intern("malloc"), signature, &[bytes]);
        let at = build.func();
        at[call].flags |= Flags::HEAP;
        let pointer = at[call].results().next().expect("a call that gives back a pointer");
        (names, func, block, pointer, index)
    }

    #[test]
    fn a_walk_by_a_step_the_ranges_bound_inside_an_allocation_goes() {
        // The first half of tamnd/rucc#880. The step is not a constant, so the walk stops at the
        // `ptr_add` and what answers the check has to be asked of the range of addresses it can
        // reach. That range is nought to seven plus the four bytes the access wants, all of it
        // inside the sixteen the call says it made, and the branch on null is what makes the
        // sixteen true here.
        let (_, mut func, inside, _, pointer, index) = allocation(16);
        let mut build = Builder::new(&mut func, inside);
        let step = low_bits(&mut build, index, 7);
        let at = walk(&mut build, pointer, step);
        check(&mut build, at, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 0);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_MADE), 1);
    }

    #[test]
    fn a_walk_by_a_step_that_can_leave_an_allocation_stays() {
        // The same function with the mask widened. Nought to thirty one plus four bytes runs off
        // the end of sixteen, and the bytes past the end are what the check is for.
        let (_, mut func, inside, _, pointer, index) = allocation(16);
        let mut build = Builder::new(&mut func, inside);
        let step = low_bits(&mut build, index, 31);
        let at = walk(&mut build, pointer, step);
        check(&mut build, at, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_MADE), 0);
    }

    #[test]
    fn a_derivation_by_a_step_the_ranges_bound_inside_an_allocation_goes() {
        // The same for the derivation check, which is the one the column sum is left with. Both
        // ends have to be inside and inside the same object: the near end is the pointer itself and
        // the far end is anywhere in nought to seven past it.
        let (_, mut func, inside, _, pointer, index) = allocation(16);
        let mut build = Builder::new(&mut func, inside);
        let step = low_bits(&mut build, index, 7);
        let at = walk(&mut build, pointer, step);
        deriv(&mut build, pointer, at, 1);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(derivs(&func), 0);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_DERIV_MADE), 1);
    }

    #[test]
    fn a_walk_into_an_allocation_nobody_tested_stays() {
        // The other half of the rule, which this does not weaken. A program that walks into what
        // `malloc` gave back without ever looking at it is a program that reads through null when
        // the allocation fails, and the checks are what report it.
        let (_, mut func, block, pointer, index) = untested(16);
        let mut build = Builder::new(&mut func, block);
        let step = low_bits(&mut build, index, 7);
        let at = walk(&mut build, pointer, step);
        check(&mut build, at, 4);
        deriv(&mut build, pointer, at, 1);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 1);
        assert_eq!(derivs(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_MADE), 0);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_DERIV_MADE), 0);
    }

    #[test]
    fn a_check_every_caller_hands_in_goes_across_a_call() {
        // The reason the flag is worth having at all. A frame slot of the caller is not something
        // the callee's own callees can free, so the fact does not die at a call the way a fact
        // from a check that ran does.
        let (mut names, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        access(&mut build, pointer, 4);
        let signature = build.func().add_signature(Signature::new());
        build.call(names.intern("g"), signature, &[]);
        access(&mut build, pointer, 4);
        build.ret(&[]);
        flagged(&mut func, Flags::HANDED);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 0);
        assert_eq!(lives(&func), 0);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_HANDED), 2);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_LIVE_HANDED), 2);
    }

    #[test]
    fn a_check_inside_a_global_goes_across_a_call() {
        // A callee can free what a global points at and cannot free the global, which lives as
        // long as the program does. So this is the one fact besides a local that a call leaves
        // standing, and it is read off the instruction rather than out of the scope for that
        // reason.
        let (mut names, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        let callee = names.intern("might_free");
        let signature = build.func().add_signature(Signature::new());
        build.call(callee, signature, &[]);
        access(&mut build, pointer, 4);
        build.ret(&[]);
        marked(&mut func);
        let stats = run(&mut func);
        assert_eq!(checks(&func), 0);
        assert_eq!(lives(&func), 0);
        assert_eq!(stats.count(Kind::Missed, super::PAST_A_CALL), 0);
        assert_eq!(stats.count(Kind::Missed, super::PAST_A_CALL_LIVE), 0);
    }

    #[test]
    fn a_check_the_module_marked_costs_fuel_like_any_other() {
        // A discharge is a discharge whatever established the fact, so `-fpass-fuel` has to stop
        // this one too or a bisection would step over it.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        access(&mut build, pointer, 4);
        build.ret(&[]);
        marked(&mut func);
        let stats =
            DISCHARGE.run(&mut func, &mut crate::machine::fixtures::analyses(), &mut Fuel::of(1));
        assert_eq!(checks(&func) + lives(&func), 1);
        assert_eq!(stats.count(Kind::Missed, super::NO_FUEL_LIVE), 1);
    }

    #[test]
    fn a_walk_whose_two_ends_are_off_two_pointers_with_no_ranges_says_the_same() {
        // Nothing in this function steps by a value, so the ranges are never built and the answer
        // has to come out of the constant reader alone. That reader stopped for one reason, and it
        // is the same reason.
        let (_, mut func, block, pointer) = blank();
        let other = func.append_param(block, Type::PTR);
        let mut build = Builder::new(&mut func, block);
        let at = past(&mut build, other, 8);
        deriv(&mut build, pointer, at, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(derivs(&func), 1);
        assert_eq!(stats.count(Kind::Missed, super::TWO_BASES_DERIV), 1);
    }

    #[test]
    fn a_walk_whose_two_ends_are_off_two_pointers_says_so() {
        // Nothing comparable to ask about. Both ends are readable and each is somewhere inside
        // something, and two facts of that shape say nothing at all about it being one something,
        // which is the only thing a derivation check wants to know.
        let (_, mut func, block, pointer, index) = indexed();
        let other = func.append_param(block, Type::PTR);
        let mut build = Builder::new(&mut func, block);
        let step = low_bits(&mut build, index, 7);
        let at = walk(&mut build, other, step);
        deriv(&mut build, pointer, at, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(derivs(&func), 1);
        assert_eq!(stats.count(Kind::Missed, super::TWO_BASES_DERIV), 1);
    }

    #[test]
    fn a_walk_off_a_pointer_this_function_was_handed_says_so() {
        // The largest pile after a loaded pointer, 1321 checks on SQLite. Everything about the
        // shape is readable: one base, a step the ranges bound, both ends off that base. What is
        // missing is how many bytes belong to the object, and a pointer that arrived as a
        // parameter is one nothing in the function can say that about. Section 7.5's summaries are
        // what would.
        let (_, mut func, block, pointer, index) = indexed();
        let mut build = Builder::new(&mut func, block);
        let step = low_bits(&mut build, index, 7);
        let at = walk(&mut build, pointer, step);
        deriv(&mut build, pointer, at, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(derivs(&func), 1);
        assert_eq!(stats.count(Kind::Missed, super::NO_EXTENT_HANDED), 1);
    }

    #[test]
    fn a_walk_off_a_pointer_this_function_loaded_says_so() {
        // The largest pile of the lot, 2155 checks on SQLite, and the shape is `p->field[i]`. The
        // extent of what a pointer in memory points at is not written down anywhere the compiler
        // can see today, which is what `__counted_by` and the type plane are for.
        let (_, mut func, block, pointer, index) = indexed();
        let mut build = Builder::new(&mut func, block);
        let info = MemInfo {
            size: 8,
            align: 8,
            order: MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        };
        let args = build.func().push_values(&[pointer]);
        let extra = Extra::Mem(build.func().add_mem(info));
        let held = build.value(InstData { args, extra, ..InstData::new(Opcode::Load) }, Type::PTR);
        let step = low_bits(&mut build, index, 7);
        let at = walk(&mut build, held, step);
        deriv(&mut build, held, at, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(derivs(&func), 1);
        assert_eq!(stats.count(Kind::Missed, super::NO_EXTENT_LOADED), 1);
    }

    #[test]
    fn a_walk_off_a_global_says_so() {
        // 485 checks on SQLite, and the one pile of the four where somebody does know the answer.
        // A global's extent is on the module, `crate::extents` reads it and writes the fact onto
        // every check it can settle before the pipeline starts, and it cannot settle this one
        // because it runs before anything has put a number on the index. See tamnd/rucc#878.
        let (mut names, mut func, block, _, index) = indexed();
        let mut build = Builder::new(&mut func, block);
        let extra = Extra::Symbol(names.intern("g"));
        let base = build.value(InstData { extra, ..InstData::new(Opcode::GlobalAddr) }, Type::PTR);
        let step = low_bits(&mut build, index, 7);
        let at = walk(&mut build, base, step);
        deriv(&mut build, base, at, 4);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(derivs(&func), 1);
        assert_eq!(stats.count(Kind::Missed, super::NO_EXTENT_GLOBAL), 1);
    }

    #[test]
    fn a_walk_off_a_pointer_the_check_does_not_name_stays() {
        // The capability has to be the `cap_of` of the pointer that went in. One naming something
        // else is asking about a different instance and is not this pass's to answer.
        let (_, mut func, block, pointer) = blank();
        let mut build = Builder::new(&mut func, block);
        check(&mut build, pointer, 16);
        let field = past(&mut build, pointer, 8);
        let args = build.func().push_values(&[field]);
        let capability = build.value(InstData { args, ..InstData::new(Opcode::CapOf) }, Type::CAP);
        let width = build.iconst(Type::int(64), 4);
        let args = build.func().push_values(&[capability, pointer, field, width]);
        build.inst(InstData { args, ..InstData::new(Opcode::CheckDeriv) }, &[]);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(derivs(&func), 1);
        assert_eq!(stats.count(Kind::Missed, super::NOT_ITS_CAPABILITY_DERIV), 1);
    }
}
