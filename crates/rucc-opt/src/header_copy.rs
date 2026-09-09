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
    Block, BlockCall, Builder, ExtraKind, Func, Inst, InstData, Opcode, Type, Value, ValueList,
};

use crate::cfg::Cfg;
use crate::dom::Dominators;
use crate::loops::Loops;
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
        while let Some(job) = self.plan(func, an, &done, &mut stats, say) {
            say = false;
            if !fuel.take() {
                stats.missed(NO_FUEL);
                break;
            }
            done.insert(job.header);
            done.insert(job.body);
            let copy = apply(func, &job);
            stats.optimized(COPIED);
            an.clear();
            settle(func, an, copy, &mut stats);
            an.clear();
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

impl HeaderCopy {
    /// The first loop worth copying the header of, and what it would take.
    ///
    /// One at a time, because the copy adds a block to the loop it is made for and the forest the
    /// next answer would be read out of is the one this call just invalidated. `say` is false on
    /// every call after the first so that a loop this declines is declined once rather than once
    /// per round.
    fn plan(
        &self,
        func: &Func,
        an: &mut Analyses,
        done: &HashSet<Block>,
        stats: &mut Stats,
        say: bool,
    ) -> Option<Job> {
        let cfg = an.cfg(func).clone();
        let dom = an.dominators(func).clone();
        let loops = an.loops(func).clone();
        let mut found = None;
        for id in loops.all() {
            let header = loops.header(id);
            if done.contains(&header) {
                continue;
            }
            match self.consider(func, &cfg, &dom, &loops, id, header) {
                Ok(job) => {
                    if found.is_none() {
                        found = Some(job);
                    }
                    if !say {
                        break;
                    }
                }
                Err(why) if say && why == ALREADY => stats.note(ALREADY),
                Err(why) if say => stats.missed(why),
                Err(_) => (),
            }
        }
        found
    }

    /// Whether this loop can have its header copied, and why not when it cannot.
    fn consider(
        &self,
        func: &Func,
        cfg: &Cfg,
        dom: &Dominators,
        loops: &Loops,
        id: crate::loops::LoopId,
        header: Block,
    ) -> Result<Job, &'static str> {
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
        let carried = carried(func, dom, loops, id, header, body, &insts)?;
        Ok(Job { header, entry, body, carried })
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

/// The values the header defines that the rest of the loop reads.
///
/// These are what the copy owes a merge at the body. A value read outside the loop is an error
/// rather than an entry, because merging it would need a second parameter at the exit and after
/// [`crate::canon`] there is no such value to merge: loop-closed form has already routed it.
fn carried(
    func: &Func,
    dom: &Dominators,
    loops: &Loops,
    id: crate::loops::LoopId,
    header: Block,
    body: Block,
    insts: &[Inst],
) -> Result<Vec<Value>, &'static str> {
    let mut defined: Vec<Value> = func[header].params.clone();
    for &inst in insts {
        defined.extend(func[inst].results());
    }
    let mut carried = Vec::new();
    for value in defined {
        let mut read = false;
        for block in func.blocks() {
            if block == header || !reads(func, block, value) {
                continue;
            }
            if !loops.contains(id, block) || !dom.dominates(body, block) {
                return Err(ESCAPES);
            }
            read = true;
        }
        if read {
            carried.push(value);
        }
    }
    Ok(carried)
}

/// Whether anything in this block names the value, as an operand or on an edge out of it.
fn reads(func: &Func, block: Block, value: Value) -> bool {
    for inst in func.insts(block) {
        if func[func[inst].args].contains(&value) {
            return true;
        }
        for call in func.successors(inst) {
            if func[call.args].contains(&value) {
                return true;
            }
        }
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
        if func[at].block == job.header {
            func.set_block_call(at, BlockCall { block: copy, args: ValueList::EMPTY });
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
            func.set_block_call(at, BlockCall { block: call.block, args });
        }
    }
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

/// Asks the ranges whether the copied test is settled, and takes it out where it is.
///
/// This is section 26.6's point about the entry condition. A test that always holds leaves a loop
/// known to run at least once, which is what document 07.5's trip count wanted. One that never
/// holds leaves the loop unreachable, and taking it out is [`crate::simplify_cfg::sweep`]'s job
/// rather than this one's.
fn settle(func: &mut Func, an: &mut Analyses, copy: Block, stats: &mut Stats) {
    let Some(term) = func.terminator(copy) else { return };
    let cond = func[func[term].args][0];
    let answer = {
        let cfg = an.cfg(func).clone();
        let dom = an.dominators(func).clone();
        let mut ranges = Ranges::new(func, &cfg, &dom);
        prune::settled(func, &mut ranges, copy, cond)
    };
    let Some(taken) = answer else {
        stats.missed(UNDECIDED);
        return;
    };
    let calls: Vec<BlockCall> = func.successors(term).collect();
    let call = if taken { calls[0] } else { calls[1] };
    simplify_cfg::jump_to(func, term, call);
    stats.optimized(if taken { ENTERED } else { SKIPPED });
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{
        Block, Builder, Def, Flags, Func, IntPred, MemInfo, MemOrder, Module, Opcode, Restrict,
        Signature, Type, verify_func,
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
}
