//! Puts every loop into the shape the loop passes are allowed to assume.
//!
//! Design: `spec/optimizer/26-loop-canonicalization.md` sections 26.2 through 26.5, and 26.7 for
//! where it sits.
//!
//! Document 07.3 named four properties every loop must have before the loop pipeline runs: a
//! preheader, a single latch, dedicated exits, and loop-closed SSA. This establishes all four. It
//! does not do section 26.6's header copying, which is the fifth and the only one that changes what
//! the program computes rather than where its blocks are, and which is its own pass for that
//! reason.
//!
//! # Why one pass and not four checks in four passes
//!
//! Every loop transformation needs somewhere to put code. Hoisting needs a block that runs once
//! before the loop and dominates the header. Unrolling needs somewhere for a prologue. Without a
//! preheader each of those passes makes one, each does it a little differently, and each carries
//! the
//! case where the header has several predecessors from outside. With one, each of them writes
//! `insert at the end of the preheader` and stops thinking about it. Section 26.1 makes that trade
//! and it is not a close call: what it costs is a handful of empty blocks, and the pass that takes
//! empty blocks out already exists.
//!
//! # The order is the whole design
//!
//! Section 26.8 says canonicalization does not converge if it is done in the wrong order, and the
//! right order is preheaders, then latches, then exits, then loop-closed SSA. Making a preheader
//! changes which blocks are predecessors of a header, which changes what counts as a latch.
//! Splitting
//! a latch adds a block that may need to be a dedicated exit. Loop-closed SSA goes last because it
//! is
//! the only one of the four that reads the final shape of the graph rather than changing it.
//!
//! Done in that order one pass is enough, and that is asserted rather than assumed: running the
//! pass
//! twice over the same function changes nothing the second time, which is
//! [`Canon::run`]'s postcondition and what `a_second_run_changes_nothing` checks.
//!
//! # The forest is rebuilt rather than patched
//!
//! Every split adds a block that belongs to some loop, so the forest the pass started from is wrong
//! the moment it makes the first one. GCC patches the forest as it goes, which is faster and is
//! where
//! its loop bugs live. Section 26.8 has rucc rebuilding instead, per document 06.5's rule that a
//! stale analysis is worse than an absent one, so each of the four steps works out what it wants
//! from a forest it just asked for, makes those edits, throws the cache away and asks again.
//!
//! Throwing it away is the part that is easy to leave out and is not optional. The cache hands back
//! whatever it computed last time until somebody clears it, so a step that made an edit and asked
//! again without clearing would be reading the graph as it was before its own edit, which either
//! makes the same edit for ever or stops after the first one.
//!
//! What it asks for is a round rather than one edit, and `wanted` in this module carries the
//! argument for why that is allowed. It comes down to the edit being harmless whether or not the
//! forest still describes the function, which is not true of most passes and is true of this one
//! because the only thing it ever does is put an empty block on a set of edges.
//!
//! # Irreducible regions are left exactly alone
//!
//! Section 26.2 has one sentence that is really a correctness requirement: the irreducible marking
//! has to survive canonicalization, because document 06.4 has the loop passes declining those
//! regions
//! and the decline stops happening if the marking is lost. Here that is free rather than careful.
//! The
//! forest never reports an irreducible region as a loop, so no step of this pass ever looks at one,
//! and the forest is rebuilt from the graph rather than carried across the edit, so the marking is
//! worked out again from a graph that still has the same cycles in it.
//!
//! # Which level
//!
//! Everywhere above `-O0`, and it is the first thing that runs at loop level. On its own it is a
//! no-op on the generated code: the blocks it adds are empty, the parameters it adds have one
//! argument each, and [`crate::simplify_cfg`] takes out both to a fixed point. That is the point.
//! The cost is paid now so that the passes that use it are simple, and until they land the
//! measurement to make is that the cost really is nothing, which is what the corpus says.

use std::collections::{HashMap, HashSet};

use rucc_ir::{Block, BlockCall, Builder, Def, Func, Inst, Type, Value};

use crate::cfg::Cfg;
use crate::dom::Dominators;
use crate::frontier::Frontiers;
use crate::loops::{LoopId, Loops};
use crate::{Analyses, Analysis, Fuel, Pass, Preserved, Stats};

