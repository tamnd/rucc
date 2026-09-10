//! Splits a loop into a run of iterations that needs no checks and the rest of it, which keeps them.
//!
//! Design: `spec/safe-memory/07-check-elimination.md` section 7.4, which names this and says what it
//! is for: "Loop splitting is the general form. The checked part and the unchecked part are divided
//! at `min(n, extent / sizeof(T))`."
//!
//! [`crate::hoist`] is the pass next door and it answers a different question. It puts one check in
//! front of a loop that covers every access the loop makes, which needs the loop to make every one
//! of them: an exact count, one way out, and a check every iteration reaches. Most loops in real code
//! are not like that. The census on tamnd/rucc#782 says that of the roughly fifteen hundred checks
//! SQLite still carries at `-O2`, six hundred and ninety two are in loops with a second way out and
//! sixty four are checks an iteration can finish without reaching. Neither is a loop hoisting can say
//! anything about, and both are loops this one can, because it never has to claim the loop reaches
//! the end of what it might read. It only has to know a prefix that is safe.
//!
//! # What the two halves are
//!
//! The loop is copied. The original becomes the fast half and loses its checks, the copy becomes the
//! slow half and keeps them, and a new block in front of the original decides which one runs. That
//! block counts iterations of the fast half and hands over to the slow half once the count reaches a
//! limit worked out in the preheader.
//!
//! The counter is a new one rather than the loop's own, and the test is against a limit rather than
//! against anything the loop compares. That is what makes this work on a loop with several ways out:
//! the fast half keeps every exit the loop had, so leaving early still leaves early, and the extra
//! test is only ever the reason the fast half stops early and never the reason it runs longer.
//!
//! # Where the limit comes from
//!
//! For a check whose address is `first + i * step` reading `reach` bytes each time, every iteration
//! with `i * step + reach <= extent` is one the check cannot fail on, where `extent` is how many
//! bytes from `first` on belong to whatever owns `first`. So the limit is
//! `(extent - reach) / step + 1`, or zero when `extent` is smaller than `reach`, and where a loop has
//! several such checks in it the limit is the smallest of theirs.
//!
//! The extent is the half of that a compiler cannot work out, so it is asked at run time, through the
//! `cap_extent` query that tamnd/rucc#792 added. The query takes a limit on how far to look and the
//! answer is never more than that, which is why this pass still wants a trip count: what it asks for
//! is how many bytes the loop was going to read anyway, so the walk in the runtime is bounded by work
//! the loop is already doing. A count that is too small costs iterations in the slow half and a count
//! that is too large costs a slightly longer walk, and neither is a wrong answer, which is why the
//! count is read from any exit that offers one rather than from an exit that runs every time.
//!
//! # Why the fast half may drop a check
//!
//! `check_bounds` asks whether the bytes an access names lie inside one object. Every address in
//! `[first, first + extent)` is inside the object that owns `first`, by what the query answers, and
//! the limit is exactly the iterations whose access stays in that window. So no check in the fast
//! half could have failed.
//!
//! `check_live` asks whether anything owns the address right now, and the query answered that too,
//! since a byte belonging to the owner of `first` is a byte with an owner. Right now is the catch,
//! and it is why nothing that could free may be in the loop. A call in the body could free the object
//! between the question and the iteration that reads it, and then the fast half would read freed
//! storage with nothing to say so.
//!
//! That is a question about the callee rather than about calling, and [`crate::nofree`] answers it
//! before the pipeline starts, so a call carrying [`rucc_ir::Flags::NOFREE`] is one the loop may
//! keep. Hoisting refuses every call whatever it does, and the reason is not this one: it needs the
//! loop to reach the end of what its count says, and a call that does not come back leaves it short.
//! Splitting never claims the loop reaches the end, so a call that might not come back costs it
//! nothing.
//!
//! Two answers of the query carry the weight and both are argued where the query is implemented. An
//! address no watched region covers gets the whole limit back, so a loop over a local or a global
//! splits into a fast half that runs the whole way, which is right because no check on such an
//! address ever fires under this milestone. An address whose granule nobody owns gets zero, so the
//! limit is zero, the fast half runs no iterations, and the check inside the slow half is what reports
//! the dangling pointer, at the access rather than at the loop.
//!
//! # Which loops
//!
//! Innermost, one latch, a preheader, a count, nothing in it that could free, and no value defined
//! inside it that anything outside reads. The last is loop closed form, which [`crate::canon`]
//! establishes, and it is checked rather than assumed because the copy would otherwise leave a reader
//! outside the loop seeing whichever half happened to define the value.
//!
//! Canonicalization runs a long way in front of this, and `simplify-cfg` between the two undoes some
//! of what it did, so on SQLite the closed form condition is what refuses 351 of the checks this
//! would otherwise have taken out. Running canonicalization again in front of this gets 156 of them
//! back and costs 17672 bytes of `.text`, which is a bad trade for eleven more checks, so the answer
//! is for this to repair the exits of the one loop it is splitting rather than for the pipeline to
//! repair every loop in the function. That is its own piece of work.
//!
//! Not every check in the loop has to be one this can size. A check whose address the analysis cannot
//! follow simply stays in both halves, and the fast half is then a loop with fewer checks in it rather
//! than none. That is worth having on its own and it is worth having because it is what a real loop
//! looks like: one sweep the analysis reads and one index that came out of a table.
//!
//! # Which level
//!
//! `-O2` and `-O3`, alongside `crate::unroll` and for the same reason. The loop body is copied, so
//! the function grows by about the size of the loop, and buying speed with code is what those levels
//! are for and what `-Os` and `-Oz` are for declining.

