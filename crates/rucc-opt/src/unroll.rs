//! Copies a loop that runs a known small number of times out into a straight line.
//!
//! Design: `spec/optimizer/29-unrolling-and-peeling.md`, with 29.2 for the cost model, 29.5 for
//! where it sits and 29.7 for the ways it goes wrong.
//!
//! This is complete unrolling and nothing else. A loop that runs three times becomes three copies
//! of its body with no branch back, so the counter is a literal in each copy, the addresses fold,
//! and everything after this gets straight line code where it had a loop. Partial unrolling, where
//! a loop that runs an unknown number of times gets a body of four iterations and a remainder, is
//! not here: GCC has it off at every level including `-O3`, section 29.1 says the reason is that it
//! costs code and wins nothing without the vectorizer behind it, and rucc has no vectorizer yet.
//! Peeling is not here either, for the same reason plus that it needs a profile to be worth
//! anything.
//!
//! # Which loops
//!
//! One latch, one exit, and the exit is a two way branch with a block inside the loop on one side
//! and a block outside on the other. The branch does not have to be in the latch. [`crate::canon`]
//! usually leaves the test in the header and gives the loop a dedicated latch below it that only
//! jumps back, so requiring the two to be the same block would refuse most of what this pass is
//! meant to see. What is required instead is that the block holding the test dominates the latch,
//! which is what says the test runs on every iteration and so what makes the count a count of this
//! loop's iterations. When the two are different blocks the latch has to end in a plain jump, since
//! anything else there is a second way round the loop or a second way out of it.
//!
//! The single exit is a real restriction and section 29.7 does not ask for it. A loop with several
//! ways out could be unrolled too, because every copy keeps all of its own exits and leaving early
//! still leaves early. What stops it is where the trip count comes from.
//! [`crate::scev::Scev::bound`] takes the first exit it can solve, and a count taken from an exit
//! that does not run on every iteration is not a bound on the loop at all, because control can skip
//! the test and go round again. A [`crate::scev::Bound`] does not say which exit it came from, so
//! there is nothing here to check. With one exit there is nothing to ask.
//!
//! # How many copies, which is section 29.7's off by one
//!
//! The count is the iteration at which the exit test first fails, counting the first time round as
//! iteration zero. So the back edge is taken exactly that many times and the header is entered one
//! more time than that. The number of copies is the number of times the header is entered, which is
//! the count plus one, and getting this wrong in either direction is a program that runs its body
//! the wrong number of times.
//!
//! The count has to be a number rather than an expression, and it has to rest on nothing beyond
//! signed overflow being undefined, which is [`crate::scev::Bound::under_undefined_overflow`]
//! returning [`crate::scev::Count::Exact`]. An estimate is for deciding whether a transformation
//! pays and this one is deciding what the program does, which is document 7.5's whole reason for
//! making the two different types.
//!
//! No copy keeps its exit test. The test in every copy but the last is known to hold, because the
//! loop is single exit and the count is the iteration that test first fails on, so what goes in its
//! place is a jump to whichever of its two sides stays in the loop. The last one's is known to
//! fail, so what goes in its place is a jump out. When the test is above the latch, that leaves the
//! last copy's latch and whatever sat between the two with nothing reaching them, so the pass
//! sweeps them before it is finished. Section 6.5 makes that the job of whichever pass stranded a
//! block rather than of the sweeper that comes after, and the verifier holds every pass to it.
//!
//! Leaving the tests in and letting a later pass fold them would be the more forgiving thing to
//! build, and it is not built, for two reasons. Nothing after this folds an `icmp` of two
//! constants, which is issue 352 and is deliberate, so the tests would still be there at the end
//! and the exit block would have one predecessor per copy: a loop unrolled into something larger
//! and branchier than the loop was. And forgiving is only half true anyway, since a count that came
//! out too small would truncate the loop whether the tests are there or not. Being right about the
//! count is the requirement either way, so the code says so.
//!
//! # What the copies cost
//!
//! Three limits, all GCC's. [`heuristics::UNROLL_MAX_TIMES`] is a bound on the copies,
//! [`heuristics::UNROLL_MAX_INSNS`] on the code they add up to, and
//! [`heuristics::UNROLL_MAX_DEPTH`] on how deeply the loop is nested, because the cost of unrolling
//! an inner loop is paid once per iteration of everything outside it.
//!
//! The instruction limit is measured after the folding rather than before it. An instruction whose
//! operands are all numbers once the counter is one is not counted, because it will not be there
//! when this is over, and neither is a branch whose condition is one of those. What is counted is
//! the rest, times the number of copies. Two things make that an over estimate: an instruction
//! whose operands become numbers only after something upstream of it folds is only caught when the
//! upstream instruction comes first in the block order, and an address computation that folds into
//! an addressing mode is counted as surviving because nothing here knows about addressing modes.
//! Over estimating means refusing loops GCC would take, which is the direction to be wrong in.
//!
//! Innermost loops go first, so an inner loop is unrolled before the loop around it is measured and
//! the outer one is priced against what the inner one actually became. That is also what stops the
//! nesting from multiplying out: after the inner loop is unrolled the outer body is that much
//! larger, and the instruction limit is what it runs into.
//!
//! # Which level
//!
//! `-O2` and `-O3`. Not `-Os` or `-Oz`, because the copies are code and the whole point of them is
//! speed. Section 29.6 says a trip count of two or three is worth unrolling even for size, since
//! the loop overhead that goes away is comparable to the body that arrives, and that is recorded
//! rather than built: it is a second threshold and a second pass name for a case the corpus has not
//! asked for yet.

