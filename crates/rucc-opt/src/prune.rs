//! What the ranges prove cannot happen, taken out of the graph.
//!
//! Two transformations, and they are here together because section 24.4 says so: the one on
//! switches "belongs in the same pass as the range-based branch simplification of document 21".
//! Both ask document 10's machinery one question and act on a yes, and neither of them can do
//! anything the other could not have set up, so one walk asking both is cheaper than two walks
//! asking one each.
//!
//! # The branch the ranges decide
//!
//! Section 21.1's branch simplification, the third of the three forms it takes. A branch whose
//! two arms go to the same place is a jump, and [`crate::simplify_cfg`] does that one. A branch on
//! a constant is a jump, and that one is there too. A branch on something that is not a constant
//! but that cannot come out any way other than one is a jump as well, and that is this one, because
//! it is the form that needs an analysis rather than a look at the operand.
//!
//! ```c
//! if (x > 10) { if (x > 5) { f(); } }
//! ```
//!
//! The inner condition is not a constant and no rewrite rule can see it is settled, because what
//! settles it is the edge the block was reached by rather than anything in the expression. The
//! ranges see it: on the edge out of `x > 10` where the branch was taken, `x` is in
//! `[11, INT_MAX]`, and `x > 5` over that range is always true. So the inner branch is a jump to
//! its taken arm, and the other arm stops being reachable and goes with it.
//!
//! # Why this is not jump threading
//!
//! [`crate::thread`] asks a related question and gets a different answer. It asks whether the
//! branch at the end of a block is settled by which edge control arrived on, and its answer is per
//! edge: an edge whose arrival settles the branch is redirected past it, and the other edges into
//! the same block are left alone. This asks whether the branch is settled at the block whatever
//! edge control arrived on, and its answer is per block. Neither subsumes the other. Threading
//! handles the case where one predecessor knows something the others do not, and pays for it with
//! a redirected edge or a copied block. This handles the case where the fact holds on every path
//! in, and pays nothing, but it will not fire where only one path establishes the fact.
//!
//! The two also read facts from different distances. Threading looks at the block control came
//! from. The ranges walk up the dominator tree collecting what every branch above narrowed, so the
//! `if (x > 10)` above can be any number of blocks away from the `if (x > 5)` and the answer is the
//! same.
//!
//! # The case the ranges rule out
//!
//! Section 24.4's one middle end transformation on switches, and the reason document 24 keeps a
//! `switch` whole through the entire middle end rather than lowering it early. A switch that
//! survives is a single node whose operand has one range, and a case value outside that range
//! names an arm nothing can reach.
//!
//! ```c
//! switch (x & 3) { case 0: ...; case 2: ...; case 7: ...; }
//! ```
//!
//! The operand is in `[0, 3]`, so `case 7` is dead. What that buys is more than the compare it
//! removes. Document 24's lowering decides between a walk, a binary search, a bit test and a jump
//! table by how dense the case values are, and dropping the outlier is what turns a switch that
//! looked sparse into one that is dense enough for a table. Section 24.4 puts it this way: it "can
//! turn a sparse switch into a dense one and change the lowering decision entirely".
//!
//! A switch every one of whose cases is ruled out becomes a jump to its default, which is the
//! same thing happening to the whole node rather than to one arm of it.
//!
//! # Answers first, then rewrites
//!
//! [`Ranges`] borrows the function, so nothing can be changed while it is alive. The walk
//! therefore collects every answer, drops the oracle, and then applies them all, rather than the
//! block at a time shape [`crate::phiopt`] uses.
//!
//! That is not only a borrow checker accommodation, it is also cheaper, and the reason it is
//! sound is worth stating. An answer here is a fact that holds at a block. Applying another answer
//! removes a branch, which removes edges, and a value's range at a block is the union over the
//! paths that reach it, so removing a path can only narrow a range and never widen one. A fact
//! proved before the rewrites is therefore still a fact after them. What can happen is that a
//! block an answer was about stops being reachable, and an answer about a block nothing reaches is
//! harmless because the block is about to be swept.
//!
//! # What it refuses
//!
//! A condition that is already a constant, because that is [`crate::simplify_cfg`]'s branch fold
//! and two passes doing the same rewrite is two answers to check rather than one. A branch whose
//! arms go to the same block, for the same reason.
//!
//! Everything else it refuses is the oracle saying it does not know, which is not a refusal so
//! much as the answer, and it is recorded as a miss so that `-fopt-info-all` shows how often the
//! question was asked and came back empty.
//!
//! # Which level
//!
//! `-O1` and above. Section 24.4 calls the switch half "cheap, it uses machinery that exists", and
//! the branch half asks one question per conditional branch rather than one per value, so the
//! query count is bounded by the number of branches rather than by the size of the function. Both
//! halves only ever remove code, so `-Os` and `-Oz` want them as much as `-O2` does.
//!
//! It runs after [`crate::thread`] and [`crate::phiopt`] and before [`crate::simplify_cfg`], which
//! is where it has to be at both ends. After, because both of those change the graph and the facts
//! this reads are about the graph. Before, because what this leaves is a jump where a branch was
//! and a block with one predecessor where there were two, and forwarding the first and merging the
//! second is [`crate::simplify_cfg`]'s work rather than a second copy of it here.
//!
//! The blocks that stop being reachable are this pass's own problem rather than the cleanup
//! pass's, because section 6.5 puts that obligation on whichever pass stranded them and the
//! verifier holds every pass to it. So the walk that takes them out is called from here, and it is
//! [`crate::simplify_cfg`]'s walk rather than a second one written next door.

