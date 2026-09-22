//! Takes out a loop that runs a known number of times and leaves nothing behind.
//!
//! Design: `spec/optimizer/17-dce.md` for what makes a thing removable and
//! `spec/optimizer/28-induction-variables.md` for where the trip count comes from. This is
//! tamnd/rucc#1631.
//!
//! [`crate::dce`] cannot do this and the reason is worth stating, because it looks at first like a
//! gap in that pass. An empty counted loop has a counter, an add, a compare and a branch, and
//! every one of them is used: the add feeds the compare, the compare feeds the branch, and the
//! branch feeds the block parameter the add reads. Nothing in it has a use count of zero, so a
//! pass driven by use counts correctly leaves all of it alone. The question that gets the loop out
//! is not asked of an instruction, it is asked of the loop: does anything outside read what it
//! computes, does it do anything to memory, and does it come back. Three yeses and the loop is a
//! way of spending time.
//!
//! # Which loops
//!
//! A preheader, one exit, and a trip count that is a number rather than an estimate. The count is
//! what says the loop terminates, which is the third question and the one a person is most likely
//! to forget: a loop that computes nothing and never comes back still cannot be taken out, because
//! not coming back is what it does. [`crate::scev::Bound::under_undefined_overflow`] is the same
//! accessor [`crate::unroll`] reads for the same reason, and section 7.5's distinction between a
//! bound and an estimate is exactly this: an estimate decides whether a transformation pays and a
//! bound decides what the program does.
//!
//! Every instruction inside has to be one whose not happening nothing can tell. That is the
//! predicate [`crate::dce`] already has, so it is read from there rather than written again, and
//! it means a plain load may be inside the loop and a `volatile` one may not. A call is allowed
//! when the purity analysis says it reads memory at most and comes back, which is the same rule
//! that lets a call whose result nothing reads go.
//!
//! Nothing the loop defines may be read outside it. That includes the argument list on the edge
//! out, because an argument there lands in a parameter of the block the loop leaves to, and a
//! parameter is read by whatever reads it. A loop whose final value somebody wants is a loop for
//! the second half of #1631, which writes that value down as `base + step * trips` and is a
//! different piece of work with a different correctness argument.
//!
//! # What it does
//!
//! Points the preheader at the block the loop left to, with the arguments the exit edge carried,
//! and lets the sweep in [`crate::simplify_cfg`] take the blocks nothing reaches. The arguments
//! are all defined outside the loop by the time this runs, since that is what the check above
//! established, and each one is asserted to dominate the preheader rather than assumed to: a value
//! defined outside the loop that reaches the exit test has to dominate the preheader, and an
//! assertion is cheaper than being wrong about why.

use std::collections::HashSet;

use rucc_ir::{Block, Builder, Func, Opcode, Value};

use crate::cfg::Cfg;
use crate::dom::Dominators;
use crate::loops::{LoopId, Loops};
use crate::purity::Facts;
use crate::scev::{Bound, Scev};
use crate::{Analyses, Fuel, Pass, Preserved, Stats};

const DELETED: &str = "loop taken out, it runs a known number of times and leaves nothing behind";
const NO_COUNT: &str = "loop left as it was, how many times it runs is not a number known here";
const SHAPE: &str =
    "loop left as it was, it has no preheader or it leaves from more than one place";
const EFFECTS: &str = "loop left as it was, something in it does more than work out a value";
const ESCAPES: &str = "loop left as it was, a value it defines is read outside it";
const ENTRIES: &str = "loop left as it was, it is reached somewhere other than at its header";
const NO_FUEL: &str = "loop left as it was, the pass ran out of fuel";

/// Section 17's dead code elimination, asked about a loop rather than about an instruction.
#[derive(Debug)]
pub struct LoopDelete;

impl Pass for LoopDelete {
    fn name(&self) -> &'static str {
        "loop-delete"
    }

    fn describe(&self) -> &'static str {
        "a loop that runs a known number of times and leaves nothing behind is taken out"
    }

    fn preserves(&self) -> Preserved {
        // The loop goes, and its blocks with it.
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
            stats.optimized(DELETED);
            an.clear();
            crate::simplify_cfg::sweep(func, an, &mut stats);
        }
        an.clear();
        stats
    }
}

/// One loop to take out, worked out against the function as it stands.
#[derive(Debug)]
struct Job {
    /// The block the loop is entered at, which is what says which loop this was.
    header: Block,
    /// The one block outside the loop with an edge to the header.
    preheader: Block,
    /// The block outside the loop the one exit edge arrives at.
    exit: Block,
    /// What that edge carried, which the preheader carries instead.
    args: Vec<Value>,
}

