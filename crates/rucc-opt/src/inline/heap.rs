//! The inliner's second pass, which takes the calls the first pass found too large in the order of
//! how bad each one is to inline, the least bad first.
//!
//! Design: sections 33.4, 33.5 and 33.6 of `spec/optimizer/33-inlining.md`, and tamnd/rucc#2897.
//!
//! ```c
//! static int mix(const int *a, int n, int k) { /* a loop that runs n times */ }
//! int use1(const int *a) { return mix(a, 4, 3); }
//! int use2(const int *a, int n) { return mix(a, n, 5) + mix(a + 1, n, 6); }
//! ```
//!
//! `mix` grows a caller by more than `max-inline-insns-auto` allows, so the first pass leaves all
//! three calls. gcc copies it into `use1` and leaves both calls in `use2`, because the copy in `use1`
//! knows how many times its loop runs. That is the `loop_iterations` hint, and a hint raises the
//! limit by `inline-heuristics-hint-percent`. A body that calls through one of its parameters is the
//! same when the call passes the address of a function, since the copy makes that call direct, which
//! is the `indirect_call` hint, and a call that passes a constant a `__builtin_constant_p` in the body
//! asks about is the other kind of hint, which scales the limit again on top of the first.
//!
//! What this takes and in which order is gcc's `inline_small_functions`. Every call the first pass
//! refused for its size, and every call a copy brought into a caller that the first pass never saw
//! there, goes on a heap keyed by [`rucc_cost::badness::badness`]. The least bad is taken first. A
//! call whose caller or callee has changed since it was weighed is weighed again when it comes off
//! the heap and goes back on if it is now worse than the next one. Then it is held to section 33.6:
//! the limit its hints give it or a large enough speedup, the growth of the whole unit, the growth of
//! a large caller, and the caller's frame. A call refused here is refused for good, the way gcc drops
//! it from its heap, and says why in the words the first pass would have used.
//!
//! Time is what [`super::folded`] counts with each instruction weighed by how often its block runs
//! for each time the function is entered, which is the static guess of section 11.7. With the
//! constants the call passes folded into the body, the difference is what the copy saves on top of
//! the call itself.

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use rucc_base::hash::{Map, Set};
use rucc_base::{Interner, Symbol};
use rucc_cost::badness::{self, Call, Hints};
use rucc_cost::heuristics::{
    INLINE_CALL_TIME, INLINE_HINT_PERCENT, INLINE_HINT_PERCENT_O3, INLINE_INSNS_AUTO,
    INLINE_MIN_SPEEDUP, INLINE_MIN_SPEEDUP_O3, INLINE_UNIT_GROWTH, LARGE_FUNCTION_GROWTH,
    LARGE_FUNCTION_INSNS, LARGE_UNIT_INSNS,
};
use rucc_ir::{
    AttrSet, Block, Def, Extra, Func, FuncId, Imm, Inst, Linkage, Module, Opcode, Pic, SymbolRef,
    Type, Value,
};

use super::{
    ASKED_DEPTH, How, INLINED, InlineFailure, Kind, Pool, calls_twice, fits, folded, folded_size,
    frame, grows, made_of_params, passed, passes_asked, resolved, specialized_size, splice,
};
use crate::Stats;
use crate::callgraph::CallGraph;
use crate::cfg::Cfg;
use crate::dom::Dominators;
use crate::frequency::Frequencies;
use crate::loops::Loops;
use crate::predict::Callees;

/// What the second pass says about a call it inlined.
const WEIGHED: &str = "call inlined by the second pass";

/// How the second pass weighs a call at one level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Second {
    /// How far a hint raises a limit, in percent. gcc's `inline-heuristics-hint-percent`.
    pub percent: u32,
    /// How much of the time a call and its caller take inlining has to save for a call over its
    /// limit to be taken anyway, in percent. gcc's `inline-min-speedup`.
    pub speedup: u32,
    /// Whether [`crate::ipcp`] runs in this build. gcc runs `ipa-cp` before this inliner, so a
    /// parameter every call passes the same constant for is a constant in the body by the time the
    /// hints are asked, and a call passing it tells the copy nothing the body did not know.
    pub constants: bool,
}

impl Second {
    /// gcc's numbers at `-O2`.
    pub const O2: Self =
        Self { percent: INLINE_HINT_PERCENT, speedup: INLINE_MIN_SPEEDUP, constants: true };
    /// gcc's numbers at `-O3`, where a hint raises a limit further and a smaller speedup will do.
    pub const O3: Self =
        Self { percent: INLINE_HINT_PERCENT_O3, speedup: INLINE_MIN_SPEEDUP_O3, constants: true };
}

/// What the first pass leaves the second about the calls it looked at.
#[derive(Debug, Default)]
pub(super) struct Later {
    /// Every call the first pass weighed, by caller and instruction.
    pub(super) examined: Set<(FuncId, Inst)>,
    /// The ones it found too large and left for this pass to weigh again.
    pub(super) deferred: Set<(FuncId, Inst)>,
}

