//! Peephole rewrites: a small pattern of instructions becomes a smaller one.
//!
//! The third pass, and the one that will eventually not exist. Section 9.3 of
//! `spec/09-optimizer.md` says the value level optimizer is an acyclic e-graph, and that an
//! e-graph replaces what would otherwise be a folding pass, a peephole pass, a GVN pass, a
//! reassociation pass and an instcombine pass, all with a pass ordering problem between them.
//! This is the peephole pass, written now because the e-graph is a milestone away and because
//! there is a rewrite that unblocks twelve lowering rules today.
//!
//! Every rewrite here has to survive being moved into the rule set later, so each one is stated
//! as a pattern and a replacement in its own function and nothing shares state with anything.
//!
//! # The rewrites
//!
//! Two kinds. The rules of `rules/`, one file per tier, which are matched against every
//! instruction and are where anything new goes, and four rewrites written out by hand below them.
//!
//! ## The rules
//!
//! Six tiers of `spec/optimizer/13-rewrite-rules.md` section 13.4.
//!
//! Tier one is the identities. Adding nothing, multiplying by one, and'ing a value with itself.
//! None of them needs anything known about the operands and each leaves a term strictly smaller
//! than the one it replaced.
//!
//! Tier two is the strength reductions, which swap an operation for a cheaper one rather than
//! taking one away: multiplying by two is an addition, and multiplying or dividing by minus one is
//! a subtraction from nothing. Tier one is tried first because losing an operation beats swapping
//! one.
//!
//! Tier four is the width rules, the algebra of truncation and extension. Truncating an extension
//! back to the width it came from is the value that was there before either of them, and an
//! extension of an extension is one extension. This is the tier
//! the specification says pays on real C, and the reason is C rather than anything about this
//! compiler: the integer promotions widen nearly every operand of nearly every expression, and
//! most of those widenings compute something the instruction after them throws away. The widest
//! of those promotions starts at one bit, because a comparison answers in one and everything done
//! with the answer is done at the width of an `int` or wider, so the tier is written over that
//! source as well as over the four a machine computes in.
//!
//! Tier three is the canonicalisations, which put the constant of a commutative operation on the
//! right. They make nothing smaller and nothing faster. What they do is halve how many ways a term
//! can be written, so that every rule above them needs one variant where it needs two today, and
//! so that hash consing can see two spellings of one expression as one. They are tried last rather
//! than third, because rearranging a term is only worth doing when no rule that improves it fires.
//!
//! Tier five is the comparisons a type answers on its own, and tier six is the selects. A select
//! between a value and one more or one less than it, or between one and zero, is the value plus
//! or minus the condition widened, and that is a compare and a set where the select was a compare,
//! two moves and a conditional move. Tier six is matched with each arm offered as a number and
//! then with each arm expanded into the instruction that computed it, which is the only table
//! matched with a choice of which operand to expand.
//!
//! Every rule in every tier has been proved against `crates/rucc-ir/rules/ir.model` by
//! `rucc-verify` before it may be used.
//!
//! Which plans a tier is matched under belongs to the tier. Tiers one and two are matched with
//! either operand offered as a number, since a rule about a constant should fire whichever side it
//! was written on. Tier three is matched with the left operand offered as a number and the right
//! one refused if it is one, which is what makes a rule that moves the constant across fire once
//! rather than forever. Tier four is matched with the operand expanded into the instruction that
//! computed it, which is what a rule about two instructions at once needs and what none of the
//! others wants.
//!
//! What a rule leaves behind is one of four things. `(value.iN x)` means the result is a value
//! the function already has, so every use of the result is pointed at that value and the
//! instruction is left for [`crate::dce`]. `(iconst.iN k)` means the result is a constant, and the
//! instruction becomes that constant where it stands, which keeps the result value and is why
//! nothing else has to be rewritten for that half. An instruction means this one becomes that one
//! where it stands, which keeps the result value for the same reason, and an operand of it the
//! rule wrote as a number gets an `iconst` in front of the instruction to hold it. A conversion is
//! that same rewrite in place with one operand instead of two, and it is its own case because a
//! conversion is the one instruction whose operand is not the width of its result.
//!
//! An operand of either can itself be an instruction, which is what tier six writes and no tier
//! before it did. Those are built in front of the rewritten one, innermost first, the same way a
//! number the rule wrote is, and each is at the width its head names. One of them that is an
//! exclusive or with one on a comparison is the opposite comparison straight away, by the same
//! rewrite that turns one written in the source into it, because the walk has already gone past
//! the place it was built and would not come back to it.
//!
//! ## The four written by hand
//!
//! All four are about comparisons, and all four are here rather than in `rules/` for the same
//! reason: what each one is, is one statement quantified over the predicates, and the rule language
//! has no way to say that, so writing any of them as rules would mean writing out every predicate,
//! every operand order and every width by hand and keeping the enumeration in step with the two
//! predicate sets forever.
//!
//! ### A negation of a comparison
//!
//! An exclusive or of a comparison with an `i1` of all ones is that comparison with the opposite
//! predicate. That is issue 379, and it is worth more than the instruction it saves.
//!
//! C spells eight of the sixteen floating point predicates. The six relational and equality
//! operators give the six ordered ones, `!=` gives `une`, and `__builtin_isunordered` gives `uno`.
//! The other eight are what the negation of one of those means, and the front end writes a
//! negation as an exclusive or rather than as a flipped predicate, so `!(x < y)` lowers to an
//! `fcmp olt` and an `xor` where the machine has an `fcmp uge`. Twelve rules in the x86-64 rule
//! set are written on those predicates and none of them has ever fired, over the whole torture
//! suite at every optimization level, because no IR that reaches selection contains one.
//!
//! The integer case comes with it. `!(a < b)` on integers is the same shape, the same rewrite and
//! the same saving, and leaving it out because the coverage report did not complain about it would
//! be picking the rewrite by what measures it rather than by what it does.
//!
//! ### Two comparisons over one pair of operands
//!
//! An `and` or an `or` of two comparisons about the same two values is one comparison, or it is a
//! constant. `(x == y) && (x != y)` is false whatever `x` and `y` are, `(x >= y) || (x < y)` is
//! true, and `(x < y) || (x == y)` is `x <= y`, which is one instruction where there were three.
//!
//! The way to see all of that at once is to stop reading a predicate as a question and read it as
//! the set of answers it accepts. Two values are below, equal to or above one another, and two
//! floating point values can also be neither, so there are four cases, exactly one of them holds,
//! and a predicate is the subset it says yes to. `&&` is then the intersection of two subsets and
//! `||` is the union, an empty result is false, a full one is true, and anything else is whichever
//! predicate spells that subset. That is the whole rewrite, and the reason it is a paragraph
//! rather than a table is that the sixteen floating point predicates are the sixteen subsets of
//! the four cases, so the map back from a subset is total and has nothing to special case.
//!
//! Integers have three cases rather than four, and a complication the floating point side does not
//! have: `<` is two different questions depending on whether the operands are read signed or
//! unsigned, and a subset built out of one of each would be a subset about no reading in
//! particular. So each integer predicate carries which reading it wants, two that disagree refuse
//! to combine, and `==` and `!=` want neither and go with whatever the other one wanted.
//!
//! Nesting falls out of rewriting in place. A three way condition arrives as an `or` of an `or` and
//! a comparison, the walk reaches the inner one first and leaves a single comparison where it was,
//! and by the time the outer one is looked at it has a pair of comparisons under it rather than an
//! `or` and a comparison. That is what `gcc.c-torture/execute/ieee/compare-fp-3.c` needs and it
//! costs nothing to get.
//!
//! ### A comparison one operand's sign bit settles
//!
//! `fabs (x) < 0.0` is false whatever `x` holds. That is `gcc.c-torture/execute/20020720-1.c`, and
//! it asserts it the way the two above do, by calling a function it never defines.
//!
//! The same buckets answer it. A magnitude is a positive zero, a positive number, a positive
//! infinity or a NaN, so a pair made of one and a constant that is not positive is never in the
//! bucket where the magnitude is below, and against a negative constant it is never in the one
//! where the two are equal either. Narrow the predicate's set by the buckets the pair can be in and
//! read the answer back: nothing left is false, and anything left is a shorter question than the
//! one that was asked. `fabs (x) <= 0.0` comes out as `fabs (x) == 0.0` that way, which is not a
//! constant and is still worth having.
//!
//! What it does not come out as is true. The narrowing only ever takes buckets away and always
//! takes at least one, so `fabs (x) >= 0.0` is left exactly as it was written, which is the right
//! answer rather than a missed one: a NaN has its sign bit cleared like anything else and is not
//! above, below or equal to anything at all.
//!
//! `fabs` is not a call by the time this runs. The front end knows the plain library name as well
//! as the prefixed one and lowers both to the bits, because the magnitude of a value is that value
//! with its sign bit cleared and there is nothing to call. So what the pattern looks for is a
//! bitcast of an `and` against a mask whose top bit is clear, which is what that lowering leaves.
//!
//! ### A comparison a constant or a repeated operand settles
//!
//! A NaN is unordered against everything, so `dnan < x` is false and `dnan != x` is true whatever
//! `x` holds. That is `gcc.c-torture/execute/ieee/fp-cmp-6.c` and `fp-cmp-9.c`, with the NaN read
//! out of a `const` global, and `fp-cmp-7.c` asks the same of `x > inf`, which nothing is.
//!
//! The buckets again, narrowed by what the operands allow rather than by what a magnitude does. A
//! NaN on either side leaves only the unordered bucket, two constants leave the one they are in, an
//! infinity leaves every bucket but the one past it, and a value against itself is equal or
//! unordered. Unlike the sign rewrite this one can come out true, since the unordered predicates
//! accept the one bucket a NaN leaves. gcc 16 folds all of these without `-ffast-math`.
//!
//! A branch in front narrows the pair as well. Walking back along edges that are the only way into
//! their block, a branch on a comparison of the same two operands says which side was taken, and so
//! which buckets are left. That is `isunordered (x, y) || !isunordered (x, y)` in `compare-fp-3.c`
//! at the levels that keep the `||` as two branches, where the second test is only reached when the
//! pair is ordered.
//!
//! # Why it needs dead code elimination after it
//!
//! The rewrite turns the `xor` into the comparison and leaves the original comparison where it
//! was, used by nothing when the negation was its only reader. Rewriting in place keeps the
//! result value, so every use of it is already correct and there is nothing to rewrite, and what
//! is left over is exactly what [`crate::dce`] takes out. That is why the pipeline runs the two in
//! this order, and it is why the pass before the dead code eliminator was written first.
//!
//! An identity that produces a value leaves the same kind of litter for the same reason. The
//! instruction it fired on reads what it always read and nothing reads it, so it is dead, and
//! taking it out here would mean deciding whether its operands are still read by anything, which
//! is the question the dead code eliminator answers for the whole function at once.
//!
//! The composite rewrite leaves two of them rather than one, and in the case that comes out
//! constant it leaves both comparisons and computes nothing at all. The sign rewrite leaves the
//! four instructions the magnitude was built out of. Same litter, same reason, same pass takes it
//! out.

use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::OnceLock;

use rucc_base::float::Float;
use rucc_ir::term::{PLAIN, Plan, Shown, Term, Terms};
use rucc_ir::{
    Block, Def, Extra, Flags, FloatPred, Func, Imm, Inst, InstData, IntPred, Opcode, Type, Value,
};

use crate::cfg::Cfg;
use crate::discharge::constant;
use crate::rules::{
    Match, Piece, Subject, Table, canonical, compare, identities, select, strength, width,
};
use crate::uses::{count, substitute};
use crate::{Analyses, Analysis, Fuel, Pass, Preserved, Stats};

/// Recorded once for each negation folded into the comparison under it.
const FLIPPED: &str = "comparison negated by an exclusive or rewritten as the opposite comparison";

/// Recorded for a negation that would have folded if there had been fuel for it.
const NO_FUEL: &str = "negated comparison left alone, the pass ran out of fuel";

/// Recorded once for each pair of comparisons over one operand pair folded into one answer.
const COMPOSITE: &str = "two comparisons over the same operands combined into one";

/// Recorded for a pair that would have folded if there had been fuel for it.
const NO_FUEL_COMPOSITE: &str = "pair of comparisons left alone, the pass ran out of fuel";

/// Recorded once for each comparison the sign of one operand settles on its own.
const MAGNITUDE: &str = "comparison against a value whose sign bit is clear settled by the sign";

/// Recorded for one of those that would have folded if there had been fuel for it.
const NO_FUEL_MAGNITUDE: &str =
    "comparison against a magnitude left alone, the pass ran out of fuel";

/// Recorded once for each floating point comparison a constant or a repeated operand settles.
const BOUNDED: &str = "floating point comparison settled by a constant or by one operand twice";

/// Recorded for one of those that would have folded if there had been fuel for it.
const NO_FUEL_BOUNDED: &str =
    "floating point comparison against a bound left alone, the pass ran out of fuel";

/// Recorded for a rule that would have fired if there had been fuel for it.
const NO_FUEL_RULE: &str = "rewrite left alone, the pass ran out of fuel";

/// How each operand of an instruction is shown to the matcher, and in what order the ways are
/// tried.
///
/// The two with a constant come first, because a rule about a number is the more specific one and
/// an operand that is not a constant declines it at the first node of the trie. Nothing here
/// expands an operand into the instruction that computed it, since no tier one identity is about
/// two instructions at once.
const PLANS: [Plan; 3] =
    [[Shown::Reg, Shown::Const, Shown::Reg], [Shown::Const, Shown::Reg, Shown::Reg], PLAIN];

/// How the operands are shown to a canonicalisation, which is the one plan tier three is matched
/// under.
///
/// A canonicalisation moves the constant to the right, so the left operand has to be the number
/// and the right one has to be something that is not, or the rule swaps a pair of constants back
/// and forth until the pass runs out of fuel. [`Shown::Var`] is what says the right one is not a
/// number. The plans above cannot be reused here for exactly that reason: the second of them
/// shows a constant left operand as a number and a constant right operand as a register, which is
/// the cycling match.
const CANONICAL: [Plan; 1] = [[Shown::Const, Shown::Var, Shown::Reg]];

/// How the operands are shown to a width rule, which is the one plan tier four is matched under.
///
/// Every rule in that tier is about two instructions at once, a conversion and the conversion or
/// value under it, so the operand it is about has to be shown as the instruction that computed it
/// rather than as a register holding the answer. That is [`Shown::Expand`], and it is the first
/// plan here to use it.
///
/// One operand, because every instruction the tier matches has one. The other two entries are
/// never read and say [`Shown::Reg`] because that is what an operand nobody asks about is.
const EXPAND: [Plan; 1] = [[Shown::Expand, Shown::Reg, Shown::Reg]];

/// How the operands are shown to a comparison rule, which is the two plans tier five is matched
/// under.
///
/// Every rule in that tier compares something against a constant, and writes the constant on the
/// right, so the right operand is shown as a number in both. What differs is the left one. Most of
/// the tier is about the value itself and shows it as a register, which is the first of [`PLANS`]
/// spelled again rather than borrowed, because the other two of those would be tried for nothing:
/// a comparison with the constant on the left matches no rule here, and neither does one with no
/// constant at all.
///
/// The rest of the tier is about a widened boolean compared against zero, which is two
/// instructions at once, so the left operand is shown as the instruction that computed it the way
/// tier four shows its one operand. That is the second plan, and it is a plan of its own rather
/// than a rule in tier four because the instruction that matched is a comparison: the predicate is
/// not part of the opcode, which is what makes a tier a separate file here.
///
/// The constant on the left is not the missing half of the tier. A comparison is not commutative,
/// so `0 < x` is not `x < 0` with the operands swapped, it is `x > 0`, and turning the first into
/// the second is a canonicalisation that belongs in tier three rather than four more rules here.
const COMPARE: [Plan; 2] =
    [[Shown::Reg, Shown::Const, Shown::Reg], [Shown::Expand, Shown::Const, Shown::Reg]];

/// How the operands are shown to a select rule, which is the three plans tier six is matched
/// under.
///
/// The condition is a register in all three, since what the tier asks of it is only that it is
/// one bit. The arms are what differ. The first plan shows both as numbers, for the rules about a
/// select between two constants, and the other two expand one arm each into the instruction that
/// computed it, for the rules about a value and one more or less than it. Two plans rather than
/// one that expands both, because the arm that is not expanded is the value the other was computed
/// from, and a pattern can only say that two places are the same value when both are shown as
/// registers.
const SELECT: [Plan; 3] = [
    [Shown::Reg, Shown::Const, Shown::Const],
    [Shown::Reg, Shown::Expand, Shown::Reg],
    [Shown::Reg, Shown::Reg, Shown::Expand],
];

/// The rule tables, one per tier, in the order they are tried, each with the plans it is matched
/// under.
///
/// Tier one first, because an identity takes an operation away and a strength reduction swaps one
/// for another, so a term both have something to say about is better off losing the operation.
/// Tier four after those two and tier three last, because a canonicalisation only makes a term
/// easier for another rule to be about and there is no reason to reach for it while a rule that
/// improves the code still fires. Nothing turns on the order of those last two anyway: tier three
/// is about a commutative operation with a constant in it and tier four is about a conversion, so
/// no instruction is one both have something to say about.
///
/// The plans belong to the table rather than to the loop because a tier is written against them.
/// Tier three is only correct under the one plan that refuses a constant on the right, and a
/// table matched under a plan it was not written for is a table whose rules mean something else.
/// Tier four is the other way round: its rules mean nothing at all under a plan that does not
/// expand, since the second level of every one of its patterns is an instruction.
///
/// Tier five sits where it does because nothing turns on it either. It is the only table about a
/// comparison and no other table mentions one, so there is no instruction two of them have
/// something to say about and no order in which one of them gets there first. Tier six is the
/// same: it is the only table about a select.
const TABLES: [(&Table, &[Plan]); 6] = [
    (&identities::TABLE, &PLANS),
    (&strength::TABLE, &PLANS),
    (&width::TABLE, &EXPAND),
    (&compare::TABLE, &COMPARE),
    (&select::TABLE, &SELECT),
    (&canonical::TABLE, &CANONICAL),
];

/// The pass. It holds nothing, because a peephole needs to know nothing beyond the pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Simplify;

