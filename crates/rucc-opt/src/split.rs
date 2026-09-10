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
//! block carries an offset in bytes from the first access, walks it on by the step every time round,
//! and hands over to the slow half once the offset passes a window worked out in the preheader.
//!
//! The offset is a new value rather than one of the loop's own pointers, and the test is against
//! the window rather than against anything the loop compares. That is what makes this work on a loop
//! with several ways out: the fast half keeps every exit the loop had, so leaving early still leaves
//! early, and the extra test is only ever the reason the fast half stops early and never the reason
//! it runs longer.
//!
//! A loop where no address moves gets neither the block nor the offset. Which half runs is settled
//! by an answer that does not change while the loop runs, so the way into the loop is where the two
//! halves are chosen between and there is nothing to carry. Half the loops this takes on SQLite are
//! that shape.
//!
//! # Where the window comes from
//!
//! For a check whose address is `first + delta` reading `reach` bytes each time, every offset with
//! `delta + reach <= extent` is one the check cannot fail on, where `extent` is how many bytes from
//! `first` on belong to whatever owns `first`. So the window is `extent - reach`, and where a loop
//! has several checks that walk by the same amount the window is the smallest of theirs.
//!
//! Bytes rather than iterations, and that is the whole of the arithmetic. An earlier version of this
//! counted iterations against `(extent - reach) / step + 1`, which is the same transformation and a
//! much harder claim: it has a symbolic multiply and a symbolic divide in it at sixty four bits, and
//! z3 does not finish on it in two and a half minutes in any of three formulations, so the pass sat
//! outside the rule table that `spec/safe-memory/07-check-elimination.md` section 7.7 asks every
//! elimination to be inside. In bytes it is `swept.sym.i64`, which is already in that table and
//! already proved, and the pass asks it rather than deciding. `limited` below is where that
//! happens.
//!
//! The extent is the half of that a compiler cannot work out, so it is asked at run time, through the
//! `cap_extent` query that tamnd/rucc#792 added. The query takes a limit on how far to look and the
//! answer is never more than that, which is why this pass still wants a trip count: what it asks for
//! is how many bytes the loop was going to read anyway, so the walk in the runtime is bounded by work
//! the loop is already doing. A count that is too small costs iterations in the slow half and a count
//! that is too large costs a slightly longer walk, and neither is a wrong answer, which is why the
//! count is read from any exit that offers one rather than from an exit that runs every time.
//!
//! An address that does not move is the same expression with a step of zero, and its offset is zero
//! on every iteration, so there is nothing to carry and the window question collapses into whether
//! the one access fits. How far the runtime is asked to look is then just the bytes the access reads.
//! Hoisting would rather have these, and it takes the ones in loops it is willing to touch. What is
//! left over is the ones in loops it refused for one of its own reasons, a second way out or a call
//! inside, and those come back here.
//!
//! # Why the fast half may drop a check
//!
//! `check_bounds` asks whether the bytes an access names lie inside one object. Every address in
//! `[first, first + extent)` is inside the object that owns `first`, by what the query answers, and
//! the window is exactly the offsets whose access stays inside that. So no check in the fast half
//! could have failed.
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
//! Innermost, one latch, a preheader, nothing in it that could free, and no value defined inside it
//! that anything outside reads. Not a count, unlike hoisting, because the count is not something
//! this rests on: it is spent on how far to ask the runtime to look, and the runtime answers with a
//! true count of the bytes that belong to the object whatever it was asked for. A loop nobody
//! counted gets the same guess everything else that has to guess about a loop gets, ten, which is
//! GCC's `avg-loop-niter` and the number [`crate::scev::Estimate`] already hands out. The last is
//! loop closed form, which [`crate::canon`] establishes, and it is checked rather than assumed
//! because the copy would otherwise leave a reader outside the loop seeing whichever half happened
//! to define the value.
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

use crate::canon;
use crate::cfg::Cfg;
use crate::copy;
use crate::discharge::{Question, operand_of, yes};
use crate::dom::Dominators;
use crate::loops::{LoopId, Loops};
use crate::rules::safety;
use crate::scev::{Evolution, Plain, Scev};
use crate::trip::{Around, counted, covered, inst_of};
use crate::{Analyses, Fuel, Pass, Preserved, Stats};

/// What is reported when a loop is split.
const SPLIT: &str = "loop split, the iterations in front of the first one that could fail a check \
                     run without them";

/// What is reported when a loop had to be put back into closed form before it could be split.
const CLOSED_HERE: &str = "loop put back into closed form, a value it defines is read after it and both halves define one";

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

/// What is reported for a check whose address walks backwards.
const BACKWARDS: &str = "check kept in both halves, its address walks the loop from high to low";

/// What is reported for a check whose step does not keep its alignment.
const MISALIGNED: &str =
    "check kept in both halves, its step is not a whole number of its alignment";