/// What a call was weighed on: how many times its caller and its callee had changed, and how many
/// calls the callee had. A call whose stamp is not what it would be now is weighed again.
type Stamp = (u32, u32, usize);

/// One call on the heap, with the badness it had when it went on.
#[derive(Debug)]
struct Entry {
    badness: f64,
    caller: FuncId,
    call: Inst,
    stamp: Stamp,
}

impl PartialEq for Entry {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Entry {}

impl PartialOrd for Entry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Entry {
    /// Backwards, so that the heap, which gives the largest first, gives the least bad. Two calls
    /// as bad as each other go in the order they were written, so the result does not depend on
    /// how the heap breaks ties.
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .badness
            .total_cmp(&self.badness)
            .then_with(|| other.caller.cmp(&self.caller))
            .then_with(|| other.call.cmp(&self.call))
    }
}

/// What one call came to when it was weighed.
#[derive(Debug, Clone, Copy)]
struct Weighed {
    callee: FuncId,
    kind: Kind,
    /// The body as the limit for its kind measures it.
    body: usize,
    /// The body less the call it replaces.
    growth: i64,
    hints: Hints,
    /// How much the unit grows if every call to the callee is inlined, less the body when nothing
    /// would be left to call it.
    overall: i64,
    badness: f64,
    /// Whether the copy saves enough of the time the call and its caller take.
    speedup: bool,
    /// How much longer the caller takes with the copy in it, which is the copy less the call, as
    /// often as the call runs.
    adds: f64,
}

/// How often each block of a function runs each time it is entered, how many loops deep each one
/// is, and how long the function takes.
#[derive(Debug)]
struct Profile {
    frequency: Map<Block, f64>,
    depth: Map<Block, u32>,
    time: f64,
    /// How often the most frequent direct call to each name runs and how deep it is, which is
    /// what a copy of the function brings into its caller.
    calls: Map<Symbol, (f64, u32)>,
}

/// What a body's measurement depends on. See [`Heap::measured`].
type Measured = (FuncId, u32, Vec<(Value, Imm, Type)>, bool, usize);

/// What the second pass knows as it goes.
struct Heap<'a> {
    how: &'a How<'a>,
    second: Second,
    /// The frame each function had before anything was inlined into it.
    own: &'a Map<FuncId, u64>,
    /// How many times each function has had a call inlined into it by this pass.
    changed: Map<FuncId, u32>,
    /// How many direct calls the module makes to each name now.
    calls: Map<Symbol, usize>,
    /// How large each function is now, and how large it was when this pass began.
    sizes: Map<FuncId, usize>,
    original: Map<FuncId, usize>,
    /// How large the whole unit is now, and how large it may become.
    unit: usize,
    most: usize,
    /// The functions that reach themselves through calls, which are never weighed.
    cyclic: Set<Symbol>,
    /// The profile of each callee as it was when a call to it was last weighed.
    profiles: Map<FuncId, (u32, Profile)>,
    /// How often each call runs for each time its caller does and how many loops deep it is, how
    /// long each function takes, and the most each one's frame can be. These are measured once and
    /// then kept up to date from what each copy adds, the way gcc updates its summaries, since
    /// measuring a caller again after each of hundreds of copies is quadratic in its size. The
    /// frame is measured again when that bound is what would refuse a call.
    at: Map<(FuncId, Inst), (f64, u32)>,
    spent: Map<FuncId, f64>,
    frames: Map<FuncId, u64>,
    /// What a call's body measured, how long its copy takes and the hints it has, with the callee's
    /// change it was measured at.
    bodies: Map<(FuncId, Inst), (u32, usize, f64, Hints)>,
    /// What a body measured and how long its copy takes, by callee, the callee's change, the
    /// constants it was given, whether it weighed and the limit past which it was cleaned up. A
    /// callee called from hundreds of places with the same constants is copied and folded once
    /// for all of them rather than once for each, and that copy was most of the pass.
    measured: Map<Measured, (usize, f64)>,
    /// The parameters of each function that every call to it passes the same constant for, which
    /// give no hint. See [`Second::constants`].
    settled: Map<FuncId, Set<Value>>,
    /// The functions that run at most once each time the program does. See [`Heap::once`].
    once: Set<FuncId>,
    pools: Map<FuncId, Pool>,
    stats: Map<FuncId, Stats>,
}

