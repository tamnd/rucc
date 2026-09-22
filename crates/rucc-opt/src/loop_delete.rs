//! Takes out a loop that comes back, working out what it left behind.
//!
//! Design: `spec/optimizer/17-dce.md` for what makes a thing removable and section 28.9 for the
//! closed form, the off by one in it, and why the two questions this pass asks want different
//! things from the trip count. This is tamnd/rucc#1631.
//!
//! [`crate::dce`] cannot do this and the reason is worth stating, because it looks at first like a
//! gap in that pass. An empty counted loop has a counter, an add, a compare and a branch, and
//! every one of them is used: the add feeds the compare, the compare feeds the branch, and the
//! branch feeds the block parameter the add reads. Nothing in it has a use count of zero, so a
//! pass driven by use counts correctly leaves all of it alone. The question that gets the loop out
//! is not asked of an instruction, it is asked of the loop: does anything outside read what it
//! computes, does it do anything to memory, and does it come back. Three yeses and the loop is a
//! way of spending time.
//!
//! # Which loops
//!
//! A preheader, one exit, and a bound rather than an estimate. The bound is what says the loop
//! terminates, which is the third question and the one a person is most likely to forget: a loop
//! that computes nothing and never comes back still cannot be taken out, because not coming back
//! is what it does. Section 7.5's distinction between a bound and an estimate is exactly this: an
//! estimate decides whether a transformation pays and a bound decides what the program does.
//!
//! The bound is read through [`crate::scev::Bound::comes_back`] rather than through the accessor
//! [`crate::unroll`] uses, and the difference is worth a sentence. Unrolling multiplies by the
//! count, so it needs the count to be the right number. Deleting only needs there to be a last
//! iteration, and `for (i = 0; i < n; i++)` has one whatever `n` turns out to be, so a count
//! worked out from a value the loop does not change is as good as a number here. It stops being
//! as good the moment anything reads what the loop left behind, which is why the two questions
//! are asked in that order and with different accessors.
//!
//! Every instruction inside has to be one whose not happening nothing can tell. That is the
//! predicate [`crate::dce`] already has, so it is read from there rather than written again, and
//! it means a plain load may be inside the loop and a `volatile` one may not. A call is allowed
//! when the purity analysis says it reads memory at most and comes back, which is the same rule
//! that lets a call whose result nothing reads go.
//!
//! # What the loop leaves behind
//!
//! A loop whose total somebody reads afterwards is a loop that hands something over, and most
//! loops worth writing are that kind. Sometimes what it hands over can be worked out without
//! running it. A value that goes up by the same amount every time round is `{base, +, step}` in
//! [`crate::scev`]'s terms, the loop is left on the iteration the exit test first fails, and that
//! iteration's number is the count, so what the loop leaves behind is `base + step * count`. The
//! preheader works that out in one go and hands it over instead, and then nothing outside reads
//! anything the loop computed and the loop goes.
//!
//! Handing it over is two different edits, because a value defined in the loop reaches the code
//! after it by two different roads. It may be an argument on the edge out, landing in a parameter
//! of the block the loop leaves to, which is the shape [`crate::canon`] puts things in. Or the
//! block after the loop may simply name it, which is legal wherever the definition dominates the
//! use and is what is actually there by the time this runs, since the block loop closed form put in
//! the way is one [`crate::simplify_cfg`] has every reason to fold away again. So both are looked
//! for, and a use of the second kind is rewritten where it stands.
//!
//! No overflow argument is needed for this and it is worth saying why, because the neighbouring
//! transformation in section 28.4 does need one. A value that steps by a fixed amount evolves in
//! its own type, which is to say modulo two to the width, and addition modulo two to the width is
//! associative, so adding `step` to `base` `count` times and working out `base + step * count` the
//! same way are the same number whatever either of them does to the top bit. Section 28.4's rewrite
//! is a different claim, that one comparison holds exactly where another does, and that one does
//! turn on whether the limit overflows. So the arithmetic written here carries neither `nsw` nor
//! `nuw`, and the promise the loop's own increment carried is not copied onto it, because that
//! promise is about the sequence and says nothing about this.
//!
//! What is written down is the whole expression rather than three instructions to be folded later,
//! because this pass is the last one in the pipeline and there is no later. `base` and `step` are
//! both [`crate::scev::Invariant`], which is `value * scale + offset` with the arithmetic on it
//! already, so `base + step * count` is worked out in that form first and only what is left of it
//! reaches the function. A loop adding one a million times leaves a constant behind and a loop
//! adding an invariant `n` a million times leaves one multiply.
//!
//! A count that is an expression rather than a number is written as `base + step * max(count, 0)`,
//! and the two things bolted onto it there are two assumptions paid for rather than believed. The
//! clamp is [`crate::scev::Assumption::Entered`]. A count that comes out negative is a loop whose
//! test failed the first time it ran, which is a loop that took its back edge no times and handed
//! over what one pass through its body left, and zero is the count that says exactly that. The
//! widening is the reading the exit test took, a sign extension for a signed test and a zero
//! extension for an unsigned one. Section 7.7 is the warning about getting that one wrong: a limit
//! past the middle of a thirty two bit type is a large number to an unsigned test and a negative
//! one to a signed test, so sign extending what an unsigned test compared would clamp to zero and
//! turn a loop over three billion elements into one that ran no times.
//!
//! The clamp is done in sixty four bits and the arithmetic in the value's own type, and the cut
//! between the two is exact rather than close enough. Multiplying modulo two to the width and then
//! cutting to a narrower width is the same number as cutting first and then multiplying, so a count
//! worked out wide and truncated is the count. Widening it instead is a zero extension, because the
//! clamp has already made it a number that is not negative.
//!
//! It is only done when it lets the loop go, which is a cost rule rather than a correctness one.
//! Writing the final value down where the loop stays behind costs a multiply in the preheader and
//! saves nothing, because the loop still carries the value round its own back edge and nothing in
//! rucc yet takes out a block parameter whose only reader is the argument it passes to itself.
//! [`crate::dce`]'s own notes call that out as a transformation worth having and a different one
//! from what it does. When there is one, this gate is the thing to reconsider.
//!
//! # What it does
//!
//! Works out in the preheader whatever the loop was going to leave behind, puts those values where
//! the loop's own were read, points the preheader at the block the loop left to with whatever the
//! edge out was already carrying from outside, and lets the sweep in [`crate::simplify_cfg`] take
//! the blocks nothing reaches. Every value named in any of it is asserted to dominate the preheader
//! rather than assumed to: a value defined outside the loop that reaches the exit test has to
//! dominate the preheader, and an assertion is cheaper than being wrong about why.

