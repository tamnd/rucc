//! A write to a plane made once per iteration of a counted loop, as one write after the loop.
//!
//! `spec/safe-memory/07-check-elimination.md` section 7.6 asks for this: "A loop that stores a
//! scalar array element by element performs `n` `meta_init` bit-sets. Coalesced into one range
//! operation before or after the loop, by the same counted-loop analysis as section 7.4."
//! [`crate::coalesce`] is the straight line half of that and this is the loop half.
//!
//! ```c
//! for (int i = 0; i < n; i++)
//!     a[i] = i;
//! ```
//!
//! Under `-fsafety` every one of those stores gets a `meta_type` and a `meta_init` over its own
//! four bytes, which is two calls into the runtime per element for what is one range in each of two
//! planes. After this pass the body has the store and nothing else, and the block the loop leaves
//! to starts with one write to each plane over all `4 * n` bytes. The runtime does a range in one
//! call, so the loop stops paying for the planes per element.
//!
//! # After and never before
//!
//! The same direction [`crate::coalesce`] takes and for the same reason. A plane told about a
//! store late refuses a read that would have passed, which is a false positive, and a plane told
//! about a store early lets a read of bytes nothing has written pass, which is a hole. So the one
//! write goes at the exit, where every store it stands for has happened, and never in the
//! preheader, where none of them has.
//!
//! Late is only a false positive if something reads the plane in between, and that is the
//! condition the pass has. Every instruction in the loop has to be one a write to that plane may
//! be moved past, which is `crate::coalesce::crossed`'s list, so an init check in the loop keeps
//! every `meta_init` in it where it is. The write goes first in a block only the loop reaches,
//! which is the exit block or a block put on the edge to it, so nothing runs between the last store
//! and the write that stands for it but the rest of the last iteration.
//!
//! That leaves out the loop that most needs this, which is a copy: `to[i] = from[i]` reads the
//! init plane over `from` and writes it over `to`, and the pass cannot tell the two apart. That
//! needs a fact nothing here has, which is that two ranges do not overlap, and it is tracked on
//! tamnd/rucc#1617.
//!
//! # What the range is
//!
//! The write has to run on every iteration, which is the block it is in dominating the exit test,
//! and its address has to walk the loop by a constant step. Then iteration `k` writes `reach`
//! bytes at `first + k * step`, the loop goes round `count` times and writes `count + 1` times,
//! and the range after the loop is `count * step + reach` bytes from `first`. That is the extent
//! [`crate::hoist`] works out for a check, built by the same code, and it is the same number for
//! the same reason.
//!
//! What is different is which way it may be wrong. A check over more bytes than the loop reads is
//! a false positive, and a write over more bytes than the loop wrote is a hole, because it says
//! bytes are initialized that no store touched. So the range has to be exactly the union of the
//! writes, and it only is when there are no gaps between them. For the init plane that is a step
//! no wider than the write. For the type plane it is a step exactly as wide, because a `meta_type`
//! over a range says the node starts at the front of it and repeats, and two writes that overlap by
//! part of an element leave a different pattern depending on which went last.
//!
//! Each `meta_type` names a node. Every one in the loop has to name the same node and every one
//! of them has to come out, or none of them does, since a type write left in the loop and one
//! moved past it no longer happen in the order the program had them. `meta_init` writes one fact,
//! that the bytes hold something, and writes of it can go in any order, so each one is decided on
//! its own.
//!
//! # What the rule proves and what this file decides
//!
//! Section 7.7's split again. That the union of the writes is one range is an induction on the
//! iterations, and the step of it is `grown.i64` in `crates/rucc-opt/rules/safety.rules`: a range
//! from `first` that ends where the last write ended, and the next write `step` further on, cover
//! exactly the range `step` bytes longer. The first write is the base, and each iteration is one
//! use of the rule. What the pass does is check the rule's guard and build the range the rule says
//! the loop wrote.

use rucc_ir::{Block, Builder, Extra, Flags, Func, Inst, InstData, Opcode, Type};

use std::collections::HashMap;