/// Weighs and inlines the calls the first pass left, and says what it did where.
pub(super) fn run(
    module: &mut Module,
    how: &How<'_>,
    later: &Later,
    own: &Map<FuncId, u64>,
    second: Second,
    pic: Pic,
) -> Vec<(FuncId, Stats)> {
    let graph = CallGraph::of(module, pic);
    let mut cyclic = Set::default();
    for component in graph.components() {
        for &node in component {
            if component.len() > 1 || graph.calls(node).contains(&node) {
                cyclic.insert(graph.name(node));
            }
        }
    }
    let settled = if second.constants { settled(module, &graph) } else { Map::default() };
    let defined: Vec<FuncId> = module.funcs().filter(|&id| !module[id].is_declaration()).collect();
    let sizes: Map<FuncId, usize> =
        defined.iter().map(|&id| (id, measure(&module[id], how.names))).collect();
    let unit: usize = sizes.values().sum();
    // gcc's `compute_max_insns`: the unit may grow by `inline-unit-growth` percent of itself, or of
    // `large-unit-insns` when it is smaller than that.
    let floor = usize::try_from(LARGE_UNIT_INSNS).unwrap_or(usize::MAX);
    let percent = usize::try_from(INLINE_UNIT_GROWTH).unwrap_or(0);
    let most = unit.max(floor) * (100 + percent) / 100;
    let mut heap = Heap {
        how,
        second,
        own,
        changed: Map::default(),
        calls: how.calls.clone(),
        original: sizes.clone(),
        sizes,
        unit,
        most,
        cyclic,
        profiles: Map::default(),
        at: Map::default(),
        spent: Map::default(),
        frames: Map::default(),
        bodies: Map::default(),
        measured: Map::default(),
        settled,
        once: Set::default(),
        pools: Map::default(),
        stats: Map::default(),
    };
    heap.once = heap.once(module, &defined);
    let mut queue = BinaryHeap::new();
    for &id in &defined {
        for call in direct(&module[id]) {
            let key = (id, call);
            if !later.examined.contains(&key) || later.deferred.contains(&key) {
                heap.push(module, &mut queue, id, call);
            }
        }
    }
    while let Some(entry) = queue.pop() {
        let Some(weighed) = heap.weigh(module, entry.caller, entry.call) else { continue };
        let stamp = heap.stamp(module, entry.caller, weighed.callee);
        if stamp != entry.stamp && queue.peek().is_some_and(|next| next.badness < weighed.badness) {
            queue.push(Entry { badness: weighed.badness, stamp, ..entry });
            continue;
        }
        heap.take(module, entry.caller, entry.call, weighed, &mut queue);
    }
    // What the copies left stranded goes once, after the last of them, rather than after each.
    let mut changed: Vec<FuncId> = heap.changed.keys().copied().collect();
    changed.sort_unstable();
    for id in changed {
        let mut an = crate::Analyses::new(crate::machine::Machine::unknown());
        crate::simplify_cfg::sweep(&mut module[id], &mut an, &mut Stats::new());
    }
    let mut done: Vec<(FuncId, Stats)> =
        heap.stats.into_iter().filter(|(_, stats)| !stats.is_empty()).collect();
    done.sort_unstable_by_key(|&(id, _)| id);
    done
}

