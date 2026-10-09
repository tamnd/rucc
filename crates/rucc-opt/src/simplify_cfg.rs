//! Control flow simplification: unreachable blocks go, a branch that only ever goes one way
//! becomes a jump, a block that does nothing but jump somewhere else stops being in the way, a
//! block parameter that is the same value on every way in stops being a parameter, and a block
//! with one way in is folded into the block above it.
//!
//! Design: `spec/optimizer/21-cfg-simplification.md`, and section 6.5 of
//! `spec/optimizer/06-cfg-and-dominators.md`, which states the rule for the whole optimizer, that
//! a block the entry does not reach is invisible to every analysis and is deleted here rather than
//! by whichever pass happened to notice it.
//!
//! # The order
//!
//! Section 21.4, and it is an order rather than a loop. Unreachable removal, then the branches,
//! then the straightening, then merging, each once. Running the four to a fixed point would cost a
//! walk of the function for every pass over it and buy back a case nobody has: what merging leaves
//! behind is a bigger block, and a bigger block does not make a branch foldable that was not
//! foldable before. A parameter step three takes away is different, because when it was a
//! constant the branch reading it is now a branch on a constant, so step two runs once more after
//! that and only then. The pipeline runs this pass more than once anyway, so the second chance is a
//! pass boundary away rather than a loop away, and that is a chance the pass manager can count and
//! print.
//!
//! Step three is the exception, and it is the spec's exception rather than one taken here. Taking a
//! forwarder out gives the block below it a way in it did not have, which can be the way in that
//! makes one of its parameters the same value from everywhere; and taking a parameter away can be
//! what leaves a block empty enough to be a forwarder. So the two run together on one worklist,
//! which is a fixed point over a step and not over the pass.
//!
//! # The parameter nothing reads
//!
//! Section 21.2 is about a parameter that is the same value every way in, and there is a second
//! kind that goes, which is one nothing reads at all. `crate::dce` is the pass that removes what
//! nothing reads and it says in its own documentation why this one is not its job: a count driven
//! to zero does not see a loop counter, because the counter's only reader is the addition that
//! produces the value handed back to the counter. The count never reaches zero and the whole cycle
//! is dead anyway.
//!
//! So it is answered the other way round, by asking what is live rather than what is dead.
//! Something is live if an instruction that has to happen reads it, and then live spreads: the
//! operands of a live instruction are live, and the argument every edge passes in a live
//! parameter's place is live. Anything the spread does not reach is not read by anything that
//! happens, and a parameter it does not reach goes along with the argument in its place on every
//! edge into the block. What that strands is an addition whose result nobody wants any more, which
//! is exactly the shape `crate::dce` was already good at.
//!
//! Starting from nothing live rather than from everything live is what breaks the cycle, and it is
//! the same optimistic reading section 14.1 takes and section 21.2 takes one paragraph up. The
//! risk in reading optimistically is claiming something is dead when it is not, so the seeds are
//! generous: a terminator or an instruction with effects makes its operands live whatever else is
//! true, and the entry block's parameters are the function's own and stay whether anybody reads
//! them or not.
//!
//! `crate::ivopts` is what makes this worth having. Section 28.4 of
//! `spec/optimizer/28-induction-variables.md` has a loop stop asking its counter anything, and
//! before this the counter went on being incremented round a loop that had no other use for it.
//!
//! What the addition is left reading matters, and this used to leave it reading the parameter that
//! had just gone. The argument for that was that the addition is dead and `crate::dce` is the pass
//! for what is dead, and the argument is true at every level except the one where it counts:
//! `-O0` runs this pass and nothing else, so nothing came along behind it and the operand reached
//! the printer as a use with no definition. So the addition goes here, with the parameter that
//! stranded it, and the `strand` function below carries the argument for why that is always safe.
//!
//! Cross jumping is the one transformation of section 21.1 that is not here at all, and that is
//! section 21.1's last paragraph telling us not to: it costs a branch to save a copy, so it belongs
//! at the machine level under `-Os`, which is document 37.
//!
//! # What a forwarder is allowed to be
//!
//! Section 21.1 wants four things of a block before its predecessors are pointed past it: one
//! successor, the successor is not the exit, the successor is not the block itself, and the edge
//! out is not abnormal. Then it adds the one the block parameter form needs, which is that the
//! arguments the block passes on all dominate every predecessor of it, because those predecessors
//! are the ones that will be passing them.
//!
//! Requiring the block to have no parameters of its own discharges that last one without a
//! dominator tree, and the argument is short. A value defined in a block that dominates the
//! forwarder dominates every predecessor of it too: take any path to a predecessor, follow the edge
//! to the forwarder, and the definition is somewhere on the result, which is either before the
//! predecessor or is the forwarder itself. A block with no parameters and no instructions defines
//! nothing, so the second case cannot arise and the first is the condition.
//!
//! The requirement earns something else as well. A parameter of the forwarder could be read by a
//! block below it, which is legal exactly when the forwarder dominates that block, and pointing the
//! predecessors past would leave that read with nothing to read. Insisting on no parameters is one
//! rule that answers both, and a forwarder that has one gets taken apart by the other half of the
//! worklist first.
//!
//! There is no exit block in this IR, so the second condition is not a condition. Abnormal edges
//! are the ones into a block whose address is taken, which arrive from an `indirect_br` the graph
//! reads from the other end, and those blocks are refused here the same way they are refused
//! everywhere else in this pass.
//!
//! One condition is here that the section does not ask for. A forwarder that passes arguments on,
//! and that is arrived at from a block which branches, is the block the moves for those arguments
//! go in. Take it out and the edge it was on becomes one that goes out of a block with two ways out
//! and into a block with two ways in, which has no end of a block to put a move at, so the back end
//! splits it and puts an empty block back. The block comes back at the end of the layout instead of
//! where it was, the jump that was free because it fell through is a jump that is taken, and the
//! value the move was carrying is live across more of the function. On the corpus at -O2 that costs
//! more than the block is worth, so a forwarder in that position stays. A forwarder that carries
//! nothing is taken out whatever the edges look like, since there is no move to find a place for.
//!
//! And one more that the dominance argument above does not cover. The results of an `asm goto`
//! are written by the terminator of the block above, so they exist on its edges and in the blocks
//! those edges arrive at, but not as arguments on the edges themselves. A forwarder that passes one
//! of them on is where that edge first sees it, and taking the forwarder out would put the value on
//! an edge out of the very instruction that makes it, so a forwarder like that stays.
//!
//! # Why this is not only an optimization
//!
//! Issue 359 is a program that does not link:
//!
//! ```c
//! extern void link_error(void);
//! void foo(int x) {
//!     switch (x) {
//!     case 0:
//!         if (0) { link_error(); case 1: bar(); }
//!     }
//! }
//! ```
//!
//! Nothing calls `link_error`, so a compiler that emits the call produces an object file that
//! does not link, and the difference between the two compilers is not how fast the program runs.
//! The file is in a suite of forty years of compiler bugs for the reason the `case 1:` is where
//! it is: control does reach `bar` through the switch, and it reaches it from inside the body of
//! the dead `if`. A compiler that deletes the compound statement gets this as wrong as one that
//! keeps all of it.
//!
//! Doing it in two steps is what makes that come out right without a special case for it. The
//! branch on the constant becomes a jump, which takes the edge into the dead arm away, and then
//! reachability from the entry decides what is left. The block holding `bar` has an edge from the
//! `switch` and stays. The block holding `link_error` has no edges at all and goes.
//!
//! # The condition it can read
//!
//! A constant, and a comparison of two constants. The second is here rather than in
//! [`crate::fold`] because folding a comparison would produce an `i1` standing on its own, which
//! is issue 352 and does not lower, so the pass that folds arithmetic deliberately leaves
//! comparisons alone. Reading one to decide which way a branch goes produces no `i1` at all: the
//! comparison is left exactly where it was, used by nothing, and [`crate::dce`] takes it out.
//!
//! # Fuel
//!
//! Fuel is charged for each branch that folds and for each block that is merged away, and not for
//! the blocks that go because nothing reaches them. Removal is the second half of the
//! transformation that was already paid for rather than a transformation of its own, and a fuel
//! limit that could stop between the two halves would hand the verifier a block nothing reaches.
//! Section 41.5 of `spec/optimizer/41-correctness.md` asks for fuel that is monotonic, which means
//! each step being all of one change and not part of one.
//!
//! That reasoning covers the blocks a fold stranded. It does not cover the ones that arrived
//! unreachable, and those are not charged for either, for a different reason: section 6.5 makes
//! removing them this pass's standing obligation rather than an optimization, everything below
//! reads the graph as though they are not there, and a bisection that turned the obligation off
//! would be bisecting over a function the rest of the optimizer does not believe in.

use std::collections::VecDeque;

use rucc_base::Idx;
use rucc_base::hash::{Map, Set};
use rucc_ir::{
    Block, BlockCall, Def, Extra, Func, Hint, Imm, Inst, InstData, IntPred, Opcode, SwitchInfo,
    Type, Value,
};

use crate::copy::addressed;
use crate::fold::constant;
use crate::{Analyses, Fuel, Pass, Preserved, Stats, uses};

/// Recorded once for each branch that turned into a jump.
const FOLDED: &str = "branch on a condition that is always the same way replaced by a jump";

/// Recorded once for each block cut short where control stops.
const CUT: &str = "code after a stop or an unreachable promise removed and the block ended there";

/// Recorded for a block that would have been cut short if there had been fuel for it.
const NO_FUEL_CUT: &str =
    "code after a stop or an unreachable promise kept, the pass ran out of fuel";

/// Recorded once for each `switch` whose default went nowhere and now goes to one of its cases.
const UNDEFAULTED: &str = "switch default that is never taken pointed at one of the cases";

/// Recorded for a `switch` that would have been given a new default if there had been fuel.
const NO_FUEL_DEFAULT: &str = "switch default that is never taken kept, the pass ran out of fuel";

/// Recorded once for each branch on a negation that now branches on what was negated.
const UNNEGATED: &str = "branch on a negated condition turned round to branch on the condition";

/// Recorded for a branch on a negation that would have been turned round if there had been fuel.
const NO_FUEL_UNNEGATED: &str = "branch on a negated condition kept, the pass ran out of fuel";

/// Recorded once for each block that went with it.
pub(crate) const REMOVED: &str = "block nothing reaches removed";

/// Recorded once for each block folded into the one above it.
const MERGED: &str = "block with one way into it merged into the block above it";

/// Recorded once for each block that did nothing but jump and is no longer in the way.
const FORWARDED: &str = "block that only jumped somewhere else removed and its edges pointed past";

/// Recorded once for each block parameter that turned out to be one value.
const SAME_EVERY_WAY: &str = "block parameter that arrives as the same value every way in removed";

/// Recorded for a branch that would have folded if there had been fuel for it.
const NO_FUEL: &str = "branch on a known condition left alone, the pass ran out of fuel";

/// Recorded for a block that would have been merged if there had been fuel for it.
const NO_FUEL_MERGE: &str = "block with one way into it left alone, the pass ran out of fuel";

/// Recorded for a forwarder that would have gone if there had been fuel for it.
const NO_FUEL_FORWARD: &str =
    "block that only jumped somewhere else kept, the pass ran out of fuel";

/// Recorded for a block parameter that would have gone if there had been fuel for it.
const NO_FUEL_PARAM: &str = "block parameter that is one value kept, the pass ran out of fuel";

/// Recorded once for each block parameter that nothing turned out to read.
const NOTHING_READS_IT: &str = "block parameter nothing reads removed, and the argument on every \
                                edge that was feeding it";

/// Recorded for a parameter nothing reads that would have gone if there had been fuel for it.
const NO_FUEL_UNREAD: &str = "block parameter nothing reads kept, the pass ran out of fuel";

/// The pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SimplifyCfg;

impl Pass for SimplifyCfg {
    fn name(&self) -> &'static str {
        "simplify-cfg"
    }

    fn describe(&self) -> &'static str {
        "unreachable blocks go, a branch that only goes one way becomes a jump, a block that only \
         jumps stops being in the way, and a block with one way in is merged into the one above it"
    }

    fn preserves(&self) -> Preserved {
        // Nothing at all, and this is the pass the declaration exists for. An edge moves, so the
        // graph is a different graph, and everything built on the graph was about the old one.
        Preserved::NONE
    }

    fn run(&self, func: &mut Func, an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        // Step zero, which makes the promises into edges. A block that stops or promises it is
        // never reached ends there, so what came after goes, the branch it ended with included,
        // and the blocks only it reached are the ones step one takes out.
        if cut(func, fuel, &mut stats) {
            an.clear();
        }
        // Step one, and it is first for a reason beyond tidiness: a branch in a block nothing
        // reaches is a branch nothing executes, and folding one would spend fuel on a change
        // nobody can see and charge the two steps below for walking blocks that are not there.
        sweep(func, an, &mut stats);
        let undefaulted = undefault(func, fuel, &mut stats);
        // After the defaults, because a default made one of the cases can leave the rest one run.
        if range_branches(func, fuel, &mut stats) {
            an.clear();
        }
        if unnegate(func, fuel, &mut stats) {
            an.clear();
        }
        if fold_branches(func, fuel, &mut stats) || undefaulted {
            // The second sweep section 21.4 folds into step two. The cache is holding answers
            // about the function as it was a moment ago, and the manager clears it after the pass
            // returns, which is too late for the pass itself.
            an.clear();
            sweep(func, an, &mut stats);
        }
        let mut forward = Map::default();
        // Step three, and it keeps its own record of the edges rather than asking for the graph,
        // because it changes the edges as it goes and a cached answer would be about the shape the
        // function had one forwarder ago. The parameters nothing reads go first, for the reason
        // this module's documentation gives, which is that a block they leave empty is a forwarder
        // and the worklist below is what takes forwarders out.
        let dropped = drop_unread(func, fuel, &mut stats);
        if straighten(func, fuel, &mut stats, &mut forward) || dropped {
            an.clear();
        }
        // Merging reads which blocks have one predecessor, so it has to run on the graph as it is
        // after the stranded ones have gone. A block kept alive only by an edge from a block
        // nothing reaches looks like it has two ways in until that block is out of the function.
        for chain in chains(func, an) {
            for (at, &block) in chain.iter().enumerate().skip(1) {
                if !fuel.take() {
                    // The rest of the chain goes with it. A block is merged into the one at the
                    // head of its chain, and it can only get there once everything between them
                    // has already arrived.
                    for _ in at..chain.len() {
                        stats.missed(NO_FUEL_MERGE);
                    }
                    break;
                }
                merge(func, chain[0], block, &mut forward);
                stats.optimized(MERGED);
            }
        }
        if !forward.is_empty() {
            // Once, for every parameter every merge bound, rather than a walk of the function per
            // block merged.
            uses::substitute(func, &forward);
            // A parameter that was the same value every way in can have been a constant, and a
            // branch on it is now a branch on that constant. This is the one case where the order
            // above leaves a branch behind that step two would have folded, and when it is the
            // last time the pipeline runs this pass nothing else takes it out.
            if fold_branches(func, fuel, &mut stats) {
                an.clear();
                sweep(func, an, &mut stats);
            }
        }
        // Again, because the cases of `case 0: case 1: return x;` each go to a block that only
        // jumps to the next one, and they all go to one block only now that the forwarders are out
        // of the way. Without this the switch reaches the second `phiopt` as a switch.
        if range_branches(func, fuel, &mut stats) {
            an.clear();
        }
        stats
    }
}