use std::collections::{HashMap, HashSet};

use rucc_ir::{Block, Builder, Extra, Func, Inst, InstData, IntPred, Opcode, Type, Value};

use crate::cfg::Cfg;
use crate::dom::Dominators;
use crate::loops::{LoopId, Loops};
use crate::purity::Facts;
use crate::scev::{Count, Invariant, Plain, Reading, Scev};
use crate::{Analyses, Fuel, Pass, Preserved, Stats};

const DELETED: &str = "loop taken out, it comes back and leaves nothing behind";
const WRITTEN: &str = "what the loop was going to leave behind worked out in front of it instead";
const NO_COUNT: &str = "loop left as it was, nothing here says it comes back";
const SHAPE: &str =
    "loop left as it was, it has no preheader or it leaves from more than one place";
const EFFECTS: &str = "loop left as it was, something in it does more than work out a value";
const NO_FORM: &str =
    "loop left as it was, what it leaves behind is not a thing this can work out in front of it";
const ENTRIES: &str = "loop left as it was, it is reached somewhere other than at its header";
const NO_FUEL: &str = "loop left as it was, the pass ran out of fuel";

/// Section 17's dead code elimination, asked about a loop rather than about an instruction.
#[derive(Debug)]
pub struct LoopDelete;

impl Pass for LoopDelete {
    fn name(&self) -> &'static str {
        "loop-delete"
    }

    fn describe(&self) -> &'static str {
        "a loop that comes back and leaves nothing behind is taken out"
    }

    fn preserves(&self) -> Preserved {
        // The loop goes, and its blocks with it.
        Preserved::NONE
    }

    fn run(&self, func: &mut Func, an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        if func.entry().is_none() {
            return stats;
        }
        let mut done: HashSet<Block> = HashSet::new();
        let mut say = true;
        while let Some(job) = plan(func, an, &done, &mut stats, say) {
            say = false;
            if !fuel.take() {
                stats.missed(NO_FUEL);
                break;
            }
            done.insert(job.header);
            for _ in 0..apply(func, &job) {
                stats.optimized(WRITTEN);
            }
            stats.optimized(DELETED);
            an.clear();
            crate::simplify_cfg::sweep(func, an, &mut stats);
        }
        an.clear();
        stats
    }
}

/// One loop to take out, worked out against the function as it stands.
#[derive(Debug)]
struct Job {
    /// The block the loop is entered at, which is what says which loop this was.
    header: Block,
    /// The one block outside the loop with an edge to the header.
    preheader: Block,
    /// The block outside the loop the one exit edge arrives at.
    exit: Block,
    /// The blocks the loop is made of, which is what says which uses are the ones outside it.
    inside: HashSet<Block>,
    /// What the edge out was going to carry, which the preheader carries instead.
    args: Vec<Value>,
    /// Every value the loop defines that anything outside it reads, and what it ends up holding.
    ends: Vec<(Value, Leaves)>,
}

/// What a value the loop defines holds by the time anything outside it looks.
///
/// Two shapes for the two shapes a count comes in, and what separates them is how much of the
/// answer is settled before anything reaches the function. A count that is a number settles all of
/// it, so what is left is one expression and often one constant. A count that is an expression
/// settles none of it and the preheader does the work.
#[derive(Clone, Copy, Debug)]
enum Leaves {
    /// `base + step * count`, worked out in full because the count is a number.
    Worked {
        /// The type it evolved in, which is the type the arithmetic is done in.
        ty: Type,
        /// The whole of it, as far as it goes without writing anything down.
        end: Invariant,
    },
    /// `base + step * max(count, 0)`, built in the preheader because the count is an expression.
    Built {
        /// The type it evolved in, which is the type the arithmetic is done in.
        ty: Type,
        /// What the value holds the first time anything outside could have looked.
        base: Invariant,
        /// How much it goes up by each time round.
        step: Invariant,
        /// How many times the back edge is taken, before the clamp the module notes describe.
        count: Plain,
        /// How the exit test read what the count is built on, which is the widening owed.
        reading: Reading,
    },
}

/// The innermost loop that can go, and what it would take.
///
/// One at a time, for the reason [`crate::unroll::plan`] takes one at a time: taking a loop out
/// invalidates the forest the next answer would be read out of. `say` is false after the first
/// round so that a loop this declines is declined once rather than once per round.
fn plan(
    func: &Func,
    an: &mut Analyses,
    done: &HashSet<Block>,
    stats: &mut Stats,
    say: bool,
) -> Option<Job> {
    let facts = an.purity();
    let cfg = an.cfg(func);
    let doms = an.dominators(func);
    let loops = an.loops(func);
    let mut scev = Scev::new(func, cfg, loops);
    let mut found: Option<(u32, Job)> = None;
    for id in loops.all() {
        if done.contains(&loops.header(id)) {
            continue;
        }
        match consider(func, cfg, doms, loops, facts, &mut scev, id) {
            Ok(job) => {
                let depth = loops.depth(id);
                if found.as_ref().is_none_or(|(had, _)| depth > *had) {
                    found = Some((depth, job));
                }
            }
            Err(why) if say => stats.missed(why),
            Err(_) => (),
        }
    }
    found.map(|(_, job)| job)
}