/// The innermost loop that can go, and what it would take.
///
/// One at a time, for the reason [`crate::unroll::plan`] takes one at a time: taking a loop out
/// invalidates the forest the next answer would be read out of. `say` is false after the first
/// round so that a loop this declines is declined once rather than once per round.
fn plan(
    func: &Func,
    an: &mut Analyses,
    done: &HashSet<Block>,
    stats: &mut Stats,
    say: bool,
) -> Option<Job> {
    let facts = an.purity();
    let cfg = an.cfg(func);
    let doms = an.dominators(func);
    let loops = an.loops(func);
    let mut scev = Scev::new(func, cfg, loops);
    let mut found: Option<(u32, Job)> = None;
    for id in loops.all() {
        if done.contains(&loops.header(id)) {
            continue;
        }
        match consider(func, cfg, doms, loops, facts, &mut scev, id) {
            Ok(job) => {
                let depth = loops.depth(id);
                if found.as_ref().is_none_or(|(had, _)| depth > *had) {
                    found = Some((depth, job));
                }
            }
            Err(why) if say => stats.missed(why),
            Err(_) => (),
        }
    }
    found.map(|(_, job)| job)
}

/// Whether this loop can go, and why not when it cannot.
fn consider(
    func: &Func,
    cfg: &Cfg,
    doms: &Dominators,
    loops: &Loops,
    facts: &Facts,
    scev: &mut Scev<'_>,
    id: LoopId,
) -> Result<Job, &'static str> {
    let header = loops.header(id);
    let preheader = loops.preheader(cfg, id).ok_or(SHAPE)?;
    let [only] = loops.exits(id) else {
        return Err(SHAPE);
    };

    let blocks = loops.blocks(id).to_vec();
    let inside: HashSet<Block> = blocks.iter().copied().collect();
    for &block in &blocks {
        // The same reducibility check unrolling makes. A block of the loop reached from outside
        // the loop is a region this has no right to reason about as one piece.
        if block != header && cfg.predecessors(block).iter().any(|at| !inside.contains(at)) {
            return Err(ENTRIES);
        }
        for inst in func.insts(block) {
            if func.is_terminator(inst) {
                // A terminator that leaves the function or goes somewhere worked out at run time
                // is not an edge the forest accounted for, so the one exit counted above is not
                // the only way out.
                if !matches!(func[inst].opcode, Opcode::Jump | Opcode::BrIf) {
                    return Err(EFFECTS);
                }
                continue;
            }
            if !crate::dce::removable(func, inst, facts) {
                return Err(EFFECTS);
            }
        }
    }
    if crate::unroll::escapes(func, &blocks, &inside) {
        return Err(ESCAPES);
    }

    // What the edge out carries lands in a parameter of the block it arrives at, and a parameter
    // is read by whatever reads it, so an argument defined inside the loop is a value that escapes
    // by another road. The ones that are left are defined outside and the preheader can pass them
    // itself.
    let term = func.terminator(only.from).ok_or(SHAPE)?;
    let leaving = func.successors(term).find(|call| call.block == only.to).ok_or(SHAPE)?;
    let args = func[leaving.args].to_vec();
    for &arg in &args {
        if !loops.is_invariant(func, id, arg) {
            return Err(ESCAPES);
        }
        debug_assert!(
            doms.dominates(defined_in(func, arg), preheader),
            "a value defined outside the loop that reaches the exit test dominates the preheader"
        );
    }

    // The count is what says the loop comes back. A loop that computes nothing and runs forever
    // still does something, which is run forever. A count worked out from a value the loop does
    // not change says that as well as a number does: whatever that value is, the loop gets to it,
    // and nothing in here needs to know how many steps that took.
    if scev.bound(id).as_ref().and_then(Bound::under_undefined_overflow).is_none() {
        return Err(NO_COUNT);
    }
    Ok(Job { header, preheader, exit: only.to, args })
}

/// The block a value is defined in.
fn defined_in(func: &Func, value: Value) -> Block {
    match func[value].def {
        rucc_ir::Def::Result { inst, .. } => {
            func.block_of(inst).expect("a value in use is defined in a block")
        }
        rucc_ir::Def::Param { block, .. } => block,
    }
}

