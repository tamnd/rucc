//! Taking a bounds check out of a loop and putting one check in front of it.
//!
//! Design: `spec/safe-memory/07-check-elimination.md` section 7.4, which calls this the
//! transformation that matters most and gives the reason in one line: array loops are where the
//! checks are. `crate::discharge` takes out a check a second access to the same bytes made
//! redundant, which is the case a straight run of code has. It does nothing at all for the loop in
//! section 7.4, because there the address is different every time round and no two of the checks
//! are about the same bytes. What is the same every time round is the range they all fall in.
//!
//! ```c
//! for (i = 0; i < 16; i++) sum += a[i];
//! ```
//!
//! The check inside is about four bytes at `a + 4*i`. Over the whole loop that is sixty four bytes
//! starting at `a`, and one check of those sixty four bytes in front of the loop says everything
//! the sixteen checks inside it were going to say. So the pass puts that check in the preheader and
//! takes the one in the body out, and the loop runs with no check in it.
//!
//! # The two halves
//!
//! The same split section 7.7 asks for and `crate::discharge` is built on. What is in this file is
//! a walk: which loop is counted, where its addresses go, and how far the furthest one is from the
//! first. The condition under which the check may go is a rule in `rules/safety.rules`, a solver
//! agrees with it before this crate finishes building, and the pass asks the table rather than
//! deciding for itself.
//!
//! The rule is not the one `discharge` asks. That one is about a distance the compiler has as a
//! number. This one is about every distance from zero to the furthest at once, because the pass is
//! removing one instruction that stood for as many accesses as the loop has iterations, and the
//! step from the furthest access fitting to all of them fitting is arithmetic rather than
//! bookkeeping. It is written as `swept.i64` and it is proved for a symbolic distance.
//!
//! # What the loop has to be
//!
//! Section 7.4 says counted, and spells out what goes wrong otherwise: with an early exit, a
//! program that would have left the loop before the bad access traps instead, and document 02 calls
//! a false positive a release blocking bug. So the conditions are about the loop being one that
//! runs a known number of times and reaches the check on every one of them.
//!
//! The loop has one latch, one edge out of it, and the block that edge leaves from dominates the
//! latch. That last part is the bottom test, written as dominance rather than as identity because
//! `crate::canon` gives a loop a latch of its own and the test usually stays in the header, so the
//! block that decides and the block that goes round are two different blocks in every loop this
//! pass actually sees. What it says either way is that the only way out is one test that every
//! iteration reaches. No block in the loop ends without a successor, so an iteration that starts
//! finishes, and there is no loop inside it, which is what rules out an iteration that starts and
//! spins forever without ever reaching the test. There is no call anywhere inside, which is
//! stronger than what `discharge` asks of a call and is asked for a different reason: `discharge`
//! cares whether a call frees, and this pass cares whether it comes back, because a call that does
//! not come back leaves the loop having run fewer times than its count says and the hoisted check
//! covering bytes nothing read.
//!
//! How many times it goes round comes from `crate::scev` and has to be a number. One exit means the
//! count for that exit is the count, rather than an upper bound over several. The one assumption
//! the pass accepts is that signed overflow is undefined, and only because `-fwrapv` is implemented
//! by not setting `nsw`, so a counter that still carries the flag is one the front end already
//! promised about. A counter with no such flag comes back with `NoWrap` on it and the loop keeps
//! its check. `counted` is where that is written down and why.
//!
//! # What the check has to be
//!
//! Its capability is the `cap_of` of its own pointer, which is the shape `rucc-safety` emits and
//! the shape the removal argument needs, and it is the same condition `discharge` puts on a check
//! it removes for the same reason. Its block dominates the block the loop is left from, so every
//! iteration reaches it, including the last one, which leaves rather than going round again.
//! Its address walks the loop by a constant, forwards, from a base the loop does not change, which
//! is what makes the furthest address a number rather than a guess.
//!
//! The step being a whole number of the access's alignment is the pass's own condition and not the
//! rule's. A bounds check asks about alignment as well as about bytes, the rule is about bytes, and
//! an address a constant multiple of the alignment past an aligned one is aligned. That is a small
//! enough step to make here, and it is written down rather than left out because the rule does not
//! cover it and a reader who assumed it did would be reading the wrong file.
//!
//! # What it does not do yet
//!
//! Only a count that is a number. The extent of the hoisted check is written on the
//! instruction rather than computed, because `check_bounds` carries its size in its memory payload
//! on purpose, so a loop that runs `n` times has no extent this pass can write down. Section 7.4's
//! own example is that loop, so this is the smaller half of what it asks for, and getting the rest
//! needs either a check whose extent is an operand or a commitment that a capability is one
//! interval, neither of which is a thing to decide inside a pass.
//!
//! Only forwards. A walk that counts down has its furthest address before its first rather than
//! after, so the hoisted check starts somewhere the pass would have to compute, and the rule is
//! written about a distance that is not negative. Both are fixable and neither is free.
//!
//! Loop splitting, which section 7.4 calls the general form, is not here either. It is what gets
//! the loops this pass refuses, and it is a different transformation: this one moves a check and
//! that one makes two loops.

