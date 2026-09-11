//! Every threshold any pass consults, with where it came from.
//!
//! Section 40.12 calls this "the deliverable this document exists to produce" and states the rule
//! that goes with it: "A pass may not contain a bare numeric threshold; the coding standard test
//! greps for one." So a pass that wants to know how many moves a block copy may expand to reads
//! [`BLOCK_COPY_MOVES_FOR_SPEED`], and the number 8 appears in the compiler exactly once, here,
//! next to the document that argued for it.
//!
//! The section names the file `crates/rucc-opt/src/costs.rs`. It is here instead, one crate lower,
//! because the back end has thresholds too and a constant the optimizer owns is a constant the
//! back end would copy. Nothing else about the arrangement changes.
//!
//! # Provenance is part of the constant
//!
//! Section 40.13 is blunt about why: "A constant marked chosen is a constant nobody should defend."
//! Every entry below carries a [`Provenance`], and the three that say [`Provenance::Awaiting`] are
//! the three the documents said would have to be measured and have not been. They are not hidden
//! behind a plausible number. A report can print them, and [`ALL`] exists so that it can.
//!
//! # What is not here
//!
//! Anything that varies by target. How many operations a machine issues at once, what a mispredict
//! costs it, and how narrow a store it is willing to do are facts about hardware and live in that
//! target's [`crate::CostTable`]. The line is whether a second target would want a different
//! number for a reason that is about the machine rather than about taste.

use crate::Cycles;

/// Where a constant came from, which is the first thing to ask about any of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Provenance {
    /// GCC uses this number and we adopted it on purpose after reading why.
    ///
    /// The strongest provenance on offer here. It is not a proof that the number is right, but it
    /// is decades of somebody else's regressions, which is more evidence than anything else in
    /// this file has.
    Gcc,

    /// Somebody picked it and it has not been measured.
    ///
    /// The weakest. A constant marked this way is a constant to attack first when a heuristic
    /// misbehaves, because nothing is defending it.
    Chosen,

    /// It follows from something else here, so changing it alone would be inconsistent.
    Derived,

    /// The document that needs it said it must be measured, and it has not been.
    ///
    /// The value present is a placeholder that keeps the compiler running. It is marked so that
    /// the report says so out loud rather than presenting it alongside numbers that were earned.
    Awaiting,
}

impl Provenance {
    /// How the provenance reads in a report.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Gcc => "adopted from gcc",
            Self::Chosen => "chosen, not measured",
            Self::Derived => "derived from another constant here",
            Self::Awaiting => "placeholder, awaiting measurement",
        }
    }

    /// Whether this constant is standing on evidence.
    #[must_use]
    pub const fn is_evidence(self) -> bool {
        matches!(self, Self::Gcc)
    }
}

impl std::fmt::Display for Provenance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One row of section 40.12's table, for a report that wants to print the lot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Constant {
    /// The name of the item in this module.
    pub name: &'static str,
    /// Its value, in whatever `unit` says.
    pub value: i64,
    /// What the value counts.
    pub unit: &'static str,
    /// The section of `spec/optimizer/` that argued for it.
    pub document: &'static str,
    /// The GCC parameter or macro this corresponds to, empty when there is none.
    pub gcc: &'static str,
    /// How much to trust it.
    pub provenance: Provenance,
}

/// What a branch costs when optimizing for size, per section 40.5.
///
/// GCC's `BRANCH_COST` at `gcc/config/i386/i386.h:2023` answers 2 for every x86 target when
/// `optimize_size`, and does so without consulting the cost table, which is the same shape as this.
/// Two, because a conditional branch is the compare and the jump, and the thing being priced is
/// whether removing the branch is worth the instructions that replace it.
pub const BRANCH_COST_FOR_SIZE: Cycles = Cycles::insns(2);

/// What a well predicted branch costs when optimizing for speed, per section 40.5.
///
/// Zero, and it is worth being clear that this is a claim about the hardware and not a rounding.
/// A correctly predicted branch on an out of order machine costs no cycles at all: the front end
/// fetched past it before it issued. Charging anything for it is what makes a pass if-convert a
/// branch that was already free, turning a zero cost branch into two real operations.
pub const BRANCH_COST_PREDICTABLE: Cycles = Cycles::ZERO;

/// How far a branch's probability has to be from even before it counts as predictable, per section
/// 40.5, as a percentage.
///
/// Two percent, so a branch taken 99 times in 100 is predictable and one taken 90 times in 100 is
/// not. It is a hard threshold and section 40.5 pairs it with a rule that matters more than the
/// number: a probability that came from a static heuristic never counts as predictable whatever it
/// says, because the heuristic guessed.
pub const PREDICTABLE_BRANCH_PERCENT: u32 = 2;

/// How many instructions if-conversion may add to remove a predictable branch, per section 40.5.
///
/// Small, because there is nothing to win. The branch was free, so the budget only covers the case
/// where removing it happens to shorten the code anyway.
pub const IF_CONVERSION_BUDGET_PREDICTABLE: u32 = 20;

/// The same for an unpredictable branch, per section 40.5.
///
/// Twice the predictable budget, because an unpredictable branch is paying a mispredict penalty
/// some fraction of the time and there is real money to win.
pub const IF_CONVERSION_BUDGET_UNPREDICTABLE: u32 = 40;