/// What is reported for a check that already covers a range the program worked out.
const ALREADY_COMPUTED: &str =
    "check kept in both halves, how many bytes it covers is a number only the program has";

/// What is reported for a check the rule table will not say yes about.
const NOT_PROVED: &str = "check kept in both halves, no rule in the safety namespace says an offset inside the window is \
     an access inside the object";

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
        let mut plans = planned(func, &cfg, &loops, &mut stats);

        // Closed form put back where it is missing, before anything is copied. The repair adds a
        // block parameter and rewrites uses, so it moves no edge and creates no block, which is why
        // the graph and the loop forest above are both still good after it. What it does move is
        // which value a use inside another loop names, and a plan is a list of values, so a repair
        // means the plans are worked out again rather than trusted. The stats go with them, or the
        // first round's reasons would be counted twice.
        let dom = an.dominators(func).clone();
        let repairs = repaired(func, &cfg, &dom, &loops, &plans, fuel);
        if repairs.made > 0 {
            stats = Stats::new();
            plans = planned(func, &cfg, &loops, &mut stats);
            for _ in 0..repairs.worked {
                stats.optimized(CLOSED_HERE);
            }
        }
        plans.retain(|plan| {
            if leaving(func, plan) {
                stats.missed(ESCAPES);
                return false;
            }
            true
        });

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
    /// How far past that value the first iteration reads, in bytes. Usually a number, and a value
    /// and a scale beside it when the loop started its counter at something it was handed. See
    /// `spare` for how it is built and #810 for what it is worth.
    apart: Plain,
    /// How far the address moves each time round, which is a number of bytes and never negative.
    /// Zero is an address that does not move, which is allowed and puts no limit on the loop.
    step: i128,
    /// How many bytes one access covers.
    reach: i128,
}

/// One loop to split, worked out before anything is written.
#[derive(Debug)]
struct Plan {
    /// The loop itself, which is read again when its closed form has to be repaired.
    id: LoopId,
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
    // Not a refusal when there is no count, unlike in hoisting, because the count is not something
    // this rests on. It is spent on how far to ask the runtime to look, and the runtime answers with
    // a true count of the bytes that belong to the object whatever it was asked for. A guess that is
    // too small costs iterations in the half that keeps its checks and a guess that is too large
    // costs a slightly longer walk, so a loop nobody counted gets the same guess everything else
    // that has to guess about a loop gets.
    let around =
        counted(scev, id).unwrap_or(Around::Number(i128::from(crate::scev::ASSUMED_ITERATIONS)));

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
    plans.push(Plan { id, preheader, header: loops.header(id), latch, body, around, sweeps });
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
    let size = body.iter().map(|&block| func.insts(block).count()).sum::<usize>();
    if size > heuristics::SPLIT_MAX_INSNS as usize {
        return Err(TOO_BIG);
    }
    Ok((preheader, *latch))
}

/// Every loop in the function that is worth copying, and why each of the others is not.
fn planned(func: &Func, cfg: &Cfg, loops: &Loops, stats: &mut Stats) -> Vec<Plan> {
    let mut plans = Vec::new();
    let mut scev = Scev::new(func, cfg, loops);
    for id in loops.all() {
        sweep(func, cfg, loops, &mut scev, id, &mut plans, stats);
    }
    plans
}

/// How many loops the closed form repair touched, and how many of those it finished.
///
/// Two numbers rather than one because they answer different questions. Anything touched at all is
/// why the plans have to be worked out again, and only the ones it finished are loops that can now
/// be copied and so are what gets reported.
struct Repairs {
    /// Loops the repair wrote something into.
    made: usize,
    /// Loops that are in closed form afterwards.
    worked: usize,
}

/// Puts the loops that need it back into closed form, before anything is copied.
///
/// [`crate::canon`] establishes closed form a long way in front of this pass and `simplify-cfg`
/// between the two undoes some of what it did. Running the whole of canonicalization again was
/// measured and it costs 17672 bytes of `.text` on the SQLite amalgamation, because it repairs every
/// loop in the function rather than the ones about to be copied. This repairs those, which costs
/// nothing on a function with no loop to split.
///
/// Not every loop can be repaired this way. A value read past a join that no single exit dominates
/// needs a parameter at the join as well as at each exit, and the repair adds one at the exits only,
/// so the count of what worked is a second look rather than an assumption that the first one did.
fn repaired(
    func: &mut Func,
    cfg: &Cfg,
    dom: &Dominators,
    loops: &Loops,
    plans: &[Plan],
    fuel: &mut Fuel,
) -> Repairs {
    let mut repairs = Repairs { made: 0, worked: 0 };
    for plan in plans {
        if !leaving(func, plan) {
            continue;
        }
        let mut wrote = false;
        while let Some(job) = canon::leaked(func, cfg, dom, loops, plan.id) {
            if !fuel.take() {
                break;
            }
            canon::close(func, &job);
            wrote = true;
        }
        if !wrote {
            continue;
        }
        repairs.made += 1;
        if !leaving(func, plan) {
            repairs.worked += 1;
        }
    }
    repairs
}

