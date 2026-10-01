//! What `__builtin_constant_p` answers about a value the front end could not see to be a constant.
//!
//! Design: `spec/optimizer/20-idioms-and-libcalls.md`, and tamnd/rucc#392.
//!
//! ```c
//! int size = sizeof (int);
//! if (__builtin_constant_p (size))   /* one at -O2, zero at -O0 */
//! ```
//!
//! gcc answers the builtin once it has optimized the function, which is why the same line gives
//! zero at `-O0` and one at `-O2`: by the time the question is asked, `size` has become the four it
//! was set to. The front end answers what it can see, a constant as written is one and an argument
//! with an effect is zero, and leaves the rest as an `is_constant` instruction for this pass. It
//! answers one where the operand has become a constant and zero everywhere else, and it runs late
//! enough in each level that the folding in front of it has had its chance, and early enough that
//! the folding and the control flow pass behind it take out the arm the answer did not choose.
//!
//! # The arm that was not chosen
//!
//! Leaving that last part to the passes behind this one was not enough, and the kernel is where it
//! shows. `BUILD_BUG_ON(!__builtin_constant_p(x))` in an `__always_inline` helper, the size switch
//! in `percpu.h` and the FORTIFY checks all call a function that does not exist, or one marked
//! `error`, on the arm the answer rules out. gcc takes that arm out every time, so the kernel never
//! defines the function, and a call that survives is a link error or a compile error where gcc
//! gave neither. What an answer decides can be a few steps away from it: a `select` that `phiopt`
//! made of the branch before the question was answered, then the `switch` that reads the select,
//! then a block parameter that has one way in once the switch is a jump. One round of folding and
//! one of control flow simplification behind this pass reach the first step and stop.
//!
//! So in a function where it answered something the pass follows the answers itself, the same way
//! [`crate::ipcp`] folds a function it has just put a constant into. It folds, points the readers
//! of a `select` on a constant at the arm the constant picks, and runs [`SimplifyCfg`] to turn the
//! branches that are now decided into jumps and remove the blocks they no longer reach. It goes
//! round until a round changes nothing, which is the guarantee tamnd/rucc#2265 asks for: a branch a
//! `__builtin_constant_p` answer rules out is gone before code generation, and no call in it is.
//! The rounds take no fuel, for the reason the answers take none: a call left standing because
//! the fuel ran out is a diagnostic about a call the program cannot make.
//!
//! At `-O0` every question is answered zero before any other pass runs, by [`answer`], since that
//! is gcc's answer at that level and the lowering already turns some locals into constants that gcc
//! would still be reading out of memory. The same function answers whatever is left once the passes
//! have run, where the pass list did not have this one in it, because nothing below the optimizer
//! lowers the instruction.

use rucc_base::hash::Map;
use rucc_ir::{Block, Def, Extra, Func, FuncId, Imm, Inst, InstData, Module, Opcode, Value};

use crate::load::LoadForward;
use crate::prune::Prune;
use crate::range::ops::Truth;
use crate::range::query::Ranges;
use crate::simplify_cfg::SimplifyCfg;
use crate::stats::Kind;
use crate::{Analyses, Fuel, Pass, Preserved, Stats, uses};

/// What the pass calls itself, which is what `-fdump-ir=after-constant-p` spells.
pub const NAME: &str = "constant-p";

const ANSWERED: &str = "__builtin_constant_p answered";

/// Recorded for each `select` whose readers were pointed at the arm its constant condition picks.
const CHOSEN: &str = "select on a constant condition replaced by the arm it picks";

/// How many rounds of following the answers a function gets at most.
///
/// Each round that changes anything takes an instruction, an edge or a block out, or turns an
/// instruction into a constant, so the rounds end on their own. The bound is there so that a step
/// that one day undoes another cannot turn that into a compile that never finishes. A kernel
/// helper needs three or four, one for each step between the answer and the call it rules out.
const ROUNDS: usize = 64;

#[derive(Debug)]
pub struct ConstantP;