use std::collections::{HashMap, HashSet};

use rucc_cost::heuristics;
use rucc_ir::{
    Block, BlockCall, Builder, Extra, ExtraKind, Func, Inst, InstData, Opcode, Type, Value,
    ValueList,
};

use crate::cfg::Cfg;
use crate::dom::Dominators;
use crate::loops::{LoopId, Loops};
use crate::scev::{Bound, Count, Evolution, Scev};
use crate::{Analyses, Fuel, Pass, Preserved, Stats};

const UNROLLED: &str = "loop copied out into a straight line, it runs a known number of times";
const NO_COUNT: &str = "loop left as it was, how many times it runs is not a number known here";
const TOO_MANY: &str = "loop left as it was, it runs more times than a loop is unrolled away";
const TOO_BIG: &str = "loop left as it was, the copies would be more code than the limit allows";
const TOO_DEEP: &str = "loop left as it was, it is nested deeper than an unrolled loop may be";
const SHAPE: &str =
    "loop left as it was, its exit is not a two way branch that runs on every iteration";
const EXITS: &str = "loop left as it was, it leaves from more than one place";
const LATCHES: &str = "loop left as it was, it goes back to its header from more than one place";
const ENTRIES: &str = "loop left as it was, it is reached somewhere other than at its header";
const ESCAPES: &str = "loop left as it was, a value it defines is read outside it";
const PAYLOAD: &str = "loop left as it was, something in it carries a side table this cannot copy";
const NO_FUEL: &str = "loop left as it was, the pass ran out of fuel";

/// Section 29's complete unrolling.
#[derive(Debug)]
pub struct Unroll;

impl Pass for Unroll {
    fn name(&self) -> &'static str {
        "unroll"
    }

    fn describe(&self) -> &'static str {
        "copies a loop that runs a known small number of times out into a straight line"
    }

    fn preserves(&self) -> Preserved {
        // Blocks appear, the back edge goes away and the loop with it.
        Preserved::NONE
    }

    fn run(&self, func: &mut Func, an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        if func.entry().is_none() {
            return stats;
        }
        let mut done: HashSet<Block> = HashSet::new();
        let mut say = true;
        while let Some(job) = plan(func, an, &done, &mut stats, say) {
            say = false;
            if !fuel.take() {
                stats.missed(NO_FUEL);
                break;
            }
            done.insert(job.header);
            apply(func, &job);
            stats.optimized(UNROLLED);
            an.clear();
            // The last copy's test was settled the way that leaves, so anything that only ran on
            // the other side of it is stranded. Section 6.5 puts taking it out on whichever pass
            // stranded it rather than on the sweeper that comes later.
            crate::simplify_cfg::sweep(func, an, &mut stats);
        }
        an.clear();
        stats
    }
}

/// One loop to unroll, worked out against the function as it stands.
#[derive(Debug)]
struct Job {
    /// The block the loop is entered at, which is where each copy starts.
    header: Block,
    /// The one block with an edge back to the header, which is where each copy ends.
    latch: Block,
    /// The block holding the exit test, which dominates the latch and is often the header.
    from: Block,
    /// The side of that test that stays in the loop, which every copy but the last takes.
    stay: Block,
    /// Every block of the loop, the blocks of the loops nested in it included.
    blocks: Vec<Block>,
    /// The other side of that test, which is where the last copy goes.
    exit: Block,
    /// How many times the header is entered, which is how many copies there are.
    times: u32,
    /// How deeply the loop is nested, which is how the innermost one is picked.
    depth: u32,
}

/// The innermost loop worth unrolling, and what it would take.
///
/// One at a time, because unrolling one loop invalidates the forest the next answer would be read
/// out of. `say` is false on every call after the first so that a loop this declines is declined
/// once rather than once per round.
fn plan(
    func: &Func,
    an: &mut Analyses,
    done: &HashSet<Block>,
    stats: &mut Stats,
    say: bool,
) -> Option<Job> {
    let cfg = an.cfg(func).clone();
    let doms = an.dominators(func).clone();
    let loops = an.loops(func).clone();
    let mut scev = Scev::new(func, &cfg, &loops);
    let mut found: Option<Job> = None;
    for id in loops.all() {
        if done.contains(&loops.header(id)) {
            continue;
        }
        match consider(func, &cfg, &doms, &loops, &mut scev, id) {
            Ok(job) => {
                if found.as_ref().is_none_or(|had| job.depth > had.depth) {
                    found = Some(job);
                }
            }
            Err(why) if say => stats.missed(why),
            Err(_) => (),
        }
    }
    found
}

