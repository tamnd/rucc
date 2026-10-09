//! Jump threading, with and without a copy of the block threaded past.
//!
//! Design: `spec/optimizer/23-jump-threading.md`. If, on the path through block A into block B, the
//! condition B tests is already decided, then A should branch straight to the arm B was going to
//! take and skip B's test. On real C that removes more branches than anything else in the compiler,
//! because C is full of conditions that are redundant along some paths and not along others.
//!
//! It is also the pass most likely to explode, because the general form works by copying B, and a
//! copy grows the function, and the growth compounds because each thread makes new paths on which
//! further threading is possible. Section 23.4 is four separate limits on that growth and section
//! 23.6 names the subset where there is none: the case where the block being threaded past does not
//! have to be copied at all, which is pure edge redirection. That subset is [`FREE`], and [`COPY`]
//! is that plus section 23.1's copy, under the limits of section 23.4.
//!
//! # What decides a branch here, and what does not
//!
//! Arguments in this IR live on the edge rather than in the block, so a block parameter is a value
//! that arrives differently depending on which way control came. Bind a block's parameters to what
//! one edge carries and its terminator may resolve on that edge while resolving on no other, which
//! is the whole of the path sensitivity this pass has. It covers section 23.3's example directly:
//!
//! ```c
//! if (a) x = 1; else x = 2;
//! if (x == 1) ...
//! ```
//!
//! Nothing dominating the second test decides it, so the forward threader of section 23.2 cannot
//! see it and neither can `simplify-cfg`. But `x` arrives at the second test as a block parameter,
//! it is 1 along one edge and 2 along the other, and both edges resolve. Both are threaded, nothing
//! is left reaching the block, and the second branch goes.
//!
//! What is not here is section 23.3's backward search with the path-sensitive range solver. This
//! asks about one edge and not about a path of them, so a condition decided two blocks back and not
//! one is a condition this does not see. The range machinery for that exists in [`crate::range`] and
//! the search is the larger half of the document.
//!
//! # Why no block has to be copied
//!
//! Section 23.1 quotes GCC's six step surgery, whose first step is a copy of B. The copy exists so
//! that B's side effects still happen on the threaded path and so that the values B defines are
//! available to the arm the thread lands on. Where neither is needed, neither is the copy, and this
//! pass threads exactly the edges where neither is needed:
//!
//! - Every instruction in B other than its terminator has no effects, so a path that skips them
//!   skips nothing that had to happen. That is the same predicate [`crate::dce`] deletes an
//!   instruction under, which is the point: an instruction it would delete outright is one a path
//!   can walk past.
//! - Nothing outside B reads a value B defines. Those are the values the copy would have existed to
//!   compute, and both the arm's arguments and the blocks further down are asking for them.
//!
//! The second condition has to be about the whole function and not just about the arm. An argument
//! is how a value crosses into a block that B does not dominate, but a block B does dominate reads
//! what B defined with no argument at all, because dominance is the only permission a use needs.
//! Threading an edge past B takes that dominance away, and the read is then of a value that was
//! never computed on the path taken. Checking only the arm's arguments misses exactly that, which is
//! what `a_value_the_block_defines_and_something_below_it_reads_needs_the_copy` is about.
//!
//! B's parameters are covered by the same rule, since a parameter is a value B defines. Along the
//! edge being redirected they are known, so B's own reads of them are substituted rather than
//! refused, but a read from below is a read of a value that is about to stop existing. And a value
//! the arm carries that is defined outside B dominates the block it is being carried out of, so it
//! dominates the predecessor as well: it is on every path to B, the predecessor has an edge to B, so
//! it is on every path to the predecessor. That is section 23.1's "the values must still dominate",
//! and it is the same argument `spec/optimizer/21-cfg-simplification.md` section 21.4 needs for
//! forwarder removal.
//!
//! # The copy, and what it owes the values
//!
//! On the corpus the free subset threads one edge out of the 623 that decide the branch they arrive
//! at, and 619 of the rest are refused because a block below reads a value the block defines. So
//! the copy is there to make values, not to repeat effects, and [`COPY`] only copies a block the
//! free subset would already walk past: nothing in it has an effect, and nothing in it carries a
//! side table entry [`crate::header_copy`] has not been checked against. What changes is that its
//! values may be read below and carried on the arm.
//!
//! The copy is made on the one edge being threaded. It has no parameters, since along that edge
//! they are the arguments the edge carries, it holds the block's instructions with those arguments
//! put in, and it ends in a jump to the arm the edge decides. The edge is pointed at it. The arm's
//! arguments come from the copy, so a value the block worked out is the copy's version of it.
//!
//! What that leaves is every read of the block's values from somewhere else. The block no longer
//! dominates them, since the copy reaches some of them as well, so each value now has two
//! definitions and a read below needs whichever one reached it. That is SSA construction for one
//! variable with two definitions, and it is done the classical way: a block parameter goes on each
//! block in the iterated dominance frontier of the block and its copy where the value is still
//! wanted, the edges into it carry what reached the end of the block they leave, and every read is
//! of what reached the start of the block it is in. What reached a block is the nearest of the
//! block, the copy and the new parameters up the dominator tree, which is why the parameters go
//! where the frontier says: those are exactly the places where the nearest one up the tree is not
//! the only one that can arrive. Liveness is what keeps the parameters to the ones something reads,
//! and is also what makes it safe to skip the parameters of every other block in the frontier.
//!
//! # The limits
//!
//! Section 23.4 adopts GCC's four and they are all here, in `rucc_cost::heuristics`. A block of
//! more than fifteen instructions is not copied. A thread whose arm goes back to the header of a
//! loop the block is in counts each instruction twice. A copy whose edge came out of an earlier
//! copy is the next block of one path, and a path may not copy more than a hundred instructions in
//! all. And one run makes at most sixty four copies in one function, which is GCC's bound on paths
//! turned from the paths a backward search looks at into the paths that are actually copied, since
//! there is no backward search here. The last two are what stop threading from feeding on itself,
//! since every copy is a new edge into the arm and the arm may be the next block this walk threads
//! past.
//!
//! # The loop rules, which are refusals and not scores
//!
//! Section 23.5. Threading a path into a loop somewhere other than its header makes an irreducible
//! loop, and document 06.4 established that rucc does not split nodes and gives up on irreducible
//! regions instead. So the rule here is stronger than GCC's, where it is one input to a cost model:
//! a thread that would do it is refused, at every level. A predecessor that is a latch is refused
//! too, because moving a latch's edge is how the single latch property document 07.3 wants stops
//! being true. And a copy is never made in an irreducible region, since the loop forest has given up
//! on it and the two checks above would be reading an answer nobody stands behind.
//!
//! Without a copy no new cycle can appear. The new edge from A goes where the edge out of B went, so
//! a path along it is a path that was already there with B taken out of the middle. With one the
//! same is true of the path through the copy, which is B's path with B's test taken out, so loops
//! can still only be destroyed. The loop forest is rebuilt after a thread that could have broken a
//! loop, which is what keeps the next decision honest, and kept after one that provably did not.
//! An edge between two blocks of one loop whose target still gets back round to where it came
//! from is the common one, and rebuilding the forest after each of those was most of the pass.
//!
//! That is also why a thread without a copy is allowed in an irreducible region. The region already
//! has more than one way in, no loop pass looks inside it, and the loops the forest does describe
//! are still held to the two checks. This is where an interpreter written with a computed `goto`
//! lives: every block it dispatches to is in one region, and refusing there meant that an `||` in a
//! handler was worked out as a truth value and then tested, on every step.
//!
//! # Which level this runs at
//!
//! [`FREE`] runs at `-Os` and `-Oz`. Section 23.6 restricts threading at those two to the case where
//! the block is empty, on the ground that it is the only part that is free, and [`FREE`] is that
//! part generalized: a block whose instructions all have no effects and whose outgoing arguments do
//! not come from it costs the same as an empty one, which is nothing. [`COPY`] runs at `-O1` and
//! above, as GCC's `-fthread-jumps` does.
//!
//! Not to a fixed point. Threading enables threading, and section 23.7 says the answer to that is a
//! fixed number of instances rather than a loop, because threading is the pass where adversarial
//! input is easiest to construct. Section 23.5 asks for two instances at `-O2`, an early one and a
//! late one after the loop pipeline and SCCP. The early position has both of these, one on each side
//! of `phiopt`. The
//! late one is [`FREE`] again, after the `simplify-cfg` that follows the unroller, and
//! `crate::pipeline` says what it finds that the early one cannot.
//!
//! # What it counts
//!
//! Every refusal is recorded, and they are the measurement section 23.8 asks this document for.
//! Three of them count edges that decide a branch [`FREE`] cannot thread without the copy, split by
//! which part of the copy is in the way: something in the block that has to happen, a value the arm
//! carries that the block worked out, and a value the block defines that a block below it reads.
//! [`COPY`] threads the last two and records a limit instead where one stops it. One more counts
//! edges refused on loop structure, which is the price of document 06.4's position on irreducible
//! regions stated as a number rather than as an argument.
//!
//! On the 1461 programs of the corpus at `-O2`, 623 edges decide the branch they arrive at and one
//! of them is threadable without a copy. 619 are blocked on a value the block defines being read
//! below it, 4 on the arm carrying one, and none at all on the block doing something that has to
//! happen. That split is why the copy here is the copy of a block with no effects in it.

use rucc_base::Idx;
use rucc_base::hash::{Map, Set};
use rucc_cost::heuristics;
use rucc_ir::{Block, BlockCall, Builder, Def, Extra, Func, Inst, Opcode, Start, Value, ValueList};

use crate::header_copy::{clone_into, repeatable};
use crate::loops::LoopId;
use crate::simplify_cfg::{Bindings, Edges, incoming, sweep, taken};
use crate::{Analyses, Cfg, Dominators, Fuel, Loops, Pass, Preserved, Stats, uses};

/// Recorded once for each edge that was pointed past a branch it decides.
const THREADED: &str =
    "edge pointed straight at the arm of the branch it arrives at that it decides";

/// Recorded for an edge that would have been threaded if there had been fuel for it.
const NO_FUEL: &str = "edge left on a branch it decides, the pass ran out of fuel";

/// Recorded for an edge whose block does something a path through it cannot skip.
const WOULD_COPY_EFFECT: &str =
    "edge decides the branch it arrives at, but something in the block has to happen on the way";

/// Recorded for an edge whose block defines a value read below it.
const WOULD_COPY_READ_BELOW: &str =
    "edge decides the branch it arrives at, but a block below reads a value this one defines";

/// Recorded for an edge whose arm carries a value the block itself computed.
const WOULD_COPY_CARRIED: &str =
    "edge decides the branch it arrives at, but the arm carries a value the block works out";

/// Recorded for an edge that decides a branch but whose thread would spoil the loop forest.
const WOULD_BREAK_A_LOOP: &str =
    "edge decides the branch it arrives at, but threading it would give a loop a second way in";

/// Recorded once for each edge pointed at a copy of the block it arrived at.
const COPIED: &str =
    "edge pointed at a copy of the block it arrives at that goes straight to the arm it decides";

/// Recorded for an edge whose block has something in it this pass does not copy.
const ODD: &str =
    "edge decides the branch it arrives at, but the block has something in it that is not copied";

/// Recorded for an edge whose block is larger than section 23.4 lets a thread copy.
const TOO_BIG: &str =
    "edge decides the branch it arrives at, but the block is larger than a thread may copy";

/// Recorded for an edge whose copy would make the path of copies it is on too long.
const TOO_LONG: &str =
    "edge decides the branch it arrives at, but the path of copies it is on would be too long";

/// Recorded for an edge left once the function has had as many copies as one run makes.
const TOO_MANY: &str =
    "edge decides the branch it arrives at, but this function has had its 64 copies";

/// The pass, with how many instructions it may copy a block of.
#[derive(Debug)]
pub struct Thread {
    /// What a `-f` flag spells.
    name: &'static str,
    /// The most instructions a block threaded past may hold, and zero for a pass that copies none.
    /// A function rather than the number, because `--param` sets it after this was built.
    budget: fn() -> u32,
}

