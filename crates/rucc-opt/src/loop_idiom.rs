//! A loop that fills or copies memory an element at a time, as one `memset`, `memcpy` or
//! `memmove` after it.
//!
//! Section 20.4 of `spec/optimizer/20-idioms-and-libcalls.md`, the whole loop pattern match it
//! puts in M4:
//!
//! ```c
//! for (int i = 0; i < n; i++) a[i] = 0;
//! for (int i = 0; i < n; i++) a[i] = b[i];
//! ```
//!
//! gcc 16 writes the first as a call to `memset` and the second, when `a` and `b` are `restrict`,
//! as a call to `memcpy`, and there is no loop left in either. The library routine moves a word or
//! a vector at a time where the loop moves one element, which for a large `n` is the whole of the
//! time the loop took.
//!
//! # What the loop has to be
//!
//! One store, which every iteration reaches, to an address that walks up the loop by exactly the
//! width of the store, so the bytes it writes are one range with no gaps and nothing written twice.
//! The loop has to be one `crate::hoist::shaped` accepts, which is one way out, tested by every
//! iteration, no call and no loop inside it, and it has to be counted, so that how many bytes the
//! range covers is a number or an expression the block after the loop can work out. It is the
//! same work [`crate::sink`] does for a plane write, and the extent is the same one, so it is
//! built by the same code.
//!
//! Nothing else in the loop may touch memory. That is what lets the one call go after the loop
//! rather than where each store was: the only thing the order of the stores could have been seen
//! by is another access in the loop, and there is none. A volatile or atomic store is never one.
//!
//! What the store writes decides which call it is. A constant whose bytes are all the same, like
//! zero or minus one, and any byte the loop does not change, is a `memset` of that byte. A value
//! loaded in the same iteration, used by nothing but the store, from an address that walks up
//! the loop the same way, is a copy.
//!
//! # A copy, and which one
//!
//! `memcpy` promises its two ranges do not overlap, and a loop makes no such promise, so which
//! call a copy becomes is a question about the two ranges. They are apart when the store and the
//! load are under the same `restrict` scope through different pointers, which is what `restrict`
//! means, or when they walk two different globals or two different local objects. When they walk
//! the same object from offsets that are numbers and the store starts no later than the load, the
//! loop reads every element before it writes over it, which is what `memmove` does, and that is
//! the call. Anything else is left as a loop, since a loop that writes ahead of what it reads
//! copies its first elements over and over and neither call does that.
//!
//! # Where it does not run
//!
//! Not under `-ffreestanding` or `-fno-builtin`, where a call to `memset` is a call to whatever the
//! program links against, and not inside a function called `memset`, `memcpy` or `memmove`, whose
//! own loop would otherwise become a call to itself. Both are decided where the pipeline has the
//! options and the names, in [`crate::pipeline`].

use rucc_ir::{
    Block, Builder, Def, Extra, Flags, Func, Inst, InstData, MemInfo, MemOrder, Opcode, Restrict,
    Type, Value,
};

use crate::canon::route;
use crate::cfg::Cfg;
use crate::dom::Dominators;
use crate::hoist::{anchored, fits, plain_enough, shaped, starting};
use crate::loops::{LoopId, Loops};
use crate::range::query::Ranges;
use crate::scev::{Anchor, Evolution, Plain, Reading, Scev};
use crate::trip::{Around, counted, covered, inst_of};
use crate::{Analyses, Fuel, Pass, Preserved, Stats};
use rucc_base::hash::Map;

/// What this pass is called, for the lists in [`crate::pipeline`] that name it.
pub const NAME: &str = "loop-idiom";

/// Whether a function of this name is one whose loop may be the library routine itself.
pub(crate) fn writes_itself(name: &str) -> bool {
    matches!(name, "memset" | "memcpy" | "memmove")
}

/// Recorded for a loop that became a `memset`.
const FILL: &str = "loop that stores one byte over a range replaced by memset";

/// Recorded for a loop that became a `memcpy`.
const COPY: &str = "loop that copies between ranges that do not overlap replaced by memcpy";

/// Recorded for a loop that became a `memmove`.
const MOVE: &str = "loop that copies forward within one object replaced by memmove";

