//! A `switch` whose arms are a function of the label, which is arithmetic and not branches.
//!
//! Design: `spec/optimizer/24-switch-lowering.md` section 24.1, which is the transformation the GCC
//! file `tree-switch-conversion.cc` is named after, and section 24.4, which puts it in the middle
//! end rather than in the lowering and says why: what it produces is ordinary arithmetic that every
//! pass after it optimizes, and what it needs to see is arms whose constancy earlier passes made
//! visible.
//!
//! # The shape
//!
//! ```c
//! switch (x) { case 0: return 1; case 1: return 2; case 2: return 3; case 3: return 4; }
//! return 0;
//! ```
//!
//! Four labels, four arms, and the arm for label `k` gives `k + 1`. The labels run consecutively
//! and the answers run consecutively with them, so the whole statement is one range check and one
//! addition. gcc reduces thirty three labels of this to a comparison and a `lea`, which is
//! tamnd/rucc#728, and rucc emitted a comparison and a jump per label.
//!
//! What is here is the case where the answers are an affine function of the label, `a * x + b`.
//! That covers the shape above with `a` of one and `b` of one, the shape where every arm gives the
//! same answer with `a` of zero, and the scaled ones in between. The case where the answers are
//! arbitrary constants is a lookup table in a read only section, which is section 24.4's other half
//! and is not here.
//!
//! # What it rewrites and what it leaves
//!
//! The `switch` stays a `switch`. Every case edge is pointed at one new block, which works the
//! answer out and hands it on, and the default edge is not touched at all. What that buys is that
//! the range check is not written here: a `switch` whose cases are consecutive and all go to one
//! place is exactly `crates/rucc-codegen/src/switch.rs`'s `Cluster::Run`, which is one subtraction
//! and one unsigned comparison however long the run is, and which already gets the modular
//! arithmetic and the run that covers a whole type right. Writing a second range check here would
//! be a second place for section 24.6's overflow to be got wrong.
//!
//! The default is untouched for the reason section 24.6 gives, which is that the default is never
//! dropped. A value that matches no case went to the default before this ran and goes to the same
//! place afterwards, because the edge it goes down is the same edge.
//!
//! # What has to be true
//!
//! The labels are consecutive. Not a simplification: a hole in the labels is a value the range
//! check lets through and the arithmetic then answers, where the program said it should have gone
//! to the default.
//!
//! Every arm is a block nothing else reaches, holding nothing but the constants it hands on, and
//! ending the same way as every other arm. The same way means a jump to the same block, or a
//! return, and in either case with the same values in every position but one. That one is the
//! answer. Section 24.5 gives up on arms that assign more than one thing and so does this.
//!
//! The answers are `a * label + b` at every label, checked at every label rather than fitted to two
//! of them and believed. The check is done in the answer's own width with wrapping, because that is
//! what the arithmetic this writes will do, and the arithmetic is written with no flags on it so
//! that wrapping is what it is allowed to do.
//!
//! The label and the answer are the same width. A `switch` on an `int` whose arms give a `long` is
//! the same transformation with a widening in front of the multiply, and which widening it is
//! depends on how the label is read, which is a question this would have to answer and currently
//! declines to ask.
//!
//! # Why three labels and not two
//!
//! Two labels and a default is a shape `phiopt` already has something to say about, and what it
//! says is a `select` between two constants that cost nothing to materialize. The arithmetic this
//! writes is a multiply and an add against a range check, which is not obviously better than that
//! and is worse when `a` is not one. From three labels up the chain being replaced is at least six
//! instructions and what replaces it is at most five, so it is a win at three and grows from there.

use std::collections::HashSet;

use rucc_ir::{Block, BlockCall, Builder, Extra, Flags, Func, Imm, Inst, Opcode, Type, Value};

use crate::cfg::Cfg;
use crate::{Analyses, Fuel, Pass, Preserved, Stats};

/// What is reported when a `switch` becomes arithmetic.
const CONVERTED: &str = "switch replaced by a range check and the arithmetic its arms were doing";

/// What is reported when the pass ran out of fuel with a `switch` it was about to convert.
const NO_FUEL: &str = "switch left alone, the pass ran out of fuel";