const PREHEADER: &str = "preheader made for a loop whose header several blocks outside it reach";
const LATCH: &str = "back edges to one header routed through one latch";
const EXIT: &str = "exit block split off so it belongs to the loop that leaves through it";
const CLOSED: &str = "value used outside the loop that defines it passed through the exit instead";
const NO_FUEL: &str = "loop left as it was, the pass ran out of fuel";

/// Establishes the four canonical properties of section 26.7 step two.
#[derive(Debug)]
pub struct Canon;

impl Pass for Canon {
    fn name(&self) -> &'static str {
        "canon"
    }

    fn describe(&self) -> &'static str {
        "gives every loop a preheader, one latch, exits of its own and loop closed form"
    }

    fn preserves(&self) -> Preserved {
        // Blocks and edges both move, so nothing read off the graph survives, and the values a
        // loop hands to the code after it stop being the values that code names.
        Preserved::NONE
    }

    fn run(&self, func: &mut Func, an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        if func.entry().is_none() {
            return stats;
        }
        // Section 26.8's order. Each step asks for the forest again because the step before it
        // invalidated the one it was given, and each returns early on empty fuel rather than
        // leaving a half made property behind for the next step to trip over.
        for step in [preheaders, latches, exits, closed] {
            if !step(func, an, fuel, &mut stats) {
                stats.missed(NO_FUEL);
                break;
            }
        }
        an.clear();
        stats
    }
}

/// Section 26.2. Gives a header one predecessor from outside whose only successor is the header.
///
/// rucc wants the equivalent of GCC's `CP_SIMPLE_PREHEADERS` and not its fallthru variant, because
/// the fallthru form constrains block layout and rucc's layout is document 38's, decided at the
/// machine level over a graph that has no loop forest attached any more. Requiring it here would
/// buy
/// nothing.
fn preheaders(func: &mut Func, an: &mut Analyses, fuel: &mut Fuel, stats: &mut Stats) -> bool {
    loop {
        let jobs = wanted(func, an, |loops, cfg, id| {
            let header = loops.header(id);
            if loops.preheader(cfg, id).is_some() {
                return None;
            }
            let outside: Vec<Block> = cfg
                .predecessors(header)
                .iter()
                .copied()
                .filter(|&pred| !loops.contains(id, pred))
                .collect();
            (!outside.is_empty()).then_some((header, outside))
        });
        if jobs.is_empty() {
            return true;
        }
        if !apply(func, an, fuel, stats, jobs, PREHEADER) {
            return false;
        }
    }
}

/// Section 26.3. Routes every back edge through one block, so "the back edge" means something.
///
/// Document 07.1 refuses a multiple latch loop rather than guessing which back edge belongs to an
/// inner loop the way GCC does. Section 26.3 points out that the refusal was really a deferral to
/// here: several back edges to one header become one latch by an edit, which is always right, and
/// what stays refused is a loop with several headers, which is irreducibility and is not a loop.
fn latches(func: &mut Func, an: &mut Analyses, fuel: &mut Fuel, stats: &mut Stats) -> bool {
    loop {
        let jobs = wanted(func, an, |loops, cfg, id| {
            let header = loops.header(id);
            let from = loops.latches(id).to_vec();
            let single = from.len() == 1 && cfg.successors(from[0]).len() == 1 && from[0] != header;
            (!from.is_empty() && !single).then_some((header, from))
        });
        if jobs.is_empty() {
            return true;
        }
        if !apply(func, an, fuel, stats, jobs, LATCH) {
            return false;
        }
    }
}

/// Section 26.5. Gives an exit block predecessors only from the loop it leaves.
///
/// Otherwise a transformation that rewrites the exit reaches control flow that had nothing to do
/// with this loop, and the block cannot carry the loop's exit parameters, because those parameters
/// would be undefined on the edges that came from somewhere else. It is the same edit as a
/// preheader
/// pointed the other way.
fn exits(func: &mut Func, an: &mut Analyses, fuel: &mut Fuel, stats: &mut Stats) -> bool {
    loop {
        let jobs = wanted(func, an, |loops, cfg, id| {
            for exit in loops.exits(id) {
                let outside: Vec<Block> = cfg
                    .predecessors(exit.to)
                    .iter()
                    .copied()
                    .filter(|&pred| !loops.contains(id, pred))
                    .collect();
                if !outside.is_empty() {
                    let inside: Vec<Block> = cfg
                        .predecessors(exit.to)
                        .iter()
                        .copied()
                        .filter(|&pred| loops.contains(id, pred))
                        .collect();
                    return Some((exit.to, inside));
                }
            }
            None
        });
        if jobs.is_empty() {
            return true;
        }
        if !apply(func, an, fuel, stats, jobs, EXIT) {
            return false;
        }
    }
}

