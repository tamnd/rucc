//! If-conversion, the part of it that turns a diamond into a select.
//!
//! Design: `spec/optimizer/22-phiopt-and-if-conversion.md`. A block ends in a two way branch, each
//! arm works out a value and does nothing else, and the two arms meet again at a block that takes
//! that value as a parameter. The branch is not deciding what the program does, it is deciding
//! which of two numbers to keep, and `select` says that directly. Section 22.2 asks for the shape
//! matcher and five transformations built on it, and the shape matcher plus the first, the second,
//! the third and half of the fourth of them is what is here.
//!
//! This is the highest variance transformation in the compiler and the document says so in its
//! third paragraph. Removing a mispredicted branch is worth about twenty cycles. Removing a
//! perfectly predicted one costs whatever the arm that is no longer skipped costs, and no static
//! analysis tells the two apart reliably. So the cost rule below is written to be argued with
//! rather than to be right, and section 42's measurement of the pass on and off at `-O2` is the
//! only honest evaluation there is.
//!
//! # The shape
//!
//! A head block ending in `br_if`, and a join block both arms reach. Each side of the branch is
//! either a block of its own that does nothing but work out values and jump to the join, or the
//! join itself. That gives three shapes and the pass takes all three: the diamond where both sides
//! have a block, and the two triangles where one side goes straight to the join because the arm
//! was empty and `simplify-cfg` already took it out.
//!
//! What replaces it is one block. Everything the arms worked out moves into the head, a `select`
//! is built for each of the join's parameters the two sides disagree about, and the head jumps to
//! the join carrying them. The arms are then unreachable and go, and the join is left for
//! `simplify-cfg` to merge upward when nothing else arrives at it.
//!
//! # The value the condition already settled
//!
//! Section 22.2's second transformation, `value_replacement`. `x = (a == b) ? b : a` is `x = a`,
//! because the only way to arrive at the join carrying `b` is along the edge where `a` and `b` are
//! the same number. No select is written, the branch goes with the rest of them, and whichever arm
//! was only there to work the other value out is left with nothing in it.
//!
//! What answers this is document 10's relational oracle, which is what section 22.2 says it needs
//! and this is its first caller in the compiler. The question is put as `Ranges::compare` at the
//! arm rather than as a range lookup, because the fact wanted is about two values rather than about
//! either one of them, and section 10.3 is where that distinction is made.
//!
//! The branch has to be on an equality, and that gate is the difference between a cheap pass and an
//! expensive one. Section 22.7 already names this query as the expensive part of the pass. What the
//! oracle records is what a dominating edge established between two values, and the edge out of a
//! `br_if` establishes something about two values only when the branch is on a comparison of them,
//! so a branch on `x < n` cannot answer whether two other values are equal and asking is a query
//! with nothing at the end of it. GCC gates the same way, on `EQ_EXPR` and `NE_EXPR`.
//!
//! One thing this does that the select cannot is a value whose type has no `select` at all. A
//! pointer is the case: `p == q ? q : p` used to keep its branch, because a `select` of two
//! pointers is a term nothing lowers, and it is now one move, because nothing has to be chosen.
//! The width refusal below is therefore asked after this rather than before it.
//!
//! It is one deep, in the same sense the factoring below is. What the join is handed is what gets
//! asked about, so `a == b ? b + c : a + c` factors to one add over a select and the select stays,
//! because what the oracle would have to know is that `b + c` and `a + c` are equal rather than
//! that `a` and `b` are. Asking about the factored operand instead is the change that would take
//! it, and it is left for when something measures a use for it.
//!
//! # The operation both arms did
//!
//! Section 22.2's third transformation, `factor_out_conditional_operation`. When the two sides
//! worked out their answers the same way from different operands, `cond ? f(a) : f(b)`, the select
//! goes under the operation rather than over it and the answer is `f(cond ? a : b)`. One operation
//! where there were two, and the same one select either way.
//!
//! It is structural and not a rewrite rule for the reason section 22.2 gives about all five: the
//! two `f`s are in different blocks and no pattern spans blocks. By the time they are in one block
//! the arms have already been hoisted and the select already written, and undoing that is a larger
//! rewrite than never writing it.
//!
//! The two operations have to match in everything but one operand. The opcode and the operand count
//! obviously. The flags, because those are what the optimizer is licensed to assume and one copy
//! written under the union of two sets of assumptions would be claiming on one path something only
//! the other path established. Whatever else the instruction carries, which for a comparison is the
//! predicate, since two predicates are two different questions. And exactly one operand position
//! apart, because two positions apart needs two selects and one operation, which is what one select
//! and two operations already cost.
//!
//! Agreeing in every position is allowed and is the case where no select is written at all. Both
//! arms working out the same thing from the same operands is what a common subexpression that
//! nothing has numbered looks like from here, and one copy of it serves both sides.
//!
//! The operation has to be worked out in the arm and read only by the arm's jump to the join. The
//! first because an operation to stop writing is one this has to be able to find. The second
//! because the one copy that replaces the two is written after the arms have gone, and a second
//! reader inside the arm would have been left pointing at an instruction that is no longer in any
//! block.
//!
//! Only the value the join takes is asked about, so a chain both arms share is factored one deep.
//! `total += (long long)(i * 2)` against `total += (long long)(i + 1)` has three operations in each
//! arm, the outermost pair factors, the sign extensions under them are the same operation on
//! different operands and would factor too, and they are not looked at because nothing hands them
//! to the join. Doing it to a depth would mean factoring what the select then reads, which is the
//! same function called on what it just produced, and it is left for when something asks for it.
//!
//! A constant operand is not refused and the reason is that it was measured and it goes both ways.
//! An operation with a constant in it takes that constant as an immediate, so factoring turns two
//! free immediates into a select between two values that have to be in registers, and on
//! `product + 2` against `product + 1` outside a loop that costs five bytes. On `total += 1`
//! against `total += 1000` inside one it saves fourteen, because the constants were being
//! rematerialized every iteration anyway. Over the corpus, refusing every constant operand trades
//! thirty two bytes of win for twenty two bytes of loss, which is ten bytes across 1453 programs
//! and is not worth a rule.
//!
//! # The store both arms made
//!
//! Section 22.2's fourth transformation, conditional store replacement, in the half of it that
//! needs no proof. When both arms store to the same place, `if (c) *p = a; else *p = b;` becomes
//! `*p = c ? a : b`, and the branch goes with the rest of them.
//!
//! Half, and which half is the whole point. Section 22.6 calls the other half the worst bug in the
//! document, because a store made on a path that was not going to make one writes memory the
//! program was not going to write. The load modify store form GCC uses, reading the location and
//! writing back what it read on the path that had no store, is not a no-op: it is a write, so it
//! races with another thread writing the same bytes, and it faults if the page is read only. What
//! would license it is knowing the location is written whatever happens, which is the predicate
//! section 22.6 asks for and which nothing here can answer yet.
//!
//! When both arms store to the same address, that predicate is discharged by the shape itself and
//! nothing has to be proved. One store before and one store after, to the same address, of a value
//! the program was going to write there on one path or the other. Nothing new is written, nothing
//! is written twice, and the order of that store against everything else in the function is where
//! it was. So this is the case that goes in, and the one armed case is refused by name rather than
//! by falling through the effects check, so that `-fopt-info-all` says which of the two it was.
//!
//! What has to match beyond the address is the access itself: the flags, and the alignment, size,
//! aliasing node and `restrict` scope that a store carries alongside them, because the one store
//! written below carries one of each and two that disagree have no single answer to carry. The
//! address has to be the same value rather than a provably equal one, which is the strong form of
//! the question and is the only form available without an alias analysis. It also settles where the
//! address comes from: neither arm dominates the other, so a value both of them name is worked out
//! at or above the head, and the one store is written where it is available.
//!
//! The same value rather than the same address is also where most of what this does not catch
//! goes, so the two refusals are counted separately and say which. `if (x > 128) q[i] = 128; else
//! q[i] = x;` works `q + i` out twice, once in each arm, and two instructions that compute the same
//! address are two values, so this walks away from a diamond whose two stores go to the same place
//! by any reading a person would give it. What fixes that is document 16's value numbering turning
//! the two into one, not anything about memory, and hoisting the address by hand into `int *p =
//! &q[i];` is enough to get the fold today.
//!
//! `volatile` and atomic are refused. `volatile` because section 22.6 says never, and the reason is
//! not that the flags fail to match: how many accesses there are and what order they come in are
//! both observable, and a value that arrives through a select is a different program from one that
//! arrives through a branch. Atomic for the ordering rather than the access, since a store with an
//! order on it is a fence as much as a write.
//!
//! # What a select is built for
//!
//! Two sides disagree about a parameter when they hand the join different values, and also when
//! they hand it different values that are the same number. The second half is there because the
//! corpus has eight diamonds whose two arms both work out the same constant, in separate
//! instructions that nothing has hash consed into one, and the tier six rule `select(c, x, x) -> x`
//! does not reach them for exactly the same reason: two operands that are not one value do not
//! match a pattern that writes one name twice. What would reach them is document 12.1's hash
//! consing or document 16's value numbering, and until one of those exists the cheap question is
//! worth asking here, where the alternative is a `select` this pass wrote itself between two sevens.
//!
//! # Why moving an arm's work into the head is safe
//!
//! Because the arm has exactly one predecessor, which is the head. That is checked, and it is the
//! whole of the argument in both directions.
//!
//! Downward: an instruction in the arm reads values that dominate the arm, and the head dominates
//! the arm too, so every one of them is available where the instruction is going. Upward: nothing
//! outside the arm can read what the arm defines except by the arm's own jump, since the arm
//! dominates only itself, and that jump's arguments are exactly what the selects are built out of.
//! An arm with two predecessors would break both halves at once, which is why the check is on the
//! predecessor count and not on the shape of the graph around it.
//!
//! The loop rules that `spec/optimizer/23-jump-threading.md` needs are not needed here, and the
//! reason is worth writing down rather than leaving as an absence. No edge is added, so no loop
//! gains a second way in and no loop can become irreducible. An arm cannot be a loop header, since
//! a header has a back edge and this arm has one predecessor and it is not itself. An arm can be a
//! latch, and then the head becomes the latch instead, which keeps the single latch property
//! document 07.3 wants rather than spoiling it. The one shape that would matter is a join that
//! only its own arms reach, which is a region unreachable from the entry, and the pass asks
//! whether the head is reachable before it looks at anything.
//!
//! # What it refuses, and every one of them is section 22.6
//!
//! An arm that does something. The predicate is [`rucc_ir::Opcode::has_effects`], which is what
//! dead code elimination deletes an instruction under, so an arm this pass will hoist is an arm
//! whose instructions could have been deleted outright had nothing read them. A call, a `volatile`
//! access and a load are all effects by that answer, which closes the second and sixth failures in
//! section 22.6 with one question. The one exception is the pair of stores above, which is the one
//! effect this pass moves and is allowed to because moving it does not change what happens.
//!
//! A store the other side does not match. That is the first failure in section 22.6 and it gets a
//! reason of its own rather than the general one, because it is a different answer rather than a
//! stricter one: the transformation exists, it is section 22.2's fourth, and what is missing is the
//! proof that the location is written whatever happens.
//!
//! An arm that divides. Division is not an effect, because nothing observes it and dead code
//! elimination is right to delete one, but it traps, and a trap on a path that did not have one is
//! section 22.6's third failure. The exception is a divisor that is a constant which is neither
//! zero nor minus one, which cannot trap and is most of the divisions real code contains.
//!
//! A value the two sides disagree about whose type has no `select`. The IR names a `select` at
//! eight, sixteen, thirty two and sixty four bit integers and at nothing else, so producing one of
//! any other type would build a term the back end has no rule for. That is an invisible gap rather
//! than a wrong answer, and the producer is the side that has to avoid it. It is asked after the
//! condition has had its say, because a value nothing has to choose between needs no select and so
//! does not need one that can be lowered.
//!
//! A branch that is already decided. Section 22.6 does not list this one and the corpus found it,
//! on a program whose source says `if (1)`. `simplify-cfg` runs after this pass and turns a decided
//! branch into a jump, and then the arm that cannot run is deleted whole and its work with it.
//! Converting first replaces a branch that costs nothing at run time with a select that costs
//! something, and it keeps alive the work in the arm that never ran, because the fold that would
//! undo it is `select(1, a, b) -> a` and that rule does not exist yet. The case cost twenty eight
//! bytes of `.text` and a multiply that could not happen.
//!
//! The question is put to `simplify_cfg::taken` rather than answered again here, for the
//! reason that function's own documentation gives: two answers about when a branch is decided
//! would be two compilers. It matters in this case rather than being tidiness. The condition on
//! `if (1)` is not a constant, it is `icmp ne 1, 0`, and `fold` leaves that standing on purpose,
//! because nothing lowers an `i1` by itself and folding one would turn working code into code that
//! does not build, which is issue 352. `taken` reads the answer off without leaving anything
//! standing, since the branch that was the comparison's only reader goes at the same time.
//!
//! # The cost rule
//!
//! Section 22.2 states it and this implements it without softening it.
//!
//! Both arms empty of instructions: convert, always. The select replaces a branch with one
//! operation that reads two values which already exist, and there is no machine where that is
//! worse. Nothing about predictability enters, because there is nothing being speculated.
//!
//! What is factored does not count as work. Both arms did the operation, one of them was always
//! going to do it, and afterwards one copy of it runs whichever way the branch would have gone, so
//! nothing is being speculated. A diamond whose arms factor away entirely converts on the same
//! terms as a diamond with empty arms, and one that factors down to two instructions is judged on
//! the two rather than on what it started as.
//!
//! Arms with work left in them: up to [`heuristics::PHIOPT_ARM_INSTRUCTIONS`] instructions each,
//! and only when the branch probability is within
//! [`heuristics::PHIOPT_UNPREDICTABLE_MARGIN_PERCENT`] of even by document 11's estimate. A branch
//! the estimate calls one sided keeps its branch, because if the estimate is right the branch is
//! free and the arm is not.
//!
//! The estimate is usually a guess and the guess is often wrong, which section 22.6 lists as the
//! failure with no defence. Note where that leaves an unpredicted branch: document 11 answers even
//! and says it is guessing, even is inside the margin, so a branch nothing is known about is
//! treated as unpredictable and converted. That is the aggressive reading and it is deliberate,
//! since the alternative is a pass that fires on almost nothing and measures nothing.
//!
//! It is also, today, the only reading, and that is worth saying rather than leaving to be
//! discovered. Every static predictor in document 11 that gives a one sided answer keys on
//! something one arm of the branch does and the other does not: one arm never comes back, one arm
//! calls something cold, one arm leaves the loop, one arm returns a negative number. A diamond has
//! neither of those, because both of its arms fall through to the same block, so the predictors
//! that could refuse a conversion here are exactly the ones a diamond cannot trip. What is left is
//! the branch condition itself, which is `__builtin_expect` at ninety percent and the pointer
//! heuristic at seventy, and only the first of those is outside the margin. `__builtin_expect` is
//! dropped in the front end today, so until it is wired the probability half of the rule refuses
//! nothing at all. The check is here rather than deferred because leaving it out would mean the
//! measurement never showed that, and because the day the hint is wired is the day it starts
//! mattering.
//!
//! # Which level, and how many times
//!
//! Every level that optimizes, which is section 22.2's `-O1` and above.
//!
//! Once. Section 22.7 asks for two instances at `-O2`, one before the loop pipeline and one after,
//! because the loop passes make diamonds. There is no loop pipeline yet, so the second instance
//! would be a second walk over every function to find the shapes the first one already took, and
//! it belongs in the change that adds the passes it exists to clean up after.
//!
//! Section 22.2 also wants a peephole run after this one, so that the rule set can answer what the
//! `select` becomes: `select(c, a, a)` is `a`, `select(c, 1, 0)` is `zext(c)`, and the min, max and
//! abs recognitions are all rules rather than code here. Those rules are tier six of
//! `spec/optimizer/13-rewrite-rules.md` and none of them are written, so the run that would fire
//! them is not in the pipeline yet either. It goes in with them.

