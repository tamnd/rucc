//! A vector built a lane at a time out of the lanes of one other vector, rebuilt as one operation
//! on the whole of it.
//!
//! `<emmintrin.h>` writes `_mm_slli_epi32` and its relatives as a loop over the lanes, and
//! `_mm_shuffle_epi32` as a list of lane subscripts. Once the unroller and the scalar replacement
//! have been through, each of them is a chain of `insertlane` filling every lane of a vector, and
//! what goes in each lane is either the same lane of one vector shifted by one constant or some
//! lane of one vector as it is. The first is that vector shifted by a splat of the constant, and
//! the second is a `shuffle` of it, which the back end writes as one `pslld` or `pshufd` where
//! the chain was a move out and a move back in for every lane. That is the other half of
//! tamnd/rucc#2320, after `crate::sroa` took the vector out of memory.
//!
//! Only on a build where the vector registers are there, the same question `crate::sroa` asks,
//! since the shift and the shuffle are only worth having if the back end keeps the vector in one.
//! That is x86-64 with SSE2, and wasm32 with `-msimd128`, where the shift is one `i32x4.shl` or
//! its relatives and the shuffle is one `i8x16.shuffle`.
//! A shift by the width of a lane or more is left as it is: C gives no answer for it and the
//! machine gives zero, so the rewrite would be choosing one.
//!
//! The second pass here, [`Rejoin`], is for AArch64 with the vector registers and does the
//! opposite kind of cleanup. The calling convention there passes a vector as two `long` halves,
//! so an inlined intrinsic of `<arm_neon.h>` leaves its vector taken apart and put back together
//! around the one operator it is. That pass puts the halves back into one vector, and their loads
//! and stores into one load or store of it, so the back end keeps the vector in one `v` register
//! (tamnd/rucc#3775).

use rucc_base::Idx;
use rucc_base::hash::{Map, Set};
use rucc_ir::{
    Block, BlockCall, Def, Extra, Flags, Func, Imm, Inst, InstData, MemInfo, MemOrder, Opcode,
    Restrict, Shuffle, Type, Value,
};

use crate::uses::substitute;
use crate::{Analyses, Analysis, Fuel, Pass, Phase, Preserved, Stats};

/// What this pass is called, for the lists in [`crate::pipeline`] that name it.
pub const NAME: &str = "lanes";

/// Recorded for a chain that became a shift of the whole vector.
const SHIFTED: &str = "vector built lane by lane rebuilt as one shift";

/// Recorded for a chain that became a shuffle, or the vector it was a copy of.
const SHUFFLED: &str = "vector built lane by lane rebuilt as one shuffle";

/// Recorded for a chain that would have been rebuilt if there had been fuel for it.
const NO_FUEL: &str = "vector built lane by lane left as it was, the pass ran out of fuel";

/// The pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lanes;

impl Pass for Lanes {
    fn name(&self) -> &'static str {
        NAME
    }

    fn describe(&self) -> &'static str {
        "a vector filled lane by lane from one other vector becomes one shift or shuffle of it"
    }

    fn phase(&self) -> Phase {
        Phase::Scalar
    }

    fn preserves(&self) -> Preserved {
        // Instructions are added inside blocks and no edge moves.
        Preserved::ALL.without(Analysis::Liveness)
    }

    fn run(&self, func: &mut Func, an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        if !an.outside().vectors() && !an.outside().simd128() {
            return stats;
        }
        // wasm has an arithmetic shift of `long` lanes, which SSE2 does not.
        let long_ashr = an.outside().simd128();
        let mut inside: Set<Inst> = Set::default();
        let mut forward: Map<Value, Value> = Map::default();
        for block in func.blocks().collect::<Vec<Block>>() {
            // Bottom up, so the first `insertlane` met is the last of its chain and the ones under
            // it are marked before the walk reaches them.
            for inst in func.insts_backwards(block).collect::<Vec<Inst>>() {
                if inside.contains(&inst) || func[inst].opcode != Opcode::InsertLane {
                    continue;
                }
                let Some((ty, lanes, chain)) = chain(func, inst) else { continue };
                inside.extend(chain);
                let Some(made) = Made::of(func, ty, &lanes, long_ashr) else { continue };
                if !fuel.take() {
                    stats.missed(NO_FUEL);
                    continue;
                }
                let result = func[inst].first_result.expect("an insertlane has a result");
                let value = made.build(func, inst, ty);
                forward.insert(result, value);
                stats.optimized(match made {
                    Made::Shift { .. } => SHIFTED,
                    Made::Shuffle { .. } => SHUFFLED,
                });
            }
        }
        if !forward.is_empty() {
            substitute(func, &forward);
        }
        stats
    }
}

/// The value `value` is, seen through bitcasts from other vectors of the same sixteen bytes.
fn through(func: &Func, mut value: Value) -> Value {
    while let Some(inst) = made_by(func, value) {
        let data = &func[inst];
        let &[from] = &func[data.args] else { break };
        if data.opcode != Opcode::Bitcast || !func[from].ty.is_vector() {
            break;
        }
        value = from;
    }
    value
}

/// The instruction that computed a value, if one did.
fn made_by(func: &Func, value: Value) -> Option<Inst> {
    match func[value].def {
        Def::Result { inst, .. } => Some(inst),
        _ => None,
    }
}

/// Whether the type is a vector of four `int` or two `long`, the two shapes the back end keeps
/// in a register.
fn whole(ty: Type) -> bool {
    ty.is_vector()
        && ty.lane().is_int()
        && matches!((ty.lanes(), ty.lane().bits()), (4, 32) | (2, 64))
}

/// The chain of `insertlane` ending at `top`, as the vector type, the value each lane was given
/// last, and the instructions in it, when it gives every lane a value.
fn chain(func: &Func, top: Inst) -> Option<(Type, Vec<Value>, Vec<Inst>)> {
    let ty = func[func[top].first_result?].ty;
    if !whole(ty) {
        return None;
    }
    let count = usize::try_from(ty.lanes()).ok()?;
    let mut lanes: Vec<Option<Value>> = vec![None; count];
    let mut chain = Vec::new();
    let mut at = top;
    loop {
        let data = &func[at];
        let Extra::Lane(lane) = data.extra else { return None };
        let &[into, value] = &func[data.args] else { return None };
        // Walking down from the last write, so a lane already given a value keeps it.
        let slot = lanes.get_mut(usize::from(lane))?;
        slot.get_or_insert(value);
        chain.push(at);
        if lanes.iter().all(Option::is_some) {
            return Some((ty, lanes.into_iter().map(Option::unwrap).collect(), chain));
        }
        let below = made_by(func, through(func, into))?;
        let below_data = &func[below];
        if below_data.opcode != Opcode::InsertLane || func[below_data.first_result?].ty != ty {
            return None;
        }
        at = below;
    }
}