/// Points the preheader past the loop.
fn apply(func: &mut Func, job: &Job) {
    let term = func.terminator(job.preheader).expect("a preheader ends in a jump to the header");
    func.remove_inst(term);
    Builder::new(func, job.preheader).jump(job.exit, &job.args);
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{
        Block, Builder, Flags, Func, IntPred, MemInfo, MemOrder, Module, Opcode, Restrict,
        Signature, Type, Value, verify_func,
    };
    use rucc_target::{TargetInfo, Triple};

    use super::{DELETED, EFFECTS, ESCAPES, LoopDelete, NO_COUNT, NO_FUEL};
    use crate::stats::Kind;
    use crate::{Fuel, Pass, Stats};

    /// Runs the pass over the function as it stands.
    fn delete(func: &mut Func, fuel: &mut Fuel) -> Stats {
        LoopDelete.run(func, &mut crate::machine::fixtures::analyses(), fuel)
    }

    /// Insists the function is one the rest of the compiler may believe.
    ///
    /// Pointing a block at a different successor is the edit that hands a block the wrong number
    /// of arguments and strands a definition its uses still name, so this is where most of the
    /// strength of these tests is.
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

    /// How many loops are left.
    fn loops(func: &Func) -> usize {
        let cfg = crate::cfg::Cfg::new(func);
        let doms = crate::dom::Dominators::new(&cfg);
        crate::loops::Loops::new(&cfg, &doms).count()
    }

    /// A four byte write with nothing said about what it aliases.
    fn plain() -> MemInfo {
        MemInfo {
            size: 4,
            align: 4,
            order: MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        }
    }

    /// What the limit of the exit test is.
    enum Limit {
        /// A number written in the program.
        Number(i128),
        /// A value the function was handed, which the loop does not change.
        Given,
    }

    /// What the loop does, which is the whole of what decides whether it can go.
    #[derive(Clone, Copy, PartialEq)]
    enum What {
        /// Adds up a number nothing ever reads.
        Nothing,
        /// Writes each running total to the pointer it was handed.
        Writes,
        /// Hands its running total to the block it leaves to.
        HandsOut,
    }

    struct Shape {
        names: Interner,
        func: Func,
        entry: Block,
        done: Block,
    }

    /// A counted loop in the shape `crate::canon` and `crate::header_copy` leave a `for` in.
    ///
    /// ```text
    /// entry(p, n): jump head(0, 0)
    /// head(i, sum): jump body(i, sum)
    /// body(c, r): total = r + c; next = c + 1; test = next < limit
    ///             br test -> head(next, total), done()
    /// done: ret
    /// ```
    ///
    /// Two blocks in the loop rather than one, so that taking it out has more than one block to get
    /// rid of, and a running total carried round, so that there is something inside worth asking
    /// whether anybody reads.
    fn shaped(limit: Limit, what: What) -> Shape {
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::PTR, Type::int(32)]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let head = func.create_block();
        let body = func.create_block();
        let done = func.create_block();
        let place = func.append_param(entry, Type::PTR);
        let given = func.append_param(entry, Type::int(32));
        let i = func.append_param(head, Type::int(32));
        let sum = func.append_param(head, Type::int(32));
        let carried = func.append_param(body, Type::int(32));
        let running = func.append_param(body, Type::int(32));
        if what == What::HandsOut {
            func.append_param(done, Type::int(32));
        }

        let mut build = Builder::new(&mut func, entry);
        let zero = build.iconst(Type::int(32), 0);
        build.jump(head, &[zero, zero]);
        Builder::new(&mut func, head).jump(body, &[i, sum]);

        let mut build = Builder::new(&mut func, body);
        let total = build.binary(Opcode::Add, running, carried, Flags::NSW);
        if what == What::Writes {
            build.store(total, place, plain(), Flags::NONE);
        }
        let one = build.iconst(Type::int(32), 1);
        let next = build.binary(Opcode::Add, carried, one, Flags::NSW);
        let stop = match limit {
            Limit::Number(n) => build.iconst(Type::int(32), n),
            Limit::Given => given,
        };
        let test = build.icmp(IntPred::Slt, next, stop);
        let out: Vec<Value> = if what == What::HandsOut { vec![total] } else { Vec::new() };
        build.br_if(test, head, &[next, total], done, &out);
        Builder::new(&mut func, done).ret(&[]);
        Shape { names, func, entry, done }
    }

    #[test]
    fn a_loop_that_leaves_nothing_behind_is_taken_out() {
        let mut it = shaped(Limit::Number(1000), What::Nothing);
        let stats = delete(&mut it.func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Optimized, DELETED), 1);
        assert_eq!(loops(&it.func), 0);
        assert_eq!(tally(&it.func, Opcode::Add), 0, "the counter and the total go with it");
        assert_eq!(tally(&it.func, Opcode::BrIf), 0);
        sound(&it.func, &mut it.names);
    }

    #[test]
    fn the_blocks_it_took_out_are_swept_rather_than_left_unreachable() {
        let mut it = shaped(Limit::Number(1000), What::Nothing);
        delete(&mut it.func, &mut Fuel::unlimited());
        let left: Vec<Block> = it.func.blocks().collect();
        assert_eq!(left, vec![it.entry, it.done], "the header and the body are gone");
        sound(&it.func, &mut it.names);
    }

    /// A count that rests on more than the front end already promised is not a proof.
    ///
    /// The loop here counts up to a value handed to the function, and the bound for it comes back
    /// [`crate::scev::Count::Symbolic`] with two assumptions on it rather than one. The overflow
    /// one the front end already promised. [`crate::scev::Assumption::Approaching`] it did not:
    /// nothing here has shown the counter lands on that limit rather than stepping past it. So the
    /// pass is not entitled to say the loop ends, and a loop that might not end is a loop that does
    /// something. Reading the count through [`crate::scev::Bound::under_undefined_overflow`] is
    /// what makes that the answer, rather than a thing this pass would have to check for itself.
    ///
    /// A symbolic count with nothing but the overflow assumption on it is fine and would be taken.
    /// This is about which assumptions are left, not about the count being a number.
    #[test]
    fn a_count_that_rests_on_more_than_signed_overflow_is_not_enough() {
        let mut it = shaped(Limit::Given, What::Nothing);
        let stats = delete(&mut it.func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Optimized, DELETED), 0);
        assert_eq!(stats.count(Kind::Missed, NO_COUNT), 1);
        assert_eq!(loops(&it.func), 1);
        sound(&it.func, &mut it.names);
    }

    #[test]
    fn a_loop_that_writes_to_memory_is_left_alone() {
        let mut it = shaped(Limit::Number(1000), What::Writes);
        let stats = delete(&mut it.func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Optimized, DELETED), 0);
        assert_eq!(stats.count(Kind::Missed, EFFECTS), 1);
        assert_eq!(loops(&it.func), 1);
        assert_eq!(tally(&it.func, Opcode::Store), 1);
        sound(&it.func, &mut it.names);
    }

    #[test]
    fn a_loop_whose_total_is_read_afterwards_is_left_alone() {
        let mut it = shaped(Limit::Number(1000), What::HandsOut);
        let stats = delete(&mut it.func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Optimized, DELETED), 0);
        assert_eq!(stats.count(Kind::Missed, ESCAPES), 1);
        assert_eq!(loops(&it.func), 1);
        sound(&it.func, &mut it.names);
    }

    /// A loop that comes back but not after a number of steps anything here can work out.
    ///
    /// ```text
    /// entry(p, n): jump head(1)
    /// head(c): next = c + c; test = next < 1000; br test -> head(next), done()
    /// done: ret
    /// ```
    ///
    /// The counter doubles, so it is not a value that goes up by the same amount every time and
    /// there is no count to be had. It does terminate, which is the point: the pass is not allowed
    /// to lean on a loop looking harmless, only on the count that says it ends.
    fn doubling() -> Shape {
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::PTR, Type::int(32)]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let head = func.create_block();
        let done = func.create_block();
        func.append_param(entry, Type::PTR);
        func.append_param(entry, Type::int(32));
        let carried = func.append_param(head, Type::int(32));

        let mut build = Builder::new(&mut func, entry);
        let one = build.iconst(Type::int(32), 1);
        build.jump(head, &[one]);

        let mut build = Builder::new(&mut func, head);
        let next = build.binary(Opcode::Add, carried, carried, Flags::NSW);
        let stop = build.iconst(Type::int(32), 1000);
        let test = build.icmp(IntPred::Slt, next, stop);
        build.br_if(test, head, &[next], done, &[]);
        Builder::new(&mut func, done).ret(&[]);
        Shape { names, func, entry, done }
    }

    #[test]
    fn a_loop_whose_count_is_not_known_is_left_alone() {
        let mut it = doubling();
        let stats = delete(&mut it.func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Optimized, DELETED), 0);
        assert_eq!(stats.count(Kind::Missed, NO_COUNT), 1);
        assert_eq!(loops(&it.func), 1);
        sound(&it.func, &mut it.names);
    }

    #[test]
    fn the_pass_stops_when_the_fuel_runs_out() {
        let mut it = shaped(Limit::Number(1000), What::Nothing);
        let stats = delete(&mut it.func, &mut Fuel::of(0));
        assert_eq!(stats.count(Kind::Optimized, DELETED), 0);
        assert_eq!(stats.count(Kind::Missed, NO_FUEL), 1);
        assert_eq!(loops(&it.func), 1);
        sound(&it.func, &mut it.names);
    }

    #[test]
    fn a_function_with_no_body_is_not_a_problem() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let stats = delete(&mut func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Optimized, DELETED), 0);
    }
}