use rucc_ir::{Block, Builder, Def, Extra, Func, Inst, InstData, MemInfo, Opcode, Type, Value};

use crate::cfg::Cfg;
use crate::discharge::{Question, operand_of, yes};
use crate::dom::Dominators;
use crate::loops::{LoopId, Loops};
use crate::rules::safety;
use crate::scev::{Assumption, Count, Scev};
use crate::{Analyses, Analysis, Fuel, Pass, Preserved, Stats};

/// What is reported when a check comes out of a loop.
const HOISTED: &str = "bounds check taken out of a loop, one check in front of it covers every \
                       iteration";

/// What is reported when the pass ran out of fuel with a check it was about to take out.
const NO_FUEL: &str = "bounds check kept, the pass ran out of fuel";

/// What is reported for a loop with nowhere to put the check.
const NO_PREHEADER: &str = "loop left alone, it has no block in front of it to put a check in";

/// What is reported for a loop that can be left before its bottom test.
const ANOTHER_WAY_OUT: &str =
    "loop left alone, it can be left somewhere other than its bottom test";

/// What is reported for a loop with a loop inside it.
const A_LOOP_INSIDE: &str = "loop left alone, it has another loop inside it";

/// What is reported for a loop with a call in it.
const A_CALL_INSIDE: &str = "loop left alone, a call in it might not come back";

/// What is reported for a loop whose count is not settled.
const NOT_COUNTED: &str = "loop left alone, how many times it runs is not a number known before it";

/// What is reported for a check whose address does not walk the loop.
const NOT_A_SWEEP: &str = "bounds check kept, its address does not walk the loop by a constant";

/// What is reported for a check whose address walks backwards.
const BACKWARDS: &str = "bounds check kept, its address walks the loop from high to low";

/// What is reported for a check an iteration can finish without reaching.
const NOT_EVERY_TIME: &str = "bounds check kept, an iteration can finish without reaching it";

/// What is reported for a check whose step does not keep its alignment.
const MISALIGNED: &str = "bounds check kept, its step is not a whole number of its alignment";

/// What is reported when the rule declines the range the loop sweeps.
const TOO_WIDE: &str = "bounds check kept, the range the loop sweeps is too wide for the rule";

/// The pass.
#[derive(Debug)]
pub struct Hoist;

impl Pass for Hoist {
    fn name(&self) -> &'static str {
        "hoist"
    }

    fn describe(&self) -> &'static str {
        "a bounds check in a counted loop becomes one check in front of the loop"
    }

    fn preserves(&self) -> Preserved {
        // No edge moves and no block appears, so the graph and everything built on it stand. What
        // does not is liveness, in both directions at once: the preheader reads a value it did not
        // read before and the body stops reading one it did.
        Preserved::ALL.without(Analysis::Liveness).without(Analysis::Pressure)
    }

    fn run(&self, func: &mut Func, an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        if func.entry().is_none() {
            return stats;
        }
        let cfg = an.cfg(func).clone();
        let doms = an.dominators(func).clone();
        let loops = an.loops(func).clone();
        if loops.count() == 0 {
            return stats;
        }

        // Worked out first and applied afterwards, because scalar evolution reads the function and
        // the transformation writes it. Nothing in a plan can be invalidated by another plan being
        // applied: each one adds instructions to a preheader and removes one from a body, and no
        // plan mentions an instruction another plan removes.
        let mut plans = Vec::new();
        {
            let mut scev = Scev::new(func, &cfg, &loops);
            for id in loops.all() {
                sweep(func, &cfg, &doms, &loops, &mut scev, id, &mut plans, &mut stats);
            }
        }

        for plan in plans {
            if !fuel.take() {
                stats.missed(NO_FUEL);
                continue;
            }
            apply(func, &plan);
            stats.optimized(HOISTED);
        }
        stats
    }
}

/// One check to take out of one loop, and the check to put in front of it.
#[derive(Debug)]
struct Plan {
    /// The block the new check goes in.
    preheader: Block,
    /// The value the first iteration's address is computed from.
    base: Value,
    /// How far past that value the first iteration reads.
    offset: i128,
    /// How many bytes from there the whole loop covers.
    span: u64,
    /// The payload of the check being removed, which the new one keeps everything of but the size.
    info: MemInfo,
    /// The check being removed.
    check: Inst,
}

