//! Copies a loop's header in front of the loop, so the test ends up at the bottom.
//!
//! Design: `spec/optimizer/26-loop-canonicalization.md` section 26.6, with 26.7 for where it sits
//! and 26.8 for the two ways it goes wrong.
//!
//! [`crate::canon`] establishes the four properties every loop pass is allowed to assume and
//! generates nothing on its own. This is the fifth property and it is the one that changes the
//! program. A `while (c) { body }` tests at the top, so its header is a join and a branch at once
//! and the test runs once more than the body does. Copying the header in front of the loop turns
//! it into `if (c) { do { body } while (c); }`, which evaluates the condition exactly as often and
//! leaves a loop whose body is a single region and whose exit test is at the bottom where the
//! induction variable's last value is.
//!
//! # What it is really for
//!
//! Section 26.6 says the largest single benefit is not the shape. It is that after the copy the
//! entry test stands in front of the loop where document 10's ranges can be asked about it, and
//! where the ranges settle it the loop is known to run at least one iteration. That is what turns
//! a trip count estimate into a bound, what lets hoisting move a computation out without proving
//! it safe to speculate, and what saves the vectorizer a guard. So the range query is not a
//! refinement on the copy, it is half of the reason to make it, and it happens here rather than
//! being left to [`crate::prune`] because prune has already run by the time the loop pipeline
//! opens.
//!
//! # One block, not a chain
//!
//! GCC copies as many blocks as its budget allows, walking down from the header while
//! `should_duplicate_loop_header_p` keeps saying yes. This copies the header and stops. The header
//! is where the exit test is, so one block is what the do-while form needs, and a chain buys the
//! cases where the condition is spread over several blocks that nothing has managed to merge. The
//! bound is the same either way and the second block can be added when the corpus says which
//! programs want it.
//!
//! Copying a header could otherwise feed itself: the block the copy makes the new header of the
//! loop may test and exit as well, and copying that one exposes a third. Every header this pass
//! copies and every block it makes a header of are put aside, so each loop is looked at once per
//! run and the growth is bounded by the loop count rather than by how the branches happen to nest.
//!
//! # Why the copy repeats nothing
//!
//! The copy runs exactly where the header's first execution used to, so nothing in the program
//! happens a different number of times. That argument would let a store or a call be copied, and
//! section 26.8 refuses both anyway, through document 17.1's whitelist, which is
//! [`Opcode::has_effects`]. The reason to keep the refusal is that the argument above holds for
//! one block and stops holding the moment the copy is a chain, and a pass whose correctness
//! depends on a bound somebody may raise later is one that will be wrong later. Refusing here
//! costs the headers with a load in them, which document 27's hoisting is the pass for.
//!
//! # What the copy owes the values
//!
//! The header used to dominate the whole loop. After the copy it does not: the body is reached
//! from the copy as well, so a value the header defined and the body read has two definitions
//! reaching it and needs a merge. The merge goes where the two paths meet, which is the body, as
//! one more block parameter carrying the header's value on the back edge and the copy's on the
//! way in.
//!
//! Values the header defines and something outside the loop reads are refused rather than merged.
//! After [`crate::canon`] there are none, because loop-closed form has already routed them through
//! the exit, so the case this declines is the one where somebody ran this pass without the
//! canonicalizer and the answer to that is a missed optimization rather than a second merge
//! written for a shape the pipeline does not produce.
//!
//! # Which level
//!
//! `-O1` and above at [`SPEED`]'s budget, which is GCC's twenty. `-Os` at [`SIZE`]'s, which is
//! section 26.6's five, because the do-while form is slightly smaller in the steady state and the
//! copy is what it costs. `-Oz` does not run it at all. Two passes rather than one with a knob,
//! because a pass here is a name a `-f` flag spells and there is nowhere for a level to hand a
//! pass a number.

use std::collections::{HashMap, HashSet};

use rucc_cost::heuristics;
use rucc_ir::{
    Block, BlockCall, Builder, ExtraKind, Func, Inst, InstData, Opcode, Start, Type, Value,
    ValueList,
};

use crate::cfg::Cfg;
use crate::dom::Dominators;
use crate::loops::{LoopId, Loops};
use crate::range::query::Ranges;
use crate::{Analyses, Fuel, Pass, Preserved, Stats, prune, simplify_cfg};

const COPIED: &str = "loop header copied in front of the loop so the test is at the bottom";
const ENTERED: &str = "entry test removed, the value ranges say the loop runs";
const SKIPPED: &str = "loop removed, the value ranges say the entry test never holds";
const UNDECIDED: &str = "entry test kept, the value ranges do not settle whether the loop runs";
const ALREADY: &str = "loop left as it was, it already tests at the bottom";
const TOO_BIG: &str = "loop header not copied, it is larger than this level allows";
const EFFECTS: &str = "loop header not copied, something in it may not be repeated";
const SHAPE: &str = "loop header not copied, its exit is not a two way branch";
const ESCAPES: &str = "loop header not copied, a value it defines is read outside the loop";
const NO_PREHEADER: &str = "loop header not copied, the loop has not been canonicalized";
const NO_FUEL: &str = "loop left as it was, the pass ran out of fuel";

/// Section 26.6's transformation, at one budget.
///
/// The budget is a field rather than a constant because the two levels that run this want
/// different ones, and it comes with the name for the same reason: the two instances are two
/// entries in [`crate::pass::PASSES`] and a pipeline picks one by naming it.
#[derive(Debug)]
pub struct HeaderCopy {
    /// What a `-f` flag spells.
    name: &'static str,
    /// How many instructions a header may hold and still be worth copying, which is
    /// [`heuristics::LOOP_HEADER_INSNS_FOR_SPEED`] or the size one next to it.
    budget: u32,
}

/// The instance `-O1`, `-O2` and `-O3` run, at GCC's budget.
pub static SPEED: HeaderCopy =
    HeaderCopy { name: "header-copy", budget: heuristics::LOOP_HEADER_INSNS_FOR_SPEED };

