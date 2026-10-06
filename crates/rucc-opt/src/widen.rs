//! Gives a counter narrower than a pointer a sixty four bit twin, and reads its widenings off that.
//!
//! `for (int i = 0; i < n; i++) a[i] = b[i];` indexes two arrays with an `int`, and every address
//! wants the counter sign extended to sixty four bits first. Without this pass the extension is
//! done again on every turn, and `crate::ivopts` prices an index that needs one at a `movsx` per
//! use, so a loop over several arrays came out walking a pointer for each of them beside the
//! counter. On x86-64 that is a register per array where gcc and clang keep one counter and read
//! every array with a scaled index. The loop that deforms a Postgres tuple is that loop, and with
//! its three pointers and its counter it ran out of registers and spilled. See tamnd/rucc#1994.
//!
//! What is done here is what both of them do. The loop gets a second header parameter `w` that
//! starts at the extension of where the counter starts and goes up by the same step, in sixty four
//! bits. A widening of the counter that `crate::scev` can show is the same sequence as `w` is
//! replaced by `w`, or by `w` plus the number it is apart from it. A comparison of the counter
//! against something the loop does not change is asked of `w` and of that something widened in
//! front of the loop. Whatever else reads the counter reads `w` cut back down to its width, which
//! is no instruction at all on x86-64. Once nothing reads the old counter it goes, and `crate::ivopts`
//! sees a loop with one sixty four bit counter in it.
//!
//! # Why it is the same program
//!
//! `w` is the sequence `{sext(start), +, step}` by construction: the preheader hands it the first
//! number and the latch hands it one step on. A widening is replaced only when the analysis says
//! it is that sequence, plus a number, on every turn. The analysis only says so when the narrow
//! sequence cannot wrap, which it shows from the `nsw` or `nuw` on the increment or from the
//! loop's own exit test, and that is the same proof every other reader of a widened chrec leans
//! on. Cutting `w` back down is right whether or not the counter wraps, because the low bits of
//! `start + k * step` are the same at any width.
//!
//! A comparison needs one more thing. `i < n` is `sext(i) < sext(n)` for a signed `<` and only
//! for a signed one, and the same with a zero extension and an unsigned one, while `==` and `!=`
//! hold under either. So a comparison is asked of `w` only when its predicate agrees with the way
//! the counter was widened. One asked of the increment `i + 1` rather than of `i` needs that
//! increment not to wrap either, since the extension of `i + 1` is `w + 1` only then, and the flag
//! on the increment is what says so.
//!
//! # Where it runs
//!
//! In front of `crate::ivopts`, at `-O2` and `-O3`, and only on a machine with a cost table. Every
//! such machine has sixty four bit pointers, which is the width `w` is built at.

use rucc_base::hash::{Map, Set};
use rucc_ir::{
    Block, BlockCall, Def, Extra, Flags, Func, Imm, Inst, InstData, IntPred, Opcode, Type, Value,
};

use crate::analysis::Analysis;
use crate::cfg::Cfg;
use crate::loops::{LoopId, Loops};
use crate::scev::{Chrec, Plain, Reading, Scev};
use crate::{Analyses, Fuel, Pass, Preserved, Stats};

const NO_TARGET: &str =
    "left alone, nobody has priced this machine and the answer is a fact about the machine";
const WIDENED: &str = "counter narrower than a pointer given a sixty four bit twin";
const EXTENSION: &str = "widening of a counter read off its sixty four bit twin";
const COMPARED: &str = "comparison of a counter asked of its sixty four bit twin";
const OUT_OF_FUEL: &str = "not widened, the fuel for this compilation ran out first";

/// How many times a value is followed back through blocks with one way in.
const FORWARD_LIMIT: u32 = 16;

/// The pass.
#[derive(Debug)]
pub struct Widen;