/// Plans what can come out of one loop, and counts what cannot and why.
///
/// Nothing is reported for a loop with no check in it that this pass could ever move, because a
/// loop that does no memory access is not a missed opportunity and a report for every one of them
/// would bury the loops that are.
#[expect(clippy::too_many_arguments, reason = "three analyses, a plan list and a report to fill")]
fn sweep(
    func: &Func,
    cfg: &Cfg,
    doms: &Dominators,
    loops: &Loops,
    scev: &mut Scev<'_>,
    id: LoopId,
    plans: &mut Vec<Plan>,
    stats: &mut Stats,
) {
    let checks: Vec<Inst> = loops
        .blocks(id)
        .iter()
        .filter(|&&block| loops.innermost(block) == Some(id))
        .flat_map(|&block| func.insts(block).collect::<Vec<Inst>>())
        .filter(|&inst| func[inst].opcode == Opcode::CheckBounds)
        .collect();
    if checks.is_empty() {
        return;
    }

    let (preheader, guard) = match shaped(func, cfg, doms, loops, id) {
        Ok(shape) => shape,
        Err(why) => {
            stats.missed(why);
            return;
        }
    };
    let Some(around) = counted(scev, id).and_then(|around| i128::try_from(around).ok()) else {
        stats.missed(NOT_COUNTED);
        return;
    };

    for check in checks {
        match planned(func, doms, scev, id, preheader, guard, around, check) {
            Ok(plan) => plans.push(plan),
            Err(why) => stats.missed(why),
        }
    }
}

/// How many times the loop goes round, when that is a number and the pass may believe it.
///
/// Goes round, and not runs, and the difference is the whole of an off by one. What the analysis
/// answers is the iteration at which the exit test first fails, which is how many times the back
/// edge is taken. A block that runs before that test runs one more time than that, because it ran
/// on the way to the test that ended the loop as well as on the way to all the ones that did not.
/// Every check this pass takes out is in such a block, which is what `planned` reads this number
/// with.
///
/// Not [`crate::scev::Bound::proven`], and the difference is one assumption. Every loop whose test
/// is signed comes back with [`Assumption::StrictOverflow`] on it, so `proven` answers nothing for
/// any `for (int i = 0; i < n; i++)` in any C program, and a pass built on it would be a pass that
/// never fires. What that assumption says is that the count rests on signed overflow being
/// undefined, and `-fwrapv` is implemented in `rucc-lower` by not setting `nsw` rather than by a
/// flag anything down here reads. So an increment that still carries `nsw` under `-fwrapv` does not
/// exist, and a bound with `StrictOverflow` and nothing else on it is a bound whose counter the
/// front end promised does not wrap. That promise is exactly what the assumption wanted.
///
/// [`Assumption::NoWrap`] is the case where there is no such promise, and it is refused. So is
/// [`Assumption::Approaching`], though only in passing, because it never appears on a count that is
/// a number.
fn counted(scev: &mut Scev<'_>, id: LoopId) -> Option<u128> {
    let bound = scev.bound(id)?;
    let (count, assumptions) = bound.parts();
    if !assumptions.iter().all(|rests_on| matches!(rests_on, Assumption::StrictOverflow)) {
        return None;
    }
    match count {
        Count::Exact(exact) => Some(exact),
        Count::Symbolic(_) => None,
    }
}

/// The preheader of a loop this pass can move a check out of, and the block it is left from.
///
/// The conditions are the module comment's, and they are here together because they are one claim:
/// the only way out of this loop is one test every iteration reaches. That is what makes an
/// iteration that starts an iteration that finishes, which is what turns how many times the loop
/// goes round into how many times the check runs, and it is also what makes the preheader a place a
/// check may go, since a loop that always runs once is a loop whose first access always happens.
fn shaped(
    func: &Func,
    cfg: &Cfg,
    doms: &Dominators,
    loops: &Loops,
    id: LoopId,
) -> Result<(Block, Block), &'static str> {
    let Some(preheader) = loops.preheader(cfg, id) else {
        return Err(NO_PREHEADER);
    };
    let [latch] = loops.latches(id) else {
        return Err(ANOTHER_WAY_OUT);
    };
    let [exit] = loops.exits(id) else {
        return Err(ANOTHER_WAY_OUT);
    };
    // The test the loop is left from has to be one the going round also passes, or there is a way
    // to reach the bottom without having decided anything, and then the count of how many times the
    // bottom is reached is not the count of how many times the test said carry on.
    if !doms.dominates(exit.from, *latch) {
        return Err(ANOTHER_WAY_OUT);
    }
    for &block in loops.blocks(id) {
        // A loop inside this one is an iteration that can start and never reach the test, which the
        // successor walk below does not catch because every block in it has a successor.
        if loops.innermost(block) != Some(id) {
            return Err(A_LOOP_INSIDE);
        }
        // A block with no successor at all is a `ret` or an `unreachable`, and control that
        // arrives there never comes back round. The loop forest does not call that an exit edge,
        // because it is not an edge, so it has to be looked for here.
        if cfg.successors(block).is_empty() {
            return Err(ANOTHER_WAY_OUT);
        }
        for inst in func.insts(block) {
            if matches!(
                func[inst].opcode,
                Opcode::Call
                    | Opcode::CallIndirect
                    | Opcode::TailCall
                    | Opcode::InlineAsm
                    | Opcode::MetaEnd
                    | Opcode::MetaTransfer
            ) {
                return Err(A_CALL_INSIDE);
            }
        }
    }
    Ok((preheader, exit.from))
}

