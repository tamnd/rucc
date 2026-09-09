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
//! up with is a range of bytes containing every address the walk could produce. If the local it
//! started from covers all of that, then it covers the one address the access actually uses,
//! whichever that turns out to be. That is the whole argument.
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

use rucc_ir::{Def, Extra, Flags, Func, Inst, Opcode, Value};

use crate::range::query::Ranges;
use crate::rules::{Piece, Subject, Table, safety};
use crate::{Analyses, Fuel, Pass, Preserved, Stats};

/// Recorded once for each bounds check taken out.
const REMOVED: &str = "bounds check removed, a dominating check covers the same bytes";

/// Recorded once for each bounds check taken out because it was inside a local.
const REMOVED_LOCAL: &str = "bounds check removed, its bytes are inside a local this function \
                             declares";

/// Recorded once for each bounds check taken out because it was inside a global.
const REMOVED_STATIC: &str = "bounds check removed, its bytes are inside an object of static \
                              storage duration";

/// Recorded once for each bounds check taken out because a range answered the step it walked by.
const REMOVED_RANGE: &str = "bounds check removed, every address the walk can reach is inside the \
                             object it started from";

/// Recorded once for each lifetime check taken out.
const REMOVED_LIVE: &str = "lifetime check removed, a dominating check covers the same storage";

/// Recorded once for each lifetime check taken out because it was inside a global.
const REMOVED_LIVE_STATIC: &str =
    "lifetime check removed, its storage lives as long as the program does";

/// Recorded for a bounds check that would have gone if there had been fuel for it.
const NO_FUEL: &str = "bounds check kept, the pass ran out of fuel";

/// Recorded for a lifetime check that would have gone if there had been fuel for it.
const NO_FUEL_LIVE: &str = "lifetime check kept, the pass ran out of fuel";

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

/// Recorded for a derivation check that would have gone if there had been fuel for it.
const NO_FUEL_DERIV: &str = "derivation check kept, the pass ran out of fuel";

/// Recorded for a derivation check a call cost.
const PAST_A_CALL_DERIV: &str =
    "derivation check kept, a call between it and the range that holds both ends might free";

/// Recorded for a derivation check whose operands this pass cannot read.
const UNKNOWN_SHAPE_DERIV: &str =
    "derivation check left alone, its two pointers are not one base and two constants";

/// The pass. It holds nothing, because everything it works out is about one function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Discharge;

