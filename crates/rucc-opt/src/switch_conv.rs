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
//! Where the answers are an affine function of the label, `a * x + b`, there is nothing to look
//! up. That covers the shape above with `a` of one and `b` of one, the shape where every arm gives
//! the same answer with `a` of zero, and the scaled ones in between.
//!
//! Where they are not, the answers are a table. The arm for label `k` gives the constant in cell
//! `k - low` of a read only array, and every arm becomes one load from it. That is section 24.4's
//! other half, and it is what gcc calls a `CSWTCH` array. The array is asked for through
//! `crate::readonly`, because a pass is handed one function and the array is the module's.
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
//! For arithmetic, the labels are consecutive. A hole in the labels is a stretch the lowering
//! would then have to cut the run at, and one comparison becomes several for a function that was
//! only fitted to the labels either side of it.
//!
//! For a table, the labels may have holes. Where the default hands on what the arms hand on with a
//! constant in the answer's place, which is `default: return 0;` and `default: y = 0; break;`, a
//! hole's cell is that constant and the hole is given a case of its own going to the load, as gcc's
//! `gather_default_values` does. The labels are then one run, which the lowering checks with one
//! comparison, where a run with holes in it is a comparison and a bit test, and the bit test is a
//! branch a stream of values mispredicts. Where the default does anything else, the `switch` still
//! sends a value in a hole to the default, the hole's cell is never read, and it is written as zero
//! only because an array has to have something there. What bounds the holes is size: the table spans at most eight cells for every label it replaces, which is gcc's
//! `switch-conversion-max-branch-ratio`, so a `switch` over three labels a thousand apart stays a
//! `switch`.
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
//! For arithmetic, the label and the answer are the same width. A `switch` on an `int` whose arms
//! give a `long` is the same transformation with a widening in front of the multiply, and which
//! widening it is depends on how the label is read, which is a question this would have to answer
//! and currently declines to ask. A table does not have that question, because the label only
//! picks a cell and the cell is already as wide as the answer, so a table may be any whole number
//! of bytes wide up to eight whatever the label is. A label wider than a word gets no table, since
//! the index into one is a word.
//!
//! # Why three labels and not two
//!
//! Two labels and a default is a shape `phiopt` already has something to say about, and what it
//! says is a `select` between two constants that cost nothing to materialize. The arithmetic this
//! writes is a multiply and an add against a range check, which is not obviously better than that
//! and is worse when `a` is not one. From three labels up the chain being replaced is at least six
//! instructions and what replaces it is at most five, so it is a win at three and grows from there.
//!
//! A table is held to the same three. What replaces the chain is a subtraction, the range check,
//! a widening and a load, which is five again, and the load is from a line the program keeps
//! reading if the `switch` is hot.
//!
//! # Where the table goes
//!
//! The array is internal, constant and aligned to its cell, which puts it in `.rodata`. It is
//! named `CSWTCH.` and a number, as gcc names it, which nothing written in C can spell.
//!
//! When the goal is size a cell is as narrow as the answers allow, and the load is widened back to
//! the answer's width with a sign or without one, whichever holds every answer. gcc 16 does the
//! same at `-Os` and not at `-O2`, where the widening is an instruction on the path and the bytes
//! it saves are data rather than code. Three `int` answers under ten are twelve bytes at `-O2` and
//! three at `-Os`, in gcc and here.

use std::cmp::Ordering;
use std::collections::HashSet;

use rucc_base::Symbol;
use rucc_ir::{
    Block, BlockCall, Builder, Extra, Flags, Func, Imm, Inst, InstData, MemInfo, MemOrder, Opcode,
    Restrict, Type, Value,
};

use rucc_cost::Goal;
use rucc_cost::heuristics::SWITCH_CONVERSION_MAX_GROWTH;

use crate::cfg::Cfg;
use crate::{Analyses, Fuel, Pass, Preserved, ReadOnly, Stats};

/// What is reported when a `switch` becomes arithmetic.
const CONVERTED: &str = "switch replaced by a range check and the arithmetic its arms were doing";

/// What is reported when a `switch` becomes a load from a table of what its arms gave.
const TABLED: &str = "switch replaced by a range check and a load from a table of its answers";

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