impl Pass for Simplify {
    fn name(&self) -> &'static str {
        "simplify"
    }

    fn describe(&self) -> &'static str {
        "the identities, the strength reductions, the canonicalisations, and the four comparison \
         rewrites written by hand"
    }

    fn preserves(&self) -> Preserved {
        // Everything about the shape of the function. No block is added, none is removed and no
        // edge moves, so the graph and everything built out of it stand.
        //
        // The liveness does not, and that is the whole of the difference. An identity that
        // produces a value points every reader of one value at another, which is one more place
        // the second is live and one fewer the first is, and the same is true of the negation
        // below, which reads the comparison's operands where it used to read its result.
        //
        // A rule that writes an instruction with a constant in it puts one in the block, and that
        // is still the same answer. It adds a value nothing else mentions, in the block it is
        // read in, and it ends every path it starts on, so nothing about the shape of the
        // function moves and the only analysis with something new to say about it is the one
        // already given up.
        Preserved::ALL.without(Analysis::Liveness)
    }

    fn run(&self, func: &mut Func, _an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        // What a rule that produced a value decided, applied to the whole function at the end.
        // Rewriting each one where it is found would be a walk over every instruction for every
        // rewrite, and there is nothing to be gained by it: what a pattern asks about is the
        // instruction and its operands, and neither changes under a redirection.
        let mut forward: HashMap<Value, Value> = HashMap::new();
        // Who reads what, so that an instruction nothing reads is left alone. A rule that fires
        // on one changes no program, because what it does is point the readers somewhere else and
        // there are none, and it would still spend fuel and still report having optimized
        // something. That matters here more than it would in a pass that runs once: this pass is
        // named twice in every pipeline above `-O0`, an identity it takes stays in the function
        // until dead code elimination removes it, and without this the second run would rewrite
        // everything the first run did all over again and say so.
        //
        // Stale by design. It is what the function looked like when this run started, and a
        // rewrite below only ever removes readers, so a value this says nothing reads is a value
        // nothing reads.
        let uses = count(func);
        // The edges, for the comparisons a branch in front of them settles. Nothing here adds or
        // removes an edge, so one built before the walk is the one the walk would build.
        let cfg = Cfg::new(func);
        let dead = |func: &Func, inst: Inst| match func[inst].first_result {
            Some(result) => uses[result.index()] == 0,
            None => false,
        };
        for block in func.blocks().collect::<Vec<Block>>() {
            for inst in func.insts(block).collect::<Vec<Inst>>() {
                if dead(func, inst) {
                    continue;
                }
                if let Some(flip) = negated_comparison(func, inst) {
                    if !fuel.take() {
                        // Out of fuel, which stops the transforming rather than the looking, the
                        // same way the other two passes treat it. The walk is the same walk at
                        // every fuel setting, which is what makes bisecting over it monotonic.
                        stats.missed(NO_FUEL);
                        continue;
                    }
                    become_flipped(func, inst, &flip);
                    stats.optimized(FLIPPED);
                    continue;
                }
                if let Some(composite) = composite_comparison(func, inst) {
                    if !fuel.take() {
                        stats.missed(NO_FUEL_COMPOSITE);
                        continue;
                    }
                    fold_composite(func, inst, composite);
                    stats.optimized(COMPOSITE);
                    continue;
                }
                if let Some(settled) = magnitude_comparison(func, inst) {
                    if !fuel.take() {
                        stats.missed(NO_FUEL_MAGNITUDE);
                        continue;
                    }
                    fold_composite(func, inst, settled);
                    stats.optimized(MAGNITUDE);
                    continue;
                }
                if let Some(settled) = bounded_comparison(func, &cfg, inst) {
                    if !fuel.take() {
                        stats.missed(NO_FUEL_BOUNDED);
                        continue;
                    }
                    fold_composite(func, inst, settled);
                    stats.optimized(BOUNDED);
                    continue;
                }
                let Some((rewrite, pattern)) = identity(func, inst) else { continue };
                if !fuel.take() {
                    stats.missed(NO_FUEL_RULE);
                    continue;
                }
                match rewrite {
                    Rewrite::Value(value) => {
                        let result = func[inst].first_result.expect("the rule matched a result");
                        forward.insert(result, value);
                    }
                    Rewrite::Constant(number) => become_constant(func, inst, number),
                    Rewrite::Built { opcode, pred, lhs, rhs } => {
                        become_instruction(func, inst, opcode, pred, lhs, rhs);
                    }
                    Rewrite::Converted { opcode, from } => {
                        let ty =
                            func[func[inst].first_result.expect("the rule matched a result")].ty;
                        let from = defined(func, inst, ty, from);
                        become_conversion(func, inst, opcode, from);
                    }
                }
                stats.optimized(pattern);
            }
        }
        if !forward.is_empty() {
            substitute(func, &forward);
        }
        stats
    }
}

/// What a rule says an instruction's result is instead.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Rewrite {
    /// A value the function already has, which every reader of the result is pointed at.
    Value(Value),
    /// A number, which the instruction becomes where it stands.
    Constant(i128),
    /// Another instruction, which this one becomes where it stands.
    Built {
        /// What it is.
        opcode: Opcode,
        /// Which comparison it is, when it is one.
        ///
        /// The predicate is not part of the opcode. Every one of the ten integer comparisons is
        /// `ICmp` and the predicate is beside it, so an opcode on its own does not say what a
        /// rule asked for, and a rule that wrote `icmp_sge` and got the predicate of the
        /// instruction it replaced would compute the opposite rather than something else.
        pred: Option<IntPred>,
        /// Its left operand.
        lhs: Operand,
        /// Its right operand.
        rhs: Operand,
    },
    /// A conversion, which this one becomes where it stands.
    ///
    /// Separate from [`Rewrite::Built`] rather than one variant with a list of operands, because a
    /// conversion is the one instruction a rule writes whose operand is not the width of its
    /// result. That is what makes it the one whose operand cannot be a number the rule wrote:
    /// there would be no width to give the constant, and every rule that writes one of these
    /// writes a value the pattern bound or an instruction built out of those.
    Converted {
        /// Which of the three it is.
        opcode: Opcode,
        /// What it converts, which is never [`Operand::Constant`].
        from: Operand,
    },
}

/// One operand of an instruction a rule writes.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Operand {
    /// A value the pattern bound.
    Value(Value),
    /// A number the rule wrote, which needs an `iconst` in front of the instruction before it is
    /// an operand at all, because an operand in this IR is a value and a number is not one until
    /// something defines it.
    Constant {
        /// The number.
        number: i128,
        /// How wide it is, which is the width the `iconst.iN` head named.
        ///
        /// Taken from the rule rather than from the instruction's result, because the two are
        /// the same width for everything above and are not for a comparison: the result of one
        /// is a single bit and its operands are as wide as what was compared. A constant built
        /// at the result's width would be a one bit zero standing where a thirty two bit one
        /// was asked for.
        bits: u32,
    },
    /// An instruction the rule wrote under the one it rewrites, which is built in front of it.
    Built(Box<Nested>),
}

/// An instruction a rule writes as an operand of another, which has no value until it is built.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Nested {
    /// What it is.
    opcode: Opcode,
    /// Which comparison it is, when it is one, for the reason [`Rewrite::Built`] gives.
    pred: Option<IntPred>,
    /// How wide its result is, which is the width its head names. A comparison's is one.
    bits: u32,
    /// Its operands, one for a conversion and two for anything else.
    args: Vec<Operand>,
}

/// The rule that fires on this instruction, and the pattern it came from.
///
/// The plans are tried in order and the first that matches wins. A plan is how the operands are
/// shown rather than what they are, so trying three of them is three walks over a trie, each of
/// which fails in its first node or two when the instruction is not one any rule is about.
fn identity(func: &Func, inst: Inst) -> Option<(Rewrite, &'static str)> {
    let result = func[inst].first_result?;
    for (table, plan) in
        TABLES.into_iter().flat_map(|(table, plans)| plans.iter().map(move |&plan| (table, plan)))
    {
        let terms = Terms::new(func, inst, plan);
        let Some(found) = table.find(&terms, Term::Root) else { continue };
        let rule = table.rule(&found);
        let rewrite = match rule.replacement {
            // A value the pattern bound, which is a register because that is the only thing a
            // `value.iN` binds.
            [Piece::App { head, arity: 1 }, Piece::Var { index, .. }]
                if head.starts_with("value.") =>
            {
                match found.bindings.get(*index) {
                    Some(&Term::Reg(value)) => Rewrite::Value(value),
                    _ => continue,
                }
            }
            // A constant written in the rule. Only at a width the instruction's result has, which
            // it always does: an `iconst.iN` names an integer width and a rule is proved at the
            // width it is written at.
            [Piece::App { head, arity: 1 }, Piece::Int(number)]
                if head.starts_with("iconst.") && func[result].ty.is_int() =>
            {
                Rewrite::Constant(*number)
            }
            // An instruction the rule writes, which this one becomes. That is the third shape and
            // the last one. What is under it, when the rule wrote something deeper than one
            // instruction, is built in front of it.
            pieces => match built(pieces, &found, &matched(&terms, &found)) {
                // Something built under the instruction is built at the width its head names,
                // which is a scalar, so the rule is not one about a vector whatever it matched.
                Some(rewrite) if nests(&rewrite) && func[result].ty.is_vector() => continue,
                Some(rewrite) => rewrite,
                // Any other shape, which no rule in the file has. A test below says so, because a
                // rule that fell through here would be a rule that never fires and nothing would
                // say it had stopped.
                None => continue,
            },
        };
        return Some((rewrite, rule.pattern));
    }
    None
}

/// The instruction a rule writes, out of the pieces its replacement flattened into.
///
/// Two operands under a head that names an opcode, each of them either a value the pattern bound
/// or a number the rule wrote. Anything else is nothing this pass can build, and the answer to
/// one is that the rule does not fire, which the test over the whole table turns into a failure
/// rather than a silence.
fn built(
    pieces: &'static [Piece],
    found: &Match<Term>,
    matched: &[Option<i128>],
) -> Option<Rewrite> {
    if let Some(rewrite) = converted(pieces, found, matched) {
        return Some(rewrite);
    }
    let [Piece::App { head, arity: 2 }, rest @ ..] = pieces else { return None };
    let opcode = opcode_of(head)?;
    // The predicate comes from the same head the opcode did, so a rule whose replacement this
    // pass can build is a rule written in the vocabulary it matched with, predicate and all.
    let pred = rucc_ir::term::int_pred(head);
    if (opcode == Opcode::ICmp) != pred.is_some() {
        // A comparison whose head names no predicate, or a predicate on something that is not a
        // comparison. Neither is a head the vocabulary produces, so neither is a rule anybody
        // wrote, and building the instruction anyway would mean guessing at one of the two.
        return None;
    }
    let (lhs, rest) = operand(rest, found, matched)?;
    let (rhs, rest) = operand(rest, found, matched)?;
    rest.is_empty().then_some(Rewrite::Built { opcode, pred, lhs, rhs })
}

/// The constants the pattern matched, one entry per binding, in the order it bound them.
///
/// The same list a guard is handed and worked out the same way, which is what lets a computed
/// piece be written in the names the pattern bound. It is collected here rather than kept from
/// the match because most rules have no computation and no guard and would pay for it every time.
fn matched(terms: &Terms<'_>, found: &Match<Term>) -> Vec<Option<i128>> {
    found.bindings.iter().map(|&node| terms.int(node)).collect()
}

/// The conversion a rule writes, if it wrote one.
///
/// Three heads rather than any head of one operand, because the width rules are the only tier that
/// writes an instruction with one, and being specific is what keeps this from claiming a
/// replacement it cannot build. A `value.iN` or an `iconst.iN` is also a head of one operand and
/// neither is an instruction, and [`identity`] has already dealt with both by the time anything
/// gets here, so a test would not catch the day one slipped past.
///
/// The operand is a value the pattern bound or an instruction built out of those, and never a
/// number. A number would need a width to be written at and the result's width is the wrong one
/// for a conversion, which is the whole reason this is separate from [`built`].
fn converted(
    pieces: &'static [Piece],
    found: &Match<Term>,
    matched: &[Option<i128>],
) -> Option<Rewrite> {
    let [Piece::App { head, arity: 1 }, rest @ ..] = pieces else { return None };
    let opcode = match opcode_of(head)? {
        opcode @ (Opcode::SExt | Opcode::ZExt | Opcode::Trunc) => opcode,
        _ => return None,
    };
    match operand(rest, found, matched)? {
        (Operand::Constant { .. }, _) => None,
        (from, []) => Some(Rewrite::Converted { opcode, from }),
        _ => None,
    }
}

/// Whether a rewrite builds anything in front of the instruction it rewrites, beyond a number.
fn nests(rewrite: &Rewrite) -> bool {
    match rewrite {
        Rewrite::Built { lhs, rhs, .. } => {
            matches!(lhs, Operand::Built(_)) || matches!(rhs, Operand::Built(_))
        }
        Rewrite::Converted { from, .. } => matches!(from, Operand::Built(_)),
        Rewrite::Value(_) | Rewrite::Constant(_) => false,
    }
}

/// One operand of that instruction, and the pieces after it.
fn operand(
    pieces: &'static [Piece],
    found: &Match<Term>,
    matched: &[Option<i128>],
) -> Option<(Operand, &'static [Piece])> {
    match pieces {
        [Piece::App { head, arity: 1 }, Piece::Var { index, .. }, rest @ ..]
            if head.starts_with("value.") =>
        {
            match found.bindings.get(*index) {
                Some(&Term::Reg(value)) => Some((Operand::Value(value), rest)),
                _ => None,
            }
        }
        [Piece::App { head, arity: 1 }, Piece::Int(number), rest @ ..]
            if head.starts_with("iconst.") =>
        {
            Some((Operand::Constant { number: *number, bits: bits_of(head)? }, rest))
        }
        // A number the rule works out of the ones it matched, which is how a rule about every
        // power of two is written once rather than once per power. The computation gives nothing
        // back when a binding it reads is not a constant, and the answer to that is the same as a
        // guard that does not hold: the rule does not fire.
        [Piece::App { head, arity: 1 }, Piece::Computed { work, .. }, rest @ ..]
            if head.starts_with("iconst.") =>
        {
            let number = work(matched)?;
            Some((Operand::Constant { number, bits: bits_of(head)? }, rest))
        }
        // A number the pattern bound rather than one the rule wrote. This is what a
        // canonicalisation needs: it moves the operand it matched to the other side, and what it
        // matched was whatever number happened to be there.
        [Piece::App { head, arity: 1 }, Piece::Var { index, .. }, rest @ ..]
            if head.starts_with("iconst.") =>
        {
            match found.bindings.get(*index) {
                Some(&Term::Num(number)) => {
                    Some((Operand::Constant { number, bits: bits_of(head)? }, rest))
                }
                _ => None,
            }
        }
        [Piece::App { head, arity }, rest @ ..] => nested(head, *arity, rest, found, matched),
        _ => None,
    }
}

/// An instruction the rule wrote as an operand, and the pieces after it.
///
/// The same two shapes [`built`] and [`converted`] take at the top, a conversion of one operand
/// that is not a number and anything else of two, with the predicate read off the head for a
/// comparison. A constant is not one of these: an `iconst` head the arms of [`operand`] did not
/// take is one with something other than a number under it.
fn nested(
    head: &str,
    arity: usize,
    pieces: &'static [Piece],
    found: &Match<Term>,
    matched: &[Option<i128>],
) -> Option<(Operand, &'static [Piece])> {
    let opcode = opcode_of(head)?;
    let pred = rucc_ir::term::int_pred(head);
    let converts = matches!(opcode, Opcode::SExt | Opcode::ZExt | Opcode::Trunc);
    if opcode == Opcode::IConst
        || (opcode == Opcode::ICmp) != pred.is_some()
        || converts != (arity == 1)
        || !(1..=2).contains(&arity)
    {
        return None;
    }
    let bits = bits_of(head)?;
    let mut args = Vec::with_capacity(arity);
    let mut rest = pieces;
    for _ in 0..arity {
        let (arg, after) = operand(rest, found, matched)?;
        if converts && matches!(arg, Operand::Constant { .. }) {
            return None;
        }
        args.push(arg);
        rest = after;
    }
    Some((Operand::Built(Box::new(Nested { opcode, pred, bits, args })), rest))
}

/// The width a head names, out of the `iN` after its last dot.
///
/// Every head that takes a width ends in one, and reading it off the name is what keeps the width
/// a rule was written at attached to the rule rather than inferred from whatever the instruction
/// being replaced happened to be. A head with no width, or one whose width is not a number, is a
/// head this cannot build an operand for, and the answer to that is that the rule does not fire.
fn bits_of(head: &str) -> Option<u32> {
    head.rsplit_once('.')?.1.strip_prefix('i')?.parse().ok()
}

/// The opcode a replacement head names, or nothing if the rules have no instruction by that name.
///
/// Built the once out of [`rucc_ir::term::heads`], which is where the name of the instruction a
/// pattern matched comes from as well, so a rule whose replacement this pass can build is a rule
/// written in the vocabulary it matched with. A table here would be a second vocabulary and the
/// two would drift.
///
/// A name two opcodes answer to belongs to the first of them, which is the general one:
/// `ptr_add` is an add at the address width and is named as one, and a rule that writes `add` is
/// asking for the add.
fn opcode_of(head: &str) -> Option<Opcode> {
    static NAMES: OnceLock<HashMap<&'static str, Opcode>> = OnceLock::new();
    let names = NAMES.get_or_init(|| {
        let mut names = HashMap::new();
        for (opcode, name) in rucc_ir::term::heads() {
            names.entry(name).or_insert(opcode);
        }
        names
    });
    names.get(head).copied()
}

/// Turns an instruction into the one a rule says computes the same thing.
///
/// In place, like the constant below and for the same reason: the result value survives, so every
/// reader of it is already right and there is nothing to redirect.
fn become_instruction(
    func: &mut Func,
    inst: Inst,
    opcode: Opcode,
    pred: Option<IntPred>,
    lhs: Operand,
    rhs: Operand,
) {
    let result = func[inst].first_result.expect("the rule matched a result");
    let ty = func[result].ty;
    let kept = carried(func, inst, opcode, &lhs, &rhs);
    let lhs = defined(func, inst, ty, lhs);
    let rhs = defined(func, inst, ty, rhs);
    let args = func.push_values(&[lhs, rhs]);
    let data = &mut func[inst];
    data.opcode = opcode;
    data.args = args;
    // The predicate the rule named, and nothing else a rule writes carries an extra. What was
    // there belonged to the instruction that is gone, which is the case that matters: a rule
    // rewriting a comparison into an addition that left the predicate behind would leave an
    // addition claiming to be `slt`, and one rewriting a comparison into another comparison that
    // kept the old predicate would compute the opposite of what it said.
    data.extra = match pred {
        Some(pred) => Extra::IntPred(pred),
        None => Extra::None,
    };
    // The flags go with the instruction that had them, the same as for a constant. An `nsw` on a
    // multiplication is a promise about that multiplication, and the addition that replaces it is
    // a different instruction. The promise may well still hold, and carrying one across a rewrite
    // because it probably still holds is how a wrong one gets made. Dropping it costs a later
    // pass an assumption and costs no program its meaning. The one exception is a promise that is
    // provably the same one, which `carried` says.
    data.flags = kept;
}

/// The flags a rewrite keeps from the instruction it replaces, which is none but for the few
/// rewrites of a multiplication by a constant where the promise is the same one on both sides.
///
/// Each case is checked against what the instruction was as well as what the rule writes, so a
/// rule added later that happens to share the opcodes keeps nothing until it is added here with
/// its own reason.
///
/// - `k * x` written as `x * k` is the same product.
/// - `x * 2` written as `x + x` is the same sum, and both flags say that `2x` fits.
/// - `x * -1` written as `0 - x` promises the same about the signed result, which is that `x` is
///   not the most negative number. Read unsigned, `-1` is the largest number there is and the
///   multiplication's `nuw` is about a different product, so only `nsw` is kept.
/// - `x * 2^k` written as `x << k`. `nsw` on a shift says that `x * 2^k` fits the signed width,
///   which is what it says on the multiplication while `2^k` is a positive number at that width,
///   so every `k` below the width less one. At the width less one the constant read as signed is
///   the most negative number and nothing is kept. `nuw` is the same on both.
///
/// It matters because of what reads the flags afterwards. `row * 128` that loses its `nsw` on the
/// way to a shift is a subscript scalar evolution can no longer widen to the address width, and
/// every check on `grid[row * 128 + col]` stays inside the loop, which was `a-strided-column-sum`.
/// See #1748.
fn carried(func: &Func, inst: Inst, now: Opcode, lhs: &Operand, rhs: &Operand) -> Flags {
    let data = func[inst];
    let args = &func[data.args];
    let (Opcode::Mul, Some(&first), Some(&second)) = (data.opcode, args.first(), args.get(1))
    else {
        return Flags::NONE;
    };
    let (x, k) = match (constant(func, first), constant(func, second)) {
        (None, Some(k)) => (first, k),
        (Some(k), None) => (second, k),
        _ => return Flags::NONE,
    };
    let both = data.flags.intersection(Flags::NSW.union(Flags::NUW));
    match (now, lhs, rhs) {
        (Opcode::Mul, &Operand::Value(v), &Operand::Constant { number, bits })
            if v == x && bits < i128::BITS && (number ^ k) & ((1 << bits) - 1) == 0 =>
        {
            both
        }
        (Opcode::Add, &Operand::Value(v), &Operand::Value(w)) if v == x && w == x && k == 2 => both,
        (Opcode::Sub, &Operand::Constant { number: 0, .. }, &Operand::Value(v))
            if v == x && k == -1 =>
        {
            data.flags.intersection(Flags::NSW)
        }
        (Opcode::Shl, &Operand::Value(v), &Operand::Constant { number, bits })
            if v == x && (0..i128::from(bits) - 1).contains(&number) && k == 1 << number =>
        {
            both
        }
        _ => Flags::NONE,
    }
}

/// Turns an instruction into the conversion a rule says computes the same thing.
///
/// In place, for the same reason as the two above: the result value survives, so every reader of
/// it is already right.
///
/// The result keeps the type it had, which is the type the rule wrote. A replacement head names
/// both widths it converts between, `rucc-verify` refuses a replacement narrower than the pattern
/// and the rules are written with the two the same, so the width the head names on the way out is
/// the width the instruction already produces.
fn become_conversion(func: &mut Func, inst: Inst, opcode: Opcode, from: Value) {
    let args = func.push_values(&[from]);
    let data = &mut func[inst];
    data.opcode = opcode;
    data.args = args;
    // Nothing a rule writes carries an extra, and the flags belonged to the instruction that is
    // gone. Both for the reasons `become_instruction` gives.
    data.extra = Extra::None;
    data.flags = Flags::NONE;
}

