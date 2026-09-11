//! `a && b` and `a || b` stop being a branch, per section 22.5 of
//! `spec/optimizer/22-phiopt-and-if-conversion.md`.
//!
//! `if (a && b)` is two branches. C says the second question is only asked when the first said yes,
//! and that is a promise about what runs rather than a promise about how the machine gets there.
//! When working out `b` cannot do anything and cannot fail, working it out on the path where `a`
//! said no is invisible to the program, and then the two questions are one question: `a & b`, one
//! branch and one and. On a condition the predictor cannot call, one branch that misses half the
//! time is cheaper than two that miss half the time each.
//!
//! # The shape, which is not two branches
//!
//! GCC does this to a chain of two conditional jumps, which is why the thing that controls it is
//! called `LOGICAL_OP_NON_SHORT_CIRCUIT`. Here it is the same transformation on a shape that does
//! not look like that, and the reason is worth being plain about, because a reader who goes looking
//! for two branches in a row will not find the one this pass matches.
//!
//! `a && b` in C is an expression whose value is a bit, so the lowering walk produces a bit. It
//! branches on `a`, works `b` out on the side where the answer is not yet known, and hands the join
//! block the answer either way:
//!
//! ```text
//! block0:                              block0:
//!     %a = icmp ...                        %a = icmp ...
//!     %f = iconst.i1 0                     %t = iconst.i1 1
//!     br_if %a, block1, block2(%f)         br_if %a, block2(%t), block1
//! block1:                              block1:
//!     %b = icmp ...                        %b = icmp ...
//!     jump block2(%b)                      jump block2(%b)
//! block2(%c: i1):                      block2(%c: i1):
//!     br_if %c, ...                        br_if %c, ...
//! ```
//!
//! The left one is `&&` and the right one is `||`, and the difference between them is entirely in
//! which side the constant is on and what it is. That constant is short circuiting written down. It
//! marks the side where the left operand settles the whole thing on its own, and the bit it carries
//! is the answer it settles it as. So the join's parameter is `a ? b : 0` for the first and
//! `a ? 1 : b` for the second, which are `a & b` and `a | b`, and once it is written that way the
//! branch has nothing left to decide and goes.
//!
//! It is the same transformation GCC does. The chain of two jumps and the bit with a branch around
//! it are the same program, and which of them a compiler is looking at is a fact about its front
//! end rather than about the optimization. What this pass does have to be careful about is running
//! before `thread`, which turns the shape above into the chain by pointing the constant edge
//! straight at the branch it decides. Both forms end in one branch afterwards. Only the shape above
//! ends in one branch and one and.
//!
//! # What has to be true
//!
//! The block working out the right operand has to be reached only from the branch, has to take no
//! parameters and has to end in a jump to the join. That is [`phiopt`](crate::phiopt)'s diamond and
//! this asks it rather than asking again, because the two passes are looking for the same thing and
//! two answers about what an arm is would be two compilers. What is different here is only what is
//! carried: one bit, with the answer already known on one side.
//!
//! The join carries exactly one thing. A join carrying more is a branch deciding several values at
//! once, only one of which is this, and an and written for that one would leave the branch standing
//! for the rest, which is not a collapse. `phiopt` takes that case.
//!
//! And the right operand has to be safe to work out early. Section 22.5 is exact about what that
//! means and about why: no side effect, no possible trap, and no memory access that could fault.
//! The last one is not a detail. `if (p && p->x)` is the most common `&&` in C, the whole job of the
//! `p` is to stop the load from happening, and a compiler that folds that one has written a null
//! dereference into a program that did not have one. M4 excludes loads from the right hand side
//! entirely rather than reasoning about which ones are guarded, which gives up the folds that would
//! have been safe and gives up the whole class of bug with them.
//!
//! # What it will not write
//!
//! Two of the four ways a known bit can sit on one side are folded and two are not. A false on the
//! side the condition does not hold on is an and, and a true on the side it does hold on is an or.
//! The other two, a true below and a false above, are `!a | b` and `!a & b`, and the not is an
//! instruction this would have to write that the two folded cases do not need. They are also not
//! what a front end produces, because by the time this runs `simplify` has turned a negated
//! comparison into the opposite comparison, so the negation is inside the `icmp` rather than around
//! it. They are left rather than written for a shape nothing makes.
//!
//! # The cost rule
//!
//! Work that moves up is work the other path now does for nothing, so there is a budget for it, and
//! it is [`heuristics::SHORT_CIRCUIT_INSTRUCTIONS`]. A right operand that is one comparison against
//! a constant is two instructions here and one on the machine, since the constant becomes the
//! comparison's immediate and stops being anything at all, and that is the shape this is for. A
//! right operand of ten instructions is a computation rather than a test, and speculating a
//! computation to save one branch is a trade in the wrong direction.
//!
//! When there is no work in the arm at all, which is both operands worked out above the branch,
//! nothing is speculated and the budget has nothing to price. Then the fold is one instruction
//! against one branch and it happens whatever the estimate says.
//!
//! Otherwise the estimate has to leave doubt, by the same margin `phiopt` uses and for the same
//! reason. If the estimate says the branch almost always goes one way, the machine will almost
//! always get it right, removing it saves nothing, and the price of working out the right operand
//! on the other path is paid anyway. The doubt is what the fold is bought with.
//!
//! # Where it runs
//!
//! Section 22.5 says `-O2`, and this pass is in the `-O2` and `-O3` lists and not in the `-O1` one.
//! It is also not in the `-Os` or `-Oz` lists. The reason is that the gate above is a speed gate.
//! What is bought is a branch the machine no longer has to guess, which is time, and what is paid
//! is the right operand's instructions running on a path that was skipping them. A level whose cost
//! model is size has no use for that trade.
//!
//! # What this is not
//!
//! It is not the range chain. Section 19.4 wants `x > 3 && x < 7` recognized as one test of one
//! range, and it says this collapse is the same transformation reached from the other direction,
//! because the chain is much easier to see once it is two comparisons and an and in one block than
//! while the two comparisons are still in two blocks. This pass gets the shape into that form.
//! Turning the pair into a single unsigned comparison against a width is a rewrite rule and belongs
//! to the rule set.