use rucc_ir::{Block, BlockCall, Def, Extra, Func, Imm, Inst, IntPred, Opcode, SwitchInfo, Value};

use crate::fold::constant;
use crate::range::ops::Truth;
use crate::range::query::Ranges;
use crate::simplify_cfg;
use crate::{Analyses, Fuel, Pass, Preserved, Stats};

/// Recorded once for each branch the ranges settled.
const BRANCH_DECIDED: &str =
    "branch the value ranges settle whichever way control reached it replaced by a jump";

/// Recorded once for each case value the ranges ruled out.
const CASE_REMOVED: &str =
    "case whose value the switched value cannot hold taken out of the switch";

/// Recorded once for a switch none of whose cases can be reached.
const SWITCH_REMOVED: &str =
    "switch none of whose cases the switched value can reach replaced by a jump to its default";

/// Recorded once for each branch the ranges were asked about and could not settle.
const BRANCH_UNDECIDED: &str = "branch kept, the value ranges do not settle which way it goes";

/// Recorded once for each switch the ranges ruled no case out of.
const NO_CASE_REMOVED: &str = "switch kept whole, the value ranges rule none of its cases out";
const NO_FUEL: &str = "branch or switch kept, the pass ran out of fuel";

/// The pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Prune;

impl Pass for Prune {
    fn name(&self) -> &'static str {
        "prune"
    }

    fn describe(&self) -> &'static str {
        "a branch the ranges settle becomes a jump, and a case they rule out leaves its switch"
    }

    fn preserves(&self) -> Preserved {
        // Nothing. An arm that stops being reachable is an edge that stops existing, so every
        // analysis built on the graph was built on a different graph.
        Preserved::NONE
    }

    fn run(&self, func: &mut Func, an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        if func.entry().is_none() {
            return stats;
        }
        let plan = answers(func, an, &mut stats);
        if plan.branches.is_empty() && plan.switches.is_empty() {
            return stats;
        }
        'apply: {
            for (term, call) in plan.branches {
                if !fuel.take() {
                    stats.missed(NO_FUEL);
                    break 'apply;
                }
                simplify_cfg::jump_to(func, term, call);
                stats.optimized(BRANCH_DECIDED);
            }
            for (term, keeping) in plan.switches {
                if !fuel.take() {
                    stats.missed(NO_FUEL);
                    break 'apply;
                }
                let removed = shrink(func, term, &keeping);
                for _ in 0..removed {
                    stats.optimized(CASE_REMOVED);
                }
                if keeping.is_empty() {
                    stats.optimized(SWITCH_REMOVED);
                }
            }
        }
        // The graph was about the function as it was a moment ago, and the manager clears the
        // cache after the pass returns, which is too late for the pass itself.
        an.clear();
        // Section 6.5 makes taking the stranded blocks out an obligation of whichever pass
        // stranded them rather than a favour the cleanup pass does, and the verifier holds every
        // pass to it under `-fverify-each`. An arm that stops being reachable is exactly that, so
        // the walk is here, and it is [`crate::simplify_cfg`]'s walk rather than a second one
        // written next door, because two answers about what reachable means is two compilers.
        simplify_cfg::sweep(func, an, &mut stats);
        stats
    }
}

/// Every rewrite the ranges license, worked out against the function as it stands.
#[derive(Debug, Default)]
struct Plan {
    /// The branches that only go one way, and the edge each of them goes by.
    branches: Vec<(Inst, BlockCall)>,
    /// The switches that lose a case, and which of their cases each of them keeps.
    switches: Vec<(Inst, Vec<usize>)>,
}