impl Pass for Widen {
    fn name(&self) -> &'static str {
        "widen"
    }

    fn describe(&self) -> &'static str {
        "gives a counter narrower than a pointer a sixty four bit twin to index memory with"
    }

    fn preserves(&self) -> Preserved {
        // Parameters and arithmetic, and no edge moved. What changes is which values are live
        // round the loop.
        Preserved::ALL.without(Analysis::Liveness)
    }

    fn run(&self, func: &mut Func, an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        if func.entry().is_none() {
            return stats;
        }
        if an.machine().table().is_none() {
            stats.missed(NO_TARGET);
            return stats;
        }
        let cfg = an.cfg(func);
        let loops = an.loops(func);
        let mut plans = Vec::new();
        {
            let mut scev = Scev::new(func, cfg, loops);
            let mut taken = Set::default();
            for id in loops.all() {
                plan(func, cfg, loops, &mut scev, id, &mut taken, &mut plans);
            }
        }
        // Innermost first. An inner loop counting from the outer counter widens it in its own
        // preheader, and done the other way round that would be a read of the old outer counter
        // put there after it had stopped being read, keeping it alive.
        plans.sort_by_key(|plan| std::cmp::Reverse(loops.depth(plan.id)));
        for plan in &plans {
            if !fuel.take() {
                stats.missed(OUT_OF_FUEL);
                continue;
            }
            apply(func, plan, &mut stats);
        }
        stats
    }
}

/// A widening in the loop, with what the analysis says it is.
#[derive(Clone, Copy, Debug)]
struct Ext {
    inst: Inst,
    /// What is widened.
    arg: Value,
    signed: bool,
    chrec: Chrec,
    step: i128,
}

/// One counter to give a twin, with everything that is going to read the twin.
#[derive(Debug)]
struct Plan {
    id: LoopId,
    pre: Block,
    header: Block,
    /// The ways into the header, the preheader among them.
    preds: Vec<Block>,
    counter: Value,
    /// Where the counter is among the header's parameters.
    index: usize,
    /// The counter one step on, which is what goes round the loop.
    increment: Value,
    step: i128,
    reading: Reading,
    /// Where the twin starts, as the analysis wrote it.
    start: Plain,
    /// Each widening read off the twin, how far from the twin it is, and whether it widens the
    /// increment and so comes after it.
    exts: Vec<(Inst, i128, bool)>,
    /// Each comparison asked of the twin, which side the counter is on, how far from the twin
    /// that side is, and whether it is the increment.
    compares: Vec<(Inst, usize, i128, bool)>,
}