/// Section 26.4. A use outside a loop names a parameter of the exit rather than the definition.
///
/// The reason is section 26.4's: unroll a loop whose result is read afterwards and without this the
/// outside use names a value the body defines, so after unrolling there are several copies of that
/// body and the use has to be pointed at the right one. With it the use names the exit block's
/// parameter and unrolling only has to get the argument on the exit edge right. One update rather
/// than many, and the many are where the bugs are. Peeling, versioning and vectorization all want
/// the same thing.
///
/// In rucc's IR this is close to free, because a value crossing a block boundary is already a block
/// parameter and there is nothing to invent. What it costs is a dominance query per outside use,
/// and a loop with several exits gets a parameter at each of them for the same value, which is
/// correct and is section 26.4's warning about what the property is worth. Where those exits meet
/// again the meeting gets one too, since a parameter is a name only where its block dominates, and
/// the section does not say that because it is written against phi nodes, where the merge at the
/// meeting is already there to be edited.
fn closed(func: &mut Func, an: &mut Analyses, fuel: &mut Fuel, stats: &mut Stats) -> bool {
    loop {
        let (dom, fronts, loops) = (an.dominators(func), an.frontiers(func), an.loops(func));
        let Some(job) = leak(func, dom, fronts, loops) else { return true };
        if !fuel.take() {
            return false;
        }
        close(func, dom, loops, &job);
        stats.optimized(CLOSED);
        // Every edge is where it was. A parameter appeared on some blocks and the edges into them
        // hand over one more value, which is a change to what the blocks say and not to which
        // block reaches which, so the graph, both trees, the frontiers and the forest all still
        // describe this function and the next round reads them again instead of building them
        // again. What is live where did change, since the value now arrives by name. So did the
        // frequencies, because a branch on a value this renamed is a branch the predictors read
        // differently. The clear that used to be here was a walk of the whole function per leak,
        // on top of the one the repair itself does. tamnd/rucc#1045.
        let kept = Preserved::ALL.without(Analysis::Liveness).without(Analysis::Frequencies);
        an.settle(func, kept, false);
    }
}

/// A value that leaves a loop without going through the exit, and where to catch it.
#[derive(Debug)]
pub(crate) struct Leak {
    /// The value defined inside the loop.
    value: Value,
    /// The blocks that grow a parameter for it. Every one of them is outside the loop and is
    /// dominated by the block the value is defined in, so there is something to hand over on
    /// every edge into each of them.
    at: Vec<Block>,
    /// The loop, so the repair knows which uses were already naming the right thing.
    id: LoopId,
}

/// Finds one value that crosses an exit without being handed over there.
///
/// One at a time rather than all at once, because adding a parameter changes what the uses are and
/// which blocks the exits dominate, and a list worked out before the first edit is a list that is
/// wrong after it. The cost is a walk per repair, and section 26.9 says the way to make that cheap
/// is GCC's `changed_bbs` set, which is worth building the second time it is needed rather than the
/// first.
fn leak(func: &Func, dom: &Dominators, fronts: &Frontiers, loops: &Loops) -> Option<Leak> {
    loops.all().find_map(|id| leaked(func, dom, fronts, loops, id))
}

/// The same question asked about one loop, for a caller that wants that loop in closed form.
///
/// [`crate::split`] is that caller. It copies one loop, and a value the copy defines that something
/// after the loop reads is the whole of what makes copying wrong, so it repairs the loop it is about
/// to copy rather than refusing it. Repairing here rather than by running this whole pass again is
/// the difference between paying for the one loop and paying for every loop in the function, which
/// was measured at 17672 bytes of `.text` on the SQLite amalgamation.
///
/// The repair adds a block parameter and rewrites uses. It moves no edge and creates no block, so a
/// caller holding a graph, a dominator tree or a loop forest may keep all three across it.
pub(crate) fn leaked(
    func: &Func,
    dom: &Dominators,
    fronts: &Frontiers,
    loops: &Loops,
    id: LoopId,
) -> Option<Leak> {
    for block in func.blocks() {
        if loops.contains(id, block) {
            continue;
        }
        for inst in func.insts(block) {
            for value in named(func, inst) {
                if !defined_in(func, loops, id, value) {
                    continue;
                }
                let at = caught(func, dom, fronts, loops, id, value);
                // A use no placement covers is one this repair does not reach, and reporting it
                // would have the caller do the work and find the use still there, which for a
                // caller that asks again until the answer is nothing is a loop that does not end.
                if !covered(dom, &at, block) {
                    continue;
                }
                return Some(Leak { value, at, id });
            }
        }
    }
    None
}

