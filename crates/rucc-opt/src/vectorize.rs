//! A loop over `int` arrays done four elements at a time, where that costs nothing to prove.
//!
//! Design: `spec/optimizer/32-vectorization.md`, section 32.4 for which loops and why so few.
//!
//! This is the very cheap model `gcc -O2` runs and nothing beyond it. The loop has to run a known
//! number of times that four divides, so there are no iterations left over and no scalar copy of
//! the loop to keep. Every access has to be to consecutive `int`, a step of four bytes, so each
//! one is a single sixteen byte load or store. Two accesses that might touch the same bytes are
//! either the same address on every iteration or proved apart by the alias analysis, never by a
//! test at run time. Anything the loop does that is not one of those accesses, a lanewise
//! operator on what they read, or arithmetic on the counter for the addresses, and the loop is
//! left alone.
//!
//! The loop that asked for it is the one in `pg_checksum_block`, which gcc does in the vector
//! registers and which is at the top of the Postgres profile when rucc does it a lane at a time
//! (tamnd/rucc#1994):
//!
//! ```c
//! for (j = 0; j < N_SUMS; j++)
//!     CHECKSUM_COMP(sums[j], page->data[i][j]);
//! ```
//!
//! Thirty two lanes, a load of each array, a `xor`, a multiply by a constant, a shift by a
//! constant and a store, which this turns into eight trips of the same on four lanes at once.
//!
//! # Which loops
//!
//! An innermost loop of two blocks in the shape [`crate::canon`] leaves: a header holding the
//! whole body and ending in the exit test, and a latch that only jumps back. The header has one
//! parameter, the counter, which starts at a number and goes up by one, and nothing else is
//! carried round. Nothing the loop computes is read after it, since a value read outside would be
//! the last lane's and the loop no longer has one of those on its own.
//!
//! # What changes
//!
//! Each load, store and operator on what was loaded becomes the same instruction on a vector of
//! four `int`, a constant operand becomes a `splat` of it in front of the loop, and the scalar
//! ones are taken out. The counter goes up by four and the exit test becomes a test against where
//! it stops, written from the count rather than adapted from the test that was there, so that how
//! the program spelled its test does not matter. Everything else, the address arithmetic on the
//! counter included, is left as it was: every address is the same function of the counter it was
//! before, so on the iteration that starts at lane `i` it is the address of lane `i`, which is
//! where the sixteen bytes start.
//!
//! The accesses lose their type-based aliasing node. A vector of four `int` written here may be
//! read back a lane at a time after the loop and the two have to be seen to overlap, which is the
//! same reason the front end gives its own vector accesses none.
//!
//! # Which level
//!
//! `-O2` and `-O3`, where gcc runs the same model, and only where the vector registers are there,
//! which is the question [`crate::outside::Outside::vectors`] answers for [`crate::lanes`] too.

use rucc_base::hash::{Map, Set};
use rucc_ir::{
    Block, BlockCall, Extra, Flags, Func, Imm, Inst, InstData, IntPred, MemInfo, MemOrder, Opcode,
    Type, Value,
};

use crate::alias::Alias;
use crate::cfg::Cfg;
use crate::loops::{LoopId, Loops};
use crate::scev::{Bound, Count, Invariant, Scev};
use crate::{Analyses, Analysis, Fuel, Pass, Preserved, Stats};

/// What this pass is called, for the lists in [`crate::pipeline`] that name it.
pub const NAME: &str = "vectorize";

/// How many `int` a vector register holds.
const LANES: i128 = 4;

/// How many bytes one `int` is, which is the step every access has to take.
const WIDTH: i128 = 4;

/// The fewest times a loop has to run to be worth doing four at a time.
///
/// Two trips of the vector loop. A loop of four is one trip, which [`crate::unroll`] has already
/// had the chance to copy out into a straight line.
const FEWEST: i128 = 2 * LANES;

const VECTORIZED: &str = "loop done four int at a time in the vector registers";
const SHAPE: &str = "loop left as it was, it is not one header holding the body and one latch";
const CARRIED: &str = "loop left as it was, it carries something round besides its counter";
const COUNTER: &str =
    "loop left as it was, its counter does not start at a number and go up by one";
const NO_COUNT: &str = "loop left as it was, how many times it runs is not a number known here";
const LEFT_OVER: &str =
    "loop left as it was, four does not divide how many times it runs or it runs too few";