/// The largest block if-conversion will consider, in instructions, per section 40.5.
///
/// A separate limit from the budget, and needed separately: a budget is about what the
/// transformation costs and this is about the compile time of looking. A 400 instruction block
/// will not be if-converted whatever the budget says, and finding that out cheaply is the point.
pub const IF_CONVERSION_BLOCK_LIMIT: u32 = 10;

/// How many instructions an arm of a diamond may hold and still be if-converted at the IR level,
/// per section 22.2.
///
/// Two, and much smaller than the budgets above because it is being spent differently. Those price
/// the whole conversion at the machine level, where the arms are already predicated and the
/// question is the total. This is per arm and the work it counts is work that will be done on both
/// paths afterwards, so the number is the answer to how much speculation is worth one branch. An
/// arm of two cheap operations is a value being worked out. An arm of ten is a program.
pub const PHIOPT_ARM_INSTRUCTIONS: u32 = 2;

/// How near even a branch's probability has to be before if-conversion will speculate an arm into
/// it, per section 22.2, as a percentage.
///
/// Twenty five, so the window is a quarter to three quarters. It is the mirror of
/// [`PREDICTABLE_BRANCH_PERCENT`] and deliberately not the same number, because the two questions
/// are not the same question. That one asks whether a branch is so one sided that the machine will
/// never miss it, and the answer has to be extreme before it is worth believing. This one asks
/// whether there is enough doubt to pay for doing both arms, and paying for that on a branch the
/// estimate already leans on is how if-conversion loses time.
pub const PHIOPT_UNPREDICTABLE_MARGIN_PERCENT: u32 = 25;

/// How much work the right operand of a `&&` or a `||` may be and still be folded into one branch,
/// per section 22.5.
///
/// Three, and it counts instructions in the IR rather than on the machine, which is why it is one
/// more than [`PHIOPT_ARM_INSTRUCTIONS`] rather than the same number. The shape this is for is a
/// comparison against a constant, and that is two instructions here and one on the machine, because
/// the constant becomes the comparison's immediate and stops being anything at all. Three leaves
/// room for one operation under the comparison. A right operand of four or more is a computation
/// rather than a test, and speculating a computation to save one branch is a trade in the wrong
/// direction.
pub const SHORT_CIRCUIT_INSTRUCTIONS: u32 = 3;

/// How many scalar moves a block copy may expand to when optimizing for speed, per section 40.7.
///
/// GCC's `MOVE_RATIO`. Above this the copy becomes a call to `memcpy`, which is written in assembly
/// by somebody who measured, and below it the inline expansion wins because it avoids the call and
/// lets the surrounding code see through it.
pub const BLOCK_COPY_MOVES_FOR_SPEED: u32 = 8;

/// The same when optimizing for size, per section 40.7.
///
/// Half, because a call is five bytes and eight moves are not.
pub const BLOCK_COPY_MOVES_FOR_SIZE: u32 = 4;

/// How many instructions a loop header may hold and still be worth copying, per section 26.6.
///
/// GCC's `max-loop-header-insns`, `Init(20)` at `gcc/params.opt:690`. The copy is what turns a
/// `while` into a `do-while`, and what it costs is the header again in front of the loop, so the
/// number is how much code a program will pay once per loop for a body that is one region and a
/// test at the bottom.
pub const LOOP_HEADER_INSNS_FOR_SPEED: u32 = 20;

/// The same when optimizing for size, per section 26.6.
///
/// Five, which is what that section asks for in words rather than a number GCC has: "it runs with
/// a much smaller limit, on the order of five instructions". The do-while form is slightly smaller
/// in the steady state, so the copy is worth making at a size where the header is a test and
/// little else, and not at one that writes twenty instructions out twice.
pub const LOOP_HEADER_INSNS_FOR_SIZE: u32 = 5;

/// How wide a reassociation tree is on a target nobody has tuned, per section 40.8.
///
/// One, which means no reassociation. A chain of adds becomes a tree only to use execution units
/// that are otherwise idle, so a machine whose parallelism nobody has stated gets the chain, which
/// is correct and no slower than what the program said.
pub const REASSOC_WIDTH_UNTUNED: u32 = 1;

/// The largest frequency ratio the inliner will believe without a profile, per section 40.11.
///
/// A hundred. Static frequencies come from guesses that multiply through a loop nest, so a call
/// three loops deep can come out ten thousand times hotter than the entry block on nothing but the
/// assumption that a loop runs some number of times. The clamp says that a guess, however deeply
/// nested, is worth at most two orders of magnitude.
pub const INLINE_FREQUENCY_CLAMP: u32 = 100;

/// Where the inliner starts squaring the growth term, per section 40.11.
///
/// GCC's `overall_growth` bound. Below it growth is linear in the badness formula and above it the
/// term is squared, which turns a gradual disincentive into a wall. It exists because inlining a
/// large function into many callers is the one thing that reliably makes a compilation never
/// finish.
pub const INLINE_GROWTH_SQUARING_BOUND: u32 = 256;