/// Step two, a branch that only ever goes one way becomes a jump. Says whether any did.
fn fold_branches(func: &mut Func, fuel: &mut Fuel, stats: &mut Stats) -> bool {
    let mut folded = false;
    // Nothing bound, because this step asks where a branch goes whichever way control arrived at
    // it. Binding a block's parameters to one edge's arguments is the question [`crate::thread`]
    // asks, and it is a different question with a different answer.
    let unbound = Bindings::default();
    for block in func.blocks().collect::<Vec<Block>>() {
        let Some(term) = func.terminator(block) else { continue };
        let Some(taken) = taken(func, term, &unbound) else { continue };
        if !fuel.take() {
            // Out of fuel stops the transforming and not the looking, the same way the other
            // passes treat it, so that the walk is the same walk at every fuel setting.
            stats.missed(NO_FUEL);
            continue;
        }
        jump_to(func, term, taken);
        stats.optimized(FOLDED);
        folded = true;
    }
    folded
}

/// Ends every block at the first place control cannot get past, and says whether any changed.
///
/// That is a `trap`, which stays, since it is the stop, and an `unreachable_hint`, which goes,
/// since the `unreachable` put in its place says the same thing as a terminator. The front end
/// writes both in the middle of a block because an expression can hold them and a block cannot
/// end halfway through an expression, and until this runs the code after one is lowered as
/// though it could run. That code is a `ret` after a `ud2` at the end of a function, which gcc
/// never writes and objtool reports as an instruction nothing reaches.
///
/// Nothing a later block reads is lost. A value defined after the cut is read only in blocks
/// the cut block dominates, and with no way out of it those blocks have no way in either.
fn cut(func: &mut Func, fuel: &mut Fuel, stats: &mut Stats) -> bool {
    let mut changed = false;
    for block in func.blocks().collect::<Vec<Block>>() {
        let Some(stop) = func.insts(block).find(|&inst| stops(func, inst)) else { continue };
        let rest: Vec<Inst> =
            std::iter::successors(func.next_inst(stop), |&inst| func.next_inst(inst)).collect();
        if rest.len() == 1 && func[rest[0]].opcode == Opcode::Unreachable {
            continue;
        }
        if !fuel.take() {
            stats.missed(NO_FUEL_CUT);
            continue;
        }
        let span = func.span(stop);
        if func[stop].opcode == Opcode::UnreachableHint {
            func.remove_inst(stop);
        }
        for inst in rest {
            func.remove_inst(inst);
        }
        let end = func.create_inst(InstData::new(Opcode::Unreachable), &[], span);
        func.append_inst(block, end);
        stats.optimized(CUT);
        changed = true;
    }
    changed
}

/// Gives every `switch` whose default goes where control never arrives a default it can take.
///
/// `default: __builtin_unreachable();` is the program promising the value is one of the cases,
/// so the default is free to be any of them. It becomes the last live case, and every case that
/// went there or nowhere stops being a case, which leaves a switch of two arms a branch on one
/// compare and a switch of one arm a jump. Says whether any changed.
fn undefault(func: &mut Func, fuel: &mut Fuel, stats: &mut Stats) -> bool {
    let mut changed = false;
    for block in func.blocks().collect::<Vec<Block>>() {
        let Some(term) = func.terminator(block) else { continue };
        let Extra::Switch(info) = func[term].extra else { continue };
        let calls: Vec<BlockCall> = func[func[info].targets].to_vec();
        let cases: Vec<Imm> = func[func[info].cases].to_vec();
        if !dead_end(func, calls[0].block) {
            continue;
        }
        let Some(last) = calls[1..].iter().rposition(|call| !dead_end(func, call.block)) else {
            continue;
        };
        if !fuel.take() {
            stats.missed(NO_FUEL_DEFAULT);
            continue;
        }
        let default = calls[last + 1];
        let mut targets = vec![default];
        let mut kept = Vec::with_capacity(cases.len());
        for (call, &case) in calls[1..].iter().zip(&cases) {
            let same = call.block == default.block && func[call.args] == func[default.args];
            if !same && !dead_end(func, call.block) {
                targets.push(*call);
                kept.push(case);
            }
        }
        let targets = func.push_block_calls(&targets);
        let cases = func.push_imms(&kept);
        let info = func.add_switch(SwitchInfo { targets, cases });
        func[term].extra = Extra::Switch(info);
        stats.optimized(UNDEFAULTED);
        changed = true;
    }
    changed
}

/// Recorded once for each `switch` whose cases were one run of values going to one place.
const RANGED: &str = "switch whose cases are one run of values going to one place made a branch on \
                      one compare";

/// Recorded for such a `switch` that would have been made a branch if there had been fuel.
const NO_FUEL_RANGED: &str =
    "switch whose cases are one run of values going to one place kept, the pass ran out of fuel";

/// Turns a `switch` whose cases all go to one place, and whose case values are one run with no
/// hole in it, into a branch on one unsigned compare, and says whether any turned.
///
/// `switch (x) { case 0: ... case 15: return x + 1; default: return 0; }` is a branch on
/// `x - low <= high - low` at the end, because the lowering writes a run of cases as one
/// subtraction and one unsigned compare. While it stays a `switch` in the middle end, no pass that
/// reads a `br_if` sees it. `crate::phiopt` is the pass that matters: the two arms above are a
/// diamond that is one `select`, and as a `switch` it stayed a branch. `crate::switch_conv` leaves
/// exactly this shape, so every switch it turns into arithmetic came out as a branch that a stream
/// of values mispredicts (tamnd/rucc#3262). LLVM does the same in `turnSwitchRangeIntoICmp`.
///
/// The labels are read with the sign of the switch's type and sorted, and the subtraction wraps,
/// so a run that crosses zero is one run. A run that covers every value of the type leaves the
/// default nothing, and is left for the lowering. One case is an equality, and a run from zero has
/// no subtraction. Where every case edge has a hint, the edge to the run carries their sum.
fn range_branches(func: &mut Func, fuel: &mut Fuel, stats: &mut Stats) -> bool {
    let mut changed = false;
    for block in func.blocks().collect::<Vec<Block>>() {
        let Some(term) = func.terminator(block) else { continue };
        let Extra::Switch(info) = func[term].extra else { continue };
        let Some(&value) = func[func[term].args].first() else { continue };
        let calls: Vec<BlockCall> = func[func[info].targets].to_vec();
        let Some((&default, cases)) = calls.split_first() else { continue };
        let Some(&first) = cases.first() else { continue };
        let same =
            |call: &BlockCall| call.block == first.block && func[call.args] == func[first.args];
        if !cases.iter().all(same) || same(&default) {
            continue;
        }
        let ty = func[value].ty;
        if !ty.is_int() || ty.bits() >= 128 {
            continue;
        }
        let mut labels: Vec<i128> =
            func[func[info].cases].iter().map(|case| case.signed(ty)).collect();
        labels.sort_unstable();
        if labels.windows(2).any(|pair| pair[1] != pair[0] + 1) {
            continue;
        }
        let (low, high) = (labels[0], labels[labels.len() - 1]);
        let span = high - low;
        if !u128::try_from(span).is_ok_and(|span| span < (1u128 << ty.bits()) - 1) {
            continue;
        }
        if !fuel.take() {
            stats.missed(NO_FUEL_RANGED);
            continue;
        }
        let cond = if span == 0 {
            let low = constant_before(func, term, ty, low);
            compare_before(func, term, IntPred::Eq, value, low)
        } else {
            let from = if low == 0 {
                value
            } else {
                let low = constant_before(func, term, ty, low);
                let args = func.push_values(&[value, low]);
                emit_before(func, term, InstData { args, ..InstData::new(Opcode::Sub) }, ty)
            };
            let span = constant_before(func, term, ty, span);
            compare_before(func, term, IntPred::Ule, from, span)
        };
        let hints: Option<u32> = cases.iter().map(|call| call.hint.taken()).sum();
        let hint = hints.map_or(Hint::NONE, Hint::parts);
        let targets = func.push_block_calls(&[BlockCall { hint, ..first }, default]);
        let args = func.push_values(&[cond]);
        let data = &mut func[term];
        data.opcode = Opcode::BrIf;
        data.args = args;
        data.extra = Extra::Targets(targets);
        stats.optimized(RANGED);
        changed = true;
    }
    changed
}

/// An integer constant of that type, written just before `before`.
fn constant_before(func: &mut Func, before: Inst, ty: Type, value: i128) -> Value {
    let at = func.add_imm(Imm::int(value, ty));
    emit_before(
        func,
        before,
        InstData { extra: Extra::Imm(at), ..InstData::new(Opcode::IConst) },
        ty,
    )
}

/// A compare of two integers, written just before `before`.
fn compare_before(func: &mut Func, before: Inst, pred: IntPred, a: Value, b: Value) -> Value {
    let args = func.push_values(&[a, b]);
    let data = InstData { args, extra: Extra::IntPred(pred), ..InstData::new(Opcode::ICmp) };
    emit_before(func, before, data, Type::I1)
}

/// One instruction with one result, written just before `before` with its source position.
fn emit_before(func: &mut Func, before: Inst, data: InstData, ty: Type) -> Value {
    let span = func.span(before);
    let inst = func.create_inst(data, &[ty], span);
    func.insert_before(inst, before);
    func[inst].first_result.expect("one result was asked for")
}

/// Turns a branch on `!c` into a branch on `c` with its arms the other way round, and says
/// whether any turned.
///
/// A `!` on a `_Bool` is an `xor` with one, and `if (!p->flag)` branched on the `xor`, so the
/// machine code was the load, an `xorb $1`, a `testb` and a jump where gcc writes a test and the
/// jump the other way. Postgres' `XidInMVCCSnapshot` does that on `suboverflowed` and
/// `takenDuringRecovery` for every tuple it looks at (tamnd/rucc#1994).
///
/// Only an `xor` the branch is the one reader of goes, and it goes here rather than being left for
/// [`crate::dce`], because `-O0` runs this pass and nothing after it. A negation something else
/// reads too is left alone, since turning the branch round would keep the `xor` and save nothing.
/// The arms carry their hints with them, so what `__builtin_expect` said about each arm is still
/// said about the same arm.
fn unnegate(func: &mut Func, fuel: &mut Fuel, stats: &mut Stats) -> bool {
    let uses = uses::count(func);
    let mut changed = false;
    for block in func.blocks().collect::<Vec<Block>>() {
        let Some(term) = func.terminator(block) else { continue };
        if func[term].opcode != Opcode::BrIf {
            continue;
        }
        let Extra::Targets(targets) = func[term].extra else { continue };
        let Some(&cond) = func[func[term].args].first() else { continue };
        let Some((not, negated)) = negation(func, cond) else { continue };
        if uses[cond.index()] != 1 {
            continue;
        }
        let [then, otherwise] = func[targets] else { continue };
        if !fuel.take() {
            stats.missed(NO_FUEL_UNNEGATED);
            continue;
        }
        let args = func[term].args;
        func.rewrite(args, |_| negated);
        let turned = func.push_block_calls(&[otherwise, then]);
        func[term].extra = Extra::Targets(turned);
        func.remove_inst(not);
        stats.optimized(UNNEGATED);
        changed = true;
    }
    changed
}

/// The `xor` with one that makes this `i1`, and the value it negates.
fn negation(func: &Func, value: Value) -> Option<(Inst, Value)> {
    let Def::Result { inst, .. } = func[value].def else { return None };
    if func[inst].opcode != Opcode::Xor || func[value].ty != Type::I1 {
        return None;
    }
    let [lhs, rhs] = func[func[inst].args] else { return None };
    let one = |value| constant(func, value).is_some_and(|(imm, _)| imm.unsigned() & 1 == 1);
    if one(rhs) {
        Some((inst, lhs))
    } else if one(lhs) {
        Some((inst, rhs))
    } else {
        None
    }
}

/// Whether control cannot get past an instruction.
fn stops(func: &Func, inst: Inst) -> bool {
    matches!(func[inst].opcode, Opcode::Trap | Opcode::UnreachableHint)
}

/// Whether a block is only the place control does not reach, so that a branch to it is a branch
/// that is never taken.
fn dead_end(func: &Func, block: Block) -> bool {
    func.insts(block).next().is_some_and(|inst| func[inst].opcode == Opcode::Unreachable)
}

/// What a block's parameters hold along one particular edge into it.
///
/// Empty is the honest answer for a question asked about a block rather than about an edge, and it
/// is what this pass passes, since a branch it folds has to fold whichever way control arrived.
/// [`crate::thread`] asks the same question one edge at a time and fills this in, which is the
/// whole difference between folding a branch and threading one.
pub(crate) type Bindings = Map<Value, Value>;

/// The value this one stands for along the edge, which is itself when the edge says nothing.
fn resolve(subst: &Bindings, value: Value) -> Value {
    subst.get(&value).copied().unwrap_or(value)
}

/// Where this terminator always goes, if it always goes to one place.
///
/// `None` is every reason not to fold and does not say which, because the answer to all of them
/// is to leave the branch alone.
///
/// Shared with [`crate::thread`], which asks it under a `subst` that binds the block's parameters
/// to what one edge into the block carries. Two answers about when a branch is decided would be
/// two compilers, and the threading pass would be the one nobody checked.
pub(crate) fn taken(func: &Func, term: Inst, subst: &Bindings) -> Option<BlockCall> {
    let data = &func[term];
    let arg = *func[data.args].first()?;
    match data.opcode {
        Opcode::BrIf => {
            let Extra::Targets(targets) = data.extra else { return None };
            if let Some(call) = one_place(func, &func[targets]) {
                return Some(call);
            }
            // The first target is the one taken when the condition is one, which is what
            // `Builder::br_if` writes and what the printer reads back. A known condition goes
            // first, even when the arm it picks is a dead end. `do { BUG(); } while (0);` ends
            // its body in an asm and `__builtin_unreachable()` in 6.1, and the test after it is
            // a zero whose false arm is the dead end. Taking the other arm there made the body
            // a loop, a `jmp` back to the `ud2` that objtool reports as unreachable.
            if let Some(bit) = known(func, arg, subst) {
                return func[targets].get(usize::from(!bit)).copied();
            }
            // An arm that goes where control never arrives is an arm the program promised is
            // never taken, which is how `if (x) __builtin_unreachable();` says what `x` is.
            if let [then, otherwise] = func[targets] {
                match (dead_end(func, then.block), dead_end(func, otherwise.block)) {
                    (true, false) => return Some(otherwise),
                    (false, true) => return Some(then),
                    _ => {}
                }
            }
            None
        }
        Opcode::Switch => {
            let Extra::Switch(at) = data.extra else { return None };
            let info = func[at];
            if let Some(call) = one_place(func, &func[info.targets]) {
                return Some(call);
            }
            let (value, _) = constant(func, resolve(subst, arg))?;
            // The default is the first target and the cases follow it in the order their values
            // are in, so the target for a case that matches is one past the value's own place.
            let case = func[info.cases].iter().position(|it| *it == value);
            func[info.targets].get(case.map_or(0, |case| case + 1)).copied()
        }
        _ => None,
    }
}

/// The one edge every arm of a branch is, when they are all the same edge.
///
/// Section 21.1's branch simplification, the half of it that is not about a constant. A branch
/// whose arms all go to the same block with the same arguments goes there whatever the condition
/// says, so it is a jump, and the condition becomes something nothing reads for
/// [`crate::dce`] to take out.
///
/// The arguments have to match and not only the block. Two edges to one block carrying different
/// arguments are two different edges, and that is the whole reason this IR passes arguments along
/// an edge rather than writing a phi in the block: `if (c) goto L(1); else goto L(2);` is a real
/// program and turning it into a jump would have to pick one of the two numbers.
fn one_place(func: &Func, calls: &[BlockCall]) -> Option<BlockCall> {
    let &first = calls.first()?;
    let same = |call: &BlockCall| call.block == first.block && func[call.args] == func[first.args];
    calls[1..].iter().all(same).then_some(first)
}