/// Whether this loop can go, and why not when it cannot.
fn consider(
    func: &Func,
    cfg: &Cfg,
    doms: &Dominators,
    loops: &Loops,
    facts: &Facts,
    scev: &mut Scev<'_>,
    id: LoopId,
) -> Result<Job, &'static str> {
    let header = loops.header(id);
    let preheader = loops.preheader(cfg, id).ok_or(SHAPE)?;
    let [only] = loops.exits(id) else {
        return Err(SHAPE);
    };

    let blocks = loops.blocks(id).to_vec();
    let inside: HashSet<Block> = blocks.iter().copied().collect();
    for &block in &blocks {
        // The same reducibility check unrolling makes. A block of the loop reached from outside
        // the loop is a region this has no right to reason about as one piece.
        if block != header && cfg.predecessors(block).iter().any(|at| !inside.contains(at)) {
            return Err(ENTRIES);
        }
        for inst in func.insts(block) {
            if func.is_terminator(inst) {
                // A terminator that leaves the function or goes somewhere worked out at run time
                // is not an edge the forest accounted for, so the one exit counted above is not
                // the only way out.
                if !matches!(func[inst].opcode, Opcode::Jump | Opcode::BrIf) {
                    return Err(EFFECTS);
                }
                continue;
            }
            if !crate::dce::removable(func, inst, facts) {
                return Err(EFFECTS);
            }
        }
    }
    // The bound is what says the loop comes back. A loop that computes nothing and runs forever
    // still does something, which is run forever. A count worked out from a value the loop does
    // not change says that as well as a number does: whatever that value is, the loop gets to it,
    // and nothing in here needs to know how many steps that took. What is not allowed is a loop
    // ending on `!=` whose counter may step past its limit, and that is the one assumption
    // [`crate::scev::Bound::comes_back`] holds back.
    let bound = scev.bound(id).ok_or(NO_COUNT)?;
    // Taken here rather than inside [`ending`] because it belongs to the exit test rather than to
    // any one value the loop hands over, so every one of them owes the same widening.
    let reading = bound.reading();
    let count = bound.comes_back().ok_or(NO_COUNT)?;

    let term = func.terminator(only.from).ok_or(SHAPE)?;
    let leaving = func.successors(term).find(|call| call.block == only.to).ok_or(SHAPE)?;
    let args = func[leaving.args].to_vec();

    let mut wanted = read_outside(func, &blocks, &inside);
    for &arg in &args {
        if loops.is_invariant(func, id, arg) {
            debug_assert!(
                doms.dominates(defined_in(func, arg), preheader),
                "a value outside the loop that reaches the exit test dominates the preheader"
            );
            continue;
        }
        if !wanted.contains(&arg) {
            wanted.push(arg);
        }
    }

    let mut ends = Vec::with_capacity(wanted.len());
    for value in wanted {
        let end = ending(func, scev, id, value, count, reading).ok_or(NO_FORM)?;
        debug_assert!(
            names(end).iter().all(|&on| doms.dominates(defined_in(func, on), preheader)),
            "a value the loop does not change is defined outside it and so dominates the preheader"
        );
        ends.push((value, end));
    }
    Ok(Job { header, preheader, exit: only.to, inside, args, ends })
}

/// Every value the loop defines that a block outside it names, in the order they turn up.
///
/// [`crate::unroll::escapes`] asks whether there is one of these and stops there, because a loop
/// with one is a loop it will not copy. Here they are the work rather than the reason to stop, so
/// the answer has to be which ones.
fn read_outside(func: &Func, blocks: &[Block], inside: &HashSet<Block>) -> Vec<Value> {
    let mut defined: HashSet<Value> = HashSet::new();
    for &block in blocks {
        defined.extend(func[block].params.iter().copied());
        for inst in func.insts(block) {
            defined.extend(func[inst].results());
        }
    }
    let mut found = Vec::new();
    for block in func.blocks() {
        if inside.contains(&block) {
            continue;
        }
        for inst in func.insts(block) {
            let reads = func[func[inst].args].iter().copied();
            let passes = func.successors(inst).flat_map(|call| func[call.args].to_vec());
            for value in reads.chain(passes) {
                if defined.contains(&value) && !found.contains(&value) {
                    found.push(value);
                }
            }
        }
    }
    found
}

/// What the loop leaves in a value it hands over, or `None` when that is not a thing to write down.
///
/// Every refusal here is asked before anything is written, so that a refusal is a refusal rather
/// than a preheader with half an expression in it. There is no undo and there should not need to
/// be.
fn ending(
    func: &Func,
    scev: &mut Scev<'_>,
    id: LoopId,
    value: Value,
    count: Count,
    reading: Reading,
) -> Option<Leaves> {
    let chrec = scev.evolution(id, value).chrec()?;
    // The arithmetic below is integer arithmetic in one lane. A chrec over anything else is not a
    // thing this knows how to write down, whatever the count turned out to be.
    if !chrec.ty.is_int() || chrec.ty.is_vector() {
        return None;
    }
    match count {
        Count::Exact(trips) => {
            let trips = i128::try_from(trips).ok()?;
            let all = chrec.step.times(Invariant::number(trips))?;
            let end = chrec.base.plus(all)?;
            writable(func, end, chrec.ty)?;
            Some(Leaves::Worked { ty: chrec.ty, end })
        }
        Count::Symbolic(count) => {
            writable(func, chrec.base, chrec.ty)?;
            writable(func, chrec.step, chrec.ty)?;
            let count = count.plain()?;
            // A count built on a value that is itself read through an extension carries a widening
            // of its own, and which of that one and the exit test's should be spent is not a
            // question with an answer here. Refused rather than guessed at, the same way
            // [`crate::trip::counted`] refuses it.
            if count.read.is_some() {
                return None;
            }
            // Room for the clamp to happen in. A count built on something already as wide as the
            // arithmetic that carries it has nowhere to be negative.
            if count.value.filter(|_| count.scale != 0).is_none_or(|on| func[on].ty.bits() > 64) {
                return None;
            }
            Some(Leaves::Built { ty: chrec.ty, base: chrec.base, step: chrec.step, count, reading })
        }
    }
}

