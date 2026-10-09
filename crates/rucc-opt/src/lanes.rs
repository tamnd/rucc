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

use rucc_base::hash::{Map, Set};
use rucc_ir::{Block, Def, Extra, Func, Imm, Inst, InstData, Opcode, Shuffle, Type, Value};

use crate::uses::substitute;
use crate::{Analyses, Analysis, Fuel, Pass, Preserved, Stats};

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
        if data.opcode != Opcode::Bitcast || !whole(func[from].ty) {
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rucc_base::Interner;

    use super::Lanes;
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
}