/// What is reported for a `switch` with too few labels to pay for the arithmetic.
const TOO_FEW: &str = "switch left alone, it has too few labels for arithmetic to be cheaper";

/// What is reported for a `switch` whose labels have holes in them.
const NOT_CONSECUTIVE: &str = "switch left alone, its labels are not consecutive";

/// What is reported for a `switch` with an arm that is not a block of its own.
const ARM_IS_SHARED: &str = "switch left alone, an arm is reached from somewhere other than it";

/// What is reported for a `switch` with an arm that does something.
const ARM_DOES_WORK: &str = "switch left alone, an arm does more than work out a constant";

/// What is reported for a `switch` whose arms do not end alike.
const ARMS_DIFFER: &str = "switch left alone, its arms do not all hand on the same thing";

/// What is reported for a `switch` whose answers are not a line.
const NOT_AFFINE: &str = "switch left alone, its answers are not a fixed multiple of the label \
                          plus a constant";

/// What is reported for a `switch` whose answers are a different width from its labels.
const WIDTHS_DIFFER: &str = "switch left alone, its answers are not as wide as its labels";

/// The fewest labels worth converting, per the module documentation.
const LABELS: usize = 3;

/// The pass.
#[derive(Debug)]
pub struct SwitchConv;

impl Pass for SwitchConv {
    fn name(&self) -> &'static str {
        "switch-conv"
    }

    fn describe(&self) -> &'static str {
        "a switch whose arms are a fixed multiple of the label becomes a range check and arithmetic"
    }

    fn preserves(&self) -> Preserved {
        // A block appears, the arms go, and every case edge moves.
        Preserved::NONE
    }

    fn run(&self, func: &mut Func, an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        if func.entry().is_none() {
            return stats;
        }
        let cfg = an.cfg(func);
        let found: Vec<Inst> = func
            .blocks()
            .filter_map(|block| func.terminator(block))
            .filter(|&inst| func[inst].opcode == Opcode::Switch)
            .collect();

        let mut plans = Vec::new();
        for inst in found {
            match plan(func, cfg, inst) {
                Ok(plan) => plans.push(plan),
                Err(why) => stats.missed(why),
            }
        }

        let mut changed = false;
        for plan in plans {
            if !fuel.take() {
                stats.missed(NO_FUEL);
                continue;
            }
            apply(func, &plan);
            stats.optimized(CONVERTED);
            changed = true;
        }
        if changed {
            an.clear();
        }
        stats
    }
}

/// How the arms of one `switch` hand their answer on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hands {
    /// To this block, as one of its parameters.
    On(Block),
    /// Out of the function, as one of its results.
    Back,
}

/// One `switch` and what it is about to become.
#[derive(Debug)]
struct Plan {
    /// The `switch` itself.
    inst: Inst,
    /// What it switches on, which is what the arithmetic is a function of.
    value: Value,
    /// The width of the label and of the answer, which this pass requires to be one width.
    ty: Type,
    /// Where the answer goes.
    hands: Hands,
    /// What every arm handed on, with the answer's position holding whatever the first arm had
    /// there. That position is rewritten and the rest are passed on as they were.
    args: Vec<Value>,
    /// Which of `args` is the answer.
    answer: usize,
    /// The multiple of the label.
    scale: i128,
    /// What is added to it.
    offset: i128,
    /// The blocks the arms were, which nothing reaches once the case edges have moved.
    arms: Vec<Block>,
}