impl Pass for ConstantP {
    fn name(&self) -> &'static str {
        NAME
    }

    fn describe(&self) -> &'static str {
        "__builtin_constant_p is one where the value has become a constant and zero elsewhere"
    }

    fn preserves(&self) -> Preserved {
        // Nothing, because following the answers folds branches and takes blocks out, and the
        // graph afterwards is a different graph.
        Preserved::NONE
    }

    fn required(&self) -> bool {
        // Nothing below the optimizer lowers the instruction, so a run without this pass is a
        // compile that stops on a construct the program never wrote.
        true
    }

    fn run(&self, func: &mut Func, an: &mut Analyses, _fuel: &mut Fuel) -> Stats {
        // No fuel is asked for, because leaving a question unanswered is not a smaller rewrite but
        // an instruction the back end refuses. The same reason `expect` gives.
        let mut stats = Stats::new();
        // Only where there is a question, so a function that never asked one is left exactly as
        // the pass list before this had it.
        if !asked(func).is_empty() {
            follow(func, an, &mut stats);
        }
        stats
    }
}

/// Answers the questions and follows the answers to the branches they decide, taking out what
/// those branches no longer reach, round after round until a round changes nothing.
///
/// A yes is given as soon as the value is a constant, and a no only once a round has found nothing
/// more to fold, since a value can be a constant only after the folding in the round before it:
/// the counter of a loop `unroll` copied out is an `add` of two constants until it is folded, and
/// asking before that would answer no about a four.
///
/// A value the ranges pin to one number where the question is asked counts as a constant too, the
/// way gcc's value range propagation makes it one before gcc answers. The kernel leans on that
/// for `min` and `clamp`, whose signedness check asks `__builtin_constant_p(x >= 0)` about an
/// `int` compared with a `size_t`, and gcc accepts `min(ret, sizeof(buf))` after
/// `if (ret < 0) return ret;` only because the ranges settle the comparison. The module documentation says why the rest of
/// this is here rather than left to the passes behind.
fn follow(func: &mut Func, an: &mut Analyses, stats: &mut Stats) {
    for _ in 0..ROUNDS {
        let mut fuel = Fuel::unlimited();
        let mut round = crate::fold::fold_in(func, &mut fuel);
        round.merge(&choose(func));
        let known = known(func, an);
        round.record(Kind::Optimized, ANSWERED, count(known.len()));
        let operands: Vec<Value> =
            known.into_iter().filter_map(|inst| write(func, inst, 1)).collect();
        bury(func, operands);
        // The ranges again, for the branches on the value a yes was about. The kernel's
        // `statically_true(x)` is `__builtin_constant_p(x) && (x)`, and the second `x` is a branch
        // of its own that only the ranges settle when they are what settled the first.
        round.merge(&Prune.run(func, an, &mut fuel));
        an.clear();
        round.merge(&SimplifyCfg.run(func, an, &mut fuel));
        // The control flow pass leaves clearing the cache to the manager, and the next round is
        // before the manager gets the chance.
        an.clear();
        // A store and a load of the same local that the blocks just merged put side by side. The
        // kernel's `test_bit` asks about `*bitmap` right after `bitmap_clear`, whose other arms
        // hand the bitmap to `asm`, so it stays in memory and is only zero once those arms are
        // gone and the store reaches the load. Only once nothing else moves and only for a
        // question that reads memory, since the pass is not cheap and most questions never do.
        if !round.changed() && asked(func).into_iter().any(|inst| reads_memory(func, inst)) {
            round.merge(&LoadForward.run(func, an, &mut fuel));
            an.clear();
        }
        if !round.changed() {
            // Nothing is going to become a constant now, so what is still asked is answered no,
            // and the next round follows those answers.
            let no = settle(func, Answering::Every);
            if no == 0 {
                return;
            }
            round.record(Kind::Optimized, ANSWERED, count(no));
        }
        stats.merge(&round);
    }
    // Out of rounds, which the bound says should not happen. The questions still get their
    // answers, since nothing below this lowers one.
    stats.record(Kind::Optimized, ANSWERED, count(settle(func, Answering::Every)));
}

/// Whether the operand of a question is worked out from a load, a few steps back at most.
fn reads_memory(func: &Func, inst: Inst) -> bool {
    let mut work: Vec<(Value, usize)> =
        func[func[inst].args].iter().map(|&value| (value, 0)).collect();
    while let Some((value, depth)) = work.pop() {
        let Def::Result { inst: from, .. } = func[value].def else { continue };
        if func[from].opcode == Opcode::Load {
            return true;
        }
        if depth < 8 {
            work.extend(func[func[from].args].iter().map(|&arg| (arg, depth + 1)));
        }
    }
    false
}