/// An operand as a value, defining it in front of the instruction if the rule wrote a number.
///
/// `ty` is the type of the instruction's result, which is the width the constant is built at for
/// everything whose operands are as wide as what it produces. A comparison is the exception and
/// the reason the rule's own width is carried this far: its result is one bit and its operands are
/// as wide as what was compared, so the width comes from the `iconst.iN` the rule wrote and the
/// result's type is used only for its shape.
fn defined(func: &mut Func, before: Inst, ty: Type, operand: Operand) -> Value {
    match operand {
        Operand::Value(value) => value,
        Operand::Constant { number, bits } => {
            let ty = if ty.lane() == Type::int(bits) { ty } else { Type::int(bits) };
            let at = func.add_imm(Imm::int(number, ty.lane()));
            let data = InstData { extra: Extra::Imm(at), ..InstData::new(Opcode::IConst) };
            let span = func.span(before);
            let iconst = func.create_inst(data, &[ty], span);
            func.insert_before(iconst, before);
            func[iconst].first_result.expect("one result was asked for")
        }
        Operand::Built(nested) => {
            let Nested { opcode, pred, bits, args } = *nested;
            let ty = Type::int(bits);
            let args: Vec<Value> =
                args.into_iter().map(|arg| defined(func, before, ty, arg)).collect();
            let args = func.push_values(&args);
            let extra = pred.map_or(Extra::None, Extra::IntPred);
            let data = InstData { args, extra, ..InstData::new(opcode) };
            let span = func.span(before);
            let inst = func.create_inst(data, &[ty], span);
            func.insert_before(inst, before);
            // The walk is past this point already, so a negation built here would be left for the
            // next run of the pass, and at `-O2` there is none after the one that builds it.
            if let Some(flip) = negated_comparison(func, inst) {
                become_flipped(func, inst, &flip);
            }
            func[inst].first_result.expect("one result was asked for")
        }
    }
}

/// Turns a negation of a comparison into the opposite comparison, where it stands.
fn become_flipped(func: &mut Func, inst: Inst, flip: &Flip) {
    let args = func.push_values(&[flip.lhs, flip.rhs]);
    let data = &mut func[inst];
    data.opcode = flip.opcode;
    data.flags = flip.flags;
    data.args = args;
    data.extra = flip.extra;
}

/// Turns an instruction into the constant a rule says its result is.
///
/// In place, so the result value survives and every reader of it is already right. That is what
/// makes this the half of the pass with nothing to redirect.
fn become_constant(func: &mut Func, inst: Inst, number: i128) {
    let result = func[inst].first_result.expect("the rule matched a result");
    let ty = func[result].ty;
    let imm = func.add_imm(Imm::int(number, ty.lane()));
    let args = func.push_values(&[]);
    let data = &mut func[inst];
    data.opcode = Opcode::IConst;
    data.args = args;
    data.extra = Extra::Imm(imm);
    // The flags go with the instruction that had them. An `nsw` on an add is a promise about an
    // addition, and a constant makes no promise because it performs nothing.
    data.flags = Flags::NONE;
}

/// What an instruction should become, when it is a comparison written as a negation.
pub(crate) struct Flip {
    /// `ICmp` or `FCmp`, whichever the comparison underneath was.
    opcode: Opcode,
    /// The flags of the comparison, which is where a fast math promise lives.
    flags: Flags,
    /// The opposite predicate.
    extra: Extra,
    /// The comparison's left operand.
    lhs: Value,
    /// Its right operand.
    rhs: Value,
}

/// Whether this instruction is `xor (cmp p a b), true`, and what it becomes if it is.
///
/// The exclusive or is commutative, so the constant is looked for on both sides. Nothing else
/// about the shape is negotiable: the result has to be an `i1`, because an exclusive or with one
/// is a negation only at that width, and the constant has to be all ones, because the front end
/// writes it as `iconst.i1 -1` and a reader who assumed the literal 1 would match nothing.
fn negated_comparison(func: &Func, inst: Inst) -> Option<Flip> {
    let data = &func[inst];
    if data.opcode != Opcode::Xor {
        return None;
    }
    let args = &func[data.args];
    let (&first, &second) = (args.first()?, args.get(1)?);
    if func[first].ty != Type::int(1) {
        return None;
    }
    let cmp = match (all_ones(func, first), all_ones(func, second)) {
        (true, false) => second,
        (false, true) => first,
        // Both, which folding would have turned into a constant, or neither, which is an
        // exclusive or of two comparisons and is not this pattern.
        _ => return None,
    };
    let Def::Result { inst: cmp, .. } = func[cmp].def else { return None };
    let data = &func[cmp];
    let extra = match (data.opcode, data.extra) {
        (Opcode::ICmp, Extra::IntPred(pred)) => Extra::IntPred(pred.inverse()),
        (Opcode::FCmp, Extra::FloatPred(pred)) => Extra::FloatPred(pred.inverse()),
        _ => return None,
    };
    let args = &func[data.args];
    Some(Flip {
        opcode: data.opcode,
        flags: data.flags,
        extra,
        lhs: *args.first()?,
        rhs: *args.get(1)?,
    })
}

/// Where a pair of operands can stand in relation to each other, as one bit each.
///
/// Every comparison either of the IR's two families can make is a set of these and nothing else,
/// which is the whole idea. Two values are below, equal to or above one another, and two floating
/// point values can also be neither, so a predicate is a question about which of four buckets the
/// pair falls in and the answer is the subset it accepts. `olt` accepts one bucket, `ole` accepts
/// two, `une` accepts three and `uno` accepts the fourth on its own.
///
/// Once a predicate is a set, `&&` of two of them over the same pair of operands is the
/// intersection and `||` is the union, because the buckets do not overlap and exactly one of them
/// is the case. An empty answer is a combination nothing satisfies and a full one is a combination
/// everything does, which is what the two torture cases this is for are asking about.
mod bucket {
    /// The left operand is below the right one.
    pub(super) const LT: u8 = 1;
    /// The two are equal.
    pub(super) const EQ: u8 = 2;
    /// The left operand is above the right one.
    pub(super) const GT: u8 = 4;
    /// Neither, which only a floating point pair can be and only when one of them is a NaN.
    pub(super) const UN: u8 = 8;
    /// Every bucket an integer pair can be in, which is the answer no integer comparison can fail.
    pub(super) const ALL_INT: u8 = LT | EQ | GT;
    /// Every bucket a floating point pair can be in.
    pub(super) const ALL_FLOAT: u8 = LT | EQ | GT | UN;
}

/// Which ordering an integer predicate reads its operands under.
///
/// Equality is under neither, and that is not a technicality: `x == y` and `x < y` have an answer
/// in common whichever way the second one reads its operands, so an equality can be combined with
/// a signed comparison and with an unsigned one. Two orderings that disagree cannot be combined at
/// all, because `slt` and `ult` are not the same question and a set that mixed them would be a set
/// about no ordering in particular.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reading {
    /// The predicate compares signed.
    Signed,
    /// The predicate compares unsigned.
    Unsigned,
    /// The predicate is an equality and says nothing about an ordering.
    Neither,
}

impl Reading {
    /// The reading two predicates have in common, if they have one.
    const fn shared(self, other: Self) -> Option<Self> {
        match (self, other) {
            (Self::Neither, same) | (same, Self::Neither) => Some(same),
            (Self::Signed, Self::Signed) => Some(Self::Signed),
            (Self::Unsigned, Self::Unsigned) => Some(Self::Unsigned),
            (Self::Signed, Self::Unsigned) | (Self::Unsigned, Self::Signed) => None,
        }
    }
}

/// The buckets an integer predicate accepts, and the ordering it read them under.
const fn int_buckets(pred: IntPred) -> (u8, Reading) {
    use bucket::{EQ, GT, LT};
    match pred {
        IntPred::Eq => (EQ, Reading::Neither),
        IntPred::Ne => (LT | GT, Reading::Neither),
        IntPred::Slt => (LT, Reading::Signed),
        IntPred::Sle => (LT | EQ, Reading::Signed),
        IntPred::Sgt => (GT, Reading::Signed),
        IntPred::Sge => (GT | EQ, Reading::Signed),
        IntPred::Ult => (LT, Reading::Unsigned),
        IntPred::Ule => (LT | EQ, Reading::Unsigned),
        IntPred::Ugt => (GT, Reading::Unsigned),
        IntPred::Uge => (GT | EQ, Reading::Unsigned),
    }
}

/// The integer predicate that accepts exactly this set of buckets under this ordering.
///
/// Nothing for the empty set or the full one, which are the two answers that are not a comparison
/// at all and are dealt with before this is asked. Nothing either for a set that wants an ordering
/// from a pair that had none, which is a set neither `eq` nor `ne` can spell: two equalities
/// combine into an equality or into one of those two extremes and never into an ordering, so the
/// case does not arise and answering it would mean choosing an ordering out of nowhere.
const fn int_pred(buckets: u8, reading: Reading) -> Option<IntPred> {
    use bucket::{EQ, GT, LT};
    match (buckets, reading) {
        (EQ, _) => Some(IntPred::Eq),
        (b, _) if b == LT | GT => Some(IntPred::Ne),
        (LT, Reading::Signed) => Some(IntPred::Slt),
        (GT, Reading::Signed) => Some(IntPred::Sgt),
        (b, Reading::Signed) if b == LT | EQ => Some(IntPred::Sle),
        (b, Reading::Signed) if b == GT | EQ => Some(IntPred::Sge),
        (LT, Reading::Unsigned) => Some(IntPred::Ult),
        (GT, Reading::Unsigned) => Some(IntPred::Ugt),
        (b, Reading::Unsigned) if b == LT | EQ => Some(IntPred::Ule),
        (b, Reading::Unsigned) if b == GT | EQ => Some(IntPred::Uge),
        _ => None,
    }
}

/// The buckets a floating point predicate accepts.
///
/// The sixteen predicates are the sixteen subsets, which is why the IR has `false` and `true` among
/// them and why this direction and the one below are both total.
const fn float_buckets(pred: FloatPred) -> u8 {
    use bucket::{ALL_FLOAT, EQ, GT, LT, UN};
    match pred {
        FloatPred::False => 0,
        FloatPred::Oeq => EQ,
        FloatPred::Ogt => GT,
        FloatPred::Oge => GT | EQ,
        FloatPred::Olt => LT,
        FloatPred::Ole => LT | EQ,
        FloatPred::One => LT | GT,
        FloatPred::Ord => LT | EQ | GT,
        FloatPred::Uno => UN,
        FloatPred::Ueq => EQ | UN,
        FloatPred::Ugt => GT | UN,
        FloatPred::Uge => GT | EQ | UN,
        FloatPred::Ult => LT | UN,
        FloatPred::Ule => LT | EQ | UN,
        FloatPred::Une => LT | GT | UN,
        FloatPred::True => ALL_FLOAT,
    }
}

/// The floating point predicate that accepts exactly this set of buckets.
fn float_pred(buckets: u8) -> Option<FloatPred> {
    FloatPred::all().find(|pred| float_buckets(*pred) == buckets)
}

/// One of the two comparisons under an `and` or an `or`, read as a set of buckets.
struct Side {
    /// `ICmp` or `FCmp`, which both sides have to be the same of.
    opcode: Opcode,
    /// The flags, which both sides have to carry the same of. A fast math promise is a promise
    /// about one comparison, and a set built out of two comparisons that were not promised the
    /// same thing is a set under no promise in particular.
    flags: Flags,
    /// The buckets the predicate accepts, already turned round if the operands were.
    buckets: u8,
    /// Which ordering it read, for an integer comparison. Always [`Reading::Neither`] for a
    /// floating point one, where there is only the one ordering and nothing to agree about.
    reading: Reading,
    /// The left operand.
    lhs: Value,
    /// The right operand.
    rhs: Value,
}

/// The comparison a value holds the result of, if that is what it is.
fn side(func: &Func, value: Value) -> Option<Side> {
    let Def::Result { inst, .. } = func[value].def else { return None };
    let data = &func[inst];
    let (buckets, reading) = match (data.opcode, data.extra) {
        (Opcode::ICmp, Extra::IntPred(pred)) => int_buckets(pred),
        (Opcode::FCmp, Extra::FloatPred(pred)) => (float_buckets(pred), Reading::Neither),
        _ => return None,
    };
    let args = &func[data.args];
    Some(Side {
        opcode: data.opcode,
        flags: data.flags,
        buckets,
        reading,
        lhs: *args.first()?,
        rhs: *args.get(1)?,
    })
}

/// The same set of buckets read with the operands the other way round.
///
/// Which of the two is below the other changes places and nothing else moves: equal is equal from
/// both ends, and a NaN makes a pair unordered from both ends.
const fn turned(buckets: u8) -> u8 {
    use bucket::{GT, LT};
    let mut out = buckets & !(LT | GT);
    if buckets & LT != 0 {
        out |= GT;
    }
    if buckets & GT != 0 {
        out |= LT;
    }
    out
}

/// The second side read as though its operands were written in the first side's order.
///
/// A comparison is not commutative, so `y < x` is not `x < y`, it is `x > y`. Turning the second
/// side round is what lets `(x<y) && (y<x)` be a pair about one ordered pair of operands rather
/// than two unrelated comparisons, and it is the only case in either torture program that needs it.
fn aligned(first: &Side, second: Side) -> Option<Side> {
    if first.lhs == second.lhs && first.rhs == second.rhs {
        return Some(second);
    }
    if first.lhs != second.rhs || first.rhs != second.lhs {
        return None;
    }
    let buckets = turned(second.buckets);
    Some(Side { buckets, lhs: first.lhs, rhs: first.rhs, ..second })
}

/// What an `and` or an `or` of two comparisons over one pair of operands comes to.
pub(crate) enum Composite {
    /// Nothing the operands could hold makes it come out the other way.
    Always(bool),
    /// One comparison over the same pair says the same thing as the two together.
    Pred(Flip),
}

/// Whether this instruction is `and` or `or` of two comparisons over the same pair of operands,
/// and what it becomes if it is.
///
/// This is `(x==y) && (x!=y)`, which is false, and `(x>=y) || (x<y)`, which is true, and the four
/// other shapes `gcc.c-torture/execute/compare-3.c` is built out of. Neither program is contrived:
/// a composite condition written out of macros, or one arm of it produced by inlining, arrives
/// looking exactly like this, and the front end has already flattened the `&&` into an `and` by the
/// time anything here runs, so what would otherwise be a question about two blocks is a question
/// about one instruction and its two operands.
///
/// Both sides have to be the same family of comparison, carry the same flags and be about the same
/// two values. Past that the arithmetic is [`bucket`]: intersect for an `and`, union for an `or`,
/// and read the answer back as a predicate. An answer of no buckets is false, an answer of every
/// bucket is true, and anything between the two is one comparison where there were two, which is
/// worth taking on its own and is also what lets the three way condition in
/// `gcc.c-torture/execute/ieee/compare-fp-3.c` fold: the inner `or` becomes a single `uge` and the
/// outer one then has a pair to work on rather than an `or` and a comparison.
fn composite_comparison(func: &Func, inst: Inst) -> Option<Composite> {
    let data = &func[inst];
    if func[data.first_result?].ty != Type::int(1) {
        return None;
    }
    let args = &func[data.args];
    composite(func, data.opcode, *args.first()?, *args.get(1)?)
}

/// The same question asked about an `and` or an `or` that is not there yet.
///
/// [`crate::short_circuit`] asks it before it writes one, because whether the collapse it is
/// looking at is worth making is the question of whether what it writes survives this pass, and a
/// collapse that leaves one comparison where there were two and a branch is worth making at every
/// optimization level rather than only where speculating work is.
pub(crate) fn composite(func: &Func, opcode: Opcode, lhs: Value, rhs: Value) -> Option<Composite> {
    let intersect = match opcode {
        Opcode::And => true,
        Opcode::Or => false,
        _ => return None,
    };
    let first = side(func, lhs)?;
    let second = aligned(&first, side(func, rhs)?)?;
    if first.opcode != second.opcode || first.flags != second.flags {
        return None;
    }
    let reading = first.reading.shared(second.reading)?;
    let buckets = match intersect {
        true => first.buckets & second.buckets,
        false => first.buckets | second.buckets,
    };
    let whole = match first.opcode {
        Opcode::ICmp => bucket::ALL_INT,
        _ => bucket::ALL_FLOAT,
    };
    if buckets == 0 {
        return Some(Composite::Always(false));
    }
    if buckets == whole {
        return Some(Composite::Always(true));
    }
    let extra = match first.opcode {
        Opcode::ICmp => Extra::IntPred(int_pred(buckets, reading)?),
        _ => Extra::FloatPred(float_pred(buckets)?),
    };
    Some(Composite::Pred(Flip {
        opcode: first.opcode,
        flags: first.flags,
        extra,
        lhs: first.lhs,
        rhs: first.rhs,
    }))
}

/// Whether the sign bit of this value is known to be clear, which is what `fabs` leaves behind.
///
/// `fabs` is not a call by the time anything here runs. The front end knows the plain library name
/// as well as the prefixed one and lowers both to the bits, because there is nothing to call: the
/// magnitude of a value is that value with its sign bit cleared, payload and all for a NaN and sign
/// and all for a negative zero, and a rewriting into `x < 0 ? -x : x` would be wrong for both. So
/// what reaches this pass is a bitcast of an `and` of a bitcast, and the `and` is against a mask
/// whose top bit is clear.
///
/// Any such mask and not the one `fabs` writes. A constant with its top bit clear leaves the top
/// bit of the answer clear whatever the rest of it does, the top bit of an integer as wide as a
/// floating point value is that value's sign bit in every format the compiler has, and asking for
/// the exact mask would mean this stopped working the day a rule ahead of it narrowed one.
fn magnitude(func: &Func, value: Value) -> bool {
    let Def::Result { inst, .. } = func[value].def else { return false };
    let data = &func[inst];
    if data.opcode != Opcode::Bitcast {
        return false;
    }
    let Some(&bits) = func[data.args].first() else { return false };
    let Def::Result { inst: masked, .. } = func[bits].def else { return false };
    let data = &func[masked];
    if data.opcode != Opcode::And {
        return false;
    }
    func[data.args].iter().any(|&arg| clears_the_sign(func, arg))
}

/// Whether this value is an integer constant whose top bit is clear.
fn clears_the_sign(func: &Func, value: Value) -> bool {
    let ty = func[value].ty;
    let Def::Result { inst, .. } = func[value].def else { return false };
    let data = &func[inst];
    let Extra::Imm(at) = data.extra else { return false };
    data.opcode == Opcode::IConst && ty.is_int() && func[at].signed(ty) >= 0
}

/// The buckets a pair made of a magnitude on the left and this constant on the right can fall in.
///
/// A magnitude is a positive zero, a positive number, a positive infinity or a NaN, so against a
/// constant that is not positive it is never the one below. Against a negative constant it is never
/// the one equal either, since every value a magnitude can be is above every negative number.
///
/// Nothing for a constant that is positive, where the answer is every bucket and there would be
/// nothing to narrow, and nothing for a NaN, where [`Float::compare`] has no ordering to report and
/// the pair is unordered whatever the other side holds. [`bounded_comparison`] answers that one.
fn against(func: &Func, value: Value) -> Option<u8> {
    use bucket::{EQ, GT, UN};
    let number = float_constant(func, value)?;
    match number.compare(Float::zero(number.format(), false))? {
        Ordering::Less => Some(GT | UN),
        Ordering::Equal => Some(GT | EQ | UN),
        Ordering::Greater => None,
    }
}

/// Whether this comparison is one the sign bit of an operand settles, and what it becomes if it is.
///
/// `fabs (x) < 0.0` is false whatever `x` holds, including a NaN, and that is what
/// `gcc.c-torture/execute/20020720-1.c` asserts by calling a function it never defines. The
/// arithmetic is [`bucket`] again: a magnitude compared against a constant that is not positive
/// cannot be the one below, so the buckets the predicate accepts are narrowed by the ones the pair
/// can be in, and what is left is false, or is a shorter question than the one that was asked.
///
/// There is no answer of every bucket here, which is why this has no case for one. The narrowing
/// only ever takes buckets away and it always takes at least the one below, so a set that survives
/// it is never the full one and a comparison this fires on is never true.
///
/// A predicate the narrowing leaves alone is declined rather than rewritten, or the pass would
/// report having optimized `fabs (x) >= 0.0` into itself once per run until the fuel ran out.
fn magnitude_comparison(func: &Func, inst: Inst) -> Option<Composite> {
    let data = &func[inst];
    let Extra::FloatPred(pred) = data.extra else { return None };
    if data.opcode != Opcode::FCmp {
        return None;
    }
    let args = &func[data.args];
    let lhs = *args.first()?;
    let rhs = *args.get(1)?;
    let possible = if magnitude(func, lhs) {
        against(func, rhs)?
    } else if magnitude(func, rhs) {
        turned(against(func, lhs)?)
    } else {
        return None;
    };
    let asked = float_buckets(pred);
    let buckets = asked & possible;
    if buckets == asked {
        return None;
    }
    if buckets == 0 {
        return Some(Composite::Always(false));
    }
    Some(Composite::Pred(Flip {
        opcode: Opcode::FCmp,
        flags: data.flags,
        extra: Extra::FloatPred(float_pred(buckets)?),
        lhs,
        rhs,
    }))
}