/// The instance `-Os` runs, at section 26.6's smaller one.
pub static SIZE: HeaderCopy =
    HeaderCopy { name: "header-copy-small", budget: heuristics::LOOP_HEADER_INSNS_FOR_SIZE };

impl Pass for HeaderCopy {
    fn name(&self) -> &'static str {
        self.name
    }

    fn describe(&self) -> &'static str {
        "copies a loop header in front of the loop, turning a while into a do-while"
    }

    fn preserves(&self) -> Preserved {
        // A block appears, two edges become four, and the body grows a parameter the loop carries.
        Preserved::NONE
    }

    fn run(&self, func: &mut Func, an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        if func.entry().is_none() {
            return stats;
        }
        let mut done = HashSet::new();
        let mut say = true;
        let mut dry = false;
        loop {
            let jobs = self.plan(func, an, &done, &mut stats, say);
            say = false;
            if jobs.is_empty() {
                break;
            }
            let mut copies = Vec::with_capacity(jobs.len());
            for job in &jobs {
                if !fuel.take() {
                    stats.missed(NO_FUEL);
                    dry = true;
                    break;
                }
                done.insert(job.header);
                done.insert(job.body);
                copies.push(apply(func, job));
                stats.optimized(COPIED);
            }
            an.clear();
            if settle(func, an, &copies, &mut stats) {
                an.clear();
            }
            if dry {
                break;
            }
        }
        if stats.changed() {
            // Section 6.5 leaves the stranded blocks to whoever stranded them, and a loop whose
            // entry test the ranges disproved is a loop nothing reaches any more.
            simplify_cfg::sweep(func, an, &mut stats);
        }
        an.clear();
        stats
    }
}

/// One loop to copy the header of, worked out against the function as it stands.
#[derive(Debug)]
struct Job {
    /// The loop, which is what [`independent`] asks about to decide whether two jobs meet.
    id: LoopId,
    /// The block holding the exit test.
    header: Block,
    /// The one block outside the loop the header is reached from.
    entry: Block,
    /// The header's successor inside the loop, which the copy makes the new header.
    ///
    /// The other one, which is where the loop leaves from, is not recorded. The copy branches to
    /// both by copying the header's own terminator, and nothing after that has a question to ask
    /// about the one that goes out.
    body: Block,
    /// The values the header defines and the rest of the loop reads, which need a merge at
    /// [`Job::body`] once there are two ways to get there.
    carried: Vec<Value>,
}

/// One loop the cheap checks accepted, waiting on the walk that says what it carries.
#[derive(Debug)]
struct Candidate {
    /// The loop.
    id: LoopId,
    /// The block holding the exit test.
    header: Block,
    /// The one block outside the loop the header is reached from.
    entry: Block,
    /// The header's successor inside the loop.
    body: Block,
    /// Everything the header defines, which [`carried`] sorts into what the loop reads and what
    /// nothing does.
    defined: Vec<Value>,
}

impl HeaderCopy {
    /// Every loop worth copying the header of that can be copied without looking again.
    ///
    /// A round rather than one at a time. A copy changes the shape of the loop it is made for, so
    /// the forest this was read out of is wrong about that loop afterwards, and the answer used to
    /// be to rebuild the graph, the dominator tree and the forest and ask again. On a function with
    /// sixteen hundred loops that is two thousand rebuilds, which was most of what an optimized
    /// build of tamnd/rucc#1086's test spent its time on. What the rebuild protects is one loop's
    /// shape, so this takes the loops whose shapes do not touch and copies all of their headers
    /// from the one look. [`independent`] is the argument for why that is the same edit.
    ///
    /// `say` is false on every call after the first so that a loop this declines is declined once
    /// rather than once per round.
    fn plan(
        &self,
        func: &Func,
        an: &mut Analyses,
        done: &HashSet<Block>,
        stats: &mut Stats,
        say: bool,
    ) -> Vec<Job> {
        let (cfg, dom, loops) = (an.cfg(func), an.dominators(func), an.loops(func));
        let mut wanted = Vec::new();
        for id in loops.all() {
            let header = loops.header(id);
            if done.contains(&header) {
                continue;
            }
            match self.consider(func, cfg, loops, id, header) {
                Ok(candidate) => wanted.push(candidate),
                Err(why) if say && why == ALREADY => stats.note(ALREADY),
                Err(why) if say => stats.missed(why),
                Err(_) => (),
            }
        }
        let jobs = carried(func, dom, loops, wanted, stats, say);
        independent(loops, jobs)
    }

    /// Whether this loop can have its header copied, and why not when it cannot.
    ///
    /// Everything here is answered out of the loop itself. What the header defines and who reads it
    /// is the one question that is about the whole function, and [`carried`] asks it for all the
    /// candidates at once.
    fn consider(
        &self,
        func: &Func,
        cfg: &Cfg,
        loops: &Loops,
        id: LoopId,
        header: Block,
    ) -> Result<Candidate, &'static str> {
        let leaves = cfg.successors(header).iter().any(|&to| !loops.contains(id, to));
        if !leaves {
            // The exit test is somewhere below, which is the shape this pass is trying to reach.
            // GCC asks the same question the other way round in `do_while_loop_p`.
            return Err(ALREADY);
        }
        let entry = loops.preheader(cfg, id).ok_or(NO_PREHEADER)?;
        let term = func.terminator(header).ok_or(SHAPE)?;
        if func[term].opcode != Opcode::BrIf {
            return Err(SHAPE);
        }
        let calls: Vec<BlockCall> = func.successors(term).collect();
        let [then_call, else_call] = calls[..].try_into().map_err(|_| SHAPE)?;
        let body = match (loops.contains(id, then_call.block), loops.contains(id, else_call.block))
        {
            (true, false) => then_call.block,
            (false, true) => else_call.block,
            _ => return Err(SHAPE),
        };
        if body == header {
            return Err(SHAPE);
        }
        let insts: Vec<Inst> = func.insts(header).filter(|&inst| inst != term).collect();
        if insts.len() > self.budget as usize {
            return Err(TOO_BIG);
        }
        for &inst in &insts {
            if !repeatable(func, inst) {
                return Err(EFFECTS);
            }
        }
        let mut defined: Vec<Value> = func[header].params.clone();
        for &inst in &insts {
            defined.extend(func[inst].results());
        }
        Ok(Candidate { id, header, entry, body, defined })
    }
}