use crate::canon::route;
use crate::cfg::Cfg;
use crate::coalesce::crossed;
use crate::discharge::{Question, yes};
use crate::dom::Dominators;
use crate::hoist::{anchored, fits, plain_enough, shaped, starting};
use crate::loops::{LoopId, Loops};
use crate::range::query::Ranges;
use crate::rules::safety;
use crate::scev::{Anchor, Evolution, Plain, Reading, Scev};
use crate::stats::Kind;
use crate::trip::{Around, counted, covered, inst_of};
use crate::{Analyses, Fuel, Pass, Preserved, Stats};

/// What is reported when an init write comes out of a loop.
const SUNK_INIT: &str =
    "init write taken out of a loop, one write after it covers every byte the loop wrote";

/// What is reported when a type write comes out of a loop.
const SUNK_TYPE: &str =
    "type write taken out of a loop, one write after it covers every byte the loop wrote";

/// What is reported when the pass ran out of fuel with a write it was about to move.
const NO_FUEL: &str = "plane write left in the loop, the pass ran out of fuel";

/// What is reported for a loop left by a computed `goto`, whose exit edge has nowhere to put a
/// block.
const COMPUTED_EXIT: &str = "plane write left in the loop, it is left by a computed goto";

/// What is reported for a loop whose shape is not one the range after it can be worked out for.
const NOT_SHAPED: &str = "plane write left in the loop, the loop has a call, another loop or \
                          another way out in it, or no block in front of it";

/// What is reported for a loop nobody can say how many times goes round.
const NOT_COUNTED: &str =
    "plane write left in the loop, how many times the loop runs is not worked out";

/// What is reported for a loop with something in it a plane write cannot be moved past.
const IN_THE_WAY: &str =
    "plane write left in the loop, something in it reads the plane or cannot be moved past";

/// What is reported for a type write in a loop that writes more than one type.
const TYPES_DIFFER: &str =
    "type write left in the loop, another type write in it names a different type";

/// What is reported for a type write in a loop where another type write stays.
const NOT_ALL_OF_THEM: &str =
    "type write left in the loop, another type write in it cannot be moved";

/// What is reported for a write an iteration can finish without reaching.
const NOT_EVERY_TIME: &str = "plane write left in the loop, an iteration can finish without it";

/// What is reported for a write whose width is not a number.
const NOT_A_NUMBER: &str = "plane write left in the loop, its width is not a constant";

/// What is reported for a write whose address does not walk up the loop by a constant.
const NOT_A_WALK: &str =
    "plane write left in the loop, its address does not walk up the loop by a constant";

/// What is reported for a write whose walk leaves bytes out.
const LEAVES_GAPS: &str =
    "plane write left in the loop, its step does not tile the bytes it writes";

/// What is reported when the range would be too wide, for the arithmetic or for the rule.
const TOO_WIDE: &str = "plane write left in the loop, the range after it is too wide for the rule";

/// The widest single write the rule takes, which is the rule file's own bound.
const LIMIT: i128 = 4_294_967_296;

/// The pass.
#[derive(Debug)]
pub struct Sink;

impl Pass for Sink {
    fn name(&self) -> &'static str {
        "plane-sink"
    }

    fn describe(&self) -> &'static str {
        "a plane write in a counted loop becomes one write over the whole range after it"
    }

    fn preserves(&self) -> Preserved {
        // Nothing, because an exit edge may get a block put on it. See [`Exit`].
        Preserved::NONE
    }

    fn run(&self, func: &mut Func, an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        if func.entry().is_none() {
            return stats;
        }
        let cfg = an.cfg(func);
        let doms = an.dominators(func);
        let loops = an.loops(func);
        if loops.count() == 0 {
            return stats;
        }

        // Worked out first and applied afterwards, as in `crate::hoist`, and for its reason. Each
        // plan adds to a block after a loop, maybe one it puts on the edge out first, and removes
        // one instruction from a body, and no plan mentions an instruction another one removes.
        let mut plans = Vec::new();
        {
            let mut scev = Scev::new(func, cfg, loops);
            let mut ranges = Ranges::new(func, cfg, doms);
            for id in loops.all() {
                sweep(func, cfg, doms, loops, &mut scev, &mut ranges, id, &mut plans, &mut stats);
            }
        }

        let mut split: HashMap<(Block, Block), Block> = HashMap::new();
        for plan in plans {
            if !fuel.take() {
                stats.missed(NO_FUEL);
                continue;
            }
            let exit = match plan.exit {
                Exit::Own(block) => block,
                Exit::Shared { from, to } => {
                    *split.entry((from, to)).or_insert_with(|| route(func, &[from], to))
                }
            };
            apply(func, &plan, exit);
            stats.optimized(if plan.kind == Opcode::MetaType { SUNK_TYPE } else { SUNK_INIT });
        }
        stats
    }
}