/// The instance `-Os` and `-Oz` run, which threads an edge only where nothing has to be copied.
pub static FREE: Thread = Thread { name: "thread", budget: || 0 };

/// The instance `-O1` and above run, which copies a block of up to section 23.4's fifteen.
pub static COPY: Thread = Thread {
    name: "thread-copy",
    budget: || rucc_cost::param!(heuristics::JUMP_THREAD_DUPLICATION_INSNS),
};

impl Pass for Thread {
    fn name(&self) -> &'static str {
        self.name
    }

    fn describe(&self) -> &'static str {
        if self.name == FREE.name {
            "an edge that already decides the branch it arrives at is pointed at the arm that \
             branch would have taken"
        } else {
            "an edge that already decides the branch it arrives at is pointed at the arm that \
             branch would have taken, through a copy of the block if the block's values are read"
        }
    }

    fn preserves(&self) -> Preserved {
        // Nothing. An edge moves, so every analysis built on the graph was built on a different
        // graph, which is the same answer `simplify-cfg` gives for the same reason.
        Preserved::NONE
    }

    fn run(&self, func: &mut Func, an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        let Some(entry) = func.entry() else { return stats };
        // The edges are kept here rather than asked for as a graph, because this pass moves them
        // as it goes and a cached graph would be about the shape the function had one thread ago.
        // A block call rather than a predecessor, since redirecting an edge wants the slot in the
        // pool and there is no finding it again from the block the edge used to arrive at.
        let mut edges: Edges = incoming(func);
        // Worked out the first time an edge gets as far as asking, since most runs over most
        // functions find no edge that does and the walk over every operand was then for nothing.
        // Nothing has moved before that first question, so the set is the one a walk here would
        // have made.
        let mut leaks: Option<Set<Block>> = None;
        let unbound = Bindings::default();
        let mut threaded = false;
        // What each copy made in this run has cost the path it is on, which is what the path limit
        // of section 23.4 adds up, and how many copies there have been.
        let mut paths: Map<Block, u32> = Map::default();
        let mut copies = 0;
        // The blocks a thread left with no way in since the graph in the cache was built, which
        // that graph still says are reached. See [`strand`].
        let mut stranded: Set<Block> = Set::default();
        'blocks: for block in func.blocks().collect::<Vec<Block>>() {
            if block == entry || func[block].params.is_empty() {
                continue;
            }
            let Some(term) = func.terminator(block) else { continue };
            if !matches!(func[term].opcode, Opcode::BrIf | Opcode::Switch) {
                continue;
            }
            // A branch that goes the same way whichever edge control arrived on is `simplify-cfg`'s
            // to fold, and folding it once is cheaper than pointing every edge into the block at the
            // same arm separately.
            if taken(func, term, &unbound).is_some() {
                continue;
            }
            // Something that has to happen stops every edge into the block, since nothing here
            // copies an effect, so it is settled once here and not once per edge below.
            let effect = !skippable(func, block);
            for (from, at) in edges.get(&block).cloned().unwrap_or_default() {
                // A block that branches to itself, where the branch resolves, is a loop that does
                // not end, and redirecting its own edge is not a description of anything a person
                // wrote. The block below refuses it as well, since the block is its own latch.
                if from == block {
                    continue;
                }
                let subst = bind(func, block, at);
                let Some(call) = taken(func, term, &subst) else { continue };
                if call.block == block {
                    continue;
                }
                if effect {
                    stats.missed(WOULD_COPY_EFFECT);
                    continue;
                }
                // The block's own values read below are what the leaky set is about, and the ones
                // the arm carries are what `carried` is about. Either needs the copy.
                let (free, why) = if leaks.get_or_insert_with(|| leaky(func)).contains(&block) {
                    (None, WOULD_COPY_READ_BELOW)
                } else {
                    (carried(func, block, call, &subst), WOULD_COPY_CARRIED)
                };
                let copies_it = free.is_none();
                let path = if free.is_some() {
                    0
                } else {
                    match self.path(func, an.loops(func), block, from, call.block, &paths, copies) {
                        Ok(path) => path,
                        Err(reason) => {
                            stats.missed(reason.unwrap_or(why));
                            continue;
                        }
                    }
                };
                if !allowed(an.loops(func), from, call.block, free.is_none()) {
                    stats.missed(WOULD_BREAK_A_LOOP);
                    continue;
                }
                if !fuel.take() {
                    // Where the pass stops rather than where it starts skipping, because a budget
                    // that has reached zero will not have anything in it at the next block either
                    // and the two refusals above are the counts worth being true.
                    stats.missed(NO_FUEL);
                    break 'blocks;
                }
                // Asked before the edge moves, since what it asks about is the graph the forest was
                // built on. A block on no cycle is asked about again once it has.
                let alone = free.is_some() && acyclic(an.loops(func), block);
                let kept = free.is_some()
                    && (alone || settled(an, func, &edges, &stranded, from, block, at, call.block));
                // The outermost loop the edge is in, when the forest is not kept as it is, which is
                // what [`rebuild`] finds it again over.
                let around =
                    (free.is_some() && !kept).then(|| outermost(an.loops(func), from, block));
                // The record has to follow the edge, so that a block further down the walk sees the
                // predecessor it now has. That is what lets one thread make the next one possible
                // within the single walk this pass is.
                if let Some(list) = edges.get_mut(&block) {
                    list.retain(|&(_, slot)| slot != at);
                }
                if let Some(args) = free {
                    let args = func.push_values(&args);
                    func.set_block_call(at, BlockCall { args, ..call });
                    edges.entry(call.block).or_default().push((from, at));
                    stats.optimized(THREADED);
                } else {
                    let loops = an.loops(func);
                    let keep = if acyclic(loops, block) {
                        Some(None)
                    } else {
                        outermost(loops, from, block).map(Some)
                    };
                    let (copy, out) = copy(func, an, block, at, call, &subst, keep);
                    edges.entry(call.block).or_default().push((copy, out));
                    edges.entry(copy).or_default().push((from, at));
                    paths.insert(copy, path);
                    copies += 1;
                    // The merges put parameters on blocks further down, and a parameter something
                    // reads from below is exactly what the leaky set is about. It went stale in the
                    // safe direction before there were copies. It does not now.
                    leaks = Some(leaky(func));
                    stats.optimized(COPIED);
                }
                // The loop forest was about the function as it was a moment ago, and the manager
                // clears the cache after the pass returns, which is too late for the next edge.
                // Unless the forest is still right, which rebuilding after each of hundreds of
                // threads in one function would otherwise spend most of the pass finding out.
                let kept =
                    kept && (!alone || strand(func, an, &edges, &mut stranded, block, entry));
                let rebuilt = !kept
                    && around
                        .flatten()
                        .is_some_and(|root| rebuild(func, an, &edges, &mut stranded, root, entry));
                // A copy left the cache with the graph it made, which nothing has moved since.
                if !kept && !rebuilt {
                    if !copies_it {
                        an.clear();
                    }
                    stranded.clear();
                }
                threaded = true;
            }
        }
        if threaded {
            // Threading every edge into a block leaves nothing arriving at it, and section 6.5
            // makes taking an unreachable block out the standing obligation of whichever pass
            // stranded it rather than something the next pass tidies up. The verifier holds every
            // pass to that, so this is not a courtesy. The graph in the cache can be from before a
            // thread that kept the forest, and the sweep wants the one there is now.
            an.clear();
            sweep(func, an, &mut stats);
        }
        stats
    }
}

impl Thread {
    /// What copying `block` onto the edge from `from` costs the path it is on, or why it may not.
    ///
    /// `None` for the reason is the instance that copies nothing, which leaves the caller to say
    /// which part of the copy was wanted. The limits are section 23.4's, in the order a block
    /// fails them: what is in it, how big it is, how long the path of copies it is on has become,
    /// and how many copies this run has already made.
    #[allow(clippy::too_many_arguments)]
    fn path(
        &self,
        func: &Func,
        loops: &Loops,
        block: Block,
        from: Block,
        into: Block,
        paths: &Map<Block, u32>,
        copies: u32,
    ) -> Result<u32, Option<&'static str>> {
        let budget = (self.budget)();
        if budget == 0 {
            return Err(None);
        }
        let body: Vec<Inst> = func.insts(block).filter(|&inst| !func.is_terminator(inst)).collect();
        if !body.iter().all(|&inst| repeatable(func, inst)) {
            return Err(Some(ODD));
        }
        let mut cost = u32::try_from(body.len()).unwrap_or(u32::MAX);
        if back_edge(loops, block, into) {
            cost = cost.saturating_mul(rucc_cost::param!(heuristics::JUMP_THREAD_BACK_EDGE_SCALE));
        }
        if cost > budget {
            return Err(Some(TOO_BIG));
        }
        let path = paths.get(&from).copied().unwrap_or(0).saturating_add(cost);
        if path > rucc_cost::param!(heuristics::JUMP_THREAD_PATH_INSNS) {
            return Err(Some(TOO_LONG));
        }
        if copies >= rucc_cost::param!(heuristics::JUMP_THREAD_PATHS) {
            return Err(Some(TOO_MANY));
        }
        Ok(path)
    }
}

/// Whether threading the edge at `at` from `from` past `block` without a copy leaves the loop
/// forest the answer it was, so the pass can go on asking it rather than building it again.
///
/// It does when the edge is on no cycle and something control reaches from outside every cycle
/// `block` is on still arrives at it by another edge. No cycle went through the edge, and none goes
/// through the one that replaces it, since that would have been a cycle through the edge before.
/// Nothing stops being reached, because whatever reached `block` this way reaches it the other
/// way, and that way cannot pass through the edge without the edge being on a cycle. A block of a
/// cycle that some path went round to reach another block of it still has a path round it for the
/// same reason. The one block that can stop dominating anything is `block`, and if it headed a
/// loop it still does: the new edge cannot enter that loop anywhere but its header, because
/// [`allowed`] refused it if it did. So the cycles are the same, the block of each that dominates
/// the rest is the same, and the same loops come out with the same headers, latches and nesting,
/// and the same blocks are irreducible. What can change is the order they are found in and which
/// edges leave a loop, and nothing here asks either.
///
/// The graph in the cache can be one from before a thread this already said yes to. What blocks
/// it reaches is still right once the ones in `stranded` are taken out, which is all this asks of
/// it. An edge that is on a cycle is [`within`]'s to answer.
#[allow(clippy::too_many_arguments)]
fn settled(
    an: &Analyses,
    func: &Func,
    edges: &Edges,
    stranded: &Set<Block>,
    from: Block,
    block: Block,
    at: Idx<BlockCall>,
    into: Block,
) -> bool {
    let loops = an.loops(func);
    if together(loops, from, block) {
        return within(loops, func, from, block, at, into);
    }
    let cfg = an.cfg(func);
    edges.get(&block).is_some_and(|list| {
        list.iter().any(|&(pred, slot)| {
            slot != at
                && cfg.reaches(pred)
                && !stranded.contains(&pred)
                && !together(loops, pred, block)
        })
    })
}

/// Whether a block is on no cycle, so that the forest says nothing of it but that.
fn acyclic(loops: &Loops, block: Block) -> bool {
    loops.innermost(block).is_none() && !loops.is_irreducible(block)
}

/// Whether the edge just threaded past `block`, a block on no cycle, left the loop forest the
/// answer it was, with every block nothing reaches any more put in `stranded`.
///
/// No cycle went through the edge, since `block` is on none, and none goes through the edge that
/// replaced it, since that would have been a cycle through `block` before. So the cycles are the
/// same, and so is every way into each of them that comes from a block still reached. What can
/// change is which blocks are reached at all: `block` once the last edge into it from a reached
/// block is gone, and then whatever nothing else reached. A block on no cycle is in no loop and no
/// irreducible region whether it is reached or not, so the forest says the same of it either way,
/// and what does change is the graph's answer to whether it is reached, which is what `stranded`
/// is kept for. A block on a cycle that stops being reached takes its loop with it, and telling
/// whether it does would be the walk the forest is, so meeting one at all is a forest built again.
///
/// The edge has moved by now, so the record of edges is the function as it is. The cached graph and
/// forest are of the function before it, which is what the walk wants to ask about.
///
/// On lz4.c at -O2 these were some of the threads that built the graph, the tree and the forest
/// again for a function whose loops had not changed. tamnd/rucc#3052.
fn strand(
    func: &Func,
    an: &Analyses,
    edges: &Edges,
    stranded: &mut Set<Block>,
    block: Block,
    entry: Block,
) -> bool {
    let (cfg, loops) = (an.cfg(func), an.loops(func));
    let mut work = vec![block];
    while let Some(at) = work.pop() {
        if at == entry || !cfg.reaches(at) || stranded.contains(&at) {
            continue;
        }
        if !acyclic(loops, at) {
            return false;
        }
        let reached = |pred: Block| cfg.reaches(pred) && !stranded.contains(&pred);
        if edges.get(&at).is_some_and(|list| list.iter().any(|&(pred, _)| reached(pred))) {
            continue;
        }
        stranded.insert(at);
        if let Some(term) = func.terminator(at) {
            work.extend(func.target_list(term).iter().map(|slot| func[slot].block));
        }
    }
    true
}