/// The plan for one check, or why there is not one.
#[expect(clippy::too_many_arguments, reason = "each one is a separate thing the answer rests on")]
fn planned(
    func: &Func,
    doms: &Dominators,
    scev: &mut Scev<'_>,
    id: LoopId,
    preheader: Block,
    guard: Block,
    around: i128,
    check: Inst,
) -> Result<Plan, &'static str> {
    let block = func.block_of(check).ok_or(NOT_EVERY_TIME)?;
    if !doms.dominates(block, guard) {
        return Err(NOT_EVERY_TIME);
    }
    let args = &func[func[check].args];
    let (Some(&capability), Some(&pointer)) = (args.first(), args.get(1)) else {
        return Err(NOT_A_SWEEP);
    };
    if operand_of(func, capability, Opcode::CapOf, 0) != Some(pointer) {
        return Err(NOT_A_SWEEP);
    }
    let Extra::Mem(held) = func[check].extra else { return Err(NOT_A_SWEEP) };
    let info = func[held];

    let Some(chrec) = scev.evolution(id, pointer).chrec() else {
        return Err(NOT_A_SWEEP);
    };
    let Some(step) = chrec.step.as_number() else {
        return Err(NOT_A_SWEEP);
    };
    if step <= 0 {
        return Err(BACKWARDS);
    }
    // Scale one because the base is an address. Anything else is a multiple of a pointer, which is
    // not a thing the loop computed, so it is a shape this reads rather than a case to handle.
    let (Some(base), 1) = (chrec.base.value, chrec.base.scale) else {
        return Err(NOT_A_SWEEP);
    };
    let offset = chrec.base.offset;
    if step % i128::from(info.align) != 0 {
        return Err(MISALIGNED);
    }

    let reach = i128::from(info.size);
    // The check runs once before the loop goes round for the first time and once more each time it
    // does, so the furthest address it sees is the one it is at after the last of those, which is
    // `around` steps along rather than one fewer. That is `counted`'s doc comment cashed out.
    let far = around.checked_mul(step).ok_or(TOO_WIDE)?;
    let span = far.checked_add(reach).ok_or(TOO_WIDE)?;
    if !swept(span, far, reach) {
        return Err(TOO_WIDE);
    }
    let span = u64::try_from(span).map_err(|_| TOO_WIDE)?;
    Ok(Plan { preheader, base, offset, span, info, check })
}

/// Whether one check over `span` bytes answers every access the loop makes.
///
/// This function decides nothing. It builds the term the rule file is written about out of what the
/// walk above worked out and asks the table, which is section 7.7's split: the walk established
/// that every address the loop touches is between zero and `far` past the first, and whether that
/// is enough is somebody's proof rather than this file's opinion.
///
/// The distance is opaque on purpose. It is not a number the pass has, it is whichever iteration
/// the reader cares about, and asking about it as a value is how one question comes to be about all
/// of them.
fn swept(span: i128, far: i128, reach: i128) -> bool {
    let mut question = Question::default();
    let at = question.opaque();
    let at = question.app("value.i64", &[at]);
    let span = question.number(span);
    let span = question.app("iconst.i64", &[span]);
    let far = question.number(far);
    let far = question.app("iconst.i64", &[far]);
    let reach = question.number(reach);
    let reach = question.app("iconst.i64", &[reach]);
    let delta = question.opaque();
    let delta = question.app("value.i64", &[delta]);
    let term = question.app("swept.i64", &[at, span, far, reach, delta]);
    match safety::TABLE.find(&question, term) {
        Some(found) => yes(&safety::TABLE, found.rule),
        None => false,
    }
}