/// What a full chain is the same as.
#[derive(Clone, Copy, Debug)]
enum Made {
    /// Every lane of `of` shifted by `by`.
    Shift { opcode: Opcode, of: Value, by: u32 },
    /// The lanes of `of` in the order `picks` says.
    Shuffle { of: Value, picks: Shuffle },
}

impl Made {
    fn of(func: &Func, ty: Type, lanes: &[Value], long_ashr: bool) -> Option<Self> {
        Self::shift(func, ty, lanes, long_ashr).or_else(|| Self::shuffle(func, ty, lanes))
    }

    /// Lane `i` of the answer is lane `i` of one vector shifted by one constant. `long_ashr` is
    /// whether the machine has an arithmetic shift of `long` lanes.
    fn shift(func: &Func, ty: Type, lanes: &[Value], long_ashr: bool) -> Option<Self> {
        let bits = ty.lane().bits();
        let mut found: Option<(Opcode, Value, u128)> = None;
        for (at, &value) in lanes.iter().enumerate() {
            let data = &func[made_by(func, value)?];
            if !matches!(data.opcode, Opcode::Shl | Opcode::LShr | Opcode::AShr) {
                return None;
            }
            let &[lane, count] = &func[data.args] else { return None };
            let (of, from) = extracted(func, lane)?;
            if usize::from(from) != at || func[of].ty != ty {
                return None;
            }
            let (count, _) = crate::fold::constant(func, count)?;
            let count = count.unsigned();
            let this = (data.opcode, of, count);
            if *found.get_or_insert(this) != this {
                return None;
            }
        }
        let (opcode, of, by) = found?;
        // There is no arithmetic shift of a `long` lane on x86-64 before AVX-512.
        let long = ty.lane().bits() == 64;
        if by >= u128::from(bits) || opcode == Opcode::AShr && long && !long_ashr {
            return None;
        }
        Some(Self::Shift { opcode, of, by: u32::try_from(by).ok()? })
    }

    /// Lane `i` of the answer is some lane of one vector as it is.
    fn shuffle(func: &Func, ty: Type, lanes: &[Value]) -> Option<Self> {
        let mut of = None;
        let mut picks = Vec::with_capacity(lanes.len());
        for &value in lanes {
            let (from, lane) = extracted(func, value)?;
            if func[from].ty != ty || *of.get_or_insert(from) != from {
                return None;
            }
            picks.push(lane);
        }
        Some(Self::Shuffle { of: of?, picks: Shuffle::new(&picks)? })
    }

    /// Writes it in front of `before` and gives the value it is.
    fn build(self, func: &mut Func, before: Inst, ty: Type) -> Value {
        match self {
            Self::Shift { opcode, of, by } => {
                let at = func.add_imm(Imm::int(i128::from(by), ty.lane()));
                let data = InstData { extra: Extra::Imm(at), ..InstData::new(Opcode::Splat) };
                let count = emit(func, before, data, ty);
                let args = func.push_values(&[of, count]);
                emit(func, before, InstData { args, ..InstData::new(opcode) }, ty)
            }
            Self::Shuffle { of, picks } => {
                if picks.lanes().enumerate().all(|(at, lane)| usize::from(lane) == at) {
                    return of;
                }
                let args = func.push_values(&[of]);
                let data = InstData {
                    args,
                    extra: Extra::Shuffle(picks),
                    ..InstData::new(Opcode::Shuffle)
                };
                emit(func, before, data, ty)
            }
        }
    }
}

/// The vector and the lane an `extractlane` read, when that is what made the value.
fn extracted(func: &Func, value: Value) -> Option<(Value, u8)> {
    let data = &func[made_by(func, value)?];
    let Extra::Lane(lane) = data.extra else { return None };
    let &[of] = &func[data.args] else { return None };
    // Not through a bitcast, which would change which lane that is.
    (data.opcode == Opcode::ExtractLane).then_some((of, lane))
}

fn emit(func: &mut Func, before: Inst, data: InstData, ty: Type) -> Value {
    let span = func.span(before);
    let inst = func.create_inst(data, &[ty], span);
    func.insert_before(inst, before);
    func[inst].first_result.expect("one result was asked for")
}

/// Recorded for a vector read as two `long` halves and put together, which became one read of it.
const LOADED: &str = "vector read as two longs rebuilt as one load";

/// Recorded for a vector written as its two `long` halves, which became one write of it.
const STORED: &str = "vector written as two longs rebuilt as one store";

/// Recorded for a vector taken apart into its lanes and put back together in the same order.
const REJOINED: &str = "vector taken apart and put back together is the vector it was";

/// Recorded for a vector a block took as its two `long` halves, which it now takes whole.
const CARRIED: &str = "vector a block took as two longs taken as one vector";

/// Recorded for an add, an or or an exclusive or with a vector of zeros.
const ZERO: &str = "vector operation with a vector of zeros is its other operand";

/// What the pass that puts AArch64's vectors back together is called.
pub const REJOIN: &str = "rejoin";

/// The vectors AArch64 keeps in a `v` register, put back together where the calling convention
/// took them apart.
///
/// A vector argument or result on AArch64 is a pair of `x` registers here, so the front end passes
/// one as two `long` values, and once an intrinsic of `<arm_neon.h>` is inlined what is left
/// around its one operator is two `long` loads put into the halves of a vector, the operator, and
/// the two halves taken out again and stored. Between two inlined intrinsics the halves taken out
/// of one are put straight back into the next. Each of those is a move from a `v` register to an
/// `x` register and back for every half, which is most of what the kernel's RAID-6 syndrome loop
/// was (tamnd/rucc#3775). This puts back a full chain of lanes that are the same lanes of one
/// vector as that vector, two adjacent `long` loads into the two halves as one load of the vector,
/// two adjacent stores of the halves as one store of it, and drops an add, an or or an exclusive or
/// with a vector of zeros, which is what `vdupq_n_u8` adds its broadcast to.
///
/// It runs at every level that optimizes, after the inlined calls are one block with their caller,
/// and only on AArch64 with the vector registers, which is the one build that passes a vector it
/// keeps whole as two integers. Two loads are one only when nothing writes memory from the first of
/// them to where the vector is put together, and two stores only when nothing touches memory
/// between them. A volatile or atomic access is never one of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rejoin;