impl Heap<'_> {
    /// How many times this function has changed.
    fn version(&self, id: FuncId) -> u32 {
        self.changed.get(&id).copied().unwrap_or(0)
    }

    fn stamp(&self, module: &Module, caller: FuncId, callee: FuncId) -> Stamp {
        let calls = self.calls.get(&module[callee].name).copied().unwrap_or(0);
        (self.version(caller), self.version(callee), calls)
    }

    /// Puts a call on the heap when it is one this pass weighs.
    fn push(&mut self, module: &Module, queue: &mut BinaryHeap<Entry>, caller: FuncId, call: Inst) {
        if let Some(weighed) = self.weigh(module, caller, call) {
            let stamp = self.stamp(module, caller, weighed.callee);
            queue.push(Entry { badness: weighed.badness, caller, call, stamp });
        }
    }

    /// The callee of a call this pass weighs, and why it would be inlined.
    ///
    /// A function that reaches itself is left to the first pass, which refuses it, since a copy
    /// of one brings the call back with it. So is a caller that asked not to be optimized. A call
    /// to a function no larger than a call is one the first pass took wherever it saw one, and here
    /// it is one a copy brought or one a copy made direct.
    fn callee(&self, module: &Module, caller: FuncId, call: Inst) -> Option<(FuncId, Kind)> {
        let func = &module[caller];
        func.block_of(call)?;
        if func.attrs.set.contains(AttrSet::OPTNONE) || func[call].opcode != Opcode::Call {
            return None;
        }
        let Extra::Call(info) = func[call].extra else { return None };
        let name = func[info].callee?;
        let &(callee, kind) = self.how.wanted.get(&name)?;
        let weighed = matches!(kind, Kind::Hinted | Kind::Auto | Kind::Small)
            && callee != caller
            && !self.cyclic.contains(&name)
            && !module[callee].is_declaration();
        weighed.then_some((callee, kind))
    }

    /// The profile of a function as it is now.
    fn profile(&mut self, module: &Module, id: FuncId) -> &Profile {
        let version = self.version(id);
        if self.profiles.get(&id).is_none_or(|&(seen, _)| seen != version) {
            self.profiles.insert(id, (version, profile(&module[id], self.how.names)));
        }
        &self.profiles[&id].1
    }

    /// The functions gcc's `ipa_propagate_frequency` finds run at most once each time the program
    /// does, which is its `NODE_FREQUENCY_EXECUTED_ONCE`.
    ///
    /// `main` is one, and so is a function that never comes back. A `static` function nothing
    /// takes the address of is one when every call to it is made from one of those, or from a
    /// function written `cold`, and is in no loop there. One the first pass copied into every caller
    /// it had is one as well, since gcc has removed it by now. A function written `hot` is never
    /// one.
    fn once(&mut self, module: &Module, defined: &[FuncId]) -> Set<FuncId> {
        let names = self.how.names;
        let mut once: Set<FuncId> = defined
            .iter()
            .copied()
            .filter(|&id| {
                let func = &module[id];
                let main = func.linkage != Linkage::Internal && names.resolve(func.name) == "main";
                !func.attrs.set.contains(AttrSet::HOT)
                    && (main || func.attrs.set.contains(AttrSet::NORETURN))
            })
            .collect();
        let by_name: Map<Symbol, FuncId> =
            defined.iter().map(|&id| (module[id].name, id)).collect();
        // Every direct call to each function, as the caller that makes it and how deep it is.
        let mut sites: Map<FuncId, Vec<(FuncId, u32)>> = Map::default();
        for &id in defined {
            let calls = direct(&module[id]);
            if calls.is_empty() {
                continue;
            }
            self.learn(module, id);
            let func = &module[id];
            for call in calls {
                let Extra::Call(info) = func[call].extra else { continue };
                let Some(&callee) = func[info].callee.and_then(|name| by_name.get(&name)) else {
                    continue;
                };
                let depth = self.at.get(&(id, call)).map_or(0, |&(_, depth)| depth);
                sites.entry(callee).or_default().push((id, depth));
            }
        }
        loop {
            let before = once.len();
            for &callee in defined {
                let func = &module[callee];
                let local = func.linkage == Linkage::Internal
                    && !func.attrs.set.contains(AttrSet::USED)
                    && !func.attrs.set.contains(AttrSet::HOT)
                    && !self.how.elsewhere.contains(&func.name);
                if !local || once.contains(&callee) {
                    continue;
                }
                let callers = sites.get(&callee).map_or(&[][..], Vec::as_slice);
                let only = callers.iter().all(|&(caller, depth)| {
                    depth == 0
                        && (once.contains(&caller)
                            || module[caller].attrs.set.contains(AttrSet::COLD))
                });
                if only {
                    once.insert(callee);
                }
            }
            if once.len() == before {
                return once;
            }
        }
    }

    /// Whether a call may be hot, which is gcc's `cgraph_edge::maybe_hot_p` with no profile as far
    /// as it decides anything here: a call to a function that runs once is not.
    ///
    /// `maybe_hot_p` also holds a call that runs less than half again as often as a caller that
    /// runs once to be cold, but gcc 16 at `-O2` copies a call `main` makes once into `main` when
    /// the callee is called from a loop somewhere else, so that test is not followed.
    fn hot(&self, callee: FuncId) -> bool {
        !self.once.contains(&callee)
    }

    /// Fills in [`Heap::at`] and [`Heap::spent`] for a function the first time they are asked
    /// about it.
    fn learn(&mut self, module: &Module, id: FuncId) {
        if self.spent.contains_key(&id) {
            return;
        }
        let func = &module[id];
        let found = profile(func, self.how.names);
        for call in direct(func) {
            let Some(block) = func.block_of(call) else { continue };
            let frequency = found.frequency.get(&block).copied().unwrap_or(1.0);
            let depth = found.depth.get(&block).copied().unwrap_or(0);
            self.at.insert((id, call), (frequency, depth));
        }
        self.spent.insert(id, found.time);
    }

    /// What a call comes to, or nothing when it is not one this pass weighs.
    fn weigh(&mut self, module: &Module, caller: FuncId, call: Inst) -> Option<Weighed> {
        let (callee, kind) = self.callee(module, caller, call)?;
        let func = &module[caller];
        let target = &module[callee];
        let names = self.how.names;
        let values = passed(func, call, target);
        let args = func[func[call].args].len();
        let version = self.version(callee);
        let (body, copied, hints) = match self.bodies.get(&(caller, call)) {
            Some(&(seen, body, copied, hints)) if seen == version => (body, copied, hints),
            _ => {
                let weighed = (kind == Kind::Auto).then_some(names);
                // What the first pass would have let through, past which it measured a cleaned up
                // copy as well.
                let plain = match kind {
                    Kind::Auto => usize::try_from(INLINE_INSNS_AUTO).unwrap_or(0) + args,
                    _ => self.how.limit,
                };
                let mut passed: Vec<(Value, Imm, Type)> =
                    values.iter().map(|(&param, &(imm, ty))| (param, imm, ty)).collect();
                passed.sort_unstable_by_key(|&(param, ..)| param);
                let key = (callee, version, passed, weighed.is_some(), plain);
                let (body, copied) = match self.measured.get(&key) {
                    Some(&measured) => measured,
                    None => {
                        let mut body = folded_size(target, Set::default(), values.clone(), weighed);
                        if body > plain {
                            body = body.min(specialized_size(target, &values, weighed));
                        }
                        let frequency = &self.profile(module, callee).frequency;
                        let copied = folded(
                            target,
                            Set::default(),
                            values.clone(),
                            Some(names),
                            Some(frequency),
                        )
                        .1;
                        self.measured.insert(key, (body, copied));
                        (body, copied)
                    }
                };
                // What the call knows that the body out of line does not.
                let fresh: Map<Value, (Imm, Type)> = match self.settled.get(&callee) {
                    Some(settled) => values
                        .iter()
                        .filter(|(value, _)| !settled.contains(value))
                        .map(|(&value, &known)| (value, known))
                        .collect(),
                    None => values.clone(),
                };
                let hints = Hints {
                    enables: loops_known(target, &fresh)
                        || indirect_known(module, func, call, target),
                    asks: passes_asked(func, call, target),
                    declared: kind == Kind::Hinted,
                };
                self.bodies.insert((caller, call), (version, body, copied, hints));
                (body, copied, hints)
            }
        };
        let growth = as_i64(body) - as_i64(1 + args);
        self.learn(module, caller);
        self.learn(module, callee);
        let time = self.spent.get(&callee).copied().unwrap_or(0.0);
        let (frequency, depth) = self.at.get(&(caller, call)).copied().unwrap_or((1.0, 0));
        let spent = self.spent.get(&caller).copied().unwrap_or(0.0);
        // gcc's `eni_time_weights`: the call, a move for each argument and one for the result.
        let reads = usize::from(func[call].results > 0);
        let call_time = f64::from(INLINE_CALL_TIME) + (args + reads) as f64;
        let saved = call_time + (time - copied).max(0.0);
        let removable = target.linkage == Linkage::Internal
            && !target.attrs.set.contains(AttrSet::USED)
            && !self.how.elsewhere.contains(&target.name);
        let count = self.calls.get(&target.name).copied().unwrap_or(1).max(1);
        let size = |id| as_i64(self.sizes.get(&id).copied().unwrap_or(0));
        let overall = growth * as_i64(count) - if removable { size(callee) } else { 0 };
        let badness = badness::badness(&Call {
            growth,
            saved,
            frequency: Some(frequency),
            depth,
            overall,
            caller: size(caller),
            hints,
        });
        let speedup =
            badness::big_speedup(time, copied, call_time, frequency, spent, self.second.speedup);
        let adds = (copied - call_time) * frequency;
        Some(Weighed { callee, kind, body, growth, hints, overall, badness, speedup, adds })
    }

    /// Whether the limit for the call's kind lets it through, which is gcc's
    /// `want_inline_small_function_p`.
    ///
    /// A call that does not grow the caller always does. Otherwise the limit is the one its hints
    /// raise it to, and a call with no hint may go as far as one hint would take it when the copy
    /// saves enough time. A call to a function nobody declared `inline` is let through as well when
    /// inlining it at every call would leave the program no larger and it is within the limit for
    /// one that was.
    fn wants(&self, weighed: &Weighed) -> bool {
        if weighed.growth <= 0 {
            return true;
        }
        let percent = self.second.percent;
        let hinted = weighed.hints.enables || weighed.hints.asks;
        let one = Hints { enables: true, ..Hints::default() };
        match weighed.kind {
            Kind::Hinted => {
                let base = u32::try_from(self.how.limit).unwrap_or(u32::MAX);
                let body = u64::try_from(weighed.body).unwrap_or(u64::MAX);
                let within = |hints: Hints| body <= hints.limit(base, percent);
                within(weighed.hints) || (!hinted && within(one) && weighed.speedup)
            }
            _ => {
                let within = |hints: Hints| {
                    weighed.growth
                        < i64::try_from(hints.limit(INLINE_INSNS_AUTO, percent)).unwrap_or(i64::MAX)
                };
                within(weighed.hints)
                    || (!hinted && within(one) && weighed.speedup)
                    || (weighed.growth < as_i64(self.how.limit) && weighed.overall <= 0)
            }
        }
    }

    /// Why a call that was weighed is not inlined, if it is not.
    fn refused(
        &mut self,
        module: &Module,
        caller: FuncId,
        call: Inst,
        weighed: &Weighed,
        pool: &Pool,
    ) -> Option<InlineFailure> {
        if !self.wants(weighed) {
            return Some(InlineFailure::TooLarge);
        }
        let func = &module[caller];
        let target = &module[weighed.callee];
        if let Some(wanted) = target.target {
            if !func.target.unwrap_or(self.how.isa).covers(wanted) {
                return Some(InlineFailure::Target);
            }
        }
        if calls_twice(module, target, self.how.names) {
            return Some(InlineFailure::Setjmp);
        }
        let cold = func.attrs.set.contains(AttrSet::COLD)
            || target.attrs.set.contains(AttrSet::COLD)
            || target.attrs.set.contains(AttrSet::NORETURN);
        if cold && grows(func, call, target, weighed.body, self.how) {
            return Some(InlineFailure::Unlikely);
        }
        // gcc takes a call that is not hot only when the program does not grow by it, which is
        // the last test in `want_inline_small_function_p`. A large function with a loop that only
        // `main` calls, with a count it knows each time, stays a call, where the hint alone would
        // have copied it at each.
        if weighed.growth > 0
            && (weighed.growth >= as_i64(self.how.limit) || weighed.overall > 0)
            && !self.hot(weighed.callee)
        {
            return Some(InlineFailure::Unlikely);
        }
        let growth = usize::try_from(weighed.growth).unwrap_or(0);
        if self.unit + growth > self.most {
            return Some(InlineFailure::UnitGrowth);
        }
        // gcc's `caller_growth_limits`: a caller past `large-function-insns` may grow to twice the
        // larger of what it was and the callee, and no further.
        let theirs = self.sizes.get(&weighed.callee).copied().unwrap_or(0);
        let mut limit = self.original.get(&caller).copied().unwrap_or(0).max(theirs);
        limit += limit * usize::try_from(LARGE_FUNCTION_GROWTH).unwrap_or(0) / 100;
        let after = self.sizes.get(&caller).copied().unwrap_or(0) + growth;
        let large = usize::try_from(LARGE_FUNCTION_INSNS).unwrap_or(usize::MAX);
        if after >= theirs && after > large && after > limit {
            return Some(InlineFailure::FunctionGrowth);
        }
        let layout = module.datalayout;
        let own = self.own.get(&caller).copied().unwrap_or_else(|| frame(func, layout));
        let body = pool.growth(target, layout);
        let bound = self.frames.get(&caller).copied();
        if !bound.is_some_and(|now| fits(own, now, body, self.how.growth)) {
            let now = frame(func, layout);
            self.frames.insert(caller, now);
            if !fits(own, now, body, self.how.growth) {
                return Some(InlineFailure::Frame);
            }
        }
        None
    }

    /// Inlines a call that came off the heap, or says why not, and puts what the copy brought on
    /// the heap.
    fn take(
        &mut self,
        module: &mut Module,
        caller: FuncId,
        call: Inst,
        weighed: Weighed,
        queue: &mut BinaryHeap<Entry>,
    ) {
        let share = self.how.share;
        let mut pool = self.pools.remove(&caller).unwrap_or(Pool { on: share, ..Pool::default() });
        let mut stats = self.stats.remove(&caller).unwrap_or_default();
        let mark = module[caller].counts().insts;
        let outcome = match self.refused(module, caller, call, &weighed, &pool) {
            Some(failure) => Err(failure),
            None => splice(
                module,
                caller,
                call,
                weighed.callee,
                self.how.convention,
                weighed.kind,
                &mut pool,
            ),
        };
        match outcome {
            Ok(()) => {
                stats.optimized(WEIGHED);
                if module[weighed.callee].attrs.set.contains(AttrSet::NO_LOOP_IDIOM) {
                    module[caller].attrs.set |= AttrSet::NO_LOOP_IDIOM;
                }
                // A pointer to a function the copy calls through is a direct call now. One to an
                // `always_inline` function is a promise, held to what the first pass holds one to,
                // and the rest go on the heap with the other calls the copy brought.
                for (_, inst, callee, kind) in resolved(module, caller, self.how, true, Some(mark))
                {
                    if kind != Kind::Always {
                        continue;
                    }
                    let covers = module[callee].target.is_none_or(|wanted| {
                        module[caller].target.unwrap_or(self.how.isa).covers(wanted)
                    });
                    let result = if callee == caller {
                        Err(InlineFailure::Recursive)
                    } else if !covers {
                        Err(InlineFailure::Target)
                    } else if calls_twice(module, &module[callee], self.how.names) {
                        Err(InlineFailure::Setjmp)
                    } else {
                        splice(
                            module,
                            caller,
                            inst,
                            callee,
                            self.how.convention,
                            Kind::Always,
                            &mut pool,
                        )
                    };
                    match result {
                        Ok(()) => stats.optimized(INLINED),
                        Err(failure) => stats.missed(failure.why()),
                    }
                }
                // What the copies added is every instruction made since, which is all that has
                // to be looked at to keep the sizes, the times, the frame and the call counts.
                let brought = self.profile(module, weighed.callee).calls.clone();
                let func = &module[caller];
                let mut grew = 0;
                let mut calls = Vec::new();
                for inst in (mark..func.counts().insts).map(Inst::from_usize) {
                    if func.block_of(inst).is_none() {
                        continue;
                    }
                    match (func[inst].opcode, func[inst].extra) {
                        (Opcode::Alloca, Extra::Mem(mem)) if func[inst].args.is_empty() => {
                            grew += func[mem].size;
                        }
                        (Opcode::Call, Extra::Call(info)) => {
                            if let Some(name) = func[info].callee {
                                calls.push((inst, name));
                            }
                        }
                        _ => {}
                    }
                }
                let (frequency, depth) = self.at.remove(&(caller, call)).unwrap_or((1.0, 0));
                *self.changed.entry(caller).or_default() += 1;
                let growth = isize::try_from(weighed.growth).unwrap_or(0);
                let size = self.sizes.entry(caller).or_default();
                *size = size.saturating_add_signed(growth);
                self.unit = self.unit.saturating_add_signed(growth);
                if let Some(spent) = self.spent.get_mut(&caller) {
                    *spent += weighed.adds;
                }
                if let Some(bound) = self.frames.get_mut(&caller) {
                    *bound += grew;
                }
                if let Some(total) = self.calls.get_mut(&module[weighed.callee].name) {
                    *total = total.saturating_sub(1);
                }
                for &(inst, name) in &calls {
                    let (often, deep) = brought.get(&name).copied().unwrap_or((1.0, 0));
                    self.at.insert((caller, inst), (frequency * often, depth + deep));
                    *self.calls.entry(name).or_default() += 1;
                }
                self.pools.insert(caller, pool);
                self.stats.insert(caller, stats);
                for (inst, _) in calls {
                    self.push(module, queue, caller, inst);
                }
                return;
            }
            Err(failure) => stats.missed(failure.hint()),
        }
        self.pools.insert(caller, pool);
        self.stats.insert(caller, stats);
    }
}