/// Recorded for a loop left by a computed `goto`, whose exit edge has nowhere to put a block.
const COMPUTED_EXIT: &str = "store left in the loop, it is left by a computed goto";

/// Recorded for a loop whose shape is not one the range after it can be worked out for.
const NOT_SHAPED: &str = "store left in the loop, the loop has a call, another loop or another way \
                          out in it, or no block in front of it";

/// Recorded for a loop nobody can say how many times goes round.
const NOT_COUNTED: &str = "store left in the loop, how many times the loop runs is not worked out";

/// Recorded for a loop with another access to memory in it.
const IN_THE_WAY: &str =
    "store left in the loop, something else in the loop reads or writes memory";

/// Recorded for a store an iteration can finish without reaching.
const NOT_EVERY_TIME: &str = "store left in the loop, an iteration can finish without it";

/// Recorded for a store whose address does not walk up the loop by its own width.
const NOT_A_WALK: &str =
    "store left in the loop, its address does not walk up the loop by its width";

/// Recorded for a store of something that is neither one byte over and over nor a copy.
const NOT_AN_IDIOM: &str = "store left in the loop, what it stores is not one byte or a copy";

/// Recorded for a copy whose two ranges may overlap in a way neither call copies.
const MAY_OVERLAP: &str = "copy left as a loop, its two ranges may overlap";

/// Recorded for a range too wide for the arithmetic that works out its length.
const TOO_WIDE: &str = "store left in the loop, the range it covers is too wide to work out";

/// Recorded for a loop that would have been replaced if there had been fuel for it.
const NO_FUEL: &str = "store left in the loop, the pass ran out of fuel";

/// The pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoopIdiom;

impl Pass for LoopIdiom {
    fn name(&self) -> &'static str {
        NAME
    }

    fn describe(&self) -> &'static str {
        "a loop that fills or copies a range an element at a time becomes memset, memcpy or memmove"
    }

    fn preserves(&self) -> Preserved {
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
        // Worked out first and applied afterwards, as in `crate::sink`: each plan adds to a block
        // after its own loop and removes instructions from its own body, and loops that are planned
        // have no loop inside them, so no plan touches what another one reads.
        let mut plans = Vec::new();
        {
            let mut scev = Scev::new(func, cfg, loops);
            let mut ranges = Ranges::new(func, cfg, doms);
            for id in loops.all() {
                match planned(func, cfg, doms, loops, &mut scev, &mut ranges, id) {
                    Ok(Some(plan)) => plans.push(plan),
                    Ok(None) => {}
                    Err(why) => stats.missed(why),
                }
            }
        }
        let mut split: Map<(Block, Block), Block> = Map::default();
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
            stats.optimized(match plan.kind {
                Opcode::Memset => FILL,
                Opcode::Memcpy => COPY,
                _ => MOVE,
            });
        }
        stats
    }
}

/// One loop to replace, and what goes after it instead.
struct Plan {
    exit: Exit,
    /// `memset`, `memcpy` or `memmove`.
    kind: Opcode,
    /// Where the first iteration stores.
    to: (Anchor, Plain),
    /// The byte for a fill, or where the first iteration loads for a copy.
    with: With,
    span: Extent,
    align: u32,
    /// The store, and the load for a copy, which go.
    gone: Vec<Inst>,
}

/// What the call after the loop is given as its second operand.
enum With {
    /// A fill with this byte, which is a number.
    Byte(i128),
    /// A fill with this byte, which is a value the loop does not change.
    Value(Value),
    /// A copy from here.
    From(Anchor, Plain),
}

/// Where the call after a loop goes, as in [`crate::sink`].
#[derive(Clone, Copy, Debug)]
enum Exit {
    Own(Block),
    Shared { from: Block, to: Block },
}

/// How many bytes the loop writes, as a number or as a recipe the exit block follows.
#[derive(Clone, Copy, Debug)]
enum Extent {
    Bytes(u64),
    Computed { count: Plain, step: i128, reach: i128, reading: Reading },
}