/// Rewrites the terminator as a jump to that one of its targets.
///
/// In place, and the target keeps the arguments it already had, because the arguments belong to
/// the edge and the edge is the one that survives.
pub(crate) fn jump_to(func: &mut Func, term: Inst, call: BlockCall) {
    let targets = func.push_block_calls(&[call]);
    let args = func.push_values(&[]);
    let data = &mut func[term];
    data.opcode = Opcode::Jump;
    data.args = args;
    data.extra = Extra::Targets(targets);
}

/// Takes every block the entry does not reach out of the function.
///
/// The cache goes with them, because what it is holding is answers about a function that had them
/// in it, and the pass is not finished asking.
///
/// Shared with [`crate::thread`]. Section 6.5 makes this a standing obligation of whichever pass
/// stranded the block rather than an optimization of this one, the verifier holds every pass to it,
/// and a second walk written next door would be a second answer about what reachable means.
pub(crate) fn sweep(func: &mut Func, an: &mut Analyses, stats: &mut Stats) {
    let gone = stranded(func, an);
    if gone.is_empty() {
        return;
    }
    for block in gone {
        func.remove_block(block);
        stats.optimized(REMOVED);
    }
    an.clear();
}

/// The blocks the entry cannot reach, in block order.
///
/// This is reachability as the verifier counts it, which is over the edges the terminators name
/// and additionally over the blocks a `block_addr` mentions. A block whose address is taken is
/// arrived at by an `indirect_br` somewhere, and that instruction lists every block the address
/// can hold, so the edge is already in the graph from the place control really leaves. What the
/// graph does not carry is the `block_addr` itself, and deleting the block under one would leave
/// an instruction naming a block that is not there.
fn stranded(func: &Func, an: &mut Analyses) -> Vec<Block> {
    let cfg = an.cfg(func);
    let Some(entry) = cfg.entry() else { return Vec::new() };
    let mut seen = vec![false; cfg.capacity()];
    seen[entry.index()] = true;
    let mut stack = vec![entry];
    // And every block an image names, whatever the entry reaches. The `indirect_br` that arrives at
    // one of those can be in another function, so unlike a `block_addr` below there is nothing in
    // this function that says the block is still wanted.
    for (block, _) in func.named_blocks() {
        if !seen[block.index()] {
            seen[block.index()] = true;
            stack.push(block);
        }
    }
    let mut reached = Vec::new();
    while let Some(block) = stack.pop() {
        for &succ in cfg.successors(block) {
            if !seen[succ.index()] {
                seen[succ.index()] = true;
                stack.push(succ);
            }
        }
        reached.push(block);
    }
    // The addresses in a second walk over the blocks the first one reached, because an address
    // taken in a block nothing reaches is an address nothing takes.
    let mut next = reached;
    while !next.is_empty() {
        let mut found = Vec::new();
        for block in next {
            for inst in func.insts(block) {
                if func[inst].opcode != Opcode::BlockAddr {
                    continue;
                }
                for call in func.successors(inst) {
                    if !seen[call.block.index()] {
                        seen[call.block.index()] = true;
                        found.push(call.block);
                    }
                }
            }
        }
        // Everything the newly kept blocks reach is kept too, which is what makes this a fixed
        // point rather than one extra step.
        let mut stack = found.clone();
        while let Some(block) = stack.pop() {
            for &succ in cfg.successors(block) {
                if !seen[succ.index()] {
                    seen[succ.index()] = true;
                    stack.push(succ);
                    found.push(succ);
                }
            }
        }
        next = found;
    }
    func.blocks().filter(|block| !seen[block.index()]).collect()
}

/// Where every edge that arrives at a block was written down, and which block it left.
///
/// A block call rather than a predecessor, because both halves of step three edit the edge and
/// neither of them can find it again from the block it goes to. Redirecting one wants the slot in
/// the pool, and taking a block parameter away wants the slot too, so this is what the step keeps
/// instead of a [`crate::Cfg`].
pub(crate) type Edges = Map<Block, Vec<(Block, Idx<BlockCall>)>>;

/// Every edge in the function, filed under the block it arrives at.
///
/// Terminators only. A `block_addr` names a block and is not an edge, which is the same
/// distinction [`stranded`] draws from the other side.
///
/// Shared with [`crate::thread`], which edits edges as well and so wants the slot in the pool for
/// the same reason this step does.
pub(crate) fn incoming(func: &Func) -> Edges {
    // Room for every block from the start, rather than the table growing and hashing all of it
    // again on the way. Nothing walks it in order, so how big it is changes nothing it answers.
    let mut edges: Edges = Map::with_capacity_and_hasher(func.counts().blocks, Default::default());
    for block in func.blocks() {
        let Some(term) = func.terminator(block) else { continue };
        for at in func.target_list(term).iter() {
            edges.entry(func[at].block).or_default().push((block, at));
        }
    }
    edges
}

/// Takes out every block parameter nothing reads, and the argument in its place on every edge.
///
/// This module's documentation is the argument. [`live`] is where the reading is done and this is
/// what acts on it: one walk, in block order, because the answer is about the whole function and a
/// worklist would only be asking the same question again.
///
/// # Fuel
///
/// One unit per parameter, and running out stops the removals rather than the looking, which this
/// step can do because it has already worked out the whole answer. The parameters of a block are
/// taken together once they have all been paid for, since an argument list that has lost some of
/// its entries and not others is a function the verifier refuses.
fn drop_unread(func: &mut Func, fuel: &mut Fuel, stats: &mut Stats) -> bool {
    let Some(entry) = func.entry() else { return false };
    let live = live(func, entry, &addressed(func));
    let edges = incoming(func);
    let mut changed = false;
    let mut gone: Set<Value> = Set::default();
    for block in func.blocks().collect::<Vec<Block>>() {
        let mut taking = Vec::new();
        for (index, &param) in func[block].params.iter().enumerate() {
            if live.contains(&param) {
                continue;
            }
            if !fuel.take() {
                stats.missed(NO_FUEL_UNREAD);
                continue;
            }
            taking.push(index);
        }
        if taking.is_empty() {
            continue;
        }
        for _ in &taking {
            stats.optimized(NOTHING_READS_IT);
        }
        gone.extend(taking.iter().map(|&index| func[block].params[index]));
        take_params(func, block, &taking, edges.get(&block));
        changed = true;
    }
    if !gone.is_empty() {
        strand(func, gone);
    }
    changed
}

/// Takes out the instructions that were reading a parameter the step above removed.
///
/// Without this the step above leaves a use of a value nothing defines, which is a function the
/// verifier is entitled to refuse and a printed IR that does not read back. It was written the
/// other way round, on the argument that what is stranded is dead and [`crate::dce`] is the pass
/// that takes what is dead. That holds at every level except the one that matters most, because
/// `-O0` runs this pass and no other, so at `-O0` nothing came along afterwards and the dangling
/// operand reached the printer. That was issue 1016.
///
/// Every instruction this removes is dead, and the proof is [`live`] read backwards. Live spreads
/// from a result to the operands that produced it, so an instruction whose result were live would
/// have made its operands live, and the parameter it reads was removed exactly because nothing
/// made it live. A terminator's operands and an effectful instruction's operands are live by
/// seeding, so neither of those can be reading a removed parameter either. The same argument
/// applies again to the results of what goes, which is why this follows the chain.
///
/// # Fuel
///
/// None, for the reason this module's documentation gives about the blocks a fold strands: this is
/// the second half of a transformation that has already been paid for, and a budget that could
/// stop between the two halves would hand the verifier a use with no definition.
fn strand(func: &mut Func, mut gone: Set<Value>) {
    loop {
        let mut spread = false;
        for block in func.blocks().collect::<Vec<Block>>() {
            for inst in func.insts(block).collect::<Vec<Inst>>() {
                if !func[func[inst].args].iter().any(|value| gone.contains(value)) {
                    continue;
                }
                let results: Vec<Value> = func[inst].results().collect();
                for result in results {
                    spread |= gone.insert(result);
                }
                func.remove_inst(inst);
            }
        }
        // A second walk only when something this removed produced a value of its own, since that
        // value may be read further down and layout order is not a promise about where.
        if !spread {
            return;
        }
    }
}

/// Every value something that happens reads, worked out from nothing live outwards.
///
/// The seeds are the operands of the instructions that have to happen, which are the terminators
/// and the ones with effects. A terminator's own operands are seeds and the arguments it passes to
/// the blocks it branches to are not, and that split is the whole point: an argument is read only
/// if the parameter it lands in is read, so it waits for that parameter to be reached.
///
/// Then live spreads two ways. From a value an instruction produced, to that instruction's
/// operands, because producing it meant reading them. From a parameter, to the argument in its
/// place on every edge into the block, because arriving there meant passing them.
///
/// The entry block's parameters are the function's own and are live by declaration rather than by
/// use, and so are the parameters of a block whose address is taken, because an `indirect_br` is a
/// way in that this reads from the wrong end.
fn live(func: &Func, entry: Block, addressed: &Set<Block>) -> Set<Value> {
    let mut where_from: Map<Value, (Block, usize)> = Map::default();
    let mut live: Set<Value> = Set::default();
    let mut work: Vec<Value> = Vec::new();
    let seed = |value: Value, live: &mut Set<Value>, work: &mut Vec<Value>| {
        if live.insert(value) {
            work.push(value);
        }
    };
    for block in func.blocks() {
        let held = block == entry || addressed.contains(&block);
        for (index, &param) in func[block].params.iter().enumerate() {
            where_from.insert(param, (block, index));
            if held {
                seed(param, &mut live, &mut work);
            }
        }
        for inst in func.insts(block) {
            if !func.is_terminator(inst) && !func[inst].opcode.has_effects() {
                continue;
            }
            for &value in &func[func[inst].args] {
                seed(value, &mut live, &mut work);
            }
        }
    }

    let edges = incoming(func);
    while let Some(value) = work.pop() {
        match func[value].def {
            Def::Result { inst, .. } => {
                for &operand in &func[func[inst].args] {
                    seed(operand, &mut live, &mut work);
                }
            }
            Def::Param { .. } => {
                let Some(&(block, index)) = where_from.get(&value) else { continue };
                for &(_, at) in edges.get(&block).into_iter().flatten() {
                    let Some(&arg) = func[func[at].args].get(index) else { continue };
                    seed(arg, &mut live, &mut work);
                }
            }
        }
    }
    live
}

/// Section 21.4's step three, both halves of it, on one worklist. Says whether anything changed.
///
/// Forwarder removal and redundant block parameter removal are one step because each is the other's
/// reason to look again. Pointing a block's predecessors past it hands the block below several ways
/// in where there was one, and a parameter that was obviously one value may stop being one, or
/// several arguments that were the same may now arrive together and make one; taking a parameter
/// away can leave a block with nothing but its jump, which is the whole of what a forwarder is.
///
/// A block goes back on the worklist when an edge into it or out of it moved, and the loop stops
/// when nothing has moved. That is a fixed point, and it is the one section 21.4 asks for, over the
/// step rather than over the pass.
///
/// What this does not do is put the two halves in a particular order within a block. Parameters
/// first is not a policy, it is the only order that gets a forwarder with a redundant parameter in
/// one visit rather than two.
///
/// # Fuel
///
/// One unit for each forwarder and one for each parameter, and the first refusal is where the step
/// stops rather than where it starts skipping. The other steps go on looking after they run out and
/// say so once for each thing they did not do, which they can because each of them walks the
/// function once. This one comes back to a block whenever an edge near it moved, so a refusal
/// counted per visit would count one opportunity several times and the number would say more about
/// the shape of the worklist than about the function. A budget that has reached zero is not going
/// to have anything in it later, so there is one refusal recorded and it is the true one.
fn straighten(
    func: &mut Func,
    fuel: &mut Fuel,
    stats: &mut Stats,
    forward: &mut Map<Value, Value>,
) -> bool {
    let Some(entry) = func.entry() else { return false };
    let addressed = addressed(func);
    let mut edges = incoming(func);
    let mut work: VecDeque<Block> = func.blocks().collect();
    let mut queued: Set<Block> = work.iter().copied().collect();
    let mut gone: Set<Block> = Set::default();
    let mut changed = false;
    while let Some(block) = work.pop_front() {
        queued.remove(&block);
        if gone.contains(&block) {
            continue;
        }
        let mut starved = false;
        if block != entry {
            let drop = redundant(func, block, edges.get(&block), forward);
            let mut taking = Vec::new();
            for (index, same) in drop {
                if !fuel.take() {
                    stats.missed(NO_FUEL_PARAM);
                    starved = true;
                    break;
                }
                let value = match same {
                    Same::Value(value) => value,
                    Same::Constant(imm, ty) => made(func, block, index, imm, ty),
                };
                // Through what an earlier one already decided, the same way merging does, because
                // a parameter can be redundant on an argument that is on its way somewhere else.
                let value = uses::chase(forward, value);
                forward.insert(func[block].params[index], value);
                taking.push(index);
                stats.optimized(SAME_EVERY_WAY);
            }
            if !taking.is_empty() {
                take_params(func, block, &taking, edges.get(&block));
                // Itself, because a block that has run out of parameters may be a forwarder now,
                // and because a parameter can be redundant on one that just went.
                requeue(block, &mut work, &mut queued);
                // And the blocks below, because a parameter passed straight on down is the shape
                // section 21.2 means by one removal making the next one possible.
                if let Some(term) = func.terminator(block) {
                    for call in func.successors(term).collect::<Vec<BlockCall>>() {
                        requeue(call.block, &mut work, &mut queued);
                    }
                }
                changed = true;
            }
        }
        // What was already paid for is applied first, and then the step stops, because a block
        // whose parameters half went is a block whose edges have to agree with it.
        if starved {
            break;
        }
        let Some((term, into, args)) = forwards(func, block, entry, &addressed, &edges) else {
            continue;
        };
        if !fuel.take() {
            stats.missed(NO_FUEL_FORWARD);
            break;
        }
        // The block's own edge stops existing along with the block, and it has to come out of the
        // record before its predecessors' edges go in, or the block below would be told it has a
        // way in from a block that is not there.
        let out = func.target_list(term).iter().next().expect("a jump has a target");
        if let Some(list) = edges.get_mut(&into) {
            list.retain(|&(_, at)| at != out);
        }
        let ins = edges.remove(&block).unwrap_or_default();
        for &(_, at) in &ins {
            // A list of its own for each edge rather than one shared between them, because a
            // later substitution rewrites a list in place and a shared one would be rewritten
            // once for every edge that named it.
            let call = func[at];
            let args = func.push_values(&args);
            func.set_block_call(at, BlockCall { block: into, args, ..call });
        }
        edges.entry(into).or_default().extend(ins.iter().copied());
        func.remove_block(block);
        gone.insert(block);
        stats.optimized(FORWARDED);
        changed = true;
        requeue(into, &mut work, &mut queued);
        for &(from, _) in &ins {
            requeue(from, &mut work, &mut queued);
        }
    }
    changed
}

/// Puts a block back on the worklist, if it is not on it already.
fn requeue(block: Block, work: &mut VecDeque<Block>, queued: &mut Set<Block>) {
    if queued.insert(block) {
        work.push_back(block);
    }
}