/// Everything the ranges license, worked out against the function as it stands.
///
/// One [`Ranges`] for the whole walk rather than one per block, because the oracle caches what it
/// has worked out and a second one would start from nothing.
fn answers(func: &Func, an: &mut Analyses, stats: &mut Stats) -> Plan {
    let mut plan = Plan::default();
    let cfg = an.cfg(func).clone();
    let dom = an.dominators(func).clone();
    let mut ranges = Ranges::new(func, &cfg, &dom);
    for block in func.blocks() {
        if !cfg.reaches(block) {
            continue;
        }
        let Some(term) = func.terminator(block) else { continue };
        match func[term].opcode {
            Opcode::BrIf => match decided(func, &mut ranges, block, term) {
                Answer::Jump(call) => plan.branches.push((term, call)),
                Answer::Unsettled => stats.missed(BRANCH_UNDECIDED),
                Answer::NotAsked => (),
            },
            Opcode::Switch => match reachable(func, &mut ranges, block, term) {
                Some(keeping) => plan.switches.push((term, keeping)),
                None => stats.missed(NO_CASE_REMOVED),
            },
            _ => (),
        }
    }
    plan
}

/// What came back about a conditional branch.
///
/// Three outcomes rather than two, because a branch this pass declines to look at and a branch it
/// looked at and could not settle say different things under `-fopt-info-all`. The first is not a
/// missed optimization at all, it is another pass's fold, and counting it as one would put a
/// remark on every constant branch in the program saying the ranges failed at something they were
/// never asked.
enum Answer {
    /// The one edge the branch takes.
    Jump(BlockCall),
    /// Asked, and the oracle does not know.
    Unsettled,
    /// Not this pass's question.
    NotAsked,
}

/// The one edge a conditional branch takes, when the ranges say it only has one.
///
/// The first target is the one taken when the condition is one, which is what `Builder::br_if`
/// writes and what the printer reads back, so a condition that always holds takes target zero.
fn decided(func: &Func, ranges: &mut Ranges<'_>, block: Block, term: Inst) -> Answer {
    let data = &func[term];
    let Extra::Targets(targets) = data.extra else { return Answer::NotAsked };
    let Some(&cond) = func[data.args].first() else { return Answer::NotAsked };
    // Already a constant, or both arms in one place. Both are [`crate::simplify_cfg`]'s fold and
    // it runs right after this one, so answering them here would be a second answer to the same
    // question rather than an answer to one nothing else has.
    if constant(func, cond).is_some() {
        return Answer::NotAsked;
    }
    let calls = &func[targets];
    let together =
        |two: &[BlockCall]| two[0].block == two[1].block && func[two[0].args] == func[two[1].args];
    if calls.len() == 2 && together(calls) {
        return Answer::NotAsked;
    }
    let arm = match settled(func, ranges, block, cond) {
        Some(true) => 0,
        Some(false) => 1,
        None => return Answer::Unsettled,
    };
    func[targets].get(arm).copied().map_or(Answer::NotAsked, Answer::Jump)
}

/// Whether this condition can only come out one way at this block, and which way that is.
///
/// A comparison is asked about through [`Ranges::compare`], which is the entry point section 10.3
/// puts the relational oracle behind, so a branch on `a < b` under a dominating `a < b` is settled
/// even where neither value is pinned down to a range that settles it. Anything else is asked as a
/// range: a one bit value that cannot be zero is true and one that can only be zero is false.
fn settled(func: &Func, ranges: &mut Ranges<'_>, block: Block, cond: Value) -> Option<bool> {
    if let Some((pred, lhs, rhs)) = comparison(func, cond) {
        return match ranges.compare(pred, lhs, rhs, block) {
            Truth::Always => Some(true),
            Truth::Never => Some(false),
            Truth::Either => None,
        };
    }
    let range = ranges.at(cond, block);
    if range.nonzero() {
        return Some(true);
    }
    (range.singleton() == Some(0)).then_some(false)
}

/// The comparison behind this value, if it is one.
fn comparison(func: &Func, value: Value) -> Option<(IntPred, Value, Value)> {
    let Def::Result { inst, .. } = func[value].def else { return None };
    if func[inst].opcode != Opcode::ICmp {
        return None;
    }
    let Extra::IntPred(pred) = func[inst].extra else { return None };
    let &[lhs, rhs] = func[func[inst].args].first_chunk::<2>()?;
    Some((pred, lhs, rhs))
}