const ACCESS: &str =
    "loop left as it was, an access is not to consecutive int or is volatile or atomic";
const OPERATOR: &str = "loop left as it was, something it does to what it loads has no vector form";
const OTHER: &str = "loop left as it was, it does something besides loads, stores and arithmetic";
const OVERLAP: &str = "loop left as it was, two of its accesses may touch the same bytes";
const NOTHING: &str = "loop left as it was, it stores nothing done four at a time";
const ESCAPES: &str = "loop left as it was, a value it defines is read outside it";
const NO_FUEL: &str = "loop left as it was, the pass ran out of fuel";

/// The pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Vectorize;

impl Pass for Vectorize {
    fn name(&self) -> &'static str {
        NAME
    }

    fn describe(&self) -> &'static str {
        "a loop over int arrays that runs a multiple of four times is done four at a time"
    }

    fn preserves(&self) -> Preserved {
        // Instructions change inside blocks and no edge moves, but the loop goes round a quarter
        // as often as it did.
        Preserved::ALL
            .without(Analysis::Liveness)
            .without(Analysis::Pressure)
            .without(Analysis::Frequencies)
    }

    fn run(&self, func: &mut Func, an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        if func.entry().is_none() || !an.outside().vectors() {
            return stats;
        }
        let plans = {
            let cfg = an.cfg(func);
            let loops = an.loops(func);
            let mut scev = Scev::new(func, cfg, loops);
            let mut oracle = Alias::new(func, an.outside());
            let mut plans = Vec::new();
            for id in loops.all() {
                if !loops.children(id).is_empty() {
                    continue;
                }
                match consider(func, cfg, loops, &mut scev, &mut oracle, id) {
                    Ok(plan) => plans.push(plan),
                    Err(why) => stats.missed(why),
                }
            }
            plans
        };
        for plan in &plans {
            if !fuel.take() {
                stats.missed(NO_FUEL);
                break;
            }
            apply(func, plan);
            stats.optimized(VECTORIZED);
        }
        stats
    }
}

/// One loop to do four at a time, worked out against the function as it stands.
#[derive(Debug)]
struct Plan {
    /// The block in front of the loop, where the constants spread across a vector go.
    preheader: Block,
    /// The latch, whose jump back carries the counter.
    latch: Block,
    /// The exit test at the end of the header.
    branch: Inst,
    /// Whether the test stays in the loop when its condition holds.
    stay_when_true: bool,
    /// The header's one parameter.
    counter: Value,
    /// What the counter is when the loop is over.
    end: i128,
    /// The instructions done four lanes at a time, in the order they run.
    whole: Vec<Inst>,
}

/// One load or store of the loop, and the address it starts at.
#[derive(Clone, Copy, Debug)]
struct Sweep {
    inst: Inst,
    store: bool,
    base: Invariant,
}

/// Whether this loop can be done four at a time, and why not when it cannot.
fn consider(
    func: &Func,
    cfg: &Cfg,
    loops: &Loops,
    scev: &mut Scev<'_>,
    oracle: &mut Alias<'_>,
    id: LoopId,
) -> Result<Plan, &'static str> {
    let header = loops.header(id);
    let [latch] = loops.latches(id) else { return Err(SHAPE) };
    let [exit] = loops.exits(id) else { return Err(SHAPE) };
    let latch = *latch;
    if latch == header || exit.from != header || loops.blocks(id).len() != 2 {
        return Err(SHAPE);
    }
    let back = func.terminator(latch).ok_or(SHAPE)?;
    if func[back].opcode != Opcode::Jump || func.insts(latch).count() != 1 {
        return Err(SHAPE);
    }
    let branch = func.terminator(header).ok_or(SHAPE)?;
    if func[branch].opcode != Opcode::BrIf {
        return Err(SHAPE);
    }
    let calls: Vec<BlockCall> = func.successors(branch).collect();
    let [then_call, else_call] = calls[..].try_into().map_err(|_| SHAPE)?;
    let stay_when_true = match (then_call.block == latch, else_call.block == latch) {
        (true, false) => true,
        (false, true) => false,
        _ => return Err(SHAPE),
    };
    if !func[then_call.args].is_empty() || !func[else_call.args].is_empty() {
        return Err(SHAPE);
    }
    let preheader = loops.preheader(cfg, id).ok_or(SHAPE)?;

    let &[counter] = &func[header].params[..] else { return Err(CARRIED) };
    let chrec = scev.evolution(id, counter).chrec().ok_or(COUNTER)?;
    let start = chrec.base.as_number().ok_or(COUNTER)?;
    if chrec.step.as_number() != Some(1) || !chrec.ty.is_int() || chrec.ty.is_vector() {
        return Err(COUNTER);
    }
    // The count is how often the back edge is taken, which is one less than how often the body
    // runs. `crate::unroll` has the same off by one written out at more length.
    let times = match scev.bound(id).as_ref().and_then(Bound::under_undefined_overflow) {
        Some(Count::Exact(count)) => i128::try_from(count).ok().and_then(|n| n.checked_add(1)),
        _ => None,
    }
    .ok_or(NO_COUNT)?;
    if times % LANES != 0 || times < FEWEST {
        return Err(LEFT_OVER);
    }
    let end = start.checked_add(times).ok_or(NO_COUNT)?;

    let (whole, accesses) = classify(func, scev, id, header, branch)?;
    if !accesses.iter().any(|access| access.store) {
        return Err(NOTHING);
    }
    apart(oracle, &accesses)?;
    if escapes(func, loops, id, header) {
        return Err(ESCAPES);
    }
    Ok(Plan { preheader, latch, branch, stay_when_true, counter, end, whole })
}