/// The plan for one loop, nothing for a loop with no store to think about, or why it stays.
fn planned(
    func: &Func,
    cfg: &Cfg,
    doms: &Dominators,
    loops: &Loops,
    scev: &mut Scev<'_>,
    ranges: &mut Ranges<'_>,
    id: LoopId,
) -> Result<Option<Plan>, &'static str> {
    let insts: Vec<Inst> = loops.blocks(id).iter().flat_map(|&block| func.insts(block)).collect();
    let stores: Vec<Inst> =
        insts.iter().copied().filter(|&inst| func[inst].opcode == Opcode::Store).collect();
    // A loop with no store has nothing for this pass, and one with two is a loop this pass does not
    // look at, so neither says anything.
    let [store] = stores[..] else { return Ok(None) };
    let [value, to] = func[func[store].args] else { return Ok(None) };

    let (preheader, guard) = shaped(func, cfg, doms, loops, id).map_err(|_| NOT_SHAPED)?;
    let [exit] = loops.exits(id) else { return Err(NOT_SHAPED) };
    let exit = if *cfg.predecessors(exit.to) == [exit.from] {
        Exit::Own(exit.to)
    } else if func.terminator(exit.from).is_some_and(|last| func[last].opcode == Opcode::IndirectBr)
    {
        return Err(COMPUTED_EXIT);
    } else {
        Exit::Shared { from: exit.from, to: exit.to }
    };
    let block = func.block_of(store).ok_or(NOT_EVERY_TIME)?;
    if !doms.dominates(block, guard) {
        return Err(NOT_EVERY_TIME);
    }
    let stored = access(func, store).ok_or(NOT_AN_IDIOM)?;
    let width = i128::from(func[value].ty.bits() / 8);
    if func[value].ty.bits() == 0 || func[value].ty.bits() % 8 != 0 || func[value].ty.lanes() != 1 {
        return Err(NOT_AN_IDIOM);
    }

    // What is stored, and the load behind it for a copy.
    let load = copied(func, loops, id, block, value);
    let fill = match load {
        Some(_) => None,
        None => Some(filled(func, loops, id, value).ok_or(NOT_AN_IDIOM)?),
    };
    let mut gone = vec![store];
    gone.extend(load);
    let others = insts.iter().filter(|inst| !gone.contains(inst));
    let last: Vec<Option<Inst>> =
        loops.blocks(id).iter().map(|&block| func.terminator(block)).collect();
    for &inst in others {
        if func[inst].opcode.has_effects() && !last.contains(&Some(inst)) {
            return Err(IN_THE_WAY);
        }
    }

    let around = counted(scev, id).map_err(|_| NOT_COUNTED)?;
    let walk = |scev: &mut Scev<'_>, at: Value| -> Result<(Anchor, Plain), &'static str> {
        let Evolution::Affine(chrec) = scev.evolution(id, at) else { return Err(NOT_A_WALK) };
        if chrec.step.as_number() != Some(width) {
            return Err(NOT_A_WALK);
        }
        let (anchor, start) = anchored(chrec.base).ok_or(NOT_A_WALK)?;
        plain_enough(func, start).map_err(|_| NOT_A_WALK)?;
        Ok((anchor, start))
    };
    let dest = walk(scev, to)?;
    let (kind, with, align) = match (load, fill) {
        (None, Some(fill)) => (Opcode::Memset, fill, stored.align),
        (None, None) => return Err(NOT_AN_IDIOM),
        (Some(load), _) => {
            let from = func[func[load].args][0];
            let source = walk(scev, from)?;
            let read = access(func, load).ok_or(NOT_AN_IDIOM)?;
            let kind = apart(func, &stored, &read, dest, source).ok_or(MAY_OVERLAP)?;
            (kind, With::From(source.0, source.1), stored.align.min(read.align))
        }
    };

    // Every iteration stores, including the last, which is one more store than times round.
    let span = match around {
        Around::Number(around) => {
            let span = around
                .checked_mul(width)
                .and_then(|far| far.checked_add(width))
                .filter(|&span| span <= i128::from(i64::MAX))
                .ok_or(TOO_WIDE)?;
            Extent::Bytes(u64::try_from(span).map_err(|_| TOO_WIDE)?)
        }
        Around::Computed(count, reading) => {
            fits(func, ranges, preheader, count, width, width, reading).map_err(|_| TOO_WIDE)?;
            Extent::Computed { count, step: width, reach: width, reading }
        }
    };
    Ok(Some(Plan { exit, kind, to: dest, with, span, align, gone }))
}