/// One write to take out of one loop, and what to write after it instead.
#[derive(Debug)]
struct Plan {
    /// Where the one write goes.
    exit: Exit,
    /// What the first iteration's address is computed from.
    base: Anchor,
    /// How far past that the first iteration writes.
    start: Plain,
    /// How many bytes from there the loop writes.
    span: Extent,
    /// Which plane, as the opcode that writes it.
    kind: Opcode,
    /// The node, for a type write, which the one after the loop names too.
    extra: Extra,
    /// The write being removed.
    write: Inst,
}

/// Where the one write after a loop goes, which is the front of a block only the loop reaches.
///
/// The block the loop leaves to usually is one, because `crate::canon` makes it one, and that is
/// [`Exit::Own`]. By the time this pass runs the passes after `canon` may have merged it into a
/// block the rest of the function reaches too, and a write at the front of that would run on a
/// path with no loop behind it, telling the plane about stores that never happened. So the edge
/// out of the loop gets a block of its own put on it, the same one `canon` would have put there,
/// and the write goes in that.
#[derive(Clone, Copy, Debug)]
enum Exit {
    Own(Block),
    Shared { from: Block, to: Block },
}

/// How many bytes the loop writes, as a number or as a recipe the exit block follows.
///
/// The recipe is the one [`crate::hoist`] hands a check, `max(count, 0) * step + reach`, and
/// [`fits`] is what says it cannot wrap.
#[derive(Clone, Copy, Debug)]
enum Extent {
    Bytes(u64),
    Computed { count: Plain, step: i128, reach: i128, reading: Reading },
}

/// Plans what can come out of one loop, and counts what cannot and why.
///
/// A loop with no plane write in it says nothing, because it is not a missed opportunity.
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
    let mine = |kind: Opcode| -> Vec<Inst> {
        loops
            .blocks(id)
            .iter()
            .filter(|&&block| loops.innermost(block) == Some(id))
            .flat_map(|&block| func.insts(block).collect::<Vec<Inst>>())
            .filter(|&inst| func[inst].opcode == kind)
            .collect()
    };
    let inits = mine(Opcode::MetaInit);
    let types = mine(Opcode::MetaType);
    let every = inits.len() + types.len();
    if every == 0 {
        return;
    }

    // What `shaped` and `counted` say is written for a check, so what is reported here is this
    // pass's own sentence rather than theirs.
    let Ok((preheader, guard)) = shaped(func, cfg, doms, loops, id) else {
        missed(stats, NOT_SHAPED, every);
        return;
    };
    // One exit is what `shaped` just established, so this is reading it rather than testing it.
    let [exit] = loops.exits(id) else { return };
    let exit = if *cfg.predecessors(exit.to) == [exit.from] {
        Exit::Own(exit.to)
    } else if func.terminator(exit.from).is_some_and(|last| func[last].opcode == Opcode::IndirectBr)
    {
        missed(stats, COMPUTED_EXIT, every);
        return;
    } else {
        Exit::Shared { from: exit.from, to: exit.to }
    };
    let Ok(around) = counted(scev, id) else {
        missed(stats, NOT_COUNTED, every);
        return;
    };
    let loop_ = Loop { preheader, guard, exit, around };

    for (kind, writes) in [(Opcode::MetaInit, inits), (Opcode::MetaType, types)] {
        if writes.is_empty() {
            continue;
        }
        if !clear(func, loops, id, kind) {
            missed(stats, IN_THE_WAY, writes.len());
            continue;
        }
        let planned: Vec<Result<Plan, &'static str>> = writes
            .iter()
            .map(|&write| planned(func, doms, scev, ranges, id, &loop_, write))
            .collect();
        if kind == Opcode::MetaType {
            let node = func[writes[0]].extra;
            if writes.iter().any(|&write| func[write].extra != node) {
                missed(stats, TYPES_DIFFER, writes.len());
                continue;
            }
            if let Some(why) = planned.iter().find_map(|plan| plan.as_ref().err()) {
                let kept = planned.iter().filter(|plan| plan.is_err()).count();
                missed(stats, why, kept);
                missed(stats, NOT_ALL_OF_THEM, writes.len() - kept);
                continue;
            }
        }
        for plan in planned {
            match plan {
                Ok(plan) => plans.push(plan),
                Err(why) => stats.missed(why),
            }
        }
    }
}