/// A number of answers as the count an event is kept in.
fn count(answered: usize) -> u32 {
    u32::try_from(answered).unwrap_or(u32::MAX)
}

/// Points every reader of a `select` whose condition is a constant at the arm the condition picks,
/// and takes the select out.
///
/// [`crate::fold`] cannot do this, since it rewrites an instruction into a constant where it
/// stands and the arm picked need not be a constant. It is the step between an answer and the
/// branch it decides when `phiopt` turned `__builtin_constant_p(x) ? x : 0` into a select before
/// the question was answered.
fn choose(func: &mut Func) -> Stats {
    let mut stats = Stats::new();
    let mut forward = Map::default();
    let mut chosen = Vec::new();
    for block in func.blocks().collect::<Vec<Block>>() {
        for inst in func.insts(block).collect::<Vec<Inst>>() {
            if func[inst].opcode != Opcode::Select {
                continue;
            }
            let &[condition, then, otherwise] = &func[func[inst].args] else { continue };
            let Some((known, _)) = crate::fold::constant(func, condition) else { continue };
            let result = func[inst].results().next().expect("a select is one value");
            forward.insert(result, if known.unsigned() != 0 { then } else { otherwise });
            chosen.push(inst);
            stats.optimized(CHOSEN);
        }
    }
    if !forward.is_empty() {
        uses::substitute(func, &forward);
        // Read by nothing now, and gone here rather than left for `dce`, because the next round
        // would find it again and count it as a change.
        for inst in chosen {
            func.remove_inst(inst);
        }
    }
    stats
}

/// Answers every `is_constant` in the module and says how many there were.
///
/// `look` is false at `-O0`, where every question is answered zero.
pub fn answer(module: &mut Module, look: bool) -> usize {
    let mut answered = 0;
    for id in module.funcs().collect::<Vec<FuncId>>() {
        if !module[id].is_declaration() {
            answered +=
                settle(&mut module[id], if look { Answering::Every } else { Answering::Zero });
        }
    }
    answered
}

/// Which questions [`settle`] answers, and with what.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answering {
    /// Every question, with zero whatever the value is, which is the answer at `-O0`.
    Zero,
    /// Every question, with one where the value is a constant and zero everywhere else.
    Every,
}

/// The questions whose answer is one already, because the value is a constant or because the
/// ranges say it can only be one number where the question is asked.
///
/// A comparison is asked about through [`Ranges::compare`] rather than as a range of its own,
/// since that is the entry point that reads the branches above the question as well as the
/// relations they recorded between the two sides.
fn known(func: &Func, an: &Analyses) -> Vec<Inst> {
    let asked = asked(func);
    if asked.is_empty() {
        return asked;
    }
    let (cfg, dom) = (an.cfg(func), an.dominators(func));
    let mut ranges = Ranges::new(func, cfg, dom);
    asked
        .into_iter()
        .filter(|&inst| {
            let Some(&value) = func[func[inst].args].first() else { return false };
            if is_constant(func, value) {
                return true;
            }
            let Some(block) = func.block_of(inst) else { return false };
            if !cfg.reaches(block) || !func[value].ty.is_int() || !func[value].ty.is_scalar() {
                return false;
            }
            if let Def::Result { inst: from, .. } = func[value].def {
                let data = &func[from];
                if let (Opcode::ICmp, Extra::IntPred(pred), &[a, b]) =
                    (data.opcode, data.extra, &func[data.args])
                {
                    return ranges.compare(pred, a, b, block) != Truth::Either;
                }
            }
            ranges.at(value, block).singleton().is_some()
        })
        .collect()
}

/// Every `is_constant` in the function, in block order.
fn asked(func: &Func) -> Vec<Inst> {
    func.blocks()
        .collect::<Vec<Block>>()
        .into_iter()
        .flat_map(|block| func.insts(block))
        .filter(|&inst| func[inst].opcode == Opcode::IsConstant)
        .collect()
}

/// Answers the `is_constant` questions in the function `how` says to and says how many it
/// answered.
fn settle(func: &mut Func, how: Answering) -> usize {
    let mut answered = 0;
    let mut operands = Vec::new();
    for inst in asked(func) {
        let known = how != Answering::Zero
            && func[func[inst].args].first().is_some_and(|&value| is_constant(func, value));
        operands.extend(write(func, inst, i128::from(known)));
        answered += 1;
    }
    bury(func, operands);
    answered
}