use rucc_cost::heuristics;
use rucc_ir::{
    Block, Builder, Def, Extra, Flags, Func, Inst, InstData, IntPred, MemOrder, Opcode, Type, Value,
};

use crate::cfg::Cfg;
use crate::fold::constant;
use crate::profile::Probability;
use crate::range::ops::Truth;
use crate::range::query::Ranges;
use crate::simplify_cfg::{self, Bindings};
use crate::{Analyses, Fuel, Pass, Preserved, Stats};

/// Recorded once for each diamond that became a select.
const CONVERTED: &str =
    "branch whose two arms only work out a value replaced by the value and no branch";

/// Recorded once for each operation both arms did that ended up being done once.
const FACTORED: &str = "operation both arms did to different operands done once below the branch";

/// Recorded once for each value the branch condition settled, so that no select was written for it.
const VALUE_IMPLIED: &str =
    "value the two arms disagreed about settled by the condition rather than by a select";

/// Recorded once for each pair of stores to one place that became one store below the branch.
const STORE_REPLACED: &str = "store both arms made to the same place made once below the branch";

/// Recorded for a diamond one of whose arms does something that has to happen.
const ARM_HAS_EFFECTS: &str =
    "branch kept, an arm does something that only happens on the path it is on";

/// Recorded for a diamond where only one of the two paths stores at all.
const STORE_ON_ONE_PATH: &str =
    "branch kept, a store only one path makes would have to be made on the other path too";

/// Recorded for a diamond where both paths store but not the same store to the same place.
const STORES_DO_NOT_MATCH: &str =
    "branch kept, both paths store but not to one address the two of them name the same way";

/// Recorded for a diamond one of whose arms divides by something that could be zero.
const ARM_MAY_TRAP: &str = "branch kept, an arm divides and doing it on both paths could trap";

/// Recorded for a diamond whose two arms disagree about a value nothing can choose between.
const NO_SELECT_AT_THAT_WIDTH: &str =
    "branch kept, the value the arms disagree about is not a width a select is lowered at";

/// Recorded for a diamond whose arms are more work than the branch is worth.
const ARMS_TOO_LONG: &str = "branch kept, its arms are more work than doing both of them is worth";

/// Recorded for a diamond whose branch the estimate says the machine will get right.
const BRANCH_IS_PREDICTED: &str =
    "branch kept, it goes one way often enough that the machine will predict it";

/// Recorded for a diamond that would have been converted if there had been fuel for it.
const CONDITION_IS_DECIDED: &str =
    "branch kept, its condition is already known and the arm that cannot run is better deleted";
const NO_FUEL: &str = "branch kept, the pass ran out of fuel";

/// The pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhiOpt;

impl Pass for PhiOpt {
    fn name(&self) -> &'static str {
        "phiopt"
    }

    fn describe(&self) -> &'static str {
        "a branch whose two arms only work out a value becomes a select, and the branch goes"
    }

    fn preserves(&self) -> Preserved {
        // Nothing. Blocks stop existing and an edge stops existing with them, so every analysis
        // built on the graph was built on a different graph.
        Preserved::NONE
    }

    fn run(&self, func: &mut Func, an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        if func.entry().is_none() {
            return stats;
        }
        for head in func.blocks().collect::<Vec<Block>>() {
            let cfg = an.cfg(func);
            if !cfg.reaches(head) {
                continue;
            }
            let Some(shape) = diamond(func, cfg, head) else { continue };
            let store = storing(func, &shape);
            // Before the refusals rather than after, because one of them is about a value having a
            // width a select is lowered at, and a value the condition settles gets no select at all.
            // The waste that costs is a range query on a diamond that then turns out to have an
            // effect in it, and the equality gate below is what keeps that from being every diamond.
            let implied = implied(func, an, &shape);
            if let Some(reason) = refused(func, &shape, store.as_ref(), &implied) {
                stats.missed(reason);
                continue;
            }
            let plan = factoring(func, &shape, &implied);
            // What is factored is not speculated. Both arms did the operation, one of them was
            // always going to do it, and after this one copy of it runs whichever way the branch
            // would have gone. So it comes off the count the cost rule is about, and a diamond
            // whose arms factor away entirely converts on the same terms as a diamond with empty
            // arms: always, because there is nothing being done that was not being done before.
            // The store both arms made comes off the count for the same reason a factored operation
            // does. One of the two was always going to run, and afterwards one copy of it runs
            // whichever way the branch would have gone, so nothing about memory is being speculated.
            let replaced = plan.iter().flatten().count() + usize::from(store.is_some());
            let saved = u32::try_from(replaced).unwrap_or(u32::MAX);
            let work = shape
                .arms
                .map(|arm| arm.map_or(0, |block| length(func, block)).saturating_sub(saved));
            if work.iter().any(|&count| count > 0) {
                if work.iter().any(|&count| count > heuristics::PHIOPT_ARM_INSTRUCTIONS) {
                    stats.missed(ARMS_TOO_LONG);
                    continue;
                }
                // The first edge out of the head, which is the arm taken when the condition holds,
                // because `Cfg::successors` is in the order the terminator names its targets. Which
                // of the two is asked about does not matter, since the question is whether the
                // number is near even and the other edge is its complement.
                if !unpredictable(an.frequencies(func).taken(head, 0)) {
                    stats.missed(BRANCH_IS_PREDICTED);
                    continue;
                }
            }
            if !fuel.take() {
                // Where the pass stops rather than where it starts skipping, for the reason jump
                // threading gives: a budget that has reached zero will not have anything in it at
                // the next block either, and the refusals above are the counts worth being true.
                stats.missed(NO_FUEL);
                break;
            }
            convert(func, &shape, &plan, store.as_ref(), &implied);
            // The graph was about the function as it was a moment ago, and the manager clears the
            // cache after the pass returns, which is too late for the next block.
            an.clear();
            for _ in plan.iter().flatten() {
                stats.optimized(FACTORED);
            }
            for _ in implied.iter().flatten() {
                stats.optimized(VALUE_IMPLIED);
            }
            if store.is_some() {
                stats.optimized(STORE_REPLACED);
            }
            stats.optimized(CONVERTED);
        }
        stats
    }
}

