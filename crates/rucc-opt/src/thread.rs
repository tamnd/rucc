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
//! being true. And a block already in an irreducible region is left alone entirely, since the loop
//! forest has given up on it and the two checks above would be reading an answer nobody stands
//! behind.
//!
//! Without a copy no new cycle can appear. The new edge from A goes where the edge out of B went, so
//! a path along it is a path that was already there with B taken out of the middle. With one the
//! same is true of the path through the copy, which is B's path with B's test taken out, so loops
//! can still only be destroyed. The loop forest is rebuilt after each thread anyway, which is what
//! keeps the next decision honest.
//!
//! # Which level this runs at
//!
//! [`FREE`] runs at `-Os` and `-Oz`. Section 23.6 restricts threading at those two to the case where
//! the block is empty, on the ground that it is the only part that is free, and [`FREE`] is that
//! part generalized: a block whose instructions all have no effects and whose outgoing arguments do
//! not come from it costs the same as an empty one, which is nothing. [`COPY`] runs at `-O1` and
//! above, as GCC's `-fthread-jumps` does.
//!
//! Once, and not to a fixed point. Threading enables threading, and section 23.7 says the answer to
//! that is a fixed number of instances rather than a loop, because threading is the pass where
//! adversarial input is easiest to construct. Section 23.5 asks for two instances at `-O2`, an early
//! one and a late one after the loop pipeline and SCCP. There is one here, in the early position.
//! The late one wants the passes that are not written yet.
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

use std::collections::{HashMap, HashSet};

use rucc_base::Idx;
use rucc_cost::heuristics;
use rucc_ir::{Block, BlockCall, Builder, Def, Func, Inst, Opcode, Start, Value, ValueList};

use crate::frontier::Frontiers;
use crate::header_copy::{clone_into, repeatable};
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
    budget: u32,
}

/// The instance `-Os` and `-Oz` run, which threads an edge only where nothing has to be copied.
pub static FREE: Thread = Thread { name: "thread", budget: 0 };

/// The instance `-O1` and above run, which copies a block of up to section 23.4's fifteen.
pub static COPY: Thread =
    Thread { name: "thread-copy", budget: heuristics::JUMP_THREAD_DUPLICATION_INSNS };

impl Pass for Thread {
    fn name(&self) -> &'static str {
        self.name
    }

    fn describe(&self) -> &'static str {
        if self.budget == 0 {
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
        let mut leaks = leaky(func);
        let unbound = Bindings::new();
        let mut threaded = false;
        // What each copy made in this run has cost the path it is on, which is what the path limit
        // of section 23.4 adds up, and how many copies there have been.
        let mut paths: HashMap<Block, u32> = HashMap::new();
        let mut copies = 0;
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
                let (free, why) = if leaks.contains(&block) {
                    (None, WOULD_COPY_READ_BELOW)
                } else {
                    (carried(func, block, call, &subst), WOULD_COPY_CARRIED)
                };
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
                if !allowed(an.loops(func), from, call.block) {
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
                    let (copy, out) = copy(func, block, at, call, &subst);
                    edges.entry(call.block).or_default().push((copy, out));
                    paths.insert(copy, path);
                    copies += 1;
                    // The merges put parameters on blocks further down, and a parameter something
                    // reads from below is exactly what the leaky set is about. It went stale in the
                    // safe direction before there were copies. It does not now.
                    leaks = leaky(func);
                    stats.optimized(COPIED);
                }
                // The loop forest was about the function as it was a moment ago, and the manager
                // clears the cache after the pass returns, which is too late for the next edge.
                an.clear();
                threaded = true;
            }
        }
        if threaded {
            // Threading every edge into a block leaves nothing arriving at it, and section 6.5
            // makes taking an unreachable block out the standing obligation of whichever pass
            // stranded it rather than something the next pass tidies up. The verifier holds every
            // pass to that, so this is not a courtesy.
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
        paths: &HashMap<Block, u32>,
        copies: u32,
    ) -> Result<u32, Option<&'static str>> {
        if self.budget == 0 {
            return Err(None);
        }
        let body: Vec<Inst> = func.insts(block).filter(|&inst| !func.is_terminator(inst)).collect();
        if !body.iter().all(|&inst| repeatable(func, inst)) {
            return Err(Some(ODD));
        }
        let mut cost = u32::try_from(body.len()).unwrap_or(u32::MAX);
        if back_edge(loops, block, into) {
            cost = cost.saturating_mul(heuristics::JUMP_THREAD_BACK_EDGE_SCALE);
        }
        if cost > self.budget {
            return Err(Some(TOO_BIG));
        }
        let path = paths.get(&from).copied().unwrap_or(0).saturating_add(cost);
        if path > heuristics::JUMP_THREAD_PATH_INSNS {
            return Err(Some(TOO_LONG));
        }
        if copies >= heuristics::JUMP_THREAD_PATHS {
            return Err(Some(TOO_MANY));
        }
        Ok(path)
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
fn copy(
    func: &mut Func,
    block: Block,
    at: Idx<BlockCall>,
    call: BlockCall,
    subst: &Bindings,
) -> (Block, Idx<BlockCall>) {
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
    repair(func, block, copy, &map);
    (copy, out)
}