/// How cold a block may be and still count as hot in its own function, as a fraction of the entry
/// block, per section 11.4.
///
/// GCC's `param_hot_bb_frequency_fraction`. One part in a thousand, which is a low bar on purpose:
/// the question it answers is whether a block is worth spending compile time and code size on, and
/// a block that runs once per thousand calls is still a block on somebody's path. The bar for
/// being the hot path is a different and much higher one, and it is asked as a comparison against
/// the other blocks rather than against this.
pub const HOT_BLOCK_FRACTION: u32 = 1000;

/// How often the arm a `__builtin_expect` names is the one taken, in percent, per section 11.2.
///
/// GCC's `param_builtin_expect_probability`. Ninety rather than a hundred because the hint is a
/// statement about the common case and not a promise, and a hint treated as a promise turns the
/// other arm into dead code that still has to run correctly.
pub const PREDICT_EXPECT: u32 = 90;

/// How often the arm that does not come back is the one not taken, in percent, per section 11.2.
///
/// GCC's `PRED_NORETURN`, whose hit rate is `PROB_VERY_LIKELY`, rounded to the percent this table
/// works in. This is the predictor that makes error handling cold, and error handling is most of
/// what a C program branches on: `if (x) { report(); abort(); }` is the shape, and without this
/// predictor the reporting path is as hot as the work.
pub const PREDICT_NEVER_RETURNS: u32 = 99;

/// How often the arm that calls a `cold` function is the one not taken, in percent, per section
/// 11.2.
///
/// GCC's `PRED_COLD_FUNCTION`, again `PROB_VERY_LIKELY`. Section 11.2 says the `cold` and `hot`
/// attributes are the user's explicit statement and must be honoured absolutely rather than
/// blended, so this sits with the strongest numbers here rather than with the guesses.
pub const PREDICT_COLD_CALL: u32 = 99;

/// How often a loop exit is the edge not taken, in percent, per section 11.2.
///
/// GCC's `PRED_LOOP_EXIT`. A loop that is worth writing runs more than once, and this number is
/// what says so: eighty nine percent of the time the iteration that reaches the test is not the
/// last one. Ball and Larus measured it and it has held up because it is a fact about how people
/// write programs rather than about any machine.
pub const PREDICT_LOOP_EXIT_NOT_TAKEN: u32 = 89;

/// How often the branch that guards a loop enters it, in percent, per section 11.2.
///
/// GCC's `PRED_LOOP_GUARD`. Weaker than the exit predictor, because a guard is written by somebody
/// who thought the loop might not run at all, and lower than it for the same reason.
pub const PREDICT_LOOP_GUARD_TAKEN: u32 = 73;

/// How often a pointer compared against null is not null, in percent, per section 11.2.
///
/// GCC's `PRED_POINTER`. A pointer that is tested is usually a pointer that is about to be used,
/// and the test is there for the case that does not happen.
pub const PREDICT_POINTER_NOT_NULL: u32 = 70;

/// How often the arm that returns a negative constant is the one not taken, in percent, per
/// section 11.2.
///
/// GCC's `PRED_NEGATIVE_RETURN`. A negative return value in C means the call failed, which is the
/// strongest of the return value predictors because nothing else returns one on purpose.
pub const PREDICT_NEGATIVE_RETURN: u32 = 98;

/// How often the arm that returns a null pointer is the one not taken, in percent, per section
/// 11.2.
///
/// GCC's `PRED_NULL_RETURN`. The same idea as the negative return and a much weaker number,
/// because a null pointer is also an ordinary answer: the end of a list is not a failure.
pub const PREDICT_NULL_RETURN: u32 = 71;

/// How often the arm containing a call is the one not taken, in percent, per section 11.2.
///
/// GCC's `PRED_CALL`. The weakest predictor kept, and it is here because it is the one that fires
/// on code no other predictor recognises. Section 11.2 drops everything below sixty five percent,
/// on the grounds that a predictor at fifty nine moves a probability by nine points and no
/// decision downstream of it changes.
pub const PREDICT_CALL_NOT_TAKEN: u32 = 67;

/// How often a `continue` is the edge taken, in percent, per section 11.2.
///
/// GCC's `PRED_CONTINUE`. A jump back to the top of the loop from inside the body, which is a loop
/// that goes round again without reaching the bottom.
pub const PREDICT_CONTINUE_TAKEN: u32 = 67;

/// How many iterations a loop is predicted to run when nothing measured it, per section 11.2.
///
/// GCC's `max-predicted-iterations`. It is a cap and not an estimate: the frequency of a loop
/// header is its entry frequency divided by the chance of leaving, and a loop whose exit no
/// predictor recognised has no chance of leaving at all, so without a cap the division is by zero
/// and with a small one it is by nearly zero. Section 11.6 names the overflow that follows as one
/// of the two most common ways a frequency implementation breaks, the other being a float where a
/// scaled integer belongs.
pub const MAX_PREDICTED_ITERATIONS: u32 = 100;

/// How far a block's frequency may sit from the sum of the frequencies arriving at it before the
/// check in section 11.5 complains, in percent.
///
/// One percent. The sum is exact in real arithmetic, including at a loop header, where the entry
/// and the back edge add up to the header's own frequency precisely because the geometric series
/// says they do. What it is not exact in is fixed point: every edge divides by ten thousand and
/// throws the remainder away. So the check needs a tolerance, and the tolerance has to be small
/// enough that a pass which forgot a block is still caught, which a percent is.
pub const PROFILE_SUM_TOLERANCE_PERCENT: u32 = 1;