use std::collections::{HashMap, HashSet};

use rucc_cost::heuristics;
use rucc_ir::{
    Block, BlockCall, Builder, Extra, Flags, Func, Inst, InstData, IntPred, Opcode, Type, Value,
};

use crate::cfg::Cfg;
use crate::copy;
use crate::discharge::operand_of;
use crate::loops::{LoopId, Loops};
use crate::scev::{Evolution, Scev};
use crate::trip::{Around, counted, covered, inst_of};
use crate::{Analyses, Fuel, Pass, Preserved, Stats};

/// What is reported when a loop is split.
const SPLIT: &str = "loop split, the iterations in front of the first one that could fail a check \
                     run without them";

/// What is reported when the pass ran out of fuel with a loop it was about to split.
const NO_FUEL: &str = "loop left alone, the pass ran out of fuel";

/// What is reported for a loop with nowhere to work the limit out.
const NO_PREHEADER: &str = "loop left alone, it has no block in front of it to put a check in";

/// What is reported for a loop with a loop inside it.
const A_LOOP_INSIDE: &str = "loop left alone, it has another loop inside it";

/// What is reported for a loop with more than one way round.
const MANY_LATCHES: &str = "loop left alone, it goes back to its header from more than one place";

/// What is reported for a loop with a call in it that could free.
const A_CALL_INSIDE: &str = "loop left alone, a call in it might free what the loop is reading";

/// What is reported for a loop holding something that ends a lifetime outright.
const ENDS_A_LIFETIME: &str = "loop left alone, something in it ends a lifetime";

/// What is reported for a loop holding something the copier cannot copy.
const NOT_COPYABLE: &str = "loop left alone, something in it carries a side table this cannot copy";

/// What is reported for a loop whose values are read after it without going through a parameter.
const ESCAPES: &str = "loop left alone, a value it defines is read outside it";

/// What is reported for a loop whose two halves would be too much code.
const TOO_BIG: &str = "loop left alone, the two halves would be more code than the limit allows";

/// What is reported for a check whose address does not walk the loop.
const NOT_A_SWEEP: &str = "check kept in both halves, its address does not walk the loop by a \
                           constant";

/// What is reported for a check whose address the analysis has nothing to say about.
const NOT_FOLLOWED: &str = "check kept in both halves, what its address does round the loop is not \
                            something the analysis follows";

/// What is reported for a check whose address does not move.
const DOES_NOT_MOVE: &str = "check kept in both halves, its address is the same every iteration";

/// What is reported for a check whose address walks backwards.
const BACKWARDS: &str = "check kept in both halves, its address walks the loop from high to low";

/// What is reported for a check whose step does not keep its alignment.
const MISALIGNED: &str =
    "check kept in both halves, its step is not a whole number of its alignment";

/// What is reported for a check that already covers a range the program worked out.
const ALREADY_COMPUTED: &str =
    "check kept in both halves, how many bytes it covers is a number only the program has";

/// The pass.
#[derive(Debug)]
pub struct Split;

impl Pass for Split {
    fn name(&self) -> &'static str {
        "split"
    }

    fn describe(&self) -> &'static str {
        "a loop becomes a run of iterations with no checks in it and the rest of the loop with them"
    }

    fn preserves(&self) -> Preserved {
        // Blocks appear and edges move, so nothing built on the graph stands.
        Preserved::NONE
    }

    fn run(&self, func: &mut Func, an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        if func.entry().is_none() {
            return stats;
        }
        let cfg = an.cfg(func).clone();
        let loops = an.loops(func).clone();
        if loops.count() == 0 {
            return stats;
        }

        // Worked out first and applied afterwards, because scalar evolution reads the function and
        // the transformation writes it. Every plan is about an innermost loop and no two innermost
        // loops share a block, so applying one leaves every other one's blocks where they were.
        let mut plans = Vec::new();
        {
            let mut scev = Scev::new(func, &cfg, &loops);
            for id in loops.all() {
                sweep(func, &cfg, &loops, &mut scev, id, &mut plans, &mut stats);
            }
        }

        let mut changed = false;
        for plan in plans {
            if !fuel.take() {
                stats.missed(NO_FUEL);
                continue;
            }
            apply(func, &plan);
            stats.optimized(SPLIT);
            changed = true;
        }
        if changed {
            an.clear();
        }
        stats
    }
}