/// Works out which counters of this loop to widen, and everything that will read each one.
fn plan(
    func: &Func,
    cfg: &Cfg,
    loops: &Loops,
    scev: &mut Scev<'_>,
    id: LoopId,
    taken: &mut Set<Inst>,
    plans: &mut Vec<Plan>,
) {
    let Some(pre) = loops.preheader(cfg, id) else { return };
    let header = loops.header(id);
    let &[latch] = loops.latches(id) else { return };
    let wide = Type::int(64);

    let mut exts = Vec::new();
    for &block in loops.blocks(id) {
        for inst in func.insts(block) {
            let data = &func[inst];
            if !matches!(data.opcode, Opcode::SExt | Opcode::ZExt) {
                continue;
            }
            let Some(result) = data.first_result else { continue };
            let Some(&arg) = func[data.args].first() else { continue };
            if func[result].ty != wide {
                continue;
            }
            let Some(chrec) = scev.evolution(id, result).chrec() else { continue };
            let Some(step) = chrec.step.as_number() else { continue };
            if chrec.ty != wide || step == 0 {
                continue;
            }
            let signed = data.opcode == Opcode::SExt;
            exts.push(Ext { inst, arg, signed, chrec, step });
        }
    }
    if exts.is_empty() {
        return;
    }

    let mut preds = cfg.predecessors(header).to_vec();
    preds.sort_unstable();
    preds.dedup();
    for (index, &counter) in func[header].params.iter().enumerate() {
        let ty = func[counter].ty;
        if !ty.is_int() || !ty.is_scalar() || ty.bits() < 8 || ty.bits() >= 64 {
            continue;
        }
        let Some(narrow) = scev.evolution(id, counter).chrec() else { continue };
        let Some(step) = narrow.step.as_number() else { continue };
        // The first widening of the counter itself that the analysis vouches for says how the
        // counter is read wide and where the twin starts.
        let Some(first) = exts.iter().find(|ext| {
            !taken.contains(&ext.inst)
                && ext.step == step
                && forwarded(func, cfg, ext.arg) == counter
        }) else {
            continue;
        };
        let Some(increment) = increment(func, cfg, latch, header, index, counter, step) else {
            continue;
        };
        let base = first.chrec.base;
        let Some(start) = base.plain() else { continue };
        if start.value.is_some() && start.scale != 1 {
            continue;
        }
        let (reading, signed) =
            if first.signed { (Reading::Signed, true) } else { (Reading::Unsigned, false) };
        let held = match func[increment].def {
            Def::Result { inst, .. } => {
                func[inst].flags.contains(if signed { Flags::NSW } else { Flags::NUW })
            }
            Def::Param { .. } => false,
        };

        let mut reads = Vec::new();
        for ext in &exts {
            if taken.contains(&ext.inst) || ext.step != step || !ext.chrec.base.alike(base) {
                continue;
            }
            let Some(by) = ext.chrec.base.offset().checked_sub(base.offset()) else { continue };
            let after = forwarded(func, cfg, ext.arg) == increment;
            reads.push((ext.inst, by, after));
        }

        // Which side of a comparison is the counter, and how far from the twin it is.
        let role = |value: Value| {
            let at = forwarded(func, cfg, value);
            if at == counter {
                Some((0, false))
            } else if at == increment && held {
                Some((step, true))
            } else {
                None
            }
        };
        let still = |value: Value| {
            crate::fold::constant(func, value).is_some() || loops.is_invariant(func, id, value)
        };
        let mut compares = Vec::new();
        for &block in loops.blocks(id) {
            for inst in func.insts(block) {
                let data = &func[inst];
                if data.opcode != Opcode::ICmp || taken.contains(&inst) {
                    continue;
                }
                let Extra::IntPred(pred) = data.extra else { continue };
                if !matches!(pred, IntPred::Eq | IntPred::Ne) && pred.is_signed() != signed {
                    continue;
                }
                let &[left, right] = &func[data.args] else { continue };
                let side = match (role(left), role(right)) {
                    (Some((by, after)), None) if still(right) => (0, by, after),
                    (None, Some((by, after))) if still(left) => (1, by, after),
                    _ => continue,
                };
                compares.push((inst, side.0, side.1, side.2));
            }
        }

        taken.extend(reads.iter().map(|&(inst, ..)| inst));
        taken.extend(compares.iter().map(|&(inst, ..)| inst));
        plans.push(Plan {
            id,
            pre,
            header,
            preds: preds.clone(),
            counter,
            index,
            increment,
            step,
            reading,
            start,
            exts: reads,
            compares,
        });
    }
}

/// The counter one step on, when that is what the latch hands the header for it.
fn increment(
    func: &Func,
    cfg: &Cfg,
    latch: Block,
    header: Block,
    index: usize,
    counter: Value,
    step: i128,
) -> Option<Value> {
    let term = func.terminator(latch)?;
    let mut found = None;
    for call in func.successors(term) {
        if call.block != header {
            continue;
        }
        let arg = *func[call.args].get(index)?;
        if found.replace(arg).is_some_and(|had| had != arg) {
            return None;
        }
    }
    let increment = forwarded(func, cfg, found?);
    let Def::Result { inst, .. } = func[increment].def else { return None };
    if func[inst].opcode != Opcode::Add {
        return None;
    }
    let &[left, right] = &func[func[inst].args] else { return None };
    let by = |value: Value| crate::fold::constant(func, value).map(|(imm, ty)| imm.signed(ty));
    let counts =
        |of: Value, by: Option<i128>| forwarded(func, cfg, of) == counter && by == Some(step);
    (counts(left, by(right)) || counts(right, by(left))).then_some(increment)
}

/// The value a block parameter stands for, when there is only one way into its block.
///
/// The same reading `crate::scev` does, and it has to be the same, because a widening it vouches
/// for may be of the parameter a body block takes the counter as.
fn forwarded(func: &Func, cfg: &Cfg, value: Value) -> Value {
    let mut value = value;
    for _ in 0..FORWARD_LIMIT {
        let Def::Param { block, index } = func[value].def else { return value };
        let &[pred] = cfg.predecessors(block) else { return value };
        let Some(term) = func.terminator(pred) else { return value };
        let mut found = None;
        for call in func.successors(term) {
            if call.block != block {
                continue;
            }
            let Some(&arg) = func[call.args].get(index as usize) else { return value };
            if found.replace(arg).is_some_and(|had| had != arg) {
                return value;
            }
        }
        match found {
            Some(arg) if arg != value => value = arg,
            _ => return value,
        }
    }
    value
}