use rucc_cost::heuristics;
use rucc_ir::{Block, Builder, Flags, Func, Imm, Opcode, Type, Value};

use crate::fold::constant;
use crate::phiopt::{Diamond, diamond, length, speculatable, unpredictable};
use crate::simplify_cfg::{self, Bindings};
use crate::{Analyses, Fuel, Pass, Preserved, Stats};

/// Recorded once for each `&&` or `||` that stopped being a branch.
const COLLAPSED: &str =
    "both halves of an and-and or an or-or worked out at once, and the branch went";

/// Recorded when the right operand does something, which is a store, a call or a load.
const RIGHT_HAS_EFFECTS: &str =
    "branch kept, working out the right operand touches memory or calls something";

/// Recorded when the right operand could trap on a path that was not going to work it out.
const RIGHT_MAY_TRAP: &str =
    "branch kept, the right operand divides and working it out early could trap";

/// Recorded when the right operand is more work than one branch is worth.
const TOO_MUCH_WORK: &str = "branch kept, the right operand is more work than one branch is worth";

/// Recorded when the estimate says the branch is one sided enough not to be worth removing.
const BRANCH_IS_PREDICTED: &str = "branch kept, the estimate says it goes one way nearly always";

/// Recorded when the condition is already known, which `simplify-cfg` handles.
const CONDITION_IS_DECIDED: &str = "branch kept, the left operand is already decided";

/// Recorded when the budget ran out mid function.
const NO_FUEL: &str = "branch kept, the pass ran out of fuel";

/// The pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShortCircuit;

impl Pass for ShortCircuit {
    fn name(&self) -> &'static str {
        "short-circuit"
    }

    fn describe(&self) -> &'static str {
        "an and-and or an or-or works both halves out at once when the right half is safe to"
    }

    fn preserves(&self) -> Preserved {
        // Nothing. A block stops existing and an edge stops existing with it, so every analysis
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
            let Some(plan) = collapsed(func, &shape) else { continue };
            if let Some(reason) = refused(func, &shape) {
                stats.missed(reason);
                continue;
            }
            let work: u32 = shape.arms.iter().flatten().map(|&arm| length(func, arm)).sum();
            if work > 0 {
                if work > heuristics::SHORT_CIRCUIT_INSTRUCTIONS {
                    stats.missed(TOO_MUCH_WORK);
                    continue;
                }
                // The first edge out of the head. Which of the two is asked about does not matter,
                // since the question is whether the number is near even and the other edge is its
                // complement.
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
            fold(func, &shape, &plan);
            // The graph was about the function as it was a moment ago, and the manager clears the
            // cache after the pass returns, which is too late for the next block.
            an.clear();
            stats.optimized(COLLAPSED);
        }
        stats
    }
}