/// One check the fast half will not need, and the walk that says so.
#[derive(Debug)]
struct Sweep {
    /// The check itself, which is removed from the fast half and kept in the copy.
    check: Inst,
    /// Where the first iteration's address is computed from.
    base: Value,
    /// How far past that value the first iteration reads.
    offset: i128,
    /// How far the address moves each time round, which is a positive number of bytes.
    step: i128,
    /// How many bytes one access covers.
    reach: i128,
}

/// One loop to split, worked out before anything is written.
#[derive(Debug)]
struct Plan {
    /// Where the limit is worked out.
    preheader: Block,
    /// The block the guard takes over from.
    header: Block,
    /// The block the back edge leaves from, which is where the iteration count goes up.
    latch: Block,
    /// Everything that is copied, which is the whole loop.
    body: Vec<Block>,
    /// How many times the loop goes round, which is what the runtime is asked to look no further
    /// than.
    around: Around,
    /// The checks the fast half will not need, which is never empty in a plan.
    sweeps: Vec<Sweep>,
}

/// Plans a loop, or counts what stopped it.
///
/// Nothing is reported for a loop with no check in it, because a loop that does no memory access is
/// not a missed opportunity and a report for every one of them would bury the loops that are.
fn sweep(
    func: &Func,
    cfg: &Cfg,
    loops: &Loops,
    scev: &mut Scev<'_>,
    id: LoopId,
    plans: &mut Vec<Plan>,
    stats: &mut Stats,
) {
    let body = loops.blocks(id).to_vec();
    let checks: Vec<Inst> = body
        .iter()
        .flat_map(|&block| func.insts(block).collect::<Vec<Inst>>())
        .filter(|&inst| matches!(func[inst].opcode, Opcode::CheckBounds | Opcode::CheckLive))
        .collect();
    if checks.is_empty() {
        return;
    }

    let (preheader, latch) = match shaped(func, cfg, loops, id, &body) {
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

    let mut sweeps = Vec::new();
    for check in checks {
        match walked(func, scev, id, check) {
            Ok(sweep) => sweeps.push(sweep),
            Err(why) => stats.missed(why),
        }
    }
    if sweeps.is_empty() {
        return;
    }
    plans.push(Plan { preheader, header: loops.header(id), latch, body, around, sweeps });
}

/// The preheader and the latch of a loop this pass may copy, or why there is not one.
///
/// The conditions are the module comment's. The one worth restating is freeing, because it is the
/// only one that is about what the fast half is allowed to leave out rather than about whether the
/// copy can be made at all: the extent is asked once before the loop and believed for the whole of
/// the fast half, so anything that could hand the storage back in the middle would make the answer
/// stale, and the fast half has nothing left in it to notice.
///
/// Which is a question about the callee and not about calling, so it is asked of the callee.
/// [`crate::nofree`] settles it before the pipeline starts and writes the answer onto the call site,
/// and a call carrying it reaches nothing that ends a lifetime. Note that this is a weaker
/// requirement than [`crate::hoist`]'s, which refuses every call whatever it does, because hoisting
/// needs the loop to reach the end of what its count says and a call that does not come back leaves
/// it short. Splitting never claims that, so coming back is not something it needs.
fn shaped(
    func: &Func,
    cfg: &Cfg,
    loops: &Loops,
    id: LoopId,
    body: &[Block],
) -> Result<(Block, Block), &'static str> {
    let Some(preheader) = loops.preheader(cfg, id) else {
        return Err(NO_PREHEADER);
    };
    let [latch] = loops.latches(id) else {
        return Err(MANY_LATCHES);
    };
    for &block in body {
        if loops.innermost(block) != Some(id) {
            return Err(A_LOOP_INSIDE);
        }
        for inst in func.insts(block) {
            match func[inst].opcode {
                Opcode::Call | Opcode::CallIndirect | Opcode::TailCall
                    if !func[inst].flags.contains(Flags::NOFREE) =>
                {
                    return Err(A_CALL_INSIDE);
                }
                // Assembly could do anything and the two meta instructions end a lifetime by
                // definition, which is the same answer `crate::nofree` gives for all three.
                Opcode::InlineAsm | Opcode::MetaEnd | Opcode::MetaTransfer => {
                    return Err(ENDS_A_LIFETIME);
                }
                _ => {}
            }
            if !copy::copyable(func, inst) {
                return Err(NOT_COPYABLE);
            }
        }
    }
    let inside: HashSet<Block> = body.iter().copied().collect();
    if escapes(func, body, &inside) {
        return Err(ESCAPES);
    }
    let size = body.iter().map(|&block| func.insts(block).count()).sum::<usize>();
    if size > heuristics::SPLIT_MAX_INSNS as usize {
        return Err(TOO_BIG);
    }
    Ok((preheader, *latch))
}