/// Whether an instruction may stand in a second copy of the block it is in.
///
/// Two questions rather than one. [`Opcode::has_effects`] is document 17.1's whitelist and is what
/// section 26.8 names. The second is about this pass rather than about the program: an instruction
/// carrying a side table entry is copied here by copying the index, which is right for an
/// immediate, a symbol and a comparison because those tables are written once and read for ever,
/// and is not something to assume about a table nobody has checked. So the copy is restricted to
/// the payloads it has been thought about, and an instruction with any other is declined the same
/// way one with an effect is.
fn repeatable(func: &Func, inst: Inst) -> bool {
    let data = func[inst];
    if data.opcode.has_effects() || func.carries_mem(inst) {
        return false;
    }
    matches!(
        data.extra.kind(),
        ExtraKind::None
            | ExtraKind::Imm
            | ExtraKind::Symbol
            | ExtraKind::IntPred
            | ExtraKind::FloatPred
    )
}

/// Turns the candidates into jobs by working out, for each, which of the values its header defines
/// the rest of its loop reads.
///
/// Those are what the copy owes a merge at the body. A value read outside the loop takes the
/// candidate out rather than joining the list, because merging it would need a second parameter at
/// the exit and after [`crate::canon`] there is no such value to merge: loop-closed form has already
/// routed it.
///
/// One walk of the function for every candidate at once. tamnd/rucc#1015 made this one walk per
/// candidate instead of one per value, and tamnd/rucc#1086 is the same move one level up: a header
/// defines a handful of values, the function it is in can be very large, and a function with sixteen
/// hundred loops in it was paying for sixteen hundred walks per round. A value belongs to exactly
/// one candidate, because two candidates are two loops and two loops have two headers, so one map
/// from value to candidate is enough to share the walk.
fn carried(
    func: &Func,
    dom: &Dominators,
    loops: &Loops,
    wanted: Vec<Candidate>,
    stats: &mut Stats,
    say: bool,
) -> Vec<Job> {
    let mut watched: HashMap<Value, usize> = HashMap::new();
    for (which, candidate) in wanted.iter().enumerate() {
        for &value in &candidate.defined {
            watched.insert(value, which);
        }
    }
    let mut read: Vec<HashSet<Value>> = vec![HashSet::new(); wanted.len()];
    let mut escapes = vec![false; wanted.len()];
    let mut names: Vec<usize> = Vec::new();
    for block in func.blocks() {
        names.clear();
        for inst in func.insts(block) {
            reads(func, inst, block, &wanted, &watched, &mut read, &mut names);
        }
        for &which in &names {
            let candidate = &wanted[which];
            if !loops.contains(candidate.id, block) || !dom.dominates(candidate.body, block) {
                escapes[which] = true;
            }
        }
    }
    let mut jobs = Vec::new();
    for (which, candidate) in wanted.into_iter().enumerate() {
        if escapes[which] {
            if say {
                stats.missed(ESCAPES);
            }
            continue;
        }
        let taken = &read[which];
        let carried = candidate.defined.into_iter().filter(|value| taken.contains(value)).collect();
        jobs.push(Job {
            id: candidate.id,
            header: candidate.header,
            entry: candidate.entry,
            body: candidate.body,
            carried,
        });
    }
    jobs
}

/// Records every watched value this instruction names, as an operand or on an edge out of it, and
/// notes which candidates the block named something of.
///
/// Which candidates rather than which values, because a block that reads one of these from the wrong
/// place is an error for that candidate whichever of its values it read. A candidate's own header is
/// left out on both counts: the header is where these values are defined and reading one there is
/// neither a carry nor an escape.
fn reads(
    func: &Func,
    inst: Inst,
    block: Block,
    wanted: &[Candidate],
    watched: &HashMap<Value, usize>,
    read: &mut [HashSet<Value>],
    names: &mut Vec<usize>,
) {
    let mut note = |value: Value| {
        let Some(&which) = watched.get(&value) else { return };
        if block == wanted[which].header {
            return;
        }
        read[which].insert(value);
        if !names.contains(&which) {
            names.push(which);
        }
    };
    for &value in &func[func[inst].args] {
        note(value);
    }
    for call in func.successors(inst) {
        for &value in &func[call.args] {
            note(value);
        }
    }
}

/// The jobs out of a round that may all be applied before the function is looked at again.
///
/// Two jobs are safe together when the loops they are about share no block and neither loop holds
/// the other's preheader. The argument is that a job writes only inside its own loop and to its own
/// preheader. The copy is a new block put on the edge into the header, and the only block outside
/// the loop it edits is the preheader, whose one successor is the header by the definition
/// [`Loops::preheader`] uses. The merge gives the body a parameter and hands a value over on every
/// edge into the body, and every one of those edges comes from inside the loop, because a natural
/// loop is entered at its header alone and the body is not the header. The rewrite that follows
/// reaches only the blocks that read the carried values, and [`carried`] has already taken the job
/// out if any of those is outside the loop. So two jobs whose loops and preheaders do not meet edit
/// two disjoint sets of blocks, and applying both from one look at the function is the same function
/// as applying one, looking again, and applying the other.
///
/// Loops that share a block at all are nested, so the loops a taken one rules out are the ones it is
/// nested in and the ones nested in it.
fn independent(loops: &Loops, jobs: Vec<Job>) -> Vec<Job> {
    let mut blocked = vec![false; loops.count()];
    let mut taken = vec![false; loops.count()];
    let mut kept: Vec<Job> = Vec::new();
    for job in jobs {
        if blocked[job.id.index()] || inside(loops, &taken, job.entry) {
            continue;
        }
        let mut up = Some(job.id);
        while let Some(id) = up {
            blocked[id.index()] = true;
            up = loops.parent(id);
        }
        let mut down = vec![job.id];
        while let Some(id) = down.pop() {
            blocked[id.index()] = true;
            down.extend(loops.children(id));
        }
        // And no later job may be about a loop this one's preheader sits in.
        let mut around = loops.innermost(job.entry);
        while let Some(id) = around {
            blocked[id.index()] = true;
            around = loops.parent(id);
        }
        taken[job.id.index()] = true;
        kept.push(job);
    }
    kept
}