/// What is reported for a `switch` whose labels are too far apart for a table of them.
const TOO_SPARSE: &str = "switch left alone, a table of its answers would be mostly holes";

/// What is reported for a `switch` whose label is wider than an index into a table.
const LABEL_TOO_WIDE: &str = "switch left alone, its label is wider than a word";

/// What is reported for a `switch` whose answers are not something a table cell holds.
const CELL_IS_ODD: &str =
    "switch left alone, its answers are not a whole number of bytes of integer";

/// The fewest labels worth converting, per the module documentation.
const LABELS: usize = 3;

/// How many cells a table may have for every label it stands for, per the module documentation.
const GROWTH: i128 = SWITCH_CONVERSION_MAX_GROWTH as i128;

/// The pass.
#[derive(Debug)]
pub struct SwitchConv;

impl Pass for SwitchConv {
    fn name(&self) -> &'static str {
        "switch-conv"
    }

    fn describe(&self) -> &'static str {
        "a switch whose arms give constants becomes a range check and arithmetic or a table load"
    }

    fn preserves(&self) -> Preserved {
        // A block appears, the arms go, and every case edge moves.
        Preserved::NONE
    }

    fn run(&self, func: &mut Func, an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        convert(func, an, fuel, None)
    }

    fn run_emitting(
        &self,
        func: &mut Func,
        an: &mut Analyses,
        fuel: &mut Fuel,
        data: &mut ReadOnly<'_>,
    ) -> Stats {
        convert(func, an, fuel, Some(data))
    }
}

/// The pass, with somewhere to put a table or without one.
///
/// Without one is what a caller that is not the pipeline gets, and it is arithmetic or nothing.
fn convert(
    func: &mut Func,
    an: &mut Analyses,
    fuel: &mut Fuel,
    mut data: Option<&mut ReadOnly<'_>>,
) -> Stats {
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

    let index_bits = data.as_ref().map(|data| data.pointer_bits());
    let small = an.machine().goal() == Goal::Size;
    let mut plans = Vec::new();
    for inst in found {
        match plan(func, cfg, inst, index_bits, small) {
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
        let table = match (&plan.how, data.as_deref_mut()) {
            (How::Table { cell, cells, .. }, Some(data)) => {
                Some(data.table(cell.ty, cells.clone()))
            }
            _ => None,
        };
        stats.optimized(if table.is_some() { TABLED } else { CONVERTED });
        apply(func, &plan, table);
        changed = true;
    }
    if changed {
        an.clear();
    }
    stats
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
    /// What it switches on, which is what the answer is a function of.
    value: Value,
    /// The width of the label.
    ty: Type,
    /// Where the answer goes.
    hands: Hands,
    /// What every arm handed on, with the answer's position holding whatever the first arm had
    /// there. That position is rewritten and the rest are passed on as they were.
    args: Vec<Value>,
    /// Which of `args` is the answer.
    answer: usize,
    /// How the answer is worked out from the label.
    how: How,
    /// The blocks the arms were, which nothing reaches once the case edges have moved.
    arms: Vec<Block>,
    /// Values between two labels that get a case of their own going to the load, because the
    /// default gives what their cell holds.
    holes: Vec<i128>,
}

/// How the answer is worked out from the label.
#[derive(Debug)]
enum How {
    /// As `scale * label + offset`, at the label's width.
    Line {
        /// The multiple of the label.
        scale: i128,
        /// What is added to it.
        offset: i128,
    },
    /// As cell `label - low` of a table.
    Table {
        /// The lowest label, which is cell zero.
        low: i128,
        /// The width of the answer.
        ty: Type,
        /// What a cell is, which is the answer unless the goal is size.
        cell: Cell,
        /// Every cell, with zero in the holes.
        cells: Vec<i128>,
        /// The width of an index into the table, which is a word on the target.
        index_bits: u32,
    },
}

/// How a cell of a table is held, and how it is made an answer again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Cell {
    /// The width of a cell.
    ty: Type,
    /// Whether a cell narrower than the answer is widened with its sign.
    signed: bool,
}