/// Whether this loop can be unrolled away, and why not when it cannot.
fn consider(
    func: &Func,
    cfg: &Cfg,
    doms: &Dominators,
    loops: &Loops,
    scev: &mut Scev<'_>,
    id: LoopId,
) -> Result<Job, &'static str> {
    let header = loops.header(id);
    if loops.depth(id) > heuristics::UNROLL_MAX_DEPTH {
        return Err(TOO_DEEP);
    }
    let [latch] = loops.latches(id) else {
        return Err(LATCHES);
    };
    let [only] = loops.exits(id) else {
        return Err(EXITS);
    };
    // The test has to run on every iteration or the count is not a count of this loop's iterations,
    // and dominating the latch is what says it does. Being the latch is the special case of that.
    if !doms.dominates(only.from, *latch) {
        return Err(SHAPE);
    }
    let term = func.terminator(only.from).ok_or(SHAPE)?;
    if func[term].opcode != Opcode::BrIf {
        return Err(SHAPE);
    }
    let calls: Vec<BlockCall> = func.successors(term).collect();
    let [then_call, else_call] = calls[..].try_into().map_err(|_| SHAPE)?;
    let in_loop = |call: &BlockCall| loops.contains(id, call.block);
    let (stay, exit) = match (in_loop(&then_call), in_loop(&else_call)) {
        (true, false) => (then_call.block, else_call.block),
        (false, true) => (else_call.block, then_call.block),
        _ => return Err(SHAPE),
    };
    if exit != only.to {
        return Err(SHAPE);
    }
    if only.from != *latch {
        // Anything but a plain jump back there is a second way round the loop or a second way out
        // of it, and the one latch and the one exit have already been counted.
        let back = func.terminator(*latch).ok_or(SHAPE)?;
        if func[back].opcode != Opcode::Jump {
            return Err(SHAPE);
        }
    }

    // Section 29.7's off by one. The count is the iteration the test first fails on, so it is how
    // often the back edge is taken and one less than how often the header is entered.
    let count = match scev.bound(id).as_ref().and_then(Bound::under_undefined_overflow) {
        Some(Count::Exact(count)) => count,
        _ => return Err(NO_COUNT),
    };
    let times = count.checked_add(1).and_then(|times| u32::try_from(times).ok()).ok_or(TOO_MANY)?;
    if times > heuristics::UNROLL_MAX_TIMES {
        return Err(TOO_MANY);
    }

    let blocks = loops.blocks(id).to_vec();
    let inside: HashSet<Block> = blocks.iter().copied().collect();
    for &block in &blocks {
        // The copy reaches every block of the loop from the copied header, so a block reached from
        // anywhere else would be reached from the original in the copy and from the copy in the
        // original. A natural loop has no such block and an irreducible one does.
        if block != header && cfg.predecessors(block).iter().any(|at| !inside.contains(at)) {
            return Err(ENTRIES);
        }
        for inst in func.insts(block) {
            if !copyable(func, inst) {
                return Err(PAYLOAD);
            }
        }
    }
    if escapes(func, &blocks, &inside) {
        return Err(ESCAPES);
    }
    let size = size_after(func, scev, id, &blocks, times).ok_or(TOO_BIG)?;
    if size > heuristics::UNROLL_MAX_INSNS {
        return Err(TOO_BIG);
    }
    Ok(Job {
        header,
        latch: *latch,
        from: only.from,
        stay,
        blocks,
        exit,
        times,
        depth: loops.depth(id),
    })
}

/// Whether an instruction can be copied by copying what it carries.
///
/// Most of the side tables are written once and read for ever, so an index into one means the same
/// thing in a copy as it does in the original. The ones that are not are the ones holding branch
/// targets, which a copy has to remap and this does not reach into, and the varargs table, whose
/// entries describe a walk over an argument list that nobody here has thought about copying.
fn copyable(func: &Func, inst: Inst) -> bool {
    !matches!(func[inst].extra.kind(), ExtraKind::Switch | ExtraKind::Asm | ExtraKind::VaObject)
}

/// Whether anything outside the loop reads a value defined inside it.
///
/// After [`crate::canon`] there is no such value, because loop closed form has already routed every
/// one of them through a parameter of the block the loop leaves to. Where there is one, the copies
/// would leave it reading the first iteration's value instead of the last, so this is refused
/// rather than repaired.
fn escapes(func: &Func, blocks: &[Block], inside: &HashSet<Block>) -> bool {
    let mut defined: HashSet<Value> = HashSet::new();
    for &block in blocks {
        defined.extend(func[block].params.iter().copied());
        for inst in func.insts(block) {
            defined.extend(func[inst].results());
        }
    }
    for block in func.blocks() {
        if inside.contains(&block) {
            continue;
        }
        for inst in func.insts(block) {
            if func[func[inst].args].iter().any(|value| defined.contains(value)) {
                return true;
            }
            for call in func.successors(inst) {
                if func[call.args].iter().any(|value| defined.contains(value)) {
                    return true;
                }
            }
        }
    }
    false
}