/// What one `switch` becomes, or why it stays as it is.
fn plan(func: &Func, cfg: &Cfg, inst: Inst) -> Result<Plan, &'static str> {
    let Extra::Switch(info) = func[inst].extra else { return Err(ARMS_DIFFER) };
    let info = func[info];
    let Some(&value) = func[func[inst].args].first() else { return Err(ARMS_DIFFER) };
    let ty = func[value].ty;
    if !ty.is_int() {
        return Err(WIDTHS_DIFFER);
    }
    let calls: Vec<BlockCall> = func[info.targets].to_vec();
    let labels: Vec<i128> = func[info.cases].iter().map(|imm| imm.signed(ty)).collect();
    let Some((&default, arms)) = calls.split_first() else { return Err(ARMS_DIFFER) };
    if arms.len() != labels.len() || arms.len() < LABELS {
        return Err(TOO_FEW);
    }
    // A block that is both an arm and the default is not an arm this may take away, and it looks
    // like one from here: the predecessor count below says one, because one block reaching another
    // down two edges is one predecessor, and the arm being removed would take the default with it.
    if arms.iter().any(|call| call.block == default.block) {
        return Err(ARM_IS_SHARED);
    }

    // Consecutive and ascending. The front end sorts nothing, so this is asked of the list as it
    // arrived rather than of a sorted copy: what is wanted is that the labels are a run, and a run
    // read out of order is still a run only if it is sorted first, which is work this declines to
    // do before it knows the answers are a line.
    for pair in labels.windows(2) {
        if pair[1].checked_sub(pair[0]) != Some(1) {
            return Err(NOT_CONSECUTIVE);
        }
    }

    // Every arm is a block of its own that works out constants and hands them on, and the way it
    // hands them on is the way every other arm does.
    let mut hands = None;
    let mut shared: Option<Vec<Value>> = None;
    let mut answer = None;
    let mut answers = Vec::new();
    for call in arms {
        if !call.args.is_empty() {
            return Err(ARM_DOES_WORK);
        }
        if cfg.predecessors(call.block).len() != 1 {
            return Err(ARM_IS_SHARED);
        }
        let (way, args) = tail(func, call.block)?;
        if *hands.get_or_insert(way) != way {
            return Err(ARMS_DIFFER);
        }
        let previous = shared.get_or_insert_with(|| args.clone());
        if previous.len() != args.len() {
            return Err(ARMS_DIFFER);
        }
        // The one position they disagree about is the answer, and it is the same position every
        // time. The first arm sets nothing, since it agrees with itself everywhere.
        for (index, (&mine, &theirs)) in previous.iter().zip(&args).enumerate() {
            if mine == theirs {
                continue;
            }
            if *answer.get_or_insert(index) != index {
                return Err(ARMS_DIFFER);
            }
        }
        let at = answer.unwrap_or(0);
        let Some(&handed) = args.get(at) else { return Err(ARMS_DIFFER) };
        if func[handed].ty != ty {
            return Err(WIDTHS_DIFFER);
        }
        let Some(number) = constant(func, handed) else { return Err(NOT_AFFINE) };
        answers.push(number);
    }
    let (Some(hands), Some(args)) = (hands, shared) else { return Err(ARMS_DIFFER) };
    let answer = answer.ok_or(NOT_AFFINE)?;

    let (scale, offset) = line(&labels, &answers, ty).ok_or(NOT_AFFINE)?;
    Ok(Plan {
        inst,
        value,
        ty,
        hands,
        args,
        answer,
        scale,
        offset,
        arms: arms.iter().map(|call| call.block).collect(),
    })
}

/// What a block hands on, when handing something on is the whole of what it does.
///
/// Every instruction in it but the last has to be a constant, because the last one is about to be
/// written somewhere else and anything the block worked out for it would be left behind. A constant
/// is the exception because a constant is rewritten rather than moved.
fn tail(func: &Func, block: Block) -> Result<(Hands, Vec<Value>), &'static str> {
    let Some(last) = func.terminator(block) else { return Err(ARM_DOES_WORK) };
    for inst in func.insts(block) {
        if inst != last && func[inst].opcode != Opcode::IConst {
            return Err(ARM_DOES_WORK);
        }
    }
    let args: Vec<Value> = match func[last].opcode {
        Opcode::Jump => {
            let Some(call) = func.successors(last).next() else { return Err(ARM_DOES_WORK) };
            let args = func[call.args].to_vec();
            return Ok((Hands::On(call.block), args));
        }
        Opcode::Return => func[func[last].args].to_vec(),
        _ => return Err(ARM_DOES_WORK),
    };
    Ok((Hands::Back, args))
}

/// The value of an integer constant, read with its own sign.
fn constant(func: &Func, value: Value) -> Option<i128> {
    crate::discharge::constant(func, value)
}