/// Which of a block's parameters arrive as the same value every way in, and what that value is.
///
/// Section 21.2. A parameter that is `x` from one edge and `x` from every other is not carrying
/// anything, it is spelling `x` a second way, and document 12's hash consing cannot see through the
/// spelling, so two equal values look different for as long as it is there.
///
/// The one subtlety is an argument that is the parameter itself, which is what a loop header looks
/// like: the preheader passes `init` and the latch passes the parameter back. Reading that
/// literally says two different values and the answer is `init`, because a value that can only ever
/// be itself or `init` was `init` to begin with. So a self reference is not an argument for this
/// purpose, which is the same optimistic reading section 14.1 takes.
///
/// A block with no way in gets nothing said about it. That is an unreachable block, [`sweep`] has
/// already run, and answering `init` for a parameter with no arguments at all would be inventing
/// one.
fn redundant(
    func: &Func,
    block: Block,
    ins: Option<&Vec<(Block, Idx<BlockCall>)>>,
    forward: &Map<Value, Value>,
) -> Vec<(usize, Same)> {
    let Some(ins) = ins.filter(|ins| !ins.is_empty()) else { return Vec::new() };
    let mut found = Vec::new();
    for (index, &param) in func[block].params.iter().enumerate() {
        let mut only = None;
        let mut agree = true;
        let mut number = None;
        let mut numbers = true;
        for &(_, at) in ins {
            let list = func[at].args;
            let Some(&arg) = func[list].get(index) else {
                // Fewer arguments than parameters is a function the verifier will refuse, and
                // guessing what the missing one was is not this pass's job.
                agree = false;
                break;
            };
            let arg = uses::chase(forward, arg);
            if arg == param {
                continue;
            }
            if numbers {
                let this = numbered(func, arg);
                numbers = this.is_some() && (number.is_none() || number == this);
                number = this;
            }
            match only {
                None => only = Some(arg),
                Some(seen) if seen == arg => {}
                Some(_) => agree = false,
            }
            if !agree && !numbers {
                break;
            }
        }
        match (only, number) {
            (Some(value), _) if agree => found.push((index, Same::Value(value))),
            (Some(_), Some((imm, ty))) if numbers => found.push((index, Same::Constant(imm, ty))),
            _ => {}
        }
    }
    found
}

/// What a redundant parameter arrives as every way in.
///
/// Either one value, or one constant that each edge made for itself. The second is how two inlined
/// `return NULL` paths meet: each arm has its own `iconst` and its own `inttoptr`, which value
/// numbering leaves apart on purpose, so the parameter is the same null pointer from every edge
/// without being the same value. `bh_end_async_read` in fs/buffer.c is that shape, and the test
/// of the pointer below it is what keeps `fsverity_enqueue_verify_work` and its `WARN_ON` alive.
enum Same {
    Value(Value),
    Constant(Imm, Type),
}

/// Writes the constant a parameter always arrives as at the top of its block, as an integer or as
/// the pointer made from one, and gives back the value that stands for it now.
fn made(func: &mut Func, block: Block, index: usize, imm: Imm, ty: Type) -> Value {
    let param = func[block].params[index];
    let wanted = func[param].ty;
    let first = func.insts(block).next().expect("a block ends in a terminator");
    let span = func.span(first);
    let at = func.add_imm(imm);
    let data = InstData { extra: Extra::Imm(at), ..InstData::new(Opcode::IConst) };
    let number = func.create_inst(data, &[ty], span);
    func.insert_before(number, first);
    let value = func[number].results().next().expect("a constant is one value");
    if wanted == ty {
        return value;
    }
    let args = func.push_values(&[value]);
    let data = InstData { args, ..InstData::new(Opcode::IntToPtr) };
    let pointer = func.create_inst(data, &[wanted], span);
    func.insert_before(pointer, first);
    func[pointer].results().next().expect("a conversion is one value")
}

/// Drops those parameters of a block and the arguments in their places on every edge into it.
///
/// Both halves together, because a block whose parameters and arguments disagree in number is one
/// the verifier refuses, and section 21.6 says that is the most common bug in this document.
fn take_params(
    func: &mut Func,
    block: Block,
    taking: &[usize],
    ins: Option<&Vec<(Block, Idx<BlockCall>)>>,
) {
    for &(_, at) in ins.into_iter().flatten() {
        let call = func[at];
        let kept: Vec<Value> = func[call.args]
            .iter()
            .enumerate()
            .filter(|(index, _)| !taking.contains(index))
            .map(|(_, &value)| value)
            .collect();
        let args = func.push_values(&kept);
        func.set_block_call(at, BlockCall { args, ..call });
    }
    let mut index = 0;
    func.retain_params(block, |_| {
        let keep = !taking.contains(&index);
        index += 1;
        keep
    });
}

/// Where a block forwards to and what it passes on, when it is a forwarder.
///
/// The conditions are in this module's documentation, and every one of them is a `None` here. What
/// comes back is the terminator, the block below, and the arguments the jump was carrying, which
/// are what each of the block's predecessors will be carrying instead.
fn forwards(
    func: &Func,
    block: Block,
    entry: Block,
    addressed: &Set<Block>,
    edges: &Edges,
) -> Option<(Inst, Block, Vec<Value>)> {
    if block == entry || addressed.contains(&block) || !func[block].params.is_empty() {
        return None;
    }
    let term = func.terminator(block)?;
    if func[term].opcode != Opcode::Jump {
        return None;
    }
    // Nothing above the jump, which is what "no instructions" means once the jump is counted as
    // one of them.
    if func.insts(block).count() != 1 {
        return None;
    }
    let call = func.successors(term).next()?;
    if call.block == block {
        return None;
    }
    if carrying(func, block, call.block, func[call.args].len(), edges) {
        return None;
    }
    if func[call.args].iter().any(|&arg| from_a_terminator(func, arg)) {
        return None;
    }
    Some((term, call.block, func[call.args].to_vec()))
}

/// Whether a value is a result of the instruction that ends its block, which only an `asm goto`
/// has, and which an edge out of that block cannot carry.
fn from_a_terminator(func: &Func, value: Value) -> bool {
    match func[value].def {
        Def::Result { inst, .. } => func.is_terminator(inst),
        _ => false,
    }
}

/// Whether taking this forwarder out would put arguments on an edge that has nowhere to move them.
///
/// An edge carries values when the block it arrives at takes parameters, and giving a parameter its
/// value is a move that has to happen on the edge itself. An edge out of a block that goes two ways
/// and into a block arrived at two ways has no block to put that move in, so the back end splits it
/// and puts an empty block back on it, which is `rucc_codegen::split::critical`. A forwarder that
/// carries arguments and whose predecessor branches is already that block, sitting in the place the
/// layout wants it rather than at the end where the splitter has to append it. Taking it out and
/// having it put back costs a jump and a longer live range, and the measurement on the corpus says
/// it costs enough to see, so it is not taken out.
///
/// A forwarder that carries nothing is removed whatever the edges look like, because there is no
/// move to find a place for and the splitter would leave the edge alone as well.
fn carrying(func: &Func, block: Block, into: Block, args: usize, edges: &Edges) -> bool {
    if args == 0 {
        return false;
    }
    let ins = edges.get(&block).map_or(0, Vec::len);
    let after = edges.get(&into).map_or(0, Vec::len) - 1 + ins;
    if after < 2 {
        return false;
    }
    edges.get(&block).into_iter().flatten().any(|&(from, _)| {
        let Some(term) = func.terminator(from) else { return false };
        func.target_list(term).iter().count() >= 2
    })
}

/// The runs of blocks that are one block written as several, head first.
///
/// Section 21.1's block merging, and the doc calls it a pure win for a reason worth stating: it
/// does not delete an instruction or move one earlier, it takes a boundary out. Every analysis
/// that is cheap inside a block and expensive across one gets more of the cheap kind, which is
/// most of them, and the branch that stops being a branch is the smallest part of it.
///
/// A block goes into the one above it when the one above it ends in a jump and this is the only
/// way in. Both halves are needed. One way in and a `br_if` above means the other arm would lose
/// its terminator, and a jump above with two ways in means the second predecessor would arrive in
/// the middle of a block.
///
/// The refusals are the entry block, which has to stay where control arrives even when one block
/// jumps to it; a block that jumps to itself, whose one predecessor is itself; and a block whose
/// address is taken, which is arrived at by an `indirect_br` the graph reads from the other end.
///
/// The answer is chains rather than pairs because a run of three is ordinary and the middle one
/// stops existing partway through. Each block is the head of at most one of these and the tail of
/// at most one, so what comes out is disjoint paths, and starting only from a head is what leaves
/// a ring of blocks that all point at each other alone rather than walking it forever.
fn chains(func: &Func, an: &mut Analyses) -> Vec<Vec<Block>> {
    let cfg = an.cfg(func);
    let Some(entry) = cfg.entry() else { return Vec::new() };
    let addressed = addressed(func);
    let mut below = Map::default();
    let mut is_below = Set::default();
    for block in func.blocks() {
        let Some(term) = func.terminator(block) else { continue };
        if func[term].opcode != Opcode::Jump {
            continue;
        }
        let Some(call) = func.successors(term).next() else { continue };
        let into = call.block;
        let preds = cfg.predecessors(into);
        if into == entry || into == block || addressed.contains(&into) {
            continue;
        }
        if preds.len() != 1 || preds[0] != block {
            continue;
        }
        below.insert(block, into);
        is_below.insert(into);
    }
    let heads = func.blocks().filter(|it| below.contains_key(it) && !is_below.contains(it));
    heads
        .map(|head| {
            let mut chain = vec![head];
            let mut at = head;
            while let Some(&next) = below.get(&at) {
                chain.push(next);
                at = next;
            }
            chain
        })
        .collect()
}

/// Merges one block into the block above it, when that is the only way in, and says whether it did.
///
/// Step four of this pass asked for one chain at a time over the whole function, which is what it
/// wants and is the wrong shape for a caller holding two particular blocks. [`crate::short_circuit`]
/// is that caller. It has just left a head that jumps to a join nothing else reaches any more, and
/// the jump and the join's parameter between them are what stands in the way of the collapse above
/// this one: the next `&&` up asks whether its condition is a comparison, and a block parameter is
/// not one until the block it belongs to has been merged away.
///
/// It is this function rather than one written next door for the reason the rest of the sharing in
/// this module has. Two answers about when a block may be folded into the one above it would be two
/// compilers, and the conditions are not obvious ones: the entry keeps its own block, a block whose
/// address is taken is arrived at from somewhere this cannot see, and a block that jumps to itself
/// is not above anything.
pub(crate) fn merge_below(func: &mut Func, an: &mut Analyses, head: Block, into: Block) -> bool {
    let cfg = an.cfg(func);
    let Some(entry) = cfg.entry() else { return false };
    if into == entry || into == head || cfg.predecessors(into) != [head] {
        return false;
    }
    let Some(term) = func.terminator(head) else { return false };
    if func[term].opcode != Opcode::Jump || addressed(func).contains(&into) {
        return false;
    }
    let mut forward = Map::default();
    merge(func, head, into, &mut forward);
    uses::substitute(func, &forward);
    an.clear_merging(func, &[(into, head)]);
    true
}

/// Moves everything in a block into the head of its chain and takes the block out of the function.
///
/// The jump is what is really being deleted, and the arguments it carried are what the merged
/// block's parameters were going to be told. Binding each parameter to the argument in its place
/// and pointing every reader at it is exactly what the jump was doing at run time, so the record
/// goes in the map and the whole map is spent in one walk when the pass is done.
fn merge(func: &mut Func, head: Block, block: Block, forward: &mut Map<Value, Value>) {
    let term = func.terminator(head).expect("the head of a chain ends in a jump");
    let call = func.successors(term).next().expect("a jump goes somewhere");
    let args = func[call.args].to_vec();
    let params = func[block].params.clone();
    for (param, arg) in params.into_iter().zip(args) {
        // Through whatever the merge above this one already decided, because a chain of three
        // binds the middle block's parameter to something the head passed and then binds the last
        // block's parameter to that same parameter.
        let arg = uses::chase(forward, arg);
        forward.insert(param, arg);
    }
    func.remove_inst(term);
    // The top of the block going away is right after what the head ends with before the move, and a
    // declaration that starts holding a value there has to be told, since nothing else will say.
    let top = func.insts_backwards(head).next();
    for inst in func.insts(block).collect::<Vec<Inst>>() {
        func.remove_inst(inst);
        func.append_inst(head, inst);
    }
    func.carry_starts(block, head, top);
    func.remove_block(block);
}

/// Whether this condition is always true or always false, given what the edge binds.
fn known(func: &Func, value: Value, subst: &Bindings) -> Option<bool> {
    truth(func, value, subst, LOGIC_DEPTH)
}

/// How many `xor`, `and` and `or` of one bit [`truth`] looks through before it gives up.
const LOGIC_DEPTH: u32 = 4;

/// [`known`], looking through the logic of one bit values as far as `depth` allows.
///
/// A `!` on a `_Bool` is an `xor` with one, and an inlined function that returns a `_Bool` hands
/// it to its caller as a block parameter. So `if (!begin(p)) return -EFAULT;` branches on the
/// `xor` of a parameter that is a constant along each edge in, and without this the branch is
/// decided on neither. The kernel's `user_access_begin` is that shape, and when the edge on which
/// the check failed was not threaded, it joined the other one after the `stac`, which objtool
/// reads as a `clac` with nothing to close.
fn truth(func: &Func, value: Value, subst: &Bindings, depth: u32) -> Option<bool> {
    let value = resolve(subst, value);
    if let Some((imm, _)) = constant(func, value) {
        return Some(imm.unsigned() != 0);
    }
    let Def::Result { inst, .. } = func[value].def else { return None };
    let data = &func[inst];
    let logic = matches!(data.opcode, Opcode::Xor | Opcode::And | Opcode::Or);
    if !logic || func[value].ty != Type::I1 || depth == 0 {
        return compared(func, value, subst);
    }
    let [lhs, rhs] = func[data.args] else { return None };
    let lhs = truth(func, lhs, subst, depth - 1);
    let rhs = truth(func, rhs, subst, depth - 1);
    match (data.opcode, lhs, rhs) {
        (Opcode::Xor, Some(lhs), Some(rhs)) => Some(lhs != rhs),
        (Opcode::And, Some(false), _) | (Opcode::And, _, Some(false)) => Some(false),
        (Opcode::And, Some(true), Some(true)) => Some(true),
        (Opcode::Or, Some(true), _) | (Opcode::Or, _, Some(true)) => Some(true),
        (Opcode::Or, Some(false), Some(false)) => Some(false),
        _ => None,
    }
}

/// What a comparison of two constants comes out as.
///
/// The comparison itself is never rewritten here, and it does not have to be. [`crate::fold`]
/// evaluates one whose operands are both already constants, so what is left for this is the case
/// folding cannot see: an operand that is a constant only along the edge being followed, which is
/// what `subst` carries. That is the whole reason this is still a question worth asking after
/// folding has run.
///
/// The arithmetic is [`crate::fold::compare`] rather than a copy of it, so the two places cannot
/// come to differ about what `slt` means.
fn compared(func: &Func, value: Value, subst: &Bindings) -> Option<bool> {
    let Def::Result { inst, .. } = func[value].def else { return None };
    let data = &func[inst];
    if data.opcode != Opcode::ICmp {
        return None;
    }
    let Extra::IntPred(pred) = data.extra else { return None };
    let args = &func[data.args];
    let (lhs, ty) = numbered(func, resolve(subst, *args.first()?))?;
    let (rhs, _) = numbered(func, resolve(subst, *args.get(1)?))?;
    Some(crate::fold::compare(pred, lhs, rhs, ty))
}