/// The outermost loop `from` is in, when `block` is in it too.
fn outermost(loops: &Loops, from: Block, block: Block) -> Option<LoopId> {
    let mut root = loops.innermost(from)?;
    while let Some(parent) = loops.parent(root) {
        root = parent;
    }
    loops.contains(root, block).then_some(root)
}

/// Finds the forest again over the outermost loop `root` after a thread that copied nothing moved
/// an edge out of one of its blocks, with every block nothing reaches any more put in `stranded`,
/// or says it could not.
///
/// The edge went to a block the loop got to anyway, through the block threaded past, so
/// [`Loops::rebuild`] has what it asks for. What it gives back is the blocks of the loop nothing
/// reaches now, and a block they went to that nothing else reaches goes with them, which is
/// [`strand`]'s walk, and a block on a cycle among those is the whole forest built again. The
/// graph in the cache stays the one from before, which is right about what is reached once the
/// blocks in `stranded` are taken out, and that is all anything here asks of it.
fn rebuild(
    func: &Func,
    an: &mut Analyses,
    edges: &Edges,
    stranded: &mut Set<Block>,
    root: LoopId,
    entry: Block,
) -> bool {
    let mut loops = an.take_loops(func);
    let mut inside = vec![false; func.counts().blocks];
    for &block in loops.blocks(root) {
        inside[block.index()] = true;
    }
    let gone = {
        let cfg = an.cfg(func);
        loops.rebuild(
            root,
            func.counts().blocks,
            |block| targets(func, block),
            |block| {
                edges
                    .get(&block)
                    .map_or_else(Vec::new, |list| list.iter().map(|&(pred, _)| pred).collect())
            },
            |block| cfg.reaches(block) && !stranded.contains(&block),
            &[],
        )
    };
    an.keep_loops(loops);
    stranded.extend(gone.iter().copied());
    // A block of the loop is one the rebuild said is reached or put in `gone`, so the walk only
    // has the ones outside it to look at, and [`strand`] would give up on the ones still reached
    // for being on a cycle.
    for &block in &gone {
        for next in targets(func, block) {
            if !inside[next.index()] && !strand(func, an, edges, stranded, next, entry) {
                return false;
            }
        }
    }
    #[cfg(debug_assertions)]
    assert_eq!(
        shape(an.loops(func)),
        shape(&fresh(func)),
        "the forest found again over one loop is not the forest of the function"
    );
    true
}

/// Where control goes from a block, as the function has it now.
fn targets(func: &Func, block: Block) -> Vec<Block> {
    func.terminator(block).map_or_else(Vec::new, |term| {
        func.target_list(term).iter().map(|slot| func[slot].block).collect()
    })
}

/// The forest of a function built from nothing, for [`rebuild`] to check itself against.
#[cfg(debug_assertions)]
fn fresh(func: &Func) -> Loops {
    let cfg = Cfg::new(func);
    Loops::new(&cfg, &Dominators::new(&cfg))
}

/// What jump threading asks of a forest, in an order that does not depend on the order the
/// loops were found in: each loop's header, blocks, latches and the header of the loop around
/// it, and the irreducible blocks.
#[cfg(debug_assertions)]
type Forest = (Vec<(Block, Vec<Block>, Vec<Block>, Option<Block>)>, Vec<Block>);

/// A forest as a [`Forest`].
#[cfg(debug_assertions)]
fn shape(loops: &Loops) -> Forest {
    let sorted = |list: &[Block]| {
        let mut list = list.to_vec();
        list.sort_unstable();
        list
    };
    let mut all: Vec<_> = loops
        .all()
        .map(|id| {
            let around = loops.parent(id).map(|parent| loops.header(parent));
            (loops.header(id), sorted(loops.blocks(id)), sorted(loops.latches(id)), around)
        })
        .collect();
    all.sort_unstable();
    (all, sorted(loops.irreducible()))
}

/// Whether threading the edge at `at` from `from` past `block` to `into`, where `from` and `block`
/// are in a loop together, leaves the loop forest the answer it was.
///
/// The forest is the cycles of the graph found level by level: the loops of the reachable blocks,
/// then the loops of each of those with its header taken out, and so on. A path along the new edge
/// is a path that was already there with `block` taken out of the middle, so no two blocks come to
/// be on a cycle that were not on one before. What can happen is that a cycle stops being one.
///
/// Take the innermost loop holding both blocks. Inside it, below its header, the two are in no
/// cycle together, so the edge between them was on no cycle there and taking it away breaks none.
/// The new edge is on none either, since [`allowed`] only lets it into a loop `from` is outside of
/// at that loop's header. In that loop and every loop around it, `into` is a block of the loop, and
/// if `into` still gets back to `block` without the edge, the loop is still one cycle: everything
/// reaches `from` as it did, since a path to `from` never needed the edge out of it, `from` reaches
/// `into` by the new edge, and `into` reaches `block` and from there what `block` reached. So the
/// same blocks are in the same loops at every level.
///
/// With no irreducible region inside that loop, every loop there and around it is entered at its
/// header and nowhere else, before and after, so the header still dominates the rest and is the
/// header the forest finds. An irreducible region anywhere else stays one, with the same blocks.
/// None is around that loop, since the forest does not look inside a region like that for loops.
/// One beside it has the same cycles, because every edge that moved is inside the loop, and no
/// block comes to dominate the rest of it, because every path now is a path there was before with
/// `block` left out of it, so whatever dominates a block now dominated it then. A latch is a
/// block with an edge to its loop's header, and the edge `from` loses cannot have been one, since
/// [`allowed`] refused a latch. The edge it gains would make it one if `into` headed a loop `from`
/// is in, so that is refused here. Every block reached is still reached, by the same argument as in
/// [`settled`], so the graph's reach is still right too.
///
/// When `into` is outside that loop, or does not get back to `block`, the same holds if the
/// loop's header still gets to `block` and `from` still gets to the header, both without the edge.
/// Then a path that used the edge has one that goes round it instead, through the header, and
/// that path stays inside the loop, so it is there in every graph the forest looks at that holds
/// the loop. The new edge is a path that was already there through `block`, so no level finds a
/// cycle it did not before, and below the loop's header the two blocks were on none together. No
/// block is stranded, since every block reached by way of the edge still is. The loop gains an
/// exit, which this pass never asks about, and the cache is cleared when it ends. On lz4.c at
/// `-O2` these were most of the threads that still built the forest again. tamnd/rucc#3052.
///
/// The walk goes over the function as it is now and not the cached graph, which can be one from
/// before an earlier thread, and it stays inside the loop, so it costs the loop rather than the
/// three analyses built again over the whole function.
fn within(
    loops: &Loops,
    func: &Func,
    from: Block,
    block: Block,
    at: Idx<BlockCall>,
    into: Block,
) -> bool {
    let around = || std::iter::successors(loops.innermost(from), |&id| loops.parent(id));
    let Some(both) = around().find(|&id| loops.contains(id, block)) else { return false };
    // Only a region inside the loop, since one anywhere else stays as it was. On lz4.c at `-O2`
    // asking about the whole function was more than half the threads that built the forest again.
    // tamnd/rucc#3052.
    if loops.irreducible().iter().any(|&odd| loops.contains(both, odd)) {
        return false;
    }
    if around().any(|id| loops.header(id) == into) {
        return false;
    }
    if loops.contains(both, into) && reaches(loops, func, both, into, block, at) {
        return true;
    }
    let header = loops.header(both);
    reaches(loops, func, both, header, block, at) && reaches(loops, func, both, from, header, at)
}

/// Whether `start` gets to `to` along the edges the function has now, staying inside the loop
/// `inside` and leaving out the edge at `at`.
fn reaches(
    loops: &Loops,
    func: &Func,
    inside: LoopId,
    start: Block,
    to: Block,
    at: Idx<BlockCall>,
) -> bool {
    if start == to {
        return true;
    }
    let mut seen: Set<Block> = Set::default();
    seen.insert(start);
    let mut work = vec![start];
    while let Some(next) = work.pop() {
        let Some(term) = func.terminator(next) else { continue };
        for slot in func.target_list(term).iter() {
            if slot == at {
                continue;
            }
            let next = func[slot].block;
            if next == to {
                return true;
            }
            if loops.contains(inside, next) && seen.insert(next) {
                work.push(next);
            }
        }
    }
    false
}

/// Whether two blocks might be on a cycle together.
///
/// Two blocks are when they are in the same outermost loop. Outside every loop the forest only
/// says a block is in some irreducible region and not which, so two such blocks might be.
fn together(loops: &Loops, a: Block, b: Block) -> bool {
    let outermost = |block: Block| {
        let mut id = loops.innermost(block)?;
        while let Some(parent) = loops.parent(id) {
            id = parent;
        }
        Some(id)
    };
    match (outermost(a), outermost(b)) {
        (None, None) => loops.is_irreducible(a) && loops.is_irreducible(b),
        (a, b) => a == b,
    }
}

/// Whether the edge from `from` to `into` goes back to the header of a loop `from` is in.
fn back_edge(loops: &Loops, from: Block, into: Block) -> bool {
    let mut id = loops.innermost(from);
    while let Some(loop_id) = id {
        if loops.header(loop_id) == into {
            return true;
        }
        id = loops.parent(loop_id);
    }
    false
}

/// Section 23.1's surgery on one edge: a copy of `block` that goes straight to `call`, the edge at
/// `at` pointed at it, and every read of `block`'s values from below given the one that reaches it.
///
/// Returns the copy and the slot of the edge out of it, which the caller's record of edges needs.
/// The cache is cleared once the edge has moved, so what it builds after is of the graph there is
/// now, and the merges only add parameters, which move no edge. The loop forest is kept when
/// `keep` says how and [`lost`] finds no cycle that stopped being reached. It is `Some(None)` for
/// a block on no cycle and `Some(Some(root))` for a block in the outermost loop `root` the edge
/// starts in.
///
/// A copy of a block on no cycle is on none either, since a way back to it from where it goes
/// would have been one back to the block. So the cycles are the ones there were, and the ways
/// into each are too, through the copy rather than the block, from every block still reached.
/// That is [`strand`]'s argument for a thread that copies nothing, and the copy is in no loop,
/// which is what the forest says of a block it has never seen. What it says of the edges out of a
/// loop is stale, and nothing here asks it that. A copy of a block in `root` stands in for it on
/// an edge out of a block of `root` and goes only where it went, which is what [`Loops::rebuild`]
/// asks of a block it is given, so only that loop's part of the forest is found again.
///
/// On lz4hc.c at `-O2` building the forest again after each of the 92 copies was a thirty-sixth
/// of the build. Nearly all of them are of blocks in a loop that is most of its function, so
/// finding the forest again over that loop costs most of what building all of it did, and the
/// build is six in a thousand fewer instructions. tamnd/rucc#3052.
fn copy(
    func: &mut Func,
    an: &mut Analyses,
    block: Block,
    at: Idx<BlockCall>,
    call: BlockCall,
    subst: &Bindings,
    keep: Option<Option<LoopId>>,
) -> (Block, Idx<BlockCall>) {
    let loops = keep.map(|_| an.take_loops(func));
    let term = func.terminator(block).expect("the block was chosen for its terminator");
    let mut map = subst.clone();
    let copy = func.create_block();
    let insts: Vec<Inst> = func.insts(block).filter(|&inst| inst != term).collect();
    for inst in insts {
        clone_into(func, copy, inst, &mut map);
    }
    let args: Vec<Value> =
        func[call.args].iter().map(|value| map.get(value).copied().unwrap_or(*value)).collect();
    let jump = Builder::new(func, copy).jump(call.block, &args);
    let out = func.target_list(jump).iter().next().expect("a jump has one edge");
    let edge = func[at];
    func.set_block_call(at, BlockCall { block: copy, args: ValueList::EMPTY, ..edge });
    an.clear();
    repair(func, an, block, copy, &map);
    #[cfg(debug_assertions)]
    {
        let cfg = Cfg::new(func);
        assert!(*an.cfg(func) == cfg, "the merges moved an edge");
        assert!(
            *an.dominators(func) == Dominators::new(&cfg),
            "the merges moved a block's dominator"
        );
    }
    if let Some(mut loops) = loops {
        let cfg = an.cfg(func);
        if let Some(root) = keep.flatten() {
            loops.rebuild(
                root,
                func.counts().blocks,
                |block| targets(func, block),
                |block| cfg.predecessors(block).to_vec(),
                |block| cfg.reaches(block),
                &[copy],
            );
        }
        if lost(func, cfg, &loops, block) {
            return (copy, out);
        }
        an.keep_loops(loops);
        #[cfg(debug_assertions)]
        assert_eq!(
            shape(an.loops(func)),
            shape(&fresh(func)),
            "the forest kept across a copy is not the forest of the function"
        );
    }
    (copy, out)
}