/// Writes the twin in and points everything the plan found at it.
fn apply(func: &mut Func, plan: &Plan, stats: &mut Stats) {
    let wide = Type::int(64);
    let Def::Result { inst: stepped, .. } = func[plan.increment].def else { return };
    let term = func.terminator(plan.pre).expect("a preheader ends in a jump to the header");
    let start = match plan.start.value {
        None => crate::ivopts::number(func, term, wide, plan.start.offset),
        Some(_) => {
            let reading = plan.start.read.map_or(plan.reading, |read| read.reading);
            crate::loop_delete::widened(func, term, plan.start, reading)
        }
    };

    let twin = func.append_param(plan.header, wide);
    // One step on, right behind the increment, so that it is there for everything the increment
    // is there for.
    let (by, step) = constant_after(func, stepped, wide, plan.step);
    let args = func.push_values(&[twin, step]);
    let data = InstData { args, flags: Flags::NSW, ..InstData::new(Opcode::Add) };
    let (next_at, next) = made_after(func, by, data, wide);
    for &block in &plan.preds {
        let term = func.terminator(block).expect("a block with a successor ends in a branch");
        let carry = if block == plan.pre { start } else { next };
        for at in func.target_list(term).iter() {
            let call = func[at];
            if call.block != plan.header {
                continue;
            }
            let args = func.append_arg(call.args, carry);
            func.set_block_call(at, BlockCall { args, ..call });
        }
    }

    // The twin this far on, at an instruction.
    let at = |func: &mut Func, inst: Inst, by: i128, after: bool| -> Value {
        if by == 0 {
            twin
        } else if after && by == plan.step {
            next
        } else {
            let by = crate::ivopts::number(func, inst, wide, by);
            crate::loop_delete::arith(func, inst, Opcode::Add, twin, by, wide)
        }
    };

    let mut forward: Map<Value, Value> = Map::default();
    for &(inst, by, after) in &plan.exts {
        let Some(result) = func[inst].first_result else { continue };
        let value = at(func, inst, by, after);
        forward.insert(result, value);
        stats.optimized(EXTENSION);
    }
    crate::uses::substitute(func, &forward);
    for &(inst, ..) in &plan.exts {
        func.remove_inst(inst);
    }

    let mut limits: Map<Value, Value> = Map::default();
    for &(inst, side, by, after) in &plan.compares {
        let other = func[func[inst].args][1 - side];
        let limit = match limits.get(&other) {
            Some(&limit) => limit,
            None => {
                let limit = widen(func, term, other, plan.reading);
                limits.insert(other, limit);
                limit
            }
        };
        let counter = at(func, inst, by, after);
        set_arg(func, inst, side, counter);
        set_arg(func, inst, 1 - side, limit);
        stats.optimized(COMPARED);
    }

    // Whatever else reads the counter or its increment reads the twin cut back down.
    let narrow = func[plan.counter].ty;
    let first = func.insts(plan.header).next().expect("a block ends in a terminator");
    let args = func.push_values(&[twin]);
    let cut = func.create_inst(
        InstData { args, ..InstData::new(Opcode::Trunc) },
        &[narrow],
        func.span(first),
    );
    func.insert_before(cut, first);
    let cut_value = func[cut].first_result.expect("one result was asked for");
    let args = func.push_values(&[next]);
    let (cut_next, cut_next_value) =
        made_after(func, next_at, InstData { args, ..InstData::new(Opcode::Trunc) }, narrow);
    let (counter, increment) = (plan.counter, plan.increment);
    let (mut read_cut, mut read_cut_next) = (false, false);
    for block in func.blocks().collect::<Vec<Block>>() {
        for inst in func.insts(block).collect::<Vec<Inst>>() {
            if inst == stepped || inst == cut || inst == cut_next {
                continue;
            }
            let mut swap = |value: Value| {
                if value == counter {
                    read_cut = true;
                    cut_value
                } else if value == increment {
                    read_cut_next = true;
                    cut_next_value
                } else {
                    value
                }
            };
            let args = func[inst].args;
            func.rewrite(args, &mut swap);
            for call in func.successors(inst).collect::<Vec<BlockCall>>() {
                // The way round still hands the old counter its increment, which is what leaves
                // the two of them reading nothing but each other.
                let round = call.block == plan.header && block != plan.pre;
                let mut position = 0;
                func.rewrite(call.args, |value| {
                    let here = position;
                    position += 1;
                    if round && here == plan.index { value } else { swap(value) }
                });
            }
        }
    }
    if !read_cut {
        func.remove_inst(cut);
    }
    if !read_cut_next {
        func.remove_inst(cut_next);
    }
    stats.optimized(WIDENED);
}