/// Whether any loop holding this block has been taken already.
fn inside(loops: &Loops, taken: &[bool], block: Block) -> bool {
    let mut walk = loops.innermost(block);
    while let Some(id) = walk {
        if taken[id.index()] {
            return true;
        }
        walk = loops.parent(id);
    }
    false
}

/// Makes the copy, puts it on the edge into the loop, and returns it.
fn apply(func: &mut Func, job: &Job) -> Block {
    let term = func.terminator(job.header).expect("the plan read this terminator");
    let entry_term = func.terminator(job.entry).expect("a preheader ends in a jump");
    // The header's parameters stand for whatever the one edge in hands them, so the copy is
    // written in terms of those arguments and needs no parameters of its own.
    let incoming = edge_args(func, entry_term, job.header);
    let mut map: HashMap<Value, Value> = HashMap::new();
    for (&param, &arg) in func[job.header].params.clone().iter().zip(&incoming) {
        map.insert(param, arg);
    }
    let copy = func.create_block();
    let insts: Vec<Inst> = func.insts(job.header).filter(|&inst| inst != term).collect();
    for inst in insts {
        clone_into(func, copy, inst, &mut map);
    }
    clone_branch(func, copy, term, &map);
    for at in func.target_list(entry_term).iter() {
        let call = func[at];
        if call.block == job.header {
            func.set_block_call(at, BlockCall { block: copy, args: ValueList::EMPTY, ..call });
        }
    }
    for &value in &job.carried {
        let arrived = map.get(&value).copied().unwrap_or(value);
        merge(func, job, copy, value, arrived);
    }
    copy
}

/// The arguments a terminator hands one of its targets.
fn edge_args(func: &Func, term: Inst, to: Block) -> Vec<Value> {
    for call in func.successors(term) {
        if call.block == to {
            return func[call.args].to_vec();
        }
    }
    Vec::new()
}

/// Copies one instruction to the end of a block, under the substitution, and records its results.
fn clone_into(func: &mut Func, into: Block, inst: Inst, map: &mut HashMap<Value, Value>) {
    let data = func[inst];
    let args: Vec<Value> =
        func[data.args].iter().map(|value| map.get(value).copied().unwrap_or(*value)).collect();
    let types: Vec<Type> = data.results().map(|result| func[result].ty).collect();
    let span = func.span(inst);
    let args = func.push_values(&args);
    let fresh = func.create_inst(InstData { args, ..data }, &types, span);
    func.append_inst(into, fresh);
    for (old, new) in data.results().zip(func[fresh].results()) {
        map.insert(old, new);
    }
}

/// Copies the header's two way branch to the end of the copy, under the substitution.
///
/// The targets are the header's own. What that means for the graph is that the copy decides,
/// before the loop, which of the two places the header would have gone control goes to, and the
/// header is left deciding it for every iteration after the first.
fn clone_branch(func: &mut Func, into: Block, term: Inst, map: &HashMap<Value, Value>) {
    let at = |value: &Value| map.get(value).copied().unwrap_or(*value);
    let cond = at(&func[func[term].args][0]);
    let calls: Vec<BlockCall> = func.successors(term).collect();
    let args: Vec<Vec<Value>> =
        calls.iter().map(|call| func[call.args].iter().map(at).collect()).collect();
    Builder::new(func, into).br_if(cond, calls[0].block, &args[0], calls[1].block, &args[1]);
}

/// Gives the body a parameter for a value the header defines, and points the loop at it.
///
/// Three kinds of edge arrive at the body once the copy is in place. The header's carries what the
/// header worked out, which is the value on every iteration after the first. The copy's carries
/// what the copy worked out, which is the value on the first. Anything else is a block inside the
/// loop, and what is current there is the parameter itself, which is available because the body
/// dominates the whole loop the moment the copy is the only way in.
fn merge(func: &mut Func, job: &Job, copy: Block, value: Value, arrived: Value) {
    let param = func.append_param(job.body, func[value].ty);
    for block in func.blocks().collect::<Vec<_>>() {
        let Some(term) = func.terminator(block) else { continue };
        let carry = if block == job.header {
            value
        } else if block == copy {
            arrived
        } else {
            param
        };
        for at in func.target_list(term).iter() {
            let call = func[at];
            if call.block != job.body {
                continue;
            }
            let args = func.append_arg(call.args, carry);
            func.set_block_call(at, BlockCall { args, ..call });
        }
    }
    // The names go where the readers go. The header still has the value and the rest of the loop
    // has the parameter, so a declaration named on the value is the parameter as well, and a start
    // anywhere but the header is a place the declaration was given the parameter. Left on the
    // value, it would say the declaration holds whatever the header is handed once the loop has
    // turned round, which is the next value rather than this one. tamnd/rucc#1810.
    for decl in func.value_decls(value).collect::<Vec<u32>>() {
        func.declare_value(param, decl);
    }
    let elsewhere: Vec<Start> = func
        .value_starts(value)
        .filter(|&start| {
            func.start_place(start).map_or(start.block, |(block, _)| block) != job.header
        })
        .collect();
    func.move_starts(value, param, &elsewhere);
    // Everything the header used to reach reads the parameter now. The header itself does not:
    // what it hands the body is still its own definition, and that is the edge the parameter was
    // put there to distinguish.
    for block in func.blocks().collect::<Vec<_>>() {
        if block == job.header || block == copy {
            continue;
        }
        for inst in func.insts(block).collect::<Vec<_>>() {
            let swap = |had: Value| if had == value { param } else { had };
            func.rewrite(func[inst].args, swap);
            for at in func.target_list(inst).iter() {
                func.rewrite(func[at].args, swap);
            }
        }
    }
}