/// Whether a block on a cycle stopped being reached when the edge into `block` moved, which takes
/// its loop with it.
///
/// `cfg` is of the function as it is now and `loops` of the function before the edge moved. Only
/// what `block` went to can have stopped being reached, and only if `block` did.
fn lost(func: &Func, cfg: &Cfg, loops: &Loops, block: Block) -> bool {
    let mut seen: Set<Block> = Set::default();
    let mut work = vec![block];
    while let Some(at) = work.pop() {
        if cfg.reaches(at) || !seen.insert(at) {
            continue;
        }
        if !acyclic(loops, at) {
            return true;
        }
        work.extend(targets(func, at));
    }
    false
}

/// Gives every read of a value `block` defines, from anywhere but `block`, the definition that
/// reaches it now that `copy` defines the value as well.
///
/// The frontier and the dominator tree are of the graph with the edge already moved, and they are
/// shared by every value, since the merges only add parameters and a parameter moves no edge. They
/// are the cache's, so the next edge asks the same graph for its loops rather than building it again,
/// which after each of the 92 copies in lz4hc.c at `-O2` was a graph and tree of a function of
/// nineteen thousand instructions built twice.
fn repair(func: &mut Func, an: &Analyses, block: Block, copy: Block, map: &Bindings) {
    let values = read_outside(func, block, copy);
    if values.is_empty() {
        return;
    }
    let (cfg, dom, frontiers) = (an.cfg(func), an.dominators(func), an.frontiers(func));
    let mut joins: Set<Block> = Set::default();
    let mut work = vec![block, copy];
    while let Some(at) = work.pop() {
        for &join in frontiers.of(at) {
            if joins.insert(join) {
                work.push(join);
            }
        }
    }
    for (value, readers) in values {
        let copied = map.get(&value).copied().expect("the copy defines every value the block does");
        let mut reaching = Reaching {
            dom,
            block,
            copy,
            value,
            copied,
            params: Map::default(),
            memo: Map::default(),
        };
        merge(func, cfg, &joins, &readers, &mut reaching);
    }
}

/// The values `block` defines that something outside it and outside its copy reads, each with the
/// blocks that read it in the order the function has them.
///
/// One walk for all of them. Repairing one value only rewrites reads of that value and gives edges
/// what reaches them of it, so the blocks that read the next one are the ones that did here.
fn read_outside(func: &Func, block: Block, copy: Block) -> Vec<(Value, Vec<Block>)> {
    let mut seen: Map<Value, usize> = Map::default();
    let mut out: Vec<(Value, Vec<Block>)> = Vec::new();
    for other in func.blocks() {
        if other == block || other == copy {
            continue;
        }
        for inst in func.insts(other) {
            uses::operands(func, inst, |value| {
                if defined_in(func, value) == Some(block) {
                    let at = *seen.entry(value).or_insert_with(|| {
                        out.push((value, Vec::new()));
                        out.len() - 1
                    });
                    let readers = &mut out[at].1;
                    if readers.last() != Some(&other) {
                        readers.push(other);
                    }
                }
            });
        }
    }
    out
}

/// Which definition of one value reaches each block, once the merges are in.
struct Reaching<'a> {
    /// The tree the answer is read off.
    dom: &'a Dominators,
    /// The block the value was defined in first.
    block: Block,
    /// The copy of it, which defines the value as well.
    copy: Block,
    /// The value as `block` defines it.
    value: Value,
    /// The value as `copy` defines it.
    copied: Value,
    /// The parameter each merge block was given for the value.
    params: Map<Block, Value>,
    /// What reached the start of each block already asked about.
    memo: Map<Block, Value>,
}

impl Reaching<'_> {
    /// What reaches the start of a block that is neither the block nor its copy.
    ///
    /// The nearest definition up the dominator tree, where a merge block's parameter is a
    /// definition. The frontier put a parameter everywhere two could meet, so nothing between here
    /// and the nearest one can have had another arrive. A block the tree does not reach is one the
    /// program does not reach either, and it keeps the value it had.
    fn start(&mut self, of: Block) -> Value {
        let mut chain = Vec::new();
        let mut at = of;
        let found = loop {
            if let Some(&param) = self.params.get(&at) {
                break param;
            }
            if let Some(&known) = self.memo.get(&at) {
                break known;
            }
            chain.push(at);
            match self.dom.immediate_dominator(at) {
                Some(up) if up == self.block => break self.value,
                Some(up) if up == self.copy => break self.copied,
                Some(up) => at = up,
                None => break self.value,
            }
        };
        for at in chain {
            self.memo.insert(at, found);
        }
        found
    }

    /// What reaches the end of a block, which is what an edge out of it carries.
    fn end(&mut self, of: Block) -> Value {
        if of == self.block {
            self.value
        } else if of == self.copy {
            self.copied
        } else {
            self.start(of)
        }
    }
}

/// Puts the parameters one value needs where its two definitions meet, and points every read of it
/// at the definition that reaches the read.
fn merge(
    func: &mut Func,
    cfg: &Cfg,
    joins: &Set<Block>,
    readers: &[Block],
    reaching: &mut Reaching<'_>,
) {
    let (block, copy, value) = (reaching.block, reaching.copy, reaching.value);
    // Where the value is still wanted at the start of a block. A read is where it starts, and it
    // goes up through predecessors until it meets one of the two definitions.
    let mut live: Set<Block> = readers.iter().copied().collect();
    let mut work = readers.to_vec();
    while let Some(at) = work.pop() {
        for &pred in cfg.predecessors(at) {
            if pred != block && pred != copy && live.insert(pred) {
                work.push(pred);
            }
        }
    }
    let mut places: Vec<Block> = joins
        .iter()
        .copied()
        .filter(|&join| join != block && join != copy && live.contains(&join))
        .collect();
    places.sort_by_key(|join| join.index());
    let ty = func[value].ty;
    let decls: Vec<u32> = func.value_decls(value).collect();
    for &place in &places {
        let param = func.append_param(place, ty);
        for &decl in &decls {
            func.declare_value(param, decl);
        }
        reaching.params.insert(place, param);
    }
    for &reader in readers {
        let now = reaching.start(reader);
        if now == value {
            continue;
        }
        let swap = |had: Value| if had == value { now } else { had };
        for inst in func.insts(reader).collect::<Vec<Inst>>() {
            func.rewrite(func[inst].args, swap);
            for at in func.target_list(inst).iter() {
                func.rewrite(func[at].args, swap);
            }
        }
    }
    // The edges into a merge carry what reached the end of the block they leave. This comes after
    // the reads are rewritten so that what it appends is not rewritten a second time.
    let edges = if places.is_empty() { Vec::new() } else { func.blocks().collect::<Vec<Block>>() };
    for other in edges {
        let Some(term) = func.terminator(other) else { continue };
        for at in func.target_list(term).iter() {
            let call = func[at];
            if !reaching.params.contains_key(&call.block) {
                continue;
            }
            let carry = reaching.end(other);
            let args = func.append_arg(call.args, carry);
            func.set_block_call(at, BlockCall { args, ..call });
        }
    }
    // A debugger asking for the variable at a start somewhere below is asking for whichever
    // definition reached there, so a start moves to it the same way a read does.
    let starts: Vec<(Start, Value)> = func
        .value_starts(value)
        .filter_map(|start| {
            let at = func.start_place(start).map_or(start.block, |(at, _)| at);
            if at == block || at == copy {
                return None;
            }
            let now = reaching.start(at);
            (now != value).then_some((start, now))
        })
        .collect();
    let mut targets: Vec<Value> = starts.iter().map(|&(_, now)| now).collect();
    targets.dedup();
    for target in targets {
        let which: Vec<Start> =
            starts.iter().filter(|&&(_, now)| now == target).map(|&(start, _)| start).collect();
        if !which.is_empty() {
            func.move_starts(value, target, &which);
        }
    }
}

/// What this block's parameters hold along one edge into it.
fn bind(func: &Func, block: Block, at: Idx<BlockCall>) -> Bindings {
    let args = func[at].args;
    let params = func[block].params.iter().copied();
    params.zip(func[args].iter().copied()).collect()
}

/// The arguments the redirected edge carries, or `None` when one of them is only computed here.
///
/// A parameter of the block is replaced by whatever the edge being redirected was passing for it. A
/// value from anywhere else is passed on as it stands, because a value used in this block and
/// defined outside it dominates the predecessor, which is the argument the module doc makes. A value
/// defined by an instruction in this block is the case that needs section 23.1's copy, and it is the
/// answer this returns `None` for.
///
/// [`leaky`] does not cover this one. An argument on the arm is read by the block's own terminator,
/// so the value never leaves the block by that route and the block is not leaky on account of it.
/// The two checks are about the two ways a value gets out, and both are needed.
fn carried(func: &Func, block: Block, call: BlockCall, subst: &Bindings) -> Option<Vec<Value>> {
    let mut out = Vec::with_capacity(func[call.args].len());
    for &arg in &func[call.args] {
        if let Some(&bound) = subst.get(&arg) {
            out.push(bound);
            continue;
        }
        if let Def::Result { inst, .. } = func[arg].def {
            if func.block_of(inst) == Some(block) {
                return None;
            }
        }
        out.push(arg);
    }
    Some(out)
}

/// Whether a path may walk past everything this block does on the way to its terminator.
///
/// The predicate is [`Opcode::has_effects`], which is what [`crate::dce`] deletes an instruction
/// under, and the terminator is exempt because the thread is what replaces it. `is_terminator` on
/// the function rather than on the opcode, for the reason dead code elimination gives: `asm goto`
/// branches and its opcode does not say so.
///
/// A load answers that it has effects, so a block with one in it is not threaded past. That is
/// conservative rather than necessary, since skipping a load skips a value nothing on the threaded
/// path reads, and it is most of what [`WOULD_COPY_EFFECT`] turns out to be counting.
fn skippable(func: &Func, block: Block) -> bool {
    func.insts(block).all(|inst| func.is_terminator(inst) || !func[inst].opcode.has_effects())
}