/// Which of a switch's cases the switched value can still hold, when that is not all of them.
///
/// The answer is the places the surviving cases are in, in the order they were in, and `None` is
/// the switch that keeps every case rather than the switch that keeps none. An empty list is the
/// switch none of whose cases can be reached, which becomes a jump to its default.
fn reachable(func: &Func, ranges: &mut Ranges<'_>, block: Block, term: Inst) -> Option<Vec<usize>> {
    let Extra::Switch(at) = func[term].extra else { return None };
    let info = func[at];
    let arg = *func[func[term].args].first()?;
    let range = ranges.at(arg, block);
    if range.is_full() {
        return None;
    }
    let cases = &func[info.cases];
    let keeping: Vec<usize> =
        (0..cases.len()).filter(|&at| range.contains(cases[at].unsigned())).collect();
    (keeping.len() < cases.len()).then_some(keeping)
}

/// Rewrites a switch to the cases in that list, and says how many it dropped.
///
/// A switch left with no cases is a jump to its default, because a decision tree over nothing is
/// the default arm and document 24's lowering would rather not be handed one.
fn shrink(func: &mut Func, term: Inst, keeping: &[usize]) -> usize {
    let Extra::Switch(at) = func[term].extra else { return 0 };
    let info = func[at];
    let all = func[info.targets].to_vec();
    let values = func[info.cases].to_vec();
    let removed = values.len() - keeping.len();
    // The default is the first target and the cases follow it in the order their values are in,
    // so the target for the case in place `at` is one past it.
    let default = all[0];
    if keeping.is_empty() {
        simplify_cfg::jump_to(func, term, default);
        return removed;
    }
    let targets: Vec<BlockCall> =
        std::iter::once(default).chain(keeping.iter().map(|&at| all[at + 1])).collect();
    let cases: Vec<Imm> = keeping.iter().map(|&at| values[at]).collect();
    let targets = func.push_block_calls(&targets);
    let cases = func.push_imms(&cases);
    let fresh = func.add_switch(SwitchInfo { targets, cases });
    func[term].extra = Extra::Switch(fresh);
    removed
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{Block, Builder, Flags, Func, IntPred, Opcode, Signature, Type};

    use super::Prune;
    use crate::stats::Kind;
    use crate::{Analyses, Fuel, Pass, Stats};

    /// Runs the pass with as much fuel as it wants.
    fn prune(func: &mut Func) -> Stats {
        Prune.run(func, &mut Analyses::new(), &mut Fuel::unlimited())
    }

    /// The opcode of a block's terminator.
    fn terminator(func: &Func, block: usize) -> Opcode {
        let block = Block::from_usize(block);
        func[func.terminator(block).expect("every block here has one")].opcode
    }

    /// The blocks a block's terminator names, in the order it names them.
    fn goes_to(func: &Func, block: usize) -> Vec<usize> {
        let block = Block::from_usize(block);
        let term = func.terminator(block).expect("every block here has one");
        func.successors(term).map(|call| call.block.index()).collect()
    }

    /// Two nested branches on the same value, the outer one narrowing it for the inner one.
    ///
    /// Block 0 branches on `x <outer> bound`, block 1 branches on `x <inner> 5`, and blocks 2 and
    /// 3 are the inner branch's two arms. Block 4 is where the outer branch goes when it does not
    /// hold, and it is there so that the inner block has one predecessor rather than being the
    /// entry's only successor.
    fn nested(outer: IntPred, bound: i128, inner: IntPred) -> Func {
        let mut names = Interner::new();
        let ty = Type::int(32);
        let mut func = Func::new(names.intern("f"), Signature::new().with_params(&[ty]));
        let entry = func.create_block();
        let middle = func.create_block();
        let arms = [func.create_block(), func.create_block()];
        let away = func.create_block();
        let x = func.append_param(entry, ty);
        let mut build = Builder::new(&mut func, entry);
        let edge = build.iconst(ty, bound);
        let first = build.icmp(outer, x, edge);
        build.br_if(first, middle, &[], away, &[]);
        let mut build = Builder::new(&mut func, middle);
        let five = build.iconst(ty, 5);
        let second = build.icmp(inner, x, five);
        build.br_if(second, arms[0], &[], arms[1], &[]);
        for block in [arms[0], arms[1], away] {
            let mut build = Builder::new(&mut func, block);
            build.ret(&[]);
        }
        func
    }

    #[test]
    fn a_branch_the_ranges_settle_becomes_a_jump_to_the_arm_they_settle_on() {
        // `if (x > 10) { if (x > 5) ... }`. On the edge into block 1 the value is at least 11, so
        // the second comparison holds there whatever else is true, and the arm it does not take
        // stops being reachable.
        let mut func = nested(IntPred::Sgt, 10, IntPred::Sgt);
        let stats = prune(&mut func);
        assert!(stats.changed());
        assert_eq!(terminator(&func, 1), Opcode::Jump);
        assert_eq!(goes_to(&func, 1), [2]);
        assert_eq!(stats.count(Kind::Optimized, super::BRANCH_DECIDED), 1);
    }

    #[test]
    fn a_branch_the_ranges_settle_the_other_way_jumps_to_the_other_arm() {
        // `if (x > 10) { if (x < 5) ... }`, where the inner comparison cannot hold. The pass has
        // to name the second target rather than the first, and getting that backwards would build
        // a compiler that quietly runs the wrong arm.
        let mut func = nested(IntPred::Sgt, 10, IntPred::Slt);
        let stats = prune(&mut func);
        assert!(stats.changed());
        assert_eq!(terminator(&func, 1), Opcode::Jump);
        assert_eq!(goes_to(&func, 1), [3]);
    }

    #[test]
    fn a_branch_the_ranges_do_not_settle_keeps_its_two_arms() {
        // `if (x > 10) { if (x > 20) ... }` the other way round. Being over 10 says nothing about
        // being over 20, so both arms are still reachable and the branch stays.
        let mut func = nested(IntPred::Sgt, 3, IntPred::Sgt);
        let stats = prune(&mut func);
        assert!(!stats.changed());
        assert_eq!(terminator(&func, 1), Opcode::BrIf);
        assert_eq!(stats.count(Kind::Missed, super::BRANCH_UNDECIDED), 2);
    }

    #[test]
    fn a_branch_on_a_constant_is_left_for_the_control_flow_pass() {
        // Two passes writing the same rewrite is two answers to check, and this one is section
        // 21.1's rather than section 10's. It is refused before the oracle is asked at all.
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let arms = [func.create_block(), func.create_block()];
        let mut build = Builder::new(&mut func, entry);
        let always = build.iconst(Type::int(1), 1);
        build.br_if(always, arms[0], &[], arms[1], &[]);
        for block in arms {
            let mut build = Builder::new(&mut func, block);
            build.ret(&[]);
        }
        let stats = prune(&mut func);
        assert!(!stats.changed());
        assert_eq!(terminator(&func, 0), Opcode::BrIf);
        assert_eq!(stats.count(Kind::Missed, super::BRANCH_UNDECIDED), 0);
    }

    /// A switch on `x & mask`, with those case values and a default.
    fn masked(mask: i128, cases: &[i128]) -> Func {
        let mut names = Interner::new();
        let ty = Type::int(32);
        let mut func = Func::new(names.intern("f"), Signature::new().with_params(&[ty]));
        let entry = func.create_block();
        let default = func.create_block();
        let arms: Vec<Block> = cases.iter().map(|_| func.create_block()).collect();
        let x = func.append_param(entry, ty);
        let mut build = Builder::new(&mut func, entry);
        let bits = build.iconst(ty, mask);
        let narrowed = build.binary(Opcode::And, x, bits, Flags::NONE);
        let pairs: Vec<(i128, Block)> =
            cases.iter().copied().zip(arms.iter().copied()).collect::<Vec<_>>();
        build.switch(narrowed, default, &pairs);
        for block in std::iter::once(default).chain(arms) {
            let mut build = Builder::new(&mut func, block);
            build.ret(&[]);
        }
        func
    }

    #[test]
    fn a_case_the_switched_value_cannot_hold_leaves_the_switch() {
        // `switch (x & 3) { case 0: case 2: case 7: }`. The operand is in [0, 3], so the last
        // case names an arm nothing reaches, and the two that are left are what document 24's
        // lowering gets to decide over.
        let mut func = masked(3, &[0, 2, 7]);
        let stats = prune(&mut func);
        assert!(stats.changed());
        assert_eq!(terminator(&func, 0), Opcode::Switch);
        // The default first and the surviving cases after it, in the order they were in.
        assert_eq!(goes_to(&func, 0), [1, 2, 3]);
        assert_eq!(stats.count(Kind::Optimized, super::CASE_REMOVED), 1);
    }

    #[test]
    fn a_switch_no_case_of_which_can_be_reached_jumps_to_its_default() {
        // Every case is outside the operand's range, so the whole node goes rather than an arm
        // of it. What is left is the default, which is where control was always going.
        let mut func = masked(1, &[5, 9]);
        let stats = prune(&mut func);
        assert!(stats.changed());
        assert_eq!(terminator(&func, 0), Opcode::Jump);
        assert_eq!(goes_to(&func, 0), [1]);
        assert_eq!(stats.count(Kind::Optimized, super::SWITCH_REMOVED), 1);
    }

    #[test]
    fn a_switch_whose_cases_the_operand_can_all_hold_is_kept_whole() {
        let mut func = masked(7, &[0, 2, 7]);
        let stats = prune(&mut func);
        assert!(!stats.changed());
        assert_eq!(terminator(&func, 0), Opcode::Switch);
        assert_eq!(stats.count(Kind::Missed, super::NO_CASE_REMOVED), 1);
    }

    #[test]
    fn a_branch_two_values_are_related_on_settles_without_either_being_pinned_down() {
        // The oracle's half rather than the ranges' half, and the case section 10.3 says it is
        // for. Nothing here says what `a` or `b` can be, so no interval settles the second
        // comparison. What settles it is that the edge into block 1 recorded that `a < b`.
        let mut names = Interner::new();
        let ty = Type::int(32);
        let mut func = Func::new(names.intern("f"), Signature::new().with_params(&[ty, ty]));
        let entry = func.create_block();
        let middle = func.create_block();
        let arms = [func.create_block(), func.create_block()];
        let away = func.create_block();
        let a = func.append_param(entry, ty);
        let b = func.append_param(entry, ty);
        let mut build = Builder::new(&mut func, entry);
        let first = build.icmp(IntPred::Slt, a, b);
        build.br_if(first, middle, &[], away, &[]);
        let mut build = Builder::new(&mut func, middle);
        // A second comparison of the same two values, written again rather than reused, which is
        // what a program with the test in two places hands the optimizer.
        let second = build.icmp(IntPred::Sle, a, b);
        build.br_if(second, arms[0], &[], arms[1], &[]);
        for block in [arms[0], arms[1], away] {
            let mut build = Builder::new(&mut func, block);
            build.ret(&[]);
        }
        let stats = prune(&mut func);
        assert!(stats.changed());
        assert_eq!(terminator(&func, 1), Opcode::Jump);
        assert_eq!(goes_to(&func, 1), [2]);
    }

    #[test]
    fn a_value_that_cannot_be_zero_is_a_branch_that_always_holds() {
        // `if (x == 5) { if ((_Bool)x) ... }`. The condition is a truncation rather than a
        // comparison, so what answers is the range on its own: on the edge into block 1 the value
        // is exactly five, five truncated to one bit is one, and one is not zero.
        let mut names = Interner::new();
        let wide = Type::int(32);
        let mut func = Func::new(names.intern("f"), Signature::new().with_params(&[wide]));
        let entry = func.create_block();
        let middle = func.create_block();
        let arms = [func.create_block(), func.create_block()];
        let away = func.create_block();
        let x = func.append_param(entry, wide);
        let mut build = Builder::new(&mut func, entry);
        let five = build.iconst(wide, 5);
        let is_five = build.icmp(IntPred::Eq, x, five);
        build.br_if(is_five, middle, &[], away, &[]);
        let mut build = Builder::new(&mut func, middle);
        let bit = build.unary(Opcode::Trunc, x, Type::int(1));
        build.br_if(bit, arms[0], &[], arms[1], &[]);
        for block in [arms[0], arms[1], away] {
            let mut build = Builder::new(&mut func, block);
            build.ret(&[]);
        }
        let stats = prune(&mut func);
        assert!(stats.changed());
        assert_eq!(terminator(&func, 1), Opcode::Jump);
        assert_eq!(goes_to(&func, 1), [2]);
    }

    #[test]
    fn a_function_with_no_body_is_left_alone() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let stats = prune(&mut func);
        assert!(!stats.changed());
    }

    #[test]
    fn no_fuel_leaves_the_branch_where_it_is() {
        let mut func = nested(IntPred::Sgt, 10, IntPred::Sgt);
        let stats = Prune.run(&mut func, &mut Analyses::new(), &mut Fuel::of(0));
        assert!(!stats.changed());
        assert_eq!(terminator(&func, 1), Opcode::BrIf);
        assert_eq!(stats.count(Kind::Missed, super::NO_FUEL), 1);
    }
}