/// The multiple and the offset that give every answer from its label, when one pair does.
///
/// Fitted to the first two labels, which is exact because they are one apart, and then checked at
/// every label including those two. Checked rather than trusted because the arithmetic that is
/// about to be written wraps at the type's width, and a fit that is right about the numbers and
/// wrong about the wrapping is a miscompile that only shows up at the ends of the range.
fn line(labels: &[i128], answers: &[i128], ty: Type) -> Option<(i128, i128)> {
    let [first, second, ..] = *labels else { return None };
    let [low, high, ..] = *answers else { return None };
    debug_assert_eq!(second - first, 1, "the labels were checked to be consecutive");
    let scale = high.checked_sub(low)?;
    let offset = low.checked_sub(scale.checked_mul(first)?)?;
    for (&label, &answer) in labels.iter().zip(answers) {
        let want = scale.checked_mul(label)?.checked_add(offset)?;
        if wrap(want, ty) != answer {
            return None;
        }
    }
    Some((scale, offset))
}

/// A number as the machine will hold it at that width, read back with its own sign.
///
/// An immediate is stored in exactly the width its type has, so building one and reading it back is
/// the truncation, and it is the same one every other part of the compiler uses.
fn wrap(value: i128, ty: Type) -> i128 {
    Imm::int(value, ty).signed(ty)
}