/// Gives every read of a value `block` defines, from anywhere but `block`, the definition that
/// reaches it now that `copy` defines the value as well.
///
/// The frontier and the dominator tree are of the graph with the edge already moved, and they are
/// shared by every value, since the merges only add parameters and a parameter moves no edge.
fn repair(func: &mut Func, block: Block, copy: Block, map: &Bindings) {
    let values = read_outside(func, block, copy);
    if values.is_empty() {
        return;
    }
    let cfg = Cfg::new(func);
    let dom = Dominators::new(&cfg);
    let frontiers = Frontiers::new(&cfg, &dom);
    let mut joins: HashSet<Block> = HashSet::new();
    let mut work = vec![block, copy];
    while let Some(at) = work.pop() {
        for &join in frontiers.of(at) {
            if joins.insert(join) {
                work.push(join);
            }
        }
    }
    for value in values {
        let copied = map.get(&value).copied().expect("the copy defines every value the block does");
        let mut reaching = Reaching {
            dom: &dom,
            block,
            copy,
            value,
            copied,
            params: HashMap::new(),
            memo: HashMap::new(),
        };
        merge(func, &cfg, &joins, &mut reaching);
    }
}

/// The values `block` defines that something outside it and outside its copy reads.
fn read_outside(func: &Func, block: Block, copy: Block) -> Vec<Value> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for other in func.blocks() {
        if other == block || other == copy {
            continue;
        }
        for inst in func.insts(other) {
            uses::operands(func, inst, |value| {
                if defined_in(func, value) == Some(block) && seen.insert(value) {
                    out.push(value);
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
    params: HashMap<Block, Value>,
    /// What reached the start of each block already asked about.
    memo: HashMap<Block, Value>,
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
fn merge(func: &mut Func, cfg: &Cfg, joins: &HashSet<Block>, reaching: &mut Reaching<'_>) {
    let (block, copy, value) = (reaching.block, reaching.copy, reaching.value);
    // Where the value is still wanted at the start of a block. A read is where it starts, and it
    // goes up through predecessors until it meets one of the two definitions.
    let mut readers = Vec::new();
    for other in func.blocks() {
        if other == block || other == copy {
            continue;
        }
        let mut reads = false;
        for inst in func.insts(other) {
            uses::operands(func, inst, |used| reads |= used == value);
        }
        if reads {
            readers.push(other);
        }
    }
    let mut live: HashSet<Block> = readers.iter().copied().collect();
    let mut work = readers.clone();
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
    for &reader in &readers {
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
    for other in func.blocks().collect::<Vec<Block>>() {
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
fn leaky(func: &Func) -> HashSet<Block> {
    let mut out = HashSet::new();
    for block in func.blocks().collect::<Vec<Block>>() {
        for inst in func.insts(block).collect::<Vec<Inst>>() {
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

/// Whether the loop structure survives pointing this edge at that block.
///
/// Section 23.5, and every answer of `false` is a refusal rather than a cost. Entering a loop
/// anywhere but at its header makes the loop irreducible, moving a latch's edge is how the single
/// latch property stops holding, and a block the forest has already given up on is one there is no
/// useful answer about.
fn allowed(loops: &Loops, from: Block, into: Block) -> bool {
    if loops.is_irreducible(from) || loops.is_irreducible(into) {
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
        Block, Builder, Flags, Func, IntPred, MemInfo, MemOrder, Restrict, Signature, Type, Value,
    };

    use std::collections::HashMap;

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
        let address = build.unary(rucc_ir::Opcode::IntToPtr, address, Type::PTR);
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
            let address = build.unary(rucc_ir::Opcode::IntToPtr, address, Type::PTR);
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
                    sum = build.binary(rucc_ir::Opcode::Add, sum, param, Flags::NONE);
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
        let mut paths = HashMap::new();
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
        let sum = build.binary(rucc_ir::Opcode::Add, param, one, Flags::NONE);
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
        let sum = build.binary(rucc_ir::Opcode::Add, param, one, Flags::NONE);
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
        let sum = build.binary(rucc_ir::Opcode::Add, param, one, Flags::NONE);
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