/// A floating point comparison that a constant on one side settles, or the same value on both.
///
/// A NaN is unordered against everything, so `dnan < x` is false and `dnan != x` is true whatever
/// `x` holds, and `gcc.c-torture/execute/ieee/fp-cmp-6.c` and `fp-cmp-9.c` assert that of a NaN a
/// `const` global was given by calling a function they never define. Nothing is above a positive
/// infinity, so `x > __builtin_inf ()` is false, which is `fp-cmp-7.c`. Two constants are one
/// bucket, and a value against itself is equal or unordered and never below or above. gcc 16 folds
/// all of these without `-ffast-math`, since none of them depends on anything but the operands.
///
/// It is the narrowing [`magnitude_comparison`] does, over what these operands allow rather than
/// over what a magnitude does, and unlike that one it can come out true, since a NaN on either side
/// leaves the one bucket `une` and the other unordered predicates accept.
///
/// A branch in front of the comparison narrows it too, which is [`guarded`]. That is the seventh
/// test of `gcc.c-torture/execute/ieee/compare-fp-3.c`, `isunordered (x, y) || !isunordered (x,
/// y)`, at the levels that keep the `||` as two branches: the second comparison is only reached
/// where the first was false, so the pair is ordered there and `ord` is true.
fn bounded_comparison(func: &Func, cfg: &Cfg, inst: Inst) -> Option<Composite> {
    use bucket::{ALL_FLOAT, EQ, GT, LT, UN};
    let data = &func[inst];
    let Extra::FloatPred(pred) = data.extra else { return None };
    if data.opcode != Opcode::FCmp {
        return None;
    }
    let args = &func[data.args];
    let lhs = *args.first()?;
    let rhs = *args.get(1)?;
    let left = float_constant(func, lhs);
    let right = float_constant(func, rhs);
    let possible = match (left, right) {
        _ if left.is_some_and(Float::is_nan) || right.is_some_and(Float::is_nan) => UN,
        (Some(left), Some(right)) => match left.compare(right)? {
            Ordering::Less => LT,
            Ordering::Equal => EQ,
            Ordering::Greater => GT,
        },
        (None, Some(bound)) => past(bound).unwrap_or(ALL_FLOAT),
        (Some(bound), None) => turned(past(bound).unwrap_or(ALL_FLOAT)),
        (None, None) if lhs == rhs => EQ | UN,
        (None, None) => ALL_FLOAT,
    };
    let possible = possible & guarded(func, cfg, func.block_of(inst)?, lhs, rhs);
    if possible == ALL_FLOAT {
        return None;
    }
    let asked = float_buckets(pred);
    let buckets = asked & possible;
    if buckets == 0 {
        return Some(Composite::Always(false));
    }
    if buckets == possible {
        return Some(Composite::Always(true));
    }
    if buckets == asked {
        return None;
    }
    Some(Composite::Pred(Flip {
        opcode: Opcode::FCmp,
        flags: data.flags,
        extra: Extra::FloatPred(float_pred(buckets)?),
        lhs,
        rhs,
    }))
}

/// How many edges back [`guarded`] looks for a branch over the same pair.
const GUARDS: u32 = 8;

/// The buckets the branches in front of this block leave a pair of floating point operands in.
///
/// It walks back while the block has one predecessor, so every step is an edge the block can only
/// be reached along, and a branch there on a comparison of the same two operands says which of its
/// sides was taken. A block with one predecessor is never a loop header unless nothing reaches it,
/// so the operands are the same values at the branch as they are here.
fn guarded(func: &Func, cfg: &Cfg, block: Block, lhs: Value, rhs: Value) -> u8 {
    let mut possible = bucket::ALL_FLOAT;
    let mut at = block;
    for _ in 0..GUARDS {
        let &[from] = cfg.predecessors(at) else { break };
        if let Some(buckets) = edge(func, from, at, lhs, rhs) {
            possible &= buckets;
        }
        at = from;
    }
    possible
}

/// The buckets the edge from one block to the next leaves the pair in, when the first ends in a
/// branch on a floating point comparison of the same two operands and the two sides go to different
/// blocks.
fn edge(func: &Func, from: Block, to: Block, lhs: Value, rhs: Value) -> Option<u8> {
    let term = func.terminator(from)?;
    if func[term].opcode != Opcode::BrIf {
        return None;
    }
    let calls: Vec<_> = func.successors(term).collect();
    let (then, other) = (calls.first()?, calls.get(1)?);
    if then.block == other.block {
        return None;
    }
    let cond = *func[func[term].args].first()?;
    let Def::Result { inst, .. } = func[cond].def else { return None };
    let data = &func[inst];
    let Extra::FloatPred(pred) = data.extra else { return None };
    if data.opcode != Opcode::FCmp {
        return None;
    }
    let args = &func[data.args];
    let (&left, &right) = (args.first()?, args.get(1)?);
    let accepted = if then.block == to {
        float_buckets(pred)
    } else {
        bucket::ALL_FLOAT & !float_buckets(pred)
    };
    if (left, right) == (lhs, rhs) {
        Some(accepted)
    } else if (left, right) == (rhs, lhs) {
        Some(turned(accepted))
    } else {
        None
    }
}

/// The buckets a pair with this constant on the right can be in, when the constant is an infinity.
///
/// Nothing is above a positive infinity and nothing is below a negative one. Any other constant
/// leaves every bucket, which is nothing to narrow by.
fn past(bound: Float) -> Option<u8> {
    use bucket::{EQ, GT, LT, UN};
    if !bound.is_infinite() {
        return None;
    }
    Some(if bound.is_negative() { GT | EQ | UN } else { LT | EQ | UN })
}

/// The number a floating point constant holds.
fn float_constant(func: &Func, value: Value) -> Option<Float> {
    let Def::Result { inst, .. } = func[value].def else { return None };
    let data = &func[inst];
    if data.opcode != Opcode::FConst {
        return None;
    }
    let Extra::Imm(at) = data.extra else { return None };
    let format = func[value].ty.format()?.encoding();
    Some(Float::from_bits(format, func[at].bits()))
}

/// Writes what a set of buckets came to over the instruction it was worked out from.
///
/// In place, which keeps the result value, so every reader of that instruction is already reading
/// the one answer and whatever it used to read is left where it was for [`crate::dce`].
pub(crate) fn fold_composite(func: &mut Func, inst: Inst, composite: Composite) {
    match composite {
        Composite::Always(answer) => become_constant(func, inst, answer.into()),
        Composite::Pred(flip) => {
            let args = func.push_values(&[flip.lhs, flip.rhs]);
            let data = &mut func[inst];
            data.opcode = flip.opcode;
            data.flags = flip.flags;
            data.args = args;
            data.extra = flip.extra;
        }
    }
}