/// How many registers of each class loop invariant motion leaves unused, per section 40.6.
///
/// Two, which is GCC's `ira-loop-reserved-regs` at `gcc/params.opt:336`, "The number of registers
/// in each class kept unused by loop invariant motion". A hoist that takes the pressure inside a
/// loop up to the allocatable count has not saved anything: the value it hoisted is now live
/// across the whole loop and something else has to be spilled to make room for it, and the spill
/// is inside the loop while the computation it replaced might not have been. The margin is what
/// stops the pass from walking up to the edge and stepping off it. It is the allocator's own
/// parameter, which is the right place for it to come from, since the allocator is what pays.
pub const LOOP_RESERVED_REGS: u32 = 2;

/// What a computation has to cost before it is worth hoisting out of a loop under pressure, per
/// section 27.2.
///
/// GCC's `lim-expensive`, `Init(20)` at `gcc/params.opt:428`. It is not the price of an operation,
/// it is the line between an operation worth a register held across the whole loop and one that is
/// cheaper to do again. Below the line a hoist under pressure buys a computation per iteration and
/// pays for it with a spill and a reload per iteration, which is the classic way loop invariant
/// motion measures worse than not running it at all.
pub const LICM_EXPENSIVE: u32 = 20;

/// How many times a loop may run and still be unrolled away entirely, per section 29.2.
///
/// GCC's `max-completely-peel-times`, `Init(16)` at `gcc/params.opt:549`. It is a bound on the
/// copies rather than on the code, which the instruction limit below is, and both are needed: a
/// two instruction body run four hundred times is under nobody's idea of a small loop and would
/// pass an instruction limit alone if the body were small enough.
pub const UNROLL_MAX_TIMES: u32 = 16;

/// How large the unrolled form may be, in instructions, per section 29.2.
///
/// GCC's `max-completely-peeled-insns`, `Init(200)` at `gcc/params.opt:553`. What is counted is
/// the estimate of what survives the folding the unroll enables and not the body times the count,
/// which section 29.2 says is the whole difficulty of the cost model: the induction variable is a
/// constant in every copy, so the addressing folds, and a model that prices the output before that
/// happens refuses transformations that would have made the function smaller.
pub const UNROLL_MAX_INSNS: u32 = 200;

/// How large a loop may be, in instructions, and still be split into a checked half and an
/// unchecked one, per `spec/safe-memory/07-check-elimination.md` section 7.4.
///
/// GCC's `loop-versioning-max-inner-insns`, `Init(200)` at `gcc/params.opt:695`. That pass makes two
/// copies of a loop under a test decided when the loop starts, which is the same shape and the same
/// cost, so the limit it settled on is the one to take. What is counted here is the body as it
/// stands rather than an estimate of what survives, because unlike unrolling nothing folds
/// afterwards: both halves keep their branches and their counter, and the only difference between
/// them is which checks they hold.
pub const SPLIT_MAX_INSNS: u32 = 200;

/// How many instructions the guard a split puts in front of a loop may write again to work out where
/// a check's address is, per `spec/safe-memory/07-check-elimination.md` section 7.4.
///
/// No GCC parameter to take, because no GCC pass rematerializes an address to compare it against a
/// bound. What sets it is what it buys: every instruction counted here runs once per iteration in the
/// guard and what it removes is a call into the safety runtime, which is a call, a load of the
/// capability, a compare and a branch at the very least. Eight is loose against that and is there to
/// stop an address built out of half the loop body from having half the loop body copied into the
/// guard, which is a real shape in generated code and not a real opportunity.
///
/// Eight is where the curve flattens, measured on SQLite by the checks the census stops reporting as
/// unfollowed: 2194 at a cap of two, 2020 at four, 1956 at eight, and 1950 at both sixteen and
/// thirty two. So sixteen reaches six more checks than eight does and leaves the object byte for byte
/// the same size, which is not a reason to write twice as much arithmetic into a guard.
pub const SPLIT_REMADE_INSNS: usize = 8;

/// How deeply nested a loop may be and still be unrolled away entirely, per section 29.2.
///
/// GCC's `max-completely-peel-loop-nest-depth`, `Init(8)` at `gcc/params.opt:545`. The cost of
/// unrolling an inner loop is paid once per iteration of everything outside it, so the depth is
/// the multiplier on a limit that is otherwise about one loop in isolation.
pub const UNROLL_MAX_DEPTH: u32 = 8;

/// How far the return value predictors will walk to find the return they are predicting, in blocks,
/// per section 11.2.
///
/// Eight, and it is ours rather than GCC's. GCC propagates a return value prediction backwards over
/// every path that reaches the return, which needs the paths; this walks forward from the arm of
/// the branch over blocks that have one way out, which needs nothing and finds `if (bad) return
/// -1;` and the handful of statements somebody put in front of the return. A branch whose arm runs
/// eight straight blocks before returning is not the shape the predictor was measured on.
pub const PREDICT_RETURN_BLOCKS: u32 = 8;

/// How cold a block may be and still be worth aligning, as a fraction of the hottest block, per
/// section 38.5.
///
/// One hundredth. Alignment is padding, and padding a block nobody executes is bytes spent on
/// nothing plus an instruction cache line that could have held something.
pub const ALIGN_FREQUENCY_FRACTION: u32 = 100;