/// How many instructions the copies come to once the folding the unroll enables has happened.
///
/// `None` when the count does not fit, which is a loop too large to be worth measuring precisely.
fn size_after(
    func: &Func,
    scev: &mut Scev<'_>,
    id: LoopId,
    blocks: &[Block],
    times: u32,
) -> Option<u32> {
    let mut settled: HashSet<Value> = HashSet::new();
    let mut survives = 0u32;
    for &block in blocks {
        for inst in func.insts(block) {
            let data = func[inst];
            let known = func[data.args]
                .iter()
                .all(|arg| settled.contains(arg) || is_number(scev, id, *arg));
            // A terminator is not deletable and is still free when its condition is a number,
            // because the edge it does not take goes away with it. A plain jump has no condition
            // and is free for the same reason: it joins two blocks that follow each other.
            if known && (func.is_terminator(inst) || !data.opcode.has_effects()) {
                settled.extend(data.results());
                continue;
            }
            survives = survives.checked_add(1)?;
        }
    }
    survives.checked_mul(times)
}

/// Whether this value is a number in every copy once the counter is one.
fn is_number(scev: &mut Scev<'_>, id: LoopId, value: Value) -> bool {
    match scev.evolution(id, value) {
        Evolution::Invariant(inv) => inv.as_number().is_some(),
        Evolution::Affine(chrec) => {
            chrec.base.as_number().is_some() && chrec.step.as_number().is_some()
        }
        Evolution::Unknown => false,
    }
}

/// Makes the copies and takes the loop out.
///
/// The original blocks are the first copy, so nothing is left behind for the sweeper to pick up.
/// The exit test goes first, before anything is copied, together with the latch's jump back when
/// those are two different instructions, because both of them are about to be replaced in every
/// copy and neither should be cloned.
///
/// Then the bodies, in order, each one's substitution seeded from the one before it, and only after
/// all of them are there do the terminators go in. Doing it in that order is what lets a copy point
/// at the next copy's header, which does not exist yet while the copy is being made.
///
/// A copy is gone over once more when it is finished, because the order the blocks come in says
/// nothing about which of them makes a value and which of them reads it, and a read copied before
/// its definition was copied is a read of the original.
///
/// What each round is written in terms of is a snapshot of the original blocks taken before any of
/// this, because these are blocks this edits and a round that read one live would copy an
/// instruction a round before it wrote.
fn apply(func: &mut Func, job: &Job) {
    let term = func.terminator(job.from).expect("the plan read this terminator");
    let staying = edge_args(func, term, job.stay);
    let out = edge_args(func, term, job.exit);
    // Where the header's parameters come from next time round, which is the jump back when there is
    // a separate latch and the staying side of the test when the latch is where the test is.
    let round_trip = if job.from == job.latch {
        None
    } else {
        Some(func.terminator(job.latch).expect("the plan read this terminator too"))
    };
    let back = edge_args(func, round_trip.unwrap_or(term), job.header);
    let params: Vec<Value> = func[job.header].params.clone();
    func.remove_inst(term);
    if let Some(round_trip) = round_trip {
        func.remove_inst(round_trip);
    }

    let body: Vec<(Block, Vec<Inst>)> =
        job.blocks.iter().map(|&block| (block, func.insts(block).collect())).collect();
    // One entry per copy, the first being the original blocks standing for themselves under a
    // substitution that renames nothing.
    let mut copies: Vec<HashMap<Block, Block>> =
        vec![job.blocks.iter().map(|&block| (block, block)).collect()];
    let mut subs: Vec<HashMap<Value, Value>> = vec![HashMap::new()];

    for round in 1..job.times as usize {
        let mut blocks: HashMap<Block, Block> = HashMap::new();
        for &(block, _) in &body {
            blocks.insert(block, func.create_block());
        }
        // The header's parameters stand for whatever the one edge in hands them, so each copy is
        // written in terms of the previous copy's arguments and needs no parameters of its own.
        // Always the original arguments read through the previous copy's substitution, never the
        // previous copy's read through it again, which would be one round's worth of renaming
        // applied on top of another's.
        let mut map: HashMap<Value, Value> = HashMap::new();
        for (&param, arg) in params.iter().zip(&back) {
            map.insert(param, subs[round - 1].get(arg).copied().unwrap_or(*arg));
        }
        for &(block, _) in &body {
            if block == job.header {
                continue;
            }
            let copy = blocks[&block];
            for param in func[block].params.clone() {
                let fresh = func.append_param(copy, func[param].ty);
                map.insert(param, fresh);
            }
        }
        let mut copied: Vec<Inst> = Vec::new();
        for (block, insts) in &body {
            let into = blocks[block];
            for &inst in insts {
                copied.push(clone_into(func, into, inst, &mut map, &blocks));
            }
        }
        // Every value the copy makes has a name by now, which is what this waited for: the block
        // list says nothing about which block makes a value and which one reads it, so an operand
        // settled while the copy was being made would sometimes have been settled too early and
        // kept a name that does not reach it. Once each, over the original operands the copies
        // still hold, is also what keeps this right where a header parameter stands for a value
        // that is itself renamed further along.
        for inst in copied {
            let args = func[inst].args;
            func.rewrite(args, |value| map.get(&value).copied().unwrap_or(value));
            let edges: Vec<ValueList> = func.successors(inst).map(|call| call.args).collect();
            for edge in edges {
                func.rewrite(edge, |value| map.get(&value).copied().unwrap_or(value));
            }
        }
        copies.push(blocks);
        subs.push(map);
    }

    for round in 0..job.times as usize {
        let map = &subs[round];
        let at = |value: &Value| map.get(value).copied().unwrap_or(*value);
        let from = copies[round][&job.from];
        let latch = copies[round][&job.latch];
        if round + 1 == job.times as usize {
            let leaving: Vec<Value> = out.iter().map(at).collect();
            Builder::new(func, from).jump(job.exit, &leaving);
            if latch != from {
                // The test above it has just been settled the other way, so nothing reaches it.
                Builder::new(func, latch).unreachable();
            }
        } else if from == latch {
            Builder::new(func, from).jump(copies[round + 1][&job.header], &[]);
        } else {
            let carried: Vec<Value> = staying.iter().map(at).collect();
            Builder::new(func, from).jump(copies[round][&job.stay], &carried);
            Builder::new(func, latch).jump(copies[round + 1][&job.header], &[]);
        }
    }
}