/// What one loop's writes are planned against, which is the same for all of them.
#[derive(Clone, Copy, Debug)]
struct Loop {
    preheader: Block,
    /// The block the loop is left from.
    guard: Block,
    /// Where the write after it goes.
    exit: Exit,
    around: Around,
}

/// Whether a write to this plane may be moved past everything else in the loop.
///
/// The writes to the plane itself are what is being moved, so they are passed over, and so is each
/// block's terminator, which is a branch because [`shaped`] refused anything else. Everything left
/// has to be on [`crossed`]'s list, which is a list of instructions that do not read the plane and
/// do not change what its bytes mean.
fn clear(func: &Func, loops: &Loops, id: LoopId, kind: Opcode) -> bool {
    loops.blocks(id).iter().all(|&block| {
        let last = func.terminator(block);
        func.insts(block).all(|inst| {
            let opcode = func[inst].opcode;
            opcode == kind || Some(inst) == last || crossed(opcode, kind)
        })
    })
}

/// The plan for one write, or why there is not one.
fn planned(
    func: &Func,
    doms: &Dominators,
    scev: &mut Scev<'_>,
    ranges: &mut Ranges<'_>,
    id: LoopId,
    loop_: &Loop,
    write: Inst,
) -> Result<Plan, &'static str> {
    // Every iteration writes, including the last one, which is the one the exit test ends. That is
    // what makes the count of writes one more than the count of times round.
    let block = func.block_of(write).ok_or(NOT_EVERY_TIME)?;
    if !doms.dominates(block, loop_.guard) {
        return Err(NOT_EVERY_TIME);
    }
    let kind = func[write].opcode;
    let [pointer, length] = func[func[write].args] else { return Err(NOT_A_NUMBER) };
    let (imm, width) = crate::fold::constant(func, length).ok_or(NOT_A_NUMBER)?;
    let reach = imm.signed(width);
    if !(1..=LIMIT).contains(&reach) {
        return Err(NOT_A_NUMBER);
    }

    let Evolution::Affine(chrec) = scev.evolution(id, pointer) else { return Err(NOT_A_WALK) };
    let Some(step) = chrec.step.as_number().filter(|&step| step > 0) else {
        return Err(NOT_A_WALK);
    };
    // No gaps, and for the type plane no overlaps either. The module comment says why the two
    // planes differ here.
    let tiles = if kind == Opcode::MetaType { step == reach } else { step <= reach };
    if !tiles {
        return Err(LEAVES_GAPS);
    }
    let (base, start) = anchored(chrec.base).ok_or(NOT_A_WALK)?;
    plain_enough(func, start).map_err(|_| NOT_A_WALK)?;
    if !grown(step, reach) {
        return Err(TOO_WIDE);
    }

    let span = match loop_.around {
        Around::Number(around) => {
            let span = around
                .checked_mul(step)
                .and_then(|far| far.checked_add(reach))
                .filter(|&span| span <= i128::from(i64::MAX))
                .ok_or(TOO_WIDE)?;
            Extent::Bytes(u64::try_from(span).map_err(|_| TOO_WIDE)?)
        }
        Around::Computed(count, reading) => {
            fits(func, ranges, loop_.preheader, count, step, reach, reading)
                .map_err(|_| TOO_WIDE)?;
            Extent::Computed { count, step, reach, reading }
        }
    };
    let extra = func[write].extra;
    Ok(Plan { exit: loop_.exit, base, start, span, kind, extra, write })
}

