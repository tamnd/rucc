//! What `__builtin_expect` said, moved onto the branch it was said about.
//!
//! Design: `spec/optimizer/11-profile.md` section 11.2, and tamnd/rucc#364.
//!
//! ```c
//! if (__builtin_expect(error, 0)) { report(); }
//! ```
//!
//! The front end builds an `expect` instruction holding the value and what the program says it will
//! be. The value is what the branch is on, and this pass is what turns the pair into a statement
//! about the branch: the arm the hint names gets ninety parts in a hundred, the other gets ten, and
//! the instruction comes out. Everything downstream reads the number off the arm rather than
//! chasing the condition back to a node, which is what makes a profile and a hint the same thing to
//! everything that consumes either.
//!
//! # Why it runs first and at every level
//!
//! An `expect` sits on the branch condition, and a wrapper on a branch condition is a wrapper on
//! whatever the peephole, the folder and the comparison simplifier were about to match. Left
//! standing through a pipeline it would cost code quality on exactly the programs that took the
//! trouble to say which way their branches go, which is the wrong way round. So it comes off in the
//! first pass, before anything has had a chance to fail to match through it.
//!
//! At `-O0` too, where nothing else runs, for the same reason a stray node is a cost there as well:
//! `-O0` emits what it is given, and what it is given would otherwise have an instruction in it per
//! `__builtin_expect` the program wrote. gcc has no such instruction at any level.
//!
//! # What it does not do
//!
//! It does not predict anything. `crate::predict` is where the prediction is, this only records
//! what the program claimed, and the difference matters: a hint is a fact about what somebody wrote
//! and a prediction is a guess, so the ten static predictors write no hints at all. A hint that a
//! heuristic could have written would be indistinguishable afterwards from one the program did.
//!
//! It does not touch a branch whose condition it cannot follow back to an `expect`, and it removes
//! the instruction either way. A hint nothing can place is a hint nothing can use, and leaving the
//! instruction standing for a later pass to find would be leaving the wrapper in the way of
//! everything, which is the thing this exists to avoid.

use std::collections::HashMap;

use rucc_cost::heuristics::PREDICT_EXPECT;
use rucc_ir::{Block, Def, Extra, Func, Hint, Inst, IntPred, Opcode, Value};

use crate::fold::constant;
use crate::{Analyses, Analysis, Fuel, Pass, Preserved, Stats, uses};

/// A branch now says which way the program expects it to go.
const PLACED: &str = "branch weight written from a __builtin_expect on its condition";

/// The value was expected and nothing branches on it.
const NO_BRANCH: &str = "__builtin_expect dropped, no branch in this function is on its value";

/// Ran out.
const NO_FUEL: &str = "__builtin_expect kept, the pass ran out of fuel";

/// The pass.
#[derive(Debug)]
pub struct Expect;

impl Pass for Expect {
    fn name(&self) -> &'static str {
        "expect"
    }

    fn describe(&self) -> &'static str {
        "what __builtin_expect said moves onto the arms of the branch it was said about"
    }

    fn preserves(&self) -> Preserved {
        // No block moves and no edge moves, so the graph and everything built on it stand. What
        // changes is who reads which value, which liveness is a statement about, and what the
        // branches say about themselves, which is what the frequencies are worked out from.
        Preserved::ALL.without(Analysis::Liveness).without(Analysis::Frequencies)
    }

    fn required(&self) -> bool {
        // This is the only thing that removes an `Opcode::Expect`, and the back end has no rule
        // that lowers one, so `-fno-expect` would not be a compile without the hints. It would be
        // a compile that stops on a construct the program never wrote.
        true
    }

    fn run(&self, func: &mut Func, _an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        let mut hints: Vec<Inst> = Vec::new();
        for block in func.blocks().collect::<Vec<Block>>() {
            for inst in func.insts(block) {
                if func[inst].opcode == Opcode::Expect {
                    hints.push(inst);
                }
            }
        }
        // The whole of almost every function, since almost no program says anything about its
        // branches, and the walk above is the only cost this pass has on one that does not.
        if hints.is_empty() {
            return stats;
        }

        let mut placed = 0;
        for block in func.blocks().collect::<Vec<Block>>() {
            let Some(term) = func.terminator(block) else { continue };
            if func[term].opcode != Opcode::BrIf {
                continue;
            }
            let Some(&cond) = func[func[term].args].first() else { continue };
            let Some((inst, sense)) = through(func, cond) else { continue };
            let Some(parts) = claim(func, inst, sense) else { continue };
            if !fuel.take() {
                stats.missed(NO_FUEL);
                break;
            }
            write(func, term, parts);
            stats.optimized(PLACED);
            placed += 1;
        }

        // Every one of them, and not only the ones a branch was found for. The instruction has done
        // whatever it is going to do by this point, and what it would do from here is sit in front
        // of the folder.
        let mut forward: HashMap<Value, Value> = HashMap::new();
        for &inst in &hints {
            let args = &func[func[inst].args];
            let (Some(&result), Some(&value)) = (func[inst].first_result.as_ref(), args.first())
            else {
                continue;
            };
            forward.insert(result, value);
        }
        uses::substitute(func, &forward);
        for &inst in &hints {
            func.remove_inst(inst);
        }
        for _ in placed..hints.len() {
            stats.note(NO_BRANCH);
        }
        stats
    }
}