/// How many iterations a loop must be estimated to run before its head is worth aligning, per
/// section 38.5.
///
/// Four. Below that the padding is executed about as often as the loop body, so the alignment is
/// paying for itself out of its own pocket.
pub const LOOP_ALIGN_MIN_ITERATIONS: u32 = 4;

/// How many induction variable uses a loop may have before the choosing gives up on it, per
/// section 28.2.
///
/// GCC's `iv-max-considered-uses`, `Init(250)` at `gcc/params.opt:364`. The search is cubic in the
/// uses, so this is the number that stops compile time being cubic in something the program chose
/// rather than something the compiler did. Section 28.2's measurement over the corpus is that the
/// most any loop in it has is sixteen, so the bound is a guard against a generated program and not
/// a limit anything real runs into.
pub const IV_MAX_CONSIDERED_USES: usize = 250;

/// How many candidate variables a loop may have before the search stops looking at all of them
/// together, per section 28.2.
///
/// GCC's `iv-consider-all-candidates-bound`, `Init(40)` at `gcc/params.opt:360`. Above it GCC
/// considers only the candidates a use asked for rather than every candidate for every use, which
/// is the same cubic term seen from the other side. The corpus measurement is fifteen candidates
/// at most, so this does not bind either.
pub const IV_CONSIDER_ALL_CANDIDATES_BOUND: usize = 40;

/// How many candidates a loop may have before the ones nothing wants are dropped first, per
/// section 28.2.
///
/// GCC's `iv-always-prune-cand-set-bound`, `Init(10)` at `gcc/params.opt:356`. Below it a useless
/// candidate costs a column in a small search and is not worth the pass making a decision about.
/// Above it every candidate kept is paid for in the selection, which is why the pruning is
/// unconditional there.
pub const IV_ALWAYS_PRUNE_CAND_SET_BOUND: usize = 10;

/// How much a loop's own induction variables are preferred over made up ones, in increments, per
/// section 28.1.
///
/// Three increments, which is three of whatever the target's own table charges for the `add` a new
/// variable would be stepped by. It used to be three cycles, and the comment on it said that was
/// about one add, which it is not on any table in the tree. A made up variable costs an `add` a
/// turn and nothing else, so an `add` is the unit this preference is in whether or not it is
/// written that way.
///
/// Writing it that way changes no number today. x86-64 is the only target with a table, and both of
/// its goals put an `add` at one unit, the speed one because that is the latency and the size one
/// because `bytes(2)` is a cycle where the two meet. What it buys is that the constant still means
/// three increments on the next table anybody writes, rather than three of whatever that table
/// happens to call a unit.
///
/// GCC's `tree-ssa-loop-ivopts.cc` says in `determine_iv_cost` that "the original variables are
/// somewhat preferred" and prices that as a fifth of the candidate's own cost, which is a shape
/// rather than a number and does not transfer to a different cost model. What the bias is for is
/// real: a variable the loop already increments is one the rewrite does not have to introduce, one
/// the register allocator has already been living with, and one the programmer can still recognise
/// in a debugger.
///
/// Three is now measured rather than guessed, over the 1830 corpus programs that build at `-O2`,
/// one binary per value, reading `.text`. Zero costs 10068 bytes against three and one costs 8796,
/// so the whole of the interesting range is above one. The floor is flat from about five to twelve,
/// where three buys back another 300 bytes or so, and a bias of a thousand, which is the pass
/// switched off in all but name, is 15 bytes worse than three. Moving into that flat is a real
/// change and not this one: it costs the single access walk of a wide element its pointer, which is
/// a loop where the pointer is right on the instructions, so it is buying size on loops that should
/// not have had a pointer by taking it away from loops that should. tamnd/rucc#922 carries the
/// numbers.
pub const IVOPTS_NEW_VARIABLE_BIAS: u32 = 3;

/// What each induction variable past the register budget costs, in cycles, per section 28.2.
///
/// Ten, and it is not measured. A set larger than the registers left over spills, and a spill in a
/// loop is a store and a reload every iteration, which is the failure mode section 28.7 lists as
/// "a set that does not fit". The number has to be large enough that the selection would rather
/// recompute an address than carry one more variable, and an address recomputation is an add or
/// two, so ten leaves room. It is chosen from that relationship rather than from a measurement,
/// and the relationship is what a change to it has to keep.
pub const IVOPTS_SET_PENALTY: u32 = 10;

/// How many instructions the scheduler will keep in its ready list, per section 38.8.
///
/// A hundred, and it is a compile time bound rather than a code quality one. The list is scanned
/// once per instruction placed, so an unbounded list makes scheduling quadratic in the size of the
/// block, and a block with more than a hundred ready instructions has more choice than the
/// heuristic can tell apart anyway.
pub const SCHEDULER_READY_LIST_BOUND: usize = 100;

/// How many targets a switch needs before a jump table beats a chain of compares, per section 40.10.
///
/// Not measured. The right answer depends on what a mispredicted indirect branch costs against
/// what a run of well predicted compares costs, and both are machine numbers, so section 40.10
/// asks for a measurement and document 42 owes it. The value here keeps the lowering working and
/// should not be quoted at anybody.
pub const JUMP_TABLE_MIN_TARGETS: u32 = 8;