/// The instructions of the header that become vector ones, and the accesses among them.
///
/// A value is whole once it is read from memory four lanes at a time, and anything that reads a
/// whole value has to be something with a vector form that makes it whole too. Everything else
/// stays scalar, which is fine for the counter and the addresses and nothing else, since a scalar
/// that changes from one iteration to the next would be wanted at four values at once.
fn classify(
    func: &Func,
    scev: &mut Scev<'_>,
    id: LoopId,
    header: Block,
    branch: Inst,
) -> Result<(Vec<Inst>, Vec<Sweep>), &'static str> {
    let int = Type::int(32);
    let mut whole: Set<Value> = Set::default();
    let mut order = Vec::new();
    let mut accesses = Vec::new();
    for inst in func.insts(header) {
        if inst == branch {
            break;
        }
        let data = func[inst];
        let args = &func[data.args];
        let reads_whole = args.iter().any(|arg| whole.contains(arg));
        let result = data.first_result;
        match data.opcode {
            Opcode::Load => {
                let result = result.ok_or(ACCESS)?;
                if func[result].ty != int || !plain(func, inst) {
                    return Err(ACCESS);
                }
                let base = consecutive(scev, id, args[0]).ok_or(ACCESS)?;
                whole.insert(result);
                order.push(inst);
                accesses.push(Sweep { inst, store: false, base });
            }
            Opcode::Store => {
                let &[value, address] = args else { return Err(ACCESS) };
                if !whole.contains(&value) {
                    return Err(OPERATOR);
                }
                if whole.contains(&address) || !plain(func, inst) {
                    return Err(ACCESS);
                }
                let base = consecutive(scev, id, address).ok_or(ACCESS)?;
                order.push(inst);
                accesses.push(Sweep { inst, store: true, base });
            }
            Opcode::Add | Opcode::Sub | Opcode::Mul | Opcode::And | Opcode::Or | Opcode::Xor
                if reads_whole =>
            {
                let result = result.ok_or(OPERATOR)?;
                let constants =
                    args.iter().all(|arg| whole.contains(arg) || constant(func, *arg).is_some());
                if func[result].ty != int || !constants {
                    return Err(OPERATOR);
                }
                whole.insert(result);
                order.push(inst);
            }
            Opcode::Shl | Opcode::LShr | Opcode::AShr if reads_whole => {
                let result = result.ok_or(OPERATOR)?;
                let &[shifted, by] = args else { return Err(OPERATOR) };
                let fits = constant(func, by).is_some_and(|by| (0..32).contains(&by));
                if func[result].ty != int || !whole.contains(&shifted) || !fits {
                    return Err(OPERATOR);
                }
                whole.insert(result);
                order.push(inst);
            }
            _ if reads_whole => return Err(OPERATOR),
            opcode if scalar(opcode) => {}
            _ => return Err(OTHER),
        }
    }
    Ok((order, accesses))
}