/// The parameters of each function that every call to it passes the same constant for, when this
/// unit sees every call to it, which is what [`crate::ipcp`] makes constants in the body.
fn settled(module: &Module, graph: &CallGraph) -> Map<FuncId, Set<Value>> {
    let by_name: Map<Symbol, FuncId> =
        crate::ipa::closed(module, graph).into_iter().map(|id| (module[id].name, id)).collect();
    let mut seen: Map<FuncId, Vec<Option<(Imm, Type)>>> = Map::default();
    for id in module.funcs() {
        let func = &module[id];
        for call in direct(func) {
            let Extra::Call(info) = func[call].extra else { continue };
            let Some(&callee) = func[info].callee.and_then(|name| by_name.get(&name)) else {
                continue;
            };
            let target = &module[callee];
            let Some(entry) = target.entry() else { continue };
            let values = passed(func, call, target);
            let now: Vec<Option<(Imm, Type)>> =
                target[entry].params.iter().map(|param| values.get(param).copied()).collect();
            match seen.get_mut(&callee) {
                Some(before) => {
                    for (was, is) in before.iter_mut().zip(now) {
                        if *was != is {
                            *was = None;
                        }
                    }
                }
                None => {
                    seen.insert(callee, now);
                }
            }
        }
    }
    seen.into_iter()
        .filter_map(|(id, held)| {
            let func = &module[id];
            let entry = func.entry()?;
            let params: Set<Value> = func[entry]
                .params
                .iter()
                .zip(held)
                .filter_map(|(&param, held)| held.map(|_| param))
                .collect();
            (!params.is_empty()).then_some((id, params))
        })
        .collect()
}