/// Whether a range that ends where the last write ended, and the next write, are one range.
///
/// The step of the induction the module comment describes. `span` is opaque because it is every
/// iteration's range at once, and `at` and `byte` are opaque because the claim is about every
/// address. What is left as numbers is the step and the width, which is what the guard is about.
fn grown(step: i128, reach: i128) -> bool {
    let mut question = Question::default();
    let at = question.opaque();
    let at = question.app("value.i64", &[at]);
    let span = question.opaque();
    let span = question.app("value.i64", &[span]);
    let step = question.number(step);
    let step = question.app("iconst.i64", &[step]);
    let reach = question.number(reach);
    let reach = question.app("iconst.i64", &[reach]);
    let byte = question.opaque();
    let byte = question.app("value.i64", &[byte]);
    let term = question.app("grown.i64", &[at, span, step, reach, byte]);
    match safety::TABLE.find(&question, term) {
        Some(found) => yes(&safety::TABLE, found.rule),
        None => false,
    }
}

/// Puts the one write at the front of the block after the loop and takes the one inside it out.
///
/// Built at the end of the block, since that is where a builder puts things, and then moved in
/// front of the first instruction in the order it was built. Everything it reads is defined outside
/// the loop, which dominates the block because the loop is the only way in.
fn apply(func: &mut Func, plan: &Plan, exit: Block) {
    let first = func.insts(exit).next().expect("a block ends in a terminator");

    let mut made = Vec::new();
    let mut build = Builder::new(func, exit);
    let base = match plan.base {
        Anchor::Value(value) => value,
        Anchor::Address(symbol) => {
            let extra = Extra::Symbol(symbol);
            let at =
                build.value(InstData { extra, ..InstData::new(Opcode::GlobalAddr) }, Type::PTR);
            made.push(at);
            at
        }
    };
    let at = starting(&mut build, &mut made, base, plan.start);
    let length = match plan.span {
        Extent::Bytes(bytes) => {
            let length = build.iconst(Type::int(64), i128::from(bytes));
            made.push(length);
            length
        }
        Extent::Computed { count, step, reach, reading } => {
            covered(&mut build, &mut made, count, step, reach, reading, Flags::NSW)
        }
    };
    let args = build.func().push_values(&[at, length]);
    let data = InstData { args, extra: plan.extra, ..InstData::new(plan.kind) };
    let one = build.inst(data, &[]);

    for value in made {
        let inst = inst_of(func, value);
        func.remove_inst(inst);
        func.insert_before(inst, first);
    }
    func.remove_inst(one);
    func.insert_before(one, first);
    func.remove_inst(plan.write);
}

/// Records the same reason for several writes at once, since a loop refused is every write in it.
fn missed(stats: &mut Stats, why: &'static str, writes: usize) {
    stats.record(Kind::Missed, why, u32::try_from(writes).unwrap_or(u32::MAX));
}

#[cfg(test)]
mod tests {
    use rucc_base::{Idx, Interner};
    use rucc_ir::{
        Block, Builder, Extra, Flags, Func, Inst, InstData, IntPred, Module, Opcode, Signature,
        Type, Value, verify_func,
    };
    use rucc_target::{TargetInfo, Triple};

    use super::{IN_THE_WAY, LEAVES_GAPS, SUNK_INIT, SUNK_TYPE, Sink, TYPES_DIFFER};
    use crate::canon::Canon;
    use crate::stats::Kind;
    use crate::{Fuel, Pass, Stats};