/// Whether some way into `block` from a block `from` allows decides the branch it ends in for the
/// arm that goes to `arm`, looking back through blocks that do nothing but jump on.
///
/// This is the question gcc's threader answers before any of its limits, asked without making the
/// copy. gcc threads paths this pass does not, and later than the point where it works out which
/// blocks never run. Code that wants to read the function the way gcc's last passes see it, like
/// the code generator choosing which blocks are cold, asks this.
///
/// A way in from a block that only jumps, passing on parameters of its own, is looked through to
/// the ways into that block, which is how `ret = -ENOMEM` set on one path and merged with another
/// on the way down reaches the test of it. Four such blocks deep is as far as it looks.
pub fn decided_towards(
    func: &Func,
    block: Block,
    arm: Block,
    from: impl Fn(Block) -> bool,
) -> bool {
    let Some(term) = func.terminator(block) else { return false };
    if !matches!(func[term].opcode, Opcode::BrIf | Opcode::Switch)
        || func[block].params.is_empty()
        || taken(func, term, &Bindings::default()).is_some()
    {
        return false;
    }
    let edges = incoming(func);
    let Some(ways) = edges.get(&block) else { return false };
    ways.iter().any(|&(before, at)| {
        before != block
            && from(before)
            && goes(func, &edges, term, bind(func, block, at), (before, at), arm, 4)
    })
}

/// Whether `term` goes to `arm` under `subst` once control has come along `way`, or as some way
/// into the block that edge leaves carries on.
///
/// Two things can decide it. One is a value `subst` binds, as above. The other is the branch at the
/// end of the block the edge leaves, which says its condition was true or false, so a test of the
/// same condition is decided. That is `for (i = 0; i < n; i++) if (...) break; if (i < n) ...`,
/// where the way out of the loop that did not break already knows `i < n` is false.
fn goes(
    func: &Func,
    edges: &Edges,
    term: Inst,
    subst: Bindings,
    way: (Block, Idx<BlockCall>),
    arm: Block,
    depth: u32,
) -> bool {
    if let Some(call) = taken(func, term, &subst) {
        return call.block == arm;
    }
    let (from, at) = way;
    if let Some(call) = implied(func, term, &subst, from, at) {
        return call.block == arm;
    }
    let only_jumps = func.insts(from).all(|inst| func[inst].opcode == Opcode::Jump);
    if depth == 0 || !only_jumps {
        return false;
    }
    let Some(ways) = edges.get(&from) else { return false };
    ways.iter().any(|&(before, at)| {
        let inner = bind(func, from, at);
        let next = subst.iter().map(|(&param, &v)| (param, inner.get(&v).copied().unwrap_or(v)));
        before != from && goes(func, edges, term, next.collect(), (before, at), arm, depth - 1)
    })
}

/// Where `term`, a `br_if`, goes when control came along edge `at` out of `from` and `from` ends
/// in a `br_if` on the same condition, which that edge says the value of.
fn implied(
    func: &Func,
    term: Inst,
    subst: &Bindings,
    from: Block,
    at: Idx<BlockCall>,
) -> Option<BlockCall> {
    let before = func.terminator(from)?;
    if func[term].opcode != Opcode::BrIf || func[before].opcode != Opcode::BrIf {
        return None;
    }
    let Extra::Targets(out) = func[before].extra else { return None };
    let [yes, no] = func[out] else { return None };
    if yes.block == no.block {
        return None;
    }
    let held = func.target_list(before).iter().position(|it| it == at)? == 0;
    let known = *func[func[before].args].first()?;
    let asked = *func[func[term].args].first()?;
    let asked = subst.get(&asked).copied().unwrap_or(asked);
    if !same_test(func, asked, known, subst) {
        return None;
    }
    let Extra::Targets(targets) = func[term].extra else { return None };
    func[targets].get(usize::from(!held)).copied()
}

/// Whether two conditions are one, either the same value or the same comparison of the same two
/// values once `subst` has said what the first one's operands stand for.
fn same_test(func: &Func, asked: Value, known: Value, subst: &Bindings) -> bool {
    if asked == known {
        return true;
    }
    let (Def::Result { inst: a, .. }, Def::Result { inst: k, .. }) =
        (func[asked].def, func[known].def)
    else {
        return false;
    };
    if func[a].opcode != Opcode::ICmp || func[k].opcode != Opcode::ICmp {
        return false;
    }
    if func[a].extra != func[k].extra {
        return false;
    }
    let ours = func[func[a].args].iter().map(|v| subst.get(v).copied().unwrap_or(*v));
    ours.eq(func[func[k].args].iter().copied())
}

/// Every block that defines a value read from somewhere other than itself.
///
/// The other half of what section 23.1's copy is for, and the half an argument list does not show.
/// A block the candidate dominates reads what the candidate defined with nothing carrying it across,
/// because dominance is the only permission a use needs in this IR. Point an edge past the candidate
/// and that dominance is gone, so the read below is of a value nothing on the new path computed.
///
/// One walk for the whole function rather than one per candidate block. A thread with no copy only
/// makes it stale in the safe direction, since it never adds a read of a value defined in the block
/// it went past: [`carried`] refuses the edge when an arm carries one, and everything else it passes
/// on was defined further up. A copy does add reads, of the parameters its merges put further down,
/// so the pass walks again after each one.
fn leaky(func: &Func) -> Set<Block> {
    let mut out = Set::default();
    for block in func.blocks() {
        for inst in func.insts(block) {
            uses::operands(func, inst, |value| {
                if let Some(home) = defined_in(func, value) {
                    if home != block {
                        out.insert(home);
                    }
                }
            });
        }
    }
    out
}

/// The block a value comes from, whether it is a parameter of one or a result computed in one.
fn defined_in(func: &Func, value: Value) -> Option<Block> {
    match func[value].def {
        Def::Result { inst, .. } => func.block_of(inst),
        Def::Param { block, .. } => Some(block),
    }
}