/// The one instruction that replaces the branch.
struct Collapse {
    /// The bit the side that had to work something out handed the join, which is the right operand.
    right: Value,
    /// What joins it to the condition, which is [`Opcode::And`] for a `&&` and [`Opcode::Or`] for a
    /// `||`.
    joined: Opcode,
}

/// What this diamond collapses to, if it is a short circuit at all.
///
/// The known bit is what identifies one. A side of a branch that hands the join a bit it already
/// has is a side where the left operand settled the answer on its own, and which side that is and
/// which bit it is are between them the whole difference between a `&&` and a `||`.
fn collapsed(func: &Func, shape: &Diamond) -> Option<Collapse> {
    // One thing carried, and it is a bit.
    let [param] = func[shape.join].params[..] else { return None };
    if func[param].ty != Type::I1 {
        return None;
    }
    let sides = [shape.args[0][0], shape.args[1][0]];
    let known = [constant(func, sides[0]), constant(func, sides[1])];
    let settled = |imm: Imm| imm.unsigned() != 0;
    match known {
        // `a && b`. The side the condition does not hold on already knows the answer is no.
        [None, Some((imm, _))] if !settled(imm) => {
            Some(Collapse { right: sides[0], joined: Opcode::And })
        }
        // `a || b`. The side it does hold on already knows the answer is yes.
        [Some((imm, _)), None] if settled(imm) => {
            Some(Collapse { right: sides[1], joined: Opcode::Or })
        }
        // The other two ways round need a not written, and the module comment says why nothing
        // produces them. Two known bits is an answer that does not depend on the branch at all.
        _ => None,
    }
}

/// Why this collapse is left alone, or `None` when nothing is in the way.
fn refused(func: &Func, shape: &Diamond) -> Option<&'static str> {
    // A branch nobody has to take is not a branch worth removing, and a condition that is already
    // known is one `simplify-cfg` turns into a jump, after which the arm that cannot run goes
    // whole. Folding first replaces a branch that costs nothing with an and that costs something.
    //
    // The question is put to `simplify-cfg` rather than answered again here, for the reason its own
    // documentation gives: two answers about when a branch is decided would be two compilers.
    let term = func.terminator(shape.head).expect("the head of a diamond ends in its branch");
    if simplify_cfg::taken(func, term, &Bindings::new()).is_some() {
        return Some(CONDITION_IS_DECIDED);
    }
    for &arm in shape.arms.iter().flatten() {
        for inst in func.insts(arm) {
            if func.is_terminator(inst) {
                continue;
            }
            // Section 22.5's memory rule is inside this one. A load has an effect by this answer,
            // which is what keeps `if (p && p->x)` from becoming a null dereference.
            if func[inst].opcode.has_effects() {
                return Some(RIGHT_HAS_EFFECTS);
            }
            if !speculatable(func, inst) {
                return Some(RIGHT_MAY_TRAP);
            }
        }
    }
    None
}