/// The `expect` a branch condition comes from, and whether the condition is the expectation being
/// met or its opposite.
///
/// A condition is one bit and an `expect` is as wide as the `long` the prototype converted its
/// arguments to, so there is always something in between. What the lowering walk writes is a
/// comparison against zero, and what the peephole writes for `!x` is the same comparison the other
/// way round, so the chain is comparisons against zero and widenings that do not change whether a
/// value is zero.
fn through(func: &Func, cond: Value) -> Option<(Inst, bool)> {
    let mut value = cond;
    let mut sense = true;
    // Bounded, because a chain this walks is a chain of instructions the function has and each step
    // moves to the operand of the one before it.
    loop {
        let Def::Result { inst, .. } = func[value].def else { return None };
        let data = &func[inst];
        match data.opcode {
            Opcode::Expect => return Some((inst, sense)),
            // A widening keeps a value zero and keeps it non-zero, whichever bit pattern it makes.
            Opcode::ZExt | Opcode::SExt => value = *func[data.args].first()?,
            Opcode::ICmp => {
                let Extra::IntPred(pred) = data.extra else { return None };
                let args = &func[data.args];
                let lhs = *args.first()?;
                let rhs = *args.get(1)?;
                if literal(func, rhs)? != 0 {
                    return None;
                }
                match pred {
                    IntPred::Ne => {}
                    IntPred::Eq => sense = !sense,
                    _ => return None,
                }
                value = lhs;
            }
            _ => return None,
        }
    }
}

/// How often the first arm of the branch is taken, in parts of [`Hint::SCALE`], given what the
/// `expect` says and which way round the condition is.
///
/// The probability is the program's where it wrote one and ninety percent otherwise, which is
/// GCC's `param_builtin_expect_probability` and is section 11.2's number. It is the probability
/// that the value turns out to be the hint, so a hint of zero is the same claim about the other
/// arm and the complement is what the branch gets.
fn claim(func: &Func, inst: Inst, sense: bool) -> Option<u32> {
    let args = &func[func[inst].args];
    let value = literal(func, *args.get(1)?)?;
    let parts = match args.get(2) {
        Some(&given) => u32::try_from(literal(func, given)?).ok()?.min(Hint::SCALE),
        None => PREDICT_EXPECT * Hint::SCALE / 100,
    };
    let met = (value != 0) == sense;
    Some(if met { parts } else { Hint::SCALE - parts })
}

/// The constant a value is, looking through the widenings a converted argument arrives behind.
///
/// `__builtin_expect(x, 0)` writes its second argument as an `int` and the prototype converts it to
/// `long`, so what the lowering walk leaves is a sign extension of a constant and not a constant.
/// This pass runs before anything that would fold one away, which is the point of it running first,
/// so the walk is here rather than left to the folder.
///
/// A widening only, because it is the one conversion whose answer is the value it was given. A
/// truncation is not: it can turn a value that is not zero into one that is, which would be the pass
/// reading a hint the program did not write.
fn literal(func: &Func, value: Value) -> Option<i128> {
    let mut value = value;
    loop {
        if let Some((bits, ty)) = constant(func, value) {
            return Some(bits.signed(ty));
        }
        let Def::Result { inst, .. } = func[value].def else { return None };
        let data = &func[inst];
        match data.opcode {
            Opcode::ZExt | Opcode::SExt => value = *func[data.args].first()?,
            _ => return None,
        }
    }
}