/// Whether anything after this loop reads a value its body defines.
fn leaving(func: &Func, plan: &Plan) -> bool {
    let inside: HashSet<Block> = plan.body.iter().copied().collect();
    escapes(func, &plan.body, &inside)
}

/// Whether anything outside the loop reads a value defined inside it.
///
/// Where there is one, the two halves would leave it reading whichever of them happened to define
/// it. Closed form is what makes it not one: the use names a parameter of the block the loop leaves
/// to, and each half fills that parameter in on its own way out.
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

    // Whether an offset inside the window means an access inside the object, which is what dropping
    // this check rests on and is not something this file decides. Asked per check rather than once,
    // because the reach is the one number in the rule the pass has and it is this check's.
    if !windowed(reach) {
        return Err(NOT_PROVED);
    }

    // An address that does not move is a sweep with a step of zero, and the arithmetic below takes
    // it without a special case anywhere. Hoisting would rather have these, but
    // hoisting only gets the ones in loops it is willing to touch at all, and a loop it refused for
    // one of its own reasons leaves the check where it is. Splitting is willing to touch more loops,
    // so the same check comes back here and there is no reason to hand it back.
    let (start, step) = match scev.evolution(id, pointer) {
        Evolution::Affine(chrec) => {
            let Some(step) = chrec.step.as_number() else {
                return Err(NOT_A_SWEEP);
            };
            (chrec.base, step)
        }
        Evolution::Invariant(base) => (base, 0),
        _ => return Err(NOT_FOLLOWED),
    };
    if step < 0 {
        return Err(BACKWARDS);
    }
    // Scale one because the base is an address. Anything else is a multiple of a pointer, which is
    // not a thing the loop computed, so it is a shape this reads rather than a case to handle.
    //
    // The second arm is `a + 8 * start`, an address the loop reached before it began, which is what
    // a counter the caller handed in looks like once the front end has multiplied the element size
    // through it. The pointer is the side the whole thing is measured from and the index is what is
    // scaled beside it, so anything else with two values in it is refused here rather than turned
    // into an address off whichever value came first.
    let (base, apart) = match (start.plain(), start.on()) {
        (Some(at @ Plain { value: Some(base), scale: 1, .. }), _) => {
            (base, Plain { value: None, scale: 0, offset: at.offset })
        }
        (_, Some((base, apart))) if walks(func, base, apart) => (base, apart),
        _ => return Err(NOT_A_SWEEP),
    };
    if step != 0 && step % align != 0 {
        return Err(MISALIGNED);
    }
    Ok(Sweep { check, base, apart, step, reach })
}