/// How much worse than the reference the allocator may do before it counts as a regression, as a
/// percentage, per section 39.4.
///
/// Not measured either, and for the same reason: the number that matters is how much spill code a
/// real program tolerates before it shows up in run time, and that is a corpus question.
pub const ALLOCATOR_DEGRADATION_PERCENT: u32 = 10;

/// Section 40.12's table, in its order, so a report can print it and a test can check it.
///
/// The point of having the list as data is that "which of our heuristics are guesses" becomes a
/// question with an answer, rather than a question that needs somebody to read every pass.
pub const ALL: &[Constant] = &[
    Constant {
        name: "BRANCH_COST_FOR_SIZE",
        value: 2,
        unit: "operations",
        document: "40.5",
        gcc: "BRANCH_COST when optimize_size",
        provenance: Provenance::Gcc,
    },
    Constant {
        name: "BRANCH_COST_PREDICTABLE",
        value: 0,
        unit: "operations",
        document: "40.5",
        gcc: "BRANCH_COST for a predictable branch",
        provenance: Provenance::Gcc,
    },
    Constant {
        name: "PREDICTABLE_BRANCH_PERCENT",
        value: 2,
        unit: "percent",
        document: "40.5",
        gcc: "PROB_VERY_LIKELY",
        provenance: Provenance::Gcc,
    },
    Constant {
        name: "IF_CONVERSION_BUDGET_PREDICTABLE",
        value: 20,
        unit: "instructions",
        document: "40.5",
        gcc: "",
        provenance: Provenance::Chosen,
    },
    Constant {
        name: "IF_CONVERSION_BUDGET_UNPREDICTABLE",
        value: 40,
        unit: "instructions",
        document: "40.5",
        gcc: "",
        provenance: Provenance::Chosen,
    },
    Constant {
        name: "IF_CONVERSION_BLOCK_LIMIT",
        value: 10,
        unit: "instructions",
        document: "40.5",
        gcc: "param_max_rtl_if_conversion_insns",
        provenance: Provenance::Gcc,
    },
    Constant {
        name: "PHIOPT_ARM_INSTRUCTIONS",
        value: 2,
        unit: "instructions per arm",
        document: "22.2",
        gcc: "",
        provenance: Provenance::Chosen,
    },
    Constant {
        name: "PHIOPT_UNPREDICTABLE_MARGIN_PERCENT",
        value: 25,
        unit: "percent",
        document: "22.2",
        gcc: "",
        provenance: Provenance::Chosen,
    },
    Constant {
        name: "SHORT_CIRCUIT_INSTRUCTIONS",
        value: 3,
        unit: "instructions",
        document: "22.5",
        gcc: "LOGICAL_OP_NON_SHORT_CIRCUIT",
        provenance: Provenance::Chosen,
    },
    Constant {
        name: "BLOCK_COPY_MOVES_FOR_SPEED",
        value: 8,
        unit: "moves",
        document: "40.7",
        gcc: "MOVE_RATIO",
        provenance: Provenance::Gcc,
    },
    Constant {
        name: "BLOCK_COPY_MOVES_FOR_SIZE",
        value: 4,
        unit: "moves",
        document: "40.7",
        gcc: "MOVE_RATIO when optimize_size",
        provenance: Provenance::Gcc,
    },
    Constant {
        name: "LOOP_HEADER_INSNS_FOR_SPEED",
        value: 20,
        unit: "instructions",
        document: "26.6",
        gcc: "max-loop-header-insns",
        provenance: Provenance::Gcc,
    },
    Constant {
        name: "LOOP_HEADER_INSNS_FOR_SIZE",
        value: 5,
        unit: "instructions",
        document: "26.6",
        gcc: "",
        provenance: Provenance::Chosen,
    },
    Constant {
        name: "REASSOC_WIDTH_UNTUNED",
        value: 1,
        unit: "operations in parallel",
        document: "40.8",
        gcc: "TARGET_SCHED_REASSOCIATION_WIDTH default",
        provenance: Provenance::Gcc,
    },
    Constant {
        name: "INLINE_FREQUENCY_CLAMP",
        value: 100,
        unit: "times the entry frequency",
        document: "40.11",
        gcc: "",
        provenance: Provenance::Chosen,
    },
    Constant {
        name: "INLINE_GROWTH_SQUARING_BOUND",
        value: 256,
        unit: "instructions of growth",
        document: "40.11",
        gcc: "overall_growth in edge_badness",
        provenance: Provenance::Gcc,
    },
    Constant {
        name: "HOT_BLOCK_FRACTION",
        value: 1000,
        unit: "one part in",
        document: "11.4",
        gcc: "param_hot_bb_frequency_fraction",
        provenance: Provenance::Gcc,
    },
    Constant {
        name: "PREDICT_EXPECT",
        value: 90,
        unit: "percent",
        document: "11.2",
        gcc: "param_builtin_expect_probability",
        provenance: Provenance::Gcc,
    },
    Constant {
        name: "PREDICT_NEVER_RETURNS",
        value: 99,
        unit: "percent",
        document: "11.2",
        gcc: "PRED_NORETURN",
        provenance: Provenance::Gcc,
    },
    Constant {
        name: "PREDICT_COLD_CALL",
        value: 99,
        unit: "percent",
        document: "11.2",
        gcc: "PRED_COLD_FUNCTION",
        provenance: Provenance::Gcc,
    },
    Constant {
        name: "PREDICT_LOOP_EXIT_NOT_TAKEN",
        value: 89,
        unit: "percent",
        document: "11.2",
        gcc: "PRED_LOOP_EXIT",
        provenance: Provenance::Gcc,
    },
    Constant {
        name: "PREDICT_LOOP_GUARD_TAKEN",
        value: 73,
        unit: "percent",
        document: "11.2",
        gcc: "PRED_LOOP_GUARD",
        provenance: Provenance::Gcc,
    },
    Constant {
        name: "PREDICT_POINTER_NOT_NULL",
        value: 70,
        unit: "percent",
        document: "11.2",
        gcc: "PRED_POINTER",
        provenance: Provenance::Gcc,
    },
    Constant {
        name: "PREDICT_NEGATIVE_RETURN",
        value: 98,
        unit: "percent",
        document: "11.2",
        gcc: "PRED_NEGATIVE_RETURN",
        provenance: Provenance::Gcc,
    },
    Constant {
        name: "PREDICT_NULL_RETURN",
        value: 71,
        unit: "percent",
        document: "11.2",
        gcc: "PRED_NULL_RETURN",
        provenance: Provenance::Gcc,
    },
    Constant {
        name: "PREDICT_CALL_NOT_TAKEN",
        value: 67,
        unit: "percent",
        document: "11.2",
        gcc: "PRED_CALL",
        provenance: Provenance::Gcc,
    },
    Constant {
        name: "PREDICT_CONTINUE_TAKEN",
        value: 67,
        unit: "percent",
        document: "11.2",
        gcc: "PRED_CONTINUE",
        provenance: Provenance::Gcc,
    },
    Constant {
        name: "MAX_PREDICTED_ITERATIONS",
        value: 100,
        unit: "iterations",
        document: "11.2",
        gcc: "param_max_predicted_iterations",
        provenance: Provenance::Gcc,
    },
    Constant {
        name: "LOOP_RESERVED_REGS",
        value: 2,
        unit: "registers",
        document: "40.6",
        gcc: "param_ira_loop_reserved_regs",
        provenance: Provenance::Gcc,
    },
    Constant {
        name: "LICM_EXPENSIVE",
        value: 20,
        unit: "cost units",
        document: "27.2",
        gcc: "param_lim_expensive",
        provenance: Provenance::Gcc,
    },
    Constant {
        name: "UNROLL_MAX_TIMES",
        value: 16,
        unit: "iterations",
        document: "29.2",
        gcc: "param_max_completely_peel_times",
        provenance: Provenance::Gcc,
    },
    Constant {
        name: "UNROLL_MAX_INSNS",
        value: 200,
        unit: "instructions",
        document: "29.2",
        gcc: "param_max_completely_peeled_insns",
        provenance: Provenance::Gcc,
    },
    Constant {
        name: "UNROLL_MAX_DEPTH",
        value: 8,
        unit: "loops",
        document: "29.2",
        gcc: "param_max_completely_peel_loop_nest_depth",
        provenance: Provenance::Gcc,
    },
    Constant {
        name: "PROFILE_SUM_TOLERANCE_PERCENT",
        value: 1,
        unit: "percent",
        document: "11.5",
        gcc: "",
        provenance: Provenance::Chosen,
    },
    Constant {
        name: "PREDICT_RETURN_BLOCKS",
        value: 8,
        unit: "blocks",
        document: "11.2",
        gcc: "",
        provenance: Provenance::Chosen,
    },
    Constant {
        name: "ALIGN_FREQUENCY_FRACTION",
        value: 100,
        unit: "one part in",
        document: "38.5",
        gcc: "",
        provenance: Provenance::Chosen,
    },
    Constant {
        name: "LOOP_ALIGN_MIN_ITERATIONS",
        value: 4,
        unit: "iterations",
        document: "38.5",
        gcc: "param_align_loop_iterations",
        provenance: Provenance::Gcc,
    },
    Constant {
        name: "IV_MAX_CONSIDERED_USES",
        value: 250,
        unit: "uses",
        document: "28.2",
        gcc: "param_iv_max_considered_uses",
        provenance: Provenance::Gcc,
    },
    Constant {
        name: "IV_CONSIDER_ALL_CANDIDATES_BOUND",
        value: 40,
        unit: "candidates",
        document: "28.2",
        gcc: "param_iv_consider_all_candidates_bound",
        provenance: Provenance::Gcc,
    },
    Constant {
        name: "IV_ALWAYS_PRUNE_CAND_SET_BOUND",
        value: 10,
        unit: "candidates",
        document: "28.2",
        gcc: "param_iv_always_prune_cand_set_bound",
        provenance: Provenance::Gcc,
    },
    Constant {
        name: "IVOPTS_NEW_VARIABLE_BIAS",
        value: 3,
        unit: "increments",
        document: "28.1",
        gcc: "",
        provenance: Provenance::Chosen,
    },
    Constant {
        name: "IVOPTS_SET_PENALTY",
        value: 10,
        unit: "cycles",
        document: "28.2",
        gcc: "",
        provenance: Provenance::Chosen,
    },
    Constant {
        name: "SCHEDULER_READY_LIST_BOUND",
        value: 100,
        unit: "instructions",
        document: "38.8",
        gcc: "param_max_sched_ready_insns",
        provenance: Provenance::Gcc,
    },
    Constant {
        name: "JUMP_TABLE_MIN_TARGETS",
        value: 8,
        unit: "case targets",
        document: "40.10",
        gcc: "param_case_values_threshold",
        provenance: Provenance::Awaiting,
    },
    Constant {
        name: "ALLOCATOR_DEGRADATION_PERCENT",
        value: 10,
        unit: "percent",
        document: "39.4",
        gcc: "",
        provenance: Provenance::Awaiting,
    },
];