/// A branch whose two arms meet again, and what each of them hands the block they meet at.
pub(crate) struct Diamond {
    /// The block the branch is in.
    pub(crate) head: Block,
    /// The bit the branch is on, which is the bit the selects are on.
    pub(crate) cond: Value,
    /// The block both arms reach.
    pub(crate) join: Block,
    /// The block on each side, when that side is a block of its own rather than the join.
    ///
    /// Index zero is the side taken when the condition holds, which is the side `select` calls
    /// `then`, and the order is the order the terminator names its targets in.
    pub(crate) arms: [Option<Block>; 2],
    /// What each side hands the join, in the order the join takes its parameters.
    pub(crate) args: [Vec<Value>; 2],
}

/// The diamond this block is the head of, if it is the head of one.
pub(crate) fn diamond(func: &Func, cfg: &Cfg, head: Block) -> Option<Diamond> {
    let entry = cfg.entry()?;
    let term = func.terminator(head)?;
    if func[term].opcode != Opcode::BrIf {
        return None;
    }
    let cond = *func[func[term].args].first()?;
    let mut targets = func.successors(term);
    let sides = [targets.next()?, targets.next()?];
    // Both arms at the same block is a branch that goes to one place carrying two argument lists.
    // It is convertible and it is rare enough not to be worth a second shape, and `simplify-cfg`
    // takes the case where the two lists agree.
    if sides[0].block == sides[1].block {
        return None;
    }
    let through = [
        passes_through(func, cfg, head, sides[0].block),
        passes_through(func, cfg, head, sides[1].block),
    ];
    // The diamond, then the two triangles. A side that is not the join has to be a block that
    // reaches it, which is what makes the arm below a side that has one.
    let join = match through {
        [Some(left), Some(right)] if left == right => left,
        [Some(left), _] if left == sides[1].block => left,
        [_, Some(right)] if right == sides[0].block => right,
        _ => return None,
    };
    // A join that is the head is a loop with nothing outside it, and one that is the entry is a
    // block control arrives at rather than one it reaches.
    if join == head || join == entry {
        return None;
    }
    let arms = [
        (sides[0].block != join).then_some(sides[0].block),
        (sides[1].block != join).then_some(sides[1].block),
    ];
    let mut args = [Vec::new(), Vec::new()];
    for (index, side) in sides.iter().enumerate() {
        let carried = match arms[index] {
            // The arm's own jump is what tells the join what this side worked out.
            Some(arm) => func.successors(func.terminator(arm)?).next()?.args,
            None => side.args,
        };
        args[index] = func[carried].to_vec();
    }
    Some(Diamond { head, cond, join, arms, args })
}

/// Where this side of the branch ends up, when it is a block whose only job is to get there.
///
/// Everything this asks is needed. Parameters, because a block that takes them is being told
/// something on the edge and there would be nothing to tell it once the edge is gone. One
/// predecessor and it being the head, because that is the whole argument for moving the block's
/// work upward and it is also what makes removing the block afterwards legal. A jump, because an
/// arm that branches is a second decision and this pass is about one.
fn passes_through(func: &Func, cfg: &Cfg, head: Block, block: Block) -> Option<Block> {
    if !func[block].params.is_empty() {
        return None;
    }
    match cfg.predecessors(block) {
        [only] if *only == head => {}
        _ => return None,
    }
    let term = func.terminator(block)?;
    if func[term].opcode != Opcode::Jump {
        return None;
    }
    Some(func.successors(term).next()?.block)
}

/// Why this diamond is left alone, or `None` when nothing is in the way.
///
/// The store plan is passed in because the two stores it names are the one pair of instructions
/// with effects this pass is allowed to move, and everything else with an effect still refuses.
fn refused(
    func: &Func,
    shape: &Diamond,
    store: Option<&Stored>,
    implied: &[Option<usize>],
) -> Option<&'static str> {
    // A branch nobody has to take is not a branch worth removing. `simplify-cfg` runs after this
    // pass and turns a decided branch into a jump, and then the arm that cannot run is deleted
    // whole. Converting first replaces a branch that costs nothing with a select that costs
    // something, and the fold that would undo it is a rule the set does not have yet, so the work
    // in the arm that never ran survives into the machine code. The corpus found this on `if (1)`.
    //
    // The question is put to `simplify-cfg` rather than answered again here, for the reason its
    // own documentation gives: two answers about when a branch is decided would be two compilers.
    // It matters in this case, because the condition on `if (1)` is not a constant, it is a
    // comparison of two constants, which `fold` deliberately leaves standing.
    let term = func.terminator(shape.head).expect("the head of a diamond ends in its branch");
    if simplify_cfg::taken(func, term, &Bindings::new()).is_some() {
        return Some(CONDITION_IS_DECIDED);
    }
    let moving = store.map(|one| one.insts);
    for &arm in shape.arms.iter().flatten() {
        for inst in func.insts(arm) {
            if func.is_terminator(inst) || moving.is_some_and(|two| two.contains(&inst)) {
                continue;
            }
            if func[inst].opcode == Opcode::Store {
                // Named separately from the effects below because it is a different answer rather
                // than a stricter one. A store the other side does not make is section 22.2's
                // fourth transformation without its proof, and section 22.6 calls it the worst bug
                // in the document: making it on both paths writes memory the program was not going
                // to write, which is not a no-op if another thread is writing the same bytes and is
                // not a no-op if the page is read only. What would license it is knowing the
                // location is written whatever happens, and nothing here knows that yet.
                return Some(mismatch(func, shape));
            }
            if func[inst].opcode.has_effects() {
                return Some(ARM_HAS_EFFECTS);
            }
            if !speculatable(func, inst) {
                return Some(ARM_MAY_TRAP);
            }
        }
    }
    let params = func[shape.join].params.to_vec();
    for (index, &param) in params.iter().enumerate() {
        // The two sides agreeing about a parameter is the common case in a triangle, where one
        // side passes on what it was already holding, and it needs no select at all. Neither does
        // one the condition settles, which is why this is asked after that question rather than
        // before it: a value of a type nothing can select between is fine when nothing has to.
        if agree(func, shape.args[0][index], shape.args[1][index]) {
            continue;
        }
        if implied.get(index).copied().flatten().is_some() {
            continue;
        }
        if !selectable(func[param].ty) {
            return Some(NO_SELECT_AT_THAT_WIDTH);
        }
    }
    None
}

/// Whether the two sides hand the join the same thing, so that no `select` is needed for it.
///
/// The same value is the easy answer and it is the one a triangle gives, where one side passes on
/// what it was already holding. The same constant is the answer the corpus asked for. `x ? 7 : 7`
/// arrives here as two `iconst.i32 7` instructions, one in each arm, which are two values because
/// nothing has hash consed them into one. Asking only about the value builds a `select` between two
/// sevens, which costs a compare, a byte and a conditional move to work out that seven is seven.
/// The module comment says what the general answer would be and why it is not available yet.
fn agree(func: &Func, then: Value, other: Value) -> bool {
    if then == other {
        return true;
    }
    let (Some((left, lty)), Some((right, rty))) = (constant(func, then), constant(func, other))
    else {
        return false;
    };
    lty == rty && left == right
}

/// Whether doing this on a path that was not going to do it is harmless.
///
/// Only division asks anything here, because the caller has already refused everything with an
/// effect and what is left is arithmetic. Zero is the divisor everybody knows about. Minus one is
/// the other one: the smallest signed number divided by it is not representable and x86 raises the
/// same exception it raises for zero.
pub(crate) fn speculatable(func: &Func, inst: Inst) -> bool {
    let opcode = func[inst].opcode;
    if !matches!(opcode, Opcode::SDiv | Opcode::UDiv | Opcode::SRem | Opcode::URem) {
        return true;
    }
    let Some(&divisor) = func[func[inst].args].get(1) else { return false };
    let Some((imm, ty)) = constant(func, divisor) else { return false };
    if imm.unsigned() == 0 {
        return false;
    }
    imm.signed(ty) != -1
}

/// A store both arms make to the same place, which becomes one store below the branch.
///
/// Section 22.2's fourth transformation, in the half of it that needs no proof. `if (c) *p = a;
/// else *p = b;` is `*p = c ? a : b`, and the number of stores is one before and one after, to the
/// same address, of a value the program was going to write there on one path or the other.
struct Stored {
    /// The store each side wrote, which goes when the one copy below replaces both.
    insts: [Inst; 2],
    /// What each side wrote, taken in the order the branch names its targets.
    values: [Value; 2],
    /// The address, which is one value both sides named.
    addr: Value,
    /// The store to write once, whose value operand is replaced by the select above it.
    data: InstData,
}

/// Which side's value serves for both, for each join parameter the branch condition settles.
///
/// Section 22.2's second transformation, `value_replacement`. `x = (a == b) ? b : a` is `x = a`,
/// because the only way to arrive carrying `b` is along the edge where `a` and `b` are the same
/// number. The select goes, and so does whichever arm was only there to work the other value out.
///
/// The answer for a parameter is the side whose value is passed on. If the two are known equal on
/// one side's edge, then that side's value is the other side's value there, and the other side's
/// value is right on both edges. Availability comes for free: everything both arms worked out is
/// moved into the head before the jump is written, so a value that came from an arm is defined
/// above the point it is now read at.
///
/// # Why the condition has to be an equality
///
/// This is the pass's one expensive question and section 22.7 says so. What answers it is document
/// 10's relational oracle, which records what a dominating edge established between two values, and
/// the edge out of a `br_if` establishes something about two values only when the branch is on a
/// comparison of them. A branch on `x < n` says nothing about whether two other values are equal,
/// so asking is a query that cannot come back with anything. Gating on the comparison being an
/// equality is what turns this from a query per diamond into a query per diamond that could
/// possibly answer, which on the corpus is a small fraction of them. GCC gates the same way, on
/// `EQ_EXPR` and `NE_EXPR` at `gcc/tree-ssa-phiopt.cc`.
fn implied(func: &Func, an: &mut Analyses, shape: &Diamond) -> Vec<Option<usize>> {
    let count = shape.args[0].len();
    let mut answers = vec![None; count];
    if !equality(func, shape.cond) {
        return answers;
    }
    let asking: Vec<usize> = (0..count).filter(|&index| worth_asking(func, shape, index)).collect();
    if asking.is_empty() {
        return answers;
    }
    // Cloned because the two are held at once and the cache hands out one borrow at a time. It is
    // paid for only by a diamond that got this far, which the two gates above have already made
    // rare, and section 22.7 is where the cost of this query was budgeted.
    let cfg = an.cfg(func).clone();
    let dom = an.dominators(func).clone();
    let mut ranges = Ranges::new(func, &cfg, &dom);
    for index in asking {
        let pair = [shape.args[0][index], shape.args[1][index]];
        for side in 0..2 {
            let Some(block) = shape.arms[side] else { continue };
            if ranges.compare(IntPred::Eq, pair[0], pair[1], block) == Truth::Always {
                answers[index] = Some(1 - side);
                break;
            }
        }
    }
    answers
}

/// Whether the branch is on a comparison that says two values are the same or are not.
fn equality(func: &Func, cond: Value) -> bool {
    let Def::Result { inst, .. } = func[cond].def else { return false };
    if func[inst].opcode != Opcode::ICmp {
        return false;
    }
    matches!(func[inst].extra, Extra::IntPred(IntPred::Eq | IntPred::Ne))
}