/// Whether the loop structure survives pointing this edge at that block, with or without a copy.
///
/// Section 23.5, and every answer of `false` is a refusal rather than a cost. Entering a loop
/// anywhere but at its header makes the loop irreducible, and moving a latch's edge is how the
/// single latch property stops holding.
///
/// A block the forest has already given up on is refused only for a copy. A thread that copies
/// nothing takes a block out of a path that was already there, so it makes no cycle, and a region
/// that is irreducible already has more than one way in and is left alone by every loop pass. What
/// is left to break is the loops the forest does stand behind, and the two checks below are about
/// exactly those. A copy is a new block in a region nobody has described, which is a question
/// this does not answer. The case that matters is a computed `goto`: every block an interpreter
/// dispatches to is in one irreducible region, so without this no branch in any of its handlers
/// was ever threaded.
fn allowed(loops: &Loops, from: Block, into: Block, copies: bool) -> bool {
    if copies && (loops.is_irreducible(from) || loops.is_irreducible(into)) {
        return false;
    }
    if loops.all().any(|id| loops.latches(id).contains(&from)) {
        return false;
    }
    let mut id = loops.innermost(into);
    while let Some(loop_id) = id {
        // Only a loop the predecessor is outside of, because an edge that stays within a loop is
        // not a way into it.
        if !loops.contains(loop_id, from) && loops.header(loop_id) != into {
            return false;
        }
        id = loops.parent(loop_id);
    }
    true
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{
        Block, BlockCall, Builder, Flags, Func, IntPred, MemInfo, MemOrder, Opcode, Restrict,
        Signature, Type, Value, ValueList,
    };

    use rucc_base::hash::{Map, Set};
    use rucc_ir::{Module, verify_func};
    use rucc_target::{TargetInfo, Triple};

    use super::{COPY, FREE, Thread};
    use crate::stats::Kind;
    use crate::{Fuel, Pass, Stats};

    /// Runs the instance that copies nothing, with as much fuel as it wants.
    fn thread(func: &mut Func) -> Stats {
        FREE.run(func, &mut crate::machine::fixtures::analyses(), &mut Fuel::unlimited())
    }

    /// Runs the instance that copies, and checks what it left is still a function.
    fn copying(func: &mut Func) -> Stats {
        copying_with(&COPY, func)
    }

    fn copying_with(pass: &Thread, func: &mut Func) -> Stats {
        let stats =
            pass.run(func, &mut crate::machine::fixtures::analyses(), &mut Fuel::unlimited());
        let mut names = Interner::new();
        let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().unwrap());
        let module = Module::new(names.intern("t.c"), &target);
        if let Err(errors) = verify_func(&module, func, &names) {
            panic!("{errors:#?}");
        }
        stats
    }

    /// The blocks the function still has, by number.
    fn blocks(func: &Func) -> Vec<usize> {
        func.blocks().map(Block::index).collect()
    }

    /// Where a block's terminator goes, as block numbers.
    fn goes_to(func: &Func, block: usize) -> Vec<usize> {
        let block = Block::from_usize(block);
        let term = func.terminator(block).expect("every block here has one");
        func.successors(term).map(|call| call.block.index()).collect()
    }

    /// What a block's terminator carries on its first edge.
    fn carries(func: &Func, block: usize) -> Vec<Value> {
        let block = Block::from_usize(block);
        let term = func.terminator(block).expect("every block here has one");
        let call = func.successors(term).next().expect("a terminator here has an edge");
        func[call.args].to_vec()
    }

    /// Section 23.3's example: two arms set one value to two constants and a join tests it.
    ///
    /// Block 0 is the entry, blocks 1 and 2 are the arms carrying `left` and `right`, block 3 is
    /// the join and takes the value as a parameter, and blocks 4 and 5 are the two ways the test
    /// can come out. The value the arms carry comes back, so a test can say which one was
    /// substituted into what.
    fn diamond(left: i128, right: i128) -> (Func, [Value; 2]) {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let arms = [func.create_block(), func.create_block()];
        let join = func.create_block();
        let param = func.append_param(join, Type::int(32));
        let yes = func.create_block();
        let no = func.create_block();

        let mut build = Builder::new(&mut func, entry);
        let cond = build.iconst(Type::int(1), 1);
        build.br_if(cond, arms[0], &[], arms[1], &[]);
        let mut sent = Vec::new();
        for (arm, value) in arms.iter().zip([left, right]) {
            let mut build = Builder::new(&mut func, *arm);
            let it = build.iconst(Type::int(32), value);
            sent.push(it);
            build.jump(join, &[it]);
        }
        let mut build = Builder::new(&mut func, join);
        let one = build.iconst(Type::int(32), 1);
        let test = build.icmp(IntPred::Eq, param, one);
        build.br_if(test, yes, &[], no, &[]);
        for block in [yes, no] {
            let mut build = Builder::new(&mut func, block);
            build.ret(&[]);
        }
        (func, [sent[0], sent[1]])
    }

    /// Points the one edge out of a block at another block, carrying nothing, the way a thread
    /// would have.
    fn point(func: &mut Func, from: usize, to: usize) {
        let term = func.terminator(Block::from_usize(from)).expect("every block here has one");
        let at = func.target_list(term).iter().next().expect("a jump has one edge");
        let call = func[at];
        let block = Block::from_usize(to);
        func.set_block_call(at, BlockCall { block, args: ValueList::EMPTY, ..call });
    }

    /// Whether the forest built before the arms of `func` were pointed at `to` still holds once
    /// they are, and what that left with no way in.
    fn stranding(mut func: Func, to: [usize; 2]) -> (bool, Vec<usize>) {
        let an = crate::machine::fixtures::analyses();
        an.loops(&func);
        point(&mut func, 1, to[0]);
        point(&mut func, 2, to[1]);
        let edges = crate::simplify_cfg::incoming(&func);
        let mut stranded = Set::default();
        let (join, entry) = (Block::from_usize(3), Block::from_usize(0));
        let kept = super::strand(&func, &an, &edges, &mut stranded, join, entry);
        let mut left: Vec<usize> = stranded.into_iter().map(Block::index).collect();
        left.sort_unstable();
        (kept, left)
    }

    #[test]
    fn a_join_both_arms_were_threaded_past_is_stranded_and_the_forest_kept() {
        let (func, _) = diamond(1, 2);
        // Each arm still reaches the side it was pointed at, so only the join goes.
        assert_eq!(stranding(func, [4, 5]), (true, vec![3]));
        // Both arms to one side, and the other side goes with the join.
        let (func, _) = diamond(1, 2);
        assert_eq!(stranding(func, [4, 4]), (true, vec![3, 5]));
    }

    #[test]
    fn a_loop_only_a_stranded_join_reached_is_a_forest_built_again() {
        let (mut func, _) = diamond(1, 2);
        // Block 5 goes round itself until it leaves for block 4, so pointing both arms at block
        // 4 takes away the only way into a loop.
        let no = Block::from_usize(5);
        let term = func.terminator(no).expect("block 5 returns");
        func.remove_inst(term);
        let mut build = Builder::new(&mut func, no);
        let again = build.iconst(Type::int(1), 1);
        build.br_if(again, no, &[], Block::from_usize(4), &[]);
        assert!(!stranding(func, [4, 4]).0);
    }

    #[test]
    fn both_edges_of_a_join_that_decides_its_test_are_threaded() {
        let (mut func, _) = diamond(1, 2);
        let stats = thread(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::THREADED), 2);
        // The arm carrying 1 goes to the true side and the arm carrying 2 to the false side, so
        // the block that tested it has nothing left arriving at it.
        assert_eq!(goes_to(&func, 1), vec![4]);
        assert_eq!(goes_to(&func, 2), vec![5]);
        // And nothing arrives at the block that tested it, so it goes with the same sweep
        // `simplify-cfg` uses. The verifier holds a pass to that rather than letting the next one
        // tidy up after it.
        assert_eq!(blocks(&func), vec![0, 1, 2, 4, 5]);
        assert_eq!(stats.count(Kind::Optimized, crate::simplify_cfg::REMOVED), 1);
    }

    /// `if (!begin(p)) return -EFAULT;` with `begin` inlined: the arms carry the one bit `begin`
    /// returns and the join branches on its `xor` with one, which is what `!` on a `_Bool` is.
    /// Each edge decides the branch through the `xor`, and the kernel's `user_access_begin` needs
    /// both threaded so that the edge where the check failed does not join the one after `stac`.
    #[test]
    fn a_join_a_way_into_which_decides_its_test_says_which_arm() {
        let (func, _) = diamond(1, 2);
        let join = Block::from_usize(3);
        assert!(super::decided_towards(&func, join, Block::from_usize(4), |_| true));
        assert!(super::decided_towards(&func, join, Block::from_usize(5), |_| true));
        // Only the arm carrying 2 goes to block 5, so leaving it out leaves nothing that does.
        let not_two = |from: Block| from.index() != 2;
        assert!(!super::decided_towards(&func, join, Block::from_usize(5), not_two));
        // The entry tests a constant it was not given, so there is nothing to look at it through.
        assert!(!super::decided_towards(&func, Block::from_usize(0), join, |_| true));
    }

    /// `ret` set to a constant on two paths, merged in a block that only jumps on, and then
    /// tested. Both arms are decided once the merge is looked through.
    #[test]
    fn a_way_in_through_a_block_that_only_jumps_on_is_looked_through() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let arms = [func.create_block(), func.create_block()];
        let merge = func.create_block();
        let carried = func.append_param(merge, Type::int(32));
        let join = func.create_block();
        let param = func.append_param(join, Type::int(32));
        let yes = func.create_block();
        let no = func.create_block();

        let mut build = Builder::new(&mut func, entry);
        let cond = build.iconst(Type::int(1), 1);
        build.br_if(cond, arms[0], &[], arms[1], &[]);
        for (arm, value) in arms.iter().zip([0, -12]) {
            let mut build = Builder::new(&mut func, *arm);
            let it = build.iconst(Type::int(32), value);
            build.jump(merge, &[it]);
        }
        let mut build = Builder::new(&mut func, merge);
        build.jump(join, &[carried]);
        let mut build = Builder::new(&mut func, join);
        let zero = build.iconst(Type::int(32), 0);
        let test = build.icmp(IntPred::Ne, param, zero);
        build.br_if(test, yes, &[], no, &[]);
        for block in [yes, no] {
            let mut build = Builder::new(&mut func, block);
            build.ret(&[]);
        }

        assert!(super::decided_towards(&func, join, yes, |_| true));
        assert!(super::decided_towards(&func, join, no, |_| true));
    }

    /// `if (i < n)` after a loop, reached from the loop's own exit test `i < n` through a block
    /// that only jumps on. The exit was taken when the test was false, so the join goes to its
    /// false arm. A way in from a branch on something else decides nothing.
    #[test]
    fn a_way_out_of_a_branch_on_the_same_test_says_which_arm() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let i = func.append_param(entry, Type::int(32));
        let n = func.append_param(entry, Type::int(32));
        let other = func.append_param(entry, Type::int(1));
        let exit = func.create_block();
        let side = func.create_block();
        let join = func.create_block();
        let param = func.append_param(join, Type::int(32));
        let yes = func.create_block();
        let no = func.create_block();
        let looped = func.create_block();

        let mut build = Builder::new(&mut func, entry);
        let test = build.icmp(IntPred::Slt, i, n);
        build.br_if(test, looped, &[], exit, &[]);
        let mut build = Builder::new(&mut func, looped);
        build.br_if(other, side, &[], join, &[i]);
        let mut build = Builder::new(&mut func, side);
        build.ret(&[]);
        let mut build = Builder::new(&mut func, exit);
        build.jump(join, &[i]);
        let mut build = Builder::new(&mut func, join);
        let again = build.icmp(IntPred::Slt, param, n);
        build.br_if(again, yes, &[], no, &[]);
        for block in [yes, no] {
            let mut build = Builder::new(&mut func, block);
            build.ret(&[]);
        }

        assert!(super::decided_towards(&func, join, no, |block| block == exit));
        assert!(!super::decided_towards(&func, join, yes, |block| block == exit));
        assert!(!super::decided_towards(&func, join, yes, |block| block == looped));
        assert!(!super::decided_towards(&func, join, no, |block| block == looped));
    }

    #[test]
    fn a_join_that_tests_the_not_of_a_bit_it_was_given_is_threaded() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let arms = [func.create_block(), func.create_block()];
        let join = func.create_block();
        let param = func.append_param(join, Type::int(1));
        let yes = func.create_block();
        let no = func.create_block();

        let mut build = Builder::new(&mut func, entry);
        let cond = build.iconst(Type::int(1), 1);
        build.br_if(cond, arms[0], &[], arms[1], &[]);
        for (arm, value) in arms.iter().zip([0, -1]) {
            let mut build = Builder::new(&mut func, *arm);
            let it = build.iconst(Type::int(1), value);
            build.jump(join, &[it]);
        }
        let mut build = Builder::new(&mut func, join);
        let ones = build.iconst(Type::int(1), -1);
        let not = build.binary(Opcode::Xor, param, ones, Flags::default());
        build.br_if(not, yes, &[], no, &[]);
        for block in [yes, no] {
            let mut build = Builder::new(&mut func, block);
            build.ret(&[]);
        }

        let stats = thread(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::THREADED), 2);
        // The arm carrying 0 goes to the side the `!` is true on, and the one carrying 1 to the
        // other side.
        assert_eq!(goes_to(&func, 1), vec![4]);
        assert_eq!(goes_to(&func, 2), vec![5]);
        assert_eq!(blocks(&func), vec![0, 1, 2, 4, 5]);
    }

    #[test]
    fn an_edge_that_does_not_decide_the_test_is_left_alone() {
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::int(32)]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        // A parameter of the function rather than a constant, so binding it to the block's
        // parameter says nothing about the test.
        let outside = func.append_param(entry, Type::int(32));
        let join = func.create_block();
        let param = func.append_param(join, Type::int(32));
        let yes = func.create_block();
        let no = func.create_block();

        let mut build = Builder::new(&mut func, entry);
        build.jump(join, &[outside]);
        let mut build = Builder::new(&mut func, join);
        let one = build.iconst(Type::int(32), 1);
        let test = build.icmp(IntPred::Eq, param, one);
        build.br_if(test, yes, &[], no, &[]);
        for block in [yes, no] {
            let mut build = Builder::new(&mut func, block);
            build.ret(&[]);
        }

        assert!(!super::decided_towards(&func, join, yes, |_| true));
        let stats = thread(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::THREADED), 0);
        assert_eq!(goes_to(&func, 0), vec![1]);
    }

    #[test]
    fn a_branch_decided_whichever_way_control_arrived_is_left_to_simplify_cfg() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let arms = [func.create_block(), func.create_block()];
        let join = func.create_block();
        func.append_param(join, Type::int(32));
        let yes = func.create_block();
        let no = func.create_block();

        let mut build = Builder::new(&mut func, entry);
        let cond = build.iconst(Type::int(1), 1);
        build.br_if(cond, arms[0], &[], arms[1], &[]);
        for (arm, value) in arms.iter().zip([1, 2]) {
            let mut build = Builder::new(&mut func, *arm);
            let it = build.iconst(Type::int(32), value);
            build.jump(join, &[it]);
        }
        let mut build = Builder::new(&mut func, join);
        // The test reads nothing the edges carry, so it comes out the same way whichever edge
        // control arrived on and it is `simplify-cfg`'s to fold once rather than this pass's to
        // point every edge at separately.
        let known = build.iconst(Type::int(1), 1);
        build.br_if(known, yes, &[], no, &[]);
        for block in [yes, no] {
            let mut build = Builder::new(&mut func, block);
            build.ret(&[]);
        }

        let stats = thread(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::THREADED), 0);
        assert_eq!(goes_to(&func, 1), vec![3]);
        assert_eq!(goes_to(&func, 2), vec![3]);
    }

    #[test]
    fn a_block_with_something_that_happens_in_it_needs_the_copy() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let arms = [func.create_block(), func.create_block()];
        let join = func.create_block();
        let param = func.append_param(join, Type::int(32));
        let yes = func.create_block();
        let no = func.create_block();

        let mut build = Builder::new(&mut func, entry);
        let cond = build.iconst(Type::int(1), 1);
        build.br_if(cond, arms[0], &[], arms[1], &[]);
        for (arm, value) in arms.iter().zip([1, 2]) {
            let mut build = Builder::new(&mut func, *arm);
            let it = build.iconst(Type::int(32), value);
            build.jump(join, &[it]);
        }
        let mut build = Builder::new(&mut func, join);
        // A store above the test. It has to happen on every path that reached the block, so no
        // path may walk past it, and threading either edge would be a path that did.
        let what = build.iconst(Type::int(32), 7);
        let address = build.iconst(Type::int(64), 16);
        let address = build.unary(Opcode::IntToPtr, address, Type::PTR);
        let info = MemInfo {
            size: 4,
            align: 4,
            order: MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        };
        build.store(what, address, info, Flags::NONE);
        let one = build.iconst(Type::int(32), 1);
        let test = build.icmp(IntPred::Eq, param, one);
        build.br_if(test, yes, &[], no, &[]);
        for block in [yes, no] {
            let mut build = Builder::new(&mut func, block);
            build.ret(&[]);
        }

        let stats = thread(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::THREADED), 0);
        assert_eq!(stats.count(Kind::Missed, super::WOULD_COPY_EFFECT), 2);
    }

    /// The shape a clamp compiles to, which is where the corpus caught this being wrong.
    ///
    /// `raw < 15 ? 15 : raw` puts the value under test in a block parameter and then hands the same
    /// parameter to the arm that did not change it. The arm reads it with nothing carrying it there,
    /// because the join dominates the arm, and an edge threaded past the join is a path on which the
    /// read has no value behind it. It compiled to a program that printed the wrong number.
    fn clamp() -> Func {
        clamp_with(|_, _| {})
    }

    /// The clamp with more in the join ahead of its test, put there by `extra` from the parameter.
    fn clamp_with(extra: impl FnOnce(&mut Builder<'_>, Value)) -> Func {
        let mut names = Interner::new();
        let signature = Signature::new().with_returns(&[Type::int(32)]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let arms = [func.create_block(), func.create_block()];
        let join = func.create_block();
        let param = func.append_param(join, Type::int(32));
        let yes = func.create_block();
        let no = func.create_block();

        let mut build = Builder::new(&mut func, entry);
        let cond = build.iconst(Type::int(1), 1);
        build.br_if(cond, arms[0], &[], arms[1], &[]);
        for (arm, value) in arms.iter().zip([1, 2]) {
            let mut build = Builder::new(&mut func, *arm);
            let it = build.iconst(Type::int(32), value);
            build.jump(join, &[it]);
        }
        let mut build = Builder::new(&mut func, join);
        extra(&mut build, param);
        let one = build.iconst(Type::int(32), 1);
        let test = build.icmp(IntPred::Eq, param, one);
        build.br_if(test, yes, &[], no, &[]);
        let mut build = Builder::new(&mut func, yes);
        let floor = build.iconst(Type::int(32), 15);
        build.ret(&[floor]);
        // The read from below. Nothing on the edge carries the parameter here, and nothing has to,
        // since every path to this block goes through the block that defines it.
        let mut build = Builder::new(&mut func, no);
        build.ret(&[param]);
        func
    }

    #[test]
    fn a_value_the_block_defines_and_something_below_it_reads_needs_the_copy() {
        let mut func = clamp();
        let stats = thread(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::THREADED), 0);
        assert_eq!(stats.count(Kind::Missed, super::WOULD_COPY_READ_BELOW), 2);
        assert_eq!(goes_to(&func, 1), vec![3]);
        assert_eq!(goes_to(&func, 2), vec![3]);
    }

    /// What the copy is for: both edges of the clamp are threaded, and the read below gets the
    /// value that reached it along whichever one it came in by.
    #[test]
    fn a_copy_threads_the_clamp_and_the_read_below_gets_a_merge() {
        let mut func = clamp();
        let stats = copying(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::COPIED), 2, "{stats:?}");
        assert_eq!(stats.count(Kind::Missed, super::WOULD_COPY_READ_BELOW), 0);
        // The join is gone, since both edges into it went to copies, and each arm goes to a copy
        // that goes straight to the arm the value it carries decides.
        assert!(!blocks(&func).contains(&3), "{:?}", blocks(&func));
        let first = goes_to(&func, 1)[0];
        let second = goes_to(&func, 2)[0];
        assert_eq!(goes_to(&func, first), vec![4]);
        assert_eq!(goes_to(&func, second), vec![5]);
        // What the false arm returns is what the arm that took it carried, which is 2. The merge
        // gave the false arm a parameter while the join still had an edge to it, and once the
        // join was gone the parameter had one edge left and was swept down to the constant.
        let returned = Block::from_usize(5);
        let term = func.terminator(returned).expect("a return");
        let read = func[func[term].args][0];
        assert_eq!(super::defined_in(&func, read), Some(Block::from_usize(2)));
    }

    #[test]
    fn a_copy_is_not_made_of_a_block_with_something_that_happens_in_it() {
        let mut func = clamp_with(|build, _| {
            let what = build.iconst(Type::int(32), 7);
            let address = build.iconst(Type::int(64), 16);
            let address = build.unary(Opcode::IntToPtr, address, Type::PTR);
            let info = MemInfo {
                size: 4,
                align: 4,
                order: MemOrder::NotAtomic,
                tbaa: None,
                owns: 0,
                restrict: Restrict::NONE,
            };
            build.store(what, address, info, Flags::NONE);
        });
        let stats = copying(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::COPIED), 0);
        assert_eq!(stats.count(Kind::Missed, super::WOULD_COPY_EFFECT), 2);
    }

    /// A block of more than fifteen instructions is not copied, and the same block with its sum a
    /// little shorter is.
    #[test]
    fn a_block_larger_than_the_budget_is_not_copied() {
        for (adds, copied) in [(13, 2), (14, 0)] {
            let mut func = clamp_with(|build, param| {
                let mut sum = param;
                for _ in 0..adds {
                    sum = build.binary(Opcode::Add, sum, param, Flags::NONE);
                }
            });
            // The one, the test and the adds, which is fifteen and then sixteen.
            let stats = copying(&mut func);
            assert_eq!(stats.count(Kind::Optimized, super::COPIED), copied, "{adds}: {stats:?}");
            if copied == 0 {
                assert_eq!(stats.count(Kind::Missed, super::TOO_BIG), 2);
            }
        }
    }

    #[test]
    fn a_path_of_copies_may_not_grow_past_its_limit() {
        let func = clamp();
        let an = crate::machine::fixtures::analyses();
        let join = Block::from_usize(3);
        let from = Block::from_usize(1);
        let into = Block::from_usize(5);
        let loops = an.loops(&func);
        let mut paths = Map::default();
        assert_eq!(COPY.path(&func, loops, join, from, into, &paths, 0), Ok(2));
        paths.insert(from, 99);
        assert_eq!(
            COPY.path(&func, loops, join, from, into, &paths, 0),
            Err(Some(super::TOO_LONG))
        );
        paths.insert(from, 98);
        assert_eq!(COPY.path(&func, loops, join, from, into, &paths, 0), Ok(100));
        assert_eq!(
            COPY.path(&func, loops, join, from, into, &paths, 64),
            Err(Some(super::TOO_MANY))
        );
        assert_eq!(FREE.path(&func, loops, join, from, into, &paths, 0), Err(None));
    }

    /// Sixty four copies and no more, however many edges are left that want one.
    #[test]
    fn one_run_makes_at_most_sixty_four_copies() {
        let mut names = Interner::new();
        let signature =
            Signature::new().with_params(&[Type::int(32)]).with_returns(&[Type::int(32)]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let pick = func.append_param(entry, Type::int(32));
        let arms: Vec<Block> = (0..70).map(|_| func.create_block()).collect();
        let join = func.create_block();
        let param = func.append_param(join, Type::int(32));
        let yes = func.create_block();
        let no = func.create_block();
        let cases: Vec<(i128, Block)> = (0..).zip(arms.iter().copied()).collect();
        let (&default, _) = arms.split_last().expect("arms");
        Builder::new(&mut func, entry).switch(pick, default, &cases[..69]);
        for (&arm, value) in arms.iter().zip(0..) {
            let mut build = Builder::new(&mut func, arm);
            let it = build.iconst(Type::int(32), value);
            build.jump(join, &[it]);
        }
        let mut build = Builder::new(&mut func, join);
        let one = build.iconst(Type::int(32), 1);
        let test = build.icmp(IntPred::Eq, param, one);
        // A sum the false arm reads, so that every edge still wants a copy after the first ones:
        // the merge they leave behind is carried a value the join works out.
        let sum = build.binary(Opcode::Add, param, one, Flags::NONE);
        build.br_if(test, yes, &[], no, &[]);
        let mut build = Builder::new(&mut func, yes);
        let zero = build.iconst(Type::int(32), 0);
        build.ret(&[zero]);
        let mut build = Builder::new(&mut func, no);
        build.ret(&[sum]);

        let stats = copying(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::COPIED), 64, "{stats:?}");
        // The edge carrying 1 decides the true arm, which reads nothing of the join's. Once the
        // first copy put a merge on the false arm, nothing below read the join's values at all,
        // so that edge was threaded with no copy. The other five were left where they were.
        assert_eq!(stats.count(Kind::Optimized, super::THREADED), 1, "{stats:?}");
        assert_eq!(stats.count(Kind::Missed, super::TOO_MANY), 5, "{stats:?}");
    }

    #[test]
    fn a_thread_back_to_a_loop_header_counts_each_instruction_twice() {
        let func = loop_with_a_parameter(2);
        let an = crate::machine::fixtures::analyses();
        let loops = an.loops(&func);
        let header = Block::from_usize(1);
        let body = Block::from_usize(2);
        assert!(super::back_edge(loops, body, header));
        assert!(!super::back_edge(loops, header, Block::from_usize(3)));
    }

    #[test]
    fn an_arm_carrying_a_value_the_block_worked_out_is_threaded_through_a_copy() {
        let mut names = Interner::new();
        let signature = Signature::new().with_returns(&[Type::int(32)]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let arms = [func.create_block(), func.create_block()];
        let join = func.create_block();
        let param = func.append_param(join, Type::int(32));
        let yes = func.create_block();
        let got = func.append_param(yes, Type::int(32));
        let no = func.create_block();

        let mut build = Builder::new(&mut func, entry);
        let cond = build.iconst(Type::int(1), 1);
        build.br_if(cond, arms[0], &[], arms[1], &[]);
        for (arm, value) in arms.iter().zip([1, 2]) {
            let mut build = Builder::new(&mut func, *arm);
            let it = build.iconst(Type::int(32), value);
            build.jump(join, &[it]);
        }
        let mut build = Builder::new(&mut func, join);
        let one = build.iconst(Type::int(32), 1);
        let test = build.icmp(IntPred::Eq, param, one);
        let sum = build.binary(Opcode::Add, param, one, Flags::NONE);
        build.br_if(test, yes, &[sum], no, &[]);
        let mut build = Builder::new(&mut func, yes);
        build.ret(&[got]);
        let mut build = Builder::new(&mut func, no);
        let zero = build.iconst(Type::int(32), 0);
        build.ret(&[zero]);

        let stats = copying(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::THREADED), 1);
        assert_eq!(stats.count(Kind::Optimized, super::COPIED), 1);
        let copy = goes_to(&func, 1)[0];
        assert_eq!(goes_to(&func, copy), vec![4]);
        assert_eq!(carries(&func, copy).len(), 1);
    }

    #[test]
    fn an_arm_carrying_a_value_the_block_worked_out_needs_the_copy() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let arms = [func.create_block(), func.create_block()];
        let join = func.create_block();
        let param = func.append_param(join, Type::int(32));
        let yes = func.create_block();
        func.append_param(yes, Type::int(32));
        let no = func.create_block();

        let mut build = Builder::new(&mut func, entry);
        let cond = build.iconst(Type::int(1), 1);
        build.br_if(cond, arms[0], &[], arms[1], &[]);
        for (arm, value) in arms.iter().zip([1, 2]) {
            let mut build = Builder::new(&mut func, *arm);
            let it = build.iconst(Type::int(32), value);
            build.jump(join, &[it]);
        }
        let mut build = Builder::new(&mut func, join);
        let one = build.iconst(Type::int(32), 1);
        let test = build.icmp(IntPred::Eq, param, one);
        // The true arm carries a sum this block worked out, which is exactly the value section
        // 23.1's copy of the block exists to make available on the threaded path.
        let sum = build.binary(Opcode::Add, param, one, Flags::NONE);
        build.br_if(test, yes, &[sum], no, &[]);
        for block in [yes, no] {
            let mut build = Builder::new(&mut func, block);
            build.ret(&[]);
        }

        let stats = thread(&mut func);
        // The edge carrying 2 takes the false arm, which carries nothing, so it threads. The one
        // carrying 1 takes the arm with the sum on it and is the one that would need the copy.
        assert_eq!(stats.count(Kind::Optimized, super::THREADED), 1);
        assert_eq!(stats.count(Kind::Missed, super::WOULD_COPY_CARRIED), 1);
        assert_eq!(goes_to(&func, 2), vec![5]);
        assert_eq!(goes_to(&func, 1), vec![3]);
    }

    #[test]
    fn the_block_parameter_is_substituted_into_what_the_arm_carries() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let arms = [func.create_block(), func.create_block()];
        let join = func.create_block();
        let param = func.append_param(join, Type::int(32));
        let yes = func.create_block();
        func.append_param(yes, Type::int(32));
        let no = func.create_block();

        let mut build = Builder::new(&mut func, entry);
        let cond = build.iconst(Type::int(1), 1);
        build.br_if(cond, arms[0], &[], arms[1], &[]);
        let mut sent = Vec::new();
        for (arm, value) in arms.iter().zip([1, 2]) {
            let mut build = Builder::new(&mut func, *arm);
            let it = build.iconst(Type::int(32), value);
            sent.push(it);
            build.jump(join, &[it]);
        }
        let mut build = Builder::new(&mut func, join);
        let one = build.iconst(Type::int(32), 1);
        let test = build.icmp(IntPred::Eq, param, one);
        // The arm passes the block's own parameter on, which along each edge is the constant that
        // edge was carrying.
        build.br_if(test, yes, &[param], no, &[]);
        for block in [yes, no] {
            let mut build = Builder::new(&mut func, block);
            build.ret(&[]);
        }

        let stats = thread(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::THREADED), 2);
        assert_eq!(goes_to(&func, 1), vec![4]);
        assert_eq!(carries(&func, 1), vec![sent[0]]);
    }

    #[test]
    fn a_switch_the_edge_decides_is_threaded() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let arms = [func.create_block(), func.create_block()];
        let join = func.create_block();
        let param = func.append_param(join, Type::int(32));
        let cases = [func.create_block(), func.create_block(), func.create_block()];

        let mut build = Builder::new(&mut func, entry);
        let cond = build.iconst(Type::int(1), 1);
        build.br_if(cond, arms[0], &[], arms[1], &[]);
        for (arm, value) in arms.iter().zip([0, 1]) {
            let mut build = Builder::new(&mut func, *arm);
            let it = build.iconst(Type::int(32), value);
            build.jump(join, &[it]);
        }
        let mut build = Builder::new(&mut func, join);
        build.switch(param, cases[0], &[(0, cases[1]), (1, cases[2])]);
        for block in cases {
            let mut build = Builder::new(&mut func, block);
            build.ret(&[]);
        }

        let stats = thread(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::THREADED), 2);
        assert_eq!(goes_to(&func, 1), vec![cases[1].index()]);
        assert_eq!(goes_to(&func, 2), vec![cases[2].index()]);
    }

    /// A loop whose header takes a parameter, entered from outside with a constant.
    ///
    /// Block 0 is the entry and jumps into the header carrying 1, block 1 is the header and tests
    /// its parameter, block 2 is the body and jumps back carrying the function's own parameter,
    /// block 3 is the way out and is where the test's false arm goes, and block 4 is somewhere
    /// outside the loop. Which block the true arm goes to is the caller's to choose, which is what
    /// makes one of these a thread into the middle of the loop and the other a thread onto a block
    /// the loop has nothing to do with.
    ///
    /// This is the only shape in which a thread can make a loop irreducible when nothing is copied.
    /// The block being threaded past has to be the header itself, because otherwise the arm being
    /// threaded onto was already a way into the loop from outside it and the loop was already
    /// irreducible before this pass looked at it.
    fn loop_with_a_parameter(arm: usize) -> Func {
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::int(32)]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let outside = func.append_param(entry, Type::int(32));
        let header = func.create_block();
        let param = func.append_param(header, Type::int(32));
        let body = func.create_block();
        let out = func.create_block();
        let elsewhere = func.create_block();
        let taken = [entry, header, body, out, elsewhere][arm];

        let mut build = Builder::new(&mut func, entry);
        let one = build.iconst(Type::int(32), 1);
        build.jump(header, &[one]);
        let mut build = Builder::new(&mut func, header);
        let lit = build.iconst(Type::int(32), 1);
        let test = build.icmp(IntPred::Eq, param, lit);
        build.br_if(test, taken, &[], out, &[]);
        let mut build = Builder::new(&mut func, body);
        // Carrying the function's own parameter, so the edge back decides nothing and each of
        // these tests is about the one edge that comes from outside.
        build.jump(header, &[outside]);
        for block in [out, elsewhere] {
            let mut build = Builder::new(&mut func, block);
            build.ret(&[]);
        }
        func
    }

    /// A loop in which one arm into a join can be threaded onto the way out.
    ///
    /// Block 0 is the entry, block 1 the header, which branches to blocks 2 and 3, block 4 the
    /// join, which leaves the loop for block 6 or goes on to the latch, block 5. Block 2 carries 1
    /// into the join, so its edge there could be threaded onto the way out, and block 3 carries 0.
    /// With `other` block 2 also has an edge to block 3, which is a way back to the header that
    /// does not go through the join.
    fn loop_with_a_way_out(other: bool) -> Func {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let blocks: Vec<Block> = (0..7).map(|_| func.create_block()).collect();
        let param = func.append_param(blocks[4], Type::int(32));

        Builder::new(&mut func, blocks[0]).jump(blocks[1], &[]);
        let mut build = Builder::new(&mut func, blocks[1]);
        let cond = build.iconst(Type::int(1), 1);
        build.br_if(cond, blocks[2], &[], blocks[3], &[]);
        let mut build = Builder::new(&mut func, blocks[2]);
        let one = build.iconst(Type::int(32), 1);
        if other {
            let cond = build.iconst(Type::int(1), 1);
            build.br_if(cond, blocks[4], &[one], blocks[3], &[]);
        } else {
            build.jump(blocks[4], &[one]);
        }
        let mut build = Builder::new(&mut func, blocks[3]);
        let zero = build.iconst(Type::int(32), 0);
        build.jump(blocks[4], &[zero]);
        let mut build = Builder::new(&mut func, blocks[4]);
        let lit = build.iconst(Type::int(32), 1);
        let test = build.icmp(IntPred::Eq, param, lit);
        build.br_if(test, blocks[6], &[], blocks[5], &[]);
        Builder::new(&mut func, blocks[5]).jump(blocks[1], &[]);
        Builder::new(&mut func, blocks[6]).ret(&[]);
        func
    }

    /// Every loop as its header, its blocks and its latches, and the blocks in no loop that are
    /// in an irreducible region, which is what the pass asks the forest.
    type Forest = (Vec<(usize, Vec<usize>, Vec<usize>)>, Vec<usize>);

    fn forest(loops: &crate::Loops) -> Forest {
        let sorted = |blocks: &[Block]| {
            let mut out: Vec<usize> = blocks.iter().map(|block| block.index()).collect();
            out.sort_unstable();
            out
        };
        let mut all: Vec<_> = loops
            .all()
            .map(|id| {
                (loops.header(id).index(), sorted(loops.blocks(id)), sorted(loops.latches(id)))
            })
            .collect();
        all.sort_unstable();
        (all, sorted(loops.irreducible()))
    }

    /// Asks [`super::within`] about threading block 2's edge into the join onto the way out, then
    /// makes the thread and says whether the forest built before it is the one built after.
    fn exit_thread(other: bool) -> (bool, bool) {
        let mut func = loop_with_a_way_out(other);
        let an = crate::machine::fixtures::analyses();
        let before = forest(an.loops(&func));
        let (from, join, out) = (Block::from_usize(2), Block::from_usize(4), Block::from_usize(6));
        let term = func.terminator(from).expect("every block here has one");
        let at = func.target_list(term).iter().next().expect("the edge into the join is first");
        let kept = super::within(an.loops(&func), &func, from, join, at, out);
        let call = func[at];
        func.set_block_call(at, BlockCall { block: out, args: ValueList::EMPTY, ..call });
        let after = forest(crate::machine::fixtures::analyses().loops(&func));
        (kept, before == after)
    }

    #[test]
    fn a_thread_onto_the_way_out_keeps_the_forest_when_the_loop_still_goes_round() {
        // Block 2 still gets back to the header through block 3, and the header still gets to the
        // join through block 3, so every loop is what it was.
        assert_eq!(exit_thread(true), (true, true));
    }

    #[test]
    fn a_thread_onto_the_way_out_that_takes_a_block_out_of_the_loop_is_a_forest_built_again() {
        // Block 2's only way back went through the join, so the thread takes it out of the loop.
        assert_eq!(exit_thread(false), (false, false));
    }

    #[test]
    fn threading_into_a_loop_anywhere_but_its_header_is_refused() {
        // The true arm is the body, so pointing the edge from outside at it would give the loop a
        // second way in and make it irreducible.
        let mut func = loop_with_a_parameter(2);
        let stats = thread(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::THREADED), 0);
        assert_eq!(stats.count(Kind::Missed, super::WOULD_BREAK_A_LOOP), 1);
        assert_eq!(goes_to(&func, 0), vec![1]);
    }

    #[test]
    fn threading_onto_a_block_outside_the_loop_is_allowed() {
        // The true arm is in no loop at all, so the edge from outside can be pointed straight at
        // it and the loop keeps the one way in it had.
        let mut func = loop_with_a_parameter(4);
        let stats = thread(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::THREADED), 1);
        assert_eq!(goes_to(&func, 0), vec![4]);
    }

    #[test]
    fn threading_onto_the_header_of_a_loop_is_allowed() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let join = func.create_block();
        let param = func.append_param(join, Type::int(32));
        let header = func.create_block();
        let out = func.create_block();

        let mut build = Builder::new(&mut func, entry);
        let one = build.iconst(Type::int(32), 1);
        build.jump(join, &[one]);
        let mut build = Builder::new(&mut func, join);
        let lit = build.iconst(Type::int(32), 1);
        let test = build.icmp(IntPred::Eq, param, lit);
        build.br_if(test, header, &[], out, &[]);
        let mut build = Builder::new(&mut func, header);
        // A loop of one block, so the header is its own latch and the block being threaded onto
        // is the header itself, which is the way in the loop already has.
        let again = build.iconst(Type::int(1), 1);
        build.br_if(again, header, &[], out, &[]);
        let mut build = Builder::new(&mut func, out);
        build.ret(&[]);

        let stats = thread(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::THREADED), 1);
        assert_eq!(goes_to(&func, 0), vec![2]);
    }

    /// A region with two ways in, which is what the dispatch of a computed `goto` makes.
    ///
    /// Block 0 is the entry and goes to block 1 or to block 3 on its parameter. Block 1 jumps to
    /// block 2 carrying a constant true, block 2 tests what it was given and goes to block 3 or to
    /// block 4, and block 3 goes back to block 1 or out to block 4. So 1, 2 and 3 are a cycle the
    /// entry reaches at two places, and the forest calls all three irreducible.
    fn two_way_region() -> Func {
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::int(1)]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let cond = func.append_param(entry, Type::int(1));
        let first = func.create_block();
        let test = func.create_block();
        let param = func.append_param(test, Type::int(1));
        let second = func.create_block();
        let out = func.create_block();

        let mut build = Builder::new(&mut func, entry);
        build.br_if(cond, first, &[], second, &[]);
        let mut build = Builder::new(&mut func, first);
        let yes = build.iconst(Type::int(1), 1);
        build.jump(test, &[yes]);
        let mut build = Builder::new(&mut func, test);
        build.br_if(param, second, &[], out, &[]);
        let mut build = Builder::new(&mut func, second);
        build.br_if(cond, first, &[], out, &[]);
        let mut build = Builder::new(&mut func, out);
        build.ret(&[]);
        func
    }

    #[test]
    fn a_thread_that_copies_nothing_is_allowed_in_an_irreducible_region() {
        let mut func = two_way_region();
        let region = crate::machine::fixtures::analyses().loops(&func).irreducible().to_vec();
        assert_eq!(region, [Block::from_usize(1), Block::from_usize(2), Block::from_usize(3)]);
        let stats = thread(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::THREADED), 1);
        assert_eq!(stats.count(Kind::Missed, super::WOULD_BREAK_A_LOOP), 0);
        assert_eq!(goes_to(&func, 1), vec![3]);
    }

    #[test]
    fn a_copy_is_still_refused_in_an_irreducible_region() {
        // The same region with a number worked out in block 2 and carried to block 3, so the
        // thread needs a copy of block 2 to work the number out on the way.
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::int(1)]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let cond = func.append_param(entry, Type::int(1));
        let first = func.create_block();
        let test = func.create_block();
        let param = func.append_param(test, Type::int(1));
        let second = func.create_block();
        func.append_param(second, Type::int(32));
        let out = func.create_block();

        let mut build = Builder::new(&mut func, entry);
        let zero = build.iconst(Type::int(32), 0);
        build.br_if(cond, first, &[], second, &[zero]);
        let mut build = Builder::new(&mut func, first);
        let yes = build.iconst(Type::int(1), 1);
        build.jump(test, &[yes]);
        let mut build = Builder::new(&mut func, test);
        let seven = build.iconst(Type::int(32), 7);
        build.br_if(param, second, &[seven], out, &[]);
        let mut build = Builder::new(&mut func, second);
        build.br_if(cond, first, &[], out, &[]);
        let mut build = Builder::new(&mut func, out);
        build.ret(&[]);

        let stats = copying(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::COPIED), 0);
        assert_eq!(stats.count(Kind::Missed, super::WOULD_BREAK_A_LOOP), 1);
        assert_eq!(goes_to(&func, 1), vec![2]);
    }

    #[test]
    fn fuel_stops_the_threading_where_it_stands() {
        let (mut func, _) = diamond(1, 2);
        let mut fuel = Fuel::of(1);
        let stats = FREE.run(&mut func, &mut crate::machine::fixtures::analyses(), &mut fuel);
        assert_eq!(stats.count(Kind::Optimized, super::THREADED), 1);
        assert_eq!(stats.count(Kind::Missed, super::NO_FUEL), 1);
        assert_eq!(goes_to(&func, 2), vec![3], "the second edge is where it was");
    }
}