#[cfg(test)]
mod tests {
    use super::{
        ALL, BLOCK_COPY_MOVES_FOR_SIZE, BLOCK_COPY_MOVES_FOR_SPEED, BRANCH_COST_FOR_SIZE,
        BRANCH_COST_PREDICTABLE, IF_CONVERSION_BUDGET_PREDICTABLE,
        IF_CONVERSION_BUDGET_UNPREDICTABLE, Provenance,
    };
    use crate::Cycles;

    #[test]
    fn the_table_matches_the_constants_it_describes() {
        // The list is data and the constants are code, and the two drifting apart would make the
        // report a description of a compiler nobody is running. Checking the whole list
        // automatically is not possible without a macro that would be larger than the list, so the
        // ones a pass is most likely to be wrong about are checked by hand.
        let by_name = |name: &str| ALL.iter().find(|c| c.name == name).expect(name).value;
        assert_eq!(by_name("BRANCH_COST_FOR_SIZE") * 100, BRANCH_COST_FOR_SIZE.raw());
        assert_eq!(by_name("BRANCH_COST_PREDICTABLE") * 100, BRANCH_COST_PREDICTABLE.raw());
        assert_eq!(by_name("BLOCK_COPY_MOVES_FOR_SPEED"), i64::from(BLOCK_COPY_MOVES_FOR_SPEED));
        assert_eq!(by_name("BLOCK_COPY_MOVES_FOR_SIZE"), i64::from(BLOCK_COPY_MOVES_FOR_SIZE));
    }