/// Asks the ranges whether the copied tests are settled, and takes out the ones that are.
///
/// This is section 26.6's point about the entry condition. A test that always holds leaves a loop
/// known to run at least once, which is what document 07.5's trip count wanted. One that never
/// holds leaves the loop unreachable, and taking it out is [`crate::simplify_cfg::sweep`]'s job
/// rather than this one's.
///
/// Every copy the round made is asked from the one set of ranges, and the answers are acted on
/// afterwards. That is sound because every question is about a different block's terminator and
/// every answer is a fact about the values arriving there, which taking a branch out somewhere else
/// cannot make untrue. Asking one at a time would mean a graph and a dominator tree per copy, which
/// is the cost tamnd/rucc#1086 is about.
///
/// Answers whether it moved an edge, which the caller needs because the analyses this built are the
/// ones the next round wants and they are only stale if a branch came out. The ranges settle the
/// test on a minority of the loops here and the graph is the size of the function, so the rounds
/// where nothing happens used to pay for a rebuild that changed nothing. tamnd/rucc#1045.
fn settle(func: &mut Func, an: &mut Analyses, copies: &[Block], stats: &mut Stats) -> bool {
    let mut out: Vec<(Inst, BlockCall, bool)> = Vec::new();
    {
        let cfg = an.cfg(func);
        let dom = an.dominators(func);
        let mut ranges = Ranges::new(func, cfg, dom);
        for &copy in copies {
            let Some(term) = func.terminator(copy) else { continue };
            let cond = func[func[term].args][0];
            let Some(taken) = prune::settled(func, &mut ranges, copy, cond) else {
                stats.missed(UNDECIDED);
                continue;
            };
            let calls: Vec<BlockCall> = func.successors(term).collect();
            out.push((term, if taken { calls[0] } else { calls[1] }, taken));
        }
    }
    if out.is_empty() {
        return false;
    }
    for (term, call, taken) in out {
        simplify_cfg::jump_to(func, term, call);
        stats.optimized(if taken { ENTERED } else { SKIPPED });
    }
    true
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{
        Block, Builder, Def, Flags, Func, IntPred, MemInfo, MemOrder, Module, Opcode, Restrict,
        Signature, Start, Type, verify_func,
    };
    use rucc_target::{TargetInfo, Triple};

    use super::{HeaderCopy, SIZE, SPEED};
    use crate::canon::Canon;
    use crate::cfg::Cfg;
    use crate::dom::Dominators;
    use crate::loops::Loops;
    use crate::stats::Kind;
    use crate::{Fuel, Pass, Stats};

    /// Canonicalizes and then copies, with as much fuel as both want.
    ///
    /// Both, because the pass is written against the shape [`Canon`] leaves and running it over
    /// anything else is a test of a situation the pipeline does not produce. Section 26.7 puts the
    /// two next to each other in that order and so does this.
    fn copied(func: &mut Func, pass: &HeaderCopy) -> Stats {
        let mut an = crate::machine::fixtures::analyses();
        Canon.run(func, &mut an, &mut Fuel::unlimited());
        pass.run(func, &mut an, &mut Fuel::unlimited())
    }

    /// The forest of the function as it is now.
    fn forest(func: &Func) -> (Cfg, Dominators, Loops) {
        let cfg = Cfg::new(func);
        let dom = Dominators::new(&cfg);
        let loops = Loops::new(&cfg, &dom);
        (cfg, dom, loops)
    }

    /// Insists the function is one the rest of the compiler may believe.
    ///
    /// This is where most of the strength of these tests is. The copy gives the loop a second way
    /// in, which is exactly the edit that breaks a definition's dominance over its uses, and the
    /// verifier is what says whether the merge the pass wrote is the merge the graph needed.
    fn sound(func: &Func, names: &mut Interner) {
        let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().unwrap());
        let module = Module::new(names.intern("t.c"), &target);
        if let Err(errors) = verify_func(&module, func, names) {
            panic!("{errors:#?}");
        }
    }

    /// A counted loop that tests at the top, which is what `while (i < n)` lowers to.
    ///
    /// ```text
    /// entry: i0 = 0; jump head(i0)
    /// head(i): t = i < n; br t -> body, done
    /// body: next = i + 1; jump head(next)
    /// done: ret i
    /// ```
    ///
    /// `bound` is the limit as a constant, or nothing for a limit the function was handed and
    /// which the ranges therefore cannot settle.
    fn counted(bound: Option<i128>) -> (Func, Interner, Vec<Block>) {
        let mut names = Interner::new();
        let params: &[Type] = if bound.is_some() { &[] } else { &[Type::int(32)] };
        let signature = Signature::new().with_params(params).with_returns(&[Type::int(32)]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let head = func.create_block();
        let body = func.create_block();
        let done = func.create_block();
        let limit = match bound {
            Some(value) => Builder::new(&mut func, entry).iconst(Type::int(32), value),
            None => func.append_param(entry, Type::int(32)),
        };
        let i = func.append_param(head, Type::int(32));
        let zero = Builder::new(&mut func, entry).iconst(Type::int(32), 0);
        Builder::new(&mut func, entry).jump(head, &[zero]);
        let test = Builder::new(&mut func, head).icmp(IntPred::Slt, i, limit);
        Builder::new(&mut func, head).br_if(test, body, &[], done, &[]);
        let one = Builder::new(&mut func, body).iconst(Type::int(32), 1);
        let next = Builder::new(&mut func, body).binary(Opcode::Add, i, one, Flags::NONE);
        Builder::new(&mut func, body).jump(head, &[next]);
        Builder::new(&mut func, done).ret(&[i]);
        (func, names, vec![entry, head, body, done])
    }

    /// Whether the header of the only loop leaves it, which is the question section 26.6 is about.
    fn tests_at_the_top(func: &Func) -> bool {
        let (cfg, dom, loops) = forest(func);
        let _ = dom;
        let id = loops.all().next().expect("there is a loop");
        let header = loops.header(id);
        cfg.successors(header).iter().any(|&to| !loops.contains(id, to))
    }

    #[test]
    fn a_loop_that_tests_at_the_top_ends_up_testing_at_the_bottom() {
        let (mut func, mut names, _) = counted(None);
        assert!(tests_at_the_top(&func), "the shape this pass is for");

        let stats = copied(&mut func, &SPEED);
        assert_eq!(stats.count(Kind::Optimized, super::COPIED), 1);
        assert!(!tests_at_the_top(&func), "the header no longer leaves the loop");
        sound(&func, &mut names);
    }

    #[test]
    fn the_value_the_header_defined_is_merged_where_the_two_ways_in_meet() {
        let (mut func, mut names, blocks) = counted(None);
        let body = blocks[2];
        assert!(func[body].params.is_empty(), "the body carries nothing to start with");

        copied(&mut func, &SPEED);
        assert_eq!(func[body].params.len(), 1, "the counter arrives as a parameter now");
        assert_eq!(
            Cfg::new(&func).predecessors(body).len(),
            2,
            "one edge from the header and one from the copy"
        );
        sound(&func, &mut names);
    }

    /// `int j = i;` at the top of the body is a place `j` was given the counter, and once the body
    /// has a parameter of its own that place is where `j` was given the parameter. The header is
    /// handed the next value when the loop comes round, so a start left on its value would follow
    /// that one instead.
    #[test]
    fn a_name_on_the_value_the_body_now_takes_as_a_parameter_goes_with_it() {
        let (mut func, mut names, blocks) = counted(None);
        let (head, body) = (blocks[1], blocks[2]);
        let i = func[head].params[0];
        let test = func.insts(head).next();
        func.declare_value(i, 3);
        func.declare_value_from(i, Start { decl: 4, block: body, after: None });
        func.declare_value_from(i, Start { decl: 5, block: head, after: test });

        copied(&mut func, &SPEED);
        let param = func[body].params[0];
        assert_eq!(func.value_decls(param).collect::<Vec<u32>>(), vec![3]);
        assert_eq!(func.value_decls(i).collect::<Vec<u32>>(), vec![3], "still the header's");
        let decls = |value| func.value_starts(value).map(|start| start.decl).collect::<Vec<u32>>();
        assert_eq!(decls(param), vec![4], "the body's start is on the body's parameter");
        assert_eq!(decls(i), vec![5], "and the header's stays on the header's value");
        sound(&func, &mut names);
    }

    #[test]
    fn an_entry_test_the_ranges_settle_is_taken_out() {
        let (mut func, mut names, _) = counted(Some(10));

        let stats = copied(&mut func, &SPEED);
        assert_eq!(stats.count(Kind::Optimized, super::COPIED), 1);
        assert_eq!(stats.count(Kind::Optimized, super::ENTERED), 1);
        assert_eq!(stats.count(Kind::Missed, super::UNDECIDED), 0);
        sound(&func, &mut names);

        let (cfg, _dom, loops) = forest(&func);
        let id = loops.all().next().expect("the loop is still there");
        let entry = func.entry().expect("there is an entry");
        assert!(cfg.reaches(loops.header(id)), "and it is still reached");
        assert_eq!(cfg.successors(entry).len(), 1, "the guard in front of it has gone");
    }

    #[test]
    fn a_loop_the_ranges_say_never_runs_is_removed() {
        let (mut func, mut names, blocks) = counted(Some(0));

        let stats = copied(&mut func, &SPEED);
        assert_eq!(stats.count(Kind::Optimized, super::SKIPPED), 1);
        sound(&func, &mut names);

        let (_cfg, _dom, loops) = forest(&func);
        assert_eq!(loops.count(), 0, "there is no loop left");
        assert!(!func.blocks().any(|block| block == blocks[2]), "and the body has gone with it");
    }

    #[test]
    fn a_test_the_ranges_cannot_settle_leaves_the_guard_where_it_is() {
        let (mut func, _names, _) = counted(None);

        let stats = copied(&mut func, &SPEED);
        assert_eq!(stats.count(Kind::Optimized, super::COPIED), 1);
        assert_eq!(stats.count(Kind::Missed, super::UNDECIDED), 1);
        assert_eq!(stats.count(Kind::Optimized, super::ENTERED), 0);
    }

    #[test]
    fn a_second_run_changes_nothing() {
        let (mut func, mut names, _) = counted(None);
        copied(&mut func, &SPEED);
        let again =
            SPEED.run(&mut func, &mut crate::machine::fixtures::analyses(), &mut Fuel::unlimited());
        assert_eq!(again.count(Kind::Optimized, super::COPIED), 0, "there is nothing left to do");
        assert_eq!(again.count(Kind::Note, super::ALREADY), 1, "and it says why");
        sound(&func, &mut names);
    }

    #[test]
    fn a_header_that_writes_to_memory_is_left_alone() {
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::int(32), Type::PTR]).with_returns(&[]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let head = func.create_block();
        let body = func.create_block();
        let done = func.create_block();
        let limit = func.append_param(entry, Type::int(32));
        let addr = func.append_param(entry, Type::PTR);
        let i = func.append_param(head, Type::int(32));
        let zero = Builder::new(&mut func, entry).iconst(Type::int(32), 0);
        Builder::new(&mut func, entry).jump(head, &[zero]);
        let access = MemInfo {
            size: 4,
            align: 4,
            order: MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        };
        Builder::new(&mut func, head).store(i, addr, access, Flags::NONE);
        let test = Builder::new(&mut func, head).icmp(IntPred::Slt, i, limit);
        Builder::new(&mut func, head).br_if(test, body, &[], done, &[]);
        let one = Builder::new(&mut func, body).iconst(Type::int(32), 1);
        let next = Builder::new(&mut func, body).binary(Opcode::Add, i, one, Flags::NONE);
        Builder::new(&mut func, body).jump(head, &[next]);
        Builder::new(&mut func, done).ret(&[]);

        let stats = copied(&mut func, &SPEED);
        assert_eq!(stats.count(Kind::Optimized, super::COPIED), 0);
        assert_eq!(stats.count(Kind::Missed, super::EFFECTS), 1);
        assert!(tests_at_the_top(&func), "the loop is exactly as it was");
    }

    #[test]
    fn a_header_larger_than_the_level_allows_is_left_alone() {
        // Seven instructions in the header, which is over the size budget and well under the
        // speed one, so the two instances of the pass disagree about the same function.
        let stats = copied(&mut padded(6), &SIZE);
        assert_eq!(stats.count(Kind::Optimized, super::COPIED), 0);
        assert_eq!(stats.count(Kind::Missed, super::TOO_BIG), 1);

        let stats = copied(&mut padded(6), &SPEED);
        assert_eq!(stats.count(Kind::Optimized, super::COPIED), 1, "the speed budget is wider");
    }

    /// The counted loop with that many more instructions in its header, which do nothing.
    fn padded(extra: usize) -> Func {
        let (mut func, _names, blocks) = counted(None);
        let head = blocks[1];
        let term = func.terminator(head).expect("the header branches");
        for _ in 0..extra {
            let filler = Builder::new(&mut func, head).iconst(Type::int(32), 7);
            let Def::Result { inst, .. } = func[filler].def else { unreachable!("an iconst") };
            func.remove_inst(inst);
            func.insert_before(inst, term);
        }
        func
    }

    #[test]
    fn fuel_stops_the_copy_where_it_stands() {
        let (mut func, _names, _) = counted(None);
        let mut an = crate::machine::fixtures::analyses();
        Canon.run(&mut func, &mut an, &mut Fuel::unlimited());

        let stats = SPEED.run(&mut func, &mut an, &mut Fuel::of(0));
        assert_eq!(stats.count(Kind::Optimized, super::COPIED), 0);
        assert_eq!(stats.count(Kind::Missed, super::NO_FUEL), 1);
        assert!(tests_at_the_top(&func), "and the loop is as it was");
    }

    #[test]
    fn a_value_the_header_defines_and_the_code_after_the_loop_reads_is_declined() {
        // Straight to the copy, so loop-closed form has not been established and the counter the
        // return names is still the header's own definition. That is the one value this pass will
        // not merge, and section 26.7's answer is that [`Canon`] has already routed it by the time
        // the pipeline gets here.
        let (mut func, _names, _) = counted(None);

        let stats =
            SPEED.run(&mut func, &mut crate::machine::fixtures::analyses(), &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Optimized, super::COPIED), 0);
        assert_eq!(stats.count(Kind::Missed, super::ESCAPES), 1);
        assert!(tests_at_the_top(&func), "and the loop is as it was");
    }

    #[test]
    fn a_loop_with_no_preheader_is_declined() {
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::int(1), Type::int(32)]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let one = func.create_block();
        let two = func.create_block();
        let head = func.create_block();
        let body = func.create_block();
        let done = func.create_block();
        let c = func.append_param(entry, Type::int(1));
        let limit = func.append_param(entry, Type::int(32));
        let i = func.append_param(head, Type::int(32));
        Builder::new(&mut func, entry).br_if(c, one, &[], two, &[]);
        let zero = Builder::new(&mut func, one).iconst(Type::int(32), 0);
        Builder::new(&mut func, one).jump(head, &[zero]);
        let start = Builder::new(&mut func, two).iconst(Type::int(32), 1);
        Builder::new(&mut func, two).jump(head, &[start]);
        let test = Builder::new(&mut func, head).icmp(IntPred::Slt, i, limit);
        Builder::new(&mut func, head).br_if(test, body, &[], done, &[]);
        Builder::new(&mut func, body).jump(head, &[i]);
        Builder::new(&mut func, done).ret(&[]);

        let stats =
            SPEED.run(&mut func, &mut crate::machine::fixtures::analyses(), &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Optimized, super::COPIED), 0);
        assert_eq!(stats.count(Kind::Missed, super::NO_PREHEADER), 1);

        // And with one, which is what the pipeline hands it, the same loop is copied.
        let stats = copied(&mut func, &SPEED);
        assert_eq!(stats.count(Kind::Optimized, super::COPIED), 1);
        sound(&func, &mut names);
    }

    /// Two counted loops one after the other, which share no block and no preheader.
    ///
    /// ```text
    /// entry(n): jump one(0)
    /// one(i):  t = i < n; br t -> up, mid
    /// up:      i2 = i + 1; jump one(i2)
    /// mid:     jump two(0)
    /// two(j):  u = j < n; br u -> down, done
    /// down:    j2 = j + 1; jump two(j2)
    /// done:    ret
    /// ```
    fn side_by_side() -> (Func, Interner, Vec<Block>) {
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::int(32)]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let one = func.create_block();
        let up = func.create_block();
        let mid = func.create_block();
        let two = func.create_block();
        let down = func.create_block();
        let done = func.create_block();
        let n = func.append_param(entry, Type::int(32));
        let i = func.append_param(one, Type::int(32));
        let j = func.append_param(two, Type::int(32));
        let zero = Builder::new(&mut func, entry).iconst(Type::int(32), 0);
        Builder::new(&mut func, entry).jump(one, &[zero]);
        let t = Builder::new(&mut func, one).icmp(IntPred::Slt, i, n);
        Builder::new(&mut func, one).br_if(t, up, &[], mid, &[]);
        let step = Builder::new(&mut func, up).iconst(Type::int(32), 1);
        let next = Builder::new(&mut func, up).binary(Opcode::Add, i, step, Flags::NONE);
        Builder::new(&mut func, up).jump(one, &[next]);
        let start = Builder::new(&mut func, mid).iconst(Type::int(32), 0);
        Builder::new(&mut func, mid).jump(two, &[start]);
        let u = Builder::new(&mut func, two).icmp(IntPred::Slt, j, n);
        Builder::new(&mut func, two).br_if(u, down, &[], done, &[]);
        let stride = Builder::new(&mut func, down).iconst(Type::int(32), 1);
        let after = Builder::new(&mut func, down).binary(Opcode::Add, j, stride, Flags::NONE);
        Builder::new(&mut func, down).jump(two, &[after]);
        Builder::new(&mut func, done).ret(&[]);
        (func, names, vec![up, down])
    }

    #[test]
    fn two_loops_that_do_not_meet_are_both_copied() {
        let (mut func, mut names, bodies) = side_by_side();

        let stats = copied(&mut func, &SPEED);
        assert_eq!(stats.count(Kind::Optimized, super::COPIED), 2);
        sound(&func, &mut names);

        let (_cfg, _dom, loops) = forest(&func);
        assert_eq!(loops.count(), 2, "both loops are still loops");
        for id in loops.all() {
            let header = loops.header(id);
            assert!(
                !Cfg::new(&func).successors(header).iter().any(|&to| !loops.contains(id, to)),
                "and neither of them tests at the top any more"
            );
        }
        for body in bodies {
            assert_eq!(func[body].params.len(), 1, "each body carries its own counter");
        }
    }

    /// A counted loop with a counted loop inside it, where the inner preheader is an outer block.
    ///
    /// ```text
    /// entry(n): jump outer(0)
    /// outer(i): t = i < n; br t -> ahead, done
    /// ahead:    jump inner(0)
    /// inner(j): u = j < n; br u -> under, latch
    /// under:    j2 = j + 1; jump inner(j2)
    /// latch:    i2 = i + 1; jump outer(i2)
    /// done:     ret
    /// ```
    fn nested() -> (Func, Interner) {
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::int(32)]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let outer = func.create_block();
        let ahead = func.create_block();
        let inner = func.create_block();
        let under = func.create_block();
        let latch = func.create_block();
        let done = func.create_block();
        let n = func.append_param(entry, Type::int(32));
        let i = func.append_param(outer, Type::int(32));
        let j = func.append_param(inner, Type::int(32));
        let zero = Builder::new(&mut func, entry).iconst(Type::int(32), 0);
        Builder::new(&mut func, entry).jump(outer, &[zero]);
        let t = Builder::new(&mut func, outer).icmp(IntPred::Slt, i, n);
        Builder::new(&mut func, outer).br_if(t, ahead, &[], done, &[]);
        let start = Builder::new(&mut func, ahead).iconst(Type::int(32), 0);
        Builder::new(&mut func, ahead).jump(inner, &[start]);
        let u = Builder::new(&mut func, inner).icmp(IntPred::Slt, j, n);
        Builder::new(&mut func, inner).br_if(u, under, &[], latch, &[]);
        let stride = Builder::new(&mut func, under).iconst(Type::int(32), 1);
        let after = Builder::new(&mut func, under).binary(Opcode::Add, j, stride, Flags::NONE);
        Builder::new(&mut func, under).jump(inner, &[after]);
        let step = Builder::new(&mut func, latch).iconst(Type::int(32), 1);
        let next = Builder::new(&mut func, latch).binary(Opcode::Add, i, step, Flags::NONE);
        Builder::new(&mut func, latch).jump(outer, &[next]);
        Builder::new(&mut func, done).ret(&[]);
        (func, names)
    }

    #[test]
    fn a_loop_and_the_loop_inside_it_are_copied_one_round_apart() {
        // The two share every block the inner one has, and the inner one's preheader is a block of
        // the outer one, so a round may hold at most one of them. Both are still copied, and the
        // point of the test is that the second is planned against a function the first has already
        // changed rather than against the plan the first was made from.
        let (mut func, mut names) = nested();

        let stats = copied(&mut func, &SPEED);
        assert_eq!(stats.count(Kind::Optimized, super::COPIED), 2);
        sound(&func, &mut names);

        let (cfg, _dom, loops) = forest(&func);
        assert_eq!(loops.count(), 2, "both loops survived the copy");
        for id in loops.all() {
            let header = loops.header(id);
            assert!(
                !cfg.successors(header).iter().any(|&to| !loops.contains(id, to)),
                "and both test at the bottom now"
            );
        }
    }

    #[test]
    fn a_round_stops_where_the_fuel_does() {
        // Two loops a round may hold together, and one unit of fuel. The first is copied and the
        // second is left for a run with more, which is what taking fuel per job rather than per
        // round means.
        let (mut func, mut names) = {
            let (mut func, names, _) = side_by_side();
            let mut an = crate::machine::fixtures::analyses();
            Canon.run(&mut func, &mut an, &mut Fuel::unlimited());
            (func, names)
        };

        let stats =
            SPEED.run(&mut func, &mut crate::machine::fixtures::analyses(), &mut Fuel::of(1));
        assert_eq!(stats.count(Kind::Optimized, super::COPIED), 1);
        assert_eq!(stats.count(Kind::Missed, super::NO_FUEL), 1);
        sound(&func, &mut names);
    }
}