/// What one `switch` becomes, or why it stays as it is.
///
/// `index_bits` is the width of an address when a table may be made and `None` when it may not,
/// and `small` is whether the goal is size, which is what narrows a cell.
fn plan(
    func: &Func,
    cfg: &Cfg,
    inst: Inst,
    index_bits: Option<u32>,
    small: bool,
) -> Result<Plan, &'static str> {
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
    // do before it knows the answers are a line. Only a line asks, since a table indexes by the
    // label whatever order the labels came in.
    let consecutive = labels.windows(2).all(|pair| pair[1].checked_sub(pair[0]) == Some(1));
    if !consecutive && index_bits.is_none() {
        return Err(NOT_CONSECUTIVE);
    }

    // Every arm is a block of its own that works out constants and hands them on, and the way it
    // hands them on is the way every other arm does.
    let mut hands = None;
    let mut shared: Option<Vec<Value>> = None;
    let mut answer = None;
    let mut handed = Vec::new();
    for call in arms {
        if !call.args.is_empty() {
            return Err(ARM_DOES_WORK);
        }
        if cfg.predecessors(call.block).len() != 1 {
            return Err(ARM_IS_SHARED);
        }
        // And a block an image holds the address of is shared whatever the graph says, because what
        // arrives there is a `goto *p` that can be in another function.
        if func.block_name(call.block).is_some() {
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
        handed.push(args);
    }
    let (Some(hands), Some(args)) = (hands, shared) else { return Err(ARMS_DIFFER) };
    let answer = answer.ok_or(NOT_AFFINE)?;
    // Read once the position is known and not while it was being found. Until the second arm
    // disagrees with the first nobody knows which position the answer is in, and reading the first
    // arm's answer from a guess of the first position would take whatever was there, which is a
    // constant every arm passed if the arms pass one, and a line fitted through that is wrong at
    // the first label.
    let mut answers = Vec::with_capacity(handed.len());
    for args in &handed {
        let Some(number) = constant(func, args[answer]) else { return Err(NOT_AFFINE) };
        answers.push(number);
    }
    let kind = func[args[answer]].ty;

    let line = if consecutive && kind == ty { line(&labels, &answers, ty) } else { None };
    let (how, holes) = match (line, index_bits) {
        (Some((scale, offset)), _) => (How::Line { scale, offset }, Vec::new()),
        (None, Some(index_bits)) => {
            let fill = fallback(func, default, hands, &args, answer);
            let shape = Shape { ty, kind, index_bits, small };
            table(&labels, &answers, shape, fill)?
        }
        (None, None) if kind != ty => return Err(WIDTHS_DIFFER),
        (None, None) => return Err(NOT_AFFINE),
    };
    Ok(Plan {
        inst,
        value,
        ty,
        hands,
        args,
        answer,
        how,
        arms: arms.iter().map(|call| call.block).collect(),
        holes,
    })
}

/// What the default gives in the answer's place, when that is all it does differently from an arm.
///
/// Two shapes of default qualify. One is a block of its own that works out constants and hands
/// them on the way the arms do, which is `default: return 0;`. The other is an edge straight to
/// where the arms hand their answer, carrying the answer itself, which is what is left of
/// `default: y = 0; break;` once the empty block is gone. Either way every position but the answer
/// has to be what the arms pass, since a hole given a case is about to pass that instead. The
/// default block is only read here and never taken away, so it may be shared.
fn fallback(
    func: &Func,
    default: BlockCall,
    hands: Hands,
    args: &[Value],
    answer: usize,
) -> Option<i128> {
    let theirs = if default.args.is_empty() {
        let (way, theirs) = tail(func, default.block).ok()?;
        if way != hands {
            return None;
        }
        theirs
    } else if hands == Hands::On(default.block) {
        func[default.args].to_vec()
    } else {
        return None;
    };
    if theirs.len() != args.len() {
        return None;
    }
    let agrees =
        args.iter().zip(&theirs).enumerate().all(|(at, (mine, it))| at == answer || mine == it);
    if !agrees {
        return None;
    }
    constant(func, theirs[answer])
}

/// What a table is made for: the label's width, the answer's, an index's, and whether the goal is
/// size.
#[derive(Clone, Copy, Debug)]
struct Shape {
    /// The width of the label.
    ty: Type,
    /// The width of the answer.
    kind: Type,
    /// The width of an index into the table.
    index_bits: u32,
    /// Whether a cell may be narrower than the answer.
    small: bool,
}