/// Whether anything outside the loop reads a value defined inside it.
///
/// After [`crate::canon`] there is no such value, because loop closed form has already routed every
/// one of them through a parameter of the block the loop leaves to. Where there is one, the two
/// halves would leave it reading whichever of them happened to define it, so this is refused rather
/// than repaired.
fn escapes(func: &Func, body: &[Block], inside: &HashSet<Block>) -> bool {
    let mut defined: HashSet<Value> = HashSet::new();
    for &block in body {
        defined.extend(func[block].params.iter().copied());
        for inst in func.insts(block) {
            defined.extend(func[inst].results());
        }
    }
    for block in func.blocks() {
        if inside.contains(&block) {
            continue;
        }
        for inst in func.insts(block) {
            if func[func[inst].args].iter().any(|value| defined.contains(value)) {
                return true;
            }
            for call in func.successors(inst) {
                if func[call.args].iter().any(|value| defined.contains(value)) {
                    return true;
                }
            }
        }
    }
    false
}

/// What one check's address does round the loop, or why the pass cannot say.
fn walked(
    func: &Func,
    scev: &mut Scev<'_>,
    id: LoopId,
    check: Inst,
) -> Result<Sweep, &'static str> {
    let args = &func[func[check].args];
    // A check that already carries its own extent is one hoisting put somewhere, and how many bytes
    // it covers is not a number this pass can divide by a step.
    if args.len() > 2 {
        return Err(ALREADY_COMPUTED);
    }
    let (Some(&capability), Some(&pointer)) = (args.first(), args.get(1)) else {
        return Err(NOT_A_SWEEP);
    };
    if operand_of(func, capability, Opcode::CapOf, 0) != Some(pointer) {
        return Err(NOT_A_SWEEP);
    }
    // A liveness check reads no bytes, so the window it needs is the one byte its address is in.
    // A bounds check carries how many it reads in its payload.
    let (reach, align) = match func[check].extra {
        Extra::Mem(held) => (i128::from(func[held].size), i128::from(func[held].align)),
        _ => (1, 1),
    };

    let chrec = match scev.evolution(id, pointer) {
        Evolution::Affine(chrec) => chrec,
        // An address that does not move at all is one check covering the same bytes every time
        // round, which is a check to hoist rather than a loop to split, and hoisting is where that
        // belongs. It is reported separately so the census says how often the two passes disagree
        // about the same loop.
        Evolution::Invariant(_) => return Err(DOES_NOT_MOVE),
        _ => return Err(NOT_FOLLOWED),
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
    if step % align != 0 {
        return Err(MISALIGNED);
    }
    Ok(Sweep { check, base, offset: chrec.base.offset, step, reach })
}

/// Makes the two halves and the block that chooses between them.
///
/// The order matters in two places. The copy is made before anything is rewired, so the copy's back
/// edge is remapped to the copy's own header rather than to a guard that did not exist yet. The
/// checks come out of the fast half last, so the copy still has them.
fn apply(func: &mut Func, plan: &Plan) {
    // The slow half, which is the loop as it stands, under a substitution that renames everything it
    // defines. Nothing is seeded, so its header gets parameters of its own, which is what a copy
    // reached from a block that also reaches the original needs.
    let mut renamed: HashMap<Value, Value> = HashMap::new();
    let copies = copy::blocks(func, &plan.body, &mut renamed);
    let slow = copies[&plan.header];

    // The guard, which takes over the header's place: the preheader arrives here, the back edge
    // comes back to here, and the header is reached from here and nowhere else. Its first parameter
    // is a counter of its own, because the loop's counter is not something this pass has to find and
    // a loop with several ways out may not have one.
    let word = Type::int(64);
    let types: Vec<Type> = func[plan.header].params.iter().map(|&param| func[param].ty).collect();
    let guard = func.create_block();
    let round = func.append_param(guard, word);
    let carried: Vec<Value> = types.iter().map(|&ty| func.append_param(guard, ty)).collect();

    let (limit, start) = limited(func, plan);

    let mut build = Builder::new(func, guard);
    let inside = build.icmp(IntPred::Slt, round, limit);
    build.br_if(inside, plan.header, &carried, slow, &carried);

    // The way in, which now hands the guard a count of no iterations so far.
    let term = func.terminator(plan.preheader).expect("a preheader ends in a jump to the header");
    route(func, term, plan.header, guard, start);

    // The way round, which counts one more. The counter is the guard's parameter and the guard
    // dominates every block in the fast half, so the latch may read it.
    let term = func.terminator(plan.latch).expect("a latch ends in a branch back to the header");
    let mut build = Builder::new(func, plan.latch);
    let one = build.iconst(word, 1);
    let next = build.binary(Opcode::Add, round, one, Flags::NSW);
    for value in [one, next] {
        let inst = inst_of(func, value);
        func.remove_inst(inst);
        func.insert_before(inst, term);
    }
    route(func, term, plan.header, guard, next);

    // The checks the fast half does not need. The `cap_of` each one was reading is left where it is,
    // for `dce` after this pass to take away, which is the arrangement `crate::hoist` and
    // `crate::discharge` are both in.
    for sweep in &plan.sweeps {
        func.remove_inst(sweep.check);
    }
}