/// The access an instruction makes, when it is an ordinary one this pass may take out of a loop.
fn access(func: &Func, inst: Inst) -> Option<MemInfo> {
    let data = &func[inst];
    if data.flags.contains(Flags::VOLATILE) || func.carries_mem(inst) {
        return None;
    }
    let Extra::Mem(mem) = data.extra else { return None };
    let info = func[mem];
    (info.order == MemOrder::NotAtomic).then_some(info)
}

/// The load a stored value is, when it is one this loop does every iteration for the store alone.
fn copied(func: &Func, loops: &Loops, id: LoopId, block: Block, value: Value) -> Option<Inst> {
    let Def::Result { inst, .. } = func[value].def else { return None };
    if func[inst].opcode != Opcode::Load || func.block_of(inst) != Some(block) {
        return None;
    }
    let _ = (loops, id);
    (crate::uses::count(func)[value.index()] == 1).then_some(inst)
}

/// The byte a stored value is every byte of, when the loop does not change it.
fn filled(func: &Func, loops: &Loops, id: LoopId, value: Value) -> Option<With> {
    let ty = func[value].ty;
    if let Some((imm, width)) = crate::fold::constant(func, value) {
        if !width.is_int() {
            return None;
        }
        let bits = imm.unsigned();
        let byte = bits & 0xff;
        let bytes = width.bits() / 8;
        return (0..bytes)
            .all(|at| (bits >> (8 * at)) & 0xff == byte)
            .then_some(With::Byte(byte as i128));
    }
    (ty.is_int() && ty.bits() == 8 && loops.is_invariant(func, id, value))
        .then_some(With::Value(value))
}

/// Which call copies the way the loop does, when one does.
fn apart(
    func: &Func,
    stored: &MemInfo,
    read: &MemInfo,
    to: (Anchor, Plain),
    from: (Anchor, Plain),
) -> Option<Opcode> {
    let (one, two) = (stored.restrict, read.restrict);
    if one.clique != 0 && one.clique == two.clique && one.base != two.base {
        return Some(Opcode::Memcpy);
    }
    let (here, there) = (object(func, to.0), object(func, from.0));
    let (ahead, behind) = match (here, there) {
        (Some((here, _)), Some((there, _))) if here != there => return Some(Opcode::Memcpy),
        (Some((_, ahead)), Some((_, behind))) => (ahead, behind),
        _ if to.0 == from.0 => (0, 0),
        _ => return None,
    };
    // One object, from two offsets that are numbers, with the store no later than the load.
    let number = |plain: Plain| plain.value.filter(|_| plain.scale != 0).is_none();
    if !number(to.1) || !number(from.1) {
        return None;
    }
    let start = ahead.checked_add(to.1.offset)?;
    let read = behind.checked_add(from.1.offset)?;
    (start <= read).then_some(Opcode::Memmove)
}

/// The object a walk is inside, when it is a local or a global of its own, and how far into it
/// the anchor is, named the same way however the address was written.
fn object(func: &Func, anchor: Anchor) -> Option<(Anchor, i128)> {
    let mut at = match anchor {
        Anchor::Address(_) => return Some((anchor, 0)),
        Anchor::Value(value) => value,
    };
    let mut offset: i128 = 0;
    // Bounded, so a chain of adds written by something odd is refused rather than followed.
    for _ in 0..8 {
        let Def::Result { inst, .. } = func[at].def else { return None };
        match (func[inst].opcode, func[inst].extra) {
            (Opcode::Alloca, _) => return Some((Anchor::Value(at), offset)),
            (Opcode::GlobalAddr, Extra::Symbol(symbol)) => {
                return Some((Anchor::Address(symbol), offset));
            }
            (Opcode::PtrAdd, _) => {
                let [base, by] = func[func[inst].args] else { return None };
                let (imm, ty) = crate::fold::constant(func, by)?;
                offset = offset.checked_add(imm.signed(ty))?;
                at = base;
            }
            _ => return None,
        }
    }
    None
}