/// Whether this join parameter is one the oracle could have something to say about.
///
/// Two sides that agree need nothing. Two constants that are not the same number are not the same
/// number on any edge, and asking is a query whose answer is already in hand.
fn worth_asking(func: &Func, shape: &Diamond, index: usize) -> bool {
    let pair = [shape.args[0][index], shape.args[1][index]];
    if agree(func, pair[0], pair[1]) {
        return false;
    }
    constant(func, pair[0]).is_none() || constant(func, pair[1]).is_none()
}

/// Which of the two store refusals this diamond is, once it is known to be one of them.
///
/// The two are worth separating because they say different things about what would fix them. One
/// path storing is section 22.6's predicate, which is a proof nothing here can do. Both paths
/// storing and not matching is usually two arms that worked the same address out separately, which
/// is `a[i] = ...` on both sides, and what fixes that is document 16's value numbering making the
/// two into one value rather than anything about memory.
fn mismatch(func: &Func, shape: &Diamond) -> &'static str {
    let [Some(then), Some(other)] = shape.arms else { return STORE_ON_ONE_PATH };
    match (stored_in(func, then), stored_in(func, other)) {
        (Some(_), Some(_)) => STORES_DO_NOT_MATCH,
        _ => STORE_ON_ONE_PATH,
    }
}

/// The store this diamond can move below the branch, if it has one.
///
/// Both sides have to have a block, which is what makes this the safe half of the transformation.
/// A triangle has one side that is the join, and a store in the join already runs whichever way the
/// branch went, so there is nothing here to move and the shape that reaches this with one arm is
/// the one where a store happens on one path only. That one is refused above.
fn storing(func: &Func, shape: &Diamond) -> Option<Stored> {
    let [Some(then), Some(other)] = shape.arms else { return None };
    let insts = [stored_in(func, then)?, stored_in(func, other)?];
    let data = [func[insts[0]], func[insts[1]]];
    // The flags are what the optimizer is licensed to assume about the access, so one store written
    // under the union of two sets of assumptions would be claiming on one path something only the
    // other path established. `volatile` is refused outright rather than by disagreeing, because
    // section 22.6 says never and because the reason is not the flag matching: both how many
    // accesses there are and what order they come in are observable, and a value that arrives
    // through a select is a different program from one that arrives through a branch.
    if data[0].flags != data[1].flags || data[0].flags.contains(Flags::VOLATILE) {
        return None;
    }
    let (Extra::Mem(one), Extra::Mem(two)) = (data[0].extra, data[1].extra) else { return None };
    // The alignment, the size, the aliasing node and the `restrict` scope, all of which the one
    // store carries forward, so two that disagree about any of them have no single answer to carry.
    if func[one] != func[two] || func[one].order != MemOrder::NotAtomic {
        return None;
    }
    // A store names what it writes and then where, which is the order the builder takes them in.
    let &[then, addr] = func[data[0].args].first_chunk::<2>()?;
    let &[other, addr_two] = func[data[1].args].first_chunk::<2>()?;
    // The same value for the address, which is stronger than the same address and is what can be
    // checked without an alias analysis. It also settles where that value comes from: neither arm
    // dominates the other, so a value both of them name is one worked out at or above the head, and
    // the one store is written in the head where it is available.
    if addr != addr_two || func[then].ty != func[other].ty {
        return None;
    }
    if !agree(func, then, other) && !selectable(func[then].ty) {
        return None;
    }
    Some(Stored { insts, values: [then, other], addr, data: data[0] })
}

/// The one store this arm makes, if it makes exactly one and does nothing else that has to happen.
///
/// Exactly one, because two stores below one select is two selects and a shape nothing has asked
/// for. Nothing else with an effect, because everything else with an effect is still refused and
/// this is the check that says so: an arm that stores and also calls something has a call that only
/// happens on the path it is on, and no amount of agreement about the store changes that.
fn stored_in(func: &Func, arm: Block) -> Option<Inst> {
    let mut store = None;
    for inst in func.insts(arm) {
        if func.is_terminator(inst) || !func[inst].opcode.has_effects() {
            continue;
        }
        if func[inst].opcode != Opcode::Store || store.is_some() {
            return None;
        }
        store = Some(inst);
    }
    store
}

/// One join argument both arms worked out the same way, and the one operand they disagreed about.
///
/// Section 22.2's third transformation. `cond ? f(a) : f(b)` is `f(cond ? a : b)`, which is one
/// operation where there were two and one select either way, and it is structural rather than a
/// rewrite rule because the two `f`s are in different blocks and no pattern spans blocks.
struct Factored {
    /// The instruction each side wrote, which goes when the one copy below replaces both.
    insts: [Inst; 2],
    /// What each side handed that instruction, taken from the side taken when the condition holds.
    operands: Vec<Value>,
    /// The one position the two sides put different values in, and what each of them put there.
    ///
    /// `None` when they agree in every position, which is both arms computing the same thing from
    /// the same operands. Then one copy serves both and there is no select at all.
    differ: Option<(usize, [Value; 2])>,
    /// The instruction to write once, whose operand list is replaced by the one above.
    data: InstData,
    /// What it produces.
    ty: Type,
}

/// What can be factored out of each of the join's parameters, in the order the join takes them.
///
/// A triangle factors nothing. One of its sides is the join itself, so there is no block on that
/// side holding an operation to pair the other one with, and what that side hands the join is a
/// value worked out before the branch.
fn factoring(func: &Func, shape: &Diamond, implied: &[Option<usize>]) -> Vec<Option<Factored>> {
    let count = shape.args[0].len();
    let [Some(then), Some(other)] = shape.arms else {
        return (0..count).map(|_| None).collect();
    };
    (0..count)
        .map(|index| {
            // A value the condition settled is passed on whole, so there is no operation to write
            // once below and the two that worked the two values out are left for dead code.
            if implied.get(index).copied().flatten().is_some() {
                return None;
            }
            factored(func, shape, [then, other], index)
        })
        .collect()
}

/// Whether this join argument is the same operation on both sides, and what to write instead.
fn factored(func: &Func, shape: &Diamond, arms: [Block; 2], index: usize) -> Option<Factored> {
    let sides = [shape.args[0][index], shape.args[1][index]];
    // Two sides that agree need no operation written at all, and the caller passes the value on.
    if agree(func, sides[0], sides[1]) {
        return None;
    }
    let insts = [written_in(func, arms[0], sides[0])?, written_in(func, arms[1], sides[1])?];
    let data = [func[insts[0]], func[insts[1]]];
    // Everything about the two has to match except the operands. The flags are what the optimizer
    // is licensed to assume, so writing one copy under the union of two sets of assumptions would
    // be claiming on one path something only the other path established. The extra is whatever the
    // instruction carries that is not an operand, which for a comparison is the predicate, and two
    // predicates that differ are two different questions.
    if data[0].opcode != data[1].opcode || data[0].flags != data[1].flags {
        return None;
    }
    if data[0].extra != data[1].extra || func[sides[0]].ty != func[sides[1]].ty {
        return None;
    }
    let operands = [func[data[0].args].to_vec(), func[data[1].args].to_vec()];
    if operands[0].len() != operands[1].len() {
        return None;
    }
    let mut apart =
        operands[0].iter().zip(&operands[1]).enumerate().filter(|(_, (one, two))| one != two);
    let differ = match (apart.next(), apart.next()) {
        // Two positions apart would need two selects, and two selects and one operation is what
        // one select and two operations already cost. There is nothing to win, so it is left.
        (_, Some(_)) => return None,
        (Some((at, (&one, &two))), None) => {
            if func[one].ty != func[two].ty || !selectable(func[one].ty) {
                return None;
            }
            Some((at, [one, two]))
        }
        (None, None) => None,
    };
    let ty = func[sides[0]].ty;
    Some(Factored { insts, operands: operands[0].clone(), differ, data: data[0], ty })
}

/// The instruction in this arm that works out this value, if the arm is where it comes from and the
/// only thing that reads it is the jump to the join.
///
/// Both halves are needed. The arm has to be where it is worked out, because an operation to factor
/// out is one this pass is about to stop writing and it can only stop writing what it can find.
/// Nothing else can read it, because the one copy that replaces the two is written after the arms
/// have gone and a second reader in the arm would have been left pointing at an instruction that is
/// no longer in any block.
fn written_in(func: &Func, arm: Block, value: Value) -> Option<Inst> {
    let inst = func
        .insts(arm)
        .find(|&inst| func[inst].results == 1 && func[inst].first_result == Some(value))?;
    let mut seen = 0;
    for inst in func.insts(arm) {
        seen += func[func[inst].args].iter().filter(|&&arg| arg == value).count();
        for call in func.successors(inst) {
            seen += func[call.args].iter().filter(|&&arg| arg == value).count();
        }
    }
    (seen == 1).then_some(inst)
}

/// Whether a value of this type is one a `select` can choose.
///
/// The four widths `crates/rucc-ir/src/term.rs` names a `select` at. A wider integer, a float, a
/// pointer, a bit or a vector has no head, so a `select` of one would be a term the rule set has
/// no lowering for and the failure would be at instruction selection rather than here.
fn selectable(ty: Type) -> bool {
    ty.is_scalar() && ty.is_int() && matches!(ty.bits(), 8 | 16 | 32 | 64)
}

/// How much work an arm does, not counting the jump that is about to go.
pub(crate) fn length(func: &Func, block: Block) -> u32 {
    let count = func.insts(block).filter(|&inst| !func.is_terminator(inst)).count();
    u32::try_from(count).unwrap_or(u32::MAX)
}

/// Whether the estimate leaves enough doubt about this branch to be worth removing it.
pub(crate) fn unpredictable(taken: Probability) -> bool {
    let margin = heuristics::PHIOPT_UNPREDICTABLE_MARGIN_PERCENT * (Probability::SCALE / 100);
    taken.parts() >= margin && taken.parts() <= Probability::SCALE - margin
}