/// Whether [`write`] can put an expression in front of the loop in the type given.
fn writable(func: &Func, part: Invariant, ty: Type) -> Option<Plain> {
    let plain = part.plain()?;
    if plain.read.is_some() {
        return None;
    }
    if plain.value.is_some_and(|named| func[named].ty != ty) {
        return None;
    }
    Some(plain)
}

/// Every value an expression is built on, which is what the dominance assertion is asked of.
fn names(leaves: Leaves) -> Vec<Value> {
    let on =
        |part: Invariant| part.plain().and_then(|plain| plain.value.filter(|_| plain.scale != 0));
    match leaves {
        Leaves::Worked { end, .. } => on(end).into_iter().collect(),
        Leaves::Built { base, step, count, .. } => {
            [on(base), on(step), count.value].into_iter().flatten().collect()
        }
    }
}

/// The block a value is defined in.
fn defined_in(func: &Func, value: Value) -> Block {
    match func[value].def {
        rucc_ir::Def::Result { inst, .. } => {
            func.block_of(inst).expect("a value in use is defined in a block")
        }
        rucc_ir::Def::Param { block, .. } => block,
    }
}

/// Points the preheader past the loop, working out on the way what the loop was going to leave.
///
/// Answers how many of those there were, which is what the report counts.
fn apply(func: &mut Func, job: &Job) -> usize {
    let term = func.terminator(job.preheader).expect("a preheader ends in a jump to the header");
    let mut instead: HashMap<Value, Value> = HashMap::new();
    // One clamp for the whole loop rather than one per value, since the count belongs to the loop
    // and the values differ only in what they do with it.
    let mut times: Option<Value> = None;
    for &(value, leaves) in &job.ends {
        let worked = match leaves {
            Leaves::Worked { ty, end } => write(func, term, ty, end),
            Leaves::Built { ty, base, step, count, reading } => {
                let all = match times {
                    Some(had) => had,
                    None => *times.insert(clamped(func, term, count, reading)),
                };
                built(func, term, ty, base, step, all)
            }
        };
        instead.insert(value, worked);
    }
    swap_in(func, job, &instead);
    let args: Vec<Value> =
        job.args.iter().map(|arg| instead.get(arg).copied().unwrap_or(*arg)).collect();
    func.remove_inst(term);
    Builder::new(func, job.preheader).jump(job.exit, &args);
    instead.len()
}

/// Puts the worked out values where the loop's own were read.
///
/// Only outside the loop, because inside it the loop's own values are still the right answer right
/// up until the blocks go. The preheader is outside and gets walked with the rest, which is
/// harmless and better than a special case: what was just written into it names nothing the loop
/// defines.
fn swap_in(func: &mut Func, job: &Job, instead: &HashMap<Value, Value>) {
    if instead.is_empty() {
        return;
    }
    let outside: Vec<Block> = func.blocks().filter(|at| !job.inside.contains(at)).collect();
    for block in outside {
        for inst in func.insts(block).collect::<Vec<_>>() {
            let mut lists = vec![func[inst].args];
            lists.extend(func.successors(inst).map(|call| call.args));
            for list in lists {
                func.rewrite(list, |value| instead.get(&value).copied().unwrap_or(value));
            }
        }
    }
}

/// Works an expression out in front of an instruction.
///
/// `value * scale + offset`, with the parts that are nothing left out, so a scale of one is no
/// multiply and an offset of zero is no add and an expression built on no value at all is one
/// constant. That is what makes a loop adding one a million times leave a number behind rather than
/// three instructions nothing is going to fold, this being the last pass there is.
fn write(func: &mut Func, before: Inst, ty: Type, end: Invariant) -> Value {
    let plain = end.plain().expect("consider refused anything this cannot write");
    let Some(value) = plain.value.filter(|_| plain.scale != 0) else {
        return crate::ivopts::number(func, before, ty, plain.offset);
    };
    let mut so_far = value;
    if plain.scale != 1 {
        let by = crate::ivopts::number(func, before, ty, plain.scale);
        so_far = arith(func, before, Opcode::Mul, so_far, by, ty);
    }
    if plain.offset != 0 {
        let by = crate::ivopts::number(func, before, ty, plain.offset);
        so_far = arith(func, before, Opcode::Add, so_far, by, ty);
    }
    so_far
}

/// How many times the back edge is taken, worked out in front of the loop and clamped at zero.
///
/// `max(read(value) * scale + offset, 0)`, in sixty four bits whatever the count's own type is, and
/// the module notes say what each of those two is paying for. The clamp is a `select` rather than a
/// branch because the whole of this has to be straight line code in a preheader, and nothing on any
/// of it promises anything about overflow, since the count came out of a subtraction the analysis
/// already reasoned about rather than out of anything written here.
fn clamped(func: &mut Func, before: Inst, count: Plain, reading: Reading) -> Value {
    let word = Type::int(64);
    let on = count.value.expect("a count that is an expression is built on a value");
    let mut wide = on;
    if func[on].ty.bits() < 64 {
        let widen = match reading {
            Reading::Signed => Opcode::SExt,
            Reading::Unsigned => Opcode::ZExt,
        };
        wide = cast(func, before, widen, wide, word);
    }
    if count.scale != 1 {
        let by = crate::ivopts::number(func, before, word, count.scale);
        wide = arith(func, before, Opcode::Mul, wide, by, word);
    }
    if count.offset != 0 {
        let by = crate::ivopts::number(func, before, word, count.offset);
        wide = arith(func, before, Opcode::Add, wide, by, word);
    }
    let none = crate::ivopts::number(func, before, word, 0);
    let args = func.push_values(&[wide, none]);
    let test =
        InstData { args, extra: Extra::IntPred(IntPred::Sgt), ..InstData::new(Opcode::ICmp) };
    let entered = made(func, before, test, word.with_lane(Type::I1));
    let args = func.push_values(&[entered, wide, none]);
    made(func, before, InstData { args, ..InstData::new(Opcode::Select) }, word)
}