/// Whether a pointer and a byte displacement beside it are the two the address is really built out
/// of, rather than two values an expression happened to end up holding.
///
/// The displacement has to be as wide as the arithmetic, because what is built from it here is a
/// `ptr_add` in a preheader and a narrower value would need widening, and which widening depends on
/// how the loop read it, which is not a question this pass has an answer to.
fn walks(func: &Func, base: Value, apart: Plain) -> bool {
    let Some(value) = apart.value else { return false };
    func[base].ty.is_ptr() && func[value].ty == Type::int(64)
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

    let Choice { ok, windows } = limited(func, plan);

    // Nothing in the loop moves, so which half runs is settled in the preheader and settled for
    // good. There is no guard block and nothing carried round: the way into the loop is the choice.
    if windows.is_empty() {
        let term = func.terminator(plan.preheader).expect("a preheader ends in a jump");
        let args = copy::edge_args(func, term, plan.header);
        func.remove_inst(term);
        Builder::new(func, plan.preheader).br_if(ok, plan.header, &args, slow, &args);
        take(func, plan);
        return;
    }

    // The guard, which takes over the header's place: the preheader arrives here, the back edge
    // comes back to here, and the header is reached from here and nowhere else. Its first
    // parameters are offsets of its own, one per distinct step, because where the loop's own
    // pointers are is not something this pass has to find and a loop with several ways out may
    // have nothing that walks in step with what its checks are about.
    let word = Type::int(64);
    let types: Vec<Type> = func[plan.header].params.iter().map(|&param| func[param].ty).collect();
    let guard = func.create_block();
    let offsets: Vec<Value> = windows.iter().map(|_| func.append_param(guard, word)).collect();
    let carried: Vec<Value> = types.iter().map(|&ty| func.append_param(guard, ty)).collect();

    // Unsigned, because the window is a byte count and so is the offset, and because unsigned is
    // what the rule the removal rests on is written in.
    let mut build = Builder::new(func, guard);
    let mut inside: Option<Value> = None;
    for (&offset, window) in offsets.iter().zip(&windows) {
        let under = build.icmp(IntPred::Ule, offset, window.bound);
        inside = Some(match inside {
            None => under,
            Some(so_far) => build.binary(Opcode::And, so_far, under, Flags::NONE),
        });
    }
    let inside = inside.expect("a plan with a window has at least one of them");
    build.br_if(inside, plan.header, &carried, slow, &carried);

    // The way in, which tests whether the fast half may run at all and starts every offset at the
    // first access. A loop nothing fits in never reaches the guard.
    let term = func.terminator(plan.preheader).expect("a preheader ends in a jump to the header");
    let args = copy::edge_args(func, term, plan.header);
    func.remove_inst(term);
    let mut build = Builder::new(func, plan.preheader);
    let zero = build.iconst(word, 0);
    let mut into: Vec<Value> = offsets.iter().map(|_| zero).collect();
    into.extend_from_slice(&args);
    build.br_if(ok, guard, &into, slow, &args);

    // The way round, which walks each offset on by its step. The offsets are the guard's parameters
    // and the guard dominates every block in the fast half, so the latch may read them. `nuw`
    // rather than `nsw` because [`bounded`] held the window short of where this could wrap, and it
    // held it there in unsigned terms.
    let term = func.terminator(plan.latch).expect("a latch ends in a branch back to the header");
    let mut build = Builder::new(func, plan.latch);
    let mut made = Vec::new();
    let mut next = Vec::new();
    for (&offset, window) in offsets.iter().zip(&windows) {
        let by = build.iconst(word, window.step);
        made.push(by);
        let walked = build.binary(Opcode::Add, offset, by, Flags::NUW);
        made.push(walked);
        next.push(walked);
    }
    for value in made {
        let inst = inst_of(func, value);
        func.remove_inst(inst);
        func.insert_before(inst, term);
    }
    route(func, term, plan.header, guard, &next);
    take(func, plan);
}

/// Takes the checks the fast half does not need out of it.
///
/// The `cap_of` each one was reading is left where it is, for `dce` after this pass to take away,
/// which is the arrangement [`crate::hoist`] and [`crate::discharge`] are both in.
fn take(func: &mut Func, plan: &Plan) {
    for sweep in &plan.sweeps {
        func.remove_inst(sweep.check);
    }
}

/// Sends every edge this terminator has to `from` to `to` instead, with more arguments in front.
fn route(func: &mut Func, term: Inst, from: Block, to: Block, first: &[Value]) {
    for at in func.target_list(term).iter() {
        let call = func[at];
        if call.block != from {
            continue;
        }
        let mut args = first.to_vec();
        args.extend_from_slice(&func[call.args]);
        let args = func.push_values(&args);
        func.set_block_call(at, BlockCall { block: to, args });
    }
}

/// One offset the guard carries round the loop, and how far it may get.
struct Window {
    /// How far the address moves each time round, which is what the offset goes up by.
    step: i128,
    /// The highest offset an access may start at and still be inside what the extent covers.
    bound: Value,
}

/// How the two halves are chosen between, which depends on whether any address in the loop moves.
struct Choice {
    /// Whether every check in the loop fits at all, which the preheader tests before it enters the
    /// fast half. It is false for a dangling pointer or an object smaller than the thing being read
    /// out of it, and then the fast half runs no iterations and the check in the slow half reports
    /// the fault at the access rather than at the loop.
    ok: Value,
    /// One per distinct step, and empty when no address in the loop moves. A loop like that needs
    /// no guard block and nothing carried round it, because `ok` is the whole answer and it does
    /// not change while the loop runs.
    windows: Vec<Window>,
}