/// The table the answers make when one is worth making, and the holes that get a case of their own.
///
/// A hole gets one only when `fill` is what the default gives, and then its cell is that. The
/// labels are read with their own sign and so are ordered that way, which is only a question
/// of which one is cell zero. What the index is at run time is the label less the lowest one at the
/// label's width, and for a label that is a case that difference is the distance between the two
/// however the bits are read, because the table is short and the distance fits.
fn table(
    labels: &[i128],
    answers: &[i128],
    shape: Shape,
    fill: Option<i128>,
) -> Result<(How, Vec<i128>), &'static str> {
    let Shape { ty, kind, index_bits, small } = shape;
    if ty.bits() > 64 {
        return Err(LABEL_TOO_WIDE);
    }
    if !kind.is_int() || !matches!(kind.bits(), 8 | 16 | 32 | 64) {
        return Err(CELL_IS_ODD);
    }
    let (Some(&low), Some(&high)) = (labels.iter().min(), labels.iter().max()) else {
        return Err(TOO_FEW);
    };
    let span = high - low + 1;
    if span > GROWTH * labels.len() as i128 {
        return Err(TOO_SPARSE);
    }
    let mut cells = vec![None; usize::try_from(span).map_err(|_| TOO_SPARSE)?];
    for (&label, &answer) in labels.iter().zip(answers) {
        let at = usize::try_from(label - low).map_err(|_| TOO_SPARSE)?;
        cells[at] = Some(answer);
    }
    let holes: Vec<i128> = match fill {
        Some(_) => (low..=high).filter(|&label| cells[(label - low) as usize].is_none()).collect(),
        None => Vec::new(),
    };
    let cells: Vec<i128> = cells.into_iter().map(|cell| cell.or(fill).unwrap_or(0)).collect();
    let cell = if small { narrowest(&cells, kind) } else { Cell { ty: kind, signed: false } };
    Ok((How::Table { low, ty: kind, cell, cells, index_bits }, holes))
}