/// The direct calls a function makes.
fn direct(func: &Func) -> Vec<Inst> {
    func.blocks()
        .flat_map(|block| func.insts(block))
        .filter(|&inst| {
            func[inst].opcode == Opcode::Call
                && matches!(func[inst].extra, Extra::Call(info) if func[info].callee.is_some())
        })
        .collect()
}

/// How large a function is, the way the second pass counts the unit and a caller.
fn measure(func: &Func, names: &Interner) -> usize {
    folded_size(func, Set::default(), Map::default(), Some(names))
}

/// What [`Profile`] says about a function.
fn profile(func: &Func, names: &Interner) -> Profile {
    let cfg = Cfg::new(func);
    let loops = Loops::new(&cfg, &Dominators::new(&cfg));
    let guessed = Frequencies::of(func, &cfg, &loops, &Callees::nothing());
    let entry = guessed.entry().raw().max(1) as f64;
    let mut frequency = Map::default();
    let mut depth = Map::default();
    for block in func.blocks() {
        frequency.insert(block, guessed.get(block).raw() as f64 / entry);
        // Counted as gcc counts, so a block in one loop is one deep.
        depth.insert(block, loops.innermost(block).map_or(0, |inner| loops.depth(inner) + 1));
    }
    let time = folded(func, Set::default(), Map::default(), Some(names), Some(&frequency)).1;
    let mut calls: Map<Symbol, (f64, u32)> = Map::default();
    for inst in direct(func) {
        let (Some(block), Extra::Call(info)) = (func.block_of(inst), func[inst].extra) else {
            continue;
        };
        let Some(name) = func[info].callee else { continue };
        let often = frequency.get(&block).copied().unwrap_or(1.0);
        let deep = depth.get(&block).copied().unwrap_or(0);
        let seen = calls.entry(name).or_insert((often, deep));
        if often > seen.0 {
            *seen = (often, deep);
        }
    }
    Profile { frequency, depth, time, calls }
}