/// A value the loop does not change, widened in front of the loop the way the counter was.
fn widen(func: &mut Func, before: Inst, value: Value, reading: Reading) -> Value {
    let wide = Type::int(64);
    if let Some((imm, ty)) = crate::fold::constant(func, value) {
        let number = match reading {
            Reading::Signed => imm.signed(ty),
            Reading::Unsigned => imm.signed(ty) & ((1i128 << ty.bits()) - 1),
        };
        return crate::ivopts::number(func, before, wide, number);
    }
    let opcode = match reading {
        Reading::Signed => Opcode::SExt,
        Reading::Unsigned => Opcode::ZExt,
    };
    let args = func.push_values(&[value]);
    let span = func.span(before);
    let inst = func.create_inst(InstData { args, ..InstData::new(opcode) }, &[wide], span);
    func.insert_before(inst, before);
    func[inst].first_result.expect("one result was asked for")
}

/// One instruction with one result, put behind another one.
fn made_after(func: &mut Func, after: Inst, data: InstData, ty: Type) -> (Inst, Value) {
    let span = func.span(after);
    let inst = func.create_inst(data, &[ty], span);
    func.insert_after(inst, after);
    (inst, func[inst].first_result.expect("one result was asked for"))
}

/// A constant, put behind an instruction.
fn constant_after(func: &mut Func, after: Inst, ty: Type, value: i128) -> (Inst, Value) {
    let imm = func.add_imm(Imm::int(value, ty.lane()));
    let data = InstData { extra: Extra::Imm(imm), ..InstData::new(Opcode::IConst) };
    made_after(func, after, data, ty)
}