/// `base + step * times`, worked out in front of the loop in the type the value evolved in.
///
/// The trivial parts are left out where the numbers make them trivial, for the reason [`write`]
/// leaves them out: nothing after this pass folds a multiply by one, so a loop counting by ones
/// would otherwise leave one in every preheader.
fn built(
    func: &mut Func,
    before: Inst,
    ty: Type,
    base: Invariant,
    step: Invariant,
    times: Value,
) -> Value {
    if step.as_number() == Some(0) {
        return write(func, before, ty, base);
    }
    let narrow = resize(func, before, times, ty);
    let mut so_far = narrow;
    if step.as_number() != Some(1) {
        let by = write(func, before, ty, step);
        so_far = arith(func, before, Opcode::Mul, by, narrow, ty);
    }
    if base.as_number() != Some(0) {
        let from = write(func, before, ty, base);
        so_far = arith(func, before, Opcode::Add, from, so_far, ty);
    }
    so_far
}

/// The clamped count in the type the arithmetic is done in.
///
/// A truncation where that type is narrower, which loses nothing that matters: cutting a product
/// modulo two to the width and multiplying a cut are the same number. A zero extension where it is
/// wider, which is exact because the clamp has already made the count a number that is not
/// negative. Neither where the widths agree.
fn resize(func: &mut Func, before: Inst, times: Value, ty: Type) -> Value {
    let had = func[times].ty.bits();
    if had == ty.bits() {
        return times;
    }
    let either = if ty.bits() < had { Opcode::Trunc } else { Opcode::ZExt };
    cast(func, before, either, times, ty)
}

/// One widening or narrowing, worked out in front of another instruction.
fn cast(func: &mut Func, before: Inst, opcode: Opcode, arg: Value, ty: Type) -> Value {
    let args = func.push_values(&[arg]);
    made(func, before, InstData { args, ..InstData::new(opcode) }, ty)
}

/// One arithmetic instruction, worked out in front of another one and promising nothing.
///
/// Neither `nsw` nor `nuw`, which is the point rather than an omission. The module notes say why:
/// what the loop did is the same arithmetic modulo two to the width as many times as it ran, and
/// `base + step * count` worked out the same way is the same number. A flag the loop's own
/// increment carried is a fact about that sequence, and putting it here would be inventing one.
fn arith(
    func: &mut Func,
    before: Inst,
    opcode: Opcode,
    left: Value,
    right: Value,
    ty: Type,
) -> Value {
    let args = func.push_values(&[left, right]);
    made(func, before, InstData { args, ..InstData::new(opcode) }, ty)
}