/// Whether the constants a call passes make one of the body's loops count known, which is gcc's
/// `loop_iterations` and `loop_stride` hints.
///
/// The count is known when a comparison that decides whether the loop is left sets a value the
/// loop steps against a value made of parameters the call passes constants for, which is a count
/// gcc's `number_of_iterations_exit` can work out. A test against a load or a call result is no
/// count, however constant the other side is. The stride is known when the loop steps one of its
/// header's values by a value made of those parameters.
fn loops_known(callee: &Func, values: &Map<Value, (Imm, Type)>) -> bool {
    let Some(entry) = callee.entry() else { return false };
    if values.is_empty() {
        return false;
    }
    let params = &callee[entry].params;
    let known = |value: Value| {
        let mut found = Vec::new();
        made_of_params(callee, entry, value, ASKED_DEPTH, &mut found)
            && !found.is_empty()
            && found
                .iter()
                .all(|&at| params.get(at).is_some_and(|param| values.contains_key(param)))
    };
    let invariant = |value: Value| {
        let mut found = Vec::new();
        made_of_params(callee, entry, value, ASKED_DEPTH, &mut found)
    };
    let cfg = Cfg::new(callee);
    let loops = Loops::new(&cfg, &Dominators::new(&cfg));
    for id in loops.all() {
        let header = &callee[loops.header(id)].params;
        // The header's values the loop adds to or takes from by the same amount each time round.
        let mut stepped: Set<Value> = Set::default();
        for &block in loops.blocks(id) {
            for inst in callee.insts(block) {
                if !matches!(callee[inst].opcode, Opcode::Add | Opcode::Sub) {
                    continue;
                }
                if let [from, by] = callee[callee[inst].args] {
                    if header.contains(&from) && known(by) {
                        return true;
                    }
                    if header.contains(&from) && invariant(by) {
                        stepped.insert(from);
                    }
                }
            }
        }
        // A stepped value, or the step itself, which is what the test reads once the loop is
        // rotated, and either through a conversion.
        let steps = |value: Value| {
            let mut value = value;
            if let Def::Result { inst, .. } = callee[value].def {
                if matches!(callee[inst].opcode, Opcode::SExt | Opcode::ZExt | Opcode::Trunc) {
                    if let Some(&from) = callee[callee[inst].args].first() {
                        value = from;
                    }
                }
            }
            if stepped.contains(&value) {
                return true;
            }
            let Def::Result { inst, .. } = callee[value].def else { return false };
            matches!(callee[inst].opcode, Opcode::Add | Opcode::Sub)
                && callee[callee[inst].args].first().is_some_and(|from| stepped.contains(from))
        };
        for exit in loops.exits(id) {
            let Some(branch) = callee.terminator(exit.from) else { continue };
            if callee[branch].opcode != Opcode::BrIf {
                continue;
            }
            let Some(&test) = callee[callee[branch].args].first() else { continue };
            let Def::Result { inst, .. } = callee[test].def else { continue };
            if callee[inst].opcode != Opcode::ICmp {
                continue;
            }
            if let [left, right] = callee[callee[inst].args] {
                if (known(left) && steps(right)) || (known(right) && steps(left)) {
                    return true;
                }
            }
        }
    }
    false
}

/// Whether the body calls through a parameter the call passes the address of a function for,
/// which the copy makes a direct call, gcc's `indirect_call` hint.
fn indirect_known(module: &Module, func: &Func, call: Inst, callee: &Func) -> bool {
    let Some(entry) = callee.entry() else { return false };
    let args = &func[func[call].args];
    callee.blocks().flat_map(|block| callee.insts(block)).any(|inst| {
        if callee[inst].opcode != Opcode::CallIndirect {
            return false;
        }
        let Some(&through) = callee[callee[inst].args].first() else { return false };
        let Def::Param { block, index } = callee[through].def else { return false };
        let Some(&arg) = usize::try_from(index).ok().and_then(|at| args.get(at)) else {
            return false;
        };
        block == entry
            && matches!(func[arg].def, Def::Result { inst: made, .. }
                if func[made].opcode == Opcode::GlobalAddr
                    && matches!(func[made].extra, Extra::Symbol(name)
                        if matches!(module.lookup(name), Some(SymbolRef::Func(_)))))
    })
}

/// A count as a signed one, which a growth is measured in.
fn as_i64(count: usize) -> i64 {
    i64::try_from(count).unwrap_or(i64::MAX)
}