impl Pass for Discharge {
    fn name(&self) -> &'static str {
        "discharge"
    }

    fn describe(&self) -> &'static str {
        "a bounds, lifetime or derivation check whose answer is already known is removed"
    }

    fn preserves(&self) -> Preserved {
        // Instructions go and blocks do not. A check is not a terminator and removing one leaves
        // every edge where it was.
        Preserved::ALL
    }

    fn run(&self, func: &mut Func, an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        let Some(entry) = func.entry() else { return stats };
        let dom = an.dominators(func).clone();

        // Only when there is a walk the constant reader gives up on, because that is the only
        // thing the ranges are asked about here and a function without one would pay for a copy
        // of the graph and get nothing back.
        let cfg = walks_by_a_value(func).then(|| an.cfg(func).clone());
        let mut ranges = cfg.as_ref().map(|cfg| Ranges::new(&*func, cfg, &dom));

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
                        // The two objects whose extent is known without anybody having checked
                        // it. A global was worked out over the module by `crate::extents` and
                        // arrives as a flag, a local is read off its `alloca` here, and both are
                        // asked of the same rule as every other fact. The reach of a walk the
                        // constant reader could not finish is asked last, because it is the only
                        // one of the four that costs an analysis to answer.
                        let why = if func[inst].flags.contains(Flags::STATIC) {
                            Some(REMOVED_STATIC)
                        } else if declared(func, asked.base)
                            .is_some_and(|local| covers(&local, &asked))
                        {
                            Some(REMOVED_LOCAL)
                        } else if scope.bounds.covers(&asked) {
                            Some(REMOVED)
                        } else {
                            reach(func, ranges.as_mut(), &asked, inst)
                                .filter(|wide| {
                                    declared(func, wide.base)
                                        .is_some_and(|local| covers(&local, wide))
                                        || scope.bounds.covers(wide)
                                })
                                .map(|_| REMOVED_RANGE)
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
                        // A local answers a bounds check and not this one. What a local gives is
                        // an extent, and how long it is alive is the block it was declared in,
                        // which is a question this pass has nothing to say about. A global is
                        // alive as long as the program, so the flag answers both.
                        let inside =
                            func[inst].flags.contains(Flags::STATIC).then_some(REMOVED_LIVE_STATIC);
                        if inside.is_none() && !scope.alive.covers(&asked) {
                            if scope.alive.covered_before(&asked) {
                                stats.missed(PAST_A_CALL_LIVE);
                            }
                            scope.alive.held.push(widened(func, &scope.bounds, asked));
                            continue;
                        }
                        if !fuel.take() {
                            stats.missed(NO_FUEL_LIVE);
                            scope.alive.held.push(widened(func, &scope.bounds, asked));
                            continue;
                        }
                        going.push((inst, inside.unwrap_or(REMOVED_LIVE)));
                    }
                    Opcode::CheckDeriv => {
                        let Some((from, to)) = derives(func, inst) else {
                            stats.missed(UNKNOWN_SHAPE_DERIV);
                            continue;
                        };
                        let inside = if func[inst].flags.contains(Flags::STATIC) {
                            Some(REMOVED_DERIV_STATIC)
                        } else if declared(func, from.base)
                            .is_some_and(|local| covers(&local, &from) && covers(&local, &to))
                        {
                            Some(REMOVED_DERIV_LOCAL)
                        } else {
                            None
                        };
                        if inside.is_none() && !scope.bounds.holds_both(&from, &to) {
                            if scope.bounds.held_both_before(&from, &to) {
                                stats.missed(PAST_A_CALL_DERIV);
                            }
                            continue;
                        }
                        if !fuel.take() {
                            stats.missed(NO_FUEL_DERIV);
                            continue;
                        }
                        going.push((inst, inside.unwrap_or(REMOVED_DERIV)));
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
fn normal(func: &Func, value: Value) -> (Value, i128) {
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
/// What comes out is a range of bytes that contains every address the walk could possibly produce.
/// If the object the walk started from covers all of it then it covers the one address the access
/// actually uses, whichever that turns out to be, so the check has nothing left to say. That is
/// the whole argument, and it works for the same reason a wider fact answers more checks
/// everywhere else in this pass.
///
/// It is only ever asked with. A fact this returns must never be recorded as established, and the
/// one place it could be is the push in the `check_bounds` arm, which happens only where this
/// returned nothing or answered nothing. The reason is that the widened range is not what a check
/// proves. A check that runs and passes proves the address the program used was inside the object,
/// and says nothing at all about the rest of the range this function made up around it.
fn reach(func: &Func, ranges: Option<&mut Ranges<'_>>, asked: &Fact, at: Inst) -> Option<Fact> {
    let ranges = ranges?;
    let mut base = asked.base;
    let mut offset = asked.offset;
    let mut slack: i128 = 0;
    loop {
        // A constant step again, because past a step that needed a range there can be more of
        // them, and the frontend leaves a field offset as a constant under an array index.
        if let Some((from, step)) = walked(func, base) {
            offset = offset.checked_add(step)?;
            base = from;
            continue;
        }
        let Some(from) = operand_of(func, base, Opcode::PtrAdd, 0) else { break };
        let by = operand_of(func, base, Opcode::PtrAdd, 1)?;
        let (low, high) = ranges.at_inst(by, at).signed_bounds()?;
        offset = offset.checked_add(low)?;
        slack = slack.checked_add(high.checked_sub(low)?)?;
        base = from;
    }
    // Nothing was walked past, so this is the fact that came in and asking it again is work
    // somebody already did.
    if base == asked.base {
        return None;
    }
    Some(Fact { base, offset, size: slack.checked_add(asked.size)? })
}

/// Whether any walk in this function steps by a value rather than a constant.
///
/// The question the ranges are built for. A function without one of these would pay for a copy of
/// the control flow graph and never ask anything of it.
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
fn constant(func: &Func, value: Value) -> Option<i128> {
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
        AsmInfo, Block, BlockCallList, Builder, Extra, Flags, Func, Inst, InstData, MemInfo,
        MemOrder, Opcode, Restrict, Signature, Type, Value,
    };

    use super::{Discharge, Fact};
    use crate::stats::Kind;
    use crate::{Analyses, Fuel, Pass};

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
        let insts: Vec<Inst> =
            func.blocks().flat_map(|block| func.insts(block).collect::<Vec<_>>()).collect();
        for inst in insts {
            let check = matches!(
                func[inst].opcode,
                Opcode::CheckBounds | Opcode::CheckLive | Opcode::CheckDeriv
            );
            if check {
                func[inst].flags |= Flags::STATIC;
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
        Discharge.run(func, &mut Analyses::new(), &mut Fuel::unlimited())
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
        let stats = Discharge.run(&mut func, &mut Analyses::new(), &mut fuel);
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
        let stats = Discharge.run(&mut func, &mut Analyses::new(), &mut fuel);
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

    /// A stack slot of `size` bytes, in the entry block where the verifier wants one.
    fn local(build: &mut Builder<'_>, size: u64) -> Value {
        let info = MemInfo {
            size,
            align: 8,
            order: MemOrder::NotAtomic,
            tbaa: None,
            restrict: Restrict::NONE,
        };
        let extra = Extra::Mem(build.func().add_mem(info));
        build.value(InstData { extra, ..InstData::new(Opcode::Alloca) }, Type::PTR)
    }

    /// A function taking a pointer and an index, with one block.
    fn indexed() -> (Interner, Func, Block, Value) {
        let mut names = Interner::new();
        let name = names.intern("f");
        let mut func = Func::new(name, Signature::new().with_params(&[Type::PTR, Type::int(64)]));
        let block = func.create_block();
        func.append_param(block, Type::PTR);
        let index = func.append_param(block, Type::int(64));
        (names, func, block, index)
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
        let (_, mut func, block, index) = indexed();
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
        let (_, mut func, block, index) = indexed();
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
        let (_, mut func, block, index) = indexed();
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
        let (_, mut func, block, index) = indexed();
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
        let (_, mut func, block, index) = indexed();
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
    fn a_lifetime_check_in_a_local_widens_to_the_whole_local() {
        // The widening the module comment argues for, with the local standing in for the checked
        // range. The first lifetime check found the instance holding the slot alive, the slot is
        // one instance, so the second one anywhere in it is asking a question already answered.
        let (_, mut func, block, _) = blank();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build, 16);
        live(&mut build, slot);
        let field = past(&mut build, slot, 12);
        live(&mut build, field);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(lives(&func), 1);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_LIVE), 1);
    }

    #[test]
    fn a_lifetime_check_past_the_end_of_a_local_stays() {
        // The widening stops where the slot does, so an address outside it is a different
        // instance and a question nothing has answered.
        let (_, mut func, block, _) = blank();
        let mut build = Builder::new(&mut func, block);
        let slot = local(&mut build, 16);
        live(&mut build, slot);
        let field = past(&mut build, slot, 24);
        live(&mut build, field);
        build.ret(&[]);
        let stats = run(&mut func);
        assert_eq!(lives(&func), 2);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED_LIVE), 0);
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
        let stats = Discharge.run(&mut func, &mut Analyses::new(), &mut Fuel::of(1));
        assert_eq!(checks(&func) + lives(&func), 1);
        assert_eq!(stats.count(Kind::Missed, super::NO_FUEL_LIVE), 1);
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
        assert_eq!(stats.count(Kind::Missed, super::UNKNOWN_SHAPE_DERIV), 1);
    }
}
