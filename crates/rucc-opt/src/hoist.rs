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
//! How many times it goes round comes from `crate::scev`, and it is either a number or an
//! expression the loop does not change. One exit means the count for that exit is the count, rather
//! than an upper bound over several. The one assumption the pass accepts on a count that is a number
//! is that signed overflow is undefined, and only because `-fwrapv` is implemented by not setting
//! `nsw`, so a counter that still carries the flag is one the front end already promised about. A
//! counter with no such flag comes back with `NoWrap` on it and the loop keeps its check. `counted`
//! is where that is written down and why.
//!
//! A count that is an expression is the case section 7.4 is really about, since `for (i = 0; i < n;
//! i++)` is what array code looks like. Then the extent is not a number either, so the check is
//! written in the form that carries its extent as an operand and the preheader computes it. Two
//! things have to be settled before that is allowed. The count has to be widened the way its own
//! exit test read it, which the analysis reports and `computed` spends on a sign extension or a zero
//! extension where there is any widening left to do. And the arithmetic that turns the count into a
//! byte count has to be arithmetic that cannot wrap, which `fits` establishes by bounding the count
//! and doing the whole calculation in wider arithmetic first. The bound comes from the width of the
//! type the count is read out of, or, where that type is as wide as the arithmetic and its width
//! says nothing, from the ranges.
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
//! Or it does not walk at all. An address that is the same every time round is the degenerate case
//! of the same argument, with the furthest access being the first one, so the check in front covers
//! one access and there is no arithmetic to write. It is a case worth naming because it looks like
//! `crate::licm`'s work and is not: that pass moves something that can trap only where post
//! dominance over the loop it was handed says the instruction was going to run anyway, and what is
//! established here is stronger and already in hand. SQLite's chacha block function is 288 checks of
//! this shape in one loop, all on the same sixteen word array at fixed indices.
//!
//! The step being a whole number of the access's alignment is the pass's own condition and not the
//! rule's. A bounds check asks about alignment as well as about bytes, the rule is about bytes, and
//! an address a constant multiple of the alignment past an aligned one is aligned. That is a small
//! enough step to make here, and it is written down rather than left out because the rule does not
//! cover it and a reader who assumed it did would be reading the wrong file.
//!
//! # What it does not do yet
//!
//! Not every counter as wide as the arithmetic. `fits` bounds a narrow count from the width of the
//! type it is read out of, and for a sixty four bit count that width is the whole range and says
//! nothing, so it asks `crate::range` for a bound instead. That gets `for (size_t i = 0; i < n;
//! i++)` under a guard on `n`, which is a real thing people write and the shape most such loops
//! have. What it does not get is a count nothing anywhere bounds, a `strlen` result walked to the
//! end being the one on the SQLite amalgamation, and there the loop still keeps its check. Getting
//! that one needs a bound on what a call returned, which is a different piece of work.
//!
//! Only forwards. A walk that counts down has its furthest address before its first rather than
//! after, so the hoisted check starts somewhere the pass would have to compute, and the rule is
//! written about a distance that is not negative. Both are fixable and neither is free.
//!
//! Loop splitting, which section 7.4 calls the general form, is not here either. It is what gets
//! the loops this pass refuses, and it is a different transformation: this one moves a check and
//! that one makes two loops.

use rucc_ir::{Block, Builder, Extra, Flags, Func, Inst, InstData, MemInfo, Opcode, Type, Value};

use crate::cfg::Cfg;
use crate::discharge::{Question, operand_of, yes};
use crate::dom::Dominators;
use crate::loops::{LoopId, Loops};
use crate::range::query::Ranges;
use crate::rules::safety;
use crate::scev::{Anchor, Evolution, Invariant, Plain, Reading, Scev};
use crate::trip::{Around, counted, covered, inst_of};
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

/// What is reported for a loop that could cover more bytes than the arithmetic holds.
const COUNT_TOO_WIDE: &str =
    "bounds check kept, how many bytes the loop covers might not fit in sixty four bits";

/// What is reported for a check whose address does not walk the loop.
const NOT_A_SWEEP: &str = "bounds check kept, its address does not walk the loop by a constant";

/// What is reported for a check whose address the analysis has nothing to say about.
const NOT_FOLLOWED: &str =
    "bounds check kept, what its address does round the loop is not something the analysis follows";