/// Puts the call at the front of the block after the loop and takes the store out of the loop.
fn apply(func: &mut Func, plan: &Plan, exit: Block) {
    let first = func.insts(exit).next().expect("a block ends in a terminator");
    let mut made = Vec::new();
    let mut build = Builder::new(func, exit);
    let to = address(&mut build, &mut made, plan.to);
    let with = match plan.with {
        With::Byte(byte) => {
            let byte = build.iconst(Type::int(8), byte);
            made.push(byte);
            byte
        }
        With::Value(value) => value,
        With::From(anchor, start) => address(&mut build, &mut made, (anchor, start)),
    };
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
    let info = MemInfo {
        size: 0,
        align: plan.align,
        order: MemOrder::NotAtomic,
        tbaa: None,
        owns: 0,
        restrict: Restrict::NONE,
    };
    let mem = build.func().add_mem(info);
    let args = build.func().push_values(&[to, with, length]);
    let call =
        build.inst(InstData { args, extra: Extra::Mem(mem), ..InstData::new(plan.kind) }, &[]);

    for value in made {
        let inst = inst_of(func, value);
        func.remove_inst(inst);
        func.insert_before(inst, first);
    }
    func.remove_inst(call);
    func.insert_before(call, first);
    for &inst in &plan.gone {
        func.remove_inst(inst);
    }
}