impl Pass for Rejoin {
    fn name(&self) -> &'static str {
        REJOIN
    }

    fn describe(&self) -> &'static str {
        "a vector AArch64 passed as two longs is put back together, and the halves read or written \
         one after the other are one load or store of it"
    }

    fn phase(&self) -> Phase {
        Phase::Scalar
    }

    fn preserves(&self) -> Preserved {
        // Instructions come and go inside blocks and no edge moves.
        Preserved::ALL.without(Analysis::Liveness).without(Analysis::Pressure)
    }

    fn run(&self, func: &mut Func, an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        if !an.outside().neon() || an.outside().little_endian() != Some(true) {
            return Stats::new();
        }
        rejoin(func, fuel)
    }
}

/// [`Rejoin`] on one function.
fn rejoin(func: &mut Func, fuel: &mut Fuel) -> Stats {
    let mut stats = Stats::new();
    // A loop nest passes the halves from one header to the next, so the vector a header now takes
    // whole is two halves put together in the block before it, which is another header's pair.
    let mut done: Set<Inst> = Set::default();
    for _ in 0..8 {
        if !carried(func, fuel, &mut stats, &mut done) {
            break;
        }
    }
    let mut inside: Set<Inst> = Set::default();
    let mut forward: Map<Value, Value> = Map::default();
    for block in func.blocks().collect::<Vec<Block>>() {
        // Bottom up for the reason `Lanes` gives.
        for inst in func.insts_backwards(block).collect::<Vec<Inst>>() {
            let data = &func[inst];
            let Some(result) = data.first_result else { continue };
            let ty = func[result].ty;
            if matches!(data.opcode, Opcode::Add | Opcode::Or | Opcode::Xor)
                && ty.is_vector()
                && ty.lane().is_int()
            {
                let &[a, b] = &func[data.args] else { continue };
                let other = if zeros(func, b) {
                    a
                } else if zeros(func, a) {
                    b
                } else {
                    continue;
                };
                if fuel.take() {
                    forward.insert(result, other);
                    stats.optimized(ZERO);
                }
                continue;
            }
            if inside.contains(&inst) || data.opcode != Opcode::InsertLane {
                continue;
            }
            let Some((ty, lanes, chain)) = chain(func, inst) else { continue };
            inside.extend(chain);
            let same = match Made::shuffle(func, ty, &lanes) {
                Some(Made::Shuffle { of, picks }) => picks
                    .lanes()
                    .enumerate()
                    .all(|(at, lane)| usize::from(lane) == at)
                    .then_some(of),
                _ => None,
            };
            if let Some(of) = same {
                if fuel.take() {
                    forward.insert(result, of);
                    stats.optimized(REJOINED);
                }
                continue;
            }
            let Some((address, align)) = halves(func, inst, ty, &lanes) else { continue };
            if !fuel.take() {
                stats.missed(NO_FUEL);
                continue;
            }
            let mem = func.add_mem(plain(align));
            let args = func.push_values(&[address]);
            let data = InstData { args, extra: Extra::Mem(mem), ..InstData::new(Opcode::Load) };
            let loaded = emit(func, inst, data, ty);
            forward.insert(result, loaded);
            stats.optimized(LOADED);
        }
    }
    if !forward.is_empty() {
        substitute(func, &forward);
    }
    for block in func.blocks().collect::<Vec<Block>>() {
        stores(func, block, fuel, &mut stats);
    }
    stats
}