/// Builds what the preheader has to work out before either half can run.
///
/// One `cap_extent` per check and what it leaves room for, all of it in the preheader in front of
/// the jump into the loop. A builder appends to the end of a block, which in a block that already
/// has its terminator is after it, so everything is built first and then moved in front of the
/// terminator in the order it was built.
///
/// # Why the window is bytes and not iterations
///
/// This used to work out how many iterations a check allows, which is `(extent - reach) / step + 1`
/// clamped at zero, and count iterations against it. The claim that has to hold for the fast half
/// to be allowed to drop its checks was then that `i * step + reach <= extent` for every `i` below
/// that limit, which has a symbolic multiply and a symbolic divide in it at sixty four bits, and
/// z3 does not finish on it in two and a half minutes in any of three formulations. So the whole
/// transformation sat outside the rule table that `spec/safe-memory/07-check-elimination.md`
/// section 7.7 asks every elimination to be inside, and it sat there for a solver reason rather
/// than a design one, which is the worst kind.
///
/// Counting bytes instead of iterations takes the arithmetic out. The offset the loop is at moves
/// by `step` each time round exactly as the address does, the window is `extent - reach`, and the
/// claim is that an offset at or below that plus the reach is inside the extent. No multiply and no
/// divide, and it is the claim `swept.sym.i64` in `crates/rucc-opt/rules/safety.rules` already
/// makes, which [`windowed`] asks. The pass earns that rule's hypotheses rather than assuming them:
/// `ok` is where `extent` is held to be at least `reach`, so the window cannot have wrapped, and
/// [`bounded`] is where the offset is held short of where adding one more step would.
///
/// It is also less code. A loop with one step in it loses a divide from its preheader and carries
/// the same one value round that it did before.
///
/// # Why one window per step and not one per check
///
/// Two checks that walk by the same amount are at the same offset on every iteration, so they can
/// share the offset and the smaller of their two windows. On SQLite 127 of the 268 loops this
/// splits have one distinct step and five have two, so this is one value round the loop almost
/// always and two occasionally.
fn limited(func: &mut Func, plan: &Plan) -> Choice {
    let term = func.terminator(plan.preheader).expect("a preheader ends in a jump to the header");
    let mut made = Vec::new();
    let mut build = Builder::new(func, plan.preheader);

    let mut ok: Option<Value> = None;
    let mut windows: Vec<Window> = Vec::new();
    for sweep in &plan.sweeps {
        // How far the runtime is asked to look is how many bytes the loop reads from this address
        // on, and an address that does not move reads the same bytes however many times the loop
        // goes round, so the count the loop was going to run does not come into it.
        let around = if sweep.step == 0 { Around::Number(0) } else { plan.around };
        let (window, zero) = spare(&mut build, &mut made, sweep, around);
        // Every check has to fit for the fast half to be the one that runs, and this is where the
        // hypothesis the rule is asked under is earned: a window worked out from an extent smaller
        // than the reach is one that wrapped, and none of what follows would mean anything.
        let fits = build.icmp(IntPred::Sge, window, zero);
        made.push(fits);
        ok = Some(match ok {
            None => fits,
            Some(so_far) => {
                let both = build.binary(Opcode::And, so_far, fits, Flags::NONE);
                made.push(both);
                both
            }
        });
        if sweep.step == 0 {
            continue;
        }
        // Two checks that walk by the same amount are at the same offset on every iteration, so
        // they share the offset and the smaller of their two windows.
        match windows.iter().position(|held| held.step == sweep.step) {
            Some(at) => {
                let bound = windows[at].bound;
                let smaller = build.icmp(IntPred::Ult, window, bound);
                made.push(smaller);
                let least = build.select(smaller, window, bound);
                made.push(least);
                windows[at].bound = least;
            }
            None => windows.push(Window { step: sweep.step, bound: window }),
        }
    }
    let ok = ok.expect("a plan holds at least one check");

    for window in &mut windows {
        window.bound = bounded(&mut build, &mut made, window.step, window.bound);
    }

    for value in made {
        let inst = inst_of(func, value);
        func.remove_inst(inst);
        func.insert_before(inst, term);
    }
    Choice { ok, windows }
}

/// Holds a window short of where one more step would take the offset out of sixty four bits.
///
/// The offset goes up by the step every time round and is tested afterwards, so it reaches one step
/// past the window before the guard sends the loop to the other half. Nothing else here bounds the
/// window: `cap_extent` answers with no more than it was asked for, and what it was asked for is a
/// trip count times a step, which saturates rather than refusing. An offset that wrapped would come
/// back small, the guard would let it through, and the fast half would read past the end of the
/// object with nothing left in it to say so.
///
/// One comparison and one select in the preheader, and the value it clamps to is so far past any
/// object a program allocates that this never fires. It is here because the failure it stops is
/// silent.
fn bounded(build: &mut Builder<'_>, made: &mut Vec<Value>, step: i128, bound: Value) -> Value {
    let word = Type::int(64);
    let room = build.iconst(word, i128::from(i64::MAX) - step);
    made.push(room);
    let over = build.icmp(IntPred::Ugt, bound, room);
    made.push(over);
    let held = build.select(over, room, bound);
    made.push(held);
    held
}