/// The constant this value is, or the constant a pointer made from one is, which is how a null
/// pointer is written. An inlined function returning `NULL` on some paths hands the caller a
/// block parameter that is that pointer along those edges, and `if (p)` in the caller is decided
/// on each of them. `__schedstats_from_dl_se` in kernel/sched/deadline.c is that shape.
fn numbered(func: &Func, value: Value) -> Option<(Imm, Type)> {
    if let Some(found) = constant(func, value) {
        return Some(found);
    }
    let Def::Result { inst, .. } = func[value].def else { return None };
    if func[inst].opcode != Opcode::IntToPtr {
        return None;
    }
    constant(func, *func[func[inst].args].first()?)
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{
        Block, Builder, Def, Extra, Flags, Func, Hint, Inst, IntPred, MemInfo, MemOrder, Module,
        Opcode, Restrict, Signature, Type, Value,
    };
    use rucc_target::{Arch, Env, Os, TargetInfo, Triple};

    use super::SimplifyCfg;
    use crate::stats::Kind;
    use crate::testing::graph;
    use crate::{Fuel, Pass, Preserved, Stats};

    /// Runs the pass with as much fuel as it wants.
    fn simplify(func: &mut Func) -> Stats {
        SimplifyCfg.run(func, &mut crate::machine::fixtures::analyses(), &mut Fuel::unlimited())
    }

    /// The blocks the function still has, by number.
    fn blocks(func: &Func) -> Vec<usize> {
        func.blocks().map(Block::index).collect()
    }

    /// The opcode of a block's terminator.
    fn terminator(func: &Func, block: usize) -> Opcode {
        let block = Block::from_usize(block);
        func[func.terminator(block).expect("every block here has one")].opcode
    }

    /// Where a block's terminator goes, as block numbers.
    fn goes_to(func: &Func, block: usize) -> Vec<usize> {
        let block = Block::from_usize(block);
        let term = func.terminator(block).expect("every block here has one");
        func.successors(term).map(|call| call.block.index()).collect()
    }

    /// The block the instruction that produced a value is in now, if it is in one.
    ///
    /// Which arm of a branch survived is a question about where its code ended up rather than
    /// about the shape of the graph, because the arm that survives is merged into the block above
    /// it in the same run and the two blocks stop being two.
    fn lives_in(func: &Func, value: Value) -> Option<usize> {
        let Def::Result { inst, .. } = func[value].def else { return None };
        func.block_of(inst).map(Block::index)
    }

    /// A function with an entry, a `br_if` on `cond`, two arms and a join.
    ///
    /// The condition is built by the caller out of the builder it is handed, which is what lets
    /// one shape stand for a constant, a comparison and a value nothing knows anything about. Each
    /// arm holds one instruction that does nothing, which is there to be told apart from the one
    /// in the other arm, and the two of them come back with the function.
    fn diamond(cond: impl FnOnce(&mut Builder<'_>) -> Value) -> (Func, [Value; 2]) {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let then_block = func.create_block();
        let else_block = func.create_block();
        let join = func.create_block();
        let mut build = Builder::new(&mut func, entry);
        let cond = cond(&mut build);
        build.br_if(cond, then_block, &[], else_block, &[]);
        let mut marks = Vec::new();
        for (arm, mark) in [(then_block, 111), (else_block, 222)] {
            let mut build = Builder::new(&mut func, arm);
            marks.push(build.iconst(Type::int(32), mark));
            build.jump(join, &[]);
        }
        let mut build = Builder::new(&mut func, join);
        build.ret(&[]);
        (func, [marks[0], marks[1]])
    }

    #[test]
    fn a_branch_on_a_true_constant_becomes_a_jump_to_the_first_arm() {
        let (mut func, [taken, other]) = diamond(|build| build.iconst(Type::int(1), 1));
        let stats = simplify(&mut func);
        assert!(stats.changed());
        assert_eq!(stats.count(Kind::Optimized, super::FOLDED), 1);
        // The arm it did not take is gone, because nothing else went there, and the arm it did
        // take had one way in and went into the entry along with the join below it.
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED), 1);
        assert_eq!(stats.count(Kind::Optimized, super::MERGED), 2);
        assert_eq!(lives_in(&func, taken), Some(0));
        assert_eq!(lives_in(&func, other), None);
        assert_eq!(blocks(&func), [0]);
    }

    #[test]
    fn a_branch_on_a_false_constant_becomes_a_jump_to_the_second_arm() {
        let (mut func, [other, taken]) = diamond(|build| build.iconst(Type::int(1), 0));
        assert!(simplify(&mut func).changed());
        assert_eq!(lives_in(&func, taken), Some(0));
        assert_eq!(lives_in(&func, other), None);
        assert_eq!(blocks(&func), [0]);
    }

    #[test]
    fn a_branch_on_a_parameter_that_was_a_constant_every_way_in_is_folded_in_the_same_run() {
        // The shape `__mas_set_range` leaves in `regcache_maple_drop` in
        // drivers/base/regmap/regcache-maple.c, once `mas_reset` has stored the state its warning
        // reads. The two arms only jump, so step three takes them out, and then the join is
        // handed the same constant both ways in and the branch below it reads that constant.
        let text = "\
; ModuleID = 't.c'
; format 0
target triple = \"x86_64-unknown-linux-gnu\"
target datalayout = \"e-p:64:64-i64:64-f80:128-S128\"
func @warn(), linkage(external);
func @f(i1), linkage(external) {
block0(%0: i1):
    %1 = iconst.i1 0
    br_if %0, block1, block2
block1:
    jump block3(%1)
block2:
    jump block3(%1)
block3(%2: i1):
    br_if %2, block4, block5
block4:
    call @warn() : ()
    jump block5
block5:
    return
}
";
        let mut names = Interner::new();
        let mut module = rucc_ir::parse(text, &mut names).expect("the fixture parses");
        let id = module.funcs().last().expect("there is a function");
        let stats = simplify(&mut module[id]);
        let func = &module[id];
        let calls = func
            .blocks()
            .flat_map(|block| func.insts(block))
            .filter(|&inst| func[inst].opcode == Opcode::Call)
            .count();
        assert_eq!(calls, 0, "{}", rucc_ir::print(&module, &names));
        assert!(stats.count(Kind::Optimized, super::FOLDED) >= 1);
    }

    #[test]
    fn a_known_condition_wins_over_an_arm_that_is_a_dead_end() {
        // `do { asm("ud2"); } while (0); __builtin_unreachable();` with the hint already cut to
        // a block of its own. The test is a zero, so it leaves the loop for the dead end, and
        // reading the dead end as the arm never taken would make the body jump to itself.
        let text = "\
; ModuleID = 't.c'
; format 0
target triple = \"x86_64-unknown-linux-gnu\"
target datalayout = \"e-p:64:64-i64:64-f80:128-S128\"

func @f() {
block0:
    jump block1
block1:
    inline_asm.volatile.nomem \"ud2\", \"\", \"\"()
    %0 = iconst.i1 0
    br_if %0, block1, block2
block2:
    unreachable
}
";
        let mut names = Interner::new();
        let mut module = rucc_ir::parse(text, &mut names).expect("the fixture parses");
        let id = module.funcs().last().expect("there is a function");
        simplify(&mut module[id]);
        let func = &module[id];
        let printed = rucc_ir::print(&module, &names);
        let looped = func.blocks().any(|block| {
            func.terminator(block)
                .is_some_and(|term| func.successors(term).any(|to| to.block == block))
        });
        assert!(!looped, "{printed}");
        assert!(printed.contains("unreachable"), "{printed}");
    }

    #[test]
    fn folding_a_branch_and_merging_what_it_leaves_are_two_things_fuel_buys_apart() {
        // The same function as the test above, with fuel for the fold and nothing after it. The
        // jump is there to be seen, which is the shape the merge would otherwise take away.
        let (mut func, _) = diamond(|build| build.iconst(Type::int(1), 1));
        let stats =
            SimplifyCfg.run(&mut func, &mut crate::machine::fixtures::analyses(), &mut Fuel::of(1));
        assert_eq!(terminator(&func, 0), Opcode::Jump);
        assert_eq!(goes_to(&func, 0), [1]);
        assert_eq!(blocks(&func), [0, 1, 3]);
        assert_eq!(stats.count(Kind::Optimized, super::MERGED), 0);
        // Both blocks of the chain, because a block only reaches the head once the block between
        // them has, so running out before the first one means neither.
        assert_eq!(stats.count(Kind::Missed, super::NO_FUEL_MERGE), 2);
    }

    #[test]
    fn a_branch_on_a_comparison_of_two_constants_is_read_without_folding_it() {
        // Both ways round on every predicate, which is where a sign error or an inverted
        // comparison would hide. A comparison the pass reads is left standing, because folding
        // it would produce an `i1` on its own and issue 352 says that does not lower.
        let cases: &[(IntPred, i128, i128, bool)] = &[
            (IntPred::Eq, 7, 7, true),
            (IntPred::Eq, 7, 8, false),
            (IntPred::Ne, 7, 8, true),
            (IntPred::Ne, 7, 7, false),
            (IntPred::Slt, -1, 1, true),
            (IntPred::Slt, 1, -1, false),
            (IntPred::Sle, -1, -1, true),
            (IntPred::Sle, 1, -1, false),
            (IntPred::Sgt, 1, -1, true),
            (IntPred::Sgt, -1, 1, false),
            (IntPred::Sge, -1, -1, true),
            (IntPred::Sge, -1, 1, false),
            (IntPred::Ult, 1, -1, true),
            (IntPred::Ult, -1, 1, false),
            (IntPred::Ule, -1, -1, true),
            (IntPred::Ule, -1, 1, false),
            (IntPred::Ugt, -1, 1, true),
            (IntPred::Ugt, 1, -1, false),
            (IntPred::Uge, -1, -1, true),
            (IntPred::Uge, 1, -1, false),
        ];
        for &(pred, lhs, rhs, taken) in cases {
            let (mut func, marks) = diamond(|build| {
                let lhs = build.iconst(Type::int(32), lhs);
                let rhs = build.iconst(Type::int(32), rhs);
                build.icmp(pred, lhs, rhs)
            });
            assert!(simplify(&mut func).changed(), "{pred:?} {lhs} {rhs}");
            let [went, gone] = if taken { [marks[0], marks[1]] } else { [marks[1], marks[0]] };
            assert_eq!(lives_in(&func, went), Some(0), "{pred:?} {lhs} {rhs}");
            assert_eq!(lives_in(&func, gone), None, "{pred:?} {lhs} {rhs}");
            let kept = func.insts(Block::from_usize(0)).any(|it| func[it].opcode == Opcode::ICmp);
            assert!(kept, "the comparison was folded away and issue 352 says it must not be");
        }
    }

    #[test]
    fn a_branch_on_something_nobody_knows_is_left_alone() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new().with_params(&[Type::int(1)]));
        let entry = func.create_block();
        let then_block = func.create_block();
        let else_block = func.create_block();
        let cond = func.append_param(entry, Type::int(1));
        let mut build = Builder::new(&mut func, entry);
        build.br_if(cond, then_block, &[], else_block, &[]);
        for arm in [then_block, else_block] {
            let mut build = Builder::new(&mut func, arm);
            build.ret(&[]);
        }
        let stats = simplify(&mut func);
        assert!(!stats.changed());
        assert!(stats.is_empty(), "a pass with nothing to say should say nothing");
        assert_eq!(terminator(&func, 0), Opcode::BrIf);
        assert_eq!(blocks(&func), [0, 1, 2]);
    }

    /// A function that branches on `!c`, where `c` is its one parameter, with the arm taken when
    /// `c` is false hinted at nine in ten, and the branch. The negation is returned too, along
    /// with a second reader of it when `twice` asks for one.
    fn negated(twice: bool) -> (Func, Value) {
        let mut names = Interner::new();
        let sig = Signature::new().with_params(&[Type::int(1)]).with_returns(&[Type::int(1)]);
        let mut func = Func::new(names.intern("f"), sig);
        let entry = func.create_block();
        let then_block = func.create_block();
        let else_block = func.create_block();
        let cond = func.append_param(entry, Type::int(1));
        let mut build = Builder::new(&mut func, entry);
        let one = build.iconst(Type::int(1), -1);
        let not = build.binary(Opcode::Xor, cond, one, Flags::NONE);
        let term = build.br_if(not, then_block, &[], else_block, &[]);
        let Extra::Targets(targets) = func[term].extra else { unreachable!("a br_if has arms") };
        let mut arms = func[targets].to_vec();
        arms[0].hint = Hint::parts(9_000);
        arms[1].hint = Hint::parts(1_000);
        let hinted = func.push_block_calls(&arms);
        func[term].extra = Extra::Targets(hinted);
        for (arm, mark) in [(then_block, 1), (else_block, 0)] {
            let mut build = Builder::new(&mut func, arm);
            let mark = build.iconst(Type::int(1), mark);
            build.ret(&[if twice { not } else { mark }]);
        }
        (func, cond)
    }

    #[test]
    fn a_branch_on_a_negation_branches_on_what_was_negated_with_the_arms_turned_round() {
        let (mut func, cond) = negated(false);
        let stats = simplify(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::UNNEGATED), 1);
        let term = func.terminator(Block::from_usize(0)).expect("the entry still branches");
        assert_eq!(func[term].opcode, Opcode::BrIf);
        assert_eq!(func[func[term].args], [cond]);
        let arms: Vec<(usize, Option<u32>)> =
            func.successors(term).map(|call| (call.block.index(), call.hint.taken())).collect();
        assert_eq!(arms, [(2, Some(1_000)), (1, Some(9_000))]);
        let xor = func.insts(Block::from_usize(0)).any(|it| func[it].opcode == Opcode::Xor);
        assert!(!xor, "the negation nothing else read is still there");
    }

    #[test]
    fn a_negation_something_else_reads_as_well_is_left_alone() {
        let (mut func, _) = negated(true);
        let stats = simplify(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::UNNEGATED), 0);
        assert_eq!(goes_to(&func, 0), [1, 2]);
    }

    /// A function whose entry switches on a constant, with a marker in the default and in each
    /// case, in that order.
    fn switched(on: i128, cases: &[i128]) -> (Func, Vec<Value>) {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let arms: Vec<Block> = (0..=cases.len()).map(|_| func.create_block()).collect();
        let mut build = Builder::new(&mut func, entry);
        let value = build.iconst(Type::int(32), on);
        let pairs: Vec<(i128, Block)> =
            cases.iter().enumerate().map(|(at, &case)| (case, arms[at + 1])).collect();
        build.switch(value, arms[0], &pairs);
        let mut marks = Vec::new();
        for (at, &arm) in arms.iter().enumerate() {
            let mut build = Builder::new(&mut func, arm);
            marks.push(build.iconst(Type::int(32), 100 + at as i128));
            build.ret(&[]);
        }
        (func, marks)
    }

    #[test]
    fn a_switch_on_a_constant_takes_the_case_that_matches() {
        let (mut func, marks) = switched(5, &[4, 5]);
        assert!(simplify(&mut func).changed());
        assert_eq!(lives_in(&func, marks[2]), Some(0));
        assert_eq!(lives_in(&func, marks[0]), None);
        assert_eq!(lives_in(&func, marks[1]), None);
        assert_eq!(blocks(&func), [0]);
    }

    #[test]
    fn a_switch_on_a_constant_no_case_names_takes_the_default() {
        let (mut func, marks) = switched(9, &[4]);
        assert!(simplify(&mut func).changed());
        assert_eq!(lives_in(&func, marks[0]), Some(0));
        assert_eq!(lives_in(&func, marks[1]), None);
        assert_eq!(blocks(&func), [0]);
    }

    #[test]
    fn the_arguments_travel_with_the_edge_that_survives() {
        // The whole reason there are no phi nodes: the argument is in the branch beside the
        // block it goes to, so the surviving arm brings its own and the other one leaves with
        // the edge it was on. Both arms name the same block, so this is also the case branch
        // simplification has to leave alone: one block and two edges, because the two edges say
        // different things.
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let join = func.create_block();
        let param = func.append_param(join, Type::int(32));
        let mut build = Builder::new(&mut func, entry);
        let cond = build.iconst(Type::int(1), 0);
        let taken = build.iconst(Type::int(32), 11);
        let other = build.iconst(Type::int(32), 22);
        build.br_if(cond, join, &[other], join, &[taken]);
        let mut build = Builder::new(&mut func, join);
        build.ret(&[param]);
        assert!(simplify(&mut func).changed());
        // The jump took the edge that survived, and then the block below it had one way in and
        // came up, which is where the parameter stopped being a parameter: whatever read it reads
        // the argument that edge was carrying.
        assert_eq!(blocks(&func), [0]);
        let term = func.terminator(entry).expect("the entry has one");
        assert_eq!(func[func[term].args], [taken]);
        assert_ne!(func[func[term].args], [param]);
    }

    #[test]
    fn a_branch_whose_arms_are_the_same_edge_becomes_a_jump() {
        // Section 21.1's branch simplification, which is about the targets rather than about the
        // condition: nothing here knows what `cond` is and it does not matter, because both ways
        // out arrive at the same place carrying the same thing.
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::int(1)]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let join = func.create_block();
        let cond = func.append_param(entry, Type::int(1));
        let mut build = Builder::new(&mut func, entry);
        build.br_if(cond, join, &[], join, &[]);
        let mut build = Builder::new(&mut func, join);
        build.ret(&[]);
        let stats = simplify(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::FOLDED), 1);
        assert_eq!(stats.count(Kind::Optimized, super::MERGED), 1);
        assert_eq!(blocks(&func), [0]);
        assert_eq!(terminator(&func, 0), Opcode::Return);
    }

    #[test]
    fn a_switch_whose_cases_all_go_to_one_place_becomes_a_jump() {
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::int(32)]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let join = func.create_block();
        let value = func.append_param(entry, Type::int(32));
        let mut build = Builder::new(&mut func, entry);
        build.switch(value, join, &[(4, join), (5, join)]);
        let mut build = Builder::new(&mut func, join);
        build.ret(&[]);
        let stats = simplify(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::FOLDED), 1);
        assert_eq!(blocks(&func), [0]);
    }

    /// A function that switches on a parameter of type `ty`, every case going to block 1 and the
    /// default to block 2, after the pass.
    fn ranged(ty: Type, labels: &[i128]) -> (Func, Stats) {
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[ty]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let hit = func.create_block();
        let miss = func.create_block();
        let value = func.append_param(entry, ty);
        let cases: Vec<(i128, Block)> = labels.iter().map(|&label| (label, hit)).collect();
        Builder::new(&mut func, entry).switch(value, miss, &cases);
        Builder::new(&mut func, hit).ret(&[]);
        Builder::new(&mut func, miss).ret(&[]);
        let stats = simplify(&mut func);
        (func, stats)
    }

    /// The opcodes of the entry block, and the predicate of the compare in it if it has one.
    fn entry_ops(func: &Func) -> (Vec<Opcode>, Option<IntPred>) {
        let entry = Block::from_usize(0);
        let ops = func.insts(entry).map(|inst| func[inst].opcode).collect();
        let pred = func.insts(entry).find_map(|inst| match func[inst].extra {
            Extra::IntPred(pred) => Some(pred),
            _ => None,
        });
        (ops, pred)
    }

    #[test]
    fn a_switch_whose_cases_are_one_run_to_one_place_is_one_unsigned_compare() {
        let (func, stats) = ranged(Type::int(32), &[6, 3, 5, 4]);
        assert_eq!(stats.count(Kind::Optimized, super::RANGED), 1);
        let (ops, pred) = entry_ops(&func);
        let want = [Opcode::IConst, Opcode::Sub, Opcode::IConst, Opcode::ICmp, Opcode::BrIf];
        assert_eq!(ops, want);
        assert_eq!(pred, Some(IntPred::Ule));
        assert_eq!(goes_to(&func, 0), [1, 2]);
    }

    #[test]
    fn a_run_from_zero_has_no_subtraction_and_one_case_is_an_equality() {
        let (func, _) = ranged(Type::int(32), &[0, 1, 2]);
        assert_eq!(
            entry_ops(&func),
            (vec![Opcode::IConst, Opcode::ICmp, Opcode::BrIf], Some(IntPred::Ule))
        );
        let (func, _) = ranged(Type::int(32), &[7]);
        assert_eq!(
            entry_ops(&func),
            (vec![Opcode::IConst, Opcode::ICmp, Opcode::BrIf], Some(IntPred::Eq))
        );
    }

    #[test]
    fn a_run_that_crosses_zero_is_one_run() {
        let (func, stats) = ranged(Type::int(32), &[-1, 0, 1]);
        assert_eq!(stats.count(Kind::Optimized, super::RANGED), 1);
        assert_eq!(entry_ops(&func).1, Some(IntPred::Ule));
    }

    /// `case 0: case 1: case 2:` as the front end writes it, each case a block that only jumps to
    /// the next. The cases go to one place only after the forwarders go.
    #[test]
    fn cases_that_fall_through_to_one_arm_are_one_run() {
        let mut names = Interner::new();
        let ty = Type::int(32);
        let signature = Signature::new().with_params(&[ty]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let miss = func.create_block();
        let steps: Vec<Block> = (0..3).map(|_| func.create_block()).collect();
        let hit = func.create_block();
        let value = func.append_param(entry, ty);
        let cases: Vec<(i128, Block)> = (0..).zip(steps.iter().copied()).collect();
        Builder::new(&mut func, entry).switch(value, miss, &cases);
        Builder::new(&mut func, miss).ret(&[]);
        for (at, &step) in steps.iter().enumerate() {
            Builder::new(&mut func, step).jump(*steps.get(at + 1).unwrap_or(&hit), &[]);
        }
        Builder::new(&mut func, hit).ret(&[]);
        let stats = simplify(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::RANGED), 1);
        assert_eq!(
            entry_ops(&func),
            (vec![Opcode::IConst, Opcode::ICmp, Opcode::BrIf], Some(IntPred::Ule))
        );
    }

    #[test]
    fn a_run_with_a_hole_or_over_the_whole_type_keeps_its_switch() {
        let (func, stats) = ranged(Type::int(32), &[1, 3]);
        assert_eq!(stats.count(Kind::Optimized, super::RANGED), 0);
        assert_eq!(terminator(&func, 0), Opcode::Switch);
        let every: Vec<i128> = (-128..128).collect();
        let (func, stats) = ranged(Type::int(8), &every);
        assert_eq!(stats.count(Kind::Optimized, super::RANGED), 0);
        assert_eq!(terminator(&func, 0), Opcode::Switch);
    }

    #[test]
    fn a_branch_to_one_block_by_two_edges_that_differ_is_left_alone() {
        // One block and two edges. Folding would have to pick one of the two arguments, and
        // whichever it picked would be the wrong one half the time.
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::int(1)]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let join = func.create_block();
        let cond = func.append_param(entry, Type::int(1));
        let param = func.append_param(join, Type::int(32));
        let mut build = Builder::new(&mut func, entry);
        let first = build.iconst(Type::int(32), 11);
        let second = build.iconst(Type::int(32), 22);
        build.br_if(cond, join, &[first], join, &[second]);
        let mut build = Builder::new(&mut func, join);
        // Returned rather than dropped, because a parameter nobody reads is one the step that
        // takes those out would take, and this test is about the branch above it.
        build.ret(&[param]);
        let stats = simplify(&mut func);
        assert!(!stats.changed());
        assert_eq!(terminator(&func, 0), Opcode::BrIf);
        assert_eq!(blocks(&func), [0, 1]);
    }

    #[test]
    fn a_block_the_dead_arm_shared_with_a_live_one_stays() {
        // Issue 359 in the small. The block holding `bar` is inside the body of the dead `if`
        // and is a `case` of the switch as well, so the arm goes and the block does not.
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new().with_params(&[Type::int(32)]));
        let entry = func.create_block();
        let dead = func.create_block();
        let shared = func.create_block();
        let exit = func.create_block();
        let x = func.append_param(entry, Type::int(32));
        let mut build = Builder::new(&mut func, entry);
        let never = build.iconst(Type::int(1), 0);
        build.switch(x, exit, &[(0, dead), (1, shared)]);
        // The `if (0)` inside the first case, whose body is where the second case's label sits.
        // The constant is the body of the arm that survives, and it is there so that the block is
        // a block with something in it rather than a forwarder that step three points past.
        let mut build = Builder::new(&mut func, dead);
        build.iconst(Type::int(32), 1);
        build.br_if(never, shared, &[], exit, &[]);
        for arm in [shared, exit] {
            let mut build = Builder::new(&mut func, arm);
            build.ret(&[]);
        }
        let stats = simplify(&mut func);
        assert!(stats.changed());
        // The switch is on a parameter, so it stays. The branch inside the dead arm folds to the
        // exit, and nothing is removed at all, because the shared block is still a case.
        assert_eq!(terminator(&func, 0), Opcode::Switch);
        assert_eq!(goes_to(&func, 1), [3]);
        assert_eq!(blocks(&func), [0, 1, 2, 3]);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED), 0);
    }

    #[test]
    fn a_block_whose_address_is_taken_is_not_removed() {
        // Reachability here has to be the verifier's reachability. The graph does not carry the
        // edge from a `block_addr` to the block it names, and a pass that removed the block
        // under one would leave an instruction pointing at nothing.
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let labelled = func.create_block();
        let arm = func.create_block();
        let mut build = Builder::new(&mut func, entry);
        let cond = build.iconst(Type::int(1), 1);
        let addr = build.block_addr(labelled);
        build.br_if(cond, arm, &[], labelled, &[]);
        let mut build = Builder::new(&mut func, arm);
        build.indirect_br(addr, &[labelled]);
        let mut build = Builder::new(&mut func, labelled);
        build.ret(&[]);
        assert!(simplify(&mut func).changed());
        assert!(blocks(&func).contains(&1), "the labelled block went with the arm");
        // The arm had one way in and came up into the entry, which is where the `indirect_br`
        // that reaches the labelled block is now.
        assert_eq!(blocks(&func), [0, 1]);
        assert_eq!(goes_to(&func, 0), [1]);
    }

    #[test]
    fn a_block_only_an_unreachable_block_takes_the_address_of_goes_too() {
        // The other half of the same rule. Once the block holding the `block_addr` is gone, the
        // address is gone with it, and the block it named is reached by nothing.
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let dead = func.create_block();
        let labelled = func.create_block();
        let mut build = Builder::new(&mut func, entry);
        let cond = build.iconst(Type::int(1), 1);
        build.br_if(cond, entry, &[], dead, &[]);
        let mut build = Builder::new(&mut func, dead);
        let addr = build.block_addr(labelled);
        build.indirect_br(addr, &[labelled]);
        let mut build = Builder::new(&mut func, labelled);
        build.ret(&[]);
        assert!(simplify(&mut func).changed());
        assert_eq!(blocks(&func), [0]);
    }

    #[test]
    fn a_block_nothing_reaches_goes_even_when_no_branch_folded() {
        // Section 6.5 says this pass is the one that deletes them, and it says so about the
        // blocks the front end handed over as well as the ones a fold here stranded. Nothing in
        // this function folds, and the block still has to go, because every analysis below reads
        // the graph as though it is not there.
        let mut func = graph(&[&[], &[]]);
        let stats = simplify(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::FOLDED), 0);
        assert_eq!(stats.count(Kind::Optimized, super::REMOVED), 1);
        assert_eq!(blocks(&func), [0]);
    }

    #[test]
    fn a_block_with_one_way_into_it_goes_into_the_block_above_it() {
        // Something in each of the first two blocks, so that this is three blocks for the merge
        // rather than two forwarders step three would point past before it got here.
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let middle = func.create_block();
        let last = func.create_block();
        let mut build = Builder::new(&mut func, entry);
        build.iconst(Type::int(32), 1);
        build.jump(middle, &[]);
        let mut build = Builder::new(&mut func, middle);
        build.iconst(Type::int(32), 2);
        build.jump(last, &[]);
        let mut build = Builder::new(&mut func, last);
        build.ret(&[]);
        let stats = simplify(&mut func);
        // A run of three is one chain and not two rounds of one pair, because the block in the
        // middle stops being a block partway through.
        assert_eq!(stats.count(Kind::Optimized, super::MERGED), 2);
        assert_eq!(stats.count(Kind::Optimized, super::FORWARDED), 0);
        assert_eq!(blocks(&func), [0]);
        assert_eq!(terminator(&func, 0), Opcode::Return);
    }

    #[test]
    fn a_block_with_two_ways_into_it_stays_where_it_is() {
        // The join of a diamond nobody can fold. Merging it into either arm would leave the other
        // arm branching into the middle of a block.
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::int(1)]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let then_block = func.create_block();
        let else_block = func.create_block();
        let join = func.create_block();
        let cond = func.append_param(entry, Type::int(1));
        let mut build = Builder::new(&mut func, entry);
        build.br_if(cond, then_block, &[], else_block, &[]);
        for (arm, mark) in [(then_block, 111), (else_block, 222)] {
            // An arm with something in it, because an empty one is a forwarder and step three
            // would take it away before merging ever looked at the join.
            let mut build = Builder::new(&mut func, arm);
            build.iconst(Type::int(32), mark);
            build.jump(join, &[]);
        }
        let mut build = Builder::new(&mut func, join);
        build.ret(&[]);
        let stats = simplify(&mut func);
        assert!(!stats.changed());
        assert_eq!(blocks(&func), [0, 1, 2, 3]);
    }

    #[test]
    fn a_block_above_one_that_does_not_end_in_a_jump_keeps_it() {
        // One way into the join, and the block above it is a branch. Merging would take the
        // terminator off the other arm.
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::int(1)]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let arm = func.create_block();
        let exit = func.create_block();
        let cond = func.append_param(entry, Type::int(1));
        let mut build = Builder::new(&mut func, entry);
        build.br_if(cond, arm, &[], exit, &[]);
        for block in [arm, exit] {
            let mut build = Builder::new(&mut func, block);
            build.ret(&[]);
        }
        let stats = simplify(&mut func);
        assert!(!stats.changed());
        assert_eq!(blocks(&func), [0, 1, 2]);
    }

    #[test]
    fn the_entry_block_is_never_the_one_that_moves() {
        // A loop back to the entry, so the entry has one way in and the block above it ends in a
        // jump, which is every condition but the one that matters. Control arrives at the entry
        // and it has to still be there when it does.
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::int(1)]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let latch = func.create_block();
        let exit = func.create_block();
        let cond = func.append_param(entry, Type::int(1));
        let mut build = Builder::new(&mut func, entry);
        build.br_if(cond, latch, &[], exit, &[]);
        // The body of the loop, which is there so that the latch is a block and not a forwarder.
        let mut build = Builder::new(&mut func, latch);
        build.iconst(Type::int(32), 1);
        build.jump(entry, &[]);
        let mut build = Builder::new(&mut func, exit);
        build.ret(&[]);
        let stats = simplify(&mut func);
        assert!(!stats.changed());
        assert_eq!(blocks(&func), [0, 1, 2]);
    }

    #[test]
    fn a_block_whose_address_is_taken_is_not_merged_away_either() {
        // The same rule as the one about deleting it. Merging it into the block above would take
        // the block out of the function, and the `block_addr` would name one that is not there.
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let middle = func.create_block();
        let labelled = func.create_block();
        let mut build = Builder::new(&mut func, entry);
        build.block_addr(labelled);
        build.jump(middle, &[]);
        // Something in the middle block, so that this is a question about merging rather than one
        // about the forwarder removal that would otherwise get there first.
        let mut build = Builder::new(&mut func, middle);
        build.iconst(Type::int(32), 1);
        build.jump(labelled, &[]);
        let mut build = Builder::new(&mut func, labelled);
        build.ret(&[]);
        let stats = simplify(&mut func);
        // The middle block had one way in and no address, so it came up. The labelled block has
        // one way in too, and stayed.
        assert_eq!(stats.count(Kind::Optimized, super::MERGED), 1);
        assert_eq!(blocks(&func), [0, 2]);
    }

    #[test]
    fn a_block_an_image_names_is_not_merged_away_either() {
        // The case with nothing in the function to find. No `block_addr` names this block, only a
        // relocation in another section does, so the whole of what keeps it is the name, and a
        // merge would leave that name on an instruction in the middle of the block above.
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let middle = func.create_block();
        let labelled = func.create_block();
        Builder::new(&mut func, entry).jump(middle, &[]);
        let mut build = Builder::new(&mut func, middle);
        build.iconst(Type::int(32), 1);
        build.jump(labelled, &[]);
        Builder::new(&mut func, labelled).ret(&[]);
        func.name_block(labelled, names.intern(".Llbl.0"));
        let stats = simplify(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::MERGED), 1);
        assert_eq!(blocks(&func), [0, 2]);
        assert_eq!(func.block_name(labelled), Some(names.intern(".Llbl.0")));
    }

    #[test]
    fn merging_binds_a_block_parameter_to_the_argument_the_jump_carried() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let below = func.create_block();
        let param = func.append_param(below, Type::int(32));
        let mut build = Builder::new(&mut func, entry);
        let arg = build.iconst(Type::int(32), 7);
        build.jump(below, &[arg]);
        let mut build = Builder::new(&mut func, below);
        build.ret(&[param]);
        assert!(simplify(&mut func).changed());
        assert_eq!(blocks(&func), [0]);
        let term = func.terminator(entry).expect("the entry has one");
        assert_eq!(func[func[term].args], [arg]);
    }

    #[test]
    fn a_chain_of_merges_follows_a_parameter_bound_to_a_parameter() {
        // The middle block passes its own parameter down, so the last block's parameter is bound
        // to something that is on its way to being the entry's constant. Following the map is
        // what makes the second merge worth as much as the first.
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let middle = func.create_block();
        let last = func.create_block();
        let carried = func.append_param(middle, Type::int(32));
        let arrived = func.append_param(last, Type::int(32));
        let mut build = Builder::new(&mut func, entry);
        let arg = build.iconst(Type::int(32), 7);
        build.jump(middle, &[arg]);
        let mut build = Builder::new(&mut func, middle);
        build.jump(last, &[carried]);
        let mut build = Builder::new(&mut func, last);
        build.ret(&[arrived]);
        assert!(simplify(&mut func).changed());
        assert_eq!(blocks(&func), [0]);
        let term = func.terminator(entry).expect("the entry has one");
        assert_eq!(func[func[term].args], [arg]);
    }

    /// A function whose entry branches on its own parameter into two arms that each hold one
    /// instruction and then jump where the caller says, head to tail.
    ///
    /// Two arms rather than one because almost every question about step three is a question about
    /// a block with more than one way in, and something in each arm because an empty arm is itself
    /// a forwarder and would answer a different question. The blocks are entry 0, the arms 1 and 2,
    /// and whatever the caller builds after that.
    fn arms(func: &mut Func) -> (Value, [Block; 2]) {
        let entry = func.create_block();
        let first = func.create_block();
        let second = func.create_block();
        let cond = func.append_param(entry, Type::int(1));
        let mut build = Builder::new(func, entry);
        let carried = build.iconst(Type::int(32), 7);
        build.br_if(cond, first, &[], second, &[]);
        for (arm, mark) in [(first, 111), (second, 222)] {
            let mut build = Builder::new(func, arm);
            build.iconst(Type::int(32), mark);
        }
        (carried, [first, second])
    }

    /// A function with one `i1` parameter, which is what [`arms`] wants.
    fn taking_a_condition() -> Func {
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::int(1)]);
        Func::new(names.intern("f"), signature)
    }

    /// The arguments a block's terminator passes on the edge in that place.
    fn carries(func: &Func, block: usize, edge: usize) -> Vec<Value> {
        let block = Block::from_usize(block);
        let term = func.terminator(block).expect("every block here has one");
        let call = func.successors(term).nth(edge).expect("the edge is there");
        func[call.args].to_vec()
    }

    #[test]
    fn a_block_that_does_nothing_but_jump_stops_being_in_the_way() {
        // Section 21.1's edge forwarding. Two arms arrive at a block that only jumps, so the two
        // of them go where it was going and it is not there any more.
        let mut func = taking_a_condition();
        let (_, arms) = arms(&mut func);
        let forwarder = func.create_block();
        let exit = func.create_block();
        for arm in arms {
            Builder::new(&mut func, arm).jump(forwarder, &[]);
        }
        Builder::new(&mut func, forwarder).jump(exit, &[]);
        Builder::new(&mut func, exit).ret(&[]);
        let stats = simplify(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::FORWARDED), 1);
        assert_eq!(blocks(&func), [0, 1, 2, 4]);
        assert_eq!(goes_to(&func, 1), [4]);
        assert_eq!(goes_to(&func, 2), [4]);
    }

    #[test]
    fn a_forwarder_hands_its_predecessors_the_arguments_it_was_passing() {
        // The forwarder was passing something, and taking it out means whoever ends up branching
        // to the block below has to pass it instead. Section 21.1's extra condition is about
        // exactly this, and a block with no parameters and no instructions cannot be where the
        // value came from, so there is nothing further to check. The one way in is through a block
        // that goes nowhere else, which keeps the edge off the list of ones that carry a move with
        // no block to put it in.
        let mut func = taking_a_condition();
        let (carried, [arm, above]) = arms(&mut func);
        let forwarder = func.create_block();
        let exit = func.create_block();
        let other = func.append_param(exit, Type::int(32));
        let mut build = Builder::new(&mut func, arm);
        let mine = build.iconst(Type::int(32), 9);
        build.jump(exit, &[mine]);
        Builder::new(&mut func, above).jump(forwarder, &[]);
        Builder::new(&mut func, forwarder).jump(exit, &[carried]);
        Builder::new(&mut func, exit).ret(&[other]);
        let stats = simplify(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::FORWARDED), 1);
        assert_eq!(blocks(&func), [0, 1, 2, 4]);
        // The block above the forwarder is the edge that used to go through it, and it is carrying
        // what the forwarder was carrying.
        assert_eq!(carries(&func, 2, 0), [carried]);
        assert_eq!(carries(&func, 1, 0), [mine]);
        // Two edges saying different things, so the parameter is not redundant and stays.
        assert_eq!(stats.count(Kind::Optimized, super::SAME_EVERY_WAY), 0);
    }

    #[test]
    fn a_forwarder_carrying_something_on_an_edge_out_of_a_branch_stays() {
        // Both ways out of the entry end up at the same block, and that block takes a parameter, so
        // the edge through the forwarder is one the back end would have to split again the moment
        // the forwarder stopped being there. The block is already the split, in the place the
        // layout wants it, so it is left where it is.
        let mut func = taking_a_condition();
        let (carried, [arm, forwarder]) = arms(&mut func);
        let exit = func.create_block();
        let other = func.append_param(exit, Type::int(32));
        // The second arm is emptied back out, which is what makes it a forwarder at all.
        for inst in func.insts(forwarder).collect::<Vec<Inst>>() {
            func.remove_inst(inst);
        }
        let mut build = Builder::new(&mut func, arm);
        let mine = build.iconst(Type::int(32), 9);
        build.jump(exit, &[mine]);
        Builder::new(&mut func, forwarder).jump(exit, &[carried]);
        Builder::new(&mut func, exit).ret(&[other]);
        let stats = simplify(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::FORWARDED), 0);
        assert_eq!(blocks(&func), [0, 1, 2, 3]);
    }

    #[test]
    fn a_forwarder_carrying_nothing_out_of_a_branch_goes_anyway() {
        // The same shape with nothing on the edge. There is no move to find a place for, so the
        // back end would leave the edge alone and the block is only in the way.
        let mut func = taking_a_condition();
        let (_, [arm, forwarder]) = arms(&mut func);
        let exit = func.create_block();
        for inst in func.insts(forwarder).collect::<Vec<Inst>>() {
            func.remove_inst(inst);
        }
        Builder::new(&mut func, arm).jump(exit, &[]);
        Builder::new(&mut func, forwarder).jump(exit, &[]);
        Builder::new(&mut func, exit).ret(&[]);
        let stats = simplify(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::FORWARDED), 1);
        assert_eq!(blocks(&func), [0, 1, 3]);
    }

    #[test]
    fn a_block_that_jumps_to_itself_is_not_a_forwarder() {
        // Section 21.1 says so in as many words, and the reason is that it does not forward
        // anywhere: pointing its predecessors past it would have to point them at it.
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let spin = func.create_block();
        Builder::new(&mut func, entry).jump(spin, &[]);
        Builder::new(&mut func, spin).jump(spin, &[]);
        let stats = simplify(&mut func);
        assert!(!stats.changed());
        assert_eq!(blocks(&func), [0, 1]);
    }

    #[test]
    fn the_entry_block_is_never_the_forwarder_that_goes() {
        // The entry doing nothing but jumping is every condition of a forwarder except the one
        // that matters. What happens instead is the block below coming up into it, which leaves
        // control arriving where it has to arrive.
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let below = func.create_block();
        Builder::new(&mut func, entry).jump(below, &[]);
        let mut build = Builder::new(&mut func, below);
        build.iconst(Type::int(32), 1);
        build.ret(&[]);
        let stats = simplify(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::FORWARDED), 0);
        assert_eq!(stats.count(Kind::Optimized, super::MERGED), 1);
        assert_eq!(blocks(&func), [0]);
    }

    #[test]
    fn a_block_whose_address_is_taken_is_not_forwarded_past_either() {
        // The abnormal edge condition, which in this IR is the edge an `indirect_br` takes. The
        // block is arrived at from somewhere the graph reads from the other end, and pointing the
        // edges the graph does carry past it would not move that one.
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let labelled = func.create_block();
        let exit = func.create_block();
        let mut build = Builder::new(&mut func, entry);
        let addr = build.block_addr(labelled);
        build.indirect_br(addr, &[labelled]);
        Builder::new(&mut func, labelled).jump(exit, &[]);
        let mut build = Builder::new(&mut func, exit);
        build.iconst(Type::int(32), 1);
        build.ret(&[]);
        let stats = simplify(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::FORWARDED), 0);
        assert!(blocks(&func).contains(&1), "the labelled block was forwarded past");
    }

    #[test]
    fn a_run_of_forwarders_comes_out_as_one_edge() {
        let mut func = taking_a_condition();
        let (_, arms) = arms(&mut func);
        let first = func.create_block();
        let second = func.create_block();
        let exit = func.create_block();
        for arm in arms {
            Builder::new(&mut func, arm).jump(first, &[]);
        }
        Builder::new(&mut func, first).jump(second, &[]);
        Builder::new(&mut func, second).jump(exit, &[]);
        Builder::new(&mut func, exit).ret(&[]);
        let stats = simplify(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::FORWARDED), 2);
        assert_eq!(blocks(&func), [0, 1, 2, 5]);
        assert_eq!(goes_to(&func, 1), [5]);
        assert_eq!(goes_to(&func, 2), [5]);
    }

    #[test]
    fn a_block_parameter_that_arrives_as_one_value_every_way_in_goes() {
        // Section 21.2. The parameter is not carrying anything, it is spelling the constant a
        // second way, and document 12 cannot see through the spelling.
        let mut func = taking_a_condition();
        let (carried, arms) = arms(&mut func);
        let join = func.create_block();
        let param = func.append_param(join, Type::int(32));
        for arm in arms {
            Builder::new(&mut func, arm).jump(join, &[carried]);
        }
        Builder::new(&mut func, join).ret(&[param]);
        let stats = simplify(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::SAME_EVERY_WAY), 1);
        assert!(func[Block::from_usize(3)].params.is_empty());
        // What read the parameter reads the value it was always going to be.
        let term = func.terminator(Block::from_usize(3)).expect("the join has one");
        assert_eq!(func[func[term].args], [carried]);
        // And the argument in its place is off both edges, because a branch that passes more
        // arguments than the block takes is one the verifier refuses.
        assert!(carries(&func, 1, 0).is_empty());
        assert!(carries(&func, 2, 0).is_empty());
    }

    #[test]
    fn a_null_pointer_each_way_in_made_for_itself_is_one_null_pointer() {
        // Two inlined `return NULL` paths: each arm has its own zero and its own pointer made of
        // it, so the parameter is not one value every way in, and it is still always null.
        let mut func = taking_a_condition();
        let (_, arms) = arms(&mut func);
        let join = func.create_block();
        let param = func.append_param(join, Type::PTR);
        for arm in arms {
            let mut build = Builder::new(&mut func, arm);
            let zero = build.iconst(Type::int(64), 0);
            let null = build.unary(Opcode::IntToPtr, zero, Type::PTR);
            build.jump(join, &[null]);
        }
        Builder::new(&mut func, join).ret(&[param]);
        let stats = simplify(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::SAME_EVERY_WAY), 1);
        let join = Block::from_usize(3);
        assert!(func[join].params.is_empty());
        let term = func.terminator(join).expect("the join has one");
        let returned = func[func[term].args][0];
        assert_eq!(super::numbered(&func, returned).map(|(imm, _)| imm.bits()), Some(0));
        assert_eq!(lives_in(&func, returned), Some(3));
    }

    #[test]
    fn two_different_constants_each_way_in_stay_a_parameter() {
        let mut func = taking_a_condition();
        let (_, arms) = arms(&mut func);
        let join = func.create_block();
        let param = func.append_param(join, Type::int(32));
        for (arm, value) in arms.into_iter().zip([1, 2]) {
            let mut build = Builder::new(&mut func, arm);
            let constant = build.iconst(Type::int(32), value);
            build.jump(join, &[constant]);
        }
        Builder::new(&mut func, join).ret(&[param]);
        let stats = simplify(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::SAME_EVERY_WAY), 0);
        assert_eq!(func[Block::from_usize(3)].params.len(), 1);
    }

    #[test]
    fn a_block_parameter_that_differs_on_one_way_in_stays() {
        let mut func = taking_a_condition();
        let (carried, arms) = arms(&mut func);
        let join = func.create_block();
        let param = func.append_param(join, Type::int(32));
        let mut build = Builder::new(&mut func, arms[0]);
        let mine = build.iconst(Type::int(32), 9);
        build.jump(join, &[mine]);
        Builder::new(&mut func, arms[1]).jump(join, &[carried]);
        Builder::new(&mut func, join).ret(&[param]);
        let stats = simplify(&mut func);
        assert!(!stats.changed());
        assert_eq!(func[Block::from_usize(3)].params, [param]);
    }

    #[test]
    fn a_loop_header_parameter_whose_other_argument_is_itself_is_what_it_started_as() {
        // The subtlety section 21.2 spends its second paragraph on. The latch passes the
        // parameter back, so reading the arguments literally says two values and says leave it
        // alone. A value that can only ever be itself or the initial one was the initial one.
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::int(1)]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let header = func.create_block();
        let latch = func.create_block();
        let exit = func.create_block();
        let cond = func.append_param(entry, Type::int(1));
        let param = func.append_param(header, Type::int(32));
        let mut build = Builder::new(&mut func, entry);
        let init = build.iconst(Type::int(32), 7);
        build.jump(header, &[init]);
        Builder::new(&mut func, header).br_if(cond, latch, &[], exit, &[]);
        let mut build = Builder::new(&mut func, latch);
        build.iconst(Type::int(32), 1);
        build.jump(header, &[param]);
        Builder::new(&mut func, exit).ret(&[param]);
        let stats = simplify(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::SAME_EVERY_WAY), 1);
        assert!(func[Block::from_usize(1)].params.is_empty());
        let term = func.terminator(Block::from_usize(3)).expect("the exit has one");
        assert_eq!(func[func[term].args], [init]);
    }

    #[test]
    fn the_entry_blocks_parameters_are_the_functions_and_stay() {
        // The entry's parameters arrive from the caller, which is a way in the graph has no edge
        // for. A branch back to the entry is one edge out of two, and reading it as though it
        // were the only one would replace an argument with whatever the loop happened to pass.
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::int(1), Type::int(32)]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let latch = func.create_block();
        let exit = func.create_block();
        let cond = func.append_param(entry, Type::int(1));
        let x = func.append_param(entry, Type::int(32));
        Builder::new(&mut func, entry).br_if(cond, latch, &[], exit, &[]);
        let mut build = Builder::new(&mut func, latch);
        let one = build.iconst(Type::int(1), 1);
        let seven = build.iconst(Type::int(32), 7);
        build.jump(entry, &[one, seven]);
        Builder::new(&mut func, exit).ret(&[x]);
        let stats = simplify(&mut func);
        assert!(!stats.changed());
        assert_eq!(func[Block::from_usize(0)].params, [cond, x]);
    }

    #[test]
    fn taking_one_parameter_away_is_what_makes_the_next_one_redundant() {
        // Section 21.2's reason for a worklist. The last block's parameter arrives as the middle
        // block's parameter one way and as the constant the other way, which is two values until
        // the middle block's parameter turns out to be that same constant.
        let mut func = taking_a_condition();
        let (carried, arms) = arms(&mut func);
        let join = func.create_block();
        let inner = func.append_param(join, Type::int(32));
        let left = func.create_block();
        let right = func.create_block();
        let last = func.create_block();
        let outer = func.append_param(last, Type::int(32));
        for arm in arms {
            Builder::new(&mut func, arm).jump(join, &[carried]);
        }
        let cond = func[Block::from_usize(0)].params[0];
        Builder::new(&mut func, join).br_if(cond, left, &[], right, &[]);
        let mut build = Builder::new(&mut func, left);
        build.iconst(Type::int(32), 1);
        build.jump(last, &[inner]);
        let mut build = Builder::new(&mut func, right);
        build.iconst(Type::int(32), 2);
        build.jump(last, &[carried]);
        Builder::new(&mut func, last).ret(&[outer]);
        let stats = simplify(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::SAME_EVERY_WAY), 2);
        let term = func.terminator(Block::from_usize(6)).expect("the last block has one");
        assert_eq!(func[func[term].args], [carried]);
    }

    #[test]
    fn a_forwarder_with_a_parameter_goes_once_the_parameter_does() {
        // The two halves of step three being one step. The block passes its own parameter on, so
        // it is not a forwarder while it has one, and the parameter is the same value both ways
        // in, so it does not have one for long.
        let mut func = taking_a_condition();
        let (carried, arms) = arms(&mut func);
        let forwarder = func.create_block();
        let param = func.append_param(forwarder, Type::int(32));
        let exit = func.create_block();
        let arrived = func.append_param(exit, Type::int(32));
        for arm in arms {
            Builder::new(&mut func, arm).jump(forwarder, &[carried]);
        }
        Builder::new(&mut func, forwarder).jump(exit, &[param]);
        Builder::new(&mut func, exit).ret(&[arrived]);
        let stats = simplify(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::FORWARDED), 1);
        // Both of them: the forwarder's, which is what let it go, and the exit's, which arrives
        // as the same thing from both arms once the block between them is not there.
        assert_eq!(stats.count(Kind::Optimized, super::SAME_EVERY_WAY), 2);
        assert_eq!(blocks(&func), [0, 1, 2, 4]);
        let term = func.terminator(Block::from_usize(4)).expect("the exit has one");
        assert_eq!(func[func[term].args], [carried]);
    }

    #[test]
    fn fuel_stops_step_three_the_same_way_it_stops_the_rest() {
        // One unit, and the first thing that asks for it is the parameter, because parameters go
        // first within a block. The forwarder then has nothing to spend and stays.
        let mut func = taking_a_condition();
        let (carried, arms) = arms(&mut func);
        let forwarder = func.create_block();
        let param = func.append_param(forwarder, Type::int(32));
        let exit = func.create_block();
        // Arriving at the exit and returned there, so that the forwarder's parameter is one
        // something reads and the step that takes the unread ones out leaves it alone.
        let arrived = func.append_param(exit, Type::int(32));
        for arm in arms {
            Builder::new(&mut func, arm).jump(forwarder, &[carried]);
        }
        Builder::new(&mut func, forwarder).jump(exit, &[param]);
        Builder::new(&mut func, exit).ret(&[arrived]);
        let stats =
            SimplifyCfg.run(&mut func, &mut crate::machine::fixtures::analyses(), &mut Fuel::of(1));
        assert_eq!(stats.count(Kind::Optimized, super::SAME_EVERY_WAY), 1);
        assert_eq!(stats.count(Kind::Optimized, super::FORWARDED), 0);
        assert_eq!(stats.count(Kind::Missed, super::NO_FUEL_FORWARD), 1);
        assert_eq!(blocks(&func), [0, 1, 2, 3, 4]);
    }

    /// A loop that carries a counter and a pointer, and leaves when the pointer reaches `end`.
    ///
    /// The shape `crate::ivopts` produces once section 28.4 has moved the exit test off the
    /// counter: nothing asks the counter anything any more, and the only thing left reading it is
    /// the addition that produces what the latch hands back to it.
    ///
    /// `on_counter` puts the exit test back on the counter, which is the same loop with the
    /// counter live, and it is the negative half of every test below.
    fn walking_a_pointer(on_counter: bool) -> Func {
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::int(64)]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let head = func.create_block();
        let out = func.create_block();
        let end = func.append_param(entry, Type::int(64));
        let counter = func.append_param(head, Type::int(32));
        let pointer = func.append_param(head, Type::int(64));
        let mut build = Builder::new(&mut func, entry);
        let from_zero = build.iconst(Type::int(32), 0);
        let from_start = build.iconst(Type::int(64), 0);
        build.jump(head, &[from_zero, from_start]);
        let mut build = Builder::new(&mut func, head);
        let one = build.iconst(Type::int(32), 1);
        let eight = build.iconst(Type::int(64), 8);
        let next = build.binary(Opcode::Add, counter, one, Flags::NONE);
        let along = build.binary(Opcode::Add, pointer, eight, Flags::NONE);
        // The write the loop is there for, so that the pointer is read by something that happens
        // whichever way the exit test is written.
        let address = build.unary(Opcode::IntToPtr, pointer, Type::PTR);
        let info = MemInfo {
            size: 8,
            align: 8,
            order: MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        };
        build.store(eight, address, info, Flags::NONE);
        let going = if on_counter {
            let limit = build.iconst(Type::int(32), 10);
            build.icmp(IntPred::Ne, next, limit)
        } else {
            build.icmp(IntPred::Ne, along, end)
        };
        build.br_if(going, head, &[next, along], out, &[]);
        Builder::new(&mut func, out).ret(&[]);
        func
    }

    #[test]
    fn a_counter_the_loop_stopped_asking_about_stops_going_round() {
        let mut func = walking_a_pointer(false);
        let stats = simplify(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::NOTHING_READS_IT), 1);
        // The pointer stays, because the test that decides whether to go round again reads it.
        assert_eq!(func[Block::from_usize(1)].params.len(), 1);
        // And the edge that was feeding the counter is carrying one value now instead of two.
        assert_eq!(carries(&func, 1, 0).len(), 1);
        assert_eq!(carries(&func, 0, 0).len(), 1);
    }

    #[test]
    fn a_counter_the_loop_still_asks_about_goes_round_exactly_as_before() {
        let mut func = walking_a_pointer(true);
        let stats = simplify(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::NOTHING_READS_IT), 0);
        assert_eq!(func[Block::from_usize(1)].params.len(), 2);
    }

    /// A counter nothing reads, with two instructions behind it rather than one.
    ///
    /// The addition reads the parameter and the doubling reads the addition, so taking the
    /// parameter out strands the first and taking the first out strands the second. The condition
    /// is asked about the function's own parameter so that nothing here folds and the loop stays a
    /// loop.
    fn counting_into_nothing() -> (Func, Value, Value) {
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::int(32)]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let head = func.create_block();
        let out = func.create_block();
        let limit = func.append_param(entry, Type::int(32));
        let counter = func.append_param(head, Type::int(32));
        let mut build = Builder::new(&mut func, entry);
        let zero = build.iconst(Type::int(32), 0);
        build.jump(head, &[zero]);
        let mut build = Builder::new(&mut func, head);
        let one = build.iconst(Type::int(32), 1);
        let next = build.binary(Opcode::Add, counter, one, Flags::NONE);
        let twice = build.binary(Opcode::Add, next, next, Flags::NONE);
        let going = build.icmp(IntPred::Ne, limit, one);
        build.br_if(going, head, &[next], out, &[]);
        Builder::new(&mut func, out).ret(&[]);
        (func, next, twice)
    }

    #[test]
    fn what_was_reading_a_parameter_nothing_reads_goes_with_it() {
        let (mut func, next, twice) = counting_into_nothing();
        let stats = simplify(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::NOTHING_READS_IT), 1);
        // Both of them, and the second one is the point: it was never reading the parameter, it
        // was reading what did, so one walk that only took the direct readers would have left a
        // use of a value nothing defines. This is issue 1016.
        assert_eq!(lives_in(&func, next), None);
        assert_eq!(lives_in(&func, twice), None);
    }

    #[test]
    fn the_functions_own_parameters_stay_whether_or_not_anything_reads_them() {
        // The entry's parameters are the signature. Nothing in this function reads the one it
        // has, and taking it out would be changing what the function is rather than what it does.
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::int(32)]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        func.append_param(entry, Type::int(32));
        Builder::new(&mut func, entry).ret(&[]);
        let stats = simplify(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::NOTHING_READS_IT), 0);
        assert_eq!(func[entry].params.len(), 1);
    }

    #[test]
    fn a_parameter_nothing_reads_costs_one_unit_of_fuel_and_stays_without_it() {
        let mut func = walking_a_pointer(false);
        let stats =
            SimplifyCfg.run(&mut func, &mut crate::machine::fixtures::analyses(), &mut Fuel::of(0));
        assert_eq!(stats.count(Kind::Optimized, super::NOTHING_READS_IT), 0);
        assert_eq!(stats.count(Kind::Missed, super::NO_FUEL_UNREAD), 1);
        assert_eq!(func[Block::from_usize(1)].params.len(), 2);
    }

    #[test]
    fn the_counter_that_went_leaves_the_verifier_nothing_to_complain_about() {
        let target = TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu));
        let mut names = Interner::new();
        let mut module = Module::new(names.intern("test.c"), &target);
        let mut func = walking_a_pointer(false);
        simplify(&mut func);
        module.add_func(func);
        rucc_ir::verify(&module, &names).expect("taking a parameter out left the function whole");
    }

    #[test]
    fn step_three_leaves_the_verifier_nothing_to_complain_about() {
        // Section 21.6 names an argument list that stops matching its block's parameters as the
        // most common bug in this document, and both halves of step three change one.
        let target = TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu));
        let mut names = Interner::new();
        let mut module = Module::new(names.intern("test.c"), &target);
        let mut func = taking_a_condition();
        let (carried, arms) = arms(&mut func);
        let forwarder = func.create_block();
        let param = func.append_param(forwarder, Type::int(32));
        let exit = func.create_block();
        let arrived = func.append_param(exit, Type::int(32));
        let mut build = Builder::new(&mut func, arms[0]);
        let mine = build.iconst(Type::int(32), 9);
        build.jump(exit, &[mine]);
        Builder::new(&mut func, arms[1]).jump(forwarder, &[carried]);
        Builder::new(&mut func, forwarder).jump(exit, &[param]);
        let mut build = Builder::new(&mut func, exit);
        // A reader of the parameter that is not the return, because this function returns nothing
        // and the point is that something downstream still has the value it was passed.
        build.icmp(IntPred::Eq, arrived, arrived);
        build.ret(&[]);
        simplify(&mut func);
        module.add_func(func);
        rucc_ir::verify(&module, &names).expect("step three left the function verifiable");
    }

    #[test]
    fn out_of_fuel_leaves_the_function_exactly_as_it_was() {
        let (mut func, _) = diamond(|build| build.iconst(Type::int(1), 1));
        let before = blocks(&func);
        let stats =
            SimplifyCfg.run(&mut func, &mut crate::machine::fixtures::analyses(), &mut Fuel::of(0));
        assert!(!stats.changed());
        assert_eq!(stats.count(Kind::Missed, super::NO_FUEL), 1);
        assert_eq!(terminator(&func, 0), Opcode::BrIf);
        assert_eq!(blocks(&func), before);
    }

    #[test]
    fn what_fuel_buys_is_one_whole_change_and_never_half_of_one() {
        // Two foldable branches and fuel for one. The half that removes the stranded blocks is
        // not charged for, because a limit that could stop between the two halves would leave a
        // block nothing reaches and the verifier would refuse the function.
        let mut func = graph(&[&[1, 2], &[3, 4], &[5], &[5], &[5], &[]]);
        let stats =
            SimplifyCfg.run(&mut func, &mut crate::machine::fixtures::analyses(), &mut Fuel::of(1));
        assert_eq!(stats.count(Kind::Optimized, super::FOLDED), 1);
        assert_eq!(stats.count(Kind::Missed, super::NO_FUEL), 1);
        // The entry folded to its first arm, so the second arm is stranded and goes, and the
        // block only it reached goes with it.
        assert_eq!(blocks(&func), [0, 1, 3, 4, 5]);
    }

    #[test]
    fn the_pass_leaves_the_verifier_nothing_to_complain_about() {
        let target = TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu));
        let mut names = Interner::new();
        let mut module = Module::new(names.intern("test.c"), &target);
        let mut func = graph(&[&[1, 2], &[3], &[3], &[4, 1], &[]]);
        simplify(&mut func);
        module.add_func(func);
        rucc_ir::verify(&module, &names).expect("the pass left the function verifiable");
    }

    #[test]
    fn the_pass_says_it_preserves_nothing() {
        assert_eq!(SimplifyCfg.preserves(), Preserved::NONE);
    }
}