/// Every value one instruction names, its own operands and the arguments it hands to its targets.
///
/// An iterator rather than a list, because the caller asks this of every instruction outside the
/// loop once per loop and a list would be one allocation each time. tamnd/rucc#1015.
fn named(func: &Func, inst: Inst) -> impl Iterator<Item = Value> + '_ {
    func[func[inst].args]
        .iter()
        .copied()
        .chain(func.successors(inst).flat_map(move |call| func[call.args].iter().copied()))
}

/// The block the value is defined in.
fn defining(func: &Func, value: Value) -> Option<Block> {
    match func[value].def {
        Def::Result { inst, .. } => func.block_of(inst),
        Def::Param { block, .. } => Some(block),
    }
}

/// Whether this loop defines the value.
fn defined_in(func: &Func, loops: &Loops, id: LoopId, value: Value) -> bool {
    defining(func, value).is_some_and(|block| loops.contains(id, block))
}

/// Where a parameter has to go for this value to stop leaving the loop without being handed over.
///
/// Every exit destination gets one, which is section 26.4's answer and is the whole of it when the
/// exit dominates the use. It is not the whole of it when two exits meet at a join, because neither
/// of them dominates the join and a use there names the definition inside the loop however many
/// parameters the exits grew. The join needs one too, and so does anything the joins in turn meet
/// at, which is the iterated dominance frontier of the exits. That is the same set SSA construction
/// puts merges at and it is the same argument, that a block two definitions reach needs one of its
/// own for a use below it to have a single name to say.
///
/// A block the definition does not dominate is left out, because the value does not reach it and a
/// parameter there would have edges with nothing to put on them. That covers a loop whose exits do
/// not all come after the definition, where the value is genuinely absent on one way out. Blocks
/// inside the loop are left out for a different reason: inside the loop the definition is the only
/// name the value has, so a merge there would only hand the value to itself.
fn caught(
    func: &Func,
    dom: &Dominators,
    fronts: &Frontiers,
    loops: &Loops,
    id: LoopId,
    value: Value,
) -> Vec<Block> {
    let Some(from) = defining(func, value) else { return Vec::new() };
    let mut at = Vec::new();
    let mut seen: HashSet<Block> = HashSet::new();
    let mut queue: Vec<Block> = Vec::new();
    for exit in loops.exits(id) {
        if seen.insert(exit.to) {
            queue.push(exit.to);
        }
    }
    while let Some(block) = queue.pop() {
        if loops.contains(id, block) || !dom.dominates(from, block) {
            continue;
        }
        at.push(block);
        for &next in fronts.of(block) {
            if seen.insert(next) {
                queue.push(next);
            }
        }
    }
    at
}

/// Whether one of the placements dominates this block, so a use in it has a parameter to name.
fn covered(dom: &Dominators, at: &[Block], block: Block) -> bool {
    let mut here = Some(block);
    while let Some(now) = here {
        if at.contains(&now) {
            return true;
        }
        here = dom.immediate_dominator(now);
    }
    false
}

/// The name the value goes by at the end of this block, which is the nearest parameter above it.
fn reaching(dom: &Dominators, param: &HashMap<Block, Value>, value: Value, block: Block) -> Value {
    let mut here = Some(block);
    while let Some(now) = here {
        if let Some(&had) = param.get(&now) {
            return had;
        }
        here = dom.immediate_dominator(now);
    }
    value
}