/// What is reported for a check that already covers a range the program worked out.
const ALREADY_COMPUTED: &str =
    "bounds check kept, how many bytes it covers is a number only the program has";

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
        Preserved::ALL.without(Analysis::Liveness)
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
            // Beside the evolution rather than instead of it. What the counter does each time round
            // is scalar evolution's answer and how large the value it stops at can be is the
            // ranges' answer, and a counter as wide as the arithmetic needs both.
            let mut ranges = Ranges::new(func, &cfg, &doms);
            for id in loops.all() {
                sweep(
                    func,
                    &cfg,
                    &doms,
                    &loops,
                    &mut scev,
                    &mut ranges,
                    id,
                    &mut plans,
                    &mut stats,
                );
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
    /// What the first iteration's address is computed from. An address rather than a value when it
    /// is a global, since nothing outside the loop computes one of those. See [`Anchor`].
    base: Anchor,
    /// How far past that value the first iteration reads.
    offset: i128,
    /// How many bytes from there the whole loop covers.
    span: Extent,
    /// The payload of the check being removed, which the new one keeps everything of but the size.
    info: MemInfo,
    /// The check being removed.
    check: Inst,
}

/// How many bytes the loop covers, which the pass has either as a number or as a recipe.
#[derive(Clone, Copy, Debug)]
enum Extent {
    /// This many, worked out here, and written on the check as its size.
    Bytes(u64),
    /// This many, worked out in the preheader, and handed to the check as an operand.
    ///
    /// The recipe is `max(scale * value + offset, 0) * step + reach`, in sixty four bit arithmetic
    /// that [`fits`] has already established cannot wrap. The `max` is
    /// [`crate::scev::Assumption::Approaching`] discharged rather than assumed: a count that comes
    /// out negative is a loop whose test failed the first time it ran, which is a loop that went
    /// round no times and read one access, and zero is the count that says so.
    ///
    /// The reading is how `value` is widened to sixty four bits before any of that, and it is the
    /// reading the exit test the count came from took rather than anything decided here.
    Computed { count: Plain, step: i128, reach: i128, reading: Reading },
}

/// Plans what can come out of one loop, and counts what cannot and why.
///
/// Nothing is reported for a loop with no check in it that this pass could ever move, because a
/// loop that does no memory access is not a missed opportunity and a report for every one of them
/// would bury the loops that are.
#[expect(clippy::too_many_arguments, reason = "four analyses, a plan list and a report to fill")]
fn sweep(
    func: &Func,
    cfg: &Cfg,
    doms: &Dominators,
    loops: &Loops,
    scev: &mut Scev<'_>,
    ranges: &mut Ranges<'_>,
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
    let around = match counted(scev, id) {
        Ok(around) => around,
        Err(why) => {
            stats.missed(why);
            return;
        }
    };

    for check in checks {
        match planned(func, doms, scev, ranges, id, preheader, guard, around, check) {
            Ok(plan) => plans.push(plan),
            Err(why) => stats.missed(why),
        }
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
    ranges: &mut Ranges<'_>,
    id: LoopId,
    preheader: Block,
    guard: Block,
    around: Around,
    check: Inst,
) -> Result<Plan, &'static str> {
    let block = func.block_of(check).ok_or(NOT_EVERY_TIME)?;
    if !doms.dominates(block, guard) {
        return Err(NOT_EVERY_TIME);
    }
    let args = &func[func[check].args];
    // A check that already carries its own extent is one this pass put somewhere, and how many
    // bytes it covers is not a number this pass can multiply.
    if args.len() > 2 {
        return Err(ALREADY_COMPUTED);
    }
    let (Some(&capability), Some(&pointer)) = (args.first(), args.get(1)) else {
        return Err(NOT_A_SWEEP);
    };
    if operand_of(func, capability, Opcode::CapOf, 0) != Some(pointer) {
        return Err(NOT_A_SWEEP);
    }
    let Extra::Mem(held) = func[check].extra else { return Err(NOT_A_SWEEP) };
    let info = func[held];

    let reach = i128::from(info.size);
    let (base, offset, span) = match scev.evolution(id, pointer) {
        // An address that does not move at all. Every iteration checks the same bytes, so the one
        // in front covers all of them and there is no arithmetic to write: the extent is one
        // access. `crate::licm` is the pass that would otherwise own this, and it leaves a check
        // where it is, because moving something that traps in front of a loop needs the loop to run
        // and that pass answers with post dominance over the loop it was handed. Everything
        // `shaped` established is a stronger answer to the same question, and it is already here.
        //
        // The scale is one for the same reason it is below, and the alignment needs nothing said
        // about it, since the address the check in front asks about is the address the one inside
        // was asking about.
        Evolution::Invariant(at) => {
            let Some((base, offset)) = anchored(at) else {
                return Err(NOT_A_SWEEP);
            };
            if !swept(reach, 0, reach) {
                return Err(TOO_WIDE);
            }
            (base, offset, Extent::Bytes(u64::try_from(reach).map_err(|_| TOO_WIDE)?))
        }
        Evolution::Affine(chrec) => {
            let Some(step) = chrec.step.as_number() else {
                return Err(NOT_A_SWEEP);
            };
            if step <= 0 {
                return Err(BACKWARDS);
            }
            // Scale one because the base is an address. Anything else is a multiple of a pointer,
            // which is not a thing the loop computed, so it is a shape this reads rather than a
            // case to handle.
            let Some((base, offset)) = anchored(chrec.base) else {
                return Err(NOT_A_SWEEP);
            };
            if step % i128::from(info.align) != 0 {
                return Err(MISALIGNED);
            }
            // The check runs once before the loop goes round for the first time and once more each
            // time it does, so the furthest address it sees is the one it is at after the last of
            // those, which is `around` steps along rather than one fewer. That is `counted`'s doc
            // comment cashed out.
            let span = match around {
                Around::Number(around) => {
                    let far = around.checked_mul(step).ok_or(TOO_WIDE)?;
                    let span = far.checked_add(reach).ok_or(TOO_WIDE)?;
                    if !swept(span, far, reach) {
                        return Err(TOO_WIDE);
                    }
                    Extent::Bytes(u64::try_from(span).map_err(|_| TOO_WIDE)?)
                }
                Around::Computed(count, reading) => {
                    fits(func, ranges, preheader, count, step, reach, reading)?;
                    if !swept_sym(reach) {
                        return Err(TOO_WIDE);
                    }
                    Extent::Computed { count, step, reach, reading }
                }
            };
            (base, offset, span)
        }
        // Not the same as a step that is not a number, and the two used to be reported as if they
        // were. This one is an address the analysis has nothing at all to say about, which on real
        // code is usually an index that was loaded from memory, `sqlite3Toupper(z[i])` being the
        // shape: a table indexed by a byte the loop just read. There is no arithmetic to hoist
        // there and there never will be. A step that is not a number is a sweep this pass could
        // cover and does not yet.
        Evolution::Unknown => return Err(NOT_FOLLOWED),
    };
    Ok(Plan { preheader, base, offset, span, info, check })
}