/// Whether this value is a constant with every bit of its type set.
fn all_ones(func: &Func, value: Value) -> bool {
    let ty = func[value].ty;
    let Def::Result { inst, .. } = func[value].def else { return false };
    let data = &func[inst];
    let Extra::Imm(at) = data.extra else { return false };
    if data.opcode != Opcode::IConst {
        return false;
    }
    // Read as signed, because an all ones value of any width is minus one that way and reading
    // it unsigned would need the width to build the mask from.
    func[at].signed(ty) == -1
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{
        Block, Builder, Extra, Flags, Float, FloatPred, Func, IntPred, Module, Opcode, Signature,
        Type, Value,
    };
    use rucc_target::{Arch, Env, Os, TargetInfo, Triple};

    use super::{
        CANONICAL, COMPARE, EXPAND, PLANS, SELECT, Shown, TABLES, canonical, compare, identities,
        select, strength, width,
    };
    use crate::rules::Piece;
    use crate::stats::Kind;
    use crate::{Fuel, Pass, simplify::Simplify};

    /// A function with one block, ready to have instructions appended to it.
    fn blank() -> (Interner, Func, Block) {
        let mut names = Interner::new();
        let name = names.intern("f");
        let mut func = Func::new(name, Signature::new().with_returns(&[Type::int(1)]));
        let block = func.create_block();
        (names, func, block)
    }

    /// The same, at the width the test is about and taking a parameter of it, since every identity
    /// below needs an operand that is not itself a constant.
    fn one_block(ty: Type) -> (Interner, Func, Block) {
        let mut names = Interner::new();
        let name = names.intern("f");
        let signature = Signature::new().with_params(&[ty]).with_returns(&[ty]);
        let mut func = Func::new(name, signature);
        let block = func.create_block();
        (names, func, block)
    }

    /// Runs the pass with as much fuel as it wants, and says whether it rewrote anything.
    fn simplify(func: &mut Func) -> bool {
        Simplify
            .run(func, &mut crate::machine::fixtures::analyses(), &mut Fuel::unlimited())
            .changed()
    }

    /// The opcode and the predicate the value now comes from.
    fn came_from(func: &Func, value: Value) -> (Opcode, Extra) {
        let rucc_ir::Def::Result { inst, .. } = func[value].def else { panic!("not a result") };
        (func[inst].opcode, func[inst].extra)
    }

    /// What the block gives back, which is where every identity test reads its answer. A rule
    /// that produces a value is only worth anything if the readers move, so the readers are what
    /// the test looks at rather than the instruction that fired.
    fn returned(func: &Func, block: Block) -> Value {
        let inst = func.terminator(block).expect("the block has a terminator");
        func[func[inst].args][0]
    }

    /// The operands of the instruction a value comes from.
    fn operands(func: &Func, value: Value) -> Vec<Value> {
        let rucc_ir::Def::Result { inst, .. } = func[value].def else { panic!("not a result") };
        func[func[inst].args].to_vec()
    }

    /// The number a value is, which panics unless it is a constant.
    fn number(func: &Func, value: Value) -> i128 {
        let rucc_ir::Def::Result { inst, .. } = func[value].def else { panic!("not a result") };
        let data = &func[inst];
        assert_eq!(data.opcode, Opcode::IConst, "not a constant");
        let Extra::Imm(at) = data.extra else { panic!("a constant with no number") };
        func[at].signed(func[value].ty)
    }

    /// Every rule in every table leaves one of the four shapes the pass knows how to apply.
    ///
    /// A rule that left anything else would be matched, found to be none of them, and skipped, and
    /// nothing at run time would say so: the rewrite would simply stop happening. So it is said
    /// here instead, once, over every table.
    #[test]
    fn every_rule_leaves_a_shape_the_pass_knows_what_to_do_with() {
        for (table, _) in TABLES {
            for rule in table.rules {
                let known = matches!(
                    rule.replacement,
                    [Piece::App { head, arity: 1 }, Piece::Var { .. }]
                        if head.starts_with("value.")
                ) || matches!(
                    rule.replacement,
                    [Piece::App { head, arity: 1 }, Piece::Int(_)]
                        if head.starts_with("iconst.")
                ) || matches!(
                    rule.replacement,
                    [Piece::App { arity: 2, .. }, ..] if instruction(rule.replacement)
                ) || conversion(rule.replacement);
                assert!(known, "{} leaves a shape the pass would skip", rule.pattern);
            }
        }
    }

    /// The pieces of a replacement that is a conversion, read the way [`super::converted`] reads
    /// them, and shape only for the same reason [`instruction`] is: there are no bindings here to
    /// resolve the operand against.
    fn conversion(pieces: &'static [Piece]) -> bool {
        let [Piece::App { head, arity: 1 }, rest @ ..] = pieces else { return false };
        let converts =
            matches!(super::opcode_of(head), Some(Opcode::SExt | Opcode::ZExt | Opcode::Trunc));
        let number = matches!(rest, [Piece::App { head, .. }, ..] if head.starts_with("iconst."));
        converts && !number && shape(rest).is_some_and(<[Piece]>::is_empty)
    }

    /// Every rule in the width table writes a term ending at the width the one it matched ended
    /// at.
    ///
    /// The pass rewrites in place and leaves the result type where it was, so a rule whose
    /// replacement converted to some other width would quietly produce a value of the wrong one.
    /// `rucc-verify` refuses a replacement narrower than what it replaces and says nothing about a
    /// wider one, so this is the half of that pair the solver does not cover.
    #[test]
    fn a_width_rule_writes_a_term_that_ends_where_the_one_it_matched_ended() {
        for rule in width::TABLE.rules {
            let [Piece::App { head, .. }, ..] = rule.replacement else {
                panic!("{} writes no head", rule.pattern)
            };
            let wrote = head.rsplit_once('.').expect("a replacement head names a width").1;
            let matched = rule
                .pattern
                .trim_start_matches('(')
                .split([' ', ')'])
                .next()
                .and_then(|head| head.rsplit_once('.'))
                .expect("a pattern head names a width")
                .1;
            assert_eq!(wrote, matched, "{} ends somewhere else", rule.pattern);
        }
    }

    /// The pieces of a replacement that is an instruction, read the way the pass reads them, so
    /// that the check above is the pass's own answer rather than a second opinion about it.
    ///
    /// The bindings are empty, which is why a `value.iN` operand fails to resolve and this only
    /// says the shape is one the pass would take rather than that it would take it here.
    fn instruction(pieces: &'static [Piece]) -> bool {
        let [Piece::App { head, arity: 2 }, rest @ ..] = pieces else { return false };
        if super::opcode_of(head).is_none() {
            return false;
        }
        shape(rest).and_then(shape).is_some_and(<[Piece]>::is_empty)
    }

    /// One operand of a replacement, read the way [`super::operand`] reads it, and the pieces
    /// after it. An instruction under the one a rule writes is an operand too, and its own
    /// operands are read the same way.
    fn shape(pieces: &'static [Piece]) -> Option<&'static [Piece]> {
        match pieces {
            [Piece::App { head, arity: 1 }, Piece::Var { .. }, rest @ ..]
                if head.starts_with("value.") =>
            {
                Some(rest)
            }
            [
                Piece::App { head, arity: 1 },
                Piece::Int(_) | Piece::Var { .. } | Piece::Computed { .. },
                rest @ ..,
            ] if head.starts_with("iconst.") => Some(rest),
            [Piece::App { head, arity }, rest @ ..]
                if super::opcode_of(head).is_some_and(|opcode| opcode != Opcode::IConst) =>
            {
                (0..*arity).try_fold(rest, |rest, _| shape(rest))
            }
            _ => None,
        }
    }

    /// And each table holds every rule its file writes. The tables are generated, so this is
    /// asking whether the generator saw the whole file, which is the one thing about it worth
    /// doubting.
    #[test]
    fn each_table_holds_every_rule_its_file_writes() {
        let tier_one = include_str!("../rules/simplify.rules");
        let tier_two = include_str!("../rules/strength.rules");
        let tier_three = include_str!("../rules/canonical.rules");
        let tier_four = include_str!("../rules/width.rules");
        let tier_five = include_str!("../rules/compare.rules");
        let tier_six = include_str!("../rules/select.rules");
        let count = |text: &str| text.matches("(rule (simplify ").count();
        assert_eq!(identities::TABLE.rules.len(), count(tier_one));
        assert_eq!(strength::TABLE.rules.len(), count(tier_two));
        assert_eq!(canonical::TABLE.rules.len(), count(tier_three));
        assert_eq!(width::TABLE.rules.len(), count(tier_four));
        assert_eq!(compare::TABLE.rules.len(), count(tier_five));
        assert_eq!(select::TABLE.rules.len(), count(tier_six));
        assert!(
            identities::TABLE.rules.len() > 100,
            "tier one is about a hundred rules and there are fewer"
        );
        assert!(
            strength::TABLE.rules.len() > 20,
            "tier two is the multiplications and the divisions and there are fewer"
        );
        assert_eq!(
            canonical::TABLE.rules.len(),
            20,
            "tier three is five commutative operators at four widths"
        );
        assert_eq!(
            width::TABLE.rules.len(),
            66,
            "tier four is the truncation and extension algebra over four widths, and the three \
             shapes of it that exist over the one bit a comparison answers in"
        );
        assert_eq!(
            compare::TABLE.rules.len(),
            72,
            "tier five is four predicates against each of four constants at four widths, and a \
             widened boolean against zero under two predicates at the same four"
        );
        assert_eq!(
            select::TABLE.rules.len(),
            32,
            "tier six is eight shapes of select at the four widths a select comes in"
        );
    }

    /// Three ways of showing an operand and no more, since a fourth would be a plan nothing
    /// tries and a rule written for it would never fire.
    #[test]
    fn a_pattern_is_reached_by_one_of_the_plans() {
        assert_eq!(PLANS.len(), 3);
    }

    /// Tier four is matched with its operand expanded, and none of the shared plans expands one.
    ///
    /// Every pattern in that tier has an instruction at its second level, so under any of the
    /// plans above it every rule in it would fail at the first node and the whole tier would be a
    /// file nobody matched with. Asserted rather than left to be read, because that failure is
    /// silent.
    ///
    /// Tier five expands as well, under the second of its own two plans, which is the half of it
    /// about a widened boolean compared against zero.
    #[test]
    fn a_width_rule_is_only_matched_with_its_operand_expanded() {
        let (_, plans) = TABLES[2];
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0], EXPAND[0]);
        assert_eq!(plans[0][0], Shown::Expand);
        for plan in PLANS {
            assert_ne!(plan, plans[0], "no shared plan expands an operand");
        }
        assert_ne!(CANONICAL[0], plans[0]);
        assert_eq!(COMPARE[1][0], Shown::Expand);
        assert_eq!(COMPARE[1][1], Shown::Const);
    }

    /// Tier three is matched under its own plan and no other.
    ///
    /// This is what makes the rules terminate rather than swap a pair of constants back and forth
    /// until the fuel runs out. It is asserted rather than left to be read, because the cost of
    /// somebody adding the shared plans to the tier three row is a pass that does not stop.
    #[test]
    fn a_canonicalisation_is_only_matched_with_the_right_operand_refused() {
        let (_, plans) = TABLES[5];
        assert_eq!(plans.len(), 1);
        assert_eq!(plans[0], CANONICAL[0]);
        assert_eq!(plans[0][1], Shown::Var);
        for plan in PLANS {
            assert_ne!(plan, plans[0], "a shared plan would let a canonicalisation cycle");
        }
    }

    /// Tier five is matched with the constant on the right and no other way.
    ///
    /// Every rule in it writes the constant there, so under the plan that shows a constant left
    /// operand as a number none of them would match and under the plan that refuses a constant on
    /// the right none of them would either. Two plans, differing only in how the left operand is
    /// shown, which is what the two halves of the tier are about.
    #[test]
    fn a_comparison_rule_is_only_matched_with_the_constant_on_the_right() {
        let (_, plans) = TABLES[3];
        assert_eq!(plans.len(), 2);
        assert_eq!(plans, COMPARE);
        for plan in plans {
            assert_eq!(plan[1], Shown::Const);
        }
        assert_eq!(plans[0][0], Shown::Reg);
        assert_eq!(plans[1][0], Shown::Expand);
    }

    /// Tier six is matched with the arms as numbers, and then with one arm expanded at a time.
    ///
    /// The condition is a register under every plan, since every rule in the tier binds it as a
    /// value. Expanding both arms at once would match nothing in the tier, because the arm that is
    /// not expanded is the value the other was computed from and only two registers can be said to
    /// be the same value.
    #[test]
    fn a_select_rule_is_matched_with_one_arm_expanded_at_a_time() {
        let (_, plans) = TABLES[4];
        assert_eq!(plans, SELECT);
        for plan in plans {
            assert_eq!(plan[0], Shown::Reg);
            assert!(plan[1] != Shown::Expand || plan[2] != Shown::Expand);
        }
    }

    /// A function of two numbers at a width that compares them with `slt` and hands the answer to
    /// `arms`, which builds a select from it and returns what to return.
    fn selecting(
        width: u32,
        arms: impl FnOnce(&mut Builder<'_>, Value, Value) -> Value,
    ) -> (Func, Block, Value, Value) {
        let ty = Type::int(width);
        let (_, mut func, block) = blank();
        let x = func.append_param(block, ty);
        let y = func.append_param(block, ty);
        let mut build = Builder::new(&mut func, block);
        let cmp = build.icmp(IntPred::Slt, x, y);
        let out = arms(&mut build, cmp, x);
        build.ret(&[out]);
        (func, block, x, y)
    }

    /// The comparison a value was widened from, with its predicate and its two operands.
    fn widened(func: &Func, value: Value) -> (IntPred, Vec<Value>) {
        assert_eq!(came_from(func, value).0, Opcode::ZExt);
        let bit = operands(func, value)[0];
        let (opcode, extra) = came_from(func, bit);
        assert_eq!(opcode, Opcode::ICmp);
        let Extra::IntPred(pred) = extra else { panic!("a comparison with no predicate") };
        (pred, operands(func, bit))
    }

    /// `a < b ? 1 : 0` is the comparison widened, at every width, and `a < b ? 0 : 1` is the
    /// opposite comparison widened, with no exclusive or left between the two.
    #[test]
    fn a_select_between_one_and_zero_is_the_comparison_widened() {
        for width in [8u32, 16, 32, 64] {
            let ty = Type::int(width);
            for (then, other, pred) in [(1, 0, IntPred::Slt), (0, 1, IntPred::Sge)] {
                let (mut func, block, x, y) = selecting(width, |build, cmp, _| {
                    let then = build.iconst(ty, then);
                    let other = build.iconst(ty, other);
                    build.select(cmp, then, other)
                });
                assert!(simplify(&mut func), "i{width} {then} {other} was left alone");
                let got = returned(&func, block);
                assert_eq!(func[got].ty, ty);
                assert_eq!(widened(&func, got), (pred, vec![x, y]), "i{width} {then} {other}");
            }
        }
    }

    /// A condition that is not a comparison has nothing to flip, and the exclusive or stays.
    #[test]
    fn a_select_between_zero_and_one_on_a_bit_is_the_bit_negated_and_widened() {
        let (_, mut func, block) = blank();
        let bit = func.append_param(block, Type::int(1));
        let mut build = Builder::new(&mut func, block);
        let zero = build.iconst(Type::int(32), 0);
        let one = build.iconst(Type::int(32), 1);
        let out = build.select(bit, zero, one);
        build.ret(&[out]);
        assert!(simplify(&mut func));
        let got = returned(&func, block);
        assert_eq!(came_from(&func, got).0, Opcode::ZExt);
        let negated = operands(&func, got)[0];
        assert_eq!(came_from(&func, negated).0, Opcode::Xor);
        let args = operands(&func, negated);
        assert_eq!(args[0], bit);
        assert_eq!(func[args[1]].ty, Type::int(1));
    }

    /// `a < b ? -1 : 0` is nothing less the comparison widened, and `a < b ? 0 : -1` is the
    /// comparison widened less one.
    #[test]
    fn a_select_between_minus_one_and_zero_is_the_comparison_widened_and_moved() {
        for width in [8u32, 16, 32, 64] {
            let ty = Type::int(width);
            for (then, other, opcode) in [(-1, 0, Opcode::Sub), (0, -1, Opcode::Add)] {
                let (mut func, block, x, y) = selecting(width, |build, cmp, _| {
                    let then = build.iconst(ty, then);
                    let other = build.iconst(ty, other);
                    build.select(cmp, then, other)
                });
                assert!(simplify(&mut func), "i{width} {then} {other} was left alone");
                let got = returned(&func, block);
                assert_eq!(came_from(&func, got).0, opcode, "i{width} {then} {other}");
                let args = operands(&func, got);
                let (number_at, widened_at) = if opcode == Opcode::Sub { (0, 1) } else { (1, 0) };
                assert_eq!(
                    number(&func, args[number_at]),
                    if opcode == Opcode::Sub { 0 } else { -1 }
                );
                assert_eq!(func[args[number_at]].ty, ty);
                assert_eq!(widened(&func, args[widened_at]), (IntPred::Slt, vec![x, y]));
            }
        }
    }

    /// `a < b ? x + 1 : x` is `x` plus the comparison, and `a < b ? x - 1 : x` is `x` less it,
    /// and with the arms the other way round the comparison is the opposite one.
    #[test]
    fn a_select_between_a_value_and_one_step_from_it_is_the_value_moved_by_the_comparison() {
        for width in [8u32, 16, 32, 64] {
            let ty = Type::int(width);
            for step in [Opcode::Add, Opcode::Sub] {
                for stepped_first in [true, false] {
                    let (mut func, block, x, y) = selecting(width, |build, cmp, x| {
                        let one = build.iconst(ty, 1);
                        let stepped = build.binary(step, x, one, Flags::NSW);
                        if stepped_first {
                            build.select(cmp, stepped, x)
                        } else {
                            build.select(cmp, x, stepped)
                        }
                    });
                    let case = format!("i{width} {step:?} first {stepped_first}");
                    assert!(simplify(&mut func), "{case} was left alone");
                    let got = returned(&func, block);
                    assert_eq!(came_from(&func, got).0, step, "{case}");
                    // What the select chose between made no promise the new instruction keeps.
                    let rucc_ir::Def::Result { inst, .. } = func[got].def else { panic!() };
                    assert_eq!(func[inst].flags, Flags::NONE, "{case}");
                    let args = operands(&func, got);
                    assert_eq!(args[0], x, "{case}");
                    let pred = if stepped_first { IntPred::Slt } else { IntPred::Sge };
                    assert_eq!(widened(&func, args[1]), (pred, vec![x, y]), "{case}");
                }
            }
        }
    }

    /// A step of two is not one step, and the select is left for the back end.
    #[test]
    fn a_select_between_a_value_and_two_more_is_left_alone() {
        let (mut func, block, _, _) = selecting(32, |build, cmp, x| {
            let two = build.iconst(Type::int(32), 2);
            let stepped = build.binary(Opcode::Add, x, two, Flags::NONE);
            build.select(cmp, stepped, x)
        });
        simplify(&mut func);
        let got = returned(&func, block);
        assert_eq!(came_from(&func, got).0, Opcode::Select);
    }

    /// The edge of a type, at each width, read each way.
    ///
    /// The least and greatest unsigned value and the least and greatest signed one, which are the
    /// four constants tier five is written against.
    fn edges(width: u32) -> [(i128, bool); 4] {
        let signed = 1i128 << (width - 1);
        [(0, false), (-1, false), (-signed, true), (signed - 1, true)]
    }

    /// A comparison that its type has already answered becomes the answer.
    ///
    /// Nothing unsigned is below zero, everything unsigned is at least zero, and the same pair of
    /// sentences holds at each of the other three edges. Thirty two rules, run as one test,
    /// because what is being checked is the same sentence at four constants and four widths.
    #[test]
    fn a_comparison_against_the_edge_of_its_type_folds_to_a_bit() {
        for width in [8u32, 16, 32, 64] {
            let ty = Type::int(width);
            for (edge, signed) in edges(width) {
                // Below the edge is false at the bottom and above it is false at the top, and the
                // other of each pair is the negation, so one table gives all four.
                let below = edge == 0 || edge == -(1i128 << (width - 1));
                let (false_pred, true_pred) = match (signed, below) {
                    (false, true) => (IntPred::Ult, IntPred::Uge),
                    (false, false) => (IntPred::Ugt, IntPred::Ule),
                    (true, true) => (IntPred::Slt, IntPred::Sge),
                    (true, false) => (IntPred::Sgt, IntPred::Sle),
                };
                // Minus one for the true bit, because the rule writes `(iconst.i1 1)` and one bit
                // holding a one read signed is minus one, which is the same bit pattern and the
                // reading everything else in the compiler takes of a true condition.
                for (pred, answer) in [(false_pred, 0), (true_pred, -1)] {
                    let (_, mut func, block) = blank();
                    let x = func.append_param(block, ty);
                    let mut build = Builder::new(&mut func, block);
                    let bound = build.iconst(ty, edge);
                    let cmp = build.icmp(pred, x, bound);
                    build.ret(&[cmp]);
                    assert!(simplify(&mut func), "i{width} {pred:?} {edge} was left alone");
                    let got = returned(&func, block);
                    assert_eq!(
                        came_from(&func, got).0,
                        Opcode::IConst,
                        "i{width} {pred:?} {edge} did not fold"
                    );
                    assert_eq!(number(&func, got), answer, "i{width} {pred:?} {edge}");
                    assert_eq!(func[got].ty, Type::int(1), "i{width} {pred:?} {edge} is a bit");
                }
            }
        }
    }

    /// And one that is true or false for exactly one value becomes the test for that value.
    ///
    /// The predicate has to come from the rule. Every case here matched an ordering and every one
    /// of them has to leave `eq` or `ne`, so a rewriter that took the predicate from the
    /// instruction it replaced would leave the ordering in place and this would say so.
    #[test]
    fn a_comparison_true_for_one_value_becomes_a_test_for_that_value() {
        for width in [8u32, 16, 32, 64] {
            let ty = Type::int(width);
            for (edge, signed) in edges(width) {
                let below = edge == 0 || edge == -(1i128 << (width - 1));
                // At most the bottom is equality and above it is inequality, and at the top the
                // two swap over.
                let (eq_pred, ne_pred) = match (signed, below) {
                    (false, true) => (IntPred::Ule, IntPred::Ugt),
                    (false, false) => (IntPred::Uge, IntPred::Ult),
                    (true, true) => (IntPred::Sle, IntPred::Sgt),
                    (true, false) => (IntPred::Sge, IntPred::Slt),
                };
                for (pred, left) in [(eq_pred, IntPred::Eq), (ne_pred, IntPred::Ne)] {
                    let (_, mut func, block) = blank();
                    let x = func.append_param(block, ty);
                    let mut build = Builder::new(&mut func, block);
                    let bound = build.iconst(ty, edge);
                    let cmp = build.icmp(pred, x, bound);
                    build.ret(&[cmp]);
                    assert!(simplify(&mut func), "i{width} {pred:?} {edge} was left alone");
                    let got = returned(&func, block);
                    assert_eq!(
                        came_from(&func, got),
                        (Opcode::ICmp, Extra::IntPred(left)),
                        "i{width} {pred:?} {edge} kept the predicate it matched"
                    );
                    let args = operands(&func, got);
                    assert_eq!(args[0], x, "i{width} {pred:?} {edge} lost its value");
                    assert_eq!(number(&func, args[1]), edge, "i{width} {pred:?} {edge}");
                    // The width the rule was written at, which is the width of what is being
                    // compared and not the width of the answer. A constant built at the result's
                    // type would be a one bit zero standing where a wider one was asked for.
                    assert_eq!(func[args[1]].ty, ty, "i{width} {pred:?} {edge} narrowed its bound");
                }
            }
        }
    }

    /// A boolean widened and compared against zero is the boolean.
    ///
    /// The shape `if (flag)` and `(long)(a == b)` and every `__builtin_expect` arrive in, since
    /// each of them widens a comparison and then asks whether the wide value is zero. What the
    /// test asserts is that the branch ends up on the comparison itself, at one bit, with the
    /// widening left for dead code elimination.
    #[test]
    fn a_widened_boolean_compared_against_zero_is_the_boolean() {
        for width in [8u32, 16, 32, 64] {
            let ty = Type::int(width);
            let (_, mut func, block) = blank();
            let x = func.append_param(block, Type::int(32));
            let mut build = Builder::new(&mut func, block);
            let seven = build.iconst(Type::int(32), 7);
            let flag = build.icmp(IntPred::Eq, x, seven);
            let wide = build.unary(Opcode::ZExt, flag, ty);
            let zero = build.iconst(ty, 0);
            let test = build.icmp(IntPred::Ne, wide, zero);
            build.ret(&[test]);
            assert!(simplify(&mut func), "i{width} was left alone");
            let got = returned(&func, block);
            assert_eq!(got, flag, "i{width} did not end up on the comparison");
            assert_eq!(func[got].ty, Type::int(1), "i{width} is a bit");
        }
    }

    /// And one compared against zero the other way is that boolean negated.
    ///
    /// The rule writes an exclusive or with a one bit one, because what is under the widening is
    /// whatever produced the bit and there is no predicate to flip in the general case. Where it
    /// is a comparison, which is this test, the hand written rewrite above the tables turns that
    /// exclusive or into the opposite comparison, and the pair composes into one instruction.
    ///
    /// Two runs, because the walk visits each instruction once and the hand written rewrite is
    /// tried before the tables are: the exclusive or did not exist when this instruction was
    /// looked at. Every pipeline above `-O0` names the pass twice, which is where the second run
    /// comes from in a real compile.
    #[test]
    fn a_widened_boolean_that_is_zero_is_the_boolean_negated() {
        for width in [8u32, 16, 32, 64] {
            let ty = Type::int(width);
            let (_, mut func, block) = blank();
            let x = func.append_param(block, Type::int(32));
            let mut build = Builder::new(&mut func, block);
            let seven = build.iconst(Type::int(32), 7);
            let flag = build.icmp(IntPred::Eq, x, seven);
            let wide = build.unary(Opcode::ZExt, flag, ty);
            let zero = build.iconst(ty, 0);
            let test = build.icmp(IntPred::Eq, wide, zero);
            build.ret(&[test]);
            assert!(simplify(&mut func), "i{width} was left alone");
            let got = returned(&func, block);
            assert_eq!(came_from(&func, got).0, Opcode::Xor, "i{width} is not a negation");
            assert!(simplify(&mut func), "i{width} kept the exclusive or");
            assert_eq!(
                came_from(&func, got),
                (Opcode::ICmp, Extra::IntPred(IntPred::Ne)),
                "i{width} did not come out as the opposite comparison"
            );
            let args = operands(&func, got);
            assert_eq!(args[0], x, "i{width} lost its value");
            assert_eq!(number(&func, args[1]), 7, "i{width} lost its bound");
        }
    }

    /// Every commutative operator tier three writes moves its constant to the right.
    ///
    /// One test over the five rather than five tests, because what is being checked is the same
    /// thing five times and the operator is the only part that differs.
    #[test]
    fn a_constant_on_the_left_of_a_commutative_operation_moves_to_the_right() {
        for opcode in [Opcode::Add, Opcode::Mul, Opcode::And, Opcode::Or, Opcode::Xor] {
            for width in [8, 16, 32, 64] {
                let ty = Type::int(width);
                let (_, mut func, block) = one_block(ty);
                let x = func.append_param(block, ty);
                let mut build = Builder::new(&mut func, block);
                // Three, because it is a number no identity in tier one is about and no strength
                // reduction in tier two is about, so the only rule that can fire is the one this
                // test is here for.
                let three = build.iconst(ty, 3);
                let value = build.binary(opcode, three, x, Flags::NONE);
                build.ret(&[value]);
                assert!(simplify(&mut func), "{opcode:?} at i{width} was left alone");
                let args = operands(&func, returned(&func, block));
                assert_eq!(came_from(&func, returned(&func, block)).0, opcode);
                assert_eq!(args[0], x, "{opcode:?} at i{width} kept the value on the right");
                assert_eq!(number(&func, args[1]), 3, "{opcode:?} at i{width} lost its constant");
            }
        }
    }

    /// And an operation whose operands are both constants is left where it is.
    ///
    /// This is the termination argument, run rather than read. Without the plan that refuses a
    /// constant on the right, the rule above would match this, swap the two, match the swapped
    /// form, and go on doing it until the fuel ran out. Folding is what this instruction is for
    /// and `crate::fold` is where it happens.
    #[test]
    fn an_operation_on_two_constants_is_not_swapped_back_and_forth() {
        let i32 = Type::int(32);
        let (_, mut func, block) = one_block(i32);
        let mut build = Builder::new(&mut func, block);
        let three = build.iconst(i32, 3);
        let five = build.iconst(i32, 5);
        let sum = build.binary(Opcode::Add, three, five, Flags::NONE);
        build.ret(&[sum]);
        assert!(!simplify(&mut func), "the constants were rearranged rather than left to folding");
        let args = operands(&func, returned(&func, block));
        assert_eq!(number(&func, args[0]), 3);
        assert_eq!(number(&func, args[1]), 5);
    }

    /// A constant already on the right stays there and nothing fires.
    ///
    /// The other half of the same argument. A canonicalisation that fired on the shape it produces
    /// would be a canonicalisation with no direction, which is what section 13.5 refuses.
    #[test]
    fn a_constant_already_on_the_right_is_left_alone() {
        let i32 = Type::int(32);
        let (_, mut func, block) = one_block(i32);
        let x = func.append_param(block, i32);
        let mut build = Builder::new(&mut func, block);
        let three = build.iconst(i32, 3);
        let sum = build.binary(Opcode::Add, x, three, Flags::NONE);
        build.ret(&[sum]);
        assert!(!simplify(&mut func));
        let args = operands(&func, returned(&func, block));
        assert_eq!(args[0], x);
        assert_eq!(number(&func, args[1]), 3);
    }

    /// A subtraction is not commutative and nothing moves its constant.
    ///
    /// Turning `c - x` into anything is not what tier three does, and the rules are written per
    /// opcode rather than over a set of them, so this is asking whether the wrong opcode found its
    /// way into the file.
    #[test]
    fn a_subtraction_keeps_its_operands_where_they_are() {
        let i32 = Type::int(32);
        let (_, mut func, block) = one_block(i32);
        let x = func.append_param(block, i32);
        let mut build = Builder::new(&mut func, block);
        let three = build.iconst(i32, 3);
        let difference = build.binary(Opcode::Sub, three, x, Flags::NONE);
        build.ret(&[difference]);
        assert!(!simplify(&mut func));
        let args = operands(&func, returned(&func, block));
        assert_eq!(number(&func, args[0]), 3);
        assert_eq!(args[1], x);
    }

    /// A block whose parameter and whose result are different widths, which is what every width
    /// rule needs and what `one_block` cannot give.
    fn narrow_to_wide(takes: Type, gives: Type) -> (Interner, Func, Block) {
        let mut names = Interner::new();
        let name = names.intern("f");
        let signature = Signature::new().with_params(&[takes]).with_returns(&[gives]);
        let mut func = Func::new(name, signature);
        let block = func.create_block();
        (names, func, block)
    }

    /// A conversion of a conversion of a parameter, which is the shape every width rule matches.
    ///
    /// The parameter is at `from`, the inner conversion takes it to `through` and the outer one
    /// takes that to `to`, and what comes back is the function, the block and the parameter.
    fn chain(
        inner: Opcode,
        outer: Opcode,
        from: Type,
        through: Type,
        to: Type,
    ) -> (Func, Block, Value) {
        let (_, mut func, block) = narrow_to_wide(from, to);
        let x = func.append_param(block, from);
        let mut build = Builder::new(&mut func, block);
        let middle = build.unary(inner, x, through);
        let outside = build.unary(outer, middle, to);
        build.ret(&[outside]);
        (func, block, x)
    }

    /// Truncating an extension back to the width it came from is the value that was there.
    ///
    /// Every pair of widths and both extensions, because the rule file writes all twelve and a
    /// test of one of them would say nothing about the other eleven.
    #[test]
    fn truncating_an_extension_back_to_its_own_width_gives_the_value_back() {
        for extend in [Opcode::SExt, Opcode::ZExt] {
            for (narrow, wide) in [(8, 16), (8, 32), (8, 64), (16, 32), (16, 64), (32, 64)] {
                let (from, through) = (Type::int(narrow), Type::int(wide));
                let (mut func, block, x) = chain(extend, Opcode::Trunc, from, through, from);
                assert!(simplify(&mut func), "{extend:?} i{narrow} to i{wide} was left alone");
                assert_eq!(
                    returned(&func, block),
                    x,
                    "{extend:?} i{narrow} to i{wide} and back did not give the value back"
                );
            }
        }
    }

    /// Truncating an extension to a width still above the source is the same extension, stopping
    /// earlier.
    #[test]
    fn truncating_an_extension_above_its_source_is_a_shorter_extension() {
        let (mut func, block, x) =
            chain(Opcode::SExt, Opcode::Trunc, Type::int(8), Type::int(64), Type::int(16));
        assert!(simplify(&mut func));
        let result = returned(&func, block);
        assert_eq!(came_from(&func, result).0, Opcode::SExt);
        assert_eq!(operands(&func, result), vec![x]);
        assert_eq!(func[result].ty, Type::int(16));
    }

    /// Truncating an extension to a width below the source is a truncation of the source, and
    /// which extension it was never mattered.
    #[test]
    fn truncating_an_extension_below_its_source_is_a_truncation_of_the_source() {
        let (mut func, block, x) =
            chain(Opcode::ZExt, Opcode::Trunc, Type::int(16), Type::int(32), Type::int(8));
        assert!(simplify(&mut func));
        let result = returned(&func, block);
        assert_eq!(came_from(&func, result).0, Opcode::Trunc);
        assert_eq!(operands(&func, result), vec![x]);
        assert_eq!(func[result].ty, Type::int(8));
    }

    /// An extension of an extension is one extension, and a sign extension of a zero extension is
    /// a zero extension rather than a sign extension.
    #[test]
    fn an_extension_of_an_extension_is_one_extension() {
        for (inner, outer, want) in [
            (Opcode::ZExt, Opcode::ZExt, Opcode::ZExt),
            (Opcode::SExt, Opcode::SExt, Opcode::SExt),
            (Opcode::ZExt, Opcode::SExt, Opcode::ZExt),
        ] {
            let (mut func, block, x) =
                chain(inner, outer, Type::int(8), Type::int(16), Type::int(64));
            assert!(simplify(&mut func), "{outer:?} of {inner:?} was left alone");
            let result = returned(&func, block);
            assert_eq!(came_from(&func, result).0, want, "{outer:?} of {inner:?}");
            assert_eq!(operands(&func, result), vec![x]);
            assert_eq!(func[result].ty, Type::int(64));
        }
    }

    /// A truncation of a truncation is one truncation, straight to the width the outer one asked
    /// for.
    ///
    /// The inner one threw away bits the outer one was going to throw away as well, so the width
    /// in the middle was never read and the rule goes to the outer width from the source. Both
    /// orderings of the three widths are tried, because a rule that picked the middle width rather
    /// than the outer one would still pass a test that only went from sixty four to eight through
    /// thirty two.
    #[test]
    fn a_truncation_of_a_truncation_is_one_truncation() {
        for (from, through, to) in [(64u32, 32u32, 16u32), (64, 32, 8), (64, 16, 8), (32, 16, 8)] {
            let (mut func, block, x) = chain(
                Opcode::Trunc,
                Opcode::Trunc,
                Type::int(from),
                Type::int(through),
                Type::int(to),
            );
            assert!(simplify(&mut func), "i{from} to i{through} to i{to} was left alone");
            let result = returned(&func, block);
            assert_eq!(came_from(&func, result).0, Opcode::Trunc, "i{from} to i{through} to i{to}");
            assert_eq!(operands(&func, result), vec![x]);
            assert_eq!(func[result].ty, Type::int(to));
        }
    }

    /// And zero extending a sign extension is not one, because the bits the sign extension copied
    /// are bits of the value now and nothing above them is a function of the source alone.
    #[test]
    fn zero_extending_a_sign_extension_is_left_alone() {
        let (mut func, _, _) =
            chain(Opcode::SExt, Opcode::ZExt, Type::int(8), Type::int(16), Type::int(64));
        assert!(!simplify(&mut func), "a zero extension of a sign extension was rewritten");
    }

    /// And zero extending a truncation is left alone, which is the rule the tier would be expected
    /// to have and does not.
    ///
    /// It was written and proved and then measured, and the measurement is why it went: the
    /// machine has one instruction for the pair already, the `and` with an immediate that replaced
    /// it is the longer encoding of the two, and the mask hides the narrowing from
    /// [`crate::narrow`]. The rule file says the whole of it. This is here so that somebody adding
    /// it back finds a test rather than a silence.
    #[test]
    fn zero_extending_a_truncation_is_left_alone() {
        let (mut func, _, _) =
            chain(Opcode::Trunc, Opcode::ZExt, Type::int(64), Type::int(32), Type::int(64));
        assert!(!simplify(&mut func), "a zero extension of a truncation became a mask");
    }

    /// A width rule needs an operand something computed, and a parameter is not one.
    ///
    /// This is what the plan being an expanding one means at the bottom: there is no instruction
    /// under the operand to be the second level of the pattern, so nothing matches and nothing is
    /// rewritten. Said out loud because it is the case that would otherwise be a crash rather than
    /// a miss.
    #[test]
    fn a_width_rule_needs_an_operand_an_instruction_computed() {
        let (_, mut func, block) = narrow_to_wide(Type::int(64), Type::int(32));
        let x = func.append_param(block, Type::int(64));
        let mut build = Builder::new(&mut func, block);
        let narrowed = build.unary(Opcode::Trunc, x, Type::int(32));
        build.ret(&[narrowed]);
        assert!(!simplify(&mut func), "a truncation of a parameter was rewritten");
    }

    #[test]
    fn adding_nothing_points_every_reader_at_the_operand() {
        let i32 = Type::int(32);
        let (_, mut func, block) = one_block(i32);
        let x = func.append_param(block, i32);
        let mut build = Builder::new(&mut func, block);
        let zero = build.iconst(i32, 0);
        let sum = build.binary(Opcode::Add, x, zero, Flags::NONE);
        build.ret(&[sum]);
        assert!(simplify(&mut func));
        // The `add` is still there, used by nothing, which is what dead code elimination is for.
        assert_eq!(returned(&func, block), x);
        assert_eq!(came_from(&func, sum).0, Opcode::Add);
    }

    /// The constant on either side, since nothing puts it on the right yet and a rule written one
    /// way round would fire on half the additions it should.
    #[test]
    fn the_constant_is_found_on_either_side_of_an_identity() {
        for swapped in [false, true] {
            let i32 = Type::int(32);
            let (_, mut func, block) = one_block(i32);
            let x = func.append_param(block, i32);
            let mut build = Builder::new(&mut func, block);
            let zero = build.iconst(i32, 0);
            let (lhs, rhs) = if swapped { (zero, x) } else { (x, zero) };
            let sum = build.binary(Opcode::Add, lhs, rhs, Flags::NONE);
            build.ret(&[sum]);
            assert!(simplify(&mut func), "swapped {swapped}");
            assert_eq!(returned(&func, block), x, "swapped {swapped}");
        }
    }

    #[test]
    fn multiplying_by_nothing_becomes_the_constant_where_it_stands() {
        let i32 = Type::int(32);
        let (_, mut func, block) = one_block(i32);
        let x = func.append_param(block, i32);
        let mut build = Builder::new(&mut func, block);
        let zero = build.iconst(i32, 0);
        let product = build.binary(Opcode::Mul, x, zero, Flags::NONE);
        build.ret(&[product]);
        assert!(simplify(&mut func));
        // The result value survives, which is the whole reason this half rewrites in place.
        assert_eq!(returned(&func, block), product);
        assert_eq!(came_from(&func, product).0, Opcode::IConst);
        assert_eq!(number(&func, product), 0);
    }

    /// The two identities a pattern that writes one name twice exists for, at every width they
    /// are written at.
    #[test]
    fn a_value_against_itself() {
        for bits in [8, 16, 32, 64] {
            let ty = Type::int(bits);
            let (_, mut func, block) = one_block(ty);
            let x = func.append_param(block, ty);
            let mut build = Builder::new(&mut func, block);
            let both = build.binary(Opcode::And, x, x, Flags::NONE);
            build.ret(&[both]);
            assert!(simplify(&mut func), "{bits} bits");
            assert_eq!(returned(&func, block), x, "{bits} bits");

            let (_, mut func, block) = one_block(ty);
            let x = func.append_param(block, ty);
            let mut build = Builder::new(&mut func, block);
            let nothing = build.binary(Opcode::Sub, x, x, Flags::NONE);
            build.ret(&[nothing]);
            assert!(simplify(&mut func), "{bits} bits");
            assert_eq!(number(&func, nothing), 0, "{bits} bits");
        }
    }

    /// Every predicate with one name in both operands, at every width the rules are written at.
    /// Six of the ten are true and four are false, and not one of them had to look at what the
    /// operand holds.
    #[test]
    fn every_comparison_of_a_value_with_itself_is_decided() {
        for bits in [8, 16, 32, 64] {
            for pred in IntPred::all() {
                let mut names = Interner::new();
                let name = names.intern("f");
                let int = Type::int(bits);
                let signature = Signature::new().with_params(&[int]).with_returns(&[Type::int(1)]);
                let mut func = Func::new(name, signature);
                let block = func.create_block();
                let x = func.append_param(block, int);
                let mut build = Builder::new(&mut func, block);
                let answer = build.icmp(pred, x, x);
                build.ret(&[answer]);
                assert!(simplify(&mut func), "{pred:?} at {bits} bits");
                let said = number(&func, answer);
                if matches!(
                    pred,
                    IntPred::Ne | IntPred::Slt | IntPred::Sgt | IntPred::Ult | IntPred::Ugt
                ) {
                    assert_eq!(said, 0, "{pred:?} at {bits} bits");
                } else {
                    assert_ne!(said, 0, "{pred:?} at {bits} bits");
                }
            }
        }
    }

    /// A remainder by one is nothing, and a division by one is the value. The pair is worth a
    /// test of its own because they are the two identities that produce different shapes from the
    /// same operands.
    #[test]
    fn dividing_by_one_and_the_remainder_that_goes_with_it() {
        let i32 = Type::int(32);
        let (_, mut func, block) = one_block(i32);
        let x = func.append_param(block, i32);
        let mut build = Builder::new(&mut func, block);
        let one = build.iconst(i32, 1);
        let quotient = build.binary(Opcode::SDiv, x, one, Flags::NONE);
        let rest = build.binary(Opcode::SRem, x, one, Flags::NONE);
        let sum = build.binary(Opcode::Add, quotient, rest, Flags::NONE);
        build.ret(&[sum]);
        assert!(simplify(&mut func));
        assert_eq!(number(&func, rest), 0);
        // The add reads the value the division was of, which is what the redirection did.
        let rucc_ir::Def::Result { inst, .. } = func[sum].def else { panic!("not a result") };
        assert_eq!(func[func[inst].args][0], x);
    }

    /// All ones at one bit is the `1` the rule file writes, and the front end writes it as `-1`.
    /// The two are the same bit and the rule has to fire on what the front end wrote.
    #[test]
    fn all_ones_at_one_bit_is_the_one_the_front_end_writes() {
        for written in [-1, 1] {
            let bit = Type::int(1);
            let (_, mut func, block) = one_block(bit);
            let x = func.append_param(block, bit);
            let mut build = Builder::new(&mut func, block);
            let ones = build.iconst(bit, written);
            let kept = build.binary(Opcode::And, x, ones, Flags::NONE);
            build.ret(&[kept]);
            assert!(simplify(&mut func), "written as {written}");
            assert_eq!(returned(&func, block), x, "written as {written}");
        }
    }

    /// One identity feeding another is followed all the way, so the second is worth as much as
    /// the first. The redirections are applied once at the end of the run, and this is what says
    /// that costs nothing.
    #[test]
    fn one_identity_feeding_another_is_followed_to_the_end() {
        let i32 = Type::int(32);
        let (_, mut func, block) = one_block(i32);
        let x = func.append_param(block, i32);
        let mut build = Builder::new(&mut func, block);
        let zero = build.iconst(i32, 0);
        let one = build.iconst(i32, 1);
        let sum = build.binary(Opcode::Add, x, zero, Flags::NONE);
        let product = build.binary(Opcode::Mul, sum, one, Flags::NONE);
        let shifted = build.binary(Opcode::Shl, product, zero, Flags::NONE);
        build.ret(&[shifted]);
        assert!(simplify(&mut func));
        assert_eq!(returned(&func, block), x);
    }

    /// Shifting nothing in any direction, and all ones to the right with the sign bit coming in.
    /// The count is a parameter here, so nothing at all is known about it and the identity is the
    /// only thing that could decide these.
    #[test]
    fn shifting_nothing_and_shifting_all_ones_with_the_sign() {
        for bits in [8, 16, 32, 64] {
            let ty = Type::int(bits);
            let cases = [
                (Opcode::Shl, 0_i128, 0_i128),
                (Opcode::LShr, 0, 0),
                (Opcode::AShr, 0, 0),
                (Opcode::AShr, -1, -1),
            ];
            for (opcode, from, expected) in cases {
                let (_, mut func, block) = one_block(ty);
                let count = func.append_param(block, ty);
                let mut build = Builder::new(&mut func, block);
                let value = build.iconst(ty, from);
                let shifted = build.binary(opcode, value, count, Flags::NONE);
                build.ret(&[shifted]);
                assert!(simplify(&mut func), "{opcode:?} of {from} at {bits} bits");
                let said = number(&func, shifted);
                assert_eq!(said, expected, "{opcode:?} of {from} at {bits} bits");
            }
        }
    }

    /// All ones shifted right with zeroes coming in is not all ones, and there is no rule saying
    /// it is. The pair with the arithmetic shift above is the whole of why the sign matters here.
    #[test]
    fn all_ones_shifted_right_with_zeroes_coming_in_is_left_alone() {
        let i32 = Type::int(32);
        let (_, mut func, block) = one_block(i32);
        let count = func.append_param(block, i32);
        let mut build = Builder::new(&mut func, block);
        let ones = build.iconst(i32, -1);
        let shifted = build.binary(Opcode::LShr, ones, count, Flags::NONE);
        build.ret(&[shifted]);
        assert!(!simplify(&mut func));
        assert_eq!(came_from(&func, shifted).0, Opcode::LShr);
    }

    #[test]
    fn an_instruction_no_rule_is_about_is_left_alone() {
        // Multiplying by three. Two is tier two and is an addition, one and zero are tier one, and
        // every power of two is a shift, so three is the smallest constant no tier has anything to
        // say about. Turning it into a shift and an add is a sequence rather than a rewrite.
        let i32 = Type::int(32);
        let (_, mut func, block) = one_block(i32);
        let x = func.append_param(block, i32);
        let mut build = Builder::new(&mut func, block);
        let three = build.iconst(i32, 3);
        let tripled = build.binary(Opcode::Mul, x, three, Flags::NONE);
        build.ret(&[tripled]);
        assert!(!simplify(&mut func), "no rule is about multiplying by three");
        assert_eq!(returned(&func, block), tripled);
        assert_eq!(came_from(&func, tripled).0, Opcode::Mul);
    }

    #[test]
    fn multiplying_by_two_becomes_an_addition_of_the_value_with_itself() {
        let i32 = Type::int(32);
        let (_, mut func, block) = one_block(i32);
        let x = func.append_param(block, i32);
        let mut build = Builder::new(&mut func, block);
        let two = build.iconst(i32, 2);
        let doubled = build.binary(Opcode::Mul, x, two, Flags::NONE);
        build.ret(&[doubled]);
        assert!(simplify(&mut func));
        // In place, so the value the return reads is the one it always read.
        assert_eq!(returned(&func, block), doubled);
        assert_eq!(came_from(&func, doubled).0, Opcode::Add);
        assert_eq!(operands(&func, doubled), [x, x]);
        // And it stays an addition although two is a power of two and the rule below would take
        // it. A rule naming its constant is more specific than a rule taking whatever constant is
        // there, so the trie tries it first without anything having to sort the two.
    }

    #[test]
    fn multiplying_by_a_power_of_two_becomes_a_shift_by_the_count_of_its_zeros() {
        let i32 = Type::int(32);
        let (_, mut func, block) = one_block(i32);
        let x = func.append_param(block, i32);
        let mut build = Builder::new(&mut func, block);
        let eight = build.iconst(i32, 8);
        let scaled = build.binary(Opcode::Mul, x, eight, Flags::NONE);
        build.ret(&[scaled]);
        assert!(simplify(&mut func));
        assert_eq!(returned(&func, block), scaled);
        assert_eq!(came_from(&func, scaled).0, Opcode::Shl);
        let args = operands(&func, scaled);
        assert_eq!(args[0], x);
        assert_eq!(number(&func, args[1]), 3);
    }

    #[test]
    fn the_power_of_two_with_the_sign_bit_set_is_one_of_them() {
        // The constant the compiler and the solver would disagree about if either read it at some
        // width other than the rule's. At 32 bits this is a power of two and shifts by 31, and in
        // the 128 bit integer the pass matches constants into it is a negative number, so a guard
        // that forgot to mask would call it no power of two at all.
        let i32 = Type::int(32);
        let (_, mut func, block) = one_block(i32);
        let x = func.append_param(block, i32);
        let mut build = Builder::new(&mut func, block);
        let top = build.iconst(i32, 0x8000_0000);
        let scaled = build.binary(Opcode::Mul, x, top, Flags::NONE);
        build.ret(&[scaled]);
        assert!(simplify(&mut func));
        assert_eq!(came_from(&func, scaled).0, Opcode::Shl);
        assert_eq!(number(&func, operands(&func, scaled)[1]), 31);
    }

    #[test]
    fn dividing_an_unsigned_value_by_a_power_of_two_becomes_a_shift() {
        let i32 = Type::int(32);
        let (_, mut func, block) = one_block(i32);
        let x = func.append_param(block, i32);
        let mut build = Builder::new(&mut func, block);
        let sixteen = build.iconst(i32, 16);
        let quotient = build.binary(Opcode::UDiv, x, sixteen, Flags::NONE);
        build.ret(&[quotient]);
        assert!(simplify(&mut func));
        assert_eq!(came_from(&func, quotient).0, Opcode::LShr);
        let args = operands(&func, quotient);
        assert_eq!(args[0], x);
        assert_eq!(number(&func, args[1]), 4);
    }

    #[test]
    fn dividing_a_signed_value_by_a_power_of_two_is_left_alone() {
        // Deliberately, and this says so rather than leaving it to be read as an oversight. A
        // signed division rounds towards zero and a shift rounds down, so the two agree only on
        // values that are not negative. Correcting for that is a bias added before the shift,
        // which is a sequence of instructions rather than one term in place of another.
        let i32 = Type::int(32);
        let (_, mut func, block) = one_block(i32);
        let x = func.append_param(block, i32);
        let mut build = Builder::new(&mut func, block);
        let sixteen = build.iconst(i32, 16);
        let quotient = build.binary(Opcode::SDiv, x, sixteen, Flags::NONE);
        build.ret(&[quotient]);
        assert!(!simplify(&mut func), "no rule turns a signed division into a shift");
        assert_eq!(came_from(&func, quotient).0, Opcode::SDiv);
    }

    #[test]
    fn the_unsigned_remainder_of_a_power_of_two_becomes_a_mask() {
        let i32 = Type::int(32);
        let (_, mut func, block) = one_block(i32);
        let x = func.append_param(block, i32);
        let mut build = Builder::new(&mut func, block);
        let thirty_two = build.iconst(i32, 32);
        let rest = build.binary(Opcode::URem, x, thirty_two, Flags::NONE);
        build.ret(&[rest]);
        assert!(simplify(&mut func));
        assert_eq!(came_from(&func, rest).0, Opcode::And);
        let args = operands(&func, rest);
        assert_eq!(args[0], x);
        assert_eq!(number(&func, args[1]), 31);
    }

    #[test]
    fn a_division_by_a_constant_that_is_not_a_power_of_two_is_left_alone() {
        let i32 = Type::int(32);
        let (_, mut func, block) = one_block(i32);
        let x = func.append_param(block, i32);
        let mut build = Builder::new(&mut func, block);
        let ten = build.iconst(i32, 10);
        let quotient = build.binary(Opcode::UDiv, x, ten, Flags::NONE);
        build.ret(&[quotient]);
        assert!(!simplify(&mut func), "ten is no power of two");
        assert_eq!(came_from(&func, quotient).0, Opcode::UDiv);
    }

    #[test]
    fn multiplying_by_minus_one_becomes_a_subtraction_from_a_zero_the_rewrite_defines() {
        // The other shape of operand: nothing in the function holds a zero, so the rewrite has to
        // put one in front of the instruction it is rewriting.
        let i32 = Type::int(32);
        let (_, mut func, block) = one_block(i32);
        let x = func.append_param(block, i32);
        let mut build = Builder::new(&mut func, block);
        let minus = build.iconst(i32, -1);
        let negated = build.binary(Opcode::Mul, x, minus, Flags::NONE);
        build.ret(&[negated]);
        assert!(simplify(&mut func));
        assert_eq!(returned(&func, block), negated);
        assert_eq!(came_from(&func, negated).0, Opcode::Sub);
        let args = operands(&func, negated);
        assert_eq!(number(&func, args[0]), 0);
        assert_eq!(args[1], x);
    }

    #[test]
    fn a_strength_reduction_keeps_a_promise_only_where_it_is_the_same_promise() {
        // An `nsw` on a multiplication is a promise about that multiplication, and carrying one
        // across a rewrite because it probably still holds is how a wrong one gets made. These are
        // the rewrites where it provably holds, and the two places next to them where it does not:
        // `nuw` on a multiplication by `-1` is about the largest unsigned number, and a shift by
        // thirty one is a multiplication by the most negative `int`.
        let i32 = Type::int(32);
        let both = Flags::NSW.union(Flags::NUW);
        for (by, left, flags, opcode, kept) in [
            (2, false, both, Opcode::Add, both),
            (-1, false, both, Opcode::Sub, Flags::NSW),
            (128, false, Flags::NSW, Opcode::Shl, Flags::NSW),
            (128, false, both, Opcode::Shl, both),
            (128, true, Flags::NSW, Opcode::Shl, Flags::NSW),
            (128, false, Flags::NONE, Opcode::Shl, Flags::NONE),
            (i128::from(i32::MIN), false, Flags::NSW, Opcode::Shl, Flags::NONE),
        ] {
            let (_, mut func, block) = one_block(i32);
            let x = func.append_param(block, i32);
            let mut build = Builder::new(&mut func, block);
            let k = build.iconst(i32, by);
            let (lhs, rhs) = if left { (k, x) } else { (x, k) };
            let product = build.binary(Opcode::Mul, lhs, rhs, flags);
            build.ret(&[product]);
            assert!(simplify(&mut func));
            let rucc_ir::Def::Result { inst, .. } = func[product].def else {
                panic!("not a result")
            };
            assert_eq!(func[inst].opcode, opcode, "{by}");
            assert_eq!(func[inst].flags, kept, "{by}, {flags:?}, constant on the left {left}");
        }
    }

    #[test]
    fn a_strength_reduction_leaves_the_verifier_nothing_to_complain_about() {
        // The zero the negation needs is defined in front of the instruction that reads it, and
        // whether it really is in front of it is a question about the block rather than about the
        // instruction, which is what the verifier is for.
        let target = TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu));
        let i32 = Type::int(32);
        let (mut names, mut func, block) = one_block(i32);
        let mut module = Module::new(names.intern("test.c"), &target);
        let x = func.append_param(block, i32);
        let mut build = Builder::new(&mut func, block);
        let minus = build.iconst(i32, -1);
        let negated = build.binary(Opcode::Mul, x, minus, Flags::NONE);
        let two = build.iconst(i32, 2);
        let doubled = build.binary(Opcode::Mul, negated, two, Flags::NONE);
        build.ret(&[doubled]);
        assert!(simplify(&mut func));
        module.add_func(func);
        rucc_ir::verify(&module, &names).expect("the pass left the function verifiable");
    }

    /// The function the pass leaves is still one the verifier accepts. Pointing a reader at a
    /// different value and turning an instruction into a constant are both things a rewrite could
    /// get wrong in a way none of the tests above would notice, because each of those asks about
    /// one instruction and this asks about the function.
    #[test]
    fn the_pass_leaves_the_verifier_nothing_to_complain_about() {
        let target = TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu));
        let i32 = Type::int(32);
        let (mut names, mut func, block) = one_block(i32);
        let mut module = Module::new(names.intern("test.c"), &target);
        let x = func.append_param(block, i32);
        let mut build = Builder::new(&mut func, block);
        let zero = build.iconst(i32, 0);
        let one = build.iconst(i32, 1);
        let sum = build.binary(Opcode::Add, x, zero, Flags::NONE);
        let product = build.binary(Opcode::Mul, sum, one, Flags::NONE);
        let gone = build.binary(Opcode::Sub, product, product, Flags::NONE);
        let total = build.binary(Opcode::Add, product, gone, Flags::NONE);
        build.ret(&[total]);
        assert!(simplify(&mut func));
        module.add_func(func);
        rucc_ir::verify(&module, &names).expect("the pass left the function verifiable");
    }

    #[test]
    fn fuel_stops_an_identity_and_not_the_walk() {
        let i32 = Type::int(32);
        let (_, mut func, block) = one_block(i32);
        let x = func.append_param(block, i32);
        let mut build = Builder::new(&mut func, block);
        let zero = build.iconst(i32, 0);
        let first = build.binary(Opcode::Add, x, zero, Flags::NONE);
        let second = build.binary(Opcode::Sub, x, zero, Flags::NONE);
        let sum = build.binary(Opcode::Add, first, second, Flags::NONE);
        build.ret(&[sum]);
        let stats =
            Simplify.run(&mut func, &mut crate::machine::fixtures::analyses(), &mut Fuel::of(1));
        assert!(stats.changed());
        assert_eq!(stats.total(Kind::Optimized), 1);
        assert_eq!(stats.count(Kind::Missed, super::NO_FUEL_RULE), 1);
        // The first fired and the second did not, and the second is still read by the add.
        let rucc_ir::Def::Result { inst, .. } = func[sum].def else { panic!("not a result") };
        assert_eq!(func[func[inst].args], [x, second]);
    }

    #[test]
    fn a_negated_float_comparison_becomes_the_opposite_predicate() {
        // Every ordered predicate and its opposite, which is the table `!(x < y)` is `x >= y`
        // or unordered lives in, and the one place a sign error would hide.
        for pred in FloatPred::all() {
            let (_, mut func, block) = blank();
            let mut build = Builder::new(&mut func, block);
            let x = build.iconst(Type::int(64), 0);
            let x = build.unary(Opcode::Bitcast, x, Type::float(Float::F64));
            // Not `x` against itself, which is equal or unordered and settles most predicates on its own.
            let y = build.iconst(Type::int(64), 1);
            let y = build.unary(Opcode::Bitcast, y, Type::float(Float::F64));
            let cmp = build.fcmp(pred, x, y, Flags::NONE);
            let ones = build.iconst(Type::int(1), -1);
            let not = build.binary(Opcode::Xor, cmp, ones, Flags::NONE);
            build.ret(&[not]);
            assert!(simplify(&mut func), "{pred:?}");
            assert_eq!(
                came_from(&func, not),
                (Opcode::FCmp, Extra::FloatPred(pred.inverse())),
                "{pred:?}"
            );
        }
    }

    #[test]
    fn a_negated_integer_comparison_becomes_the_opposite_predicate() {
        for pred in IntPred::all() {
            let (_, mut func, block) = blank();
            let mut build = Builder::new(&mut func, block);
            let x = build.iconst(Type::int(32), 3);
            let y = build.iconst(Type::int(32), 4);
            let cmp = build.icmp(pred, x, y);
            let ones = build.iconst(Type::int(1), -1);
            let not = build.binary(Opcode::Xor, cmp, ones, Flags::NONE);
            build.ret(&[not]);
            assert!(simplify(&mut func), "{pred:?}");
            assert_eq!(
                came_from(&func, not),
                (Opcode::ICmp, Extra::IntPred(pred.inverse())),
                "{pred:?}"
            );
        }
    }

    #[test]
    fn the_constant_is_found_on_either_side() {
        for swapped in [false, true] {
            let (_, mut func, block) = blank();
            let mut build = Builder::new(&mut func, block);
            let x = build.iconst(Type::int(32), 3);
            let y = build.iconst(Type::int(32), 4);
            let cmp = build.icmp(IntPred::Slt, x, y);
            let ones = build.iconst(Type::int(1), -1);
            let (lhs, rhs) = if swapped { (ones, cmp) } else { (cmp, ones) };
            let not = build.binary(Opcode::Xor, lhs, rhs, Flags::NONE);
            build.ret(&[not]);
            assert!(simplify(&mut func), "swapped {swapped}");
            assert_eq!(came_from(&func, not).1, Extra::IntPred(IntPred::Sge));
        }
    }

    #[test]
    fn an_exclusive_or_of_two_comparisons_is_left_alone() {
        let (_, mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let x = build.iconst(Type::int(32), 3);
        let y = build.iconst(Type::int(32), 4);
        let a = build.icmp(IntPred::Slt, x, y);
        let b = build.icmp(IntPred::Sgt, x, y);
        let differ = build.binary(Opcode::Xor, a, b, Flags::NONE);
        build.ret(&[differ]);
        assert!(!simplify(&mut func));
        assert_eq!(came_from(&func, differ).0, Opcode::Xor);
    }

    #[test]
    fn an_exclusive_or_of_something_that_is_not_a_comparison_is_left_alone() {
        let (_, mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let x = build.iconst(Type::int(32), 3);
        let narrow = build.unary(Opcode::Trunc, x, Type::int(1));
        let ones = build.iconst(Type::int(1), -1);
        let not = build.binary(Opcode::Xor, narrow, ones, Flags::NONE);
        build.ret(&[not]);
        assert!(!simplify(&mut func));
        assert_eq!(came_from(&func, not).0, Opcode::Xor);
    }

    #[test]
    fn a_wider_exclusive_or_with_one_is_not_a_negation_and_is_left_alone() {
        let (_, mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let x = build.iconst(Type::int(32), 3);
        let y = build.iconst(Type::int(32), 4);
        let cmp = build.icmp(IntPred::Slt, x, y);
        let wide = build.unary(Opcode::ZExt, cmp, Type::int(32));
        let one = build.iconst(Type::int(32), 1);
        let flipped = build.binary(Opcode::Xor, wide, one, Flags::NONE);
        let narrow = build.unary(Opcode::Trunc, flipped, Type::int(1));
        build.ret(&[narrow]);
        assert!(!simplify(&mut func), "an i32 xor 1 flips one bit of thirty two");
        assert_eq!(came_from(&func, flipped).0, Opcode::Xor);
    }

    #[test]
    fn the_comparisons_flags_travel_with_the_predicate() {
        let (_, mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let x = build.iconst(Type::int(64), 0);
        let x = build.unary(Opcode::Bitcast, x, Type::float(Float::F64));
        // Not `x` against itself, which is equal or unordered and settles most predicates on its own.
        let y = build.iconst(Type::int(64), 1);
        let y = build.unary(Opcode::Bitcast, y, Type::float(Float::F64));
        let cmp = build.fcmp(FloatPred::Olt, x, y, Flags::FAST);
        let ones = build.iconst(Type::int(1), -1);
        let not = build.binary(Opcode::Xor, cmp, ones, Flags::NONE);
        build.ret(&[not]);
        assert!(simplify(&mut func));
        let rucc_ir::Def::Result { inst, .. } = func[not].def else { panic!("not a result") };
        // The promise the original comparison was made under, not the exclusive or's absence of
        // one. Dropping it would be correct and would quietly undo a fast math flag.
        assert_eq!(func[inst].flags, Flags::FAST);
    }

    #[test]
    fn fuel_stops_the_transformation_and_not_the_walk() {
        let (_, mut func, block) = blank();
        let mut build = Builder::new(&mut func, block);
        let x = build.iconst(Type::int(32), 3);
        let y = build.iconst(Type::int(32), 4);
        let a = build.icmp(IntPred::Slt, x, y);
        let b = build.icmp(IntPred::Sgt, x, y);
        let ones = build.iconst(Type::int(1), -1);
        let first = build.binary(Opcode::Xor, a, ones, Flags::NONE);
        let second = build.binary(Opcode::Xor, b, ones, Flags::NONE);
        let both = build.binary(Opcode::And, first, second, Flags::NONE);
        build.ret(&[both]);
        let stats =
            Simplify.run(&mut func, &mut crate::machine::fixtures::analyses(), &mut Fuel::of(1));
        assert!(stats.changed());
        assert_eq!(stats.count(Kind::Optimized, super::FLIPPED), 1);
        assert_eq!(stats.count(Kind::Missed, super::NO_FUEL), 1);
        assert_eq!(came_from(&func, first).0, Opcode::ICmp);
        assert_eq!(came_from(&func, second).0, Opcode::Xor);
    }

    /// Two `i32` parameters to compare, which every composite test below is about.
    fn a_pair() -> (Func, Block, Value, Value) {
        let mut names = Interner::new();
        let name = names.intern("f");
        let int = Type::int(32);
        let signature = Signature::new().with_params(&[int, int]).with_returns(&[Type::int(1)]);
        let mut func = Func::new(name, signature);
        let block = func.create_block();
        let x = func.append_param(block, int);
        let y = func.append_param(block, int);
        (func, block, x, y)
    }

    /// The same, of `f64`.
    fn a_float_pair() -> (Func, Block, Value, Value) {
        let mut names = Interner::new();
        let name = names.intern("f");
        let float = Type::float(Float::F64);
        let signature = Signature::new().with_params(&[float, float]).with_returns(&[Type::int(1)]);
        let mut func = Func::new(name, signature);
        let block = func.create_block();
        let x = func.append_param(block, float);
        let y = func.append_param(block, float);
        (func, block, x, y)
    }

    /// Every bucket table agrees with [`FloatPred::inverse`] about what the opposite of a predicate
    /// is.
    ///
    /// The point of the assertion is that the two were written in different crates by different
    /// reasoning. `inverse` is a sixteen line table of names, and the buckets are four bits and a
    /// complement, so a predicate given the wrong set here disagrees with the name it was given
    /// there and this says which one.
    #[test]
    fn the_opposite_of_a_float_predicate_is_the_buckets_it_leaves_out() {
        for pred in FloatPred::all() {
            assert_eq!(
                super::float_buckets(pred.inverse()),
                super::bucket::ALL_FLOAT ^ super::float_buckets(pred),
                "{pred:?}"
            );
        }
    }

    /// And with [`FloatPred::swapped`] about what reading the operands the other way round does.
    ///
    /// Which of two values is below the other changes and nothing else does, because equal is equal
    /// from both ends and a NaN makes a pair unordered from both ends.
    #[test]
    fn swapping_a_float_predicates_operands_exchanges_below_and_above() {
        for pred in FloatPred::all() {
            let want = super::turned(super::float_buckets(pred));
            assert_eq!(super::float_buckets(pred.swapped()), want, "{pred:?}");
        }
    }

    /// The sixteen floating point predicates are the sixteen sets, so reading a set back is total
    /// and gives the predicate it came from.
    #[test]
    fn every_set_of_float_buckets_is_a_predicate() {
        for pred in FloatPred::all() {
            assert_eq!(super::float_pred(super::float_buckets(pred)), Some(pred), "{pred:?}");
        }
        for buckets in 0..=super::bucket::ALL_FLOAT {
            assert!(super::float_pred(buckets).is_some(), "{buckets} spells nothing");
        }
    }

    /// The same two agreements for the integer predicates.
    #[test]
    fn an_integer_predicate_agrees_with_its_own_opposite_and_its_own_swap() {
        use super::bucket::ALL_INT;
        for pred in IntPred::all() {
            let (before, reading) = super::int_buckets(pred);
            let (opposite, other) = super::int_buckets(pred.inverse());
            assert_eq!(opposite, ALL_INT ^ before, "the opposite of {pred:?}");
            assert_eq!(other, reading, "the opposite of {pred:?} reads the operands differently");
            let (swapped, other) = super::int_buckets(pred.swapped());
            assert_eq!(swapped, super::turned(before), "the swap of {pred:?}");
            assert_eq!(other, reading, "the swap of {pred:?} reads the operands differently");
        }
    }

    /// Reading an integer set back gives the predicate it came from, under the reading that
    /// predicate wanted.
    #[test]
    fn every_integer_predicate_is_read_back_as_itself() {
        for pred in IntPred::all() {
            let (buckets, reading) = super::int_buckets(pred);
            assert_eq!(super::int_pred(buckets, reading), Some(pred), "{pred:?}");
        }
    }

    #[test]
    fn two_integer_comparisons_that_agree_about_nothing_are_false() {
        let (mut func, block, x, y) = a_pair();
        let mut build = Builder::new(&mut func, block);
        let same = build.icmp(IntPred::Eq, x, y);
        let differ = build.icmp(IntPred::Ne, x, y);
        let both = build.binary(Opcode::And, same, differ, Flags::NONE);
        build.ret(&[both]);
        assert!(simplify(&mut func));
        assert_eq!(number(&func, both), 0);
    }

    #[test]
    fn two_integer_comparisons_that_cover_everything_are_true() {
        let (mut func, block, x, y) = a_pair();
        let mut build = Builder::new(&mut func, block);
        let above = build.icmp(IntPred::Sge, x, y);
        let below = build.icmp(IntPred::Slt, x, y);
        let either = build.binary(Opcode::Or, above, below, Flags::NONE);
        build.ret(&[either]);
        assert!(simplify(&mut func));
        assert_ne!(number(&func, either), 0);
    }

    /// Below or equal is one comparison, and the saving is what makes the rewrite worth taking on
    /// its own rather than only for the two answers that are constants.
    #[test]
    fn two_integer_comparisons_that_overlap_become_one() {
        let (mut func, block, x, y) = a_pair();
        let mut build = Builder::new(&mut func, block);
        let below = build.icmp(IntPred::Slt, x, y);
        let same = build.icmp(IntPred::Eq, x, y);
        let either = build.binary(Opcode::Or, below, same, Flags::NONE);
        build.ret(&[either]);
        assert!(simplify(&mut func));
        assert_eq!(came_from(&func, either), (Opcode::ICmp, Extra::IntPred(IntPred::Sle)));
        assert_eq!(operands(&func, either), [x, y]);
    }

    /// `(x<y) && (y<x)`, which is the third test of `gcc.c-torture/execute/compare-3.c` and the one
    /// that needs the second comparison turned round before the two are about one pair.
    #[test]
    fn the_second_comparison_is_read_in_the_first_ones_operand_order() {
        let (mut func, block, x, y) = a_pair();
        let mut build = Builder::new(&mut func, block);
        let below = build.icmp(IntPred::Slt, x, y);
        let above = build.icmp(IntPred::Slt, y, x);
        let both = build.binary(Opcode::And, below, above, Flags::NONE);
        build.ret(&[both]);
        assert!(simplify(&mut func));
        assert_eq!(number(&func, both), 0);
    }

    /// An equality says nothing about how the operands are read, so it combines with either
    /// ordering and takes the one beside it.
    #[test]
    fn an_equality_takes_the_ordering_of_the_comparison_beside_it() {
        for (ordered, want) in [(IntPred::Ult, IntPred::Ule), (IntPred::Slt, IntPred::Sle)] {
            let (mut func, block, x, y) = a_pair();
            let mut build = Builder::new(&mut func, block);
            let below = build.icmp(ordered, x, y);
            let same = build.icmp(IntPred::Eq, x, y);
            let either = build.binary(Opcode::Or, below, same, Flags::NONE);
            build.ret(&[either]);
            assert!(simplify(&mut func), "{ordered:?}");
            assert_eq!(came_from(&func, either).1, Extra::IntPred(want), "{ordered:?}");
        }
    }

    /// A signed comparison and an unsigned one are two different questions, and a set built out of
    /// one of each would be a set about no reading in particular.
    #[test]
    fn a_signed_comparison_and_an_unsigned_one_are_left_alone() {
        let (mut func, block, x, y) = a_pair();
        let mut build = Builder::new(&mut func, block);
        let signed = build.icmp(IntPred::Slt, x, y);
        let unsigned = build.icmp(IntPred::Ugt, x, y);
        let both = build.binary(Opcode::And, signed, unsigned, Flags::NONE);
        build.ret(&[both]);
        assert!(!simplify(&mut func));
        assert_eq!(came_from(&func, both).0, Opcode::And);
    }

    #[test]
    fn two_comparisons_about_different_operands_are_left_alone() {
        let (mut func, block, x, y) = a_pair();
        let mut build = Builder::new(&mut func, block);
        let other = build.iconst(Type::int(32), 7);
        let first = build.icmp(IntPred::Slt, x, y);
        let second = build.icmp(IntPred::Sgt, x, other);
        let both = build.binary(Opcode::And, first, second, Flags::NONE);
        build.ret(&[both]);
        assert!(!simplify(&mut func));
        assert_eq!(came_from(&func, both).0, Opcode::And);
    }

    /// `x == y && x != y` on floating point, which is false for the same reason it is on integers
    /// and is not the same reason a reader might expect: `oeq` and `une` do not overlap because
    /// `oeq` refuses a NaN and `une` accepts one, so the pair is empty rather than only unequal.
    #[test]
    fn two_float_comparisons_that_agree_about_nothing_are_false() {
        let (mut func, block, x, y) = a_float_pair();
        let mut build = Builder::new(&mut func, block);
        let same = build.fcmp(FloatPred::Oeq, x, y, Flags::NONE);
        let differ = build.fcmp(FloatPred::Une, x, y, Flags::NONE);
        let both = build.binary(Opcode::And, same, differ, Flags::NONE);
        build.ret(&[both]);
        assert!(simplify(&mut func));
        assert_eq!(number(&func, both), 0);
    }

    /// Unordered, or above or equal, or below, which is the fifth test of
    /// `gcc.c-torture/execute/ieee/compare-fp-3.c`. It is three comparisons and two `or`s, and it
    /// folds because the walk is forward and the rewrite is in place: the inner pair is one
    /// comparison by the time the outer `or` is looked at.
    #[test]
    fn a_three_way_float_condition_folds_one_pair_at_a_time() {
        let (mut func, block, x, y) = a_float_pair();
        let mut build = Builder::new(&mut func, block);
        let neither = build.fcmp(FloatPred::Uno, x, y, Flags::NONE);
        let above = build.fcmp(FloatPred::Oge, x, y, Flags::NONE);
        let below = build.fcmp(FloatPred::Olt, x, y, Flags::NONE);
        let first = build.binary(Opcode::Or, neither, above, Flags::NONE);
        let whole = build.binary(Opcode::Or, first, below, Flags::NONE);
        build.ret(&[whole]);
        assert!(simplify(&mut func));
        assert_eq!(came_from(&func, first).1, Extra::FloatPred(FloatPred::Uge));
        assert_ne!(number(&func, whole), 0);
    }

    /// One `f64` parameter, which every magnitude test below takes the magnitude of.
    fn a_float() -> (Func, Block, Value) {
        let mut names = Interner::new();
        let name = names.intern("f");
        let float = Type::float(Float::F64);
        let signature = Signature::new().with_params(&[float]).with_returns(&[Type::int(1)]);
        let mut func = Func::new(name, signature);
        let block = func.create_block();
        let x = func.append_param(block, float);
        (func, block, x)
    }

    /// `fabs (x)` as the lowering writes it, which is the sign bit cleared over the bits.
    fn magnitude_of(build: &mut Builder<'_>, x: Value) -> Value {
        let bits = Type::int(64);
        let number = build.unary(Opcode::Bitcast, x, bits);
        let mask = build.iconst(bits, i128::from(i64::MAX));
        let cleared = build.binary(Opcode::And, number, mask, Flags::NONE);
        build.unary(Opcode::Bitcast, cleared, Type::float(Float::F64))
    }

    /// `fabs (x) < 0.0`, which is what `gcc.c-torture/execute/20020720-1.c` asserts is false by
    /// calling a function it never defines.
    #[test]
    fn a_magnitude_is_never_below_zero() {
        let (mut func, block, x) = a_float();
        let mut build = Builder::new(&mut func, block);
        let p = magnitude_of(&mut build, x);
        let zero = build.fconst(Type::float(Float::F64), 0);
        let below = build.fcmp(FloatPred::Olt, p, zero, Flags::NONE);
        build.ret(&[below]);
        assert!(simplify(&mut func));
        assert_eq!(number(&func, below), 0);
    }

    /// The same question with the operands the other way round. `0.0 > fabs (x)` is the same claim
    /// and is a different instruction, and the buckets have to be turned round to see it.
    #[test]
    fn zero_is_never_above_a_magnitude() {
        let (mut func, block, x) = a_float();
        let mut build = Builder::new(&mut func, block);
        let p = magnitude_of(&mut build, x);
        let zero = build.fconst(Type::float(Float::F64), 0);
        let above = build.fcmp(FloatPred::Ogt, zero, p, Flags::NONE);
        build.ret(&[above]);
        assert!(simplify(&mut func));
        assert_eq!(number(&func, above), 0);
    }

    /// `fabs (x) <= 0.0` is not a constant and is still shorter than it was: the only way a
    /// magnitude is at or below zero is by being zero, so the answer is an equality.
    #[test]
    fn a_magnitude_at_or_below_zero_is_a_magnitude_equal_to_it() {
        let (mut func, block, x) = a_float();
        let mut build = Builder::new(&mut func, block);
        let p = magnitude_of(&mut build, x);
        let zero = build.fconst(Type::float(Float::F64), 0);
        let atmost = build.fcmp(FloatPred::Ole, p, zero, Flags::NONE);
        build.ret(&[atmost]);
        assert!(simplify(&mut func));
        assert_eq!(came_from(&func, atmost).1, Extra::FloatPred(FloatPred::Oeq));
    }

    /// A negative constant takes the equal bucket with it, because every value a magnitude can be
    /// is above every negative number, so the comparison is false rather than shorter.
    #[test]
    fn a_magnitude_is_never_at_or_below_a_negative_number() {
        let (mut func, block, x) = a_float();
        let mut build = Builder::new(&mut func, block);
        let p = magnitude_of(&mut build, x);
        let minus_one = build.fconst(Type::float(Float::F64), 0xbff0_0000_0000_0000);
        let atmost = build.fcmp(FloatPred::Ole, p, minus_one, Flags::NONE);
        build.ret(&[atmost]);
        assert!(simplify(&mut func));
        assert_eq!(number(&func, atmost), 0);
    }

    /// `fabs (x) >= 0.0` is left alone, and a reader who expects it to be true is the reason this
    /// test is here rather than the reason it fails: a NaN has its sign bit cleared like anything
    /// else and is not above, below or equal to anything, so the comparison is false for one.
    #[test]
    fn a_magnitude_at_or_above_zero_is_still_a_question_about_a_nan() {
        let (mut func, block, x) = a_float();
        let mut build = Builder::new(&mut func, block);
        let p = magnitude_of(&mut build, x);
        let zero = build.fconst(Type::float(Float::F64), 0);
        let atleast = build.fcmp(FloatPred::Oge, p, zero, Flags::NONE);
        build.ret(&[atleast]);
        assert!(!simplify(&mut func));
        assert_eq!(came_from(&func, atleast).1, Extra::FloatPred(FloatPred::Oge));
    }

    /// A positive constant narrows nothing, so the comparison stands as it was written.
    #[test]
    fn a_magnitude_against_a_positive_number_is_left_alone() {
        let (mut func, block, x) = a_float();
        let mut build = Builder::new(&mut func, block);
        let p = magnitude_of(&mut build, x);
        let one = build.fconst(Type::float(Float::F64), 0x3ff0_0000_0000_0000);
        let below = build.fcmp(FloatPred::Olt, p, one, Flags::NONE);
        build.ret(&[below]);
        assert!(!simplify(&mut func));
        assert_eq!(came_from(&func, below).1, Extra::FloatPred(FloatPred::Olt));
    }

    /// A mask with its top bit set says nothing about the sign of what comes out of it, so the
    /// bitcast under it is not a magnitude and the comparison stands.
    #[test]
    fn a_mask_that_keeps_the_sign_bit_is_not_a_magnitude() {
        let (mut func, block, x) = a_float();
        let mut build = Builder::new(&mut func, block);
        let bits = Type::int(64);
        let number = build.unary(Opcode::Bitcast, x, bits);
        let mask = build.iconst(bits, -2);
        let cleared = build.binary(Opcode::And, number, mask, Flags::NONE);
        let p = build.unary(Opcode::Bitcast, cleared, Type::float(Float::F64));
        let zero = build.fconst(Type::float(Float::F64), 0);
        let below = build.fcmp(FloatPred::Olt, p, zero, Flags::NONE);
        build.ret(&[below]);
        assert!(!simplify(&mut func));
        assert_eq!(came_from(&func, below).1, Extra::FloatPred(FloatPred::Olt));
    }

    /// A NaN on the other side settles the comparison on its own, and it is the NaN that does it
    /// rather than the magnitude, so the answer is the one any value against a NaN has.
    #[test]
    fn a_magnitude_against_a_nan_is_settled_by_the_nan() {
        let (mut func, block, x) = a_float();
        let mut build = Builder::new(&mut func, block);
        let p = magnitude_of(&mut build, x);
        let nan = build.fconst(Type::float(Float::F64), NAN);
        let below = build.fcmp(FloatPred::Olt, p, nan, Flags::NONE);
        build.ret(&[below]);
        let stats = Simplify.run(
            &mut func,
            &mut crate::machine::fixtures::analyses(),
            &mut Fuel::unlimited(),
        );
        assert_eq!(stats.count(Kind::Optimized, super::MAGNITUDE), 0);
        assert_eq!(stats.count(Kind::Optimized, super::BOUNDED), 1);
        assert_eq!(number(&func, below), 0);
    }

    /// The quiet NaN a `const double` given `1.0/0.0 - 1.0/0.0` holds.
    const NAN: u128 = 0x7ff8_0000_0000_0000;

    /// A positive infinity.
    const INFINITY: u128 = 0x7ff0_0000_0000_0000;

    /// Every comparison of `gcc.c-torture/execute/ieee/fp-cmp-6.c`, which is a NaN against a
    /// number the program could have changed. The ordered ones and `ueq`'s missing half are false
    /// and `une` is true, whatever `x` holds.
    #[test]
    fn a_nan_is_unordered_against_anything() {
        for (pred, answer) in [
            (FloatPred::Oeq, false),
            (FloatPred::Olt, false),
            (FloatPred::Ogt, false),
            (FloatPred::Ole, false),
            (FloatPred::Oge, false),
            (FloatPred::One, false),
            (FloatPred::Une, true),
            (FloatPred::Ult, true),
            (FloatPred::Uno, true),
        ] {
            let (mut func, block, x) = a_float();
            let mut build = Builder::new(&mut func, block);
            let nan = build.fconst(Type::float(Float::F64), NAN);
            let asked = build.fcmp(pred, nan, x, Flags::NONE);
            build.ret(&[asked]);
            assert!(simplify(&mut func), "{pred:?}");
            assert_eq!(number(&func, asked) != 0, answer, "{pred:?}");
        }
    }

    /// Nothing is above a positive infinity, which is `gcc.c-torture/execute/ieee/fp-cmp-7.c`. At or
    /// below one is only a question about a NaN, and it is left as written because what it narrows
    /// to is the predicate it already is.
    #[test]
    fn nothing_is_above_a_positive_infinity() {
        let (mut func, block, x) = a_float();
        let mut build = Builder::new(&mut func, block);
        let infinity = build.fconst(Type::float(Float::F64), INFINITY);
        let above = build.fcmp(FloatPred::Ogt, x, infinity, Flags::NONE);
        let atmost = build.fcmp(FloatPred::Ole, x, infinity, Flags::NONE);
        let below = build.fcmp(FloatPred::Olt, x, infinity, Flags::NONE);
        build.ret(&[above, atmost, below]);
        assert!(simplify(&mut func));
        assert_eq!(number(&func, above), 0);
        assert_eq!(came_from(&func, atmost).1, Extra::FloatPred(FloatPred::Ole));
        assert_eq!(came_from(&func, below).1, Extra::FloatPred(FloatPred::Olt));
    }

    /// The same on the left of a negative infinity, turned round.
    #[test]
    fn a_negative_infinity_is_above_nothing() {
        let (mut func, block, x) = a_float();
        let mut build = Builder::new(&mut func, block);
        let infinity = build.fconst(Type::float(Float::F64), INFINITY | 1 << 63);
        let above = build.fcmp(FloatPred::Ogt, infinity, x, Flags::NONE);
        build.ret(&[above]);
        assert!(simplify(&mut func));
        assert_eq!(number(&func, above), 0);
    }

    /// Two constants are one bucket, so every predicate over them is an answer.
    #[test]
    fn two_float_constants_are_an_answer() {
        let (mut func, block, _) = a_float();
        let mut build = Builder::new(&mut func, block);
        let one = build.fconst(Type::float(Float::F64), 0x3ff0_0000_0000_0000);
        let two = build.fconst(Type::float(Float::F64), 0x4000_0000_0000_0000);
        let below = build.fcmp(FloatPred::Olt, one, two, Flags::NONE);
        let equal = build.fcmp(FloatPred::Ueq, one, two, Flags::NONE);
        build.ret(&[below, equal]);
        assert!(simplify(&mut func));
        assert_ne!(number(&func, below), 0);
        assert_eq!(number(&func, equal), 0);
    }

    /// A value is equal to itself or is a NaN, so it is never below itself, and `x != x` is the
    /// question of whether it is a NaN. `x == x` is the shortest it can be written already.
    #[test]
    fn a_value_against_itself_is_equal_or_a_nan() {
        let (mut func, block, x) = a_float();
        let mut build = Builder::new(&mut func, block);
        let below = build.fcmp(FloatPred::Olt, x, x, Flags::NONE);
        let differs = build.fcmp(FloatPred::Une, x, x, Flags::NONE);
        let same = build.fcmp(FloatPred::Oeq, x, x, Flags::NONE);
        build.ret(&[below, differs, same]);
        assert!(simplify(&mut func));
        assert_eq!(number(&func, below), 0);
        assert_eq!(came_from(&func, differs).1, Extra::FloatPred(FloatPred::Uno));
        assert_eq!(came_from(&func, same).1, Extra::FloatPred(FloatPred::Oeq));
    }

    /// `isunordered (x, y) || !isunordered (x, y)` kept as two branches, which is the seventh test
    /// of `gcc.c-torture/execute/ieee/compare-fp-3.c` at `-O1`, `-Os` and `-Oz`. The second
    /// comparison is only reached where the first was false, so the pair is ordered there. On the
    /// side where it was true nothing is settled, and `x < y` after `x >= y` was false is `x < y` or
    /// unordered, which is shorter only as far as `ult` is.
    #[test]
    fn a_branch_in_front_settles_the_same_pair() {
        let (mut func, entry, x, y) = a_float_pair();
        let [then, other, join] = [(); 3].map(|()| func.create_block());
        let mut build = Builder::new(&mut func, entry);
        let neither = build.fcmp(FloatPred::Uno, x, y, Flags::NONE);
        build.br_if(neither, then, &[], other, &[]);
        let mut build = Builder::new(&mut func, other);
        let ordered = build.fcmp(FloatPred::Ord, x, y, Flags::NONE);
        let turned = build.fcmp(FloatPred::Ord, y, x, Flags::NONE);
        let above = build.fcmp(FloatPred::Ogt, x, y, Flags::NONE);
        build.jump(join, &[]);
        let mut build = Builder::new(&mut func, then);
        let there = build.fcmp(FloatPred::Ord, x, y, Flags::NONE);
        build.jump(join, &[]);
        let mut build = Builder::new(&mut func, join);
        let both = build.binary(Opcode::And, ordered, turned, Flags::NONE);
        let all = build.binary(Opcode::And, both, above, Flags::NONE);
        let all = build.binary(Opcode::And, all, there, Flags::NONE);
        build.ret(&[all]);
        assert!(simplify(&mut func));
        assert_ne!(number(&func, ordered), 0);
        assert_ne!(number(&func, turned), 0);
        assert_eq!(came_from(&func, above).1, Extra::FloatPred(FloatPred::Ogt));
        assert_eq!(number(&func, there), 0);
    }

    /// A join has two ways in, and what one branch said is not what the other did, so nothing is
    /// settled past it.
    #[test]
    fn a_join_settles_nothing() {
        let (mut func, entry, x, y) = a_float_pair();
        let [then, other, join] = [(); 3].map(|()| func.create_block());
        let mut build = Builder::new(&mut func, entry);
        let neither = build.fcmp(FloatPred::Uno, x, y, Flags::NONE);
        build.br_if(neither, then, &[], other, &[]);
        Builder::new(&mut func, then).jump(join, &[]);
        Builder::new(&mut func, other).jump(join, &[]);
        let mut build = Builder::new(&mut func, join);
        let ordered = build.fcmp(FloatPred::Ord, x, y, Flags::NONE);
        build.ret(&[ordered]);
        assert!(!simplify(&mut func));
        assert_eq!(came_from(&func, ordered).1, Extra::FloatPred(FloatPred::Ord));
    }

    /// A number that is not an infinity says nothing about the other side on its own.
    #[test]
    fn a_finite_bound_is_left_alone() {
        let (mut func, block, x) = a_float();
        let mut build = Builder::new(&mut func, block);
        let one = build.fconst(Type::float(Float::F64), 0x3ff0_0000_0000_0000);
        let below = build.fcmp(FloatPred::Olt, x, one, Flags::NONE);
        build.ret(&[below]);
        assert!(!simplify(&mut func));
        assert_eq!(came_from(&func, below).1, Extra::FloatPred(FloatPred::Olt));
    }

    /// The fold spends fuel like the other two and stopping it stops the transforming rather than
    /// the walking.
    #[test]
    fn fuel_stops_the_magnitude_fold_and_not_the_walk() {
        let (mut func, block, x) = a_float();
        let mut build = Builder::new(&mut func, block);
        let p = magnitude_of(&mut build, x);
        let zero = build.fconst(Type::float(Float::F64), 0);
        let below = build.fcmp(FloatPred::Olt, p, zero, Flags::NONE);
        let also = build.fcmp(FloatPred::Olt, p, zero, Flags::NONE);
        let both = build.binary(Opcode::Or, below, also, Flags::NONE);
        build.ret(&[both]);
        let stats =
            Simplify.run(&mut func, &mut crate::machine::fixtures::analyses(), &mut Fuel::of(1));
        assert_eq!(stats.count(Kind::Optimized, super::MAGNITUDE), 1);
        assert_eq!(stats.count(Kind::Missed, super::NO_FUEL_MAGNITUDE), 1);
        assert_eq!(number(&func, below), 0);
        assert_eq!(came_from(&func, also).0, Opcode::FCmp);
    }

    /// A fast math promise is a promise about one comparison, and a set built out of two that were
    /// not promised the same thing is a set under no promise in particular.
    #[test]
    fn two_comparisons_promised_different_things_are_left_alone() {
        let (mut func, block, x, y) = a_float_pair();
        let mut build = Builder::new(&mut func, block);
        let below = build.fcmp(FloatPred::Olt, x, y, Flags::FAST);
        let same = build.fcmp(FloatPred::Oeq, x, y, Flags::NONE);
        let either = build.binary(Opcode::Or, below, same, Flags::NONE);
        build.ret(&[either]);
        assert!(!simplify(&mut func));
        assert_eq!(came_from(&func, either).0, Opcode::Or);
    }

    #[test]
    fn the_promise_both_comparisons_were_made_under_travels_to_the_one_that_replaces_them() {
        let (mut func, block, x, y) = a_float_pair();
        let mut build = Builder::new(&mut func, block);
        let below = build.fcmp(FloatPred::Olt, x, y, Flags::FAST);
        let same = build.fcmp(FloatPred::Oeq, x, y, Flags::FAST);
        let either = build.binary(Opcode::Or, below, same, Flags::NONE);
        build.ret(&[either]);
        assert!(simplify(&mut func));
        assert_eq!(came_from(&func, either).1, Extra::FloatPred(FloatPred::Ole));
        let rucc_ir::Def::Result { inst, .. } = func[either].def else { panic!("not a result") };
        assert_eq!(func[inst].flags, Flags::FAST);
    }

    /// An `and` of two comparisons at a width that is not one bit is an and of two bits held in
    /// something wider, which is a different program.
    #[test]
    fn a_wider_and_of_two_comparisons_is_left_alone() {
        let (mut func, block, x, y) = a_pair();
        let mut build = Builder::new(&mut func, block);
        let same = build.icmp(IntPred::Eq, x, y);
        let differ = build.icmp(IntPred::Ne, x, y);
        let first = build.unary(Opcode::ZExt, same, Type::int(32));
        let second = build.unary(Opcode::ZExt, differ, Type::int(32));
        let both = build.binary(Opcode::And, first, second, Flags::NONE);
        let narrow = build.unary(Opcode::Trunc, both, Type::int(1));
        build.ret(&[narrow]);
        assert!(!simplify(&mut func));
        assert_eq!(came_from(&func, both).0, Opcode::And);
    }

    #[test]
    fn fuel_stops_the_composite_fold_and_not_the_walk() {
        let (mut func, block, x, y) = a_pair();
        let mut build = Builder::new(&mut func, block);
        let same = build.icmp(IntPred::Eq, x, y);
        let differ = build.icmp(IntPred::Ne, x, y);
        let below = build.icmp(IntPred::Slt, x, y);
        let above = build.icmp(IntPred::Sgt, x, y);
        let first = build.binary(Opcode::And, same, differ, Flags::NONE);
        let second = build.binary(Opcode::And, below, above, Flags::NONE);
        let both = build.binary(Opcode::Or, first, second, Flags::NONE);
        build.ret(&[both]);
        let stats =
            Simplify.run(&mut func, &mut crate::machine::fixtures::analyses(), &mut Fuel::of(1));
        assert!(stats.changed());
        assert_eq!(stats.count(Kind::Optimized, super::COMPOSITE), 1);
        assert_eq!(stats.count(Kind::Missed, super::NO_FUEL_COMPOSITE), 1);
        assert_eq!(came_from(&func, first).0, Opcode::IConst);
        assert_eq!(came_from(&func, second).0, Opcode::And);
    }
}