/// The narrowest cell every answer fits in, read back to the answer's width.
///
/// With a sign first, because an answer below zero only fits that way, and then without one, which
/// is what fits a `200` in a byte. An answer that fits neither way at a width is an answer that
/// needs the next one, and the answer's own width always fits.
fn narrowest(answers: &[i128], kind: Type) -> Cell {
    let whole = 1i128 << kind.bits();
    for bits in [8u32, 16, 32] {
        if bits >= kind.bits() {
            break;
        }
        let half = 1i128 << (bits - 1);
        if answers.iter().all(|&answer| (-half..half).contains(&answer)) {
            return Cell { ty: Type::int(bits), signed: true };
        }
        if answers.iter().all(|&answer| answer.rem_euclid(whole) < half * 2) {
            return Cell { ty: Type::int(bits), signed: false };
        }
    }
    Cell { ty: kind, signed: false }
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

/// The answer as `scale * label + offset`.
fn arithmetic(builder: &mut Builder<'_>, plan: &Plan, scale: i128, offset: i128) -> Value {
    let scaled = match scale {
        0 => builder.iconst(plan.ty, offset),
        1 => plan.value,
        scale => {
            let by = builder.iconst(plan.ty, scale);
            builder.binary(Opcode::Mul, plan.value, by, Flags::NONE)
        }
    };
    if offset == 0 || scale == 0 {
        scaled
    } else {
        let by = builder.iconst(plan.ty, offset);
        builder.binary(Opcode::Add, scaled, by, Flags::NONE)
    }
}

/// The answer as cell `label - low` of the table called `name`.
///
/// The subtraction is at the label's width and wraps, and then the difference is made a word. Only
/// a label that is a case gets here, so the difference is below the table's length and the
/// widening is the same with or without a sign, and it is written without one.
fn look_up(
    builder: &mut Builder<'_>,
    plan: &Plan,
    name: Symbol,
    low: i128,
    ty: Type,
    index_bits: u32,
) -> Value {
    let from = if low == 0 {
        plan.value
    } else {
        let by = builder.iconst(plan.ty, low);
        builder.binary(Opcode::Sub, plan.value, by, Flags::NONE)
    };
    let word = Type::int(index_bits);
    let index = match plan.ty.bits().cmp(&index_bits) {
        Ordering::Less => builder.unary(Opcode::ZExt, from, word),
        Ordering::Greater => builder.unary(Opcode::Trunc, from, word),
        Ordering::Equal => from,
    };
    let bytes = ty.bits() / 8;
    let distance = if bytes == 1 {
        index
    } else {
        let by = builder.iconst(word, i128::from(bytes));
        builder.binary(Opcode::Mul, index, by, Flags::NONE)
    };
    let base = builder.value(
        InstData { extra: Extra::Symbol(name), ..InstData::new(Opcode::GlobalAddr) },
        Type::PTR,
    );
    let cell = builder.binary(Opcode::PtrAdd, base, distance, Flags::NONE);
    let info = MemInfo {
        size: u64::from(bytes),
        align: bytes,
        order: MemOrder::NotAtomic,
        tbaa: None,
        owns: 0,
        restrict: Restrict::NONE,
    };
    builder.load(ty, cell, info, Flags::NONE)
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
///
/// `table` is the name of the table a plan for one was given, and `None` for a line.
fn apply(func: &mut Func, plan: &Plan, table: Option<Symbol>) {
    let span = func.span(plan.inst);
    let hit = func.create_block();
    let mut builder = Builder::new(func, hit).at(span);
    let answer = match (&plan.how, table) {
        (&How::Line { scale, offset }, _) => arithmetic(&mut builder, plan, scale, offset),
        (&How::Table { low, ty, cell, index_bits, .. }, Some(name)) => {
            let read = look_up(&mut builder, plan, name, low, cell.ty, index_bits);
            match (cell.ty == ty, cell.signed) {
                (true, _) => read,
                (false, true) => builder.unary(Opcode::SExt, read, ty),
                (false, false) => builder.unary(Opcode::ZExt, read, ty),
            }
        }
        (How::Table { .. }, None) => unreachable!("a table was planned with nowhere to put it"),
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
    let mut cases: Vec<Imm> = func[func[info].cases].to_vec();
    for &hole in &plan.holes {
        calls.push(BlockCall::new(hit, empty));
        cases.push(Imm::int(hole, plan.ty));
    }
    let targets = func.push_block_calls(&calls);
    let cases = func.push_imms(&cases);
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
    use std::collections::{HashMap, HashSet};

    use rucc_base::Interner;
    use rucc_cost::Goal;
    use rucc_ir::{Block, Builder, Func, Opcode, Signature, Type, Value};

    use super::SwitchConv;
    use crate::stats::Kind;
    use crate::{Fuel, Pass, ReadOnly, Stats, Table};

    /// The width everything here switches on and answers in, unless a test says otherwise.
    fn i32() -> Type {
        Type::int(32)
    }

    /// Runs the pass with as much fuel as it wants.
    fn convert(func: &mut Func) -> Stats {
        SwitchConv.run(func, &mut crate::machine::fixtures::analyses(), &mut Fuel::unlimited())
    }

    /// Runs the pass the way the pipeline does, with a place for tables, and hands back the
    /// tables it asked for.
    fn tabled(func: &mut Func) -> (Stats, Vec<Table>) {
        tabled_for(func, Goal::Speed)
    }

    /// The same for a goal, which is what decides how wide a cell is.
    fn tabled_for(func: &mut Func, goal: Goal) -> (Stats, Vec<Table>) {
        let mut names = Interner::new();
        let taken = HashSet::new();
        let mut data = ReadOnly::new(&mut names, &taken, 64, 0);
        let mut an = crate::Analyses::new(crate::Machine::with(None, goal));
        let stats = SwitchConv.run_emitting(func, &mut an, &mut Fuel::unlimited(), &mut data);
        (stats, data.into_tables())
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
        looked_up(func, block, label, &[])
    }

    /// What the block the case edges go to answers, given that label and the tables it may read.
    ///
    /// The address of a table is its cell zero counted in bytes, which is all a load from one
    /// needs, and a load stops the test if it is not on a cell or not inside the table.
    fn looked_up(func: &Func, block: Block, label: i128, tables: &[Table]) -> i128 {
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
                Opcode::Sub => args[0].wrapping_sub(args[1]),
                // Unsigned, which is what the widening is, and then the width it widens to.
                Opcode::ZExt => {
                    let from = func[func[data.args][0]].ty;
                    super::wrap(args[0], from).rem_euclid(1 << from.bits())
                }
                Opcode::SExt => args[0],
                Opcode::GlobalAddr => 0,
                Opcode::PtrAdd => args[0] + args[1],
                Opcode::Load => {
                    assert_eq!(tables.len(), 1, "a load with no single table to read");
                    let table = &tables[0];
                    let bytes = i128::from(table.ty.bits() / 8);
                    assert_eq!(args[0] % bytes, 0, "a load between two cells");
                    let at = usize::try_from(args[0] / bytes).expect("a load before the table");
                    *table.cells.get(at).expect("a load after the table")
                }
                other => panic!("this pass does not write {other:?}"),
            };
            // An address is a number of bytes into a table and has no width to wrap at.
            let ty = func[result].ty;
            values.insert(result, if ty.is_int() { super::wrap(it, ty) } else { it });
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
    fn labels_with_a_hole_in_them_are_left_alone_where_no_table_can_be_made() {
        let mut func = returning(i32(), &[0, 1, 3], &[1, 2, 4]);
        assert!(!fired(&convert(&mut func)));
        assert_eq!(cases(&func).len(), 3);
    }

    #[test]
    fn answers_that_are_not_a_line_are_left_alone_where_no_table_can_be_made() {
        let mut func = returning(i32(), &[0, 1, 2], &[5, 9, 2]);
        assert!(!fired(&convert(&mut func)));
    }

    #[test]
    fn two_labels_are_not_enough_to_pay_for_the_arithmetic() {
        let mut func = returning(i32(), &[0, 1], &[1, 2]);
        assert!(!fired(&convert(&mut func)));
    }

    #[test]
    fn an_answer_wider_than_its_label_is_left_alone_where_no_table_can_be_made() {
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

    /// The opcodes of a block that answers from a table, from the label at zero.
    const LOOKUP: [Opcode; 6] = [
        Opcode::ZExt,
        Opcode::IConst,
        Opcode::Mul,
        Opcode::GlobalAddr,
        Opcode::PtrAdd,
        Opcode::Load,
    ];

    #[test]
    fn answers_that_are_not_a_line_are_one_load_from_a_table() {
        let mut func = returning(i32(), &[0, 1, 2, 3], &[5, 9, 2, 7]);
        let (stats, tables) = tabled(&mut func);
        assert!(fired(&stats));
        assert_eq!(tables.len(), 1);
        assert_eq!(tables[0].ty, i32());
        assert_eq!(tables[0].cells, [5, 9, 2, 7]);
        let arm = arm(&func);
        let mut want = LOOKUP.to_vec();
        want.push(Opcode::Return);
        assert_eq!(opcodes(&func, arm), want);
        for (label, answer) in [(0, 5), (1, 9), (2, 2), (3, 7)] {
            assert_eq!(looked_up(&func, arm, label, &tables), answer);
        }
    }

    /// A hole gets the default's answer and a case of its own, when the default only gives one.
    ///
    /// The default here returns 999 and nothing else, so the value in the hole reads 999 out of
    /// the table and gets what it got before, and the labels are one run with no hole in it.
    #[test]
    fn a_hole_is_filled_with_what_a_default_that_only_answers_gives() {
        let mut func = returning(i32(), &[1, 2, 4, 5], &[10, 20, 40, 55]);
        let head = func.entry().expect("a function with blocks in it");
        let before = func.terminator(head).expect("a head block has one");
        let default = func.successors(before).next().expect("a switch has a default").block;
        let (stats, tables) = tabled(&mut func);
        assert!(fired(&stats));
        assert_eq!(tables[0].cells, [10, 20, 999, 40, 55]);
        assert_eq!(cases(&func).len(), 5, "the hole was not given a case");
        let after = func.terminator(head).expect("a head block has one");
        assert_eq!(func.successors(after).next().map(|call| call.block), Some(default));
        let arm = arm(&func);
        for (label, answer) in [(1, 10), (2, 20), (3, 999), (4, 40), (5, 55)] {
            assert_eq!(looked_up(&func, arm, label, &tables), answer);
        }
    }

    /// A hole is a cell nothing reads when the default does something other than answer.
    ///
    /// This default returns the label, which is no constant, so the value in the hole has to
    /// keep going to it down the edge it always went down.
    #[test]
    fn a_hole_still_goes_to_a_default_that_does_more_than_answer() {
        let mut func = returning(i32(), &[1, 2, 4, 5], &[10, 20, 40, 55]);
        let head = func.entry().expect("a function with blocks in it");
        let before = func.terminator(head).expect("a head block has one");
        let default = func.successors(before).next().expect("a switch has a default").block;
        let label = func[func[before].args][0];
        let ret = func.terminator(default).expect("the default returns");
        func.remove_inst(ret);
        Builder::new(&mut func, default).ret(&[label]);
        let (stats, tables) = tabled(&mut func);
        assert!(fired(&stats));
        assert_eq!(tables[0].cells, [10, 20, 0, 40, 55]);
        assert_eq!(cases(&func).len(), 4, "a hole was given a case");
        let after = func.terminator(head).expect("a head block has one");
        assert_eq!(func.successors(after).next().map(|call| call.block), Some(default));
        let arm = arm(&func);
        for (label, answer) in [(1, 10), (2, 20), (4, 40), (5, 55)] {
            assert_eq!(looked_up(&func, arm, label, &tables), answer);
        }
    }

    /// Cell zero is the lowest label, which here is below zero and reached by wrapping.
    ///
    /// Every label of a `signed char` is tried, which is the whole of what can reach the block
    /// and the whole of what can go wrong with the subtraction and the widening after it.
    #[test]
    fn labels_below_zero_index_from_the_lowest_of_them() {
        let ty = Type::int(8);
        let labels = [-128, -3, -1, 0, 2, 127];
        let answers = [7, -5, 11, 3, -100, 42];
        let mut func = returning(ty, &labels, &answers);
        // Far apart at the ends, so a table is only allowed because the ratio is eight to one and
        // there are six labels: two hundred and fifty six cells is more than forty eight.
        let (stats, _) = tabled(&mut func);
        assert!(!fired(&stats), "a table of mostly holes was made");

        let labels = [-3, -2, -1, 0, 2];
        let answers = [7, -5, 11, 3, -100];
        let mut func = returning(ty, &labels, &answers);
        let (stats, tables) = tabled(&mut func);
        assert!(fired(&stats));
        // The default returns 999, which is -25 as a `signed char`, and the hole at 1 is given it.
        assert_eq!(tables[0].cells, [7, -5, 11, 3, -25, -100]);
        let arm = arm(&func);
        assert_eq!(opcodes(&func, arm)[..2], [Opcode::IConst, Opcode::Sub]);
        for (&label, &answer) in labels.iter().zip(&answers).chain([(&1, &-25)]) {
            assert_eq!(looked_up(&func, arm, label, &tables), answer);
        }
    }

    /// An `int` label and a `long` answer, which a line declines and a table does not mind.
    #[test]
    fn an_answer_wider_than_its_label_is_a_table_of_the_wider_type() {
        let mut names = Interner::new();
        let answers = [1i128 << 40, 3, -1, 1 << 33];
        let mut func = Func::new(names.intern("f"), Signature::new());
        let head = func.create_block();
        let value = func.append_param(head, i32());
        let default = func.create_block();
        let arms: Vec<Block> = answers.iter().map(|_| func.create_block()).collect();
        for (&arm, &answer) in arms.iter().zip(&answers) {
            let mut build = Builder::new(&mut func, arm);
            let it = build.iconst(Type::int(64), answer);
            build.ret(&[it]);
        }
        let mut build = Builder::new(&mut func, default);
        let it = build.iconst(Type::int(64), 0);
        build.ret(&[it]);
        let cases: Vec<(i128, Block)> = (10..14).zip(arms.iter().copied()).collect();
        Builder::new(&mut func, head).switch(value, default, &cases);
        let (stats, tables) = tabled(&mut func);
        assert!(fired(&stats));
        assert_eq!(tables[0].ty, Type::int(64));
        let arm = arm(&func);
        for (label, &answer) in (10..14).zip(&answers) {
            assert_eq!(looked_up(&func, arm, label, &tables), answer);
        }
    }

    #[test]
    fn labels_too_far_apart_for_a_table_are_left_alone() {
        let mut func = returning(i32(), &[0, 100, 200], &[1, 5, 3]);
        let (stats, tables) = tabled(&mut func);
        assert!(!fired(&stats));
        assert!(tables.is_empty());
    }

    #[test]
    fn a_line_is_still_arithmetic_where_a_table_could_be_made() {
        let mut func = returning(i32(), &[0, 1, 2, 3], &[1, 2, 3, 4]);
        let (stats, tables) = tabled(&mut func);
        assert!(fired(&stats));
        assert!(tables.is_empty(), "a table was made for a line");
    }

    #[test]
    fn a_label_wider_than_a_word_gets_no_table() {
        let mut func = returning(Type::int(128), &[0, 1, 2, 3], &[5, 9, 2, 7]);
        let (stats, tables) = tabled(&mut func);
        assert!(!fired(&stats));
        assert!(tables.is_empty());
    }

    /// The answer is in the second place and the first is a constant every arm passes.
    ///
    /// The first arm's answer used to be read from the first place, because which place the
    /// answer is in is not known until a second arm disagrees. Here that reads one at the first
    /// label, and one, two and three are a line, so the pass returned one where the program said
    /// ten. What it must do is see ten, two and three, which is not a line.
    #[test]
    fn the_answer_is_read_from_the_place_the_arms_disagree_about() {
        let mut names = Interner::new();
        let mut func = Func::new(names.intern("f"), Signature::new());
        let head = func.create_block();
        let value = func.append_param(head, i32());
        let join = func.create_block();
        let first = func.append_param(join, i32());
        let second = func.append_param(join, i32());
        Builder::new(&mut func, join).ret(&[second, first]);
        let default = func.create_block();
        let arms: Vec<Block> = (0..3).map(|_| func.create_block()).collect();
        let mut build = Builder::new(&mut func, head);
        let one = build.iconst(i32(), 1);
        let cases: Vec<(i128, Block)> = (0..3).zip(arms.iter().copied()).collect();
        build.switch(value, default, &cases);
        let mut build = Builder::new(&mut func, default);
        let it = build.iconst(i32(), 999);
        build.jump(join, &[one, it]);
        for (&arm, answer) in arms.iter().zip([10, 2, 3]) {
            let mut build = Builder::new(&mut func, arm);
            let it = build.iconst(i32(), answer);
            build.jump(join, &[one, it]);
        }
        assert!(!fired(&convert(&mut func)), "ten, two and three were taken for a line");
        let (stats, tables) = tabled(&mut func);
        assert!(fired(&stats));
        assert_eq!(tables[0].cells, [10, 2, 3]);
    }

    /// At the size goal a cell is a byte when every answer is one, and the load is widened back.
    ///
    /// Below zero is widened with a sign and two hundred without one, so both are tried, and the
    /// speed goal keeps the answer's own width, which is what gcc 16 does at the two levels.
    #[test]
    fn a_table_for_size_has_cells_as_narrow_as_its_answers() {
        let mut func = returning(i32(), &[0, 1, 2, 3], &[5, -9, 2, 7]);
        let (stats, tables) = tabled_for(&mut func, Goal::Size);
        assert!(fired(&stats));
        assert_eq!(tables[0].ty, Type::int(8));
        let at = arm(&func);
        assert!(opcodes(&func, at).contains(&Opcode::SExt));
        for (label, answer) in [(0, 5), (1, -9), (2, 2), (3, 7)] {
            assert_eq!(looked_up(&func, at, label, &tables), answer);
        }

        let mut func = returning(i32(), &[0, 1, 2, 3], &[5, 200, 2, 255]);
        let (_, tables) = tabled_for(&mut func, Goal::Size);
        assert_eq!(tables[0].ty, Type::int(8));
        let at = arm(&func);
        assert!(opcodes(&func, at).contains(&Opcode::ZExt));
        for (label, answer) in [(0, 5), (1, 200), (2, 2), (3, 255)] {
            assert_eq!(looked_up(&func, at, label, &tables), answer);
        }

        let mut func = returning(i32(), &[0, 1, 2, 3], &[5, -300, 2, 40000]);
        let (_, tables) = tabled_for(&mut func, Goal::Size);
        assert_eq!(tables[0].ty, i32(), "a cell narrower than an answer that needs all of it");

        let mut func = returning(i32(), &[0, 1, 2, 3], &[5, -9, 2, 7]);
        let (_, tables) = tabled_for(&mut func, Goal::Speed);
        assert_eq!(tables[0].ty, i32());
    }
}