/// Whether an instruction that reads no whole value may stay as it is in the loop.
///
/// Arithmetic with no effect and no way to trap, which is what the counter and the addresses are
/// worked out with. A division is not here because it can trap, and a call or any other access to
/// memory is not here because this pass has not asked whether it overlaps anything.
fn scalar(opcode: Opcode) -> bool {
    matches!(
        opcode,
        Opcode::IConst
            | Opcode::GlobalAddr
            | Opcode::Add
            | Opcode::Sub
            | Opcode::Mul
            | Opcode::And
            | Opcode::Or
            | Opcode::Xor
            | Opcode::Shl
            | Opcode::LShr
            | Opcode::AShr
            | Opcode::ZExt
            | Opcode::SExt
            | Opcode::Trunc
            | Opcode::PtrAdd
            | Opcode::ICmp
    )
}

/// Whether an access is an ordinary one, neither volatile nor atomic and in a function that does
/// not thread memory through its instructions.
fn plain(func: &Func, inst: Inst) -> bool {
    let data = func[inst];
    let Extra::Mem(mem) = data.extra else { return false };
    func[mem].order == MemOrder::NotAtomic
        && !data.flags.contains(Flags::VOLATILE)
        && func.mem_in(inst).is_none()
}

/// Where an address starts, when it moves on by one `int` each time round.
fn consecutive(scev: &mut Scev<'_>, id: LoopId, address: Value) -> Option<Invariant> {
    let chrec = scev.evolution(id, address).chrec()?;
    (chrec.step.as_number() == Some(WIDTH)).then_some(chrec.base)
}

/// The number a value is, when an `iconst` made it.
fn constant(func: &Func, value: Value) -> Option<i128> {
    crate::fold::constant(func, value).map(|(imm, ty)| imm.signed(ty))
}