/// Moves the right operand's work into the head, joins the two bits and jumps.
///
/// The order matters and is the reason this is one function. The branch goes first, so that the
/// work can be appended to the head without anything having to be threaded around a terminator. The
/// and is built after that work has moved, since it reads what the work produced. The jump goes
/// last because it is the terminator, and the arm goes after that, since removing a block while its
/// own jump still named the join would be removing an edge that is still being read.
fn fold(func: &mut Func, shape: &Diamond, plan: &Collapse) {
    let term = func.terminator(shape.head).expect("the head of a diamond ends in its branch");
    let span = func.span(term);
    func.remove_inst(term);
    for &arm in shape.arms.iter().flatten() {
        for inst in func.insts(arm).collect::<Vec<_>>() {
            if func.is_terminator(inst) {
                continue;
            }
            func.remove_inst(inst);
            func.append_inst(shape.head, inst);
        }
    }
    let mut build = Builder::new(func, shape.head).at(span);
    // No flags. Both operands are bits, so there is no wrap to promise anything about, and an and
    // of two truth values is exact whatever either of them turned out to be.
    let cond = build.binary(plan.joined, shape.cond, plan.right, Flags::NONE);
    build.jump(shape.join, &[cond]);
    for &arm in shape.arms.iter().flatten() {
        func.remove_block(arm);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use rucc_base::Interner;
    use rucc_ir::{
        Block, BlockCall, Builder, Extra, Flags, Func, IntPred, MemInfo, MemOrder, Opcode,
        Restrict, Signature, Type, Value,
    };

    use super::ShortCircuit;
    use crate::stats::Kind;
    use crate::{Fuel, Pass, Stats};

    /// Runs the pass with as much fuel as it wants.
    fn collapse(func: &mut Func) -> Stats {
        ShortCircuit.run(func, &mut crate::machine::fixtures::analyses(), &mut Fuel::unlimited())
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

    /// Which block the function ends in, given these numbers for its parameters.
    ///
    /// This is what the strongest of the tests below ask, because what a fold of a branch into an
    /// and has to get right is where control goes and not what the code looks like on the way.
    /// Counting opcodes says the pass built an and. Running every combination of the two operands
    /// through what it built says the and is the right one.
    ///
    /// It is an interpreter of exactly what these functions hold, which is a comparison, a
    /// constant, an addition, the and or the or the pass writes, and the branches. Anything else is
    /// a test that has drifted away from what it is testing, so it stops rather than guesses.
    fn ends_at(func: &Func, inputs: &[i128]) -> usize {
        let mut values: HashMap<Value, i128> = HashMap::new();
        let mut block = func.entry().expect("a function with blocks in it");
        for (&param, &input) in func[block].params.iter().zip(inputs) {
            values.insert(param, input);
        }
        loop {
            let mut end = None;
            for inst in func.insts(block) {
                if func.is_terminator(inst) {
                    end = Some(inst);
                    break;
                }
                let data = func[inst];
                let args: Vec<i128> = func[data.args].iter().map(|arg| values[arg]).collect();
                let result = func[inst].first_result.expect("one result");
                let it = match data.opcode {
                    Opcode::IConst => {
                        let (imm, ty) =
                            crate::fold::constant(func, result).expect("a constant is one");
                        imm.signed(ty)
                    }
                    Opcode::ICmp => {
                        let Extra::IntPred(pred) = data.extra else {
                            panic!("a comparison carries its predicate");
                        };
                        i128::from(match pred {
                            IntPred::Slt => args[0] < args[1],
                            IntPred::Sgt => args[0] > args[1],
                            other => panic!("nothing here compares with {other:?}"),
                        })
                    }
                    Opcode::And => i128::from(args[0] != 0 && args[1] != 0),
                    Opcode::Or => i128::from(args[0] != 0 || args[1] != 0),
                    Opcode::Add => args[0] + args[1],
                    other => panic!("nothing here writes a {other:?}"),
                };
                values.insert(result, it);
            }
            let end = end.expect("every block here ends in a terminator");
            let data = func[end];
            let call = match data.opcode {
                Opcode::Jump => func.successors(end).next().expect("a jump has one edge"),
                Opcode::BrIf => {
                    let cond = values[&func[data.args][0]];
                    let mut edges = func.successors(end);
                    let then = edges.next().expect("a branch has two edges");
                    let other = edges.next().expect("a branch has two edges");
                    if cond == 0 { other } else { then }
                }
                Opcode::Return => return block.index(),
                other => panic!("nothing here ends a block with a {other:?}"),
            };
            let carried: Vec<i128> = func[call.args].iter().map(|arg| values[arg]).collect();
            for (&param, arg) in func[call.block].params.iter().zip(carried) {
                values.insert(param, arg);
            }
            block = call.block;
        }
    }

    /// `if (a && b)` and `if (a || b)`, as the lowering walk writes them.
    ///
    /// Block 0 works out the left operand and branches on it, handing the join the answer straight
    /// away on the side where the left operand settles it. Block 1 works out the right operand and
    /// hands the join that. Block 2 is the join, which branches on the bit, and blocks 3 and 4 are
    /// where it goes, so which one the function ends in says what the whole expression came to.
    ///
    /// `settled` is the bit the short circuiting side carries, and it is the whole difference
    /// between the two shapes. A false is a `&&`, since a first answer of no settles the pair as
    /// no. A true is a `||`.
    fn short_circuit(settled: bool) -> Func {
        let mut names = Interner::new();
        let int = Type::int(32);
        let signature = Signature::new().with_params(&[int, int, int, int]);
        let mut func = Func::new(names.intern("f"), signature);
        let head = func.create_block();
        let left = [func.append_param(head, int), func.append_param(head, int)];
        let right = [func.append_param(head, int), func.append_param(head, int)];
        let arm = func.create_block();
        let join = func.create_block();
        let bit = func.append_param(join, Type::I1);
        let ends = [func.create_block(), func.create_block()];

        let mut build = Builder::new(&mut func, head);
        let test = build.icmp(IntPred::Slt, left[0], left[1]);
        let already = build.iconst(Type::I1, i128::from(settled));
        // The side the left operand settles the answer on is the side it holds on for a `||` and
        // the other one for a `&&`, which is the same thing as saying the arm is on the other one.
        if settled {
            build.br_if(test, join, &[already], arm, &[]);
        } else {
            build.br_if(test, arm, &[], join, &[already]);
        }

        let mut build = Builder::new(&mut func, arm);
        let test = build.icmp(IntPred::Slt, right[0], right[1]);
        build.jump(join, &[test]);

        let mut build = Builder::new(&mut func, join);
        build.br_if(bit, ends[0], &[], ends[1], &[]);
        for block in ends {
            let mut build = Builder::new(&mut func, block);
            build.ret(&[]);
        }
        func
    }

    /// The four numbers that put the two operands of [`short_circuit`] each way round.
    fn both_ways() -> Vec<Vec<i128>> {
        let numbers = |holds: bool| if holds { [0, 1] } else { [1, 0] };
        let mut out = Vec::new();
        for left in [false, true] {
            for right in [false, true] {
                let mut inputs = numbers(left).to_vec();
                inputs.extend(numbers(right));
                out.push(inputs);
            }
        }
        out
    }

    /// Puts these instructions into the arm of [`short_circuit`], above the jump it ends in.
    fn into_the_arm(func: &mut Func, write: impl FnOnce(&mut Builder<'_>)) {
        let arm = Block::from_usize(1);
        let term = func.terminator(arm).expect("the jump to the join");
        func.remove_inst(term);
        let mut build = Builder::new(func, arm);
        write(&mut build);
        func.append_inst(arm, term);
    }

    #[test]
    fn an_and_and_stops_being_a_branch_and_becomes_an_and() {
        let mut func = short_circuit(false);
        let stats = collapse(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::COLLAPSED), 1);
        // The right operand moved up, the and is what the branch was, and the block it was in has
        // gone. The known bit is left for `dce`, which is the pass that removes what nothing uses.
        assert_eq!(
            opcodes(&func, 0),
            vec![Opcode::ICmp, Opcode::IConst, Opcode::ICmp, Opcode::And, Opcode::Jump]
        );
        assert_eq!(goes_to(&func, 0), vec![2]);
        assert_eq!(blocks(&func), vec![0, 2, 3, 4]);
    }

    #[test]
    fn an_or_or_becomes_an_or() {
        let mut func = short_circuit(true);
        let stats = collapse(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::COLLAPSED), 1);
        assert_eq!(
            opcodes(&func, 0),
            vec![Opcode::ICmp, Opcode::IConst, Opcode::ICmp, Opcode::Or, Opcode::Jump]
        );
        assert_eq!(goes_to(&func, 0), vec![2]);
        assert_eq!(blocks(&func), vec![0, 2, 3, 4]);
    }

    #[test]
    fn every_way_the_two_operands_can_go_ends_where_it_did() {
        for settled in [false, true] {
            let before = short_circuit(settled);
            let mut after = short_circuit(settled);
            collapse(&mut after);
            for inputs in both_ways() {
                assert_eq!(
                    ends_at(&before, &inputs),
                    ends_at(&after, &inputs),
                    "the two operands as {inputs:?}, short circuiting on {settled}"
                );
            }
        }
    }

    #[test]
    fn a_right_operand_worked_out_above_the_branch_is_folded() {
        // Both operands are above the branch, so the arm holds nothing and there is nothing to
        // speculate. The estimate is not consulted, because there is nothing for it to price.
        let mut names = Interner::new();
        let int = Type::int(32);
        let signature = Signature::new().with_params(&[int, int]);
        let mut func = Func::new(names.intern("f"), signature);
        let head = func.create_block();
        let left = func.append_param(head, int);
        let right = func.append_param(head, int);
        let arm = func.create_block();
        let join = func.create_block();
        let bit = func.append_param(join, Type::I1);
        let ends = [func.create_block(), func.create_block()];

        let mut build = Builder::new(&mut func, head);
        let first = build.icmp(IntPred::Slt, left, right);
        let second = build.icmp(IntPred::Sgt, left, right);
        let already = build.iconst(Type::I1, 0);
        build.br_if(first, arm, &[], join, &[already]);
        let mut build = Builder::new(&mut func, arm);
        build.jump(join, &[second]);
        let mut build = Builder::new(&mut func, join);
        build.br_if(bit, ends[0], &[], ends[1], &[]);
        for block in ends {
            let mut build = Builder::new(&mut func, block);
            build.ret(&[]);
        }

        let stats = collapse(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::COLLAPSED), 1);
        assert_eq!(stats.count(Kind::Missed, super::BRANCH_IS_PREDICTED), 0);
        assert_eq!(blocks(&func), vec![0, 2, 3, 4]);
    }

    #[test]
    fn a_load_on_the_right_of_an_and_and_keeps_its_branch() {
        // `if (p && p->x)`, which section 22.5 names as the reason the memory rule is not optional.
        // The whole job of the left operand is to stop the load, and a fold that runs the load
        // anyway has written a null dereference into a program that did not have one.
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::PTR]);
        let mut func = Func::new(names.intern("f"), signature);
        let head = func.create_block();
        let pointer = func.append_param(head, Type::PTR);
        let arm = func.create_block();
        let join = func.create_block();
        let bit = func.append_param(join, Type::I1);
        let ends = [func.create_block(), func.create_block()];

        let mut build = Builder::new(&mut func, head);
        let null = build.iconst(Type::int(64), 0);
        let null = build.unary(Opcode::IntToPtr, null, Type::PTR);
        let first = build.icmp(IntPred::Sgt, pointer, null);
        let already = build.iconst(Type::I1, 0);
        build.br_if(first, arm, &[], join, &[already]);
        let mut build = Builder::new(&mut func, arm);
        let info = MemInfo {
            size: 4,
            align: 4,
            order: MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        };
        let field = build.load(Type::int(32), pointer, info, Flags::NONE);
        let zero = build.iconst(Type::int(32), 0);
        let second = build.icmp(IntPred::Sgt, field, zero);
        build.jump(join, &[second]);
        let mut build = Builder::new(&mut func, join);
        build.br_if(bit, ends[0], &[], ends[1], &[]);
        for block in ends {
            let mut build = Builder::new(&mut func, block);
            build.ret(&[]);
        }

        let stats = collapse(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::COLLAPSED), 0);
        assert_eq!(stats.count(Kind::Missed, super::RIGHT_HAS_EFFECTS), 1);
        assert_eq!(blocks(&func), vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn a_right_operand_that_stores_something_keeps_its_branch() {
        let mut func = short_circuit(false);
        into_the_arm(&mut func, |build| {
            let what = build.iconst(Type::int(32), 7);
            let address = build.iconst(Type::int(64), 16);
            let address = build.unary(Opcode::IntToPtr, address, Type::PTR);
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

        let stats = collapse(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::COLLAPSED), 0);
        assert_eq!(stats.count(Kind::Missed, super::RIGHT_HAS_EFFECTS), 1);
    }

    #[test]
    fn a_right_operand_that_divides_by_something_unknown_keeps_its_branch() {
        let mut func = short_circuit(false);
        let operands: Vec<Value> = func[Block::from_usize(0)].params.to_vec();
        into_the_arm(&mut func, |build| {
            // The divisor is a parameter, so the fold would be moving a division that can trap onto
            // a path that was not going to do it.
            build.binary(Opcode::SDiv, operands[2], operands[3], Flags::NONE);
        });

        let stats = collapse(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::COLLAPSED), 0);
        assert_eq!(stats.count(Kind::Missed, super::RIGHT_MAY_TRAP), 1);
    }

    #[test]
    fn a_right_operand_with_more_work_in_it_than_the_budget_keeps_its_branch() {
        let mut func = short_circuit(false);
        into_the_arm(&mut func, |build| {
            let mut it = build.iconst(Type::int(32), 1);
            for _ in 0..2 {
                it = build.binary(Opcode::Add, it, it, Flags::NONE);
            }
        });

        let stats = collapse(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::COLLAPSED), 0);
        assert_eq!(stats.count(Kind::Missed, super::TOO_MUCH_WORK), 1);
        assert_eq!(blocks(&func), vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn a_branch_that_is_already_decided_is_left_for_simplify_cfg() {
        // What `if (1 && b)` looks like by the time it gets here. The condition is not a constant,
        // it is a comparison of two constants, since `fold` will not turn an `icmp` into an `i1`
        // that nothing lowers.
        let mut func = short_circuit(false);
        let head = Block::from_usize(0);
        let already = func.insts(head).nth(1).expect("the bit the branch carries");
        let already = func[already].first_result.expect("a constant is one value");
        let term = func.terminator(head).expect("the branch");
        func.remove_inst(term);
        let mut build = Builder::new(&mut func, head);
        let one = build.iconst(Type::int(32), 1);
        let zero = build.iconst(Type::int(32), 0);
        let decided = build.icmp(IntPred::Sgt, one, zero);
        build.br_if(decided, Block::from_usize(1), &[], Block::from_usize(2), &[already]);

        let stats = collapse(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::COLLAPSED), 0);
        assert_eq!(stats.count(Kind::Missed, super::CONDITION_IS_DECIDED), 1);
        assert_eq!(blocks(&func), vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn a_bit_that_is_known_the_wrong_way_round_is_left_alone() {
        // A false below the branch is `a && b`. A true below it is `!a || b`, which needs a not
        // written, and the module comment says why nothing makes it.
        let mut func = short_circuit(false);
        let head = Block::from_usize(0);
        let term = func.terminator(head).expect("the branch");
        let cond = func[func[term].args][0];
        let edges: Vec<BlockCall> = func.successors(term).collect();
        func.remove_inst(term);
        let mut build = Builder::new(&mut func, head);
        let flipped = build.iconst(Type::I1, 1);
        build.br_if(cond, edges[0].block, &[], edges[1].block, &[flipped]);

        let stats = collapse(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::COLLAPSED), 0);
        assert_eq!(blocks(&func), vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn a_join_carrying_more_than_the_one_bit_is_left_to_phiopt() {
        // Both edges into the join now carry a number as well as the bit, and an and written for
        // the bit alone would leave the branch standing to choose the number.
        let mut func = short_circuit(false);
        let join = Block::from_usize(2);
        func.append_param(join, Type::int(32));
        for block in [Block::from_usize(0), Block::from_usize(1)] {
            let term = func.terminator(block).expect("every block here has one");
            let edges: Vec<BlockCall> = func.successors(term).collect();
            func.remove_inst(term);
            let extra = Builder::new(&mut func, block).iconst(Type::int(32), 5);
            let mut calls = Vec::new();
            for edge in &edges {
                let mut args = func[edge.args].to_vec();
                if edge.block == join {
                    args.push(extra);
                }
                let args = func.push_values(&args);
                calls.push(BlockCall { args, ..*edge });
            }
            let calls = func.push_block_calls(&calls);
            func[term].extra = Extra::Targets(calls);
            func.append_inst(block, term);
        }

        let stats = collapse(&mut func);
        assert_eq!(stats.count(Kind::Optimized, super::COLLAPSED), 0);
        assert_eq!(blocks(&func), vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn fuel_stops_the_fold_where_it_stands() {
        let mut func = short_circuit(false);
        let mut fuel = Fuel::of(0);
        let stats =
            ShortCircuit.run(&mut func, &mut crate::machine::fixtures::analyses(), &mut fuel);
        assert_eq!(stats.count(Kind::Optimized, super::COLLAPSED), 0);
        assert_eq!(stats.count(Kind::Missed, super::NO_FUEL), 1);
        assert_eq!(goes_to(&func, 0), vec![1, 2]);
    }
}