/// The vectors a block takes as two `long` parameters and puts together, taken as one vector
/// parameter, which is what a vector variable of a loop becomes once the intrinsics that read and
/// write it are inlined: the scalar replacement saw only the halves the calls passed, so the
/// loop carries the halves, and every trip takes the vector apart at the end and puts it together
/// at the top. Each edge passes the vector its two halves came out of, or the two put together
/// where they did not come out of one. Two `long` parameters stored one after the other to an
/// address and the eight bytes after it are taken whole the same way, when some edge passes the
/// halves of one vector, which is the block after such a loop that writes the vector back: the
/// halves are then taken out of the new parameter, and the stores of them become one store in
/// [`stores`]. Whether anything was changed. `done` is the chains an earlier round took the place
/// of, which are still there with nothing reading them.
fn carried(func: &mut Func, fuel: &mut Fuel, stats: &mut Stats, done: &mut Set<Inst>) -> bool {
    let entry = func.entry();
    let mut pairs: Map<(Block, u32, u32), Vec<Value>> = Map::default();
    let mut order: Vec<(Block, u32, u32)> = Vec::new();
    let mut inside: Set<Inst> = Set::default();
    for block in func.blocks().collect::<Vec<Block>>() {
        for inst in func.insts_backwards(block).collect::<Vec<Inst>>() {
            if done.contains(&inst)
                || inside.contains(&inst)
                || func[inst].opcode != Opcode::InsertLane
            {
                continue;
            }
            let Some((ty, lanes, chain)) = chain(func, inst) else { continue };
            inside.extend(chain.iter().copied());
            let &[low, high] = &lanes[..] else { continue };
            if ty != longs() {
                continue;
            }
            let (Def::Param { block: one, index: i }, Def::Param { block: two, index: j }) =
                (func[low].def, func[high].def)
            else {
                continue;
            };
            if one != two || Some(one) == entry {
                continue;
            }
            let result = func[inst].first_result.expect("an insertlane has a result");
            let key = (one, i, j);
            pairs
                .entry(key)
                .or_insert_with(|| {
                    order.push(key);
                    Vec::new()
                })
                .push(result);
            done.extend(chain);
        }
    }
    // The parameters stored as the halves of a vector, which are taken out of the new parameter.
    let mut split: Set<(Block, u32, u32)> = Set::default();
    for block in func.blocks().collect::<Vec<Block>>() {
        let mut last: Option<Inst> = None;
        for inst in func.insts(block).collect::<Vec<Inst>>() {
            if !func[inst].opcode.touches_memory() {
                continue;
            }
            let Some(before) = last.replace(inst) else { continue };
            let (Some(one), Some(two)) = (param_store(func, before), param_store(func, inst))
            else {
                continue;
            };
            let (low, high) = if one.3 < two.3 { (one, two) } else { (two, one) };
            if low.0 != high.0
                || Some(low.0) == entry
                || low.1 == high.1
                || low.2 != high.2
                || low.3.checked_add(8) != Some(high.3)
            {
                continue;
            }
            let key = (low.0, low.1, high.1);
            if split.insert(key) && !pairs.contains_key(&key) {
                pairs.insert(key, Vec::new());
                order.push(key);
            }
        }
    }
    if order.is_empty() {
        return false;
    }
    // Every edge into each of the blocks, which has to be one a parameter can be added to.
    let mut edges: Map<Block, Vec<(Inst, Idx<BlockCall>)>> = Map::default();
    let mut awkward: Set<Block> = Set::default();
    for block in func.blocks().collect::<Vec<Block>>() {
        let Some(terminator) = func.terminator(block) else { continue };
        let simple =
            matches!(func[terminator].opcode, Opcode::Jump | Opcode::BrIf | Opcode::Switch);
        for at in func.target_list(terminator).iter() {
            let to = func[at].block;
            edges.entry(to).or_default().push((terminator, at));
            if !simple {
                awkward.insert(to);
            }
        }
    }
    let mut forward: Map<Value, Value> = Map::default();
    // The halves a branch puts together, once for all the blocks it goes to.
    let mut made: Map<(Inst, Value, Value), Value> = Map::default();
    for key in order {
        let (block, i, j) = key;
        if awkward.contains(&block) {
            continue;
        }
        // Halves that are only stored are taken whole where that saves taking a vector apart,
        // and not where every edge would put two scalars together for it.
        if pairs[&key].is_empty()
            && !edges.get(&block).into_iter().flatten().any(|&(_, at)| {
                let passed = &func[func[at].args];
                halves_of(func, passed[i as usize], passed[j as usize]).is_some()
            })
        {
            continue;
        }
        if !fuel.take() {
            continue;
        }
        let param = func.append_param(block, longs());
        for &(terminator, at) in edges.get(&block).into_iter().flatten() {
            let call = func[at];
            // An edge passes every parameter of the block it goes to.
            let passed = &func[call.args];
            let (low, high) = (passed[i as usize], passed[j as usize]);
            let whole = if let Some(of) = halves_of(func, low, high) {
                of
            } else if let Some(&whole) = made.get(&(terminator, low, high)) {
                whole
            } else if let Some((later, address, align)) = loaded_together(func, low, high) {
                // Halves read one after the other are one read of the vector, right after the
                // second, which is before every branch that passes them.
                let mem = func.add_mem(plain(align));
                let args = func.push_values(&[address]);
                let data = InstData { args, extra: Extra::Mem(mem), ..InstData::new(Opcode::Load) };
                let span = func.span(later);
                let load = func.create_inst(data, &[longs()], span);
                func.insert_after(load, later);
                let whole = func[load].first_result.expect("a load has a result");
                made.insert((terminator, low, high), whole);
                stats.optimized(LOADED);
                whole
            } else {
                let at = func.add_imm(Imm::int(0, Type::int(64)));
                let data = InstData { extra: Extra::Imm(at), ..InstData::new(Opcode::Splat) };
                let zeros = emit(func, terminator, data, longs());
                let mut into = zeros;
                for (lane, value) in [(0, low), (1, high)] {
                    let args = func.push_values(&[into, value]);
                    let data = InstData {
                        args,
                        extra: Extra::Lane(lane),
                        ..InstData::new(Opcode::InsertLane)
                    };
                    into = emit(func, terminator, data, longs());
                }
                made.insert((terminator, low, high), into);
                into
            };
            let args = func.append_arg(call.args, whole);
            func.set_block_call(at, BlockCall { args, ..call });
        }
        for &value in &pairs[&key] {
            forward.insert(value, param);
        }
        if split.contains(&key) {
            let first = func.insts(block).next().expect("a block ends in a terminator");
            for (lane, index) in [(0, i), (1, j)] {
                let args = func.push_values(&[param]);
                let data = InstData {
                    args,
                    extra: Extra::Lane(lane),
                    ..InstData::new(Opcode::ExtractLane)
                };
                let half = emit(func, first, data, Type::int(64));
                forward.insert(func[block].params[index as usize], half);
            }
        }
        stats.optimized(CARRIED);
    }
    if forward.is_empty() {
        return false;
    }
    substitute(func, &forward);
    true
}

/// The vector of two `long` that `low` and `high` are the first and the second lane of.
fn halves_of(func: &Func, low: Value, high: Value) -> Option<Value> {
    match (extracted(func, low), extracted(func, high)) {
        (Some((of, 0)), Some((other, 1)))
            if func[of].ty == longs() && through(func, of) == through(func, other) =>
        {
            Some(of)
        }
        _ => None,
    }
}

/// An ordinary store of a `long` parameter of a block other than the first: the block, which
/// parameter, and the address split into a base and a constant offset.
fn param_store(func: &Func, inst: Inst) -> Option<(Block, u32, Value, i64)> {
    let data = &func[inst];
    let Extra::Mem(mem) = data.extra else { return None };
    let &[value, address] = &func[data.args] else { return None };
    if data.opcode != Opcode::Store
        || func[mem].order != MemOrder::NotAtomic
        || data.flags.intersects(Flags::KEEP)
        || func[value].ty != Type::int(64)
    {
        return None;
    }
    let Def::Param { block, index } = func[value].def else { return None };
    let (base, offset) = crate::bswap::split(func, address)?;
    Some((block, index, base, offset))
}

/// A vector of two `long`, the shape the halves of a vector passed in two `x` registers are put
/// into.
fn longs() -> Type {
    Type::vector(Type::int(64), 2)
}

/// An ordinary access with nothing known about it but its alignment.
fn plain(align: u32) -> MemInfo {
    MemInfo {
        size: 0,
        align,
        order: MemOrder::NotAtomic,
        tbaa: None,
        owns: 0,
        restrict: Restrict::NONE,
    }
}

/// Whether the value is a vector of zeros, seen through bitcasts, which keep the bits.
fn zeros(func: &Func, mut value: Value) -> bool {
    loop {
        let Some(inst) = made_by(func, value) else { return false };
        let data = &func[inst];
        match (data.opcode, &func[data.args], data.extra) {
            (Opcode::Bitcast, &[from], _) if func[from].ty.is_vector() => value = from,
            (Opcode::Splat, &[], Extra::Imm(at)) => return func[at].bits() == 0,
            _ => return false,
        }
    }
}

/// An ordinary load of a `long` and the address it read, split into a base and a constant offset.
fn long_load(func: &Func, value: Value) -> Option<(Inst, Value, Value, i64, u32)> {
    let inst = made_by(func, value)?;
    let data = &func[inst];
    let Extra::Mem(mem) = data.extra else { return None };
    let &[address] = &func[data.args] else { return None };
    if data.opcode != Opcode::Load
        || func[value].ty != Type::int(64)
        || func[mem].order != MemOrder::NotAtomic
        || data.flags.intersects(Flags::KEEP)
    {
        return None;
    }
    let (base, offset) = crate::bswap::split(func, address)?;
    Some((inst, address, base, offset, func[mem].align))
}