/// The address a walk starts at.
fn address(
    build: &mut Builder<'_>,
    made: &mut Vec<Value>,
    (anchor, start): (Anchor, Plain),
) -> Value {
    let base = match anchor {
        Anchor::Value(value) => value,
        Anchor::Address(symbol) => {
            let extra = Extra::Symbol(symbol);
            let at =
                build.value(InstData { extra, ..InstData::new(Opcode::GlobalAddr) }, Type::PTR);
            made.push(at);
            at
        }
    };
    starting(build, made, base, start)
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;

    use super::{LoopIdiom, writes_itself};
    use crate::{Fuel, Pass};

    const HEAD: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "x86_64-unknown-linux-gnu"
target datalayout = "e-p:64:64-i64:64-f80:128-S128"
"#;

    /// The module that text is, with the pass run over every function in it under this much fuel.
    fn run(body: &str, fuel: &mut Fuel) -> String {
        let mut names = Interner::new();
        let text = format!("{HEAD}{body}");
        let mut module = rucc_ir::parse(&text, &mut names).expect("the fixture parses");
        let ids: Vec<_> = module.funcs().collect();
        for id in ids {
            if module[id].is_declaration() {
                continue;
            }
            let mut an = crate::machine::fixtures::analyses();
            LoopIdiom.run(&mut module[id], &mut an, fuel);
        }
        if let Err(errors) = rucc_ir::verify(&module, &names) {
            panic!("the pass left invalid IR, {errors:?}\n{}", rucc_ir::print(&module, &names));
        }
        rucc_ir::print(&module, &names)
    }

    fn all(body: &str) -> String {
        run(body, &mut Fuel::unlimited())
    }

    /// `for (int i = 0; i < n; i++) a[i] = x;`, with `%10`, what is stored, written by the caller.
    fn fill(stored: &str, shift: u32) -> String {
        format!(
            r"
func @z(ptr, i32, i8), linkage(external) {{
block0(%0: ptr, %1: i32, %2: i8):
    %3 = iconst.i32 0
    %4 = icmp slt %3, %1
    br_if %4, block3, block2

block1(%5: i32):
    %6 = sext.i64 %5
    %7 = iconst.i64 {shift}
    %8 = shl.nsw %6, %7
    %9 = ptr_add %0, %8
    %10 = {stored}
    store %10 -> %9, align 1
    %11 = iconst.i32 1
    %12 = add.nsw %5, %11
    %13 = icmp slt %12, %1
    br_if %13, block4, block2

block2:
    return

block3:
    jump block1(%3)

block4:
    jump block1(%12)
}}
"
        )
    }

    /// `for (int i = 0; i < n; i++) a[i] = b[i];`, where `a` and `b` are `to` and `from` moved on
    /// by so many bytes, and the load and the store carry these annotations. The two pointer
    /// arguments are `%0` and `%1`, and the globals `@g` and `@h` are `%3` and `%4`.
    fn copy(to: (&str, u32), from: (&str, u32), load: &str, store: &str) -> String {
        let ((to, ahead), (from, behind)) = (to, from);
        format!(
            r"
global @g : bytes 256 = {{ zero 256 }}, align 4, linkage(internal)
global @h : bytes 256 = {{ zero 256 }}, align 4, linkage(internal)

func @c(ptr, ptr, i32), linkage(external) {{
block0(%0: ptr, %1: ptr, %2: i32):
    %3 = global_addr @g
    %4 = global_addr @h
    %5 = iconst.i32 0
    %6 = icmp slt %5, %2
    br_if %6, block3, block2

block1(%7: i32):
    %8 = sext.i64 %7
    %9 = iconst.i64 2
    %10 = shl.nsw %8, %9
    %11 = iconst.i64 {ahead}
    %12 = ptr_add {to}, %11
    %13 = iconst.i64 {behind}
    %14 = ptr_add {from}, %13
    %15 = ptr_add %12, %10
    %16 = ptr_add %14, %10
    %17 = load.i32 %16, align 4{load}
    store %17 -> %15, align 4{store}
    %18 = iconst.i32 1
    %19 = add.nsw %7, %18
    %20 = icmp slt %19, %2
    br_if %20, block4, block2

block2:
    return

block3:
    jump block1(%5)

block4:
    jump block1(%19)
}}
"
        )
    }

    #[test]
    fn a_zero_fill_becomes_memset() {
        let out = all(&fill("iconst.i32 0", 2));
        assert!(out.contains("memset"), "{out}");
        assert!(!out.contains("store"), "{out}");
    }

    #[test]
    fn a_fill_whose_bytes_are_all_the_same_becomes_memset() {
        let out = all(&fill("iconst.i32 -1", 2));
        assert!(out.contains("memset"), "{out}");
    }

    #[test]
    fn a_fill_whose_bytes_differ_stays() {
        let out = all(&fill("iconst.i32 1", 2));
        assert!(!out.contains("memset"), "{out}");
        assert!(out.contains("store"), "{out}");
    }

    #[test]
    fn a_byte_the_loop_does_not_change_is_a_fill() {
        let out = all(&fill("iconst.i8 0", 0).replace("store %10", "store %2"));
        assert!(out.contains("memset"), "{out}");
    }

    #[test]
    fn a_store_with_a_gap_after_it_stays() {
        // Four byte elements, a stride of eight, so half the range is never written.
        let out = all(&fill("iconst.i32 0", 3));
        assert!(!out.contains("memset"), "{out}");
    }

    #[test]
    fn a_volatile_store_stays() {
        let text = fill("iconst.i32 0", 2).replace("store %10", "store.volatile %10");
        let out = all(&text);
        assert!(!out.contains("memset"), "{out}");
    }

    #[test]
    fn a_restrict_copy_becomes_memcpy() {
        let out = all(&copy(("%0", 0), ("%1", 0), ", restrict(1, 2)", ", restrict(1, 1)"));
        assert!(out.contains("memcpy"), "{out}");
        assert!(!out.contains("store"), "{out}");
        assert!(!out.contains("load"), "{out}");
    }

    #[test]
    fn a_copy_between_plain_pointers_stays() {
        let out = all(&copy(("%0", 0), ("%1", 0), "", ""));
        assert!(!out.contains("memcpy") && !out.contains("memmove"), "{out}");
    }

    #[test]
    fn a_copy_between_two_globals_becomes_memcpy() {
        let out = all(&copy(("%3", 0), ("%4", 0), "", ""));
        assert!(out.contains("memcpy"), "{out}");
    }

    #[test]
    fn a_copy_down_within_one_global_becomes_memmove() {
        let text = copy(("%3", 0), ("%3", 4), "", "");
        let out = all(&text);
        assert!(out.contains("memmove"), "{out}");
    }

    #[test]
    fn a_copy_up_within_one_global_stays() {
        let text = copy(("%3", 4), ("%3", 0), "", "");
        let out = all(&text);
        assert!(!out.contains("memmove") && !out.contains("memcpy"), "{out}");
    }

    #[test]
    fn fuel_stops_the_rewriting() {
        let text = fill("iconst.i32 0", 2);
        let out = run(&text, &mut Fuel::of(0));
        assert!(!out.contains("memset"), "{out}");
    }

    #[test]
    fn the_library_routines_are_named() {
        assert!(writes_itself("memset") && writes_itself("memcpy") && writes_itself("memmove"));
        assert!(!writes_itself("fill"));
    }
}