/// Puts the one check in front of the loop and takes the one inside it out.
fn apply(func: &mut Func, plan: &Plan) {
    let term = func.terminator(plan.preheader).expect("a preheader ends in a jump to the header");

    // A builder appends to the end of a block, which in a block that already has its terminator is
    // after it. So everything is built first and then moved in front of the terminator in the order
    // it was built, which is one pass over a list of at most four rather than a rearrangement.
    let mut made = Vec::new();
    let mut build = Builder::new(func, plan.preheader);
    let first = if plan.offset == 0 {
        plan.base
    } else {
        let by = build.iconst(Type::int(64), plan.offset);
        made.push(by);
        let args = build.func().push_values(&[plan.base, by]);
        let sum = build.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR);
        made.push(sum);
        sum
    };
    let args = build.func().push_values(&[first]);
    let capability = build.value(InstData { args, ..InstData::new(Opcode::CapOf) }, Type::CAP);
    made.push(capability);

    let info = MemInfo { size: plan.span, ..plan.info };
    let extra = Extra::Mem(build.func().add_mem(info));
    let args = build.func().push_values(&[capability, first]);
    let check = build.inst(InstData { args, extra, ..InstData::new(Opcode::CheckBounds) }, &[]);

    for value in made {
        let inst = inst_of(func, value);
        func.remove_inst(inst);
        func.insert_before(inst, term);
    }
    func.remove_inst(check);
    func.insert_before(check, term);

    // The `cap_of` the removed check was reading is left where it is. Nothing reads it now, and
    // `dce` after this pass is what makes that a smaller function rather than a dangling
    // instruction, which is the same arrangement `crate::discharge` is in.
    func.remove_inst(plan.check);
}