/// Adds the parameters, passes the value on every edge in, and points the uses outside at them.
///
/// Every parameter this writes holds the same value the loop defined, since each one is handed the
/// value or another parameter that was handed it, so a use that ends up naming one of them is right
/// whatever path it took. What has to hold is only that the parameter is in scope where the use is,
/// which is why both the placement and the rewrite go by dominance.
pub(crate) fn close(func: &mut Func, dom: &Dominators, loops: &Loops, job: &Leak) {
    let ty = func[job.value].ty;
    let param: HashMap<Block, Value> =
        job.at.iter().map(|&block| (block, func.append_param(block, ty))).collect();
    // Each edge in hands over whatever the value is called at the end of the block it leaves, which
    // is the value itself on the way out of the loop and a parameter written above on the joins.
    for term in terminators(func) {
        let Some(from) = func.block_of(term) else { continue };
        let hand = reaching(dom, &param, job.value, from);
        for at in func.target_list(term).iter() {
            let call = func[at];
            if !param.contains_key(&call.block) {
                continue;
            }
            let args = func.append_arg(call.args, hand);
            func.set_block_call(at, BlockCall { block: call.block, args });
        }
    }
    for block in func.blocks().collect::<Vec<_>>() {
        if loops.contains(job.id, block) {
            continue;
        }
        let hand = reaching(dom, &param, job.value, block);
        if hand == job.value {
            continue;
        }
        for inst in func.insts(block).collect::<Vec<_>>() {
            let mut lists = vec![func[inst].args];
            lists.extend(func.target_list(inst).iter().map(|at| func[at].args));
            for list in lists {
                func.rewrite(list, |value| if value == job.value { hand } else { value });
            }
        }
    }
}

/// The edits one step wants that do not tread on each other, from a forest it asks for itself.
///
/// The closure gets the forest, the graph and one loop, and answers with a block and the
/// predecessors of it to route through a new one, or nothing. It is asked about every loop and the
/// answers come back together, less any whose blocks another answer already named.
///
/// A round rather than one edit at a time, and the reason is what one edit costs. The forest, the
/// graph and both trees are the size of the function and are thrown away after every edit, so a
/// function with n loops in it paid for n of each, and the loop headers of a program are not a
/// small number: jtckdint from the corpus has 1600 of them and this pass was building 5200 forests
/// to canonicalize it. Most of those edits have nothing to do with each other. A preheader made in
/// front of one header does not change which blocks reach another, and what a round costs is one
/// forest however many loops are in it. tamnd/rucc#1045.
///
/// What makes it safe is that [`route`] is right on its own terms whatever else has happened.
/// Putting a block on a set of edges changes no value and no order, so the worst an edit made
/// against a forest that has moved under it can be is an edit nothing needed, and an empty block
/// with one predecessor is what [`crate::simplify_cfg`] exists to take back out. The batch is not
/// relying on the answers still being true, only on them still being harmless.
///
/// Two jobs naming a block between them are not both taken, and that is the whole of the filter.
/// It costs a round per level of nesting, since an inner loop's header is reached from the outer
/// loop's and the outer job claims that block first, which is where the count comes down to: the
/// number of forests is the depth of the deepest nest rather than the number of loops.
fn wanted(
    func: &mut Func,
    an: &mut Analyses,
    mut ask: impl FnMut(&Loops, &Cfg, LoopId) -> Option<(Block, Vec<Block>)>,
) -> Vec<(Block, Vec<Block>)> {
    let cfg = an.cfg(func);
    let loops = an.loops(func);
    let mut claimed: HashSet<Block> = HashSet::new();
    let mut jobs: Vec<(Block, Vec<Block>)> = Vec::new();
    for id in loops.all() {
        let Some((to, from)) = ask(loops, cfg, id) else { continue };
        if claimed.contains(&to) || from.iter().any(|block| claimed.contains(block)) {
            continue;
        }
        claimed.insert(to);
        claimed.extend(from.iter().copied());
        jobs.push((to, from));
    }
    jobs
}

/// Makes a round of edits and throws the analyses away once, at the end of it.
///
/// The cache hands back whatever it computed last time until somebody clears it, and every edit here
/// moves edges. Without the clear a step would ask a stale forest the same question, get the same
/// answer, and route the same edge for ever. [`closed`] does not need this because adding a block
/// parameter leaves the graph alone.
///
/// Fuel is taken per edit rather than per round, because fuel is the bisection interface and what it
/// has to be able to name is the edit somebody is looking for. A round that runs out part way
/// through still clears, since the edits before the one that stopped have already happened.
fn apply(
    func: &mut Func,
    an: &mut Analyses,
    fuel: &mut Fuel,
    stats: &mut Stats,
    jobs: Vec<(Block, Vec<Block>)>,
    what: &'static str,
) -> bool {
    let mut out = true;
    for (to, from) in jobs {
        if !fuel.take() {
            out = false;
            break;
        }
        route(func, &from, to);
        stats.optimized(what);
    }
    an.clear();
    out
}