/// The address and alignment of one load of the whole vector, when the two lanes of a vector of two
/// `long` are two loads of the eight bytes at an address and the eight after it, in the block of
/// `top` with nothing that writes memory from the first of them to it.
fn halves(func: &Func, top: Inst, ty: Type, lanes: &[Value]) -> Option<(Value, u32)> {
    if ty != longs() {
        return None;
    }
    let &[low, high] = lanes else { return None };
    let (first, address, base, offset, align) = long_load(func, low)?;
    let (second, _, other, at, _) = long_load(func, high)?;
    if base != other || offset.checked_add(8)? != at {
        return None;
    }
    let block = func.block_of(top)?;
    if func.block_of(first)? != block || func.block_of(second)? != block {
        return None;
    }
    let mut reading = false;
    for inst in func.insts(block) {
        if inst == top {
            return reading.then_some((address, align));
        }
        reading |= inst == first || inst == second;
        if reading && func[inst].opcode.writes_memory() {
            return None;
        }
    }
    None
}

/// Two ordinary `long` loads from an address and the eight bytes after it, in one block with
/// nothing written between them: the later of the two, the lower address and its alignment.
fn loaded_together(func: &Func, low: Value, high: Value) -> Option<(Inst, Value, u32)> {
    let (first, address, base, offset, align) = long_load(func, low)?;
    let (second, _, other, at, _) = long_load(func, high)?;
    if base != other || offset.checked_add(8)? != at {
        return None;
    }
    let block = func.block_of(first)?;
    if func.block_of(second)? != block {
        return None;
    }
    let mut reading = None;
    for inst in func.insts(block) {
        if inst == first || inst == second {
            if reading.is_some() {
                return Some((inst, address, align));
            }
            reading = Some(inst);
        } else if reading.is_some() && func[inst].opcode.writes_memory() {
            return None;
        }
    }
    None
}

/// An ordinary store of lane `lane` of a vector of two `long`, as the vector, the lane, the address
/// split into a base and an offset, the address itself and the alignment.
fn long_store(func: &Func, inst: Inst) -> Option<(Value, u8, Value, i64, Value, u32)> {
    let data = &func[inst];
    let Extra::Mem(mem) = data.extra else { return None };
    let &[value, address] = &func[data.args] else { return None };
    if data.opcode != Opcode::Store
        || func[mem].order != MemOrder::NotAtomic
        || data.flags.intersects(Flags::KEEP)
    {
        return None;
    }
    let (of, lane) = extracted(func, value)?;
    if func[of].ty != longs() {
        return None;
    }
    let (base, offset) = crate::bswap::split(func, address)?;
    Some((of, lane, base, offset, address, func[mem].align))
}