/// Moves the arms into the head, builds the selects and takes the branch out.
///
/// The order matters and is the reason this is one function. The branch goes first, so that what
/// the arms were doing can be appended to the head without anything having to be threaded around a
/// terminator. The selects are built after that work has moved, since they read what it produced.
/// The jump goes last because it is the terminator.
fn convert(
    func: &mut Func,
    shape: &Diamond,
    plan: &[Option<Factored>],
    store: Option<&Stored>,
    implied: &[Option<usize>],
) {
    let term = func.terminator(shape.head).expect("the head of a diamond ends in its branch");
    let span = func.span(term);
    func.remove_inst(term);
    let mut dropped: Vec<Inst> = plan.iter().flatten().flat_map(|one| one.insts).collect();
    dropped.extend(store.iter().flat_map(|one| one.insts));
    for &arm in shape.arms.iter().flatten() {
        for inst in func.insts(arm).collect::<Vec<Inst>>() {
            if func.is_terminator(inst) {
                continue;
            }
            func.remove_inst(inst);
            // A factored operation is not moved, it is replaced. One copy of it is written below,
            // after the selects it reads, and these two are what that copy is instead of.
            if !dropped.contains(&inst) {
                func.append_inst(shape.head, inst);
            }
        }
    }
    let mut build = Builder::new(func, shape.head).at(span);
    let mut args = Vec::with_capacity(shape.args[0].len());
    for (index, (&then, &other)) in shape.args[0].iter().zip(&shape.args[1]).enumerate() {
        // A value the condition settled, passed on as it is. The side named is the one whose value
        // is right on both edges, which is the side the two were not shown to be equal on.
        if let Some(side) = implied.get(index).copied().flatten() {
            args.push(shape.args[side][index]);
            continue;
        }
        if let Some(one) = &plan[index] {
            let mut operands = one.operands.clone();
            if let Some((at, sides)) = one.differ {
                operands[at] = build.select(shape.cond, sides[0], sides[1]);
            }
            let list = build.func().push_values(&operands);
            args.push(build.value(InstData { args: list, ..one.data }, one.ty));
            continue;
        }
        // The condition holds on the first side, which is the side `select` takes when the bit is
        // one, so the order the branch named its targets in is the order the arguments go in.
        let same = agree(build.func(), then, other);
        args.push(if same { then } else { build.select(shape.cond, then, other) });
    }
    // After everything the arms were doing has moved, because the value being stored is often one
    // of the things they were working out, and before the jump because the jump is the terminator.
    if let Some(one) = store {
        let [then, other] = one.values;
        let same = agree(build.func(), then, other);
        let what = if same { then } else { build.select(shape.cond, then, other) };
        let list = build.func().push_values(&[what, one.addr]);
        build.inst(InstData { args: list, ..one.data }, &[]);
    }
    build.jump(shape.join, &args);
    // Nothing arrives at the arms now, and section 6.5 makes taking an unreachable block out the
    // standing obligation of whichever pass stranded it rather than something the next pass tidies
    // up. The verifier holds every pass to that.
    for &arm in shape.arms.iter().flatten() {
        func.remove_block(arm);
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{
        Block, Builder, Flags, Func, IntPred, MemInfo, MemOrder, Opcode, Restrict, Signature, Type,
        Value,
    };

    use super::PhiOpt;
    use crate::profile::{Probability, Quality};
    use crate::stats::Kind;
    use crate::{Analyses, Fuel, Pass, Stats};

    /// Runs the pass with as much fuel as it wants.
    fn phiopt(func: &mut Func) -> Stats {
        PhiOpt.run(func, &mut Analyses::new(), &mut Fuel::unlimited())
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

    /// The opcodes a block holds, in order.
    fn opcodes(func: &Func, block: usize) -> Vec<Opcode> {
        let block = Block::from_usize(block);
        func.insts(block).map(|inst| func[inst].opcode).collect()
    }

    /// What a block's terminator carries on its first edge.
    fn carries(func: &Func, block: usize) -> Vec<Value> {
        let block = Block::from_usize(block);
        let term = func.terminator(block).expect("every block here has one");
        let call = func.successors(term).next().expect("a terminator here has an edge");
        func[call.args].to_vec()
    }

    /// Four aligned bytes, ordinary, with nothing known about aliasing.
    fn plain() -> MemInfo {
        MemInfo {
            size: 4,
            align: 4,
            order: MemOrder::NotAtomic,
            tbaa: None,
            restrict: Restrict::NONE,
        }
    }

    /// A store, which is the instruction used here whenever something has to happen.
    fn store_something(build: &mut Builder<'_>) {
        let what = build.iconst(Type::int(32), 7);
        let address = build.iconst(Type::int(64), 16);
        let address = build.unary(Opcode::IntToPtr, address, Type::PTR);
        build.store(what, address, plain(), Flags::NONE);
    }

    /// `if (x < 0) *p = a; else *p = b;`, with both stores told the same thing about the access.
    ///
    /// The address is a function parameter, so it is one value both arms name, and the two values
    /// written are the other two parameters. Block 0 is the head, blocks 1 and 2 are the arms and
    /// block 3 is the join, which takes nothing and returns.
    fn both_arms_store(info: MemInfo, flags: [Flags; 2], addresses: bool) -> Func {
        let mut names = Interner::new();
        let ints = [Type::PTR, Type::int(32), Type::int(32), Type::PTR];
        let signature = Signature::new().with_params(&ints);
        let mut func = Func::new(names.intern("f"), signature);
        let head = func.create_block();
        let address = func.append_param(head, Type::PTR);
        let written =
            [func.append_param(head, Type::int(32)), func.append_param(head, Type::int(32))];
        let elsewhere = func.append_param(head, Type::PTR);
        let arms = [func.create_block(), func.create_block()];
        let join = func.create_block();

        let mut build = Builder::new(&mut func, head);
        let zero = build.iconst(Type::int(32), 0);
        let test = build.icmp(IntPred::Slt, written[0], zero);
        build.br_if(test, arms[0], &[], arms[1], &[]);
        for (index, arm) in arms.iter().enumerate() {
            let mut build = Builder::new(&mut func, *arm);
            let where_to = if addresses && index == 1 { elsewhere } else { address };
            build.store(written[index], where_to, info, flags[index]);
            build.jump(join, &[]);
        }
        let mut build = Builder::new(&mut func, join);
        build.ret(&[]);
        func
    }

    /// `x < y ? a : b`, as a diamond whose two arms are empty.
    ///
    /// Block 0 is the head and takes the two values it compares as function parameters, blocks 1
    /// and 2 are the arms and carry one of two constants, and block 3 is the join and returns what
    /// it was given.
    fn empty_arms() -> Func {
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::int(32), Type::int(32)]);
        let mut func = Func::new(names.intern("f"), signature);
        let head = func.create_block();
        let left = func.append_param(head, Type::int(32));
        let right = func.append_param(head, Type::int(32));
        let arms = [func.create_block(), func.create_block()];
        let join = func.create_block();
        let param = func.append_param(join, Type::int(32));

        let mut build = Builder::new(&mut func, head);
        let test = build.icmp(IntPred::Slt, left, right);
        build.br_if(test, arms[0], &[], arms[1], &[]);
        for (arm, value) in arms.iter().zip([1, 2]) {
            let mut build = Builder::new(&mut func, *arm);
            let it = build.iconst(Type::int(32), value);
            build.jump(join, &[it]);
        }
        let mut build = Builder::new(&mut func, join);
        build.ret(&[param]);
        func
    }

    #[test]
    fn a_branch_that_is_already_decided_is_left_for_simplify_cfg() {
        // What `if (1)` looks like by the time it gets here. Converting would build a select on a
        // constant and keep the arm that cannot run, and the pass that would fold it does not
        // exist, so the answer is to leave the branch alone and let the arm be deleted whole.
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let head = func.create_block();
        let arms = [func.create_block(), func.create_block()];
        let join = func.create_block();
        let param = func.append_param(join, Type::int(32));

        let mut build = Builder::new(&mut func, head);
        // What `if (1)` reaches this pass as. Not a constant, a comparison of two constants, since
        // `fold` will not turn an `icmp` into an `i1` that nothing lowers.
        let one = build.iconst(Type::int(32), 1);
        let zero = build.iconst(Type::int(32), 0);
        let test = build.icmp(IntPred::Ne, one, zero);
        build.br_if(test, arms[0], &[], arms[1], &[]);
        for (arm, value) in arms.iter().zip([1, 2]) {
            let mut build = Builder::new(&mut func, *arm);
            let it = build.iconst(Type::int(32), value);
            build.jump(join, &[it]);
        }
        let mut build = Builder::new(&mut func, join);
        build.ret(&[param]);

        let stats = phiopt(&mut func);
        assert_eq!(stats.count(Kind::Missed, super::CONDITION_IS_DECIDED), 1);
        assert_eq!(blocks(&func), vec![0, 1, 2, 3]);
    }

    #[test]
    fn a_diamond_whose_arms_are_empty_becomes_a_select() {
        let mut func = empty_arms();
        let stats = phiopt(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::CONVERTED), 1);
        // The two constants moved up with the arms, and the select is what the branch was.
        assert_eq!(
            opcodes(&func, 0),
            vec![Opcode::ICmp, Opcode::IConst, Opcode::IConst, Opcode::Select, Opcode::Jump]
        );
        assert_eq!(goes_to(&func, 0), vec![3]);
        assert_eq!(blocks(&func), vec![0, 3]);
    }

    #[test]
    fn the_side_the_condition_holds_on_is_the_side_the_select_takes_first() {
        let mut func = empty_arms();
        phiopt(&mut func);
        let select = func
            .insts(Block::from_usize(0))
            .find(|&inst| func[inst].opcode == Opcode::Select)
            .expect("the select the pass just built");
        let args = func[func[select].args].to_vec();
        let one = crate::fold::constant(&func, args[1]).expect("the true arm carried a constant");
        let two = crate::fold::constant(&func, args[2]).expect("the false arm carried a constant");
        assert_eq!(one.0.unsigned(), 1, "the arm the branch named first");
        assert_eq!(two.0.unsigned(), 2, "the arm the branch named second");
    }

    /// A triangle: one side goes straight to the join carrying what it already had.
    #[test]
    fn a_triangle_whose_empty_side_goes_straight_to_the_join_is_converted() {
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::int(32)]);
        let mut func = Func::new(names.intern("f"), signature);
        let head = func.create_block();
        let outside = func.append_param(head, Type::int(32));
        let arm = func.create_block();
        let join = func.create_block();
        let param = func.append_param(join, Type::int(32));

        let mut build = Builder::new(&mut func, head);
        let zero = build.iconst(Type::int(32), 0);
        let test = build.icmp(IntPred::Slt, outside, zero);
        build.br_if(test, arm, &[], join, &[outside]);
        let mut build = Builder::new(&mut func, arm);
        let it = build.iconst(Type::int(32), 0);
        build.jump(join, &[it]);
        let mut build = Builder::new(&mut func, join);
        build.ret(&[param]);

        let stats = phiopt(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::CONVERTED), 1);
        assert_eq!(blocks(&func), vec![0, 2]);
        assert_eq!(goes_to(&func, 0), vec![2]);
        assert_eq!(opcodes(&func, 0).last(), Some(&Opcode::Jump));
    }

    #[test]
    fn a_parameter_both_sides_agree_about_needs_no_select() {
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::int(32)]);
        let mut func = Func::new(names.intern("f"), signature);
        let head = func.create_block();
        let outside = func.append_param(head, Type::int(32));
        let arms = [func.create_block(), func.create_block()];
        let join = func.create_block();
        let param = func.append_param(join, Type::int(32));

        let mut build = Builder::new(&mut func, head);
        let zero = build.iconst(Type::int(32), 0);
        let test = build.icmp(IntPred::Slt, outside, zero);
        build.br_if(test, arms[0], &[], arms[1], &[]);
        for arm in arms {
            let mut build = Builder::new(&mut func, arm);
            build.jump(join, &[outside]);
        }
        let mut build = Builder::new(&mut func, join);
        build.ret(&[param]);

        let stats = phiopt(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::CONVERTED), 1);
        assert!(!opcodes(&func, 0).contains(&Opcode::Select), "both sides carried the same value");
        assert_eq!(carries(&func, 0), vec![outside]);
    }

    #[test]
    fn two_sides_carrying_the_same_number_need_no_select_either() {
        // `x ? 7 : 7`, which the corpus has eight of. The two sevens are two values, because
        // nothing has hash consed them into one, so asking only whether the values are equal
        // builds a select between two sevens and pays a compare and a conditional move for it.
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::int(32)]);
        let mut func = Func::new(names.intern("f"), signature);
        let head = func.create_block();
        let outside = func.append_param(head, Type::int(32));
        let arms = [func.create_block(), func.create_block()];
        let join = func.create_block();
        let param = func.append_param(join, Type::int(32));

        let mut build = Builder::new(&mut func, head);
        let zero = build.iconst(Type::int(32), 0);
        let test = build.icmp(IntPred::Slt, outside, zero);
        build.br_if(test, arms[0], &[], arms[1], &[]);
        for arm in arms {
            let mut build = Builder::new(&mut func, arm);
            let seven = build.iconst(Type::int(32), 7);
            build.jump(join, &[seven]);
        }
        let mut build = Builder::new(&mut func, join);
        build.ret(&[param]);

        let stats = phiopt(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::CONVERTED), 1);
        assert!(!opcodes(&func, 0).contains(&Opcode::Select), "both sides carried a seven");
    }

    #[test]
    fn two_sides_carrying_different_numbers_still_get_a_select() {
        let mut func = empty_arms();
        let stats = phiopt(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::CONVERTED), 1);
        assert!(opcodes(&func, 0).contains(&Opcode::Select), "one and two are not the same number");
    }

    /// `x = (a == b) ? b : a`, as the diamond it arrives here as.
    ///
    /// Block 0 is the head and takes the two values, blocks 1 and 2 are the arms and each carries
    /// one of them to block 3, which returns what it was given. The comparison's predicate and the
    /// type of the two values are what the tests below vary.
    fn condition_settles_it(pred: IntPred, ty: Type) -> Func {
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[ty, ty]);
        let mut func = Func::new(names.intern("f"), signature);
        let head = func.create_block();
        let left = func.append_param(head, ty);
        let right = func.append_param(head, ty);
        let arms = [func.create_block(), func.create_block()];
        let join = func.create_block();
        let param = func.append_param(join, ty);

        let mut build = Builder::new(&mut func, head);
        let test = build.icmp(pred, left, right);
        build.br_if(test, arms[0], &[], arms[1], &[]);
        // The side taken when the condition holds carries the right hand value, the other side
        // carries the left, which is what makes the two the same number on one edge and not the
        // other. Which side that is follows the predicate.
        for (arm, value) in arms.iter().zip([right, left]) {
            let mut build = Builder::new(&mut func, *arm);
            build.jump(join, &[value]);
        }
        let mut build = Builder::new(&mut func, join);
        build.ret(&[param]);
        func
    }

    #[test]
    fn a_value_the_condition_says_is_the_other_one_needs_no_select() {
        let mut func = condition_settles_it(IntPred::Eq, Type::int(32));
        let stats = phiopt(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::VALUE_IMPLIED), 1);
        assert_eq!(stats.count(Kind::Optimized, super::CONVERTED), 1);
        assert_eq!(opcodes(&func, 0), vec![Opcode::ICmp, Opcode::Jump]);
        // The value passed on is the one carried by the side the two were not shown equal on.
        let params = func[Block::from_usize(0)].params.to_vec();
        assert_eq!(carries(&func, 0), vec![params[0]]);
        assert_eq!(blocks(&func), vec![0, 3]);
    }

    /// The same with the branch the other way round, where the equal side is the one not taken.
    #[test]
    fn an_inequality_settles_it_from_the_other_side() {
        let mut func = condition_settles_it(IntPred::Ne, Type::int(32));
        let stats = phiopt(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::VALUE_IMPLIED), 1);
        let params = func[Block::from_usize(0)].params.to_vec();
        assert_eq!(carries(&func, 0), vec![params[1]]);
    }

    /// A pointer has no `select`, and a value nothing has to choose between does not need one.
    #[test]
    fn a_value_of_a_type_with_no_select_is_still_settled_by_the_condition() {
        let mut func = condition_settles_it(IntPred::Eq, Type::PTR);
        let stats = phiopt(&mut func);
        assert_eq!(stats.count(Kind::Missed, super::NO_SELECT_AT_THAT_WIDTH), 0);
        assert_eq!(stats.count(Kind::Optimized, super::VALUE_IMPLIED), 1);
        assert_eq!(opcodes(&func, 0), vec![Opcode::ICmp, Opcode::Jump]);
    }

    /// A branch on anything but an equality is not asked about, and the select is written as usual.
    #[test]
    fn a_branch_that_is_not_an_equality_gets_its_select() {
        let mut func = condition_settles_it(IntPred::Slt, Type::int(32));
        let stats = phiopt(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::VALUE_IMPLIED), 0);
        assert_eq!(stats.count(Kind::Optimized, super::CONVERTED), 1);
        assert!(opcodes(&func, 0).contains(&Opcode::Select));
    }

    /// Two constants that are not the same number are not the same number on any edge.
    #[test]
    fn two_different_constants_are_not_asked_about() {
        let mut func = empty_arms();
        let stats = phiopt(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::VALUE_IMPLIED), 0);
        assert!(opcodes(&func, 0).contains(&Opcode::Select));
    }

    #[test]
    fn a_store_both_arms_make_to_one_place_is_made_once_below_the_branch() {
        let mut func = both_arms_store(plain(), [Flags::NONE; 2], false);
        let stats = phiopt(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::STORE_REPLACED), 1);
        assert_eq!(stats.count(Kind::Optimized, super::CONVERTED), 1);
        // One store, below the select that chooses what it writes, and no branch above either.
        assert_eq!(
            opcodes(&func, 0),
            vec![Opcode::IConst, Opcode::ICmp, Opcode::Select, Opcode::Store, Opcode::Jump]
        );
        assert_eq!(blocks(&func), vec![0, 3]);
        assert_eq!(goes_to(&func, 0), vec![3]);
    }

    #[test]
    fn the_one_store_writes_what_the_side_the_condition_holds_on_was_writing() {
        let mut func = both_arms_store(plain(), [Flags::NONE; 2], false);
        phiopt(&mut func);
        let head = Block::from_usize(0);
        let select = func
            .insts(head)
            .find(|&inst| func[inst].opcode == Opcode::Select)
            .expect("the select the pass just built");
        let store = func
            .insts(head)
            .find(|&inst| func[inst].opcode == Opcode::Store)
            .expect("the one store that is left");
        let chosen = func[func[select].args].to_vec();
        let written = func[func[store].args].to_vec();
        // The head's parameters in order: the address, then what each side writes.
        let params = func[head].params.to_vec();
        assert_eq!(chosen[1], params[1], "the arm the branch named first");
        assert_eq!(chosen[2], params[2], "the arm the branch named second");
        assert_eq!(written[0], func[select].first_result.expect("a select produces one value"));
        assert_eq!(written[1], params[0], "the address both arms named");
    }

    /// Both arms writing the same value needs no select, only the one store.
    #[test]
    fn two_arms_that_write_the_same_thing_get_a_store_and_no_select() {
        let mut func = both_arms_store(plain(), [Flags::NONE; 2], false);
        // Point the second arm's store at the first arm's value, which is what the front end
        // produces when both branches of a conditional assign the same thing.
        let head = Block::from_usize(0);
        let params = func[head].params.to_vec();
        let store = func
            .insts(Block::from_usize(2))
            .find(|&inst| func[inst].opcode == Opcode::Store)
            .expect("the second arm's store");
        let args = func.push_values(&[params[1], params[0]]);
        func[store].args = args;

        let stats = phiopt(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::STORE_REPLACED), 1);
        assert_eq!(
            opcodes(&func, 0),
            vec![Opcode::IConst, Opcode::ICmp, Opcode::Store, Opcode::Jump]
        );
    }

    /// Two stores to two different places is two writes, and doing both is writing one of them twice.
    #[test]
    fn two_arms_that_store_to_different_addresses_keep_their_branch() {
        let mut func = both_arms_store(plain(), [Flags::NONE; 2], true);
        let stats = phiopt(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::STORE_REPLACED), 0);
        assert_eq!(stats.count(Kind::Missed, super::STORES_DO_NOT_MATCH), 1);
        assert_eq!(goes_to(&func, 0), vec![1, 2]);
    }

    #[test]
    fn a_volatile_store_keeps_its_branch_even_when_both_arms_make_it() {
        let mut func = both_arms_store(plain(), [Flags::VOLATILE; 2], false);
        let stats = phiopt(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::STORE_REPLACED), 0);
        assert_eq!(stats.count(Kind::Missed, super::STORES_DO_NOT_MATCH), 1);
        assert_eq!(goes_to(&func, 0), vec![1, 2]);
    }

    #[test]
    fn an_atomic_store_keeps_its_branch_even_when_both_arms_make_it() {
        let mut func = both_arms_store(
            MemInfo { order: MemOrder::SeqCst, ..plain() },
            [Flags::NONE; 2],
            false,
        );
        let stats = phiopt(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::STORE_REPLACED), 0);
        assert_eq!(stats.count(Kind::Missed, super::STORES_DO_NOT_MATCH), 1);
    }

    /// Two stores told different things about the access have no one answer to carry downward.
    #[test]
    fn two_stores_that_disagree_about_the_access_keep_their_branch() {
        let mut func = both_arms_store(plain(), [Flags::NONE; 2], false);
        let store = func
            .insts(Block::from_usize(2))
            .find(|&inst| func[inst].opcode == Opcode::Store)
            .expect("the second arm's store");
        let mem = func.add_mem(MemInfo { align: 1, ..plain() });
        func[store].extra = rucc_ir::Extra::Mem(mem);

        let stats = phiopt(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::STORE_REPLACED), 0);
        assert_eq!(stats.count(Kind::Missed, super::STORES_DO_NOT_MATCH), 1);
    }

    /// The store comes off the work count, because one of the two arms was always going to make it.
    ///
    /// Each arm here holds the store and as much other work as the rule allows, so counting the
    /// store as work would put both arms one over the limit and the branch would stay.
    #[test]
    fn a_store_each_way_does_not_count_against_how_long_the_arms_may_be() {
        let mut func = both_arms_store(plain(), [Flags::NONE; 2], false);
        let params = func[Block::from_usize(0)].params.to_vec();
        for arm in [1, 2] {
            let block = Block::from_usize(arm);
            let term = func.terminator(block).expect("an arm ends in its jump");
            func.remove_inst(term);
            let mut build = Builder::new(&mut func, block);
            let mut value = params[1];
            for _ in 0..rucc_cost::heuristics::PHIOPT_ARM_INSTRUCTIONS {
                value = build.binary(Opcode::Add, value, params[2], Flags::NONE);
            }
            func.append_inst(block, term);
        }

        let stats = phiopt(&mut func);
        assert_eq!(stats.count(Kind::Missed, super::ARMS_TOO_LONG), 0);
        assert_eq!(stats.count(Kind::Optimized, super::STORE_REPLACED), 1);
    }

    /// A store one side makes and the other does not, which is the transformation with no proof.
    #[test]
    fn an_arm_that_stores_where_the_other_does_not_keeps_its_branch() {
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::int(32)]);
        let mut func = Func::new(names.intern("f"), signature);
        let head = func.create_block();
        let outside = func.append_param(head, Type::int(32));
        let arms = [func.create_block(), func.create_block()];
        let join = func.create_block();
        let param = func.append_param(join, Type::int(32));

        let mut build = Builder::new(&mut func, head);
        let zero = build.iconst(Type::int(32), 0);
        let test = build.icmp(IntPred::Slt, outside, zero);
        build.br_if(test, arms[0], &[], arms[1], &[]);
        let mut build = Builder::new(&mut func, arms[0]);
        store_something(&mut build);
        let it = build.iconst(Type::int(32), 1);
        build.jump(join, &[it]);
        let mut build = Builder::new(&mut func, arms[1]);
        let it = build.iconst(Type::int(32), 2);
        build.jump(join, &[it]);
        let mut build = Builder::new(&mut func, join);
        build.ret(&[param]);

        let stats = phiopt(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::CONVERTED), 0);
        assert_eq!(stats.count(Kind::Missed, super::STORE_ON_ONE_PATH), 1);
        assert_eq!(goes_to(&func, 0), vec![1, 2]);
    }

    /// A load, which is the effect that is not a store and gets the general answer.
    #[test]
    fn an_arm_that_does_something_else_keeps_its_branch() {
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::int(32)]);
        let mut func = Func::new(names.intern("f"), signature);
        let head = func.create_block();
        let outside = func.append_param(head, Type::int(32));
        let arms = [func.create_block(), func.create_block()];
        let join = func.create_block();
        let param = func.append_param(join, Type::int(32));

        let mut build = Builder::new(&mut func, head);
        let zero = build.iconst(Type::int(32), 0);
        let test = build.icmp(IntPred::Slt, outside, zero);
        build.br_if(test, arms[0], &[], arms[1], &[]);
        let mut build = Builder::new(&mut func, arms[0]);
        let address = build.iconst(Type::int(64), 16);
        let address = build.unary(Opcode::IntToPtr, address, Type::PTR);
        let it = build.load(Type::int(32), address, plain(), Flags::NONE);
        build.jump(join, &[it]);
        let mut build = Builder::new(&mut func, arms[1]);
        let it = build.iconst(Type::int(32), 2);
        build.jump(join, &[it]);
        let mut build = Builder::new(&mut func, join);
        build.ret(&[param]);

        let stats = phiopt(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::CONVERTED), 0);
        assert_eq!(stats.count(Kind::Missed, super::ARM_HAS_EFFECTS), 1);
        assert_eq!(goes_to(&func, 0), vec![1, 2]);
    }

    /// A division whose divisor is not known cannot be moved onto the path that skipped it.
    #[test]
    fn an_arm_that_divides_by_something_unknown_keeps_its_branch() {
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::int(32), Type::int(32)]);
        let mut func = Func::new(names.intern("f"), signature);
        let head = func.create_block();
        let left = func.append_param(head, Type::int(32));
        let right = func.append_param(head, Type::int(32));
        let arms = [func.create_block(), func.create_block()];
        let join = func.create_block();
        let param = func.append_param(join, Type::int(32));

        let mut build = Builder::new(&mut func, head);
        let zero = build.iconst(Type::int(32), 0);
        let test = build.icmp(IntPred::Ne, right, zero);
        build.br_if(test, arms[0], &[], arms[1], &[]);
        let mut build = Builder::new(&mut func, arms[0]);
        let it = build.binary(Opcode::SDiv, left, right, Flags::NONE);
        build.jump(join, &[it]);
        let mut build = Builder::new(&mut func, arms[1]);
        let it = build.iconst(Type::int(32), 0);
        build.jump(join, &[it]);
        let mut build = Builder::new(&mut func, join);
        build.ret(&[param]);

        let stats = phiopt(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::CONVERTED), 0);
        assert_eq!(stats.count(Kind::Missed, super::ARM_MAY_TRAP), 1);
        assert_eq!(goes_to(&func, 0), vec![1, 2]);
    }

    #[test]
    fn a_division_by_a_constant_that_is_not_zero_or_minus_one_is_moved() {
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::int(32)]);
        let mut func = Func::new(names.intern("f"), signature);
        let head = func.create_block();
        let outside = func.append_param(head, Type::int(32));
        let arms = [func.create_block(), func.create_block()];
        let join = func.create_block();
        let param = func.append_param(join, Type::int(32));

        let mut build = Builder::new(&mut func, head);
        let zero = build.iconst(Type::int(32), 0);
        let test = build.icmp(IntPred::Slt, outside, zero);
        build.br_if(test, arms[0], &[], arms[1], &[]);
        let mut build = Builder::new(&mut func, arms[0]);
        let three = build.iconst(Type::int(32), 3);
        let it = build.binary(Opcode::SDiv, outside, three, Flags::NONE);
        build.jump(join, &[it]);
        let mut build = Builder::new(&mut func, arms[1]);
        let it = build.iconst(Type::int(32), 0);
        build.jump(join, &[it]);
        let mut build = Builder::new(&mut func, join);
        build.ret(&[param]);

        let stats = phiopt(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::CONVERTED), 1);
        assert!(opcodes(&func, 0).contains(&Opcode::SDiv));
    }

    /// Nothing chooses between two pointers, so the shape is matched and then left alone.
    #[test]
    fn a_value_no_select_is_lowered_for_keeps_its_branch() {
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::int(32)]);
        let mut func = Func::new(names.intern("f"), signature);
        let head = func.create_block();
        let outside = func.append_param(head, Type::int(32));
        let arms = [func.create_block(), func.create_block()];
        let join = func.create_block();
        func.append_param(join, Type::PTR);

        let mut build = Builder::new(&mut func, head);
        let zero = build.iconst(Type::int(32), 0);
        let test = build.icmp(IntPred::Slt, outside, zero);
        build.br_if(test, arms[0], &[], arms[1], &[]);
        for (arm, value) in arms.iter().zip([16, 32]) {
            let mut build = Builder::new(&mut func, *arm);
            let it = build.iconst(Type::int(64), value);
            let it = build.unary(Opcode::IntToPtr, it, Type::PTR);
            build.jump(join, &[it]);
        }
        let mut build = Builder::new(&mut func, join);
        build.ret(&[]);

        let stats = phiopt(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::CONVERTED), 0);
        assert_eq!(stats.count(Kind::Missed, super::NO_SELECT_AT_THAT_WIDTH), 1);
    }

    #[test]
    fn arms_with_more_work_in_them_than_the_budget_keep_their_branch() {
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::int(32)]);
        let mut func = Func::new(names.intern("f"), signature);
        let head = func.create_block();
        let outside = func.append_param(head, Type::int(32));
        let arms = [func.create_block(), func.create_block()];
        let join = func.create_block();
        let param = func.append_param(join, Type::int(32));

        let mut build = Builder::new(&mut func, head);
        let zero = build.iconst(Type::int(32), 0);
        let test = build.icmp(IntPred::Slt, outside, zero);
        build.br_if(test, arms[0], &[], arms[1], &[]);
        let mut build = Builder::new(&mut func, arms[0]);
        // Four instructions, which is past the budget however cheap each of them is.
        let mut it = outside;
        for _ in 0..4 {
            it = build.binary(Opcode::Add, it, outside, Flags::NONE);
        }
        build.jump(join, &[it]);
        let mut build = Builder::new(&mut func, arms[1]);
        let it = build.iconst(Type::int(32), 0);
        build.jump(join, &[it]);
        let mut build = Builder::new(&mut func, join);
        build.ret(&[param]);

        let stats = phiopt(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::CONVERTED), 0);
        assert_eq!(stats.count(Kind::Missed, super::ARMS_TOO_LONG), 1);
    }

    /// The margin, at the two ends of it and just outside.
    ///
    /// A pass level test of the refusal it guards is not written, and the module doc says why: a
    /// diamond is the one shape none of document 11's one sided predictors can key on, so every
    /// branch this pass matches comes back even until `__builtin_expect` is wired through the
    /// front end. The arithmetic is what there is to check today.
    #[test]
    fn the_margin_is_a_quarter_in_from_each_end() {
        let guessed = |percent: u32| Probability::percent(percent, Quality::Guessed);
        assert!(super::unpredictable(Probability::even()));
        assert!(super::unpredictable(guessed(25)));
        assert!(super::unpredictable(guessed(75)));
        assert!(!super::unpredictable(guessed(24)));
        assert!(!super::unpredictable(guessed(76)));
        assert!(!super::unpredictable(Probability::always()));
        assert!(!super::unpredictable(Probability::never()));
    }

    #[test]
    fn an_arm_that_two_edges_reach_is_not_an_arm() {
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::int(32)]);
        let mut func = Func::new(names.intern("f"), signature);
        let head = func.create_block();
        let outside = func.append_param(head, Type::int(32));
        let above = func.create_block();
        let arms = [func.create_block(), func.create_block()];
        let join = func.create_block();
        let param = func.append_param(join, Type::int(32));

        // The entry reaches the first arm as well as the head does, so moving the arm's work into
        // the head would leave the entry's path without it.
        let mut build = Builder::new(&mut func, head);
        let zero = build.iconst(Type::int(32), 0);
        let first = build.icmp(IntPred::Slt, outside, zero);
        build.br_if(first, above, &[], arms[0], &[]);
        let mut build = Builder::new(&mut func, above);
        let one = build.iconst(Type::int(32), 1);
        let second = build.icmp(IntPred::Slt, outside, one);
        build.br_if(second, arms[0], &[], arms[1], &[]);
        for (arm, value) in arms.iter().zip([1, 2]) {
            let mut build = Builder::new(&mut func, *arm);
            let it = build.iconst(Type::int(32), value);
            build.jump(join, &[it]);
        }
        let mut build = Builder::new(&mut func, join);
        build.ret(&[param]);

        let stats = phiopt(&mut func);
        // Neither branch is a diamond. The head's first side goes to a block that is not the join
        // and is not an arm either, since two edges reach it, and the second branch's first side
        // is the same block for the same reason.
        assert_eq!(stats.count(Kind::Optimized, super::CONVERTED), 0);
        assert_eq!(goes_to(&func, 0), vec![1, 2]);
        assert_eq!(goes_to(&func, 1), vec![2, 3]);
    }

    #[test]
    fn fuel_stops_the_conversion_where_it_stands() {
        let mut func = empty_arms();
        let mut fuel = Fuel::of(0);
        let stats = PhiOpt.run(&mut func, &mut Analyses::new(), &mut fuel);
        assert_eq!(stats.count(Kind::Optimized, super::CONVERTED), 0);
        assert_eq!(stats.count(Kind::Missed, super::NO_FUEL), 1);
        assert_eq!(goes_to(&func, 0), vec![1, 2]);
    }

    /// `x < y ? f(a, k) : f(b, k)`, as a diamond whose two arms do the same thing to different
    /// operands.
    ///
    /// Block 0 is the head, taking the two values it compares and the two operands and working out
    /// the operand both arms share. Blocks 1 and 2 are the arms, each applying every one of
    /// `steps` to its own operand and that shared value, and block 3 is the join, taking one
    /// parameter for each of them.
    fn same_operation(steps: &[Opcode]) -> Func {
        let mut names = Interner::new();
        let int = Type::int(32);
        let signature = Signature::new().with_params(&[int, int, int, int]);
        let mut func = Func::new(names.intern("f"), signature);
        let head = func.create_block();
        let left = func.append_param(head, int);
        let right = func.append_param(head, int);
        let operands = [func.append_param(head, int), func.append_param(head, int)];
        let arms = [func.create_block(), func.create_block()];
        let join = func.create_block();
        let params: Vec<Value> = steps.iter().map(|_| func.append_param(join, int)).collect();

        let mut build = Builder::new(&mut func, head);
        // In the head rather than in each arm, so that the two sides share this operand as one
        // value. Two arms that each work out their own three are two operations apart, not one.
        let shared = build.iconst(int, 3);
        let test = build.icmp(IntPred::Slt, left, right);
        build.br_if(test, arms[0], &[], arms[1], &[]);
        for (&arm, operand) in arms.iter().zip(operands) {
            let mut build = Builder::new(&mut func, arm);
            let carried: Vec<Value> = steps
                .iter()
                .map(|&opcode| build.binary(opcode, operand, shared, Flags::default()))
                .collect();
            build.jump(join, &carried);
        }
        let mut build = Builder::new(&mut func, join);
        build.ret(&params);
        func
    }

    /// The transformation. Two adds become one add of a select, rather than one select of two adds.
    #[test]
    fn an_operation_both_arms_did_is_done_once_below_the_branch() {
        let mut func = same_operation(&[Opcode::Add]);
        let stats = phiopt(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::FACTORED), 1);
        assert_eq!(stats.count(Kind::Optimized, super::CONVERTED), 1);
        assert_eq!(
            opcodes(&func, 0),
            vec![Opcode::IConst, Opcode::ICmp, Opcode::Select, Opcode::Add, Opcode::Jump],
            "the select chooses the operand and the add happens once"
        );
        assert_eq!(blocks(&func), vec![0, 3]);
    }

    /// The select goes under the operation, so what it chooses between is the operands and not the
    /// answers. Getting that the wrong way round would be a select of two adds that happens to have
    /// the right opcodes in it.
    #[test]
    fn the_select_chooses_the_operands_and_not_the_answers() {
        let mut func = same_operation(&[Opcode::Add]);
        phiopt(&mut func);
        let head = Block::from_usize(0);
        let select = func
            .insts(head)
            .find(|&inst| func[inst].opcode == Opcode::Select)
            .expect("the select the pass just built");
        let add = func
            .insts(head)
            .find(|&inst| func[inst].opcode == Opcode::Add)
            .expect("the add the pass just wrote");
        let chosen = func[func[select].args].to_vec();
        let params = func[head].params.to_vec();
        assert_eq!(&chosen[1..], &params[2..], "the two operands the arms differed in");
        let added = func[func[add].args].to_vec();
        assert_eq!(added[0], func[select].first_result.expect("a select has a result"));
        assert_eq!(carries(&func, 0), vec![func[add].first_result.expect("an add has a result")]);
    }

    /// Nothing is speculated by an operation both arms were doing, so the length rule is about what
    /// is left after the factoring rather than about what the arms arrived holding. Three
    /// instructions an arm is over the limit, and three instructions that all factor is none.
    #[test]
    fn arms_that_factor_away_entirely_are_not_too_long() {
        let steps = [Opcode::Add, Opcode::Sub, Opcode::Mul];
        let mut func = same_operation(&steps);
        let stats = phiopt(&mut func);
        assert_eq!(stats.count(Kind::Missed, super::ARMS_TOO_LONG), 0);
        assert_eq!(stats.count(Kind::Optimized, super::FACTORED), 3);
        assert_eq!(stats.count(Kind::Optimized, super::CONVERTED), 1);
        let written = opcodes(&func, 0);
        assert_eq!(written.iter().filter(|&&op| op == Opcode::Select).count(), 3);
        for step in steps {
            assert_eq!(written.iter().filter(|&&op| op == step).count(), 1, "{step:?} once");
        }
    }

    /// Both arms doing the same thing to the same operands is a common subexpression nothing has
    /// numbered, and one copy of it serves both sides with no select at all.
    #[test]
    fn arms_that_agree_in_every_operand_need_no_select() {
        let mut names = Interner::new();
        let int = Type::int(32);
        let signature = Signature::new().with_params(&[int, int, int]);
        let mut func = Func::new(names.intern("f"), signature);
        let head = func.create_block();
        let left = func.append_param(head, int);
        let right = func.append_param(head, int);
        let operand = func.append_param(head, int);
        let arms = [func.create_block(), func.create_block()];
        let join = func.create_block();
        let param = func.append_param(join, int);

        let mut build = Builder::new(&mut func, head);
        let shared = build.iconst(int, 3);
        let test = build.icmp(IntPred::Slt, left, right);
        build.br_if(test, arms[0], &[], arms[1], &[]);
        for &arm in &arms {
            let mut build = Builder::new(&mut func, arm);
            let it = build.binary(Opcode::Add, operand, shared, Flags::default());
            build.jump(join, &[it]);
        }
        let mut build = Builder::new(&mut func, join);
        build.ret(&[param]);

        let stats = phiopt(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::FACTORED), 1);
        assert_eq!(
            opcodes(&func, 0),
            vec![Opcode::IConst, Opcode::ICmp, Opcode::Add, Opcode::Jump],
            "one add and nothing to choose between"
        );
    }

    /// Two different operations are two operations, and the pass falls back to hoisting both and
    /// selecting between what they produced.
    #[test]
    fn arms_that_do_different_things_are_not_factored() {
        let mut names = Interner::new();
        let int = Type::int(32);
        let signature = Signature::new().with_params(&[int, int, int, int]);
        let mut func = Func::new(names.intern("f"), signature);
        let head = func.create_block();
        let left = func.append_param(head, int);
        let right = func.append_param(head, int);
        let operands = [func.append_param(head, int), func.append_param(head, int)];
        let arms = [func.create_block(), func.create_block()];
        let join = func.create_block();
        let param = func.append_param(join, int);

        let mut build = Builder::new(&mut func, head);
        let shared = build.iconst(int, 3);
        let test = build.icmp(IntPred::Slt, left, right);
        build.br_if(test, arms[0], &[], arms[1], &[]);
        for ((&arm, operand), opcode) in arms.iter().zip(operands).zip([Opcode::Add, Opcode::Sub]) {
            let mut build = Builder::new(&mut func, arm);
            let it = build.binary(opcode, operand, shared, Flags::default());
            build.jump(join, &[it]);
        }
        let mut build = Builder::new(&mut func, join);
        build.ret(&[param]);

        let stats = phiopt(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::FACTORED), 0);
        assert_eq!(stats.count(Kind::Optimized, super::CONVERTED), 1);
        assert_eq!(
            opcodes(&func, 0),
            vec![
                Opcode::IConst,
                Opcode::ICmp,
                Opcode::Add,
                Opcode::Sub,
                Opcode::Select,
                Opcode::Jump
            ],
            "both operations hoisted and a select between their answers"
        );
    }

    /// Two operand positions apart needs two selects and one operation, which is what one select
    /// and two operations already cost, so there is nothing to win and it is left alone.
    #[test]
    fn arms_that_differ_in_two_operands_are_not_factored() {
        let mut names = Interner::new();
        let int = Type::int(32);
        let signature = Signature::new().with_params(&[int, int, int, int, int, int]);
        let mut func = Func::new(names.intern("f"), signature);
        let head = func.create_block();
        let left = func.append_param(head, int);
        let right = func.append_param(head, int);
        let first = [func.append_param(head, int), func.append_param(head, int)];
        let second = [func.append_param(head, int), func.append_param(head, int)];
        let arms = [func.create_block(), func.create_block()];
        let join = func.create_block();
        let param = func.append_param(join, int);

        let mut build = Builder::new(&mut func, head);
        let test = build.icmp(IntPred::Slt, left, right);
        build.br_if(test, arms[0], &[], arms[1], &[]);
        for ((&arm, one), two) in arms.iter().zip(first).zip(second) {
            let mut build = Builder::new(&mut func, arm);
            let it = build.binary(Opcode::Add, one, two, Flags::default());
            build.jump(join, &[it]);
        }
        let mut build = Builder::new(&mut func, join);
        build.ret(&[param]);

        let stats = phiopt(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::FACTORED), 0);
        assert_eq!(stats.count(Kind::Optimized, super::CONVERTED), 1);
        assert_eq!(opcodes(&func, 0).iter().filter(|&&op| op == Opcode::Add).count(), 2);
    }

    /// The one copy is written after the arms have gone, so an operation something else in the arm
    /// reads cannot be one of the two it replaces. Here each arm hands its answer to the join
    /// twice, which is two readers and not one.
    #[test]
    fn an_operation_read_more_than_once_is_not_factored() {
        let mut names = Interner::new();
        let int = Type::int(32);
        let signature = Signature::new().with_params(&[int, int, int, int]);
        let mut func = Func::new(names.intern("f"), signature);
        let head = func.create_block();
        let left = func.append_param(head, int);
        let right = func.append_param(head, int);
        let operands = [func.append_param(head, int), func.append_param(head, int)];
        let arms = [func.create_block(), func.create_block()];
        let join = func.create_block();
        let params = [func.append_param(join, int), func.append_param(join, int)];

        let mut build = Builder::new(&mut func, head);
        let shared = build.iconst(int, 3);
        let test = build.icmp(IntPred::Slt, left, right);
        build.br_if(test, arms[0], &[], arms[1], &[]);
        for (&arm, operand) in arms.iter().zip(operands) {
            let mut build = Builder::new(&mut func, arm);
            let it = build.binary(Opcode::Add, operand, shared, Flags::default());
            build.jump(join, &[it, it]);
        }
        let mut build = Builder::new(&mut func, join);
        build.ret(&params);

        let stats = phiopt(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::FACTORED), 0);
        assert_eq!(stats.count(Kind::Optimized, super::CONVERTED), 1);
        assert_eq!(opcodes(&func, 0).iter().filter(|&&op| op == Opcode::Add).count(), 2);
    }

    /// A triangle has a block on one side only, so there is no second operation to pair the first
    /// one with and nothing to factor.
    #[test]
    fn a_triangle_factors_nothing() {
        let mut names = Interner::new();
        let int = Type::int(32);
        let signature = Signature::new().with_params(&[int, int, int]);
        let mut func = Func::new(names.intern("f"), signature);
        let head = func.create_block();
        let left = func.append_param(head, int);
        let right = func.append_param(head, int);
        let operand = func.append_param(head, int);
        let arm = func.create_block();
        let join = func.create_block();
        let param = func.append_param(join, int);

        let mut build = Builder::new(&mut func, head);
        let shared = build.iconst(int, 3);
        let test = build.icmp(IntPred::Slt, left, right);
        build.br_if(test, arm, &[], join, &[operand]);
        let mut build = Builder::new(&mut func, arm);
        let it = build.binary(Opcode::Add, operand, shared, Flags::default());
        build.jump(join, &[it]);
        let mut build = Builder::new(&mut func, join);
        build.ret(&[param]);

        let stats = phiopt(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::FACTORED), 0);
        assert_eq!(stats.count(Kind::Optimized, super::CONVERTED), 1);
    }
}