/// Sends every edge this terminator has to `from` to `to` instead, with one more argument in front.
fn route(func: &mut Func, term: Inst, from: Block, to: Block, first: Value) {
    for at in func.target_list(term).iter() {
        let call = func[at];
        if call.block != from {
            continue;
        }
        let mut args = vec![first];
        args.extend_from_slice(&func[call.args]);
        let args = func.push_values(&args);
        func.set_block_call(at, BlockCall { block: to, args });
    }
}

/// Builds the number of iterations the fast half may run, and the zero the preheader starts it at.
///
/// One `cap_extent` per check and the smallest of what they allow, all of it in the preheader in
/// front of the jump into the loop. A builder appends to the end of a block, which in a block that
/// already has its terminator is after it, so everything is built first and then moved in front of
/// the terminator in the order it was built.
fn limited(func: &mut Func, plan: &Plan) -> (Value, Value) {
    let term = func.terminator(plan.preheader).expect("a preheader ends in a jump to the header");
    let mut made = Vec::new();
    let mut build = Builder::new(func, plan.preheader);
    let start = build.iconst(Type::int(64), 0);
    made.push(start);

    let mut limit: Option<Value> = None;
    for sweep in &plan.sweeps {
        let allows = reachable(&mut build, &mut made, sweep, plan.around);
        limit = Some(match limit {
            None => allows,
            Some(so_far) => {
                let smaller = build.icmp(IntPred::Slt, allows, so_far);
                made.push(smaller);
                let least = build.select(smaller, allows, so_far);
                made.push(least);
                least
            }
        });
    }
    let limit = limit.expect("a plan holds at least one check");

    for value in made {
        let inst = inst_of(func, value);
        func.remove_inst(inst);
        func.insert_before(inst, term);
    }
    (limit, start)
}