/// Two stores one after the other in `block` of the two halves of one vector to an address and
/// the eight bytes after it, made one store of the vector where the second of them was.
fn stores(func: &mut Func, block: Block, fuel: &mut Fuel, stats: &mut Stats) {
    let mut last: Option<Inst> = None;
    for inst in func.insts(block).collect::<Vec<Inst>>() {
        if !func[inst].opcode.touches_memory() {
            continue;
        }
        let Some(before) = last.replace(inst) else { continue };
        let (Some(one), Some(two)) = (long_store(func, before), long_store(func, inst)) else {
            continue;
        };
        let (low, high) = if one.1 == 0 { (one, two) } else { (two, one) };
        if through(func, low.0) != through(func, high.0)
            || (low.1, high.1) != (0, 1)
            || low.2 != high.2
            || low.3.checked_add(8) != Some(high.3)
        {
            continue;
        }
        if !fuel.take() {
            stats.missed(NO_FUEL);
            continue;
        }
        let (vector, _, _, _, address, align) = low;
        let mem = func.add_mem(plain(align));
        let args = func.push_values(&[vector, address]);
        let data = InstData { args, extra: Extra::Mem(mem), ..InstData::new(Opcode::Store) };
        let span = func.span(inst);
        let store = func.create_inst(data, &[], span);
        func.insert_before(store, inst);
        func.remove_inst(before);
        func.remove_inst(inst);
        last = None;
        stats.optimized(STORED);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rucc_base::Interner;

    use super::{Lanes, Rejoin};
    use crate::outside::Outside;
    use crate::{Fuel, Pass};

    const HEAD: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "x86_64-unknown-linux-gnu"
target datalayout = "e-p:64:64-i64:64-f80:128-S128"
"#;

    /// The same module head for wasm32.
    const WASM: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "wasm32-unknown-wasip1"
target datalayout = "e-p:32:32-i64:64-S128"
"#;

    /// The module that text is, with the pass run over every function in it, on a build with the
    /// vector registers or without them.
    fn run(body: &str, vectors: bool) -> String {
        built(&format!("{HEAD}{body}"), |outside| outside.with_vectors(vectors))
    }

    /// The same on wasm32 with `-msimd128`.
    fn on_simd128(body: &str) -> String {
        built(&format!("{WASM}{body}"), |outside| outside.with_simd128(true))
    }

    fn built(text: &str, facts: impl Fn(Outside) -> Outside) -> String {
        let mut names = Interner::new();
        let mut module = rucc_ir::parse(text, &mut names).expect("the fixture parses");
        let outside = Arc::new(facts(Outside::of(&module)));
        let ids: Vec<_> = module.funcs().collect();
        for id in ids {
            if module[id].is_declaration() {
                continue;
            }
            let mut an = crate::machine::fixtures::analyses().about(Arc::clone(&outside));
            Lanes.run(&mut module[id], &mut an, &mut Fuel::unlimited());
        }
        if let Err(errors) = rucc_ir::verify(&module, &names) {
            panic!("the pass left invalid IR, {errors:?}\n{}", rucc_ir::print(&module, &names));
        }
        rucc_ir::print(&module, &names)
    }

    /// The returned value, which is the last line before the closing brace.
    fn returned(out: &str) -> &str {
        out.lines().rev().find(|line| line.trim_start().starts_with("return")).unwrap_or("")
    }

    fn count(out: &str, opcode: &str) -> usize {
        out.lines().filter(|line| line.contains(&format!("= {opcode}"))).count()
    }

    /// `_mm_slli_epi32(x, 7)` once the loop over the lanes is unrolled and the answer is out of
    /// memory, with the bitcasts to the other shape the scalar replacement leaves between lanes.
    const SHIFTED: &str = r"
func @f(i32x4) -> i32x4, linkage(external) {
block0(%0: i32x4):
    %1 = splat.i64x2 0
    %2 = iconst.i32 7
    %3 = extractlane.i32 %0, lane 0
    %4 = shl %3, %2
    %5 = bitcast.i32x4 %1
    %6 = insertlane %5, %4, lane 0
    %7 = bitcast.i64x2 %6
    %8 = extractlane.i32 %0, lane 1
    %9 = shl %8, %2
    %10 = bitcast.i32x4 %7
    %11 = insertlane %10, %9, lane 1
    %12 = iconst.i32 7
    %13 = extractlane.i32 %0, lane 2
    %14 = shl %13, %12
    %15 = insertlane %11, %14, lane 2
    %16 = extractlane.i32 %0, lane 3
    %17 = shl %16, %2
    %18 = insertlane %15, %17, lane 3
    return %18
}
";

    #[test]
    fn every_lane_shifted_by_one_constant_is_one_shift() {
        let out = run(SHIFTED, true);
        assert_eq!(count(&out, "splat.i32x4 7"), 1, "{out}");
        let shift = out.lines().find(|line| line.contains("= shl %0,")).expect("one shift");
        let name = shift.trim().split(' ').next().expect("a name");
        assert_eq!(returned(&out).trim(), format!("return {name}"), "{out}");
    }

    #[test]
    fn on_wasm_with_simd128_every_lane_shifted_by_one_constant_is_one_shift() {
        let out = on_simd128(SHIFTED);
        assert_eq!(count(&out, "splat.i32x4 7"), 1, "{out}");
        assert!(returned(&out).trim() != "return %18", "{out}");
        assert_eq!(returned(&built(&format!("{WASM}{SHIFTED}"), |it| it)).trim(), "return %18");
    }

    #[test]
    fn without_the_vector_registers_the_lanes_stay() {
        let out = run(SHIFTED, false);
        assert_eq!(count(&out, "splat.i32x4"), 0, "{out}");
        assert_eq!(returned(&out).trim(), "return %18");
    }

    /// Two counts, or a lane from the wrong place, is not one shift.
    #[test]
    fn lanes_that_disagree_stay() {
        let other = SHIFTED.replace("%12 = iconst.i32 7", "%12 = iconst.i32 8");
        assert_eq!(returned(&run(&other, true)).trim(), "return %18");
        let crossed =
            SHIFTED.replace("%13 = extractlane.i32 %0, lane 2", "%13 = extractlane.i32 %0, lane 1");
        assert_eq!(returned(&run(&crossed, true)).trim(), "return %18");
        let wide = SHIFTED.replace("iconst.i32 7", "iconst.i32 32");
        assert_eq!(returned(&run(&wide, true)).trim(), "return %18");
    }

    /// `_mm_shuffle_epi32(x, 0x93)` once its lane subscripts are out of memory.
    #[test]
    fn lanes_of_one_vector_in_another_order_are_one_shuffle() {
        let out = run(
            r"
func @f(i32x4) -> i32x4, linkage(external) {
block0(%0: i32x4):
    %1 = splat.i32x4 0
    %2 = extractlane.i32 %0, lane 3
    %3 = insertlane %1, %2, lane 0
    %4 = extractlane.i32 %0, lane 0
    %5 = insertlane %3, %4, lane 1
    %6 = extractlane.i32 %0, lane 1
    %7 = insertlane %5, %6, lane 2
    %8 = extractlane.i32 %0, lane 2
    %9 = insertlane %7, %8, lane 3
    return %9
}
",
            true,
        );
        assert_eq!(count(&out, "shuffle"), 1, "{out}");
        assert!(out.contains("shuffle %0, lanes [3, 0, 1, 2]"), "{out}");
        let shuffle = out.lines().find(|line| line.contains("= shuffle")).expect("one shuffle");
        let name = shuffle.trim().split(' ').next().expect("a name");
        assert_eq!(returned(&out).trim(), format!("return {name}"), "{out}");
    }

    /// No arithmetic shift of a `long` lane on this machine, so that one stays a lane at a time.
    #[test]
    fn an_arithmetic_shift_of_long_lanes_stays() {
        let out = run(
            r"
func @f(i64x2) -> i64x2, linkage(external) {
block0(%0: i64x2):
    %1 = splat.i64x2 0
    %2 = iconst.i64 3
    %3 = extractlane.i64 %0, lane 0
    %4 = ashr %3, %2
    %5 = insertlane %1, %4, lane 0
    %6 = extractlane.i64 %0, lane 1
    %7 = ashr %6, %2
    %8 = insertlane %5, %7, lane 1
    return %8
}
",
            true,
        );
        assert_eq!(returned(&out).trim(), "return %8", "{out}");
        let logical = run(
            r"
func @f(i64x2) -> i64x2, linkage(external) {
block0(%0: i64x2):
    %1 = splat.i64x2 0
    %2 = iconst.i64 3
    %3 = extractlane.i64 %0, lane 0
    %4 = lshr %3, %2
    %5 = insertlane %1, %4, lane 0
    %6 = extractlane.i64 %0, lane 1
    %7 = lshr %6, %2
    %8 = insertlane %5, %7, lane 1
    return %8
}
",
            true,
        );
        assert!(logical.contains("= lshr %0,"), "{logical}");
    }

    /// wasm has `i64x2.shr_s`, so there the arithmetic shift of `long` lanes is one shift too.
    #[test]
    fn on_wasm_an_arithmetic_shift_of_long_lanes_is_one_shift() {
        let out = on_simd128(
            r"
func @f(i64x2) -> i64x2, linkage(external) {
block0(%0: i64x2):
    %1 = splat.i64x2 0
    %2 = iconst.i64 3
    %3 = extractlane.i64 %0, lane 0
    %4 = ashr %3, %2
    %5 = insertlane %1, %4, lane 0
    %6 = extractlane.i64 %0, lane 1
    %7 = ashr %6, %2
    %8 = insertlane %5, %7, lane 1
    return %8
}
",
        );
        assert!(out.contains("= ashr %0,"), "{out}");
    }

    /// The same module head for AArch64.
    const ARM: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "aarch64-unknown-linux-gnu"
target datalayout = "e-p:64:64-i64:64-S128"
"#;

    /// The module that text is, with [`super::Rejoin`] run over every function in it on a build
    /// with the vector registers.
    fn rejoined(body: &str) -> String {
        rejoined_with(body, true)
    }

    /// The same, with the vector registers or without them.
    fn rejoined_with(body: &str, neon: bool) -> String {
        let mut names = Interner::new();
        let mut module =
            rucc_ir::parse(&format!("{ARM}{body}"), &mut names).expect("the fixture parses");
        let outside = Arc::new(Outside::of(&module).with_neon(neon));
        let ids: Vec<_> = module.funcs().collect();
        for id in ids {
            if module[id].is_declaration() {
                continue;
            }
            let mut an = crate::machine::fixtures::analyses().about(Arc::clone(&outside));
            Rejoin.run(&mut module[id], &mut an, &mut Fuel::unlimited());
        }
        if let Err(errors) = rucc_ir::verify(&module, &names) {
            panic!("the pass left invalid IR, {errors:?}\n{}", rucc_ir::print(&module, &names));
        }
        rucc_ir::print(&module, &names)
    }

    /// The lines that are instructions with this opcode, a load or a store included.
    fn written(out: &str, opcode: &str) -> usize {
        out.lines().filter(|line| line.trim_start().starts_with(opcode)).count()
    }

    /// `*p = veorq_u8(*p, *q)` with both intrinsics of `<arm_neon.h>` inlined: each vector comes in
    /// as two `long` halves read from memory, and goes out as two halves written back.
    const HALVES: &str = r"
func @f(ptr, ptr), linkage(external) {
block0(%0: ptr, %1: ptr):
    %2 = splat.i8x16 0
    %3 = load.i64 %0, align 1
    %4 = iconst.i64 8
    %5 = ptr_add %0, %4
    %6 = load.i64 %5, align 1
    %7 = bitcast.i64x2 %2
    %8 = insertlane %7, %3, lane 0
    %9 = insertlane %8, %6, lane 1
    %10 = load.i64 %1, align 1
    %11 = ptr_add %1, %4
    %12 = load.i64 %11, align 1
    %13 = extractlane.i64 %9, lane 0
    %14 = extractlane.i64 %9, lane 1
    %15 = insertlane %7, %13, lane 0
    %16 = insertlane %15, %14, lane 1
    %17 = bitcast.i8x16 %16
    %18 = insertlane %7, %10, lane 0
    %19 = insertlane %18, %12, lane 1
    %20 = bitcast.i8x16 %19
    %21 = xor %17, %20
    %22 = bitcast.i64x2 %21
    %23 = extractlane.i64 %22, lane 0
    %24 = extractlane.i64 %22, lane 1
    store %23 -> %0, align 1
    %25 = ptr_add %0, %4
    store %24 -> %25, align 1
    return
}
";

    #[test]
    fn a_vector_read_and_written_as_two_longs_is_one_load_and_one_store() {
        let out = rejoined(HALVES);
        assert_eq!(count(&out, "load.i64x2"), 2, "{out}");
        assert_eq!(written(&out, "store"), 1, "{out}");
        // Each operand of the xor is a load of the whole vector, seen as bytes, with the halves of
        // the first one taken out and put back being that vector.
        let xor = out.lines().find(|line| line.contains("= xor")).expect("the xor stays");
        let (name, operands) = xor.trim().split_once(" = xor ").unwrap();
        for operand in operands.split(", ") {
            let bytes = def(&out, operand);
            assert!(bytes.contains("= bitcast.i8x16 "), "{out}");
            let loaded = def(&out, bytes.rsplit(' ').next().unwrap());
            assert!(loaded.contains("= load.i64x2 "), "{out}");
        }
        let store = out.lines().find(|line| line.trim_start().starts_with("store")).unwrap();
        let (stored, _) = store.trim()["store ".len()..].split_once(" -> ").unwrap();
        assert!(store.contains(" -> %0,"), "{out}");
        assert!(def(&out, stored).ends_with(&format!("= bitcast.i64x2 {name}")), "{out}");
    }

    /// The line that defines a value.
    fn def<'a>(out: &'a str, name: &str) -> &'a str {
        let start = format!("{name} = ");
        let found = out.lines().map(str::trim).find(|line| line.starts_with(&start));
        found.unwrap_or_else(|| panic!("nothing defines {name} in\n{out}"))
    }

    /// Under `-mgeneral-regs-only` there is no vector register to put them together in.
    #[test]
    fn without_the_vector_registers_the_halves_stay() {
        let out = rejoined_with(HALVES, false);
        assert_eq!(count(&out, "load.i64x2"), 0, "{out}");
        assert_eq!(written(&out, "store"), 2, "{out}");
    }

    /// A store between the two loads, which may be to either half, keeps them two loads.
    #[test]
    fn a_store_between_the_halves_keeps_them_apart() {
        let text = HALVES.replace(
            "    %6 = load.i64 %5, align 1\n",
            "    store %4 -> %1, align 1\n    %6 = load.i64 %5, align 1\n",
        );
        let out = rejoined(&text);
        assert_eq!(count(&out, "load.i64x2"), 1, "{out}");
    }

    /// Volatile halves are two accesses, as the program wrote them.
    #[test]
    fn volatile_halves_stay_two() {
        let text = HALVES
            .replace("%3 = load.i64 %0", "%3 = load.i64.volatile %0")
            .replace("store %24 -> %25", "store.volatile %24 -> %25");
        let out = rejoined(&text);
        assert_eq!(count(&out, "load.i64x2"), 1, "{out}");
        assert_eq!(written(&out, "store"), 2, "{out}");
    }

    /// Halves that are not the eight bytes and the eight after them are not one vector in memory.
    #[test]
    fn halves_that_are_not_next_to_each_other_stay_two() {
        let text = HALVES.replace("%4 = iconst.i64 8", "%4 = iconst.i64 16");
        let out = rejoined(&text);
        assert_eq!(count(&out, "load.i64x2"), 0, "{out}");
        assert_eq!(written(&out, "store"), 2, "{out}");
    }

    /// `vdupq_n_u8(c)` is `(uint8x16_t){0} + c`, and the add of the zeros is the broadcast.
    #[test]
    fn an_add_of_a_vector_of_zeros_is_the_other_operand() {
        let out = rejoined(
            r"
func @f(i64x2) -> i8x16, linkage(external) {
block0(%0: i64x2):
    %1 = splat.i8x16 0
    %2 = bitcast.i8x16 %0
    %3 = add %1, %2
    %4 = or %3, %1
    return %4
}
",
        );
        assert_eq!(returned(&out).trim(), "return %2", "{out}");
    }

    /// `uint8x16_t a = vld1q_u8(p); while (...) a = vshlq_n_u8(a, 1); vst1q_u8(p, a);`, where the
    /// loop carries the two halves the calls passed and puts them together at the top of each trip.
    const CARRIED: &str = r"