/// Writes the block the arms become and points every case edge at it.
fn apply(func: &mut Func, plan: &Plan) {
    let span = func.span(plan.inst);
    let hit = func.create_block();
    let mut builder = Builder::new(func, hit).at(span);
    let scaled = match plan.scale {
        0 => builder.iconst(plan.ty, plan.offset),
        1 => plan.value,
        scale => {
            let by = builder.iconst(plan.ty, scale);
            builder.binary(Opcode::Mul, plan.value, by, Flags::NONE)
        }
    };
    let answer = if plan.offset == 0 || plan.scale == 0 {
        scaled
    } else {
        let by = builder.iconst(plan.ty, plan.offset);
        builder.binary(Opcode::Add, scaled, by, Flags::NONE)
    };
    let mut args = plan.args.clone();
    args[plan.answer] = answer;
    match plan.hands {
        Hands::On(block) => builder.jump(block, &args),
        Hands::Back => builder.ret(&args),
    };

    // Every case edge, and only the case edges: the default is the first target and stays where it
    // was pointing.
    let Extra::Switch(info) = func[plan.inst].extra else { return };
    let empty = func.push_values(&[]);
    let mut calls: Vec<BlockCall> = func[func[info].targets].to_vec();
    for call in &mut calls[1..] {
        // No hint: the cases that had one had one each, and a single edge standing for all of them
        // cannot carry a number that was true of one arm.
        *call = BlockCall::new(hit, empty);
    }
    let targets = func.push_block_calls(&calls);
    let cases = func[info].cases;
    let info = func.add_switch(rucc_ir::SwitchInfo { targets, cases });
    func[plan.inst].extra = Extra::Switch(info);

    // The arms are unreachable now. Two labels sharing one arm is a shape that survives the checks
    // above only when the answer does not depend on the label, so the same block can be here twice.
    let mut gone = HashSet::new();
    for &arm in &plan.arms {
        if gone.insert(arm) {
            func.remove_block(arm);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use rucc_base::Interner;
    use rucc_ir::{Block, Builder, Func, Opcode, Signature, Type, Value};

    use super::SwitchConv;
    use crate::stats::Kind;
    use crate::{Fuel, Pass, Stats};

    /// The width everything here switches on and answers in, unless a test says otherwise.
    fn i32() -> Type {
        Type::int(32)
    }

    /// Runs the pass with as much fuel as it wants.
    fn convert(func: &mut Func) -> Stats {
        SwitchConv.run(func, &mut crate::machine::fixtures::analyses(), &mut Fuel::unlimited())
    }

    /// A function that switches on its parameter and returns a constant per label.
    ///
    /// The default returns a constant of its own that is not on any line these tests fit, so a
    /// test that says the pass fired is saying it fired on the cases and not on the whole thing.
    fn returning(ty: Type, labels: &[i128], answers: &[i128]) -> Func {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let head = func.create_block();
        let value = func.append_param(head, ty);
        let default = func.create_block();
        let arms: Vec<Block> = answers.iter().map(|_| func.create_block()).collect();
        for (&arm, &answer) in arms.iter().zip(answers) {
            let mut build = Builder::new(&mut func, arm);
            let it = build.iconst(ty, answer);
            build.ret(&[it]);
        }
        let mut build = Builder::new(&mut func, default);
        let it = build.iconst(ty, 999);
        build.ret(&[it]);
        let cases: Vec<(i128, Block)> = labels.iter().copied().zip(arms.iter().copied()).collect();
        Builder::new(&mut func, head).switch(value, default, &cases);
        func
    }

    /// The blocks every case edge goes to, which is one block when the pass has fired.
    fn cases(func: &Func) -> Vec<usize> {
        let head = func.entry().expect("a function with blocks in it");
        let term = func.terminator(head).expect("a head block has one");
        func.successors(term).skip(1).map(|call| call.block.index()).collect()
    }

    /// The block every case edge goes to, when there is exactly one of them.
    fn arm(func: &Func) -> Block {
        let blocks = cases(func);
        let first = blocks[0];
        assert!(blocks.iter().all(|&block| block == first), "the case edges did not all move");
        Block::from_usize(first)
    }

    /// The opcodes a block holds, in order.
    fn opcodes(func: &Func, block: Block) -> Vec<Opcode> {
        func.insts(block).map(|inst| func[inst].opcode).collect()
    }

    /// What the block the case edges go to answers, given that label.
    ///
    /// An interpreter of exactly the three instructions this pass writes, because what the pass
    /// has to get right is the number and not the shape. Anything else in the block is a test
    /// that has drifted away from what it is testing, so it stops rather than guesses.
    fn answer(func: &Func, block: Block, label: i128) -> i128 {
        let head = func.entry().expect("a function with blocks in it");
        let mut values: HashMap<Value, i128> = HashMap::new();
        values.insert(func[head].params[0], label);
        for inst in func.insts(block) {
            let data = func[inst];
            let Some(result) = data.first_result else {
                let args = func[data.args].to_vec();
                let handed = match data.opcode {
                    Opcode::Return => args[0],
                    Opcode::Jump => {
                        func[func.successors(inst).next().expect("a jump goes").args][0]
                    }
                    other => panic!("a block this pass wrote ends in {other:?}"),
                };
                return values[&handed];
            };
            let args: Vec<i128> = func[data.args].iter().map(|arg| values[arg]).collect();
            let it = match data.opcode {
                Opcode::IConst => {
                    let (imm, ty) = crate::fold::constant(func, result).expect("a constant is one");
                    imm.signed(ty)
                }
                Opcode::Mul => args[0].wrapping_mul(args[1]),
                Opcode::Add => args[0].wrapping_add(args[1]),
                other => panic!("this pass does not write {other:?}"),
            };
            values.insert(result, super::wrap(it, func[result].ty));
        }
        panic!("a block with no terminator");
    }

    /// Whether the pass says it changed the function.
    fn fired(stats: &Stats) -> bool {
        stats.total(Kind::Optimized) > 0
    }

    #[test]
    fn labels_that_run_with_their_answers_become_one_addition() {
        let mut func = returning(i32(), &[0, 1, 2, 3], &[1, 2, 3, 4]);
        assert!(fired(&convert(&mut func)));
        let arm = arm(&func);
        assert_eq!(opcodes(&func, arm), [Opcode::IConst, Opcode::Add, Opcode::Return]);
        for label in 0..4 {
            assert_eq!(answer(&func, arm, label), label + 1);
        }
    }

    #[test]
    fn answers_that_are_a_multiple_of_the_label_become_a_multiplication() {
        let mut func = returning(i32(), &[3, 4, 5, 6], &[30, 40, 50, 60]);
        assert!(fired(&convert(&mut func)));
        let arm = arm(&func);
        assert_eq!(opcodes(&func, arm), [Opcode::IConst, Opcode::Mul, Opcode::Return]);
        for label in 3..7 {
            assert_eq!(answer(&func, arm, label), label * 10);
        }
    }

    #[test]
    fn answers_that_are_all_the_same_become_the_constant_they_all_were() {
        let mut func = returning(i32(), &[7, 8, 9, 10], &[9, 9, 9, 9]);
        assert!(fired(&convert(&mut func)));
        let arm = arm(&func);
        assert_eq!(opcodes(&func, arm), [Opcode::IConst, Opcode::Return]);
        assert_eq!(answer(&func, arm, 8), 9);
    }

    #[test]
    fn labels_that_run_below_zero_are_a_run_like_any_other() {
        let mut func = returning(i32(), &[-2, -1, 0, 1], &[-4, -2, 0, 2]);
        assert!(fired(&convert(&mut func)));
        let arm = arm(&func);
        for label in -2..2 {
            assert_eq!(answer(&func, arm, label), label * 2);
        }
    }

    /// The line has to hold at the type's width and not at the arithmetic's.
    ///
    /// A hundred times two is two hundred, which is not a number an `i8` holds, and the answer the
    /// program gave at that label is what two hundred comes to there. The pass writes a
    /// multiplication with no flags on it, which wraps the same way, so this is a fit and not a
    /// refusal, and the number is the point of the test.
    #[test]
    fn a_line_that_only_holds_by_wrapping_still_holds() {
        let ty = Type::int(8);
        let mut func = returning(ty, &[0, 1, 2], &[0, 100, -56]);
        assert!(fired(&convert(&mut func)));
        let arm = arm(&func);
        assert_eq!(answer(&func, arm, 2), -56);
    }

    #[test]
    fn labels_with_a_hole_in_them_are_left_alone() {
        let mut func = returning(i32(), &[0, 1, 3], &[1, 2, 4]);
        assert!(!fired(&convert(&mut func)));
        assert_eq!(cases(&func).len(), 3);
    }

    #[test]
    fn answers_that_are_not_a_line_are_left_alone() {
        let mut func = returning(i32(), &[0, 1, 2], &[5, 9, 2]);
        assert!(!fired(&convert(&mut func)));
    }

    #[test]
    fn two_labels_are_not_enough_to_pay_for_the_arithmetic() {
        let mut func = returning(i32(), &[0, 1], &[1, 2]);
        assert!(!fired(&convert(&mut func)));
    }

    #[test]
    fn an_answer_wider_than_its_label_is_left_alone() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let head = func.create_block();
        let value = func.append_param(head, i32());
        let default = func.create_block();
        let arms: Vec<Block> = (0..3).map(|_| func.create_block()).collect();
        for (index, &arm) in arms.iter().enumerate() {
            let mut build = Builder::new(&mut func, arm);
            let it = build.iconst(Type::int(64), index as i128 + 1);
            build.ret(&[it]);
        }
        let mut build = Builder::new(&mut func, default);
        let it = build.iconst(Type::int(64), 0);
        build.ret(&[it]);
        let cases: Vec<(i128, Block)> = (0..3).zip(arms.iter().copied()).collect();
        Builder::new(&mut func, head).switch(value, default, &cases);
        assert!(!fired(&convert(&mut func)));
    }

    #[test]
    fn an_arm_something_else_reaches_is_left_alone() {
        let mut func = returning(i32(), &[0, 1, 2], &[1, 2, 3]);
        // The default jumps into the first arm instead of returning, so the arm is a block two
        // edges arrive at and is not one this may take away.
        let default = Block::from_usize(1);
        let arm = Block::from_usize(2);
        let term = func.terminator(default).expect("the default returns");
        func.remove_inst(term);
        Builder::new(&mut func, default).jump(arm, &[]);
        assert!(!fired(&convert(&mut func)));
    }

    #[test]
    fn an_arm_that_is_also_the_default_is_left_alone() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let head = func.create_block();
        let value = func.append_param(head, i32());
        let shared = func.create_block();
        let mut build = Builder::new(&mut func, shared);
        let it = build.iconst(i32(), 1);
        build.ret(&[it]);
        let others: Vec<Block> = (0..2).map(|_| func.create_block()).collect();
        for (index, &arm) in others.iter().enumerate() {
            let mut build = Builder::new(&mut func, arm);
            let it = build.iconst(i32(), index as i128 + 2);
            build.ret(&[it]);
        }
        let cases = [(0, shared), (1, others[0]), (2, others[1])];
        Builder::new(&mut func, head).switch(value, shared, &cases);
        assert!(!fired(&convert(&mut func)));
    }

    #[test]
    fn arms_that_join_keep_what_they_pass_beside_the_answer() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let head = func.create_block();
        let value = func.append_param(head, i32());
        let alongside = func.append_param(head, i32());
        let join = func.create_block();
        let handed = func.append_param(join, i32());
        let carried = func.append_param(join, i32());
        Builder::new(&mut func, join).ret(&[handed, carried]);
        let default = func.create_block();
        let mut build = Builder::new(&mut func, default);
        let it = build.iconst(i32(), 999);
        build.jump(join, &[it, alongside]);
        let arms: Vec<Block> = (0..3).map(|_| func.create_block()).collect();
        for (index, &arm) in arms.iter().enumerate() {
            let mut build = Builder::new(&mut func, arm);
            let it = build.iconst(i32(), index as i128 + 1);
            build.jump(join, &[it, alongside]);
        }
        let cases: Vec<(i128, Block)> = (0..3).zip(arms.iter().copied()).collect();
        Builder::new(&mut func, head).switch(value, default, &cases);
        assert!(fired(&convert(&mut func)));

        let arm = arm(&func);
        assert_eq!(answer(&func, arm, 2), 3);
        // The second argument is what it always was, which is the parameter every arm passed.
        let term = func.terminator(arm).expect("the block ends in a jump");
        let call = func.successors(term).next().expect("a jump goes somewhere");
        assert_eq!(func[call.args][1], alongside);
    }

    #[test]
    fn arms_that_hand_on_two_different_things_are_left_alone() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let head = func.create_block();
        let value = func.append_param(head, i32());
        let join = func.create_block();
        let first = func.append_param(join, i32());
        let second = func.append_param(join, i32());
        Builder::new(&mut func, join).ret(&[first, second]);
        let default = func.create_block();
        let mut build = Builder::new(&mut func, default);
        let it = build.iconst(i32(), 999);
        build.jump(join, &[it, it]);
        let arms: Vec<Block> = (0..3).map(|_| func.create_block()).collect();
        for (index, &arm) in arms.iter().enumerate() {
            let mut build = Builder::new(&mut func, arm);
            let one = build.iconst(i32(), index as i128 + 1);
            let two = build.iconst(i32(), index as i128 + 10);
            build.jump(join, &[one, two]);
        }
        let cases: Vec<(i128, Block)> = (0..3).zip(arms.iter().copied()).collect();
        Builder::new(&mut func, head).switch(value, default, &cases);
        assert!(!fired(&convert(&mut func)));
    }

    #[test]
    fn an_arm_that_does_something_is_left_alone() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let head = func.create_block();
        let value = func.append_param(head, i32());
        let default = func.create_block();
        let mut build = Builder::new(&mut func, default);
        let it = build.iconst(i32(), 999);
        build.ret(&[it]);
        let arms: Vec<Block> = (0..3).map(|_| func.create_block()).collect();
        for (index, &arm) in arms.iter().enumerate() {
            let mut build = Builder::new(&mut func, arm);
            let it = build.iconst(i32(), index as i128 + 1);
            // An addition the arm did, which is work the answer would have been left without.
            let sum = build.binary(Opcode::Add, it, value, rucc_ir::Flags::NONE);
            build.ret(&[sum]);
        }
        let cases: Vec<(i128, Block)> = (0..3).zip(arms.iter().copied()).collect();
        Builder::new(&mut func, head).switch(value, default, &cases);
        assert!(!fired(&convert(&mut func)));
    }

    #[test]
    fn the_default_goes_where_it_went() {
        let mut func = returning(i32(), &[0, 1, 2, 3], &[1, 2, 3, 4]);
        let head = func.entry().expect("a function with blocks in it");
        let before = func.terminator(head).expect("a head block has one");
        let was = func.successors(before).next().expect("a switch has a default").block;
        assert!(fired(&convert(&mut func)));
        let after = func.terminator(head).expect("a head block has one");
        let now = func.successors(after).next().expect("a switch has a default").block;
        assert_eq!(was, now, "the default moved");
    }
}