    /// One write in a loop body: which plane, how wide, and the node for a type write.
    #[derive(Clone, Copy)]
    struct Wrote {
        kind: Opcode,
        width: i128,
        node: u32,
    }

    const INIT: Wrote = Wrote { kind: Opcode::MetaInit, width: 4, node: 0 };
    const TYPE: Wrote = Wrote { kind: Opcode::MetaType, width: 4, node: 1 };

    /// A counted loop that tests at the bottom and writes the planes at `a + step * i`, which is the
    /// shape `a[i] = i` has by the time this pass runs. `trips` is a number, or the loop runs to a
    /// parameter when it is `None`, and `also` is anything else the body does, put in before the
    /// writes.
    fn filling(
        trips: Option<i128>,
        step: i128,
        writes: &[Wrote],
        also: Option<Opcode>,
    ) -> (Interner, Func, Vec<Block>) {
        let mut names = Interner::new();
        // Thirty two bits, which is an `int` counter, and which is what bounds how far a loop of a
        // length nobody knows can go.
        let ty = Type::int(32);
        let signature = Signature::new().with_params(&[Type::PTR, ty]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let head = func.create_block();
        let done = func.create_block();
        let array = func.append_param(entry, Type::PTR);
        let n = func.append_param(entry, ty);
        let counter = func.append_param(head, ty);
        let zero = Builder::new(&mut func, entry).iconst(ty, 0);
        Builder::new(&mut func, entry).jump(head, &[zero]);

        let mut build = Builder::new(&mut func, head);
        let wide = build.unary(Opcode::SExt, counter, Type::int(64));
        let by = build.iconst(Type::int(64), step);
        let scaled = build.binary(Opcode::Mul, wide, by, Flags::NSW);
        let args = build.func().push_values(&[array, scaled]);
        let pointer = build.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR);
        if let Some(opcode) = also {
            let width = build.iconst(Type::int(64), 4);
            let args = build.func().push_values(&[pointer, width]);
            build.inst(InstData { args, ..InstData::new(opcode) }, &[]);
        }
        for wrote in writes {
            plane(&mut build, pointer, *wrote);
        }
        let one = build.iconst(ty, 1);
        let next = build.binary(Opcode::Add, counter, one, Flags::NSW);
        let limit = match trips {
            Some(trips) => build.iconst(ty, trips),
            None => n,
        };
        let again = build.icmp(IntPred::Slt, next, limit);
        build.br_if(again, head, &[next], done, &[]);
        Builder::new(&mut func, done).ret(&[]);
        (names, func, vec![entry, head, done])
    }

    /// A write to one plane over `width` bytes at `pointer`.
    fn plane(build: &mut Builder<'_>, pointer: Value, wrote: Wrote) {
        let width = build.iconst(Type::int(64), wrote.width);
        let extra = match wrote.kind {
            Opcode::MetaType => Extra::Node(Idx::new(wrote.node)),
            _ => Extra::None,
        };
        let args = build.func().push_values(&[pointer, width]);
        build.inst(InstData { args, extra, ..InstData::new(wrote.kind) }, &[]);
    }

    /// Canonicalizes and then sinks, the shape the pipeline hands the pass.
    fn sunk(func: &mut Func) -> Stats {
        let mut an = crate::machine::fixtures::analyses();
        Canon.run(func, &mut an, &mut Fuel::unlimited());
        Sink.run(func, &mut an, &mut Fuel::unlimited())
    }

    /// Every write to that plane in a block.
    fn writes(func: &Func, block: Block, kind: Opcode) -> Vec<Inst> {
        func.insts(block).filter(|&inst| func[inst].opcode == kind).collect()
    }

    /// Every write to that plane anywhere in the function.
    fn anywhere(func: &Func, kind: Opcode) -> Vec<(Block, Inst)> {
        func.blocks()
            .flat_map(|block| writes(func, block, kind).into_iter().map(move |inst| (block, inst)))
            .collect()
    }