/// Writes the claim onto the two arms of a branch, so that the pair sums to certainty.
fn write(func: &mut Func, term: Inst, parts: u32) {
    let hint = Hint::parts(parts);
    for (at, hint) in func.target_list(term).iter().zip([hint, hint.complement()]) {
        let call = func[at];
        func.set_block_call(at, rucc_ir::BlockCall { hint, ..call });
    }
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{Builder, InstData, Signature, Type};

    use super::*;

    /// A function with `blocks` empty blocks in it and nothing else.
    fn blank(blocks: usize) -> (Interner, Func, Vec<Block>) {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let list = (0..blocks).map(|_| func.create_block()).collect();
        (names, func, list)
    }

    /// A function whose entry branches on `__builtin_expect(x, hint)`, with `x` a parameter.
    ///
    /// `parts` is the third operand where there is one, which is what
    /// `__builtin_expect_with_probability` wrote.
    fn shaped(hint: i128, parts: Option<i128>) -> (Interner, Func, Vec<Block>) {
        let (names, mut func, at) = blank(3);
        let i64_ = Type::int(64);
        let value = func.append_param(at[0], i64_);
        let mut build = Builder::new(&mut func, at[0]);
        let hint = build.iconst(i64_, hint);
        let mut operands = vec![value, hint];
        if let Some(parts) = parts {
            let parts = build.iconst(i64_, parts);
            operands.push(parts);
        }
        let args = build.func().push_values(&operands);
        let wrapped = build.value(InstData { args, ..InstData::new(Opcode::Expect) }, i64_);
        let zero = build.iconst(i64_, 0);
        let cond = build.icmp(IntPred::Ne, wrapped, zero);
        build.br_if(cond, at[1], &[], at[2], &[]);
        for block in [at[1], at[2]] {
            let mut build = Builder::new(&mut func, block);
            let answer = build.iconst(Type::int(32), 0);
            build.ret(&[answer]);
        }
        (names, func, at)
    }

    /// What the two arms of the entry's branch say about themselves.
    fn arms(func: &Func, block: Block) -> Vec<Option<u32>> {
        let term = func.terminator(block).expect("a branch");
        func.target_list(term).iter().map(|at| func[at].hint.taken()).collect()
    }

    /// Runs the pass over a function, and says what it did.
    fn run(func: &mut Func) -> Stats {
        Expect.run(func, &mut crate::machine::fixtures::analyses(), &mut Fuel::unlimited())
    }

    #[test]
    fn a_hint_of_one_names_the_arm_taken_when_the_condition_holds() {
        let (_, mut func, at) = shaped(1, None);
        run(&mut func);
        assert_eq!(arms(&func, at[0]), [Some(9_000), Some(1_000)]);
    }

    #[test]
    fn a_hint_of_zero_names_the_other_arm() {
        let (_, mut func, at) = shaped(0, None);
        run(&mut func);
        assert_eq!(arms(&func, at[0]), [Some(1_000), Some(9_000)]);
    }

    /// The shape every program has, since `__builtin_expect(x, 0)` writes an `int` where the
    /// prototype asks for a `long` and the conversion is an instruction of its own at this point.
    #[test]
    fn a_hint_behind_the_conversion_the_prototype_asked_for_is_still_a_hint() {
        let (_, mut func, at) = blank(3);
        let i64_ = Type::int(64);
        let value = func.append_param(at[0], i64_);
        let mut build = Builder::new(&mut func, at[0]);
        let narrow = build.iconst(Type::int(32), 0);
        let hint = build.unary(Opcode::SExt, narrow, i64_);
        let args = build.func().push_values(&[value, hint]);
        let wrapped = build.value(InstData { args, ..InstData::new(Opcode::Expect) }, i64_);
        let zero = build.iconst(i64_, 0);
        let cond = build.icmp(IntPred::Ne, wrapped, zero);
        build.br_if(cond, at[1], &[], at[2], &[]);
        for block in [at[1], at[2]] {
            let mut build = Builder::new(&mut func, block);
            let answer = build.iconst(Type::int(32), 0);
            build.ret(&[answer]);
        }
        run(&mut func);
        assert_eq!(arms(&func, at[0]), [Some(1_000), Some(9_000)]);
    }

    #[test]
    fn a_probability_the_program_wrote_is_the_one_the_branch_gets() {
        let (_, mut func, at) = shaped(1, Some(7_500));
        run(&mut func);
        assert_eq!(arms(&func, at[0]), [Some(7_500), Some(2_500)]);
    }

    /// The one with three operands says how often the value is the hint, so a hint of zero and a
    /// probability of three quarters is three quarters for the arm taken when it is zero.
    #[test]
    fn a_probability_with_a_hint_of_zero_is_about_the_other_arm() {
        let (_, mut func, at) = shaped(0, Some(7_500));
        run(&mut func);
        assert_eq!(arms(&func, at[0]), [Some(2_500), Some(7_500)]);
    }

    #[test]
    fn the_instruction_goes_and_its_readers_read_what_it_was_given() {
        let (_, mut func, at) = shaped(1, None);
        run(&mut func);
        let left: Vec<Inst> =
            func.blocks().flat_map(|block| func.insts(block)).filter(is_expect(&func)).collect();
        assert!(left.is_empty(), "the wrapper is gone");
        // The comparison now reads the parameter itself, which is what the wrapper answered with.
        let term = func.terminator(at[0]).expect("a branch");
        let cond = *func[func[term].args].first().expect("a condition");
        let Def::Result { inst, .. } = func[cond].def else { panic!("a comparison") };
        let read = *func[func[inst].args].first().expect("a left hand side");
        assert!(matches!(func[read].def, Def::Param { .. }), "it reads the parameter");
    }

    /// Whether an instruction is the wrapper, as a closure so that the filter above reads.
    fn is_expect(func: &Func) -> impl Fn(&Inst) -> bool + use<'_> {
        move |&inst| func[inst].opcode == Opcode::Expect
    }

    #[test]
    fn a_function_with_no_hint_in_it_is_left_alone() {
        let (_, mut func, at) = blank(3);
        let i64_ = Type::int(64);
        let value = func.append_param(at[0], i64_);
        let mut build = Builder::new(&mut func, at[0]);
        let zero = build.iconst(i64_, 0);
        let cond = build.icmp(IntPred::Ne, value, zero);
        build.br_if(cond, at[1], &[], at[2], &[]);
        for block in [at[1], at[2]] {
            let mut build = Builder::new(&mut func, block);
            let answer = build.iconst(Type::int(32), 0);
            build.ret(&[answer]);
        }

        let stats = run(&mut func);
        assert!(!stats.changed(), "nothing to do");
        assert_eq!(arms(&func, at[0]), [None, None]);
    }

    /// A condition written the other way round is the same claim about the other arm, which is
    /// what the sense in [`through`] is for.
    #[test]
    fn a_condition_that_is_a_comparison_against_zero_the_other_way_flips_the_arms() {
        let (_, mut func, at) = blank(3);
        let i64_ = Type::int(64);
        let value = func.append_param(at[0], i64_);
        let mut build = Builder::new(&mut func, at[0]);
        let hint = build.iconst(i64_, 1);
        let args = build.func().push_values(&[value, hint]);
        let wrapped = build.value(InstData { args, ..InstData::new(Opcode::Expect) }, i64_);
        let zero = build.iconst(i64_, 0);
        let cond = build.icmp(IntPred::Eq, wrapped, zero);
        build.br_if(cond, at[1], &[], at[2], &[]);
        for block in [at[1], at[2]] {
            let mut build = Builder::new(&mut func, block);
            let answer = build.iconst(Type::int(32), 0);
            build.ret(&[answer]);
        }

        run(&mut func);
        assert_eq!(arms(&func, at[0]), [Some(1_000), Some(9_000)]);
    }

    /// A hint about a value nothing branches on is dropped, and the instruction still goes.
    #[test]
    fn a_hint_on_a_value_no_branch_reads_leaves_nothing_behind() {
        let (_, mut func, at) = blank(1);
        let i64_ = Type::int(64);
        let value = func.append_param(at[0], i64_);
        let mut build = Builder::new(&mut func, at[0]);
        let hint = build.iconst(i64_, 1);
        let args = build.func().push_values(&[value, hint]);
        let wrapped = build.value(InstData { args, ..InstData::new(Opcode::Expect) }, i64_);
        build.ret(&[wrapped]);

        run(&mut func);
        let left: Vec<Inst> =
            func.blocks().flat_map(|block| func.insts(block)).filter(is_expect(&func)).collect();
        assert!(left.is_empty(), "the wrapper is gone");
        let term = func.terminator(at[0]).expect("a return");
        let answer = *func[func[term].args].first().expect("a returned value");
        assert!(matches!(func[answer].def, Def::Param { .. }), "it returns the parameter");
    }
}
