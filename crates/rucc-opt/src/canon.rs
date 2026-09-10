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
//! stale analysis is worse than an absent one, so each of the four steps works out one edit from a
//! forest it just asked for, makes it, throws the cache away and asks again.
//!
//! Throwing it away is the part that is easy to leave out and is not optional. The cache hands back
//! whatever it computed last time until somebody clears it, so a step that made an edit and asked
//! again without clearing would be reading the graph as it was before its own edit, which either
//! makes the same edit for ever or stops after the first one.
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

use rucc_ir::{Block, BlockCall, Builder, Def, Func, Inst, Type, Value};

use crate::cfg::Cfg;
use crate::dom::Dominators;
use crate::loops::{LoopId, Loops};
use crate::{Analyses, Fuel, Pass, Preserved, Stats};

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
    while let Some((header, from)) = wanted(func, an, |loops, cfg, id| {
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
    }) {
        if !fuel.take() {
            return false;
        }
        apply(func, an, &from, header);
        stats.optimized(PREHEADER);
    }
    true
}

/// Section 26.3. Routes every back edge through one block, so "the back edge" means something.
///
/// Document 07.1 refuses a multiple latch loop rather than guessing which back edge belongs to an
/// inner loop the way GCC does. Section 26.3 points out that the refusal was really a deferral to
/// here: several back edges to one header become one latch by an edit, which is always right, and
/// what stays refused is a loop with several headers, which is irreducibility and is not a loop.
fn latches(func: &mut Func, an: &mut Analyses, fuel: &mut Fuel, stats: &mut Stats) -> bool {
    while let Some((header, from)) = wanted(func, an, |loops, cfg, id| {
        let header = loops.header(id);
        let from = loops.latches(id).to_vec();
        let single = from.len() == 1 && cfg.successors(from[0]).len() == 1 && from[0] != header;
        (!from.is_empty() && !single).then_some((header, from))
    }) {
        if !fuel.take() {
            return false;
        }
        apply(func, an, &from, header);
        stats.optimized(LATCH);
    }
    true
}