func @f(ptr, i32), linkage(external) {
block0(%0: ptr, %1: i32):
    %2 = splat.i8x16 0
    %3 = load.i64 %0, align 1
    %4 = iconst.i64 8
    %5 = ptr_add %0, %4
    %6 = load.i64 %5, align 1
    %7 = iconst.i32 0
    jump block1(%3, %6, %7)

block1(%8: i64, %9: i64, %10: i32):
    %11 = bitcast.i64x2 %2
    %12 = insertlane %11, %8, lane 0
    %13 = insertlane %12, %9, lane 1
    %14 = bitcast.i8x16 %13
    %15 = splat.i8x16 1
    %16 = shl %14, %15
    %17 = bitcast.i64x2 %16
    %18 = extractlane.i64 %17, lane 0
    %19 = extractlane.i64 %17, lane 1
    %20 = iconst.i32 1
    %21 = add %10, %20
    %22 = icmp slt %21, %1
    br_if %22, block1(%18, %19, %21), block2

block2:
    store %18 -> %0, align 1
    store %19 -> %5, align 1
    return
}
";

    #[test]
    fn a_loop_that_carries_the_halves_of_a_vector_carries_the_vector() {
        let out = rejoined(CARRIED);
        // The header takes the vector, the edge into the loop passes one load of it, and the edge
        // round the loop passes the vector the shift made.
        let header = out.lines().find(|line| line.starts_with("block1(")).expect("the header");
        assert!(header.contains(": i64x2)"), "{out}");
        assert_eq!(count(&out, "load.i64x2"), 1, "{out}");
        let shift = out.lines().find(|line| line.contains("= shl ")).expect("the shift");
        let (name, operands) = shift.trim().split_once(" = shl ").unwrap();
        let (shifted, _) = operands.split_once(", ").unwrap();
        let carried = header.rsplit('%').next().unwrap().split(':').next().unwrap();
        assert!(def(&out, shifted).ends_with(&format!("= bitcast.i8x16 %{carried}")), "{out}");
        let back = out.lines().find(|line| line.trim_start().starts_with("br_if")).unwrap();
        let ending = format!("= bitcast.i64x2 {name}");
        let vector = out.lines().map(str::trim).find(|line| line.ends_with(&ending)).unwrap();
        let vector = vector.split(' ').next().unwrap();
        assert!(back.contains(&format!("{vector})")), "{out}");
        assert_eq!(written(&out, "store"), 1, "{out}");
    }

    /// The same loop where the block after it takes the halves from the loop and from the edge
    /// that skips it, and writes them back.
    const STORED_AFTER: &str = r"