/// The pointer an invariant is an address off, and how far past it, when it is one.
///
/// Scale one because the base is an address. Anything else is a multiple of a pointer, which is not
/// a thing the loop computed, so it is a shape this reads rather than a case to handle. The second
/// arm is the same shape with a global in place of the value, which is described rather than named
/// and so arrives in the other half of the invariant.
fn anchored(inv: Invariant) -> Option<(Anchor, i128)> {
    if let Some(at @ Plain { value: Some(base), scale: 1, .. }) = inv.plain() {
        return Some((Anchor::Value(base), at.offset));
    }
    match inv.on()? {
        (base, rest) if rest.value.is_none() || rest.scale == 0 => Some((base, rest.offset)),
        _ => None,
    }
}

/// Establishes that the extent arithmetic stays inside sixty four bits whatever the count turns out
/// to be, which is what the symbolic rule takes as a hypothesis rather than proves.
///
/// The count is `scale * value + offset` and the pass cannot evaluate it, but it can bound it,
/// because `value` is read out of a type of a known width. The largest a signed number of `bits`
/// bits can be in either direction is two to the `bits` less one, and the largest an unsigned one
/// can be is two to the `bits` less one of them, so the count is somewhere within `|scale|` of those
/// plus `|offset|`, and the extent is that times the step plus the reach. Working the whole of it
/// out in `i128` and refusing anything that does not land inside `i64` is what makes the additions
/// and the multiplications the preheader is about to do additions that cannot wrap.
///
/// The reading is why the two bounds are different rather than one conservative bound for both. An
/// unsigned count reaches twice as far as a signed one of the same width, and a bound that ignored
/// that would be a bound the arithmetic can leave.
///
/// A counter as wide as the arithmetic is bounded by asking the ranges instead, because the width
/// of its type is the whole of the range and says nothing. That is what a program written with
/// `size_t` indices needs, and it is a bound the ranges usually have, because the count of such a
/// loop is almost always a length something already established: a parameter with a range on it, a
/// field narrower than the type holding it, or a value the loop guard just compared.
fn fits(
    func: &Func,
    ranges: &mut Ranges<'_>,
    preheader: Block,
    count: Plain,
    step: i128,
    reach: i128,
    reading: Reading,
) -> Result<(), &'static str> {
    let value = count.value.ok_or(COUNT_TOO_WIDE)?;
    let ty = func[value].ty;
    if !ty.is_int() {
        return Err(COUNT_TOO_WIDE);
    }
    // Wider than the arithmetic itself, so there is nowhere to put it. A count in an `i128` is rare
    // enough that a truncation with a check on it would be work spent on nothing.
    if ty.bits() > 64 {
        return Err(COUNT_TOO_WIDE);
    }
    let most = if ty.bits() == 64 {
        widest(ranges, preheader, value, reading).ok_or(COUNT_TOO_WIDE)?
    } else {
        match reading {
            Reading::Signed => 1i128 << (ty.bits() - 1),
            Reading::Unsigned => (1i128 << ty.bits()) - 1,
        }
    };
    let reached = count
        .scale
        .checked_abs()
        .and_then(|scale| scale.checked_mul(most))
        .and_then(|far| far.checked_add(count.offset.checked_abs()?))
        .ok_or(COUNT_TOO_WIDE)?;
    let span =
        reached.checked_mul(step).and_then(|far| far.checked_add(reach)).ok_or(COUNT_TOO_WIDE)?;
    if span > i128::from(i64::MAX) {
        return Err(COUNT_TOO_WIDE);
    }
    Ok(())
}