/// The instruction that produced a value the builder just made.
fn inst_of(func: &Func, value: Value) -> Inst {
    let Def::Result { inst, .. } = func[value].def else {
        unreachable!("the builder was just asked for an instruction that produces this")
    };
    inst
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{Flags, IntPred, MemInfo, MemOrder, Module, Restrict, Signature, verify_func};
    use rucc_target::{TargetInfo, Triple};

    use super::{HOISTED, Hoist};
    use crate::canon::Canon;
    use crate::stats::Kind;
    use crate::{Analyses, Fuel, Pass, Stats};
    use rucc_ir::{Block, Builder, Extra, Func, Inst, InstData, Opcode, Type, Value};

    /// How wide each element of the walk is, and how wide each access is.
    ///
    /// The same number, because that is the loop section 7.4 is written about: a walk over an array
    /// reading one element at a time. Cases where they differ get their own tests.
    const WIDTH: i128 = 4;

    /// A counted loop that tests at the bottom and reads one element each time round.
    ///
    /// ```text
    /// entry(a): jump head(0)
    /// head(i):  p = a + i*step; check_bounds cap_of(p), p; next = i + 1
    ///           br next < trips -> head(next), done
    /// done:     ret
    /// ```
    ///
    /// The header is the latch and the only edge out leaves from it, which is the shape the pass
    /// asks for and the shape `header-copy` leaves a `for` loop in.
    fn walking(trips: i128, step: i128, size: u64, align: u32) -> (Interner, Func, Vec<Block>) {
        promising(trips, step, size, align, Flags::NSW)
    }

    /// The same loop, with whatever the counter's increment is willing to promise.
    ///
    /// Separate because the promise is the one thing here the pass reads through `crate::scev`
    /// rather than off the instruction, so a test that takes it away is testing something else.
    fn promising(
        trips: i128,
        step: i128,
        size: u64,
        align: u32,
        flags: Flags,
    ) -> (Interner, Func, Vec<Block>) {
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::PTR]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let head = func.create_block();
        let done = func.create_block();
        let array = func.append_param(entry, Type::PTR);
        let counter = func.append_param(head, Type::int(64));

        let zero = Builder::new(&mut func, entry).iconst(Type::int(64), 0);
        Builder::new(&mut func, entry).jump(head, &[zero]);

        let mut build = Builder::new(&mut func, head);
        let by = build.iconst(Type::int(64), step);
        let scaled = build.binary(Opcode::Mul, counter, by, Flags::NSW);
        let args = build.func().push_values(&[array, scaled]);
        let pointer = build.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR);
        check(&mut build, pointer, size, align);
        let one = build.iconst(Type::int(64), 1);
        let next = build.binary(Opcode::Add, counter, one, flags);
        let limit = build.iconst(Type::int(64), trips);
        let again = build.icmp(IntPred::Slt, next, limit);
        build.br_if(again, head, &[next], done, &[]);
        Builder::new(&mut func, done).ret(&[]);
        (names, func, vec![entry, head, done])
    }

    /// Puts `cap_of` and a `check_bounds` over `size` bytes at `pointer` into a block.
    ///
    /// The shape `rucc-safety` emits, written out here rather than reached for, because `rucc-opt`
    /// is rank 9 alongside `rucc-safety` and cannot depend on it.
    fn check(build: &mut Builder<'_>, pointer: Value, size: u64, align: u32) {
        let args = build.func().push_values(&[pointer]);
        let capability = build.value(InstData { args, ..InstData::new(Opcode::CapOf) }, Type::CAP);
        let info = MemInfo {
            size,
            align,
            order: MemOrder::NotAtomic,
            tbaa: None,
            restrict: Restrict::NONE,
        };
        let args = build.func().push_values(&[capability, pointer]);
        let extra = Extra::Mem(build.func().add_mem(info));
        build.inst(InstData { args, extra, ..InstData::new(Opcode::CheckBounds) }, &[]);
    }

    /// Canonicalizes and then hoists, with as much fuel as both want.
    ///
    /// Both, because the pass is written against the shape [`Canon`] leaves. Canonicalization is
    /// what gives the loop its preheader, and a test that skipped it would be a test of a function
    /// the pipeline does not produce.
    fn hoisted(func: &mut Func) -> Stats {
        let mut an = Analyses::new();
        Canon.run(func, &mut an, &mut Fuel::unlimited());
        Hoist.run(func, &mut an, &mut Fuel::unlimited())
    }

    /// Every bounds check left in a function, with the block it is in.
    fn checks(func: &Func) -> Vec<(Block, Inst)> {
        func.blocks()
            .flat_map(|block| func.insts(block).map(move |inst| (block, inst)).collect::<Vec<_>>())
            .filter(|&(_, inst)| func[inst].opcode == Opcode::CheckBounds)
            .collect()
    }

    /// How many bytes a check covers.
    fn extent(func: &Func, check: Inst) -> u64 {
        let Extra::Mem(info) = func[check].extra else { panic!("a check carries a payload") };
        func[info].size
    }

    /// Insists the function is one the rest of the compiler may believe.
    ///
    /// The pass adds instructions to a block that already had a terminator and reads a value
    /// defined outside the loop from in front of it, so this is what says the new check is where it
    /// claims to be and that everything it names is in scope there.
    fn sound(func: &Func, names: &mut Interner) {
        let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().unwrap());
        let module = Module::new(names.intern("t.c"), &target);
        if let Err(errors) = verify_func(&module, func, names) {
            panic!("{errors:#?}");
        }
    }

    #[test]
    fn a_check_that_walks_a_counted_loop_becomes_one_check_in_front_of_it() {
        // Section 7.4's own example, at sixteen iterations of four bytes each. What comes out is
        // one check of the sixty four bytes the loop reads, and a body with no check in it.
        let (mut names, mut func, _) = walking(16, WIDTH, 4, 4);
        let stats = hoisted(&mut func);
        assert_eq!(stats.count(Kind::Optimized, HOISTED), 1);

        let left = checks(&func);
        assert_eq!(left.len(), 1, "one check, and it is the one that was put in front");
        assert_eq!(extent(&func, left[0].1), 64, "fifteen steps of four, plus the last read");
        sound(&func, &mut names);
    }

    #[test]
    fn a_loop_whose_counter_promises_nothing_keeps_its_check() {
        // The same loop with the `nsw` taken off the increment, which is what `-fwrapv` produces.
        // Then the counter can wrap, the count rests on it not wrapping, and nothing in the IR says
        // it will not, so the count is refused rather than assumed. This is the difference between
        // the one assumption `counted` accepts and the one it does not.
        let (_, mut func, _) = promising(16, WIDTH, 4, 4, Flags::NONE);
        let stats = hoisted(&mut func);
        assert!(!stats.changed());
        assert_eq!(stats.count(Kind::Missed, super::NOT_COUNTED), 1);
        assert_eq!(checks(&func).len(), 1, "and it is still in the body");
    }

    #[test]
    fn the_check_that_is_left_is_outside_the_loop() {
        // The number above says one check. This says it is in the block in front of the loop and
        // not in the body, which is the whole of what the transformation is.
        let (_, mut func, _) = walking(16, WIDTH, 4, 4);
        hoisted(&mut func);
        let (block, _) = checks(&func)[0];
        let (cfg, doms, loops) = forest(&func);
        let _ = doms;
        let id = loops.all().next().expect("there is a loop");
        assert!(!loops.contains(id, block), "the check is not in the loop any more");
        assert_eq!(loops.preheader(&cfg, id), Some(block), "it is in the preheader");
    }

    /// The forest of the function as it is now.
    fn forest(func: &Func) -> (crate::Cfg, crate::Dominators, crate::Loops) {
        let cfg = crate::Cfg::new(func);
        let doms = crate::Dominators::new(&cfg);
        let loops = crate::Loops::new(&cfg, &doms);
        (cfg, doms, loops)
    }

    #[test]
    fn a_walk_whose_step_is_wider_than_its_access_covers_the_gaps_too() {
        // Reading four bytes out of every sixteen. The hoisted check covers the whole stride, which
        // is more bytes than the loop reads, and that is allowed only because the check that passed
        // put all of them inside one storage instance. Section 7.3's argument is about a range and
        // not about which bytes in it anybody touched.
        let (mut names, mut func, _) = walking(8, 16, 4, 4);
        assert_eq!(hoisted(&mut func).count(Kind::Optimized, HOISTED), 1);
        assert_eq!(extent(&func, checks(&func)[0].1), 116, "seven steps of sixteen, plus four");
        sound(&func, &mut names);
    }

    #[test]
    fn a_loop_that_runs_a_number_of_times_nobody_knows_keeps_its_check() {
        // The limit is a parameter, so the trip count is an expression and the extent of a check
        // has to be a number. This is the case section 7.4 is actually written about and the one
        // this pass does not have yet.
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::PTR, Type::int(64)]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let head = func.create_block();
        let done = func.create_block();
        let array = func.append_param(entry, Type::PTR);
        let limit = func.append_param(entry, Type::int(64));
        let counter = func.append_param(head, Type::int(64));
        let zero = Builder::new(&mut func, entry).iconst(Type::int(64), 0);
        Builder::new(&mut func, entry).jump(head, &[zero]);
        let mut build = Builder::new(&mut func, head);
        let by = build.iconst(Type::int(64), 4);
        let scaled = build.binary(Opcode::Mul, counter, by, Flags::NSW);
        let args = build.func().push_values(&[array, scaled]);
        let pointer = build.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR);
        check(&mut build, pointer, 4, 4);
        let one = build.iconst(Type::int(64), 1);
        let next = build.binary(Opcode::Add, counter, one, Flags::NSW);
        let again = build.icmp(IntPred::Slt, next, limit);
        build.br_if(again, head, &[next], done, &[]);
        Builder::new(&mut func, done).ret(&[]);

        let stats = hoisted(&mut func);
        assert!(!stats.changed());
        assert_eq!(stats.count(Kind::Missed, super::NOT_COUNTED), 1);
        assert_eq!(checks(&func).len(), 1, "and it is still in the body");
    }

    #[test]
    fn a_loop_with_a_call_in_it_keeps_its_check() {
        // Not because the call might free, which is what `discharge` worries about, but because it
        // might not come back. A loop that stops in the middle read fewer bytes than its trip count
        // says, and a check in front of it for all of them would refuse a program that was right.
        let (mut names, mut func, blocks) = walking(16, WIDTH, 4, 4);
        let head = blocks[1];
        let term = func.terminator(head).expect("the header branches");
        let callee = names.intern("might_not_return");
        let signature = func.add_signature(Signature::new());
        let call = Builder::new(&mut func, head).call(callee, signature, &[]);
        func.remove_inst(call);
        func.insert_before(call, term);

        let stats = hoisted(&mut func);
        assert!(!stats.changed());
        assert_eq!(stats.count(Kind::Missed, super::A_CALL_INSIDE), 1);
    }

    #[test]
    fn a_loop_that_can_be_left_early_keeps_its_check() {
        // The false positive section 7.4 names. The loop leaves in the middle on some input, so the
        // last few elements are never read, and a check in front for all of them would trap on a
        // program that never touched them.
        let (mut names, mut func, blocks) = walking(16, WIDTH, 4, 4);
        let (head, done) = (blocks[1], blocks[2]);
        let split = func.create_block();
        let term = func.terminator(head).expect("the header branches");
        let mut build = Builder::new(&mut func, head);
        let counter = build.func()[head].params[0];
        let seven = build.iconst(Type::int(64), 7);
        let bail = build.icmp(IntPred::Eq, counter, seven);
        let leave = build.br_if(bail, done, &[], split, &[]);
        for inst in [inst_of(&func, seven), inst_of(&func, bail), leave] {
            func.remove_inst(inst);
            func.insert_before(inst, term);
        }
        // Everything the header did after the new branch now belongs to the block it falls into.
        let rest: Vec<Inst> = func.insts(head).skip_while(|&inst| inst != term).collect();
        for inst in rest {
            func.remove_inst(inst);
            Builder::new(&mut func, split).func();
            func.append_inst(split, inst);
        }

        let stats = hoisted(&mut func);
        assert!(!stats.changed());
        assert_eq!(stats.count(Kind::Missed, super::ANOTHER_WAY_OUT), 1);
        sound(&func, &mut names);
    }

    #[test]
    fn a_check_an_iteration_can_finish_without_reaching_stays() {
        // The check is under an `if` inside the body, so the loop runs sixteen times and the check
        // runs fewer. What the loop reads is a subset of what the hoisted check would cover, which
        // is the same false positive one exit further in.
        let (mut names, mut func, _) = guarded();
        let stats = hoisted(&mut func);
        assert!(!stats.changed());
        assert_eq!(stats.count(Kind::Missed, super::NOT_EVERY_TIME), 1);
        sound(&func, &mut names);
    }

    /// A counted loop whose access is under a test, so an iteration can finish without it.
    fn guarded() -> (Interner, Func, Vec<Block>) {
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::PTR, Type::int(64)]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let head = func.create_block();
        let read = func.create_block();
        let tail = func.create_block();
        let done = func.create_block();
        let array = func.append_param(entry, Type::PTR);
        let choice = func.append_param(entry, Type::int(64));
        let counter = func.append_param(head, Type::int(64));
        let zero = Builder::new(&mut func, entry).iconst(Type::int(64), 0);
        Builder::new(&mut func, entry).jump(head, &[zero]);

        let mut build = Builder::new(&mut func, head);
        let take = build.icmp(IntPred::Ne, choice, zero);
        build.br_if(take, read, &[], tail, &[]);

        let mut build = Builder::new(&mut func, read);
        let by = build.iconst(Type::int(64), 4);
        let scaled = build.binary(Opcode::Mul, counter, by, Flags::NSW);
        let args = build.func().push_values(&[array, scaled]);
        let pointer = build.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR);
        check(&mut build, pointer, 4, 4);
        build.jump(tail, &[]);

        let mut build = Builder::new(&mut func, tail);
        let one = build.iconst(Type::int(64), 1);
        let next = build.binary(Opcode::Add, counter, one, Flags::NSW);
        let limit = build.iconst(Type::int(64), 16);
        let again = build.icmp(IntPred::Slt, next, limit);
        build.br_if(again, head, &[next], done, &[]);
        Builder::new(&mut func, done).ret(&[]);
        (names, func, vec![entry, head, read, tail, done])
    }

    #[test]
    fn a_check_whose_step_does_not_keep_its_alignment_stays() {
        // Three bytes at a time through an access that wants four byte alignment. The first address
        // is aligned and the second is not, so one check in front would say nothing about the
        // alignment the checks inside were asking about.
        let (_, mut func, _) = walking(8, 3, 4, 4);
        let stats = hoisted(&mut func);
        assert!(!stats.changed());
        assert_eq!(stats.count(Kind::Missed, super::MISALIGNED), 1);
    }

    #[test]
    fn a_check_whose_address_walks_backwards_stays() {
        let (_, mut func, _) = walking(8, -4, 4, 4);
        let stats = hoisted(&mut func);
        assert!(!stats.changed());
        assert_eq!(stats.count(Kind::Missed, super::BACKWARDS), 1);
    }

    #[test]
    fn a_check_through_a_pointer_the_loop_does_not_move_is_not_this_pass_to_take_out() {
        // The address is the same every iteration, which makes it `discharge`'s to answer and not
        // this one's. Reported rather than ignored so that the two passes' numbers add up.
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::PTR]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let head = func.create_block();
        let done = func.create_block();
        let array = func.append_param(entry, Type::PTR);
        let counter = func.append_param(head, Type::int(64));
        let zero = Builder::new(&mut func, entry).iconst(Type::int(64), 0);
        Builder::new(&mut func, entry).jump(head, &[zero]);
        let mut build = Builder::new(&mut func, head);
        check(&mut build, array, 4, 4);
        let one = build.iconst(Type::int(64), 1);
        let next = build.binary(Opcode::Add, counter, one, Flags::NSW);
        let limit = build.iconst(Type::int(64), 16);
        let again = build.icmp(IntPred::Slt, next, limit);
        build.br_if(again, head, &[next], done, &[]);
        Builder::new(&mut func, done).ret(&[]);

        let stats = hoisted(&mut func);
        assert!(!stats.changed());
        assert_eq!(stats.count(Kind::Missed, super::NOT_A_SWEEP), 1);
    }

    #[test]
    fn fuel_stops_the_hoist_where_it_stands() {
        let (mut names, mut func, _) = walking(16, WIDTH, 4, 4);
        let mut an = Analyses::new();
        Canon.run(&mut func, &mut an, &mut Fuel::unlimited());
        let stats = Hoist.run(&mut func, &mut an, &mut Fuel::of(0));
        assert_eq!(stats.count(Kind::Optimized, HOISTED), 0);
        assert_eq!(stats.count(Kind::Missed, super::NO_FUEL), 1);
        assert_eq!(checks(&func).len(), 1, "and the check is where it was");
        sound(&func, &mut names);
    }

    #[test]
    fn a_loop_that_sweeps_further_than_the_rule_goes_keeps_its_check() {
        // Four gigabytes is where the rule stops, because past there the compiler's `i128` reading
        // of the guard and the solver's sixty four bit reading start to differ. No real loop is out
        // here and the point is that one would keep its check rather than be waved through.
        let (_, mut func, _) = walking(2, 1 << 33, 4, 1);
        let stats = hoisted(&mut func);
        assert!(!stats.changed());
        assert_eq!(stats.count(Kind::Missed, super::TOO_WIDE), 1);
    }

    /// The instruction that produced a value.
    fn inst_of(func: &Func, value: Value) -> Inst {
        super::inst_of(func, value)
    }
}