func @f(ptr, i32), linkage(external) {
block0(%0: ptr, %1: i32):
    %2 = splat.i8x16 0
    %3 = load.i64 %0, align 1
    %4 = iconst.i64 8
    %5 = ptr_add %0, %4
    %6 = load.i64 %5, align 1
    %7 = iconst.i32 0
    %8 = icmp slt %7, %1
    br_if %8, block1(%3, %6, %7), block2(%3, %6)

block1(%9: i64, %10: i64, %11: i32):
    %12 = bitcast.i64x2 %2
    %13 = insertlane %12, %9, lane 0
    %14 = insertlane %13, %10, lane 1
    %15 = bitcast.i8x16 %14
    %16 = splat.i8x16 1
    %17 = shl %15, %16
    %18 = bitcast.i64x2 %17
    %19 = extractlane.i64 %18, lane 0
    %20 = extractlane.i64 %18, lane 1
    %21 = iconst.i32 1
    %22 = add %11, %21
    %23 = icmp slt %22, %1
    br_if %23, block1(%19, %20, %22), block2(%19, %20)

block2(%24: i64, %25: i64):
    store %24 -> %0, align 1
    store %25 -> %5, align 1
    return
}
";

    #[test]
    fn the_halves_a_block_after_a_loop_writes_back_are_one_vector_and_one_store() {
        let out = rejoined(STORED_AFTER);
        let after = out.lines().find(|line| line.starts_with("block2(")).expect("the exit");
        assert!(after.contains(": i64x2)"), "{out}");
        assert_eq!(written(&out, "store"), 1, "{out}");
        let carried = after.rsplit('%').next().unwrap().split(':').next().unwrap();
        let store = out.lines().find(|line| line.trim_start().starts_with("store")).unwrap();
        assert!(store.contains(&format!("store %{carried} -> %0")), "{out}");
        // The edge that skips the loop puts its two loads together as one.
        assert_eq!(count(&out, "load.i64x2"), 1, "{out}");
    }

    /// Two `long` parameters stored next to each other where no edge passes a vector's halves.
    #[test]
    fn halves_that_never_were_a_vector_stay_two_parameters() {
        let out = rejoined(
            r"
func @f(ptr, i64, i64), linkage(external) {
block0(%0: ptr, %1: i64, %2: i64):
    jump block1(%1, %2)

block1(%3: i64, %4: i64):
    %5 = iconst.i64 8
    %6 = ptr_add %0, %5
    store %3 -> %0, align 1
    store %4 -> %6, align 1
    return
}
",
        );
        assert!(!out.contains("i64x2"), "{out}");
        assert_eq!(written(&out, "store"), 2, "{out}");
    }

    /// The halves the loop is entered with read in a block before the one that branches into it.
    #[test]
    fn halves_read_in_an_earlier_block_are_one_load_there() {
        let out = rejoined(
            r"
func @f(ptr, i32), linkage(external) {
block0(%0: ptr, %1: i32):
    %2 = splat.i8x16 0
    %3 = load.i64 %0, align 1
    %4 = iconst.i64 8
    %5 = ptr_add %0, %4
    %6 = load.i64 %5, align 1
    %7 = iconst.i32 0
    jump block1

block1:
    jump block2(%3, %6, %7)

block2(%8: i64, %9: i64, %10: i32):
    %11 = bitcast.i64x2 %2
    %12 = insertlane %11, %8, lane 0
    %13 = insertlane %12, %9, lane 1
    %14 = bitcast.i8x16 %13
    %15 = splat.i8x16 1
    %16 = shl %14, %15
    %17 = bitcast.i64x2 %16
    %18 = extractlane.i64 %17, lane 0
    %19 = extractlane.i64 %17, lane 1
    %20 = iconst.i32 1
    %21 = add %10, %20
    %22 = icmp slt %21, %1
    br_if %22, block2(%18, %19, %21), block3

block3:
    store %18 -> %0, align 1
    store %19 -> %5, align 1
    return
}
",
        );
        assert_eq!(count(&out, "load.i64x2"), 1, "{out}");
        let (first, _) = out.split_once("\nblock1:").expect("the block after the first");
        assert!(first.contains("= load.i64x2"), "{out}");
        assert!(!out.contains("splat.i64x2"), "{out}");
    }
}