    /// The width a write carries, when it is a number.
    fn width(func: &Func, write: Inst) -> Option<i128> {
        let length = func[func[write].args][1];
        crate::fold::constant(func, length).map(|(imm, ty)| imm.signed(ty))
    }

    fn sound(func: &Func, names: &mut Interner) {
        let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().unwrap());
        let module = Module::new(names.intern("t.c"), &target);
        if let Err(errors) = verify_func(&module, func, names) {
            panic!("{errors:#?}");
        }
    }

    #[test]
    fn a_loop_that_fills_an_array_writes_the_planes_once_after_it() {
        let (_, mut func, blocks) = filling(Some(100), 4, &[TYPE, INIT], None);
        let stats = sunk(&mut func);
        assert_eq!(stats.count(Kind::Optimized, SUNK_INIT), 1);
        assert_eq!(stats.count(Kind::Optimized, SUNK_TYPE), 1);
        let (head, done) = (blocks[1], blocks[2]);
        for kind in [Opcode::MetaInit, Opcode::MetaType] {
            assert!(writes(&func, head, kind).is_empty(), "nothing is written in the body");
            let after = writes(&func, done, kind);
            assert_eq!(after.len(), 1);
            // A hundred writes of four bytes each, one per iteration.
            assert_eq!(width(&func, after[0]), Some(400));
        }
        let [(_, ty)] = anywhere(&func, Opcode::MetaType)[..] else { panic!("one type write") };
        assert_eq!(func[ty].extra, Extra::Node(Idx::new(1)), "and it names the same type");
    }

    #[test]
    fn what_the_pass_leaves_is_a_function_the_compiler_believes() {
        // Init writes only, since a type write names a node and the module here has none.
        let (mut names, mut func, _) = filling(Some(100), 4, &[INIT], None);
        sunk(&mut func);
        sound(&func, &mut names);
    }

    #[test]
    fn the_write_after_the_loop_goes_in_front_of_everything_else_there() {
        let (_, mut func, blocks) = filling(Some(8), 4, &[INIT], None);
        sunk(&mut func);
        let done = blocks[2];
        let last = func.terminator(done).unwrap();
        let write = writes(&func, done, Opcode::MetaInit)[0];
        let order: Vec<Inst> = func.insts(done).collect();
        let at = |inst| order.iter().position(|&it| it == inst).unwrap();
        assert!(at(write) < at(last));
    }

    #[test]
    fn a_loop_of_a_length_nobody_knows_gets_a_write_that_works_it_out() {
        let (mut names, mut func, blocks) = filling(None, 4, &[INIT], None);
        let stats = sunk(&mut func);
        assert_eq!(stats.count(Kind::Optimized, SUNK_INIT), 1);
        let after = writes(&func, blocks[2], Opcode::MetaInit);
        assert_eq!(after.len(), 1);
        assert_eq!(width(&func, after[0]), None, "the width is computed after the loop");
        sound(&func, &mut names);
    }

    #[test]
    fn an_init_check_in_the_loop_keeps_the_init_writes_in_it() {
        // `a[i] = a[i - 1]` has one of these, and a plane told late about the store is a later
        // iteration's check refusing a read of what an earlier one wrote.
        let (_, mut func, blocks) = filling(Some(100), 4, &[TYPE, INIT], Some(Opcode::CheckInit));
        let stats = sunk(&mut func);
        assert_eq!(stats.count(Kind::Missed, IN_THE_WAY), 1);
        assert_eq!(writes(&func, blocks[1], Opcode::MetaInit).len(), 1);
        // The type plane is not what that check reads, so its write still goes.
        assert_eq!(stats.count(Kind::Optimized, SUNK_TYPE), 1);
    }

    #[test]
    fn a_walk_that_steps_past_what_it_writes_stays() {
        // Every other four bytes. One write over the whole range would say the gaps hold
        // something, and nothing stored there.
        let (_, mut func, blocks) = filling(Some(100), 8, &[INIT], None);
        let stats = sunk(&mut func);
        assert_eq!(stats.count(Kind::Missed, LEAVES_GAPS), 1);
        assert_eq!(writes(&func, blocks[1], Opcode::MetaInit).len(), 1);
    }

    #[test]
    fn overlapping_init_writes_go_and_overlapping_type_writes_stay() {
        // One byte along each time and four bytes written, which leaves no gaps and so is fine for
        // the init plane. For the type plane it is a different pattern of where each element starts.
        let (_, mut func, blocks) = filling(Some(100), 1, &[TYPE, INIT], None);
        let stats = sunk(&mut func);
        assert_eq!(stats.count(Kind::Optimized, SUNK_INIT), 1);
        assert_eq!(stats.count(Kind::Missed, LEAVES_GAPS), 1);
        assert_eq!(writes(&func, blocks[1], Opcode::MetaType).len(), 1);
        let after = writes(&func, blocks[2], Opcode::MetaInit);
        assert_eq!(width(&func, after[0]), Some(99 + 4));
    }

    #[test]
    fn type_writes_of_two_types_stay() {
        let other = Wrote { node: 2, ..TYPE };
        let (_, mut func, blocks) = filling(Some(100), 4, &[TYPE, other], None);
        let stats = sunk(&mut func);
        assert_eq!(stats.count(Kind::Missed, TYPES_DIFFER), 2);
        assert_eq!(writes(&func, blocks[1], Opcode::MetaType).len(), 2);
    }

    #[test]
    fn a_loop_whose_exit_is_reached_some_other_way_gets_a_block_of_its_own() {
        // The block after the loop is also where the function goes when it skips the loop, so a
        // write there would tell the plane about stores that never happened. The write goes in a
        // block put on the edge out of the loop instead.
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::PTR, Type::int(8)]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let pre = func.create_block();
        let head = func.create_block();
        let done = func.create_block();
        let array = func.append_param(entry, Type::PTR);
        let skip = func.append_param(entry, Type::int(8));
        let counter = func.append_param(head, Type::int(64));
        let mut build = Builder::new(&mut func, entry);
        let zero = build.iconst(Type::int(8), 0);
        let go = build.icmp(IntPred::Eq, skip, zero);
        build.br_if(go, pre, &[], done, &[]);
        let zero = Builder::new(&mut func, pre).iconst(Type::int(64), 0);
        Builder::new(&mut func, pre).jump(head, &[zero]);
        let mut build = Builder::new(&mut func, head);
        let by = build.iconst(Type::int(64), 4);
        let scaled = build.binary(Opcode::Mul, counter, by, Flags::NSW);
        let args = build.func().push_values(&[array, scaled]);
        let pointer = build.value(InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR);
        plane(&mut build, pointer, INIT);
        let one = build.iconst(Type::int(64), 1);
        let next = build.binary(Opcode::Add, counter, one, Flags::NSW);
        let limit = build.iconst(Type::int(64), 100);
        let again = build.icmp(IntPred::Slt, next, limit);
        build.br_if(again, head, &[next], done, &[]);
        Builder::new(&mut func, done).ret(&[]);

        // No canonicalization, which would give the loop an exit of its own and so test something
        // else.
        let mut an = crate::machine::fixtures::analyses();
        let stats = Sink.run(&mut func, &mut an, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Optimized, SUNK_INIT), 1);
        assert!(writes(&func, head, Opcode::MetaInit).is_empty());
        assert!(writes(&func, done, Opcode::MetaInit).is_empty(), "the path that skips the loop");
        let [(block, write)] = anywhere(&func, Opcode::MetaInit)[..] else {
            panic!("one write after the loop");
        };
        assert!(![entry, pre, head, done].contains(&block), "a block of its own on the edge");
        assert_eq!(width(&func, write), Some(400));
        sound(&func, &mut names);
    }

    #[test]
    fn the_rule_takes_a_walk_with_no_gaps_and_nothing_else() {
        assert!(super::grown(4, 4));
        assert!(super::grown(1, 4));
        assert!(!super::grown(8, 4), "a step wider than the write leaves gaps");
        assert!(!super::grown(0, 4), "a step of nothing is not a walk");
    }
}