/// One instruction with one result, put in front of another one and given its source location.
fn made(func: &mut Func, before: Inst, data: InstData, ty: Type) -> Value {
    let span = func.span(before);
    let inst = func.create_inst(data, &[ty], span);
    func.insert_before(inst, before);
    func[inst].first_result.expect("one result was asked for")
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::{
        Block, Builder, Def, Flags, Func, IntPred, MemInfo, MemOrder, Module, Opcode, Restrict,
        Signature, Type, Value, verify_func,
    };
    use rucc_target::{TargetInfo, Triple};

    use super::{DELETED, EFFECTS, LoopDelete, NO_COUNT, NO_FORM, NO_FUEL, WRITTEN};
    use crate::stats::Kind;
    use crate::{Fuel, Pass, Stats};

    /// Runs the pass over the function as it stands.
    fn delete(func: &mut Func, fuel: &mut Fuel) -> Stats {
        LoopDelete.run(func, &mut crate::machine::fixtures::analyses(), fuel)
    }

    /// Insists the function is one the rest of the compiler may believe.
    ///
    /// Pointing a block at a different successor is the edit that hands a block the wrong number
    /// of arguments and strands a definition its uses still name, so this is where most of the
    /// strength of these tests is.
    fn sound(func: &Func, names: &mut Interner) {
        let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().unwrap());
        let module = Module::new(names.intern("t.c"), &target);
        if let Err(errors) = verify_func(&module, func, names) {
            panic!("{errors:#?}");
        }
    }

    /// How many instructions of that opcode the whole function holds.
    fn tally(func: &Func, opcode: Opcode) -> usize {
        func.blocks()
            .flat_map(|block| func.insts(block))
            .filter(|&inst| func[inst].opcode == opcode)
            .count()
    }

    /// The one value now handed to the block the loop used to leave to.
    fn handed_value(func: &Func, done: &Block) -> Option<Value> {
        let cfg = crate::cfg::Cfg::new(func);
        let [only] = cfg.predecessors(*done) else {
            return None;
        };
        let term = func.terminator(*only)?;
        let call = func.successors(term).find(|call| call.block == *done)?;
        let args = func[call.args].to_vec();
        let [arg] = args[..] else {
            return None;
        };
        Some(arg)
    }

    /// What the value handed over is a multiple of, when it is a multiple of something.
    fn handed(func: &Func, done: &Block) -> Option<i128> {
        let value = handed_value(func, done)?;
        let Def::Result { inst, .. } = func[value].def else {
            return None;
        };
        if func[inst].opcode != Opcode::Mul {
            return None;
        }
        let args = func[func[inst].args].to_vec();
        let (imm, ty) = crate::fold::constant(func, args[1])?;
        Some(imm.signed(ty))
    }

    /// How many loops are left.
    fn loops(func: &Func) -> usize {
        let cfg = crate::cfg::Cfg::new(func);
        let doms = crate::dom::Dominators::new(&cfg);
        crate::loops::Loops::new(&cfg, &doms).count()
    }

    /// A four byte write with nothing said about what it aliases.
    fn plain() -> MemInfo {
        MemInfo {
            size: 4,
            align: 4,
            order: MemOrder::NotAtomic,
            tbaa: None,
            owns: 0,
            restrict: Restrict::NONE,
        }
    }

    /// What the limit of the exit test is.
    #[derive(Clone, Copy)]
    enum Limit {
        /// A number written in the program.
        Number(i128),
        /// A value the function was handed, which the loop does not change.
        Given,
        /// The same value, waited for with `!=` rather than counted up to with an ordering.
        Landing,
    }

    /// What the loop does, which is the whole of what decides whether it can go.
    #[derive(Clone, Copy, PartialEq)]
    enum What {
        /// Adds up a number nothing ever reads.
        Nothing,
        /// Writes each running total to the pointer it was handed.
        Writes,
        /// Hands the block it leaves to a total that went up by the same amount every time.
        HandsOut,
        /// Hands out a total that went up by one every time, so there is no multiply to write.
        HandsOne,
        /// Hands out a total that went up by a different amount every time round.
        HandsSquare,
        /// Leaves its total to be read after the loop by a road other than the edge out.
        ReadAfter,
    }

    impl What {
        /// Whether the total goes out on the edge the loop leaves by.
        fn hands_out(self) -> bool {
            matches!(self, What::HandsOut | What::HandsOne | What::HandsSquare)
        }
    }

    struct Shape {
        names: Interner,
        func: Func,
        entry: Block,
        done: Block,
    }

    /// A counted loop in the shape `crate::canon` and `crate::header_copy` leave a `for` in.
    ///
    /// ```text
    /// entry(p, n): jump head(0, 0)
    /// head(i, sum): jump body(i, sum)
    /// body(c, r): total = r + n; next = c + 1; test = next < limit
    ///             br test -> head(next, total), done()
    /// done: ret
    /// ```
    ///
    /// Two blocks in the loop rather than one, so that taking it out has more than one block to get
    /// rid of, and a running total carried round, so that there is something inside worth asking
    /// whether anybody reads. The total goes up by `n` each time round, which is the shape of
    /// `total += seed` in the issue: a value that goes up by the same amount every time, where the
    /// amount is not a number anything here knows.
    fn shaped(limit: Limit, what: What) -> Shape {
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::PTR, Type::int(32)]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let head = func.create_block();
        let body = func.create_block();
        let done = func.create_block();
        let place = func.append_param(entry, Type::PTR);
        let given = func.append_param(entry, Type::int(32));
        let i = func.append_param(head, Type::int(32));
        let sum = func.append_param(head, Type::int(32));
        let carried = func.append_param(body, Type::int(32));
        let running = func.append_param(body, Type::int(32));
        if what.hands_out() {
            func.append_param(done, Type::int(32));
        }

        let mut build = Builder::new(&mut func, entry);
        let zero = build.iconst(Type::int(32), 0);
        build.jump(head, &[zero, zero]);
        Builder::new(&mut func, head).jump(body, &[i, sum]);

        let mut build = Builder::new(&mut func, body);
        let one = build.iconst(Type::int(32), 1);
        let by = match what {
            What::HandsOne => one,
            What::HandsSquare => carried,
            _ => given,
        };
        let total = build.binary(Opcode::Add, running, by, Flags::NSW);
        if what == What::Writes {
            build.store(total, place, plain(), Flags::NONE);
        }
        let next = build.binary(Opcode::Add, carried, one, Flags::NSW);
        let stop = match limit {
            Limit::Number(n) => build.iconst(Type::int(32), n),
            Limit::Given | Limit::Landing => given,
        };
        let pred = match limit {
            Limit::Landing => IntPred::Ne,
            _ => IntPred::Slt,
        };
        let test = build.icmp(pred, next, stop);
        let out: Vec<Value> = if what.hands_out() { vec![total] } else { Vec::new() };
        build.br_if(test, head, &[next, total], done, &out);
        let mut build = Builder::new(&mut func, done);
        if what == What::ReadAfter {
            build.store(total, place, plain(), Flags::NONE);
        }
        build.ret(&[]);
        Shape { names, func, entry, done }
    }

    #[test]
    fn a_loop_that_leaves_nothing_behind_is_taken_out() {
        let mut it = shaped(Limit::Number(1000), What::Nothing);
        let stats = delete(&mut it.func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Optimized, DELETED), 1);
        assert_eq!(loops(&it.func), 0);
        assert_eq!(tally(&it.func, Opcode::Add), 0, "the counter and the total go with it");
        assert_eq!(tally(&it.func, Opcode::BrIf), 0);
        sound(&it.func, &mut it.names);
    }

    #[test]
    fn the_blocks_it_took_out_are_swept_rather_than_left_unreachable() {
        let mut it = shaped(Limit::Number(1000), What::Nothing);
        delete(&mut it.func, &mut Fuel::unlimited());
        let left: Vec<Block> = it.func.blocks().collect();
        assert_eq!(left, vec![it.entry, it.done], "the header and the body are gone");
        sound(&it.func, &mut it.names);
    }

    /// A loop counting up to a value nothing here knows still has a last iteration.
    ///
    /// The bound comes back [`crate::scev::Count::Symbolic`] with two assumptions on it rather
    /// than one. The overflow one the front end already promised.
    /// [`crate::scev::Assumption::Entered`] it did not, and what that one says is whether the
    /// count is the distance to the limit or zero. Both of those are numbers of times a loop goes
    /// round, so the question this pass asks, which is whether there is a last time, has been
    /// answered whichever of them it turns out to be. Nothing outside reads what this loop
    /// computes, so the count is never multiplied by and the assumption is never spent.
    ///
    /// This is the `for (i = 0; i < n; i++)` of tamnd/rucc#1631 and of the corpus rows the report
    /// had rucc losing on. Reading the bound through [`crate::scev::Bound::comes_back`] is what
    /// makes it the answer.
    #[test]
    fn a_loop_counting_up_to_a_value_handed_in_is_taken_out() {
        let mut it = shaped(Limit::Given, What::Nothing);
        let stats = delete(&mut it.func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Optimized, DELETED), 1);
        assert_eq!(loops(&it.func), 0);
        assert_eq!(tally(&it.func, Opcode::Add), 0, "the counter and the total go with it");
        sound(&it.func, &mut it.names);
    }

    /// A loop that may step over the value it is waiting for is a loop that may not come back.
    ///
    /// `!=` ends a loop on the one iteration where the counter is the limit, so a counter that
    /// starts past the limit, or that steps over it, goes round until it wraps. That is
    /// [`crate::scev::Assumption::Approaching`], the one
    /// [`crate::scev::Bound::comes_back`] holds back, and it is held back because document 17.2
    /// says rucc does not take out a loop that might not end. The step here is one and the loop
    /// would in fact arrive, which is the point: the pass refuses on what it has been shown rather
    /// than on what happens to be true.
    #[test]
    fn a_loop_that_may_step_over_the_value_it_waits_for_is_left_alone() {
        let mut it = shaped(Limit::Landing, What::Nothing);
        let stats = delete(&mut it.func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Optimized, DELETED), 0);
        assert_eq!(stats.count(Kind::Missed, NO_COUNT), 1);
        assert_eq!(loops(&it.func), 1);
        sound(&it.func, &mut it.names);
    }

    #[test]
    fn a_loop_that_writes_to_memory_is_left_alone() {
        let mut it = shaped(Limit::Number(1000), What::Writes);
        let stats = delete(&mut it.func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Optimized, DELETED), 0);
        assert_eq!(stats.count(Kind::Missed, EFFECTS), 1);
        assert_eq!(loops(&it.func), 1);
        assert_eq!(tally(&it.func, Opcode::Store), 1);
        sound(&it.func, &mut it.names);
    }

    /// A total read after the loop without going through a parameter of the block that reads it.
    ///
    /// This is the shape that is actually there by the time the pass runs, rather than the loop
    /// closed form one, because the block loop closed form put in the way is one `simplify-cfg`
    /// folds back out. The value is defined in the loop and named in a block the loop dominates,
    /// which is legal and is what a `for` loop adding to a total and printing it afterwards comes
    /// out as. The worked out total goes where the loop's own was read.
    #[test]
    fn a_total_read_after_the_loop_by_another_road_is_worked_out_too() {
        let mut it = shaped(Limit::Number(1000), What::ReadAfter);
        let stats = delete(&mut it.func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Optimized, DELETED), 1);
        assert_eq!(stats.count(Kind::Optimized, WRITTEN), 1);
        assert_eq!(loops(&it.func), 0);
        assert_eq!(tally(&it.func, Opcode::Mul), 1, "one multiply, by the trip count");
        assert_eq!(tally(&it.func, Opcode::Store), 1, "and the store that read it is still there");
        sound(&it.func, &mut it.names);
    }

    /// The total the loop was going to hand over, worked out without running the loop.
    ///
    /// The loop adds `n` to a running total a thousand times, so the total it hands over is
    /// `n * 1000`, and what is left of the function is that multiply. The count is 999 rather than
    /// 1000 and the base is `n` rather than zero, because the exit edge is taken on the iteration
    /// the test first fails and the total has already been added to by then: `n + n * 999`. Doing
    /// the arithmetic on [`crate::scev::Invariant`] before writing anything down is what turns that
    /// into one instruction rather than three nothing would fold, this being the last pass run.
    #[test]
    fn a_total_the_loop_hands_over_is_worked_out_in_front_of_it() {
        let mut it = shaped(Limit::Number(1000), What::HandsOut);
        let stats = delete(&mut it.func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Optimized, DELETED), 1);
        assert_eq!(stats.count(Kind::Optimized, WRITTEN), 1);
        assert_eq!(loops(&it.func), 0);
        assert_eq!(tally(&it.func, Opcode::Mul), 1, "one multiply, by the trip count");
        assert_eq!(tally(&it.func, Opcode::Add), 0, "and nothing to add to it");
        assert_eq!(handed(&it.func, &it.done), Some(1000), "n times a thousand");
        sound(&it.func, &mut it.names);
    }

    /// The same thing where the amount is one, which leaves a number rather than a multiply.
    #[test]
    fn a_total_that_went_up_by_one_is_left_as_a_number() {
        let mut it = shaped(Limit::Number(1000), What::HandsOne);
        let stats = delete(&mut it.func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Optimized, DELETED), 1);
        assert_eq!(stats.count(Kind::Optimized, WRITTEN), 1);
        assert_eq!(tally(&it.func, Opcode::Mul), 0);
        assert_eq!(tally(&it.func, Opcode::Add), 0);
        let handed = handed_value(&it.func, &it.done).expect("the total is handed over");
        let (imm, ty) = crate::fold::constant(&it.func, handed).expect("and it is a number");
        assert_eq!(imm.signed(ty), 1000);
        sound(&it.func, &mut it.names);
    }

    /// The total handed over by a loop counting up to a value nothing here knows.
    ///
    /// The loop adds `n` to a running total until the counter arrives at `n`, so what it hands over
    /// is `n + n * max(n - 1, 0)` and every part of that is written down in front of where the loop
    /// was. The `select` is the clamp. [`crate::scev::Assumption::Entered`] says the count is
    /// either the distance to the limit or zero, and taking the larger of the two is that
    /// assumption paid for rather than leaned on. The sign extension in front of it is the exit
    /// test's own reading of the value, which is `<` on signed values here.
    #[test]
    fn a_total_from_a_loop_counting_up_to_a_value_handed_in_is_worked_out() {
        let mut it = shaped(Limit::Given, What::HandsOut);
        let stats = delete(&mut it.func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Optimized, DELETED), 1);
        assert_eq!(stats.count(Kind::Optimized, WRITTEN), 1);
        assert_eq!(loops(&it.func), 0);
        assert_eq!(tally(&it.func, Opcode::Select), 1, "the clamp at zero");
        assert_eq!(tally(&it.func, Opcode::ICmp), 1, "and the test it picks on");
        let read = "the count read the way the exit test read it";
        assert_eq!(tally(&it.func, Opcode::SExt), 1, "{read}");
        assert_eq!(tally(&it.func, Opcode::Trunc), 1, "and cut back to what the total is added in");
        assert_eq!(tally(&it.func, Opcode::Mul), 1, "one multiply, by the count");
        assert_eq!(tally(&it.func, Opcode::Add), 2, "the off by one on the count and the base");
        sound(&it.func, &mut it.names);
    }

    /// The same total read after the loop rather than handed over, which is the corpus row.
    ///
    /// `loop-deletion.u32.1000000.unknown.read-back` is this shape, a bound nothing can see and a
    /// total read once the loop is done. It is the row rucc ran a million times and gcc 16 ran no
    /// times at all. The store stays and what it stores is worked out where the loop used to be.
    #[test]
    fn a_total_read_after_a_loop_counting_up_to_a_value_handed_in_is_worked_out() {
        let mut it = shaped(Limit::Given, What::ReadAfter);
        let stats = delete(&mut it.func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Optimized, DELETED), 1);
        assert_eq!(stats.count(Kind::Optimized, WRITTEN), 1);
        assert_eq!(loops(&it.func), 0);
        assert_eq!(tally(&it.func, Opcode::Select), 1, "the clamp at zero");
        assert_eq!(tally(&it.func, Opcode::Store), 1, "and the store that read it is still there");
        sound(&it.func, &mut it.names);
    }

    /// A total that went up by a different amount every time is not a thing to write down.
    ///
    /// Here the total goes up by the counter rather than by a fixed amount, so what it holds after
    /// `k` times round is a square number and [`crate::scev`] rightly has no affine form for it.
    /// The loop ends and does nothing to memory, so the only thing keeping it is the total, and the
    /// pass says so rather than guessing at it.
    #[test]
    fn a_total_that_went_up_by_a_different_amount_each_time_leaves_the_loop_alone() {
        let mut it = shaped(Limit::Number(1000), What::HandsSquare);
        let stats = delete(&mut it.func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Optimized, DELETED), 0);
        assert_eq!(stats.count(Kind::Missed, NO_FORM), 1);
        assert_eq!(loops(&it.func), 1);
        sound(&it.func, &mut it.names);
    }

    /// A loop that comes back but not after a number of steps anything here can work out.
    ///
    /// ```text
    /// entry(p, n): jump head(1)
    /// head(c): next = c + c; test = next < 1000; br test -> head(next), done()
    /// done: ret
    /// ```
    ///
    /// The counter doubles, so it is not a value that goes up by the same amount every time and
    /// there is no count to be had. It does terminate, which is the point: the pass is not allowed
    /// to lean on a loop looking harmless, only on the count that says it ends.
    fn doubling() -> Shape {
        let mut names = Interner::new();
        let signature = Signature::new().with_params(&[Type::PTR, Type::int(32)]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let head = func.create_block();
        let done = func.create_block();
        func.append_param(entry, Type::PTR);
        func.append_param(entry, Type::int(32));
        let carried = func.append_param(head, Type::int(32));

        let mut build = Builder::new(&mut func, entry);
        let one = build.iconst(Type::int(32), 1);
        build.jump(head, &[one]);

        let mut build = Builder::new(&mut func, head);
        let next = build.binary(Opcode::Add, carried, carried, Flags::NSW);
        let stop = build.iconst(Type::int(32), 1000);
        let test = build.icmp(IntPred::Slt, next, stop);
        build.br_if(test, head, &[next], done, &[]);
        Builder::new(&mut func, done).ret(&[]);
        Shape { names, func, entry, done }
    }

    #[test]
    fn a_loop_whose_count_is_not_known_is_left_alone() {
        let mut it = doubling();
        let stats = delete(&mut it.func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Optimized, DELETED), 0);
        assert_eq!(stats.count(Kind::Missed, NO_COUNT), 1);
        assert_eq!(loops(&it.func), 1);
        sound(&it.func, &mut it.names);
    }

    #[test]
    fn the_pass_stops_when_the_fuel_runs_out() {
        let mut it = shaped(Limit::Number(1000), What::Nothing);
        let stats = delete(&mut it.func, &mut Fuel::of(0));
        assert_eq!(stats.count(Kind::Optimized, DELETED), 0);
        assert_eq!(stats.count(Kind::Missed, NO_FUEL), 1);
        assert_eq!(loops(&it.func), 1);
        sound(&it.func, &mut it.names);
    }

    #[test]
    fn a_function_with_no_body_is_not_a_problem() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let stats = delete(&mut func, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Optimized, DELETED), 0);
    }
}