/// How many iterations one check allows, which is `(extent - reach) / step + 1` and never negative.
///
/// The subtraction and the addition carry `nsw` and the division does not need it. Everything here
/// is worked out from the extent, which the runtime answers with a count of bytes it walked and so
/// is never negative and never larger than the object, so none of this can leave sixty four bits
/// whatever the limit it was asked for turned out to be.
fn reachable(
    build: &mut Builder<'_>,
    made: &mut Vec<Value>,
    sweep: &Sweep,
    around: Around,
) -> Value {
    let word = Type::int(64);
    let first = if sweep.offset == 0 {
        sweep.base
    } else {
        let by = build.iconst(word, sweep.offset);
        made.push(by);
        let args = build.func().push_values(&[sweep.base, by]);
        let sum = build.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR);
        made.push(sum);
        sum
    };
    // How many bytes the loop was going to read, which is how far the runtime is asked to look and
    // nothing more. An answer short of the truth costs iterations in the slow half and is never
    // wrong, so a count that saturates rather than one that refuses is the right thing here.
    let want = match around {
        Around::Number(times) => {
            let far = times.saturating_mul(sweep.step).saturating_add(sweep.reach);
            let far = i64::try_from(far).unwrap_or(i64::MAX);
            let bytes = build.iconst(word, i128::from(far));
            made.push(bytes);
            bytes
        }
        Around::Computed(count, reading) => {
            covered(build, made, count, sweep.step, sweep.reach, reading, Flags::NONE)
        }
    };

    let args = build.func().push_values(&[first]);
    let capability = build.value(InstData { args, ..InstData::new(Opcode::CapOf) }, Type::CAP);
    made.push(capability);
    let args = build.func().push_values(&[capability, first, want]);
    let extent = build.value(InstData { args, ..InstData::new(Opcode::CapExtent) }, word);
    made.push(extent);

    let reach = build.iconst(word, sweep.reach);
    made.push(reach);
    let left = build.binary(Opcode::Sub, extent, reach, Flags::NSW);
    made.push(left);
    let zero = build.iconst(word, 0);
    made.push(zero);
    // Not even one access fits, which is a dangling pointer or an object smaller than the thing being
    // read out of it. The fast half runs no iterations and the check in the slow half reports it, at
    // the access rather than at the loop.
    let short = build.icmp(IntPred::Slt, left, zero);
    made.push(short);

    let mut steps = left;
    if sweep.step != 1 {
        let by = build.iconst(word, sweep.step);
        made.push(by);
        steps = build.binary(Opcode::SDiv, left, by, Flags::NONE);
        made.push(steps);
    }
    let one = build.iconst(word, 1);
    made.push(one);
    let allows = build.binary(Opcode::Add, steps, one, Flags::NSW);
    made.push(allows);
    let clamped = build.select(short, zero, allows);
    made.push(clamped);
    clamped
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{
        Block, Builder, Extra, Flags, Func, Inst, InstData, IntPred, MemInfo, MemOrder, Module,
        Opcode, Restrict, Signature, Type, Value, verify_func,
    };
    use rucc_target::{TargetInfo, Triple};

    use super::{SPLIT, Split};
    use crate::canon::Canon;
    use crate::stats::Kind;
    use crate::{Fuel, Pass, Stats};

    /// How many times the loop goes round, and how wide each element of the walk is.
    const TRIPS: i128 = 16;
    const WIDTH: i128 = 4;

    /// A counted loop that reads one element each time round and can stop on what it read.
    ///
    /// ```text
    /// entry(a): jump head(0)
    /// head(i):  p = a + i*4; check_bounds cap_of(p), p; v = load p
    ///           br v == 0 -> done, more
    /// more:     next = i + 1; br next < 16 -> head(next), done
    /// done:     ret
    /// ```
    ///
    /// The second way out is the point. Hoisting refuses this loop, because a loop that can stop in
    /// the middle reads fewer bytes than its count says and one check in front of it for all of them
    /// would refuse a program that was right. Splitting does not care, because the count it reads is
    /// only ever an upper limit on how far to look.
    fn leaving() -> (Interner, Func, Vec<Block>) {
        walking(Some(TRIPS))
    }

    /// The same loop, with how many times it goes round handed in rather than written down.
    ///
    /// What this reaches is the other half of [`crate::trip::covered`], the one that builds the
    /// count out of something the loop does not change. It is worth its own test because that
    /// arithmetic promises not to wrap for hoisting and promises nothing for this pass, and the two
    /// callers now ask for different things from the same code.
    fn counting() -> (Interner, Func, Vec<Block>) {
        walking(None)
    }

    /// Builds the loop, with the exit test against a number or against a second parameter.
    fn walking(times: Option<i128>) -> (Interner, Func, Vec<Block>) {
        let mut names = Interner::new();
        let mut params = vec![Type::PTR];
        params.extend(times.is_none().then_some(Type::int(64)));
        let mut func = Func::new(names.intern("f"), Signature::new().with_params(&params));
        let entry = func.create_block();
        let head = func.create_block();
        let more = func.create_block();
        let done = func.create_block();
        let array = func.append_param(entry, Type::PTR);
        let handed = times.is_none().then(|| func.append_param(entry, Type::int(64)));
        let counter = func.append_param(head, Type::int(64));

        let zero = Builder::new(&mut func, entry).iconst(Type::int(64), 0);
        Builder::new(&mut func, entry).jump(head, &[zero]);

        let mut build = Builder::new(&mut func, head);
        let by = build.iconst(Type::int(64), WIDTH);
        let scaled = build.binary(Opcode::Mul, counter, by, Flags::NSW);
        let args = build.func().push_values(&[array, scaled]);
        let pointer = build.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR);
        check(&mut build, pointer);
        let read = build.load(Type::int(32), pointer, mem(), Flags::NONE);
        let nothing = build.iconst(Type::int(32), 0);
        let stop = build.icmp(IntPred::Eq, read, nothing);
        build.br_if(stop, done, &[], more, &[]);

        let mut build = Builder::new(&mut func, more);
        let one = build.iconst(Type::int(64), 1);
        let next = build.binary(Opcode::Add, counter, one, Flags::NSW);
        let limit = match (times, handed) {
            (Some(times), _) => build.iconst(Type::int(64), times),
            (None, handed) => handed.expect("a loop with no number for a limit was handed one"),
        };
        let again = build.icmp(IntPred::Slt, next, limit);
        build.br_if(again, head, &[next], done, &[]);
        Builder::new(&mut func, done).ret(&[]);
        (names, func, vec![entry, head, more, done])
    }

    /// What one access in the loop covers.
    fn mem() -> MemInfo {
        MemInfo {
            size: WIDTH as u64,
            align: WIDTH as u32,
            order: MemOrder::NotAtomic,
            tbaa: None,
            restrict: Restrict::NONE,
        }
    }

    /// Puts `cap_of` and a `check_bounds` at `pointer` into a block.
    ///
    /// The shape `rucc-safety` emits, written out here rather than reached for, because `rucc-opt`
    /// is rank 9 alongside `rucc-safety` and cannot depend on it.
    fn check(build: &mut Builder<'_>, pointer: Value) {
        let args = build.func().push_values(&[pointer]);
        let capability = build.value(InstData { args, ..InstData::new(Opcode::CapOf) }, Type::CAP);
        let args = build.func().push_values(&[capability, pointer]);
        let extra = Extra::Mem(build.func().add_mem(mem()));
        build.inst(InstData { args, extra, ..InstData::new(Opcode::CheckBounds) }, &[]);
    }

    /// Canonicalizes and then splits, with as much fuel as both want.
    ///
    /// Both, because the pass is written against the shape [`Canon`] leaves, and it is
    /// canonicalization that gives the loop the preheader the limit is worked out in.
    fn split_up(func: &mut Func) -> Stats {
        let mut an = crate::machine::fixtures::analyses();
        Canon.run(func, &mut an, &mut Fuel::unlimited());
        Split.run(func, &mut an, &mut Fuel::unlimited())
    }

    /// Every instruction in the function with this opcode, and the block it is in.
    fn all(func: &Func, opcode: Opcode) -> Vec<(Block, Inst)> {
        func.blocks()
            .flat_map(|block| func.insts(block).map(move |inst| (block, inst)).collect::<Vec<_>>())
            .filter(|&(_, inst)| func[inst].opcode == opcode)
            .collect()
    }

    /// Insists the function is one the rest of the compiler may believe.
    ///
    /// This is what the tests here rest on. The pass makes a second copy of a loop, gives a new
    /// block parameters that stand for the old header's, and moves a preheader's worth of
    /// arithmetic in front of a terminator that was already there, so whether every value is in
    /// scope where it is read is not something reading the code settles.
    fn sound(func: &Func, names: &mut Interner) {
        let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().unwrap());
        let module = Module::new(names.intern("t.c"), &target);
        if let Err(errors) = verify_func(&module, func, names) {
            panic!("{errors:#?}");
        }
    }

    #[test]
    fn a_loop_that_can_stop_early_is_split_even_though_hoisting_will_not_touch_it() {
        // The census row this pass was written for. Of the checks SQLite still carries at -O2, the
        // largest group by far is in loops with a second way out, which is exactly the loop here.
        let (mut names, mut func, _) = leaving();
        let mut an = crate::machine::fixtures::analyses();
        Canon.run(&mut func, &mut an, &mut Fuel::unlimited());
        let refused = crate::hoist::Hoist.run(&mut func, &mut an, &mut Fuel::unlimited());
        assert!(!refused.changed(), "hoisting has nothing to say about this loop");

        let stats = Split.run(&mut func, &mut an, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Optimized, SPLIT), 1);
        sound(&func, &mut names);
    }

    #[test]
    fn the_half_the_loop_runs_first_has_no_check_in_it_and_the_other_one_keeps_it() {
        // One check went in and one check came out, and the one that came out is in the copy. That
        // is the whole transformation: the same work, with the checking half reached only once the
        // guard says the run of safe iterations is over.
        let (mut names, mut func, blocks) = leaving();
        let head = blocks[1];
        split_up(&mut func);

        let left = all(&func, Opcode::CheckBounds);
        assert_eq!(left.len(), 1, "one check, and it is the one the slow half kept");
        assert_ne!(left[0].0, head, "and it is not in the block the loop started in");
        sound(&func, &mut names);
    }

    #[test]
    fn how_far_the_runtime_is_asked_to_look_is_settled_in_front_of_the_loop() {
        // The one thing a compiler cannot work out here is how many bytes belong to the object, so
        // it is asked, once, before the loop starts. Once is what makes this worth doing: a query
        // per loop in place of a check per iteration.
        let (mut names, mut func, _) = leaving();
        split_up(&mut func);

        let asked = all(&func, Opcode::CapExtent);
        assert_eq!(asked.len(), 1, "one question for the one check that was sized");
        let cfg = crate::Cfg::new(&func);
        let doms = crate::Dominators::new(&cfg);
        let loops = crate::Loops::new(&cfg, &doms);
        assert!(
            loops.all().all(|id| !loops.contains(id, asked[0].0)),
            "and it is outside the loop"
        );
        sound(&func, &mut names);
    }

    #[test]
    fn a_loop_with_a_call_in_it_that_might_free_is_left_alone() {
        // The extent is asked once and believed for the whole of the fast half, so anything that
        // could hand the storage back in the middle makes the answer stale and the fast half has
        // nothing left in it to notice.
        let (_, mut func, _) = calling(Flags::NONE);
        let stats = split_up(&mut func);
        assert!(!stats.changed());
        assert_eq!(stats.count(Kind::Missed, super::A_CALL_INSIDE), 1);
    }

    #[test]
    fn a_loop_with_a_call_in_it_that_cannot_free_is_split() {
        // Whether the storage can be handed back is a question about the callee, and `crate::nofree`
        // answers it before the pipeline starts. This is the largest row of the census by a long way,
        // and it is also the row where this pass and hoisting come apart the furthest: hoisting
        // refuses a call whatever it does, because it needs the loop to reach the end of what its
        // count says, and this never claims that.
        let (mut names, mut func, _) = calling(Flags::NOFREE);
        let stats = split_up(&mut func);
        assert_eq!(stats.count(Kind::Optimized, SPLIT), 1);
        assert_eq!(all(&func, Opcode::CheckBounds).len(), 1, "the fast half lost its check");
        assert_eq!(all(&func, Opcode::Call).len(), 2, "and both halves kept the call");
        sound(&func, &mut names);
    }

    /// The loop with a call added to its latch, carrying whatever the caller says about it.
    fn calling(flags: Flags) -> (Interner, Func, Vec<Block>) {
        let (mut names, mut func, blocks) = leaving();
        let more = blocks[2];
        let term = func.terminator(more).expect("the latch branches");
        let callee = names.intern("somewhere");
        let signature = func.add_signature(Signature::new());
        let call = Builder::new(&mut func, more).call(callee, signature, &[]);
        func[call].flags |= flags;
        func.remove_inst(call);
        func.insert_before(call, term);
        (names, func, blocks)
    }

    #[test]
    fn a_check_whose_address_does_not_move_is_left_to_hoisting() {
        // One check on the array itself, every time round. Nothing about it gets better from being
        // in the fast half, because it is the same bytes every iteration, which is a check to lift
        // out of the loop rather than a loop to divide in two.
        let (_, mut func, blocks) = leaving();
        let (entry, head) = (blocks[0], blocks[1]);
        let array = func[entry].params[0];
        let term = func.terminator(head).expect("the header branches");
        let mut build = Builder::new(&mut func, head);
        check(&mut build, array);
        let made: Vec<Inst> = func.insts(head).skip_while(|&inst| inst != term).skip(1).collect();
        for inst in made {
            func.remove_inst(inst);
            func.insert_before(inst, term);
        }

        let stats = split_up(&mut func);
        assert_eq!(stats.count(Kind::Missed, super::DOES_NOT_MOVE), 1);
        assert_eq!(
            all(&func, Opcode::CheckBounds).len(),
            3,
            "the walking check left the fast half, and the still one stayed in both"
        );
    }

    #[test]
    fn a_loop_whose_count_is_an_expression_is_split_on_what_that_expression_says() {
        // How far to look is worked out in the preheader rather than written down, out of a value
        // the loop does not change. Nothing here promises the arithmetic stays inside sixty four
        // bits, and it does not have to: a limit that wrapped is still answered with a true count
        // of the bytes that belong to the object.
        let (mut names, mut func, _) = counting();
        let stats = split_up(&mut func);
        assert_eq!(stats.count(Kind::Optimized, SPLIT), 1);
        assert_eq!(all(&func, Opcode::CapExtent).len(), 1);
        assert_eq!(all(&func, Opcode::CheckBounds).len(), 1, "the fast half lost its check");
        sound(&func, &mut names);
    }

    #[test]
    fn the_pass_stops_when_the_fuel_runs_out() {
        // What `-fopt-fuel` is for, and the reason every transformation here goes through the
        // counter rather than round it.
        let (_, mut func, _) = leaving();
        let mut an = crate::machine::fixtures::analyses();
        Canon.run(&mut func, &mut an, &mut Fuel::unlimited());
        let stats = Split.run(&mut func, &mut an, &mut Fuel::of(0));
        assert!(!stats.changed());
        assert_eq!(stats.count(Kind::Missed, super::NO_FUEL), 1);
    }
}