/// Whether one offset at or below the window is one whose access is inside the extent.
///
/// This function decides nothing. It builds the term `swept.sym.i64` is written about and asks the
/// table, which is section 7.7's split: the pass established the window and carries the offset, and
/// whether an offset inside the window means an access inside the object is somebody's proof rather
/// than this file's opinion. It is the same rule [`crate::hoist`] asks about a loop whose extent the
/// program works out, and it is the same question, since a window is a hoisted check's far end under
/// another name.
///
/// Four of its five arguments are opaque. The address, the extent and the window are values the pass
/// does not have as numbers, and the offset is whichever iteration the reader cares about, which is
/// how one question comes to be about all of them. The rule's three hypotheses about that pair are
/// what [`limited`] and [`bounded`] earn.
fn windowed(reach: i128) -> bool {
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

/// How far the first access sits past the base, as a value, or `None` when it sits on it.
///
/// Wrapping arithmetic throughout, because this is the address the loop was going to compute
/// anyway. The flags a `nsw` would put on it would be a promise about the caller's index, and what
/// this is building is a question for the runtime rather than an address anything reads.
fn displacement(build: &mut Builder<'_>, made: &mut Vec<Value>, apart: Plain) -> Option<Value> {
    let word = Type::int(64);
    let mut sum = match apart.value.filter(|_| apart.scale != 0) {
        None => {
            return (apart.offset != 0).then(|| {
                let by = build.iconst(word, apart.offset);
                made.push(by);
                by
            });
        }
        Some(value) => value,
    };
    if apart.scale != 1 {
        let by = build.iconst(word, apart.scale);
        made.push(by);
        sum = build.binary(Opcode::Mul, sum, by, Flags::NONE);
        made.push(sum);
    }
    if apart.offset != 0 {
        let by = build.iconst(word, apart.offset);
        made.push(by);
        sum = build.binary(Opcode::Add, sum, by, Flags::NONE);
        made.push(sum);
    }
    Some(sum)
}

/// How many bytes past the first access belong to whatever owns it, and a zero to compare that with.
///
/// The question both callers rest on. `extent - reach` is negative when the first access does not
/// fit at all, zero when exactly one fits, and how much room there is for further ones otherwise.
fn spare(
    build: &mut Builder<'_>,
    made: &mut Vec<Value>,
    sweep: &Sweep,
    around: Around,
) -> (Value, Value) {
    let word = Type::int(64);
    let first = match displacement(build, made, sweep.apart) {
        None => sweep.base,
        Some(by) => {
            let args = build.func().push_values(&[sweep.base, by]);
            let sum = build.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR);
            made.push(sum);
            sum
        }
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
    (left, zero)
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
        walking(Some(TRIPS), Flags::NSW)
    }

    /// The same loop, with how many times it goes round handed in rather than written down.
    ///
    /// What this reaches is the other half of [`crate::trip::covered`], the one that builds the
    /// count out of something the loop does not change. It is worth its own test because that
    /// arithmetic promises not to wrap for hoisting and promises nothing for this pass, and the two
    /// callers now ask for different things from the same code.
    fn counting() -> (Interner, Func, Vec<Block>) {
        walking(None, Flags::NSW)
    }

    /// The same loop again, with an increment that promises nothing, so nobody counts it.
    ///
    /// What `-fwrapv` produces, and the shape a great deal of real code is in. Hoisting refuses it,
    /// because a count that rests on the counter not wrapping is not a count it may size a check
    /// with. This pass does not size anything with it, so it guesses.
    fn uncounted() -> (Interner, Func, Vec<Block>) {
        walking(Some(TRIPS), Flags::NONE)
    }

    /// The same loop, reading from an index the caller handed in rather than from zero.
    ///
    /// `a[start + i]`, whose first address is `a + 4 * start`: a pointer and a displacement, with a
    /// number for neither of them. This is the shape the pass used to give up on, and it is a
    /// common one, because a loop over part of an array is written this way and so is every walk
    /// that begins where the last one stopped. See #810.
    fn from_an_index() -> (Interner, Func, Vec<Block>) {
        let mut names = Interner::new();
        let params = [Type::PTR, Type::int(64)];
        let mut func = Func::new(names.intern("f"), Signature::new().with_params(&params));
        let entry = func.create_block();
        let head = func.create_block();
        let more = func.create_block();
        let done = func.create_block();
        let array = func.append_param(entry, Type::PTR);
        let start = func.append_param(entry, Type::int(64));
        let counter = func.append_param(head, Type::int(64));

        let zero = Builder::new(&mut func, entry).iconst(Type::int(64), 0);
        Builder::new(&mut func, entry).jump(head, &[zero]);

        let mut build = Builder::new(&mut func, head);
        let index = build.binary(Opcode::Add, counter, start, Flags::NSW);
        let by = build.iconst(Type::int(64), WIDTH);
        let scaled = build.binary(Opcode::Mul, index, by, Flags::NSW);
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
        let limit = build.iconst(Type::int(64), TRIPS);
        let again = build.icmp(IntPred::Slt, next, limit);
        build.br_if(again, head, &[next], done, &[]);
        Builder::new(&mut func, done).ret(&[]);
        (names, func, vec![entry, head, more, done])
    }

    /// Builds the loop, with the exit test against a number or against a second parameter.
    fn walking(times: Option<i128>, flags: Flags) -> (Interner, Func, Vec<Block>) {
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
        let next = build.binary(Opcode::Add, counter, one, flags);
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

    #[test]
    fn a_loop_whose_result_is_read_after_it_is_put_back_into_closed_form_first() {
        // Canonicalization runs a long way in front of this pass and `simplify-cfg` between the two
        // undoes some of what it did, which is why the loop here is canonicalized and then broken.
        // Both halves would define the value the code after the loop reads, so the pass repairs the
        // one loop it is about to copy rather than refusing it or running canonicalization again.
        let (mut names, mut func, blocks) = leaving();
        let mut an = crate::machine::fixtures::analyses();
        Canon.run(&mut func, &mut an, &mut Fuel::unlimited());

        let (head, done) = (blocks[1], blocks[3]);
        let read = func
            .insts(head)
            .find(|&inst| func[inst].opcode == Opcode::Load)
            .and_then(|inst| func[inst].results().next())
            .expect("the loop loads what it walks over");
        let term = func.terminator(done).expect("the block after the loop returns");
        let sum = Builder::new(&mut func, done).binary(Opcode::Add, read, read, Flags::NONE);
        let inst = super::inst_of(&func, sum);
        func.remove_inst(inst);
        func.insert_before(inst, term);
        an.clear();

        let stats = Split.run(&mut func, &mut an, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Optimized, super::CLOSED_HERE), 1);
        assert_eq!(stats.count(Kind::Optimized, SPLIT), 1);
        assert_eq!(func[done].params.len(), 1, "the block after the loop took the value in");
        assert_eq!(all(&func, Opcode::CheckBounds).len(), 1, "the fast half lost its check");
        sound(&func, &mut names);
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
    fn a_walk_that_starts_at_an_index_the_caller_handed_in_is_split() {
        // #810. The first address is `a + 4 * start` and the question has to be put about that
        // address rather than about the array, because an extent measured from the array covers
        // bytes in front of where the loop begins and would say the walk fits when it does not.
        let (mut names, mut func, blocks) = from_an_index();
        let stats = split_up(&mut func);
        assert_eq!(stats.count(Kind::Optimized, SPLIT), 1);
        assert_eq!(all(&func, Opcode::CheckBounds).len(), 1, "the fast half lost its check");

        let asked = all(&func, Opcode::CapExtent);
        assert_eq!(asked.len(), 1, "one question for the one check that was sized");
        let at = func[func[asked[0].1].args][1];
        let inst = super::inst_of(&func, at);
        assert_eq!(func[inst].opcode, Opcode::PtrAdd, "the question is asked about a displacement");
        assert_eq!(func[func[inst].args][0], func[blocks[0]].params[0], "off the array");
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
    fn a_check_whose_address_does_not_move_is_taken_too() {
        // One check on the array itself, every time round, alongside the one that walks. Hoisting
        // would rather have the still one, but this loop has a second way out, so hoisting will not
        // touch it and the check is still here to be taken. A step of zero is what carries it: the
        // access fits on the first iteration or on none of them, so it puts no limit on the loop.
        let (mut names, mut func, _) = standing(false);
        let stats = split_up(&mut func);
        assert_eq!(stats.count(Kind::Optimized, SPLIT), 1);
        assert_eq!(all(&func, Opcode::CapExtent).len(), 2, "both addresses were sized in front");
        assert_eq!(
            all(&func, Opcode::CheckBounds).len(),
            2,
            "the fast half lost both checks and the slow half kept both"
        );
        sound(&func, &mut names);
    }

    #[test]
    fn the_window_is_worked_out_without_dividing_by_anything() {
        // The reason it counts bytes rather than iterations. Iterations came out of a division by
        // the step, which is a step of zero on a check whose address does not move, and on x86 that
        // is a fault rather than a wrong number, so tamnd/rucc#818 was a program dying on the way
        // into a loop it was never going to fail in. It is also why the claim could not be a rule:
        // the divide and the multiply that went with it are what z3 would not finish on. The plan
        // here has one check of each kind, which is the shape fifty six of SQLite's two hundred and
        // sixty eight split loops have.
        let (mut names, mut func, _) = standing(false);
        split_up(&mut func);
        for opcode in [Opcode::SDiv, Opcode::UDiv] {
            assert!(all(&func, opcode).is_empty(), "{opcode:?} is left in the window arithmetic");
        }
        sound(&func, &mut names);
    }

    #[test]
    fn two_checks_that_walk_by_the_same_amount_share_one_offset() {
        // One value round the loop rather than one per check, which is what the common shape wants:
        // a loop that reads one array and writes another walks both by the same step, so they are
        // at the same offset on every iteration and the window is the smaller of the two.
        let (mut names, mut func, blocks) = twinned();
        let stats = split_up(&mut func);
        assert_eq!(stats.count(Kind::Optimized, SPLIT), 1);
        assert_eq!(all(&func, Opcode::CapExtent).len(), 2, "both addresses were sized in front");

        let head = blocks[1];
        let cfg = crate::Cfg::new(&func);
        let into = cfg.predecessors(head);
        assert_eq!(into.len(), 1, "the guard is the only way into the header now");
        let guard = into[0];
        assert_eq!(
            func[guard].params.len(),
            func[head].params.len() + 1,
            "one offset, not one per check"
        );
        sound(&func, &mut names);
    }

    /// The loop with a second walking check in it, on the element after the one it reads.
    ///
    /// Two checks that move by the same amount, which is what a loop that reads one array and writes
    /// another is, and what a loop that looks one element ahead is. The window arithmetic keeps one
    /// offset for the pair of them rather than one each, and this is the fixture that says so.
    fn twinned() -> (Interner, Func, Vec<Block>) {
        let (names, mut func, blocks) = leaving();
        let (entry, head) = (blocks[0], blocks[1]);
        let array = func[entry].params[0];
        let counter = func[head].params[0];
        let term = func.terminator(head).expect("the header branches");
        let mut build = Builder::new(&mut func, head);
        let by = build.iconst(Type::int(64), WIDTH);
        let scaled = build.binary(Opcode::Mul, counter, by, Flags::NSW);
        let ahead = build.binary(Opcode::Add, scaled, by, Flags::NSW);
        let args = build.func().push_values(&[array, ahead]);
        let pointer = build.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR);
        check(&mut build, pointer);
        let made: Vec<Inst> = func.insts(head).skip_while(|&inst| inst != term).skip(1).collect();
        for inst in made {
            func.remove_inst(inst);
            func.insert_before(inst, term);
        }
        (names, func, blocks)
    }

    #[test]
    fn a_loop_where_nothing_moves_picks_its_half_once_and_counts_nothing() {
        // Half the loops this takes on SQLite are like this, and they need none of the machinery the
        // rest of them do. Which half runs is decided by the answer to a question asked in the
        // preheader, the answer does not change while the loop runs, so the way into the loop is
        // where the two halves are chosen between and there is no counter and no guard block.
        let (mut names, mut func, blocks) = standing(true);
        let stats = split_up(&mut func);
        assert_eq!(stats.count(Kind::Optimized, SPLIT), 1);
        assert_eq!(all(&func, Opcode::CapExtent).len(), 1);
        assert_eq!(all(&func, Opcode::CheckBounds).len(), 1, "the fast half lost its check");

        let (entry, head) = (blocks[0], blocks[1]);
        let term = func.terminator(entry).expect("the preheader still ends in something");
        assert_eq!(func[term].opcode, Opcode::BrIf, "the way in is the choice");
        assert_eq!(func[head].params.len(), 1, "and the header took on no counter");
        sound(&func, &mut names);
    }

    /// The loop with a check on the array itself added to its header, every time round.
    ///
    /// Hoisting would rather have that check, and it takes the ones in loops it is willing to touch.
    /// This loop has a second way out, so hoisting will not touch it and the check is still here.
    /// `alone` takes the walking check away, which leaves a loop where nothing moves at all.
    fn standing(alone: bool) -> (Interner, Func, Vec<Block>) {
        let (names, mut func, blocks) = leaving();
        let (entry, head) = (blocks[0], blocks[1]);
        let array = func[entry].params[0];
        let walking = all(&func, Opcode::CheckBounds);
        let term = func.terminator(head).expect("the header branches");
        let mut build = Builder::new(&mut func, head);
        check(&mut build, array);
        let made: Vec<Inst> = func.insts(head).skip_while(|&inst| inst != term).skip(1).collect();
        for inst in made {
            func.remove_inst(inst);
            func.insert_before(inst, term);
        }
        if alone {
            for (_, inst) in walking {
                func.remove_inst(inst);
            }
        }
        (names, func, blocks)
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
    fn a_loop_nobody_counted_is_split_on_a_guess() {
        // The difference from hoisting in one test. Hoisting refuses this loop, because the count
        // is what it sizes the check it writes with and a count nobody settled is not one it may
        // write a check from. Nothing here rests on the count: it is spent on how far to ask the
        // runtime to look, and the runtime answers with a true count of the bytes that belong to the
        // object whatever it was asked for, so a guess is as safe as a proof and only less useful.
        let (mut names, mut func, _) = uncounted();
        let mut an = crate::machine::fixtures::analyses();
        Canon.run(&mut func, &mut an, &mut Fuel::unlimited());
        let refused = crate::hoist::Hoist.run(&mut func, &mut an, &mut Fuel::unlimited());
        assert!(!refused.changed(), "hoisting will not size a check from a count nobody settled");

        let stats = Split.run(&mut func, &mut an, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Optimized, SPLIT), 1);
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