/// Section 26.5. Gives an exit block predecessors only from the loop it leaves.
///
/// Otherwise a transformation that rewrites the exit reaches control flow that had nothing to do
/// with this loop, and the block cannot carry the loop's exit parameters, because those parameters
/// would be undefined on the edges that came from somewhere else. It is the same edit as a
/// preheader
/// pointed the other way.
fn exits(func: &mut Func, an: &mut Analyses, fuel: &mut Fuel, stats: &mut Stats) -> bool {
    while let Some((to, from)) = wanted(func, an, |loops, cfg, id| {
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
    }) {
        if !fuel.take() {
            return false;
        }
        apply(func, an, &from, to);
        stats.optimized(EXIT);
    }
    true
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
/// and
/// a loop with several exits gets a parameter at each of them for the same value, which is correct
/// and is section 26.4's warning about what the property is worth.
fn closed(func: &mut Func, an: &mut Analyses, fuel: &mut Fuel, stats: &mut Stats) -> bool {
    loop {
        let cfg = an.cfg(func).clone();
        let dom = an.dominators(func).clone();
        let loops = an.loops(func).clone();
        let Some(job) = leak(func, &cfg, &dom, &loops) else { return true };
        if !fuel.take() {
            return false;
        }
        close(func, &job);
        stats.optimized(CLOSED);
        an.clear();
    }
}

/// A value that leaves a loop without going through the exit, and where to catch it.
#[derive(Debug)]
struct Leak {
    /// The exit block that will grow a parameter.
    at: Block,
    /// The value defined inside the loop.
    value: Value,
    /// The uses to point at the new parameter, as the instruction holding each.
    uses: Vec<Inst>,
}

/// Finds one value that crosses an exit without being handed over there.
///
/// One at a time rather than all at once, because adding a parameter changes what the uses are and
/// which blocks the exits dominate, and a list worked out before the first edit is a list that is
/// wrong after it. The cost is a walk per repair, and section 26.9 says the way to make that cheap
/// is GCC's `changed_bbs` set, which is worth building the second time it is needed rather than the
/// first.
fn leak(func: &Func, cfg: &Cfg, dom: &Dominators, loops: &Loops) -> Option<Leak> {
    for id in loops.all() {
        for exit in loops.exits(id) {
            // An exit whose destination is reached from outside the loop as well is not dedicated,
            // and a parameter there would be undefined on the other edges. The step before this one
            // makes them dedicated, so reaching this with one that is not means fuel ran out, and
            // the answer is to leave it rather than to write a parameter nothing can fill.
            if cfg.predecessors(exit.to).iter().any(|&pred| !loops.contains(id, pred)) {
                continue;
            }
            for inst in func.blocks().flat_map(|block| func.insts(block)) {
                let Some(holder) = func.block_of(inst) else { continue };
                if loops.contains(id, holder) || !dom.dominates(exit.to, holder) {
                    continue;
                }
                for &value in &func[func[inst].args] {
                    if !defined_in(func, loops, id, value) {
                        continue;
                    }
                    let uses = users(func, dom, exit.to, loops, id, value);
                    return Some(Leak { at: exit.to, value, uses });
                }
            }
        }
    }
    None
}

/// Whether this loop defines the value.
fn defined_in(func: &Func, loops: &Loops, id: LoopId, value: Value) -> bool {
    let block = match func[value].def {
        Def::Result { inst, .. } => func.block_of(inst),
        Def::Param { block, .. } => Some(block),
    };
    block.is_some_and(|block| loops.contains(id, block))
}

/// Every instruction the exit dominates that names the value, outside the loop.
fn users(
    func: &Func,
    dom: &Dominators,
    at: Block,
    loops: &Loops,
    id: LoopId,
    value: Value,
) -> Vec<Inst> {
    let mut found = Vec::new();
    for block in func.blocks() {
        if loops.contains(id, block) || !dom.dominates(at, block) {
            continue;
        }
        for inst in func.insts(block) {
            if func[func[inst].args].contains(&value) {
                found.push(inst);
            }
        }
    }
    found
}

/// Adds the parameter, passes the value on every edge in, and points the uses at it.
fn close(func: &mut Func, job: &Leak) {
    let ty = func[job.value].ty;
    let param = func.append_param(job.at, ty);
    // Every way into a dedicated exit is an edge out of the loop, so the value is live on all of
    // them and the same value is the argument on each.
    for term in terminators(func) {
        let targets = func.target_list(term);
        for at in targets.iter() {
            let call = func[at];
            if call.block != job.at {
                continue;
            }
            let args = func.append_arg(call.args, job.value);
            func.set_block_call(at, BlockCall { block: call.block, args });
        }
    }
    for &inst in &job.uses {
        let args = func[inst].args;
        func.rewrite(args, |value| if value == job.value { param } else { value });
    }
}

/// The next edit one step wants, from a forest it asks for itself.
///
/// The closure gets the forest, the graph and one loop, and answers with a block and the
/// predecessors of it to route through a new one, or nothing. One edit rather than a list, because
/// the edit invalidates the forest the closure was reading, so the caller applies it and asks
/// again. [`closed`] is written the same way against its own kind of job.
fn wanted(
    func: &mut Func,
    an: &mut Analyses,
    mut ask: impl FnMut(&Loops, &Cfg, LoopId) -> Option<(Block, Vec<Block>)>,
) -> Option<(Block, Vec<Block>)> {
    let cfg = an.cfg(func).clone();
    let loops = an.loops(func).clone();
    loops.all().find_map(|id| ask(&loops, &cfg, id))
}

/// Makes one edit and throws the analyses away, so the next question is asked of the graph as it is.
///
/// The cache hands back whatever it computed last time until somebody clears it, and every edit here
/// moves edges. Without the clear a step would ask a stale forest the same question, get the same
/// answer, and route the same edge for ever. [`closed`] does not need this because adding a block
/// parameter leaves the graph alone.
fn apply(func: &mut Func, an: &mut Analyses, from: &[Block], to: Block) {
    route(func, from, to);
    an.clear();
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