    #[test]
    fn every_constant_names_the_document_that_argued_for_it() {
        for constant in ALL {
            assert!(
                constant.document.starts_with(|c: char| c.is_ascii_digit()),
                "{} cites {:?}, which is not a section number",
                constant.name,
                constant.document
            );
            assert!(!constant.unit.is_empty(), "{} does not say what it counts", constant.name);
        }
    }

    #[test]
    fn a_constant_with_no_gcc_parameter_does_not_claim_to_be_adopted_from_gcc() {
        // The provenance is the only defence a number has, so a number claiming a defence it does
        // not have is worse than one admitting it was chosen.
        for constant in ALL {
            if constant.provenance == Provenance::Gcc {
                assert!(
                    !constant.gcc.is_empty(),
                    "{} says it came from gcc without saying from where",
                    constant.name
                );
            }
        }
    }

    #[test]
    fn the_two_unmeasured_constants_are_marked_and_not_dressed_up() {
        // Section 40.10 and section 39.4 both said the number would have to be measured. Until it
        // is, the honest thing is that a report can find them, and this test is what keeps them
        // findable when somebody later picks a value that looks confident.
        let waiting: Vec<&str> =
            ALL.iter().filter(|c| c.provenance == Provenance::Awaiting).map(|c| c.name).collect();
        assert_eq!(waiting, ["JUMP_TABLE_MIN_TARGETS", "ALLOCATOR_DEGRADATION_PERCENT"]);
    }

    #[test]
    fn an_unpredictable_branch_gets_a_larger_budget_than_a_predictable_one() {
        // The relationship matters more than either number. If a predictable branch ever got the
        // larger budget, if-conversion would spend the most instructions on the branches that were
        // already free. Both sides are constants, so the check happens at compile time and the
        // test is here to name what is being checked rather than to run it.
        const {
            assert!(IF_CONVERSION_BUDGET_UNPREDICTABLE > IF_CONVERSION_BUDGET_PREDICTABLE);
        }
        assert_eq!(BRANCH_COST_PREDICTABLE, Cycles::ZERO);
        assert!(BRANCH_COST_FOR_SIZE > BRANCH_COST_PREDICTABLE);
    }

    #[test]
    fn every_predictor_kept_is_above_the_bar_section_11_2_set() {
        // The document keeps the predictors at sixty five percent and above and drops the rest,
        // because a predictor at fifty nine moves a probability by nine points and nothing
        // downstream of it decides differently. A rate above ninety nine would be a certainty,
        // and a predictor that is certain is a fact and belongs in the analysis rather than here.
        for constant in ALL.iter().filter(|c| c.name.starts_with("PREDICT_") && c.unit == "percent")
        {
            assert!(
                (65..=99).contains(&constant.value),
                "{} predicts at {} percent, which section 11.2 would not have kept",
                constant.name,
                constant.value
            );
        }
    }

    #[test]
    fn every_name_in_the_table_is_distinct() {
        let mut names: Vec<&str> = ALL.iter().map(|c| c.name).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(names.len(), before);
    }
}