/// How far from zero `value` can be, in either direction, read the way its own exit test read it.
///
/// Asked at the preheader rather than at the definition, because the preheader is where the
/// arithmetic this is bounding is about to be written and because the loop guard is behind it. A
/// loop written `for (size_t i = 0; i < n; i++)` under `if (n <= COUNT)` has a bound there and none
/// at all where `n` came from.
///
/// The reading decides which end matters. A signed count can be large in either direction and the
/// arithmetic below multiplies its magnitude, so the answer is the further of the two ends. An
/// unsigned count runs from zero upwards and the greater end is the whole of it.
fn widest(
    ranges: &mut Ranges<'_>,
    preheader: Block,
    value: Value,
    reading: Reading,
) -> Option<i128> {
    let range = ranges.at(value, preheader);
    match reading {
        Reading::Signed => {
            let (low, high) = range.signed_bounds()?;
            Some(low.checked_abs()?.max(high.checked_abs()?))
        }
        Reading::Unsigned => i128::try_from(range.unsigned_bounds()?.1).ok(),
    }
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

/// The same question for a loop whose extent the program works out.
///
/// Two more of the five are opaque, because the pass has neither the span nor the distance to the
/// furthest access as a number, and everything it knows about the pair of them is written into the
/// rule as a hypothesis instead of into a guard. [`fits`] is where the pass earns those hypotheses,
/// so what is asked here is only about the reach, which is the one number it still has.
fn swept_sym(reach: i128) -> bool {
    let mut question = Question::default();
    let at = question.opaque();
    let at = question.app("value.i64", &[at]);
    let span = question.opaque();
    let span = question.app("value.i64", &[span]);
    let far = question.opaque();
    let far = question.app("value.i64", &[far]);
    let reach = question.number(reach);
    let reach = question.app("iconst.i64", &[reach]);
    let delta = question.opaque();
    let delta = question.app("value.i64", &[delta]);
    let term = question.app("swept.sym.i64", &[at, span, far, reach, delta]);
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
    let base = match plan.base {
        Anchor::Value(value) => value,
        // Written out again rather than moved, which is one instruction and is why the address
        // could be described rather than named. See [`Anchor`] and `crate::licm`'s cost table.
        Anchor::Address(symbol) => {
            let extra = Extra::Symbol(symbol);
            let at =
                build.value(InstData { extra, ..InstData::new(Opcode::GlobalAddr) }, Type::PTR);
            made.push(at);
            at
        }
    };
    let first = if plan.offset == 0 {
        base
    } else {
        let by = build.iconst(Type::int(64), plan.offset);
        made.push(by);
        let args = build.func().push_values(&[base, by]);
        let sum = build.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR);
        made.push(sum);
        sum
    };
    let args = build.func().push_values(&[first]);
    let capability = build.value(InstData { args, ..InstData::new(Opcode::CapOf) }, Type::CAP);
    made.push(capability);

    // Two shapes of check, and which one is written is which of the two the extent came in. A
    // number goes in the payload, where the front end would have put it. An expression goes in the
    // third operand, and then the payload keeps the size of one element of the walk, which is what
    // `crates/rucc-ir/src/opcode.rs` says that field means on a check of this shape.
    let (size, extent) = match plan.span {
        Extent::Bytes(bytes) => (bytes, None),
        Extent::Computed { count, step, reach, reading } => (
            plan.info.size,
            Some(covered(&mut build, &mut made, count, step, reach, reading, Flags::NSW)),
        ),
    };
    let info = MemInfo { size, ..plan.info };
    let extra = Extra::Mem(build.func().add_mem(info));
    let operands: Vec<Value> = match extent {
        Some(bytes) => vec![capability, first, bytes],
        None => vec![capability, first],
    };
    let args = build.func().push_values(&operands);
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

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{Flags, IntPred, MemInfo, MemOrder, Module, Restrict, Signature, verify_func};
    use rucc_target::{TargetInfo, Triple};

    use super::{HOISTED, Hoist};
    use crate::canon::Canon;
    use crate::stats::Kind;
    use crate::{Fuel, Pass, Stats};
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
        let mut an = crate::machine::fixtures::analyses();
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

    /// The same loop over a file scope array, with the `global_addr` inside the loop.
    ///
    /// Which is where one sits after the optimizer has been over the function, because working the
    /// address out again costs one instruction and `crate::licm` would rather do that than hold it
    /// in a register the whole way round. So the pass has to take it from there or not at all.
    fn over_a_global(trips: i128, step: i128) -> (Interner, Func, Vec<Block>) {
        let mut names = Interner::new();
        let tab = names.intern("tab");
        let mut func = Func::new(names.intern("f"), Signature::new());
        let entry = func.create_block();
        let head = func.create_block();
        let done = func.create_block();
        let counter = func.append_param(head, Type::int(64));

        let zero = Builder::new(&mut func, entry).iconst(Type::int(64), 0);
        Builder::new(&mut func, entry).jump(head, &[zero]);

        let mut build = Builder::new(&mut func, head);
        let by = build.iconst(Type::int(64), step);
        let scaled = build.binary(Opcode::Mul, counter, by, Flags::NSW);
        let extra = Extra::Symbol(tab);
        let array = build.value(InstData { extra, ..InstData::new(Opcode::GlobalAddr) }, Type::PTR);
        let args = build.func().push_values(&[array, scaled]);
        let pointer = build.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR);
        check(&mut build, pointer, 4, 4);
        let one = build.iconst(Type::int(64), 1);
        let next = build.binary(Opcode::Add, counter, one, Flags::NSW);
        let limit = build.iconst(Type::int(64), trips);
        let again = build.icmp(IntPred::Slt, next, limit);
        build.br_if(again, head, &[next], done, &[]);
        Builder::new(&mut func, done).ret(&[]);
        (names, func, vec![entry, head, done])
    }

    #[test]
    fn a_walk_over_a_file_scope_array_is_hoisted_like_any_other() {
        // The address the analysis hands back for this one is measured from the symbol rather than
        // from a value, so the check in front of the loop has a `global_addr` of its own written
        // above it. See tamnd/rucc#810.
        let (mut names, mut func, _) = over_a_global(16, WIDTH);
        let stats = hoisted(&mut func);
        assert_eq!(stats.count(Kind::Optimized, HOISTED), 1);

        let left = checks(&func);
        assert_eq!(left.len(), 1, "one check, and it is the one that was put in front");
        assert_eq!(extent(&func, left[0].1), 64, "fifteen steps of four, plus the last read");
        let (cfg, _, loops) = forest(&func);
        let id = loops.all().next().expect("there is a loop");
        assert_eq!(loops.preheader(&cfg, id), Some(left[0].0), "it is in the preheader");
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
        assert_eq!(stats.count(Kind::Missed, crate::trip::NOT_COUNTED), 1);
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

    /// A loop whose limit is a parameter, so how many times it runs is an expression.
    ///
    /// The counter is as wide as `ty` and the walk is four bytes at a time through `widening` of
    /// it, which is what `for (T i = 0; i < n; i++) a[i]` lowers to on a sixty four bit target. A
    /// signed counter is widened by a sign extension and an unsigned one by a zero extension, which
    /// is why that is an argument rather than settled here. The predicate and the flags are
    /// arguments because the two things the pass asks of a count it cannot evaluate are about
    /// exactly those.
    fn unknown(
        ty: Type,
        pred: IntPred,
        flags: Flags,
        widening: Opcode,
    ) -> (Interner, Func, Vec<Block>) {
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::PTR, ty]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let head = func.create_block();
        let done = func.create_block();
        let array = func.append_param(entry, Type::PTR);
        let limit = func.append_param(entry, ty);
        let counter = func.append_param(head, ty);
        let zero = Builder::new(&mut func, entry).iconst(ty, 0);
        Builder::new(&mut func, entry).jump(head, &[zero]);

        let mut build = Builder::new(&mut func, head);
        let wide = if ty == Type::int(64) {
            counter
        } else {
            build.unary(widening, counter, Type::int(64))
        };
        let by = build.iconst(Type::int(64), WIDTH);
        let scaled = build.binary(Opcode::Mul, wide, by, Flags::NSW);
        let args = build.func().push_values(&[array, scaled]);
        let pointer = build.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR);
        check(&mut build, pointer, 4, 4);
        let one = build.iconst(ty, 1);
        let next = build.binary(Opcode::Add, counter, one, flags);
        let again = build.icmp(pred, next, limit);
        build.br_if(again, head, &[next], done, &[]);
        Builder::new(&mut func, done).ret(&[]);
        (names, func, vec![entry, head, done])
    }

    /// The operands of a check.
    fn operands(func: &Func, check: Inst) -> Vec<Value> {
        func[func[check].args].to_vec()
    }

    #[test]
    fn a_loop_that_runs_a_number_of_times_nobody_knows_gets_a_check_that_works_it_out() {
        // Section 7.4's real example, where the limit is a parameter. The count is an expression, so
        // the extent is one too, and the check that comes out is the form that carries how many
        // bytes it covers as an operand with the preheader computing it.
        let (mut names, mut func, _) =
            unknown(Type::int(32), IntPred::Slt, Flags::NSW, Opcode::SExt);
        let stats = hoisted(&mut func);
        assert_eq!(stats.count(Kind::Optimized, HOISTED), 1);

        let left = checks(&func);
        assert_eq!(left.len(), 1, "one check, and it is the one that was put in front");
        assert_eq!(operands(&func, left[0].1).len(), 3, "its extent is an operand");
        assert_eq!(extent(&func, left[0].1), 4, "and its payload is one element of the walk");
        sound(&func, &mut names);
    }

    #[test]
    fn the_extent_a_loop_of_unknown_length_gets_is_the_one_the_arithmetic_says() {
        // `max(n - 1, 0) * 4 + 4`, which for a limit of sixteen is the sixty four bytes the loop
        // with a constant limit gets. The clamp is what makes a limit of zero or less come out at
        // one element, which is what a bottom tested loop actually reads before it leaves.
        let (_, mut func, blocks) = unknown(Type::int(32), IntPred::Slt, Flags::NSW, Opcode::SExt);
        hoisted(&mut func);
        let (block, check) = checks(&func)[0];
        assert_ne!(block, blocks[1], "the check is out of the body");

        let bytes = operands(&func, check)[2];
        let steps: Vec<Opcode> = func
            .insts(block)
            .map(|inst| func[inst].opcode)
            .filter(|&opcode| {
                matches!(opcode, Opcode::SExt | Opcode::Add | Opcode::ICmp | Opcode::Select)
            })
            .collect();
        assert_eq!(
            steps,
            [Opcode::SExt, Opcode::Add, Opcode::ICmp, Opcode::Select, Opcode::Add],
            "sign extend, take one off, clamp at zero, and add the last read back on"
        );
        assert_eq!(func[bytes].ty, Type::int(64), "the extent is a word wide");
    }

    #[test]
    fn a_loop_counted_as_wide_as_the_arithmetic_keeps_its_check_when_nothing_bounds_the_count() {
        // A sixty four bit counter, which is `for (size_t i = 0; i < n; i++)`, with the limit
        // straight off the parameter list. The width of the type says nothing here because it is
        // the whole of the arithmetic, and there is no other fact about the parameter, so the
        // extent could be anything and the loop keeps its check.
        let (_, mut func, _) = unknown(Type::int(64), IntPred::Slt, Flags::NSW, Opcode::SExt);
        let stats = hoisted(&mut func);
        assert!(!stats.changed());
        assert_eq!(stats.count(Kind::Missed, super::COUNT_TOO_WIDE), 1);
        assert_eq!(checks(&func).len(), 1, "and it is still in the body");
    }

    /// The same loop with a sixty four bit counter and a limit the ranges can bound.
    ///
    /// `for (size_t i = 0; i < n; i++)` where `n` came from something narrower, which is what a
    /// length out of a field or an `int` parameter looks like by the time it reaches the test. The
    /// widening is the fact: nothing that went through it can be larger than the type it came from.
    fn widened() -> (Interner, Func, Vec<Block>) {
        let ty = Type::int(64);
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::PTR, Type::int(32)]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let head = func.create_block();
        let done = func.create_block();
        let array = func.append_param(entry, Type::PTR);
        let narrow = func.append_param(entry, Type::int(32));
        let counter = func.append_param(head, ty);
        let limit = Builder::new(&mut func, entry).unary(Opcode::ZExt, narrow, ty);
        let zero = Builder::new(&mut func, entry).iconst(ty, 0);
        Builder::new(&mut func, entry).jump(head, &[zero]);

        let mut build = Builder::new(&mut func, head);
        let by = build.iconst(ty, WIDTH);
        let scaled = build.binary(Opcode::Mul, counter, by, Flags::NSW);
        let args = build.func().push_values(&[array, scaled]);
        let pointer = build.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR);
        check(&mut build, pointer, 4, 4);
        let one = build.iconst(ty, 1);
        let next = build.binary(Opcode::Add, counter, one, Flags::NSW);
        let again = build.icmp(IntPred::Slt, next, limit);
        build.br_if(again, head, &[next], done, &[]);
        Builder::new(&mut func, done).ret(&[]);
        (names, func, vec![entry, head, done])
    }

    #[test]
    fn a_wide_count_the_ranges_can_bound_gets_its_check_taken_out() {
        // The counter is as wide as the arithmetic, so the width of its type bounds nothing, and
        // the limit went through a widening, so the ranges bound it anyway. Four billion elements
        // of four bytes is sixteen billion, which is inside the sixty four bit arithmetic the
        // preheader is about to do, and that is the whole of what has to hold.
        let (mut names, mut func, blocks) = widened();
        let stats = hoisted(&mut func);
        assert_eq!(stats.count(Kind::Optimized, HOISTED), 1);
        let left = checks(&func);
        assert_eq!(left.len(), 1, "one check, and it is the one that was put in front");
        assert_ne!(left[0].0, blocks[1], "the check is out of the body");
        assert_eq!(operands(&func, left[0].1).len(), 3, "its extent is an operand");
        sound(&func, &mut names);
    }

    #[test]
    fn a_loop_whose_exit_test_is_unsigned_gets_its_count_widened_the_same_way() {
        // `for (unsigned i = 0; i < n; i++) a[i]`, which is not a rare loop. The count is built out
        // of the limit operand of the test, that operand is a value of the counter's own type, and
        // a limit past the middle of a thirty two bit type is a large number to this test. Sign
        // extending it would make it negative, clamp it to zero, and leave a check over one element
        // in front of a loop reading thousands, so the extension is the zero one.
        let (mut names, mut func, blocks) =
            unknown(Type::int(32), IntPred::Ult, Flags::NSW, Opcode::SExt);
        let stats = hoisted(&mut func);
        assert_eq!(stats.count(Kind::Optimized, HOISTED), 1);

        let (block, check) = checks(&func)[0];
        assert_ne!(block, blocks[1], "the check is out of the body");
        let steps: Vec<Opcode> = func
            .insts(block)
            .map(|inst| func[inst].opcode)
            .filter(|&opcode| {
                matches!(
                    opcode,
                    Opcode::SExt | Opcode::ZExt | Opcode::Add | Opcode::ICmp | Opcode::Select
                )
            })
            .collect();
        assert_eq!(
            steps,
            [Opcode::ZExt, Opcode::Add, Opcode::ICmp, Opcode::Select, Opcode::Add],
            "zero extend, take one off, clamp at zero, and add the last read back on"
        );
        assert_eq!(func[operands(&func, check)[2]].ty, Type::int(64), "the extent is a word wide");
        sound(&func, &mut names);
    }

    #[test]
    fn an_unsigned_counter_under_an_inclusive_test_keeps_its_check() {
        // What the test being unsigned is worth, and where it runs out. An unsigned counter carries
        // no `nuw`, so what rules out its wrapping is the test, and under `<=` the counter reaches
        // the limit and is stepped once more. A limit at the top of its type makes that last step
        // the one that wraps, so the count comes back resting on the counter not wrapping and
        // nothing here can discharge that.
        let (_, mut func, _) = unknown(Type::int(32), IntPred::Ule, Flags::NSW, Opcode::SExt);
        let stats = hoisted(&mut func);
        assert!(!stats.changed());
        assert_eq!(stats.count(Kind::Missed, crate::trip::RESTS_ON_NO_WRAP), 1);
    }

    #[test]
    fn a_subscript_on_an_unsigned_counter_widens_on_the_strength_of_the_test() {
        // `for (unsigned i = 0; i < n; i++) a[i]` written the way C programmers write it, with a
        // subscript rather than a pointer walked by hand. The address is a zero extension of the
        // counter, and an unsigned counter carries no `nuw`, so widening it used to be refused and
        // every check in the loop stayed where it was. What settles it is the loop's own exit test,
        // which keeps the counter under the limit and so keeps it inside its type.
        let (mut names, mut func, blocks) =
            unknown(Type::int(32), IntPred::Ult, Flags::NONE, Opcode::ZExt);
        let stats = hoisted(&mut func);
        assert_eq!(stats.count(Kind::Optimized, HOISTED), 1);

        let left = checks(&func);
        assert_eq!(left.len(), 1, "one check, and it is the one that was put in front");
        assert_ne!(left[0].0, blocks[1], "the check is out of the body");
        sound(&func, &mut names);
    }

    #[test]
    fn a_subscript_on_a_counter_under_an_inclusive_test_keeps_its_check() {
        // The same loop with `<=`, which is where the strength of the test runs out. The counter
        // reaches the limit and is stepped once more, a limit at the top of its type makes that
        // last step the one that wraps, and a sequence that wraps is not the sequence its widening
        // describes. The count is asked for first and is refused for the same reason, so that is
        // the remark, and the address never gets looked at.
        let (_, mut func, _) = unknown(Type::int(32), IntPred::Ule, Flags::NONE, Opcode::ZExt);
        let stats = hoisted(&mut func);
        assert!(!stats.changed());
        assert_eq!(stats.count(Kind::Missed, crate::trip::RESTS_ON_NO_WRAP), 1);
        assert_eq!(checks(&func).len(), 1, "and it is still in the body");
    }

    #[test]
    fn a_loop_whose_counter_of_unknown_length_promises_nothing_keeps_its_check() {
        // The same refusal as for a count that is a number, one width down. Without the flag the
        // count comes back resting on the counter not wrapping and nothing in the IR says it does
        // not.
        //
        // Reported as that rather than as a count nobody worked out, which is the difference
        // between a loop whose trip count is an expression with a condition attached and a loop
        // whose trip count nothing anywhere has. Both used to come out under the second remark and
        // they are not the same piece of work.
        let (_, mut func, _) = unknown(Type::int(32), IntPred::Slt, Flags::NONE, Opcode::SExt);
        let stats = hoisted(&mut func);
        assert!(!stats.changed());
        assert_eq!(stats.count(Kind::Missed, crate::trip::RESTS_ON_NO_WRAP), 1);
        assert_eq!(
            stats.count(Kind::Missed, crate::trip::NOT_COUNTED),
            0,
            "and not as the other one"
        );
    }

    #[test]
    fn a_check_on_an_address_the_analysis_cannot_follow_says_so() {
        // The address is loaded out of the array each time round rather than worked out from the
        // counter, so there is no sequence to describe and nothing to hoist. On real code this is
        // a table indexed by a byte the loop just read, `sqlite3Toupper(z[i])` being the shape, and
        // it is 25 of the checks SQLite keeps.
        //
        // Reported as an address nothing can be said about rather than as an address that does not
        // walk by a constant. The second is a sweep this pass could cover and does not yet, the
        // first is not a sweep at all, and reporting them as one made the wrong one look like work
        // worth doing. See #782.
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
        let by = build.iconst(Type::int(64), WIDTH);
        let scaled = build.binary(Opcode::Mul, counter, by, Flags::NSW);
        let args = build.func().push_values(&[array, scaled]);
        let slot = build.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR);
        let info = MemInfo {
            size: 8,
            align: 8,
            order: MemOrder::NotAtomic,
            tbaa: None,
            restrict: Restrict::NONE,
        };
        let loaded = build.load(Type::PTR, slot, info, Flags::NONE);
        check(&mut build, loaded, 4, 4);
        let one = build.iconst(Type::int(64), 1);
        let next = build.binary(Opcode::Add, counter, one, Flags::NSW);
        let limit = build.iconst(Type::int(64), 16);
        let again = build.icmp(IntPred::Slt, next, limit);
        build.br_if(again, head, &[next], done, &[]);
        Builder::new(&mut func, done).ret(&[]);

        let stats = hoisted(&mut func);
        assert!(!stats.changed());
        assert_eq!(stats.count(Kind::Missed, super::NOT_FOLLOWED), 1);
        assert_eq!(stats.count(Kind::Missed, super::NOT_A_SWEEP), 0, "and not as the other one");
        assert_eq!(checks(&func).len(), 1, "the check is still in the body");
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
    fn a_check_through_a_pointer_the_loop_does_not_move_comes_out_as_it_stands() {
        // The address is the same every iteration, so the check in front covers one access and
        // there is no arithmetic to write. This used to be reported as somebody else's to answer,
        // on the grounds that a check that does not move is `discharge`'s business, and that was
        // wrong: `discharge` takes out a second check the first made redundant and here there is
        // only ever the one instruction, so nothing it does makes the loop cheaper.
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
        assert!(stats.changed());
        assert_eq!(stats.count(Kind::Optimized, HOISTED), 1);
        let left = checks(&func);
        assert_eq!(left.len(), 1, "one check, and it is the one that was put in front");
        // Four bytes and not sixty four. Every iteration asked about the same four.
        assert_eq!(extent(&func, left[0].1), 4, "one access, since the address never moved");
        sound(&func, &mut names);
    }

    #[test]
    fn a_check_that_already_covers_a_computed_range_is_left_where_it_is() {
        // A check whose extent is an operand is one somebody worked out, and how many bytes it
        // covers is not a number this pass can multiply by a trip count. It is reported rather than
        // ignored so that a loop holding one is not counted as a loop with nothing in it.
        let (mut names, mut func, blocks) = walking(16, WIDTH, 4, 4);
        let head = blocks[1];
        let check = func
            .insts(head)
            .find(|&inst| func[inst].opcode == Opcode::CheckBounds)
            .expect("the body has a check");
        let [capability, pointer] = func[func[check].args] else { panic!("two operands") };
        let bytes = Builder::new(&mut func, head).iconst(Type::int(64), 64);
        let moved = inst_of(&func, bytes);
        func.remove_inst(moved);
        func.insert_before(moved, check);
        func[check].args = func.push_values(&[capability, pointer, bytes]);

        let stats = hoisted(&mut func);
        assert!(!stats.changed());
        assert_eq!(stats.count(Kind::Missed, super::ALREADY_COMPUTED), 1);
        sound(&func, &mut names);
    }

    #[test]
    fn fuel_stops_the_hoist_where_it_stands() {
        let (mut names, mut func, _) = walking(16, WIDTH, 4, 4);
        let mut an = crate::machine::fixtures::analyses();
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