/// The arguments a terminator hands the target it shares with this block.
fn edge_args(func: &Func, term: Inst, to: Block) -> Vec<Value> {
    for call in func.successors(term) {
        if call.block == to {
            return func[call.args].to_vec();
        }
    }
    Vec::new()
}

/// Copies one instruction to the end of a block, records its results, and remaps where it branches.
///
/// What it reads is left exactly as the original read it, for its caller to settle once the whole
/// copy is there. A target inside the loop becomes the copy's own block, and a target outside it
/// stays where it is. The header is not a case here: the one edge that goes to it is the back edge,
/// which is the latch's, and the latch's test was taken out before any of this started.
fn clone_into(
    func: &mut Func,
    into: Block,
    inst: Inst,
    map: &mut HashMap<Value, Value>,
    blocks: &HashMap<Block, Block>,
) -> Inst {
    let data = func[inst];
    let args: Vec<Value> = func[data.args].to_vec();
    let edges: Vec<(Block, Vec<Value>)> = func
        .successors(inst)
        .map(|call| {
            let block = blocks.get(&call.block).copied().unwrap_or(call.block);
            (block, func[call.args].to_vec())
        })
        .collect();
    let extra = match data.extra {
        Extra::Targets(_) => {
            let calls: Vec<BlockCall> = edges
                .iter()
                .map(|(block, args)| BlockCall { block: *block, args: func.push_values(args) })
                .collect();
            Extra::Targets(func.push_block_calls(&calls))
        }
        // Anything else names no block, a return and an unreachable among them.
        other => other,
    };
    let types: Vec<Type> = data.results().map(|result| func[result].ty).collect();
    let span = func.span(inst);
    let args = func.push_values(&args);
    let fresh = func.create_inst(InstData { args, extra, ..data }, &types, span);
    func.append_inst(into, fresh);
    for (old, new) in data.results().zip(func[fresh].results()) {
        map.insert(old, new);
    }
    fresh
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{
        Block, Builder, Flags, Func, IntPred, Module, Opcode, Signature, Type, Value, verify_func,
    };
    use rucc_target::{TargetInfo, Triple};

    use super::{ESCAPES, EXITS, NO_COUNT, NO_FUEL, TOO_BIG, TOO_MANY, UNROLLED, Unroll};
    use crate::stats::Kind;
    use crate::{Fuel, Pass, Stats};

    /// Runs the pass over the function as it stands.
    fn unroll(func: &mut Func, fuel: &mut Fuel) -> Stats {
        Unroll.run(func, &mut crate::machine::fixtures::analyses(), fuel)
    }

    /// Insists the function is one the rest of the compiler may believe.
    ///
    /// Copying blocks is the edit that breaks a definition's dominance over its uses and leaves a
    /// branch handing a block the wrong number of arguments, so this is where most of the strength
    /// of these tests is.
    fn sound(func: &Func, names: &mut Interner) {
        let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().unwrap());
        let module = Module::new(names.intern("t.c"), &target);
        if let Err(errors) = verify_func(&module, func, names) {
            panic!("{errors:#?}");
        }
    }

    /// How many instructions of that opcode the whole function holds.
    fn tally(func: &Func, opcode: Opcode) -> usize {
        func.blocks()
            .flat_map(|block| func.insts(block))
            .filter(|&inst| func[inst].opcode == opcode)
            .count()
    }

    /// A loop that tests at the bottom and runs a known number of times.
    ///
    /// ```text
    /// entry: jump head(0, 0)
    /// head(i, sum): jump body(i, sum)
    /// body(i, sum): total = sum + i; next = i + 1; t = next < limit
    ///               br t -> head(next, total), done(total)
    /// done(x): ret x
    /// ```
    ///
    /// Two blocks rather than one, so that copying has a block map to get wrong, and the sum is
    /// carried round so that copying has a substitution to get wrong. This is the shape
    /// `crate::canon` and `crate::header_copy` leave a `for` loop in.
    struct Rotated {
        names: Interner,
        func: Func,
        entry: Block,
        head: Block,
        body: Block,
        done: Block,
        test: Value,
    }

    fn rotated(limit: i128) -> Rotated {
        let mut names = Interner::new();
        let signature = Signature::new().with_returns(&[Type::int(32)]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let head = func.create_block();
        let body = func.create_block();
        let done = func.create_block();
        let i = func.append_param(head, Type::int(32));
        let sum = func.append_param(head, Type::int(32));
        let carried = func.append_param(body, Type::int(32));
        let running = func.append_param(body, Type::int(32));
        let answer = func.append_param(done, Type::int(32));

        let mut build = Builder::new(&mut func, entry);
        let zero = build.iconst(Type::int(32), 0);
        build.jump(head, &[zero, zero]);
        Builder::new(&mut func, head).jump(body, &[i, sum]);

        let mut build = Builder::new(&mut func, body);
        let total = build.binary(Opcode::Add, running, carried, Flags::NSW);
        let one = build.iconst(Type::int(32), 1);
        let next = build.binary(Opcode::Add, carried, one, Flags::NSW);
        let stop = build.iconst(Type::int(32), limit);
        let test = build.icmp(IntPred::Slt, next, stop);
        build.br_if(test, head, &[next, total], done, &[total]);
        Builder::new(&mut func, done).ret(&[answer]);
        Rotated { names, func, entry, head, body, done, test }
    }

    #[test]
    fn a_loop_that_runs_three_times_becomes_three_copies() {
        let mut it = rotated(3);
        let stats = unroll(&mut it.func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Optimized, UNROLLED), 1);
        // Two adds in the body, one per copy, and no branch left that could go round again.
        assert_eq!(tally(&it.func, Opcode::Add), 6);
        assert_eq!(tally(&it.func, Opcode::BrIf), 0);
        sound(&it.func, &mut it.names);
    }

    #[test]
    fn a_loop_that_runs_once_keeps_its_body_and_loses_its_back_edge() {
        let mut it = rotated(1);
        let stats = unroll(&mut it.func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Optimized, UNROLLED), 1);
        assert_eq!(tally(&it.func, Opcode::Add), 2, "the body is not copied and not removed");
        assert_eq!(tally(&it.func, Opcode::BrIf), 0);
        assert_eq!(it.func.blocks().count(), 4, "no block is added and none goes away");
        sound(&it.func, &mut it.names);
    }

    #[test]
    fn the_loop_is_gone_afterwards() {
        let mut it = rotated(4);
        unroll(&mut it.func, &mut Fuel::unlimited());
        let cfg = crate::cfg::Cfg::new(&it.func);
        let doms = crate::dom::Dominators::new(&cfg);
        assert_eq!(crate::loops::Loops::new(&cfg, &doms).count(), 0);
        sound(&it.func, &mut it.names);
    }

    #[test]
    fn the_value_the_loop_carries_out_comes_from_the_last_copy() {
        let mut it = rotated(3);
        unroll(&mut it.func, &mut Fuel::unlimited());
        // Whatever block now jumps to the exit is the last copy, and the value it hands over has to
        // be the one worked out in that same block rather than in the one the loop started as.
        let term = it
            .func
            .blocks()
            .filter_map(|block| it.func.terminator(block))
            .find(|&term| it.func.successors(term).any(|call| call.block == it.done))
            .expect("something reaches the exit");
        let last = it.func.block_of(term).expect("it is in a block");
        let handed = it.func.successors(term).next().expect("the jump hands the sum over").args;
        let value = it.func[handed][0];
        let rucc_ir::Def::Result { inst, .. } = it.func[value].def else {
            panic!("the sum is worked out rather than handed in");
        };
        assert_eq!(it.func.block_of(inst), Some(last));
        assert_ne!(last, it.body, "the first copy is not the one that leaves");
        sound(&it.func, &mut it.names);
    }

    /// The same loop with its test at the top and a latch of its own underneath.
    ///
    /// ```text
    /// entry: jump head(0, 0)
    /// head(i, sum): t = i < limit; br t -> body(i, sum), done(sum)
    /// body(c, r): total = r + c; next = c + 1; jump latch(next, total)
    /// latch(n, t): jump head(n, t)
    /// done(x): ret x
    /// ```
    ///
    /// This is what `crate::canon` actually leaves behind most of the time: the block that decides
    /// and the block that goes round are two different blocks, and the deciding one is the header.
    /// The header is entered one more time than the body runs, so there is one copy on the end
    /// whose body nothing reaches.
    fn top_tested(limit: i128) -> Rotated {
        let mut names = Interner::new();
        let signature = Signature::new().with_returns(&[Type::int(32)]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let head = func.create_block();
        let body = func.create_block();
        let latch = func.create_block();
        let done = func.create_block();
        let i = func.append_param(head, Type::int(32));
        let sum = func.append_param(head, Type::int(32));
        let carried = func.append_param(body, Type::int(32));
        let running = func.append_param(body, Type::int(32));
        let round = func.append_param(latch, Type::int(32));
        let tally = func.append_param(latch, Type::int(32));
        let answer = func.append_param(done, Type::int(32));

        let mut build = Builder::new(&mut func, entry);
        let zero = build.iconst(Type::int(32), 0);
        build.jump(head, &[zero, zero]);

        let mut build = Builder::new(&mut func, head);
        let stop = build.iconst(Type::int(32), limit);
        let test = build.icmp(IntPred::Slt, i, stop);
        build.br_if(test, body, &[i, sum], done, &[sum]);

        let mut build = Builder::new(&mut func, body);
        let total = build.binary(Opcode::Add, running, carried, Flags::NSW);
        let one = build.iconst(Type::int(32), 1);
        let next = build.binary(Opcode::Add, carried, one, Flags::NSW);
        build.jump(latch, &[next, total]);

        Builder::new(&mut func, latch).jump(head, &[round, tally]);
        Builder::new(&mut func, done).ret(&[answer]);
        Rotated { names, func, entry, head, body, done, test }
    }

    #[test]
    fn a_loop_whose_test_is_above_its_latch_is_unrolled_too() {
        let mut it = top_tested(3);
        let stats = unroll(&mut it.func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Optimized, UNROLLED), 1);
        assert_eq!(tally(&it.func, Opcode::BrIf), 0);
        // Four copies of the header for three runs of the body, and the fourth body is one nothing
        // reaches, so the pass takes it away again before it is finished.
        assert_eq!(tally(&it.func, Opcode::Add), 6);
        assert_eq!(tally(&it.func, Opcode::Unreachable), 0);
        let cfg = crate::cfg::Cfg::new(&it.func);
        let doms = crate::dom::Dominators::new(&cfg);
        assert_eq!(crate::loops::Loops::new(&cfg, &doms).count(), 0);
        sound(&it.func, &mut it.names);
    }

    #[test]
    fn a_loop_that_runs_more_times_than_the_limit_is_left_alone() {
        let mut it = rotated(100);
        let stats = unroll(&mut it.func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Missed, TOO_MANY), 1);
        assert_eq!(tally(&it.func, Opcode::Add), 2);
        sound(&it.func, &mut it.names);
    }

    #[test]
    fn a_loop_whose_count_is_not_a_number_is_left_alone() {
        let mut it = rotated(3);
        // The test now reads a value handed to the function, so nothing before the loop knows how
        // far it goes.
        let limit = it.func.append_param(it.entry, Type::int(32));
        let compare = match it.func[it.test].def {
            rucc_ir::Def::Result { inst, .. } => inst,
            other => panic!("{other:?} is not something an instruction worked out"),
        };
        let args = it.func[compare].args;
        let counter = it.func[args][0];
        it.func.rewrite(args, |value| if value == counter { value } else { limit });

        let stats = unroll(&mut it.func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Missed, NO_COUNT), 1);
        assert_eq!(tally(&it.func, Opcode::Add), 2);
    }

    #[test]
    fn a_loop_with_a_second_way_out_is_left_alone() {
        let mut it = rotated(3);
        // The head branches out of the loop as well now, which is a `break` in the body.
        let term = it.func.terminator(it.head).expect("the head ends in a jump");
        it.func.remove_inst(term);
        let mut build = Builder::new(&mut it.func, it.head);
        let i = build.func()[it.head].params[0];
        let sum = build.func()[it.head].params[1];
        let seven = build.iconst(Type::int(32), 7);
        let out = build.icmp(IntPred::Slt, i, seven);
        build.br_if(out, it.body, &[i, sum], it.done, &[sum]);

        let stats = unroll(&mut it.func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Missed, EXITS), 1);
        sound(&it.func, &mut it.names);
    }

    #[test]
    fn a_body_the_copies_would_not_fit_is_left_alone() {
        let mut it = rotated(16);
        // Fifteen instructions the counter does not settle, times sixteen copies, is over the two
        // hundred instruction limit.
        let mut build = Builder::new(&mut it.func, it.body);
        let mut running = build.func()[it.body].params[1];
        for _ in 0..15 {
            running = build.binary(Opcode::Mul, running, running, Flags::NONE);
        }
        tucked(&mut it.func, it.body);

        let stats = unroll(&mut it.func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Missed, TOO_BIG), 1);
        sound(&it.func, &mut it.names);
    }

    #[test]
    fn a_value_read_after_the_loop_that_did_not_come_out_of_it_is_left_alone() {
        let mut it = rotated(3);
        // The exit reads the counter directly rather than through its own parameter, which is what
        // a function that has not been put in loop closed form looks like.
        let i = it.func[it.head].params[0];
        let term = it.func.terminator(it.done).expect("the exit returns");
        it.func.remove_inst(term);
        Builder::new(&mut it.func, it.done).ret(&[i]);

        let stats = unroll(&mut it.func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Missed, ESCAPES), 1);
        assert_eq!(tally(&it.func, Opcode::Add), 2);
    }

    /// The outer loop of a nest, whose body is a loop this pass will not touch.
    ///
    /// ```text
    /// entry:         jump outer(0, 0)
    /// outer(x, sum): tx = x < 4; br tx -> inner(0, sum), done(sum)
    /// inner(y, acc): ty = y < 3; br ty -> body, after
    /// body:          part = x * y; run = acc + part; jump step
    /// step:          next = y + 1; jump inner(next, run)
    /// after:         on = x + 1; jump outer(on, acc)
    /// done(r):       ret r
    /// ```
    ///
    /// The inner loop is refused, because `acc` is one of its header's parameters and `after` reads
    /// it from outside. The outer one is taken, and copying it copies the inner one four times.
    ///
    /// What this is here for is `step`, whose jump reads `run` from `body`. The blocks come in
    /// whatever order the loop analysis found them, so this is the case where a copy is written
    /// before the copy of the block it reads from.
    #[test]
    fn a_loop_with_another_loop_inside_it_copies_the_inner_one_correctly() {
        let mut names = Interner::new();
        let signature = Signature::new().with_returns(&[Type::int(32)]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let outer = func.create_block();
        let inner = func.create_block();
        let body = func.create_block();
        let step = func.create_block();
        let after = func.create_block();
        let done = func.create_block();
        let x = func.append_param(outer, Type::int(32));
        let sum = func.append_param(outer, Type::int(32));
        let y = func.append_param(inner, Type::int(32));
        let acc = func.append_param(inner, Type::int(32));
        let answer = func.append_param(done, Type::int(32));

        let mut build = Builder::new(&mut func, entry);
        let zero = build.iconst(Type::int(32), 0);
        build.jump(outer, &[zero, zero]);

        let mut build = Builder::new(&mut func, outer);
        let four = build.iconst(Type::int(32), 4);
        let outer_test = build.icmp(IntPred::Slt, x, four);
        build.br_if(outer_test, inner, &[zero, sum], done, &[sum]);

        let mut build = Builder::new(&mut func, inner);
        let three = build.iconst(Type::int(32), 3);
        let inner_test = build.icmp(IntPred::Slt, y, three);
        build.br_if(inner_test, body, &[], after, &[]);

        let mut build = Builder::new(&mut func, body);
        let part = build.binary(Opcode::Mul, x, y, Flags::NSW);
        let run = build.binary(Opcode::Add, acc, part, Flags::NSW);
        build.jump(step, &[]);

        let mut build = Builder::new(&mut func, step);
        let one = build.iconst(Type::int(32), 1);
        let next = build.binary(Opcode::Add, y, one, Flags::NSW);
        build.jump(inner, &[next, run]);

        let mut build = Builder::new(&mut func, after);
        let step_on = build.iconst(Type::int(32), 1);
        let on = build.binary(Opcode::Add, x, step_on, Flags::NSW);
        build.jump(outer, &[on, acc]);
        Builder::new(&mut func, done).ret(&[answer]);

        let stats = unroll(&mut func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Optimized, UNROLLED), 1);
        assert_eq!(stats.count(Kind::Missed, ESCAPES), 1, "the inner loop is left as it is");
        // One multiply per copy of the outer body, and the four inner loops still go round.
        assert_eq!(tally(&func, Opcode::Mul), 4);
        assert_eq!(tally(&func, Opcode::BrIf), 4);
        sound(&func, &mut names);
    }

    #[test]
    fn the_pass_stops_when_the_fuel_runs_out() {
        let mut it = rotated(3);
        let stats = unroll(&mut it.func, &mut Fuel::of(0));
        assert_eq!(stats.count(Kind::Optimized, UNROLLED), 0);
        assert_eq!(stats.count(Kind::Missed, NO_FUEL), 1);
        assert_eq!(tally(&it.func, Opcode::Add), 2);
        sound(&it.func, &mut it.names);
    }

    #[test]
    fn a_function_with_no_loop_in_it_is_left_alone() {
        let mut names = Interner::new();
        let signature = Signature::new().with_returns(&[Type::int(32)]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let mut build = Builder::new(&mut func, entry);
        let zero = build.iconst(Type::int(32), 0);
        build.ret(&[zero]);

        let stats = unroll(&mut func, &mut Fuel::unlimited());
        assert!(!stats.changed());
        sound(&func, &mut names);
    }

    /// Moves whatever the caller appended after a block's terminator to in front of it.
    fn tucked(func: &mut Func, block: Block) {
        let term = func
            .insts(block)
            .find(|inst| func.is_terminator(*inst))
            .expect("the block ends in something");
        let stragglers: Vec<rucc_ir::Inst> =
            func.insts(block).skip_while(|inst| *inst != term).skip(1).collect();
        for inst in stragglers {
            func.remove_inst(inst);
            func.insert_before(inst, term);
        }
    }
}