/// Puts a new block between the given predecessors and the block, carrying the same arguments.
///
/// The new block takes a parameter for each of the target's parameters and passes them straight on,
/// so the edges that were redirected hand their arguments to it and it hands them to the target.
/// [`crate::simplify_cfg`] takes the parameters back out when they turn out to be one value, which
/// is the ordinary case and is section 26.7's point that the cost of the extra parameters is paid
/// back by a pass that exists anyway.
fn route(func: &mut Func, from: &[Block], to: Block) {
    let types: Vec<Type> = func[to].params.iter().map(|&param| func[param].ty).collect();
    let fresh = func.create_block();
    let params: Vec<Value> = types.iter().map(|&ty| func.append_param(fresh, ty)).collect();
    Builder::new(func, fresh).jump(to, &params);
    for &pred in from {
        let Some(term) = func.terminator(pred) else { continue };
        for at in func.target_list(term).iter() {
            let call = func[at];
            if call.block == to {
                func.set_block_call(at, BlockCall { block: fresh, args: call.args });
            }
        }
    }
}

/// Every terminator in the function, as a list so the function can be edited while it is walked.
fn terminators(func: &Func) -> Vec<Inst> {
    func.blocks().filter_map(|block| func.terminator(block)).collect()
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{Block, Builder, Flags, Func, IntPred, Opcode, Signature, Type, Value};

    use super::Canon;
    use crate::cfg::Cfg;
    use crate::dom::Dominators;
    use crate::loops::Loops;
    use crate::stats::Kind;
    use crate::{Fuel, Pass, Stats};

    /// Runs the pass with as much fuel as it wants.
    fn canon(func: &mut Func) -> Stats {
        Canon.run(func, &mut crate::machine::fixtures::analyses(), &mut Fuel::unlimited())
    }

    /// The forest of the function as it is now.
    fn forest(func: &Func) -> (Cfg, Dominators, Loops) {
        let cfg = Cfg::new(func);
        let dom = Dominators::new(&cfg);
        let loops = Loops::new(&cfg, &dom);
        (cfg, dom, loops)
    }

    /// A counted loop entered from two places, so its header has two predecessors from outside.
    ///
    /// ```text
    /// entry: br c -> one, two        one: jump head(0)     two: jump head(1)
    /// head(i): br i < n -> body, done
    /// body: jump head(i + 1)
    /// done: ret i
    /// ```
    fn two_ways_in(func: &mut Func, names: &mut Interner) -> Vec<Block> {
        let _ = names;
        let entry = func.create_block();
        let one = func.create_block();
        let two = func.create_block();
        let head = func.create_block();
        let body = func.create_block();
        let done = func.create_block();
        let c = func.append_param(entry, Type::int(1));
        let n = func.append_param(entry, Type::int(32));
        let i = func.append_param(head, Type::int(32));
        Builder::new(func, entry).br_if(c, one, &[], two, &[]);
        let zero = Builder::new(func, one).iconst(Type::int(32), 0);
        Builder::new(func, one).jump(head, &[zero]);
        let start = Builder::new(func, two).iconst(Type::int(32), 1);
        Builder::new(func, two).jump(head, &[start]);
        let test = Builder::new(func, head).icmp(IntPred::Slt, i, n);
        Builder::new(func, head).br_if(test, body, &[], done, &[]);
        let one_more = Builder::new(func, body).iconst(Type::int(32), 1);
        let next = Builder::new(func, body).binary(Opcode::Add, i, one_more, Flags::NONE);
        Builder::new(func, body).jump(head, &[next]);
        Builder::new(func, done).ret(&[i]);
        vec![entry, one, two, head, body, done]
    }

    fn func_with_two_ways_in() -> (Func, Interner) {
        let mut names = Interner::new();
        let signature = Signature::new()
            .with_params(&[Type::int(1), Type::int(32)])
            .with_returns(&[Type::int(32)]);
        let mut func = Func::new(names.intern("f"), signature);
        two_ways_in(&mut func, &mut names);
        (func, names)
    }

    #[test]
    fn a_header_two_blocks_outside_reach_gets_one_block_they_both_go_through() {
        let (mut func, _names) = func_with_two_ways_in();
        let (cfg, dom, loops) = forest(&func);
        let id = loops.all().next().expect("there is a loop");
        assert!(loops.preheader(&cfg, id).is_none(), "it has no preheader to start with");
        let _ = dom;

        let stats = canon(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::PREHEADER), 1);

        let (cfg, _dom, loops) = forest(&func);
        let id = loops.all().next().expect("the loop is still there");
        let pre = loops.preheader(&cfg, id).expect("it has one now");
        assert_eq!(cfg.successors(pre), &[loops.header(id)], "and it goes only to the header");
        assert_eq!(cfg.predecessors(pre).len(), 2, "and both ways in come through it");
    }

    #[test]
    fn a_loop_with_two_back_edges_ends_up_with_one_latch() {
        let mut names = Interner::new();
        let signature =
            Signature::new().with_params(&[Type::int(1), Type::int(32)]).with_returns(&[]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let head = func.create_block();
        let split = func.create_block();
        let left = func.create_block();
        let right = func.create_block();
        let done = func.create_block();
        let c = func.append_param(entry, Type::int(1));
        func.append_param(entry, Type::int(32));
        Builder::new(&mut func, entry).jump(head, &[]);
        Builder::new(&mut func, head).br_if(c, split, &[], done, &[]);
        Builder::new(&mut func, split).br_if(c, left, &[], right, &[]);
        Builder::new(&mut func, left).jump(head, &[]);
        Builder::new(&mut func, right).jump(head, &[]);
        Builder::new(&mut func, done).ret(&[]);

        let (_cfg, _dom, loops) = forest(&func);
        let id = loops.all().next().expect("there is a loop");
        assert_eq!(loops.latches(id).len(), 2, "two back edges to start with");

        let stats = canon(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::LATCH), 1);

        let (cfg, _dom, loops) = forest(&func);
        let id = loops.all().next().expect("the loop is still there");
        assert_eq!(loops.latches(id).len(), 1, "one afterwards");
        let latch = loops.latches(id)[0];
        assert_eq!(cfg.successors(latch), &[loops.header(id)]);
        assert_eq!(cfg.predecessors(latch).len(), 2, "both back edges go through it");
    }

    #[test]
    fn an_exit_something_outside_also_reaches_is_split_off() {
        let mut names = Interner::new();
        let signature =
            Signature::new().with_params(&[Type::int(1), Type::int(32)]).with_returns(&[]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let pre = func.create_block();
        let head = func.create_block();
        let body = func.create_block();
        let after = func.create_block();
        let c = func.append_param(entry, Type::int(1));
        func.append_param(entry, Type::int(32));
        // `after` is reached both by leaving the loop and by a branch that never entered it.
        Builder::new(&mut func, entry).br_if(c, pre, &[], after, &[]);
        Builder::new(&mut func, pre).jump(head, &[]);
        Builder::new(&mut func, head).br_if(c, body, &[], after, &[]);
        Builder::new(&mut func, body).jump(head, &[]);
        Builder::new(&mut func, after).ret(&[]);

        let stats = canon(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::EXIT), 1);

        let (cfg, _dom, loops) = forest(&func);
        let id = loops.all().next().expect("the loop is still there");
        let exit = loops.exits(id)[0].to;
        assert!(
            cfg.predecessors(exit).iter().all(|&pred| loops.contains(id, pred)),
            "everything that reaches the exit is now inside the loop"
        );
    }

    #[test]
    fn a_value_the_loop_computes_and_the_code_after_reads_goes_through_the_exit() {
        let (mut func, _names) = func_with_two_ways_in();
        canon(&mut func);
        let (_cfg, _dom, loops) = forest(&func);
        let id = loops.all().next().expect("the loop is still there");
        let exit = loops.exits(id)[0].to;
        let term = func.terminator(exit).expect("the exit returns");
        let returned = func[func[term].args][0];
        assert!(
            func[exit].params.contains(&returned),
            "what is returned is the exit's parameter rather than the value the loop defined"
        );
    }

    /// Two counted loops one after the other, each entered from two places.
    ///
    /// One loop is not enough to see whether a step stops after the edit it makes first, and every
    /// other fixture here has one loop in it, which is how a pass that canonicalized one loop per
    /// run looked correct for as long as it did.
    fn func_with_two_loops_two_ways_in() -> (Func, Interner) {
        let mut names = Interner::new();
        let signature =
            Signature::new().with_params(&[Type::int(1), Type::int(32)]).with_returns(&[]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let c = func.append_param(entry, Type::int(1));
        let n = func.append_param(entry, Type::int(32));
        let mut heads = Vec::new();
        let mut from = entry;
        for _ in 0..2 {
            let one = func.create_block();
            let two = func.create_block();
            let head = func.create_block();
            let body = func.create_block();
            let after = func.create_block();
            let i = func.append_param(head, Type::int(32));
            Builder::new(&mut func, from).br_if(c, one, &[], two, &[]);
            let zero = Builder::new(&mut func, one).iconst(Type::int(32), 0);
            Builder::new(&mut func, one).jump(head, &[zero]);
            let start = Builder::new(&mut func, two).iconst(Type::int(32), 1);
            Builder::new(&mut func, two).jump(head, &[start]);
            let test = Builder::new(&mut func, head).icmp(IntPred::Slt, i, n);
            Builder::new(&mut func, head).br_if(test, body, &[], after, &[]);
            let step = Builder::new(&mut func, body).iconst(Type::int(32), 1);
            let next = Builder::new(&mut func, body).binary(Opcode::Add, i, step, Flags::NONE);
            Builder::new(&mut func, body).jump(head, &[next]);
            heads.push(head);
            from = after;
        }
        Builder::new(&mut func, from).ret(&[]);
        (func, names)
    }

    #[test]
    fn every_loop_that_wants_a_preheader_gets_one_and_not_just_the_first() {
        let (mut func, _names) = func_with_two_loops_two_ways_in();
        let (cfg, _dom, loops) = forest(&func);
        assert_eq!(loops.all().count(), 2, "two loops to start with");
        assert!(
            loops.all().all(|id| loops.preheader(&cfg, id).is_none()),
            "and neither of them has a preheader"
        );

        let stats = canon(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::PREHEADER), 2, "one made for each");

        let (cfg, _dom, loops) = forest(&func);
        assert_eq!(loops.all().count(), 2, "both loops are still there");
        for id in loops.all() {
            let pre = loops.preheader(&cfg, id).expect("each one has a preheader now");
            assert_eq!(cfg.successors(pre), &[loops.header(id)], "going only to its header");
            assert_eq!(cfg.predecessors(pre).len(), 2, "with both ways in through it");
        }

        let second = canon(&mut func);
        assert!(!second.changed(), "and the second run has nothing left to do");
    }

    #[test]
    fn a_second_run_changes_nothing() {
        let (mut func, _names) = func_with_two_ways_in();
        let first = canon(&mut func);
        assert!(first.changed(), "the first run has work to do");
        let before = format!("{func:?}");
        let second = canon(&mut func);
        assert!(!second.changed(), "section 26.8's order means one pass is enough");
        assert_eq!(before, format!("{func:?}"), "and the function really is untouched");
    }

    #[test]
    fn a_function_with_no_loops_is_left_alone() {
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::int(1)]).with_returns(&[]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let then = func.create_block();
        let other = func.create_block();
        let c = func.append_param(entry, Type::int(1));
        Builder::new(&mut func, entry).br_if(c, then, &[], other, &[]);
        Builder::new(&mut func, then).ret(&[]);
        Builder::new(&mut func, other).ret(&[]);
        let before = format!("{func:?}");
        let stats = canon(&mut func);
        assert!(!stats.changed());
        assert_eq!(before, format!("{func:?}"));
    }

    #[test]
    fn a_function_with_no_body_is_left_alone() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let stats = canon(&mut func);
        assert!(!stats.changed());
    }

    #[test]
    fn no_fuel_leaves_the_loop_where_it_is() {
        let (mut func, _names) = func_with_two_ways_in();
        let before = format!("{func:?}");
        let stats =
            Canon.run(&mut func, &mut crate::machine::fixtures::analyses(), &mut Fuel::of(0));
        assert_eq!(stats.count(Kind::Missed, super::NO_FUEL), 1);
        assert!(!stats.changed());
        assert_eq!(before, format!("{func:?}"));
    }

    /// Keeps the unused import honest when a helper stops being needed.
    #[allow(dead_code)]
    fn unused(value: Value) -> Value {
        value
    }
}