/// Whether every store is apart from every other access, or at the same address as it.
///
/// The same address on every iteration is the read and the write of `sums[j]`, which four lanes
/// at a time do in the same order the one lane did. Any other distance between two accesses to
/// one array is refused, even the ones that would be safe, because the question is cheap only
/// when the answer is that simple. Two accesses that are not measured from the same thing are
/// asked of the alias analysis as references anywhere in their objects, since the loop sweeps
/// across them.
fn apart(oracle: &mut Alias<'_>, accesses: &[Sweep]) -> Result<(), &'static str> {
    for (at, one) in accesses.iter().enumerate() {
        for other in &accesses[at + 1..] {
            if !one.store && !other.store {
                continue;
            }
            if one.base.alike(other.base) {
                if one.base.offset() == other.base.offset() {
                    continue;
                }
                return Err(OVERLAP);
            }
            let anywhere = |access: &Sweep| {
                let found = if access.store {
                    oracle.writes(access.inst)
                } else {
                    oracle.reads(access.inst)
                };
                found.map(|reference| crate::alias::Access {
                    offset: None,
                    size: None,
                    ..reference
                })
            };
            let (Some(a), Some(b)) = (anywhere(one), anywhere(other)) else {
                return Err(OVERLAP);
            };
            if !oracle.query(&a, &b).is_no() {
                return Err(OVERLAP);
            }
        }
    }
    Ok(())
}

/// Whether anything outside the loop reads a value the header defines.
fn escapes(func: &Func, loops: &Loops, id: LoopId, header: Block) -> bool {
    let mut defined: Set<Value> = func[header].params.iter().copied().collect();
    for inst in func.insts(header) {
        defined.extend(func[inst].results());
    }
    func.blocks().filter(|&block| !loops.contains(id, block)).any(|block| {
        func.insts(block).any(|inst| {
            func[func[inst].args].iter().any(|arg| defined.contains(arg))
                || func
                    .successors(inst)
                    .any(|call| func[call.args].iter().any(|arg| defined.contains(arg)))
        })
    })
}

/// Rewrites the loop four lanes at a time.
fn apply(func: &mut Func, plan: &Plan) {
    let vector = Type::vector(Type::int(32), 4);
    let ahead = func.terminator(plan.preheader).expect("a preheader ends in a jump");
    let mut made: Map<Value, Value> = Map::default();
    let mut splats: Map<i128, Value> = Map::default();
    for &inst in &plan.whole {
        let data = func[inst];
        let args: Vec<Value> = func[data.args].to_vec();
        match data.opcode {
            Opcode::Load | Opcode::Store => {
                let Extra::Mem(mem) = data.extra else { unreachable!("an access has its info") };
                // The size of a load or a store is its type's, and the padding a store owns is a
                // member's, which none of the four is on its own.
                let info = MemInfo { size: 0, tbaa: None, owns: 0, ..func[mem] };
                let mem = func.add_mem(info);
                let operands: Vec<Value> =
                    args.iter().map(|arg| made.get(arg).copied().unwrap_or(*arg)).collect();
                let operands = func.push_values(&operands);
                let new = InstData {
                    args: operands,
                    extra: Extra::Mem(mem),
                    ..InstData::new(data.opcode)
                };
                let new = InstData { flags: data.flags, ..new };
                if data.opcode == Opcode::Load {
                    let value = emit(func, inst, new, vector);
                    made.insert(func[inst].first_result.expect("a load has a result"), value);
                } else {
                    let span = func.span(inst);
                    let store = func.create_inst(new, &[], span);
                    func.insert_before(store, inst);
                }
            }
            opcode => {
                let mut operands = Vec::with_capacity(args.len());
                for arg in args {
                    let operand = match made.get(&arg) {
                        Some(&value) => value,
                        None => {
                            let number = constant(func, arg).expect("a constant, by classify");
                            *splats.entry(number).or_insert_with(|| {
                                let imm = func.add_imm(Imm::int(number, Type::int(32)));
                                let data = InstData {
                                    extra: Extra::Imm(imm),
                                    ..InstData::new(Opcode::Splat)
                                };
                                emit(func, ahead, data, vector)
                            })
                        }
                    };
                    operands.push(operand);
                }
                let operands = func.push_values(&operands);
                let value =
                    emit(func, inst, InstData { args: operands, ..InstData::new(opcode) }, vector);
                made.insert(func[inst].first_result.expect("an operator has a result"), value);
            }
        }
    }
    // Every reader of a scalar taken out here is itself one of the ones taken out, which is what
    // classify checked, so last first leaves nothing reading a value that is gone.
    for &inst in plan.whole.iter().rev() {
        func.remove_inst(inst);
    }

    // Four more each time round and a test against where it stops, in place of whatever the loop
    // tested before.
    let ty = func[plan.counter].ty;
    let number = |func: &mut Func, value: i128| {
        let imm = func.add_imm(Imm::int(value, ty));
        emit(
            func,
            plan.branch,
            InstData { extra: Extra::Imm(imm), ..InstData::new(Opcode::IConst) },
            ty,
        )
    };
    let four = number(func, LANES);
    let args = func.push_values(&[plan.counter, four]);
    let next = emit(func, plan.branch, InstData { args, ..InstData::new(Opcode::Add) }, ty);
    let end = number(func, plan.end);
    let pred = if plan.stay_when_true { IntPred::Ne } else { IntPred::Eq };
    let args = func.push_values(&[next, end]);
    let test = emit(
        func,
        plan.branch,
        InstData { args, extra: Extra::IntPred(pred), ..InstData::new(Opcode::ICmp) },
        Type::I1,
    );
    let condition = func[plan.branch].args;
    func.rewrite(condition, |_| test);
    let back = func.terminator(plan.latch).expect("the latch ends in its jump");
    let call = func.successors(back).next().expect("the latch jumps to the header");
    func.rewrite(call.args, |_| next);
}

/// Makes an instruction with one result and puts it in front of another.
fn emit(func: &mut Func, before: Inst, data: InstData, ty: Type) -> Value {
    let span = func.span(before);
    let inst = func.create_inst(data, &[ty], span);
    func.insert_before(inst, before);
    func[inst].first_result.expect("one result was asked for")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rucc_base::Interner;

    use super::Vectorize;
    use crate::outside::Outside;
    use crate::{Fuel, Pass};

    const HEAD: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "x86_64-unknown-linux-gnu"
target datalayout = "e-p:64:64-i64:64-f80:128-S128"
"#;

    /// The module that text is, with the pass run over every function in it, on a build with the
    /// vector registers or without them.
    fn run(body: &str, vectors: bool) -> String {
        let mut names = Interner::new();
        let text = format!("{HEAD}{body}");
        let mut module = rucc_ir::parse(&text, &mut names).expect("the fixture parses");
        let outside = Arc::new(Outside::of(&module).with_vectors(vectors));
        let ids: Vec<_> = module.funcs().collect();
        for id in ids {
            if module[id].is_declaration() {
                continue;
            }
            let mut an = crate::machine::fixtures::analyses().about(Arc::clone(&outside));
            Vectorize.run(&mut module[id], &mut an, &mut Fuel::unlimited());
        }
        if let Err(errors) = rucc_ir::verify(&module, &names) {
            panic!("the pass left invalid IR, {errors:?}\n{}", rucc_ir::print(&module, &names));
        }
        rucc_ir::print(&module, &names)
    }

    fn count(out: &str, what: &str) -> usize {
        out.lines().filter(|line| line.contains(what)).count()
    }

    /// One row of `pg_checksum_block`, the way it reaches the pass: the sums are a local, the row
    /// comes in as a pointer, and the counter is read at the width of an address.
    const CHECKSUM: &str = r"
func @f(ptr, ptr) -> i32, linkage(external) {
block0(%0: ptr, %1: ptr):
    %2 = alloca, size 128, align 16
    %3 = iconst.i32 0
    %4 = iconst.i64 4
    jump block1(%3)

block1(%5: i32):
    %6 = zext.i64 %5
    %7 = iconst.i64 2
    %8 = shl.nsw %6, %7
    %9 = ptr_add %2, %8
    %10 = load.i32 %9, align 4
    %11 = ptr_add %0, %8
    %12 = load.i32 %11, align 4
    %13 = xor %10, %12
    %14 = iconst.i32 16777619
    %15 = mul %13, %14
    %16 = iconst.i32 17
    %17 = lshr %13, %16
    %18 = xor %15, %17
    store %18 -> %9, align 4
    %19 = iconst.i32 1
    %20 = add %5, %19
    %21 = iconst.i32 32
    %22 = icmp ult %20, %21
    br_if %22, block3, block2

block2:
    %23 = load.i32 %2, align 4
    return %23

block3:
    jump block1(%20)
}
";

    /// Untouched, which is the text with no vector in it at all.
    fn untouched(out: &str) {
        assert_eq!(count(out, "i32x4"), 0, "{out}");
    }

    #[test]
    fn the_checksum_loop_is_done_four_at_a_time() {
        let out = run(CHECKSUM, true);
        assert_eq!(count(&out, "= load.i32x4"), 2, "{out}");
        assert_eq!(count(&out, "= load.i32 "), 1, "only the one after the loop is left\n{out}");
        assert_eq!(count(&out, "= splat.i32x4 16777619"), 1, "{out}");
        assert_eq!(count(&out, "= splat.i32x4 17"), 1, "{out}");
        assert_eq!(count(&out, "= icmp ne"), 1, "{out}");
        assert!(out.contains("iconst.i32 4\n"), "the counter goes up by four\n{out}");
        // The splats are in front of the loop, not in it.
        let entry = out.split("block1(").next().expect("the entry block");
        assert_eq!(count(entry, "splat.i32x4"), 2, "{out}");
    }

    #[test]
    fn without_the_vector_registers_the_loop_stays() {
        untouched(&run(CHECKSUM, false));
    }

    /// Thirty is not a multiple of four, and there is no scalar loop to finish it off.
    #[test]
    fn a_count_four_does_not_divide_stays() {
        untouched(&run(&CHECKSUM.replace("iconst.i32 32", "iconst.i32 30"), true));
    }

    /// `sums[j + 1]` read and `sums[j]` written are a lane apart. This one would be right four at
    /// a time, but the pass does not measure distances, so it leaves every one of them alone.
    #[test]
    fn a_neighbour_written_and_read_stays() {
        let next = CHECKSUM.replace("%11 = ptr_add %0, %8", "%11 = ptr_add %9, %4");
        untouched(&run(&next, true));
    }

    /// Two arrays that come in as pointers may be the same array a lane apart.
    #[test]
    fn two_pointers_that_may_overlap_stay() {
        let both = CHECKSUM.replace("%9 = ptr_add %2, %8", "%9 = ptr_add %1, %8");
        untouched(&run(&both, true));
    }

    /// The counter itself under a lanewise operator would have to be four counters.
    #[test]
    fn the_counter_used_as_a_lane_stays() {
        let used = CHECKSUM.replace("%13 = xor %10, %12", "%13 = xor %10, %5");
        untouched(&run(&used, true));
    }

    /// The last lane's value read after the loop is not one the vector loop has on its own.
    #[test]
    fn a_value_read_after_the_loop_stays() {
        let read = CHECKSUM.replace("%23 = load.i32 %2, align 4\n    return %23", "return %18");
        untouched(&run(&read, true));
    }

    /// A volatile access has to happen once per lane.
    #[test]
    fn a_volatile_access_stays() {
        let volatile = CHECKSUM.replace("%12 = load.i32 %11", "%12 = load.i32.volatile %11");
        untouched(&run(&volatile, true));
    }
}