/// Takes out what was worked out only to be asked about, now that nothing reads it.
///
/// gcc never evaluates the operand of `__builtin_constant_p`, and the front end lets one through
/// that follows a pointer for that reason, so the read is not something the program may make. The
/// kernel guards `*(const unsigned long *)addr` with a test that `addr` is not null, but nothing
/// says every caller does, and at `-O0` no pass behind this one would take the load out. So the
/// operand goes with the question, and whatever it was computed from goes too once nothing else
/// reads that. Only what has no effect or only reads memory, which is everything the front end
/// lets through.
fn bury(func: &mut Func, operands: Vec<Value>) {
    if operands.is_empty() {
        return;
    }
    let mut counts = uses::count(func);
    let mut work = operands;
    while let Some(value) = work.pop() {
        let Def::Result { inst, .. } = func[value].def else { continue };
        if func.block_of(inst).is_none()
            || func[inst].results().any(|result| counts[result.index()] != 0)
            || (func[inst].opcode.has_effects() && !crate::dce::reads_only(func, inst))
        {
            continue;
        }
        uses::operands(func, inst, |read| {
            counts[read.index()] -= 1;
            work.push(read);
        });
        func.remove_inst(inst);
    }
}

/// Whether this value is a constant, an integer or a floating point one.
fn is_constant(func: &Func, value: Value) -> bool {
    let Def::Result { inst, .. } = func[value].def else { return false };
    matches!(func[inst].opcode, Opcode::IConst | Opcode::FConst)
}

/// Puts the answer where the question was, and says what the question was about.
fn write(func: &mut Func, inst: Inst, number: i128) -> Option<Value> {
    let operand = func[func[inst].args].first().copied();
    let result = func[inst].results().next().expect("an answer is one value");
    let ty = func[result].ty;
    let span = func.span(inst);
    let imm = func.add_imm(Imm::int(number, ty.lane()));
    let data = InstData { extra: Extra::Imm(imm), ..InstData::new(Opcode::IConst) };
    let made = func.create_inst(data, &[ty], span);
    func.insert_before(made, inst);
    let value = func[made].results().next().expect("a constant is one value");
    let forward: Map<_, _> = [(result, value)].into_iter().collect();
    uses::substitute(func, &forward);
    func.remove_inst(inst);
    operand
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;

    use super::*;

    const TEXT: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "x86_64-unknown-linux-gnu"
target datalayout = "e-p:64:64-i64:64-f80:128-S128"

func @use(i32, i32, i32), linkage(external);

func @g(i32), linkage(external) {
block0(%0: i32):
    %1 = iconst.i32 4
    %2 = is_constant.i32 %1
    %3 = is_constant.i32 %0
    %4 = fconst.f64 0x3ff0000000000000
    %5 = is_constant.i32 %4
    call @use(%2, %3, %5) : (i32, i32, i32)
    return
}
"#;

    fn answered(look: bool) -> String {
        let mut names = Interner::new();
        let mut module = rucc_ir::parse(TEXT, &mut names).expect("the fixture parses");
        assert_eq!(answer(&mut module, look), 3);
        if let Err(errors) = rucc_ir::verify(&module, &names) {
            panic!("the answers left invalid IR, {errors:?}");
        }
        rucc_ir::print(&module, &names)
    }

    /// A constant is one and a parameter is zero, whether the constant is an integer or not.
    #[test]
    fn a_value_that_became_a_constant_is_one_and_anything_else_is_zero() {
        let out = answered(true);
        assert!(!out.contains("is_constant"), "{out}");
        assert_eq!(out.matches("iconst.i32 1").count(), 2, "{out}");
        assert_eq!(out.matches("iconst.i32 0").count(), 1, "{out}");
    }

    /// `-O0` answers zero to all of them, the constants included, which is what gcc answers there.
    #[test]
    fn nothing_is_a_constant_at_o0() {
        let out = answered(false);
        assert!(!out.contains("is_constant"), "{out}");
        assert!(!out.contains("iconst.i32 1"), "{out}");
        assert_eq!(out.matches("iconst.i32 0").count(), 3, "{out}");
    }
}