/// Replaces one operand of an instruction, by position.
fn set_arg(func: &mut Func, at: Inst, position: usize, value: Value) {
    let args = func[at].args;
    let mut seen = 0;
    func.rewrite(args, |had| {
        let here = seen;
        seen += 1;
        if here == position { value } else { had }
    });
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_base::hash::Map;
    use rucc_ir::{
        Block, Builder, Extra, Flags, Func, IntPred, MemInfo, MemOrder, Module, Opcode, Restrict,
        Signature, Type, Value, verify_func,
    };
    use rucc_target::{TargetInfo, Triple};

    use super::{COMPARED, EXTENSION, NO_TARGET, WIDENED, Widen};
    use crate::stats::Kind;
    use crate::{Analyses, Fuel, Pass, Stats};

    /// Runs the pass over the function as it stands, on a machine somebody priced.
    fn widen(func: &mut Func) -> Stats {
        Widen.run(func, &mut crate::machine::fixtures::analyses(), &mut Fuel::unlimited())
    }

    /// Insists the function is one the rest of the compiler may believe.
    fn sound(func: &Func, names: &mut Interner) {
        let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().unwrap());
        let module = Module::new(names.intern("t.c"), &target);
        if let Err(errors) = verify_func(&module, func, names) {
            panic!("{errors:#?}");
        }
    }

    /// An access that says as little about itself as one may.
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

    /// `for (int i = 0; i < n; i++) base[i] = i;` with the counter read wide the way `wide` says
    /// and its increment carrying `flags`, compared under `pred`.
    ///
    /// ```text
    /// entry(base, n):  jump head(0)
    /// head(i):         t = i pred n; br t -> body(i), out
    /// body(j):         store j -> base + wide(j) * 4; jump head(j + 1)
    /// ```
    fn counted(names: &mut Interner, wide: Opcode, flags: Flags, pred: IntPred) -> (Func, Block) {
        let int = Type::int(32);
        let signature = Signature::new().with_params(&[Type::PTR, int]);
        let mut func = Func::new(names.intern("f"), signature);
        let entry = func.create_block();
        let base = func.append_param(entry, Type::PTR);
        let limit = func.append_param(entry, int);
        let head = func.create_block();
        let body = func.create_block();
        let out = func.create_block();
        let i = func.append_param(head, int);
        let j = func.append_param(body, int);

        let mut build = Builder::new(&mut func, entry);
        let zero = build.iconst(int, 0);
        build.jump(head, &[zero]);

        let mut build = Builder::new(&mut func, head);
        let test = build.icmp(pred, i, limit);
        build.br_if(test, body, &[i], out, &[]);

        let mut build = Builder::new(&mut func, body);
        let at = build.unary(wide, j, Type::int(64));
        let four = build.iconst(Type::int(64), 4);
        let by = build.binary(Opcode::Mul, at, four, Flags::NSW);
        let addr = build.binary(Opcode::PtrAdd, base, by, Flags::NONE);
        build.store(j, addr, plain(), Flags::NONE);
        let one = build.iconst(int, 1);
        let next = build.binary(Opcode::Add, j, one, flags);
        build.jump(head, &[next]);

        Builder::new(&mut func, out).ret(&[]);
        (func, head)
    }

    /// What the function stores and where, given its arguments, worked out one instruction at a
    /// time. Nothing here wraps, so every integer is carried as the number it is.
    fn stores_given(func: &Func, given: &[i128]) -> Vec<(i128, i128)> {
        let mut values: Map<Value, i128> = Map::default();
        let mut block = func.entry().expect("a function with blocks in it");
        for (nth, &param) in func[block].params.iter().enumerate() {
            values.insert(param, given.get(nth).copied().unwrap_or(0));
        }
        let mut wrote = Vec::new();
        for _ in 0..10_000 {
            let mut end = None;
            for inst in func.insts(block) {
                if func.is_terminator(inst) {
                    end = Some(inst);
                    break;
                }
                let data = func[inst];
                let args: Vec<i128> = func[data.args].iter().map(|arg| values[arg]).collect();
                if data.opcode == Opcode::Store {
                    wrote.push((args[0], args[1]));
                    continue;
                }
                let result = data.first_result.expect("one result");
                let it = match data.opcode {
                    Opcode::IConst => {
                        let (imm, ty) =
                            crate::fold::constant(func, result).expect("a constant is one");
                        imm.signed(ty)
                    }
                    Opcode::ICmp => {
                        let Extra::IntPred(pred) = data.extra else {
                            panic!("a comparison carries its predicate");
                        };
                        i128::from(match pred {
                            IntPred::Slt | IntPred::Ult => args[0] < args[1],
                            IntPred::Ne => args[0] != args[1],
                            other => panic!("nothing here compares with {other:?}"),
                        })
                    }
                    Opcode::Add | Opcode::PtrAdd => args[0] + args[1],
                    Opcode::Mul => args[0] * args[1],
                    Opcode::SExt | Opcode::ZExt | Opcode::Trunc => args[0],
                    other => panic!("nothing here writes a {other:?}"),
                };
                values.insert(result, it);
            }
            let end = end.expect("every block here ends in a terminator");
            let data = func[end];
            let call = match data.opcode {
                Opcode::Jump => func.successors(end).next().expect("a jump has one edge"),
                Opcode::BrIf => {
                    let cond = values[&func[data.args][0]];
                    let mut edges = func.successors(end);
                    let then = edges.next().expect("a branch has two edges");
                    let other = edges.next().expect("a branch has two edges");
                    if cond == 0 { other } else { then }
                }
                Opcode::Return => return wrote,
                other => panic!("nothing here ends a block with a {other:?}"),
            };
            let carried: Vec<i128> = func[call.args].iter().map(|arg| values[arg]).collect();
            for (&param, arg) in func[call.block].params.iter().zip(carried) {
                values.insert(param, arg);
            }
            block = call.block;
        }
        panic!("the loop never ended");
    }

    /// How many instructions in the function are this one.
    fn how_many(func: &Func, opcode: Opcode) -> usize {
        func.blocks()
            .flat_map(|block| func.insts(block))
            .filter(|&i| func[i].opcode == opcode)
            .count()
    }

    /// What each of these instructions in the function reads, in the order they are laid out.
    fn read_by(func: &Func, opcode: Opcode) -> Vec<Value> {
        func.blocks()
            .flat_map(|block| func.insts(block))
            .filter(|&i| func[i].opcode == opcode)
            .map(|i| func[func[i].args][0])
            .collect()
    }

    /// The width of what the loop's test compares.
    fn tested_at(func: &Func, head: Block) -> u32 {
        let term = func.terminator(head).expect("the header ends in a branch");
        let cond = func[func[term].args][0];
        let rucc_ir::Def::Result { inst, .. } = func[cond].def else {
            panic!("the condition came out of a comparison");
        };
        func[func[func[inst].args][0]].ty.bits()
    }

    #[test]
    fn an_int_counter_indexing_memory_gets_a_twin_and_the_loop_does_the_same() {
        let mut names = Interner::new();
        let (mut func, head) = counted(&mut names, Opcode::SExt, Flags::NSW, IntPred::Slt);
        let before = stores_given(&func, &[1000, 7]);

        let stats = widen(&mut func);
        assert_eq!(stats.count(Kind::Optimized, WIDENED), 1);
        assert_eq!(stats.count(Kind::Optimized, EXTENSION), 1);
        assert_eq!(stats.count(Kind::Optimized, COMPARED), 1);
        let limit = func[func.entry().expect("the function has blocks")].params[1];
        assert_eq!(read_by(&func, Opcode::SExt), [limit], "the address reads the twin");
        assert_eq!(tested_at(&func, head), 64, "the test is asked of the twin");
        assert_eq!(stores_given(&func, &[1000, 7]), before);
        assert_eq!(stores_given(&func, &[1000, 0]), Vec::new());
        sound(&func, &mut names);
    }

    #[test]
    fn an_unsigned_test_on_a_counter_read_as_signed_is_left_alone() {
        // `i < n` unsigned is not `sext(i) < sext(n)` unsigned, so the test stays on the counter
        // and the counter stays, while the address still reads the twin.
        let mut names = Interner::new();
        let (mut func, head) = counted(&mut names, Opcode::SExt, Flags::NSW, IntPred::Ult);
        let before = stores_given(&func, &[1000, 5]);

        let stats = widen(&mut func);
        assert_eq!(stats.count(Kind::Optimized, EXTENSION), 1);
        assert_eq!(stats.count(Kind::Optimized, COMPARED), 0);
        assert_eq!(tested_at(&func, head), 32);
        assert_eq!(stores_given(&func, &[1000, 5]), before);
        sound(&func, &mut names);
    }

    #[test]
    fn a_counter_that_may_wrap_is_not_widened() {
        // No `nsw` on the increment, and a loop that runs until `i != n` fails goes past the
        // largest `int` and round to `n` when `n` is negative, so `sext(i)` is not known to go up
        // by one each time and nothing about it is a sequence a twin could be. With `i < n` the
        // exit test alone says the increment cannot wrap.
        let mut names = Interner::new();
        let (mut func, _) = counted(&mut names, Opcode::SExt, Flags::NONE, IntPred::Ne);
        let stats = widen(&mut func);
        assert!(!stats.changed());
        assert_eq!(how_many(&func, Opcode::SExt), 1);
    }

    #[test]
    fn an_unpriced_machine_is_left_alone() {
        let mut names = Interner::new();
        let (mut func, _) = counted(&mut names, Opcode::SExt, Flags::NSW, IntPred::Slt);
        let mut an = Analyses::new(crate::machine::Machine::unknown());
        let stats = Widen.run(&mut func, &mut an, &mut Fuel::unlimited());
        assert_eq!(stats.count(Kind::Missed, NO_TARGET), 1);
        assert!(!stats.changed());
    }
}
