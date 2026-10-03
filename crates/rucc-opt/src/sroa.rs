//! Scalar replacement of aggregates, which turns a local whose address goes nowhere into one value
//! for each piece of it the function reads or writes.
//!
//! Design: `spec/optimizer/18-sroa.md`. A local is an `alloca` of a fixed size, and it is a
//! candidate when every use of its address is a load, a store to it, a `ptr_add` by a constant, a
//! `memset` with a constant byte, or a `memcpy` or `memmove` of a constant length between it and
//! some other memory. Anything else, an address passed to a call, stored somewhere, compared,
//! turned into an integer or handed to a block, and the local stays where it is. That is the escape
//! check of section 18.2 and it is a whitelist on purpose, since a use the pass does not know is a
//! use that could read the bytes behind its back.
//!
//! The pieces come from the loads and stores. Every access starts and ends at a cut, and the bytes
//! between two neighbouring cuts that some access covers are one piece. An access that covers one
//! piece reads or writes that piece, converting between an integer and a pointer or a float of the
//! same width where the two sides disagree. An access that covers several is an integer read or
//! written as its pieces shifted into place, which is what `struct P q = p;` followed by a read of
//! `q.x` looks like once the copy has been through here, and it is only done on a little endian
//! target where the byte at the lowest address is the low one. Anything else that overlaps without
//! lining up, a float written over half of an integer, keeps the local in memory. That is the
//! partial overlap disqualification of section 18.2, and it is correctness rather than taste.
//!
//! A `memset` writes its byte into every piece it covers. A `memcpy` into the local reads each
//! piece it covers from the source, and one out of it writes each piece it covers to the
//! destination. Bytes a copy out moves that no load or store names get integer pieces of their
//! own, so that `a = b; c = a;` carries the bytes through. A piece a bulk operation covers only
//! part of keeps the local in memory, for the same reason a partial overlap does.
//!
//! Each piece is then an ordinary variable and gets ordinary SSA: a block parameter at the
//! iterated dominance frontier of the blocks that write it, pruned to the blocks where it is live,
//! and a walk down the dominator tree that hands each read the value in front of it. A piece read
//! before anything wrote it reads zero, which is one of the values an uninitialized local may
//! hold, and a constant is cheaper than anything else it could be.
//!
//! One local at a time, looking at the function afresh each time, because the loads and stores a
//! copy between two locals turns into are what makes the second one a candidate.
//!
//! # A vector
//!
//! A local of sixteen bytes that is read or written whole as a vector of four `int` or two `long`
//! is one piece of that vector type rather than bytes cut at every lane, on a target where the
//! back end keeps such a vector in a register. That is x86-64 with SSE2, and the module facts say
//! whether this build is one. A read of four or eight bytes at a lane is an `extractlane`, and a
//! write of one is an `insertlane`, which reads the vector the way a store of part of a piece
//! would. A copy of the whole sixteen bytes is a load or a store of the vector. Anything else
//! keeps the local in memory, as a narrower lane or a lane read across two would.
//!
//! This is what `<emmintrin.h>` code looks like after inlining: every intrinsic is a vector
//! operator on copies of its operands, or a walk over the lanes of one, and tamnd/rucc#2320 is the
//! vector going back and forth through memory between each of them.

use rucc_base::hash::{Map, Set};
use rucc_cost::heuristics::{SRA_MAX_BYTES, SRA_MAX_PIECES};
use rucc_ir::{
    Block, BlockCall, Extra, Flags, Func, Imm, Inst, InstData, MemInfo, MemOrder, Opcode, Restrict,
    Type, Value,
};

use crate::cfg::Cfg;
use crate::dom::Dominators;
use crate::frontier::Frontiers;
use crate::{Analyses, Analysis, Fuel, Pass, Preserved, Stats};

/// What the pipelines and the flags call it.
pub const NAME: &str = "sroa";

/// A local went.
const SCALARIZED: &str = "local replaced by values, one for each piece of it the function uses";

/// Two accesses overlap and neither is an integer made of the other's pieces.
const OVERLAP: &str = "local kept in memory, two accesses to it overlap without lining up";

/// A piece is read as a type it cannot become.
const MISMATCH: &str = "local kept in memory, a piece of it is read as a type it cannot become";

/// The bytes of one vector register, which is the size of a local this pass keeps in a register
/// as a vector. A narrower access to it is a lane, and nothing is wider.
const WHOLE: u64 = 16; // not a threshold: sixteen bytes is one xmm register, not a tuned limit.

/// An access is a vector, a long double or an integer that is not a whole number of bytes.
const WIDTH: &str = "local kept in memory, an access to it has a width the pass does not split";

/// More than the limits in `rucc_cost::heuristics` allow.
const TOO_BIG: &str = "local kept in memory, it has more pieces or bytes than the limit";

/// Ran out.
const NO_FUEL: &str = "local kept in memory, the pass ran out of fuel";

/// The widest piece a run of bytes nothing names is cut into, in bytes, which is a register.
const WIDEST_FILLER: u64 = 8;

// The renaming keeps the pieces a block reads and writes as the bits of one word.
const _: () = assert!(SRA_MAX_PIECES <= u64::BITS);

/// The pass.
#[derive(Debug)]
pub struct Sroa;

impl Pass for Sroa {
    fn name(&self) -> &'static str {
        NAME
    }

    fn describe(&self) -> &'static str {
        "a local whose address goes nowhere becomes one value for each piece of it that is used"
    }

    fn preserves(&self) -> Preserved {
        // No edge moves and no block comes or goes. Blocks gain parameters and instructions come
        // and go, which is what liveness and pressure are statements about.
        Preserved::ALL.without(Analysis::Liveness).without(Analysis::Pressure)
    }

    fn run(&self, func: &mut Func, an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        let Some(entry) = func.entry() else {
            return stats;
        };
        let target = Target {
            pointer: an.outside().pointer_bytes(),
            little: an.outside().little_endian() == Some(true),
            vectors: an.outside().vectors() && an.outside().little_endian() == Some(true),
        };
        let an = &*an;
        let cfg = an.cfg(func);
        if !shaped(func, cfg, entry) {
            return stats;
        }
        let doms = an.dominators(func);
        let frontiers = an.frontiers(func);
        // Where each block is in reverse postorder and where it is in the layout, which are the
        // orders the walks below have to find things in. No block comes or goes, so both hold for
        // every local.
        let mut rpo = vec![0; func.counts().blocks];
        for (rank, block) in cfg.reverse_postorder().enumerate() {
            rpo[block.index()] = rank;
        }
        let mut layout = vec![0; func.counts().blocks];
        for (rank, block) in func.blocks().enumerate() {
            layout[block.index()] = rank;
        }
        let (pre, last) = preorder(doms, entry, func.counts().blocks);
        let allocas: Vec<Inst> = func
            .insts(entry)
            .filter(|&inst| func[inst].opcode == Opcode::Alloca && func[func[inst].args].is_empty())
            .collect();
        let mut readers = Readers::new(func);
        for alloca in allocas {
            let plan = match plan(func, &rpo, &mut readers, alloca, target) {
                Ok(Some(plan)) => plan,
                Ok(None) => continue,
                Err(reason) => {
                    stats.missed(reason);
                    continue;
                }
            };
            if !fuel.take() {
                stats.missed(NO_FUEL);
                continue;
            }
            let mut rewrite = Rewrite::new(func, entry, &plan, target);
            let graph = Graph { cfg, frontiers, entry, layout: &layout, pre: &pre, last: &last };
            rewrite.run(func, graph, &mut readers);
            stats.optimized(SCALARIZED);
        }
        stats
    }
}

/// What the pass needs to know about the machine.
#[derive(Clone, Copy, Debug)]
struct Target {
    /// How wide an address is, which is also the one integer width a pointer converts to.
    pointer: Option<u64>,
    /// Whether the byte at the lowest address is the low byte of an integer.
    little: bool,
    /// Whether a vector of four `int` or two `long` may be one value, see the module docs.
    vectors: bool,
}

/// The analyses the rewrite reads, none of which it changes.
#[derive(Clone, Copy)]
struct Graph<'a> {
    cfg: &'a Cfg,
    frontiers: &'a Frontiers,
    entry: Block,
    /// Where each block is in the layout, by block.
    layout: &'a [usize],
    /// Where each block is in the walk down the dominator tree, by block, or `usize::MAX` for a
    /// block the walk does not reach.
    pre: &'a [usize],
    /// The last place in that walk that is still under each block, by block.
    last: &'a [usize],
}

/// Numbers the blocks in the order the renaming goes down the dominator tree, with the children of
/// a block taken last first, and says where each block's subtree ends.
fn preorder(doms: &Dominators, entry: Block, count: usize) -> (Vec<usize>, Vec<usize>) {
    let mut pre = vec![usize::MAX; count];
    let mut last = vec![0; count];
    let mut next = 0;
    let mut stack = vec![(entry, true)];
    while let Some((block, entering)) = stack.pop() {
        if entering {
            pre[block.index()] = next;
            next += 1;
            stack.push((block, false));
            stack.extend(doms.children(block).map(|child| (child, true)));
        } else {
            last[block.index()] = next - 1;
        }
    }
    (pre, last)
}

/// Every instruction that reads each value, kept up to date as the locals go.
///
/// The pass used to find what it needed by walking the whole function, once to plan each local
/// and once more to point the readers of its loads at the values that replaced them. A function
/// with a couple of thousand locals walked itself a few thousand times over, and on jtckdint that
/// was most of an optimized compile. An instruction removed since it was filed stays on the lists,
/// and whoever reads them skips it.
struct Readers {
    /// By value.
    of: Vec<Vec<Inst>>,
    /// How many instructions the function had the last time this looked, which makes the ones
    /// past it the ones made since.
    seen: usize,
}

impl Readers {
    fn new(func: &Func) -> Self {
        let mut readers = Self { of: vec![Vec::new(); func.counts().values], seen: 0 };
        for block in func.blocks() {
            for inst in func.insts(block) {
                readers.add(func, inst);
            }
        }
        readers.seen = func.counts().insts;
        readers
    }

    /// Files an instruction under everything it reads, its own arguments and the ones it passes
    /// along an edge.
    fn add(&mut self, func: &Func, inst: Inst) {
        for &value in &func[func[inst].args] {
            self.push(value, inst);
        }
        for call in func.successors(inst) {
            for &value in &func[call.args] {
                self.push(value, inst);
            }
        }
    }

    fn push(&mut self, value: Value, inst: Inst) {
        let index = value.index();
        if self.of.len() <= index {
            self.of.resize_with(index + 1, Vec::new);
        }
        self.of[index].push(inst);
    }

    /// Files the instructions made since the last look.
    fn catch_up(&mut self, func: &Func) {
        let count = func.counts().insts;
        for index in self.seen..count {
            let inst = Inst::from_usize(index);
            if func.block_of(inst).is_some() {
                self.add(func, inst);
            }
        }
        self.seen = count;
    }

    /// The instructions filed as reading a value, some of which may have been removed since.
    fn of(&self, value: Value) -> &[Inst] {
        self.of.get(value.index()).map_or(&[][..], Vec::as_slice)
    }
}

/// Whether the function is one the renaming can walk.
///
/// Every block has to be on the dominator tree, which a block nothing reaches is not, and every
/// edge has to be one a parameter can be passed along, which an indirect branch and a call that
/// unwinds to a pad are not. An `asm goto` is one, since its labels are blocks with arguments like
/// any other branch's. A function on the memory chain is left alone because the loads and
/// stores the pass writes would have to be threaded onto it.
fn shaped(func: &Func, cfg: &Cfg, entry: Block) -> bool {
    if !cfg.predecessors(entry).is_empty() {
        return false;
    }
    for block in func.blocks() {
        if !cfg.reaches(block) {
            return false;
        }
        for inst in func.insts(block) {
            if func.carries_mem(inst) {
                return false;
            }
            if func.is_terminator(inst) {
                let opcode = func[inst].opcode;
                let plain = matches!(
                    opcode,
                    Opcode::Jump
                        | Opcode::BrIf
                        | Opcode::Switch
                        | Opcode::Return
                        | Opcode::Unreachable
                        | Opcode::InlineAsm
                );
                if !plain {
                    return false;
                }
            } else if func.successors(inst).next().is_some() {
                return false;
            }
        }
    }
    true
}

/// A run of bytes of the local that becomes one value, and the type of that value.
#[derive(Clone, Copy, Debug)]
struct Piece {
    at: u64,
    size: u64,
    ty: Type,
}

impl Piece {
    fn within(&self, at: u64, size: u64) -> bool {
        at <= self.at && self.at + self.size <= at + size
    }

    fn meets(&self, at: u64, size: u64) -> bool {
        self.at < at + size && at < self.at + self.size
    }
}

/// One instruction that reads or writes the local, with offsets from the start of it.
#[derive(Clone, Copy, Debug)]
enum Use {
    Load { at: u64, size: u64, ty: Type },
    Store { at: u64, size: u64, ty: Type, value: Value },
    Fill { at: u64, size: u64, byte: u8 },
    CopyIn { at: u64, size: u64, from: Value, align: u32 },
    CopyOut { at: u64, size: u64, to: Value, align: u32 },
}

impl Use {
    /// The bytes it reads or writes.
    const fn range(&self) -> (u64, u64) {
        match *self {
            Self::Load { at, size, .. }
            | Self::Store { at, size, .. }
            | Self::Fill { at, size, .. }
            | Self::CopyIn { at, size, .. }
            | Self::CopyOut { at, size, .. } => (at, size),
        }
    }

    const fn scalar(&self) -> Option<(u64, u64, Type)> {
        match *self {
            Self::Load { at, size, ty } | Self::Store { at, size, ty, .. } => Some((at, size, ty)),
            _ => None,
        }
    }

    const fn reads(&self) -> bool {
        matches!(self, Self::Load { .. } | Self::CopyOut { .. })
    }
}

/// What to do to one local.
#[derive(Debug)]
struct Plan {
    alloca: Inst,
    pieces: Vec<Piece>,
    /// Whether the one piece is a vector whose lanes the narrower accesses read and write.
    vector: bool,
    /// Every load, store and bulk operation on it, in reverse postorder.
    uses: Vec<(Inst, Use)>,
    /// Every `ptr_add` into it, which go once nothing reads them.
    derived: Vec<Inst>,
    /// Every `lifetime_end` of it, which go with it, since a local in values has no bytes in the
    /// frame for anything to share.
    ends: Vec<Inst>,
}

/// What one use of an address into the local turned out to be.
enum Found {
    /// Another address into it, this far in.
    Derived(Value, u64),
    Use(Use),
    /// The end of its lifetime, which says nothing about its bytes.
    End,
}

/// The plan for one local, or nothing where its address escapes, or why it has to stay.
fn plan(
    func: &Func,
    rpo: &[usize],
    readers: &mut Readers,
    alloca: Inst,
    target: Target,
) -> Result<Option<Plan>, &'static str> {
    if func[alloca].flags.contains(Flags::VOLATILE) {
        return Ok(None);
    }
    let Extra::Mem(mem) = func[alloca].extra else {
        return Ok(None);
    };
    let size = func[mem].size;
    let Some(base) = func[alloca].first_result else {
        return Ok(None);
    };
    let mut offsets: Map<Value, u64> = Map::from_iter([(base, 0)]);
    let mut uses = Vec::new();
    let mut derived = Vec::new();
    let mut ends = Vec::new();
    // Reverse postorder puts every definition in front of its uses, so an address is known by the
    // time anything reads it.
    readers.catch_up(func);
    for inst in reading(func, rpo, readers, base) {
        let named = func[func[inst].args].iter().any(|value| offsets.contains_key(value))
            || func
                .successors(inst)
                .any(|call| func[call.args].iter().any(|value| offsets.contains_key(value)));
        if inst == alloca || !named {
            continue;
        }
        match access(func, inst, &offsets, size, target)? {
            Some(Found::Derived(value, at)) => {
                offsets.insert(value, at);
                derived.push(inst);
            }
            Some(Found::Use(found)) => uses.push((inst, found)),
            Some(Found::End) => ends.push(inst),
            None => return Ok(None),
        }
    }
    // A vector, or the float `<emmintrin.h>` copies one as next to a lane of it, which is the
    // header building a vector out of its lanes in memory.
    let scalar = |pick: fn(u64, Type) -> bool| {
        uses.iter().any(|(_, found)| found.scalar().is_some_and(|(_, width, ty)| pick(width, ty)))
    };
    let vector = scalar(|_, ty| ty.is_vector())
        || scalar(|_, ty| is_quad(ty))
            && scalar(|width, ty| !is_quad(ty) && matches!(width, 4 | 8));
    let pieces = if vector { whole(&uses, size)? } else { pieces(&uses, target)? };
    Ok(Some(Plan { alloca, pieces, vector, uses, derived, ends }))
}

/// Every instruction that might name an address into the local, in reverse postorder and in order
/// within a block, which is the order a walk over the whole function would meet them in.
///
/// That is the readers of the local's address, then the readers of what each `ptr_add` among them
/// makes, and so on down. It is more than [`plan`] wants, since a `ptr_add` can add the address to
/// something rather than something to it, and [`plan`] asks each of them the question it would
/// have asked on the walk.
fn reading(func: &Func, rpo: &[usize], readers: &Readers, base: Value) -> Vec<Inst> {
    let mut found = Vec::new();
    let mut seen: Set<Inst> = Set::default();
    let mut work = vec![base];
    while let Some(value) = work.pop() {
        for &inst in readers.of(value) {
            if func.block_of(inst).is_none() || !seen.insert(inst) {
                continue;
            }
            found.push(inst);
            if func[inst].opcode == Opcode::PtrAdd {
                work.extend(func[inst].first_result);
            }
        }
    }
    let block = |inst: Inst| func.block_of(inst).expect("only placed instructions were kept");
    found.sort_by_key(|&inst| rpo[block(inst).index()]);
    // Which of two instructions in one block comes first is only known by walking it, so a block
    // with more than one is walked, and only as far as the last of them.
    let mut start = 0;
    while start < found.len() {
        let here = block(found[start]);
        let end = start + found[start..].iter().take_while(|&&inst| block(inst) == here).count();
        if end - start > 1 {
            let mut at = start;
            for inst in func.insts(here) {
                if seen.contains(&inst) {
                    found[at] = inst;
                    at += 1;
                    if at == end {
                        break;
                    }
                }
            }
        }
        start = end;
    }
    found
}

/// What an instruction naming an address into the local does with it, or nothing where it is
/// something the pass cannot follow.
fn access(
    func: &Func,
    inst: Inst,
    offsets: &Map<Value, u64>,
    size: u64,
    target: Target,
) -> Result<Option<Found>, &'static str> {
    if func.successors(inst).next().is_some() {
        // A terminator, and one that names the address, since the caller asked. Handing an address
        // to a block is an escape as far as this pass is concerned.
        return Ok(None);
    }
    let data = &func[inst];
    let args = &func[data.args];
    let volatile = data.flags.contains(Flags::VOLATILE);
    let at = |value: Value| offsets.get(&value).copied();
    let found = match (data.opcode, args) {
        (Opcode::PtrAdd, &[from, by]) => {
            let (Some(start), None) = (at(from), at(by)) else {
                return Ok(None);
            };
            let Some((imm, ty)) = crate::fold::constant(func, by) else {
                return Ok(None);
            };
            // One past the end is an address C lets a program make, and anything further out is
            // not something to reason about.
            let Ok(moved) = u64::try_from(i128::from(start) + imm.signed(ty)) else {
                return Ok(None);
            };
            if moved > size {
                return Ok(None);
            }
            let Some(result) = data.first_result else {
                return Ok(None);
            };
            Found::Derived(result, moved)
        }
        (Opcode::Load, &[address]) if !volatile => {
            let (Some(start), Some(result)) = (at(address), data.first_result) else {
                return Ok(None);
            };
            let ty = func[result].ty;
            let Some(width) = width(ty, target) else {
                return Err(WIDTH);
            };
            if start + width > size {
                return Ok(None);
            }
            Found::Use(Use::Load { at: start, size: width, ty })
        }
        (Opcode::Store, &[value, address]) if !volatile => {
            let (Some(start), None) = (at(address), at(value)) else {
                return Ok(None);
            };
            let ty = func[value].ty;
            let Some(width) = width(ty, target) else {
                return Err(WIDTH);
            };
            if start + width > size {
                return Ok(None);
            }
            Found::Use(Use::Store { at: start, size: width, ty, value })
        }
        (Opcode::LifetimeEnd, &[address]) if at(address).is_some() => Found::End,
        (Opcode::Memset | Opcode::Memcpy | Opcode::Memmove, _) if !volatile => {
            let Some(found) = bulk(func, inst, offsets, size) else {
                return Ok(None);
            };
            Found::Use(found)
        }
        _ => return Ok(None),
    };
    Ok(Some(found))
}

/// A `memset`, `memcpy` or `memmove` over the local, where its length is a constant that stays
/// inside it.
fn bulk(func: &Func, inst: Inst, offsets: &Map<Value, u64>, size: u64) -> Option<Use> {
    let bulk = func.bulk(inst)?;
    let Extra::Mem(mem) = func[inst].extra else {
        return None;
    };
    let info = &func[mem];
    let length = match bulk.length {
        None => info.size,
        Some(length) => u64::try_from(crate::fold::constant(func, length)?.0.unsigned()).ok()?,
    };
    let at = |value: Value| offsets.get(&value).copied();
    let (start, found) = if func[inst].opcode == Opcode::Memset {
        let start = at(bulk.to)?;
        let byte = crate::fold::constant(func, bulk.with)?.0.unsigned().to_le_bytes()[0];
        (start, Use::Fill { at: start, size: length, byte })
    } else {
        match (at(bulk.to), at(bulk.with)) {
            (Some(start), None) => {
                (start, Use::CopyIn { at: start, size: length, from: bulk.with, align: info.align })
            }
            (None, Some(start)) => {
                (start, Use::CopyOut { at: start, size: length, to: bulk.to, align: info.align })
            }
            // Both ends in the same local is a copy within it, and it is not worth following.
            _ => return None,
        }
    };
    (length > 0 && start + length <= size).then_some(found)
}

/// How many bytes a value of the type is in memory, for the types the pass splits.
fn width(ty: Type, target: Target) -> Option<u64> {
    if ty.is_vector() {
        return (target.vectors && rucc_ir::term::vector_slot(ty).is_some()).then_some(16);
    }
    if is_quad(ty) {
        return target.vectors.then_some(16);
    }
    if ty.is_ptr() {
        return target.pointer;
    }
    let bits = ty.bits();
    let whole = (ty.is_int() && matches!(bits, 8 | 16 | 32 | 64))
        || (ty.is_float() && matches!(bits, 16 | 32 | 64));
    whole.then_some(u64::from(bits / 8))
}

/// Whether the type is the sixteen byte float, which is how a function hands an `__m128i` in and
/// out on x86-64 and so how `<emmintrin.h>` copies one whole. It is only ever a copy of a vector
/// piece here, see [`whole`].
fn is_quad(ty: Type) -> bool {
    ty == Type::float(rucc_ir::Float::F128)
}

/// The integer type that many bytes wide.
fn integer(bytes: u64) -> Type {
    Type::int(u32::try_from(bytes * 8).expect("a piece is a register wide at most"))
}

/// Whether a value of one type can stand for a value of another over the same bytes.
fn convertible(from: Type, to: Type, pointer: Option<u64>) -> bool {
    let address = |ptr: Type, int: Type| {
        ptr.is_ptr() && int.is_int() && pointer == Some(u64::from(int.bits() / 8))
    };
    let punned = (from.is_int() && to.is_float() || from.is_float() && to.is_int())
        && from.bits() == to.bits();
    from == to || address(from, to) || address(to, from) || punned
}

/// The one vector piece of a local that is read or written whole as a vector, or why it has to
/// stay in memory.
fn whole(uses: &[(Inst, Use)], size: u64) -> Result<Vec<Piece>, &'static str> {
    let mut ty = None;
    let mut lane = None;
    for (_, found) in uses {
        let (at, width) = found.range();
        match found.scalar() {
            Some((_, _, it)) if it.is_vector() => {
                ty.get_or_insert(it);
            }
            // The vector copied whole as the float the calling convention passes it as, which
            // is the same register and so only a bitcast away.
            Some((_, _, it)) if is_quad(it) && at == 0 && width == size => {}
            Some((..)) if matches!(width, 4 | 8) && at % width == 0 => {
                lane.get_or_insert(width);
            }
            Some(_) => return Err(OVERLAP),
            None if at == 0 && width == size => {}
            None => return Err(OVERLAP),
        }
    }
    // Only the float and lanes of it, so the vector is lanes as wide as the first one.
    let lanes = |width: u64| {
        let count = u32::try_from(size / width).expect("a lane is four or eight bytes");
        Type::vector(integer(width), count)
    };
    let ty = match (ty, lane) {
        (Some(ty), _) => ty,
        (None, Some(width)) if size == WHOLE => lanes(width),
        _ => return Err(OVERLAP),
    };
    if size != WHOLE {
        return Err(OVERLAP);
    }
    Ok(vec![Piece { at: 0, size, ty }])
}

/// The pieces the accesses cut the local into, or why it cannot be cut.
fn pieces(uses: &[(Inst, Use)], target: Target) -> Result<Vec<Piece>, &'static str> {
    let scalars: Vec<(u64, u64, Type)> =
        uses.iter().filter_map(|(_, found)| found.scalar()).collect();
    // The sixteen byte float is one piece or nothing, since it cannot be put together out of
    // smaller ones the way an integer can.
    let quad = scalars.iter().any(|&(.., ty)| is_quad(ty));
    if quad && scalars.iter().any(|&(at, _, ty)| at != 0 || !is_quad(ty)) {
        return Err(OVERLAP);
    }
    let mut cuts: Vec<u64> = scalars.iter().flat_map(|&(at, size, _)| [at, at + size]).collect();
    cuts.sort_unstable();
    cuts.dedup();
    let mut pieces = Vec::new();
    for pair in cuts.windows(2) {
        let (at, size) = (pair[0], pair[1] - pair[0]);
        let mut covered = false;
        let mut exact = None;
        for &(start, width, ty) in &scalars {
            if start <= at && at + size <= start + width {
                covered = true;
                if start == at && width == size && exact.is_none() {
                    exact = Some(ty);
                }
            }
        }
        if !covered {
            continue;
        }
        let ty = match exact {
            Some(ty) => ty,
            None if matches!(size, 1 | 2 | 4 | 8) => integer(size),
            None => return Err(OVERLAP),
        };
        pieces.push(Piece { at, size, ty });
    }

    for &(at, size, ty) in &scalars {
        let covered: Vec<&Piece> = pieces.iter().filter(|piece| piece.within(at, size)).collect();
        if let [piece] = covered[..] {
            if !convertible(piece.ty, ty, target.pointer) {
                return Err(MISMATCH);
            }
            continue;
        }
        if !target.little || !ty.is_int() {
            return Err(OVERLAP);
        }
        if covered.iter().any(|piece| !convertible(piece.ty, integer(piece.size), target.pointer)) {
            return Err(MISMATCH);
        }
    }

    let bulk: Vec<(u64, u64, bool)> = uses
        .iter()
        .filter(|(_, found)| found.scalar().is_none())
        .map(|(_, found)| {
            let (at, size) = found.range();
            (at, size, found.reads())
        })
        .collect();
    if bulk.iter().any(|&(_, _, reads)| reads) {
        fill(&mut pieces, &bulk)?;
    }

    for &(at, size, _) in &bulk {
        if pieces.iter().any(|piece| piece.meets(at, size) && !piece.within(at, size)) {
            return Err(OVERLAP);
        }
    }
    let bytes: u64 = pieces.iter().map(|piece| piece.size).sum();
    if pieces.len() > SRA_MAX_PIECES as usize || bytes > u64::from(SRA_MAX_BYTES) {
        return Err(TOO_BIG);
    }
    Ok(pieces)
}

/// Adds integer pieces for the bytes a copy out of the local moves that a bulk write put there and
/// no load or store names, so that the copy has something to write.
fn fill(pieces: &mut Vec<Piece>, bulk: &[(u64, u64, bool)]) -> Result<(), &'static str> {
    let mut cuts: Vec<u64> =
        pieces.iter().flat_map(|piece| [piece.at, piece.at + piece.size]).collect();
    cuts.extend(bulk.iter().flat_map(|&(at, size, _)| [at, at + size]));
    cuts.sort_unstable();
    cuts.dedup();
    let mut added = Vec::new();
    for pair in cuts.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        let inside = |reads: bool| {
            bulk.iter().any(|&(at, size, read)| read == reads && at <= start && end <= at + size)
        };
        if !inside(true)
            || !inside(false)
            || pieces.iter().any(|piece| piece.meets(start, end - start))
        {
            continue;
        }
        let mut at = start;
        while at < end {
            let mut size = WIDEST_FILLER;
            while at % size != 0 || at + size > end {
                size /= 2;
            }
            added.push(Piece { at, size, ty: integer(size) });
            at += size;
            if pieces.len() + added.len() > SRA_MAX_PIECES as usize {
                return Err(TOO_BIG);
            }
        }
    }
    pieces.extend(added);
    pieces.sort_unstable_by_key(|piece| piece.at);
    Ok(())
}

/// The rewrite of one local, and the state it carries while it walks.
struct Rewrite<'a> {
    plan: &'a Plan,
    target: Target,
    /// The instruction at the top of the entry block, which is where zeros go.
    top: Inst,
    /// The zero of each piece, made the first time something reads a piece nothing wrote.
    zeros: Vec<Option<Value>>,
}

impl<'a> Rewrite<'a> {
    fn new(func: &Func, entry: Block, plan: &'a Plan, target: Target) -> Self {
        let top = func.insts(entry).next().expect("the entry block holds the alloca");
        Self { plan, target, top, zeros: vec![None; plan.pieces.len()] }
    }

    /// The pieces inside a run of bytes, as a set.
    fn mask(&self, at: u64, size: u64) -> u64 {
        let mut mask = 0;
        for (index, piece) in self.plan.pieces.iter().enumerate() {
            if piece.within(at, size) || self.plan.vector && piece.meets(at, size) {
                mask |= 1 << index;
            }
        }
        mask
    }

    /// The pieces inside a run of bytes, lowest address first.
    fn within(&self, at: u64, size: u64) -> Vec<usize> {
        (0..self.plan.pieces.len()).filter(|&k| self.plan.pieces[k].within(at, size)).collect()
    }

    fn run(&mut self, func: &mut Func, graph: Graph<'_>, readers: &mut Readers) {
        let count = self.plan.pieces.len();

        // Where each block's uses are in the plan. They are next to each other, since the plan
        // lists them in reverse postorder and in order within a block.
        let mut spans: Map<Block, (usize, usize)> = Map::default();
        for (index, &(inst, _)) in self.plan.uses.iter().enumerate() {
            let block = func.block_of(inst).expect("a use of the local is in a block");
            spans.entry(block).and_modify(|span| span.1 = index + 1).or_insert((index, index + 1));
        }

        // What each block reads before it writes, and what it writes. A block with no use of the
        // local does neither and is left out.
        let mut upward: Map<Block, u64> = Map::default();
        let mut writes: Map<Block, u64> = Map::default();
        for (&block, &(start, end)) in &spans {
            let (mut up, mut written) = (0, 0);
            for (_, found) in &self.plan.uses[start..end] {
                let (at, size) = found.range();
                let mask = self.mask(at, size);
                // A lane written is the rest of the vector read first.
                let lane = self.plan.vector && size < WHOLE && !found.reads();
                if found.reads() || lane {
                    up |= mask & !written;
                }
                if !found.reads() {
                    written |= mask;
                }
            }
            upward.insert(block, up);
            writes.insert(block, written);
        }
        let mask = |masks: &Map<Block, u64>, block: Block| masks.get(&block).copied().unwrap_or(0);

        // Where each piece is live on the way in, which is the least answer the equations have.
        // Worked back from the blocks that read something before writing it, so a block the
        // answer never reaches is never looked at.
        let mut live: Map<Block, u64> =
            upward.iter().filter(|&(_, &up)| up != 0).map(|(&block, &up)| (block, up)).collect();
        let mut work: Vec<Block> = live.keys().copied().collect();
        while let Some(block) = work.pop() {
            for &pred in graph.cfg.predecessors(block) {
                let out =
                    graph.cfg.successors(pred).iter().fold(0, |out, &next| out | mask(&live, next));
                let now = mask(&upward, pred) | (out & !mask(&writes, pred));
                if now != mask(&live, pred) {
                    live.insert(pred, now);
                    work.push(pred);
                }
            }
        }

        // A parameter for each piece at the iterated frontier of the blocks that write it, where
        // it is live. The entry block writes every piece, with the zero nobody has made yet. The
        // blocks start out in layout order, which is the order a walk over them all would find
        // them in and so the order the parameters are added in.
        let mut writers: Vec<Block> = writes
            .iter()
            .filter(|&(&block, &written)| written != 0 && block != graph.entry)
            .map(|(&block, _)| block)
            .collect();
        writers.push(graph.entry);
        writers.sort_unstable_by_key(|block| graph.layout[block.index()]);
        let mut params: Map<Block, Vec<(usize, Value)>> = Map::default();
        for (k, piece) in self.plan.pieces.iter().enumerate() {
            let bit = 1u64 << k;
            let mut work: Vec<Block> = writers
                .iter()
                .copied()
                .filter(|&block| block == graph.entry || mask(&writes, block) & bit != 0)
                .collect();
            let mut seen: Set<Block> = work.iter().copied().collect();
            let mut placed: Set<Block> = Set::default();
            while let Some(block) = work.pop() {
                for &join in graph.frontiers.of(block) {
                    if !placed.insert(join) {
                        continue;
                    }
                    if mask(&live, join) & bit != 0 {
                        let param = func.append_param(join, piece.ty);
                        params.entry(join).or_default().push((k, param));
                    }
                    if seen.insert(join) {
                        work.push(join);
                    }
                }
            }
        }

        // The renaming, down the dominator tree. Only the blocks that use the local, take a
        // parameter for it or pass one are visited, in the order a walk over the whole tree would
        // meet them. What a block changes is put back once the walk is out from under it rather
        // than each child getting a copy of its own, and what a block ends with is kept only for
        // a block an edge into a parameter leaves from.
        let mut sources: Set<Block> = Set::default();
        for &block in params.keys() {
            sources.extend(graph.cfg.predecessors(block).iter().copied());
        }
        let mut forward: Map<Value, Value> = Map::default();
        let mut ends: Map<Block, Vec<Option<Value>>> = Map::default();
        let mut current = Current { values: vec![None; count], undo: Vec::new() };
        let mut order: Vec<Block> = spans
            .keys()
            .chain(params.keys())
            .chain(&sources)
            .copied()
            .filter(|block| graph.pre[block.index()] != usize::MAX)
            .collect();
        order.sort_unstable_by_key(|block| graph.pre[block.index()]);
        order.dedup();
        // The blocks visited that the one being visited is under, each with where its changes
        // start in the undo list.
        let mut open: Vec<(Block, usize)> = Vec::new();
        for block in order {
            let at = graph.pre[block.index()];
            while let Some(&(above, mark)) = open.last() {
                if at <= graph.last[above.index()] {
                    break;
                }
                current.undo_to(mark);
                open.pop();
            }
            let mark = current.undo.len();
            for &(k, param) in params.get(&block).into_iter().flatten() {
                current.set(k, param);
            }
            if let Some(&(start, end)) = spans.get(&block) {
                let plan = self.plan;
                for &(inst, found) in &plan.uses[start..end] {
                    self.visit(func, inst, found, &mut current, &mut forward);
                }
            }
            if sources.contains(&block) {
                ends.insert(block, current.values.clone());
            }
            open.push((block, mark));
        }

        // Every edge into a block with parameters passes the values its source ended with, in
        // the order the parameters were added. The sources go in layout order, which is the order
        // the zeros are made in.
        let mut sources: Vec<Block> = sources.into_iter().collect();
        sources.sort_unstable_by_key(|block| graph.layout[block.index()]);
        for block in sources {
            let Some(terminator) = func.terminator(block) else {
                continue;
            };
            for at in func.target_list(terminator).iter() {
                let call = func[at];
                let Some(list) = params.get(&call.block) else {
                    continue;
                };
                let mut args = call.args;
                for &(k, _) in list {
                    let value = match ends[&block][k] {
                        Some(value) => value,
                        None => self.zero(func, k),
                    };
                    args = func.append_arg(args, value);
                    readers.push(value, terminator);
                }
                func.set_block_call(at, BlockCall { args, ..call });
            }
        }

        substitute(func, &forward, readers);
        for &(inst, _) in &self.plan.uses {
            func.remove_inst(inst);
        }
        for &inst in &self.plan.ends {
            func.remove_inst(inst);
        }
        for &inst in self.plan.derived.iter().rev() {
            func.remove_inst(inst);
        }
        func.remove_inst(self.plan.alloca);
    }

    /// Does what one use of the local did, to the values standing for it.
    fn visit(
        &mut self,
        func: &mut Func,
        inst: Inst,
        found: Use,
        current: &mut Current,
        forward: &mut Map<Value, Value>,
    ) {
        if self.plan.vector && self.lane(func, inst, found, current, forward) {
            return;
        }
        match found {
            Use::Load { at, size, ty } => {
                let result = func[inst].first_result.expect("a load produces its value");
                let value = match self.within(at, size)[..] {
                    [k] => {
                        let value = self.value(func, k, &current.values);
                        convert(func, inst, value, self.plan.pieces[k].ty, ty)
                    }
                    ref several => self.compose(func, inst, several, at, ty, &current.values),
                };
                forward.insert(result, value);
            }
            Use::Store { at, size, ty, value } => match self.within(at, size)[..] {
                [k] => current.set(k, convert(func, inst, value, ty, self.plan.pieces[k].ty)),
                ref several => {
                    for &k in several {
                        let piece = self.plan.pieces[k];
                        let narrow = integer(piece.size);
                        let mut part = value;
                        if piece.at > at {
                            let by = constant(func, inst, ty, i128::from((piece.at - at) * 8));
                            part = binary(func, inst, Opcode::LShr, part, by, ty);
                        }
                        if narrow != ty {
                            part = unary(func, inst, Opcode::Trunc, part, narrow);
                        }
                        current.set(k, convert(func, inst, part, narrow, piece.ty));
                    }
                }
            },
            Use::Fill { at, size, byte } => {
                for k in self.within(at, size) {
                    let piece = self.plan.pieces[k];
                    let value = self.pattern(func, inst, piece, byte);
                    current.set(k, value);
                }
            }
            Use::CopyIn { at, size, from, align } => {
                for k in self.within(at, size) {
                    let piece = self.plan.pieces[k];
                    let delta = piece.at - at;
                    let address = self.offset(func, inst, from, delta);
                    current.set(k, load(func, inst, piece.ty, address, aligned(align, delta)));
                }
            }
            Use::CopyOut { at, size, to, align } => {
                for k in self.within(at, size) {
                    let piece = self.plan.pieces[k];
                    let delta = piece.at - at;
                    let value = self.value(func, k, &current.values);
                    let address = self.offset(func, inst, to, delta);
                    store(func, inst, value, address, aligned(align, delta));
                }
            }
        }
    }

    /// One access to a vector piece that is not a copy of the whole of it, or false for a copy,
    /// which the plain way does.
    fn lane(
        &mut self,
        func: &mut Func,
        inst: Inst,
        found: Use,
        current: &mut Current,
        forward: &mut Map<Value, Value>,
    ) -> bool {
        let whole = self.plan.pieces[0].ty;
        // The vector as lanes of the access's width, and which lane the access is.
        let shape = |at: u64, size: u64| {
            let lanes = u32::try_from(16 / size).expect("a lane is four or eight bytes");
            let lane = u8::try_from(at / size).expect("a lane of a sixteen byte vector");
            (Type::vector(integer(size), lanes), lane)
        };
        match found {
            Use::Load { at, size, ty } => {
                let result = func[inst].first_result.expect("a load produces its value");
                let value = self.value(func, 0, &current.values);
                let read = if size == WHOLE {
                    convert(func, inst, value, whole, ty)
                } else {
                    let (lanes, lane) = shape(at, size);
                    let vector = convert(func, inst, value, whole, lanes);
                    let args = func.push_values(&[vector]);
                    let data = InstData {
                        args,
                        extra: Extra::Lane(lane),
                        ..InstData::new(Opcode::ExtractLane)
                    };
                    let one = emit(func, inst, data, integer(size));
                    convert(func, inst, one, integer(size), ty)
                };
                forward.insert(result, read);
            }
            Use::Store { at, size, ty, value } => {
                let written = if size == WHOLE {
                    convert(func, inst, value, ty, whole)
                } else {
                    let (lanes, lane) = shape(at, size);
                    let before = self.value(func, 0, &current.values);
                    let vector = convert(func, inst, before, whole, lanes);
                    let one = convert(func, inst, value, ty, integer(size));
                    let args = func.push_values(&[vector, one]);
                    let data = InstData {
                        args,
                        extra: Extra::Lane(lane),
                        ..InstData::new(Opcode::InsertLane)
                    };
                    let after = emit(func, inst, data, lanes);
                    convert(func, inst, after, lanes, whole)
                };
                current.set(0, written);
            }
            Use::Fill { .. } | Use::CopyIn { .. } | Use::CopyOut { .. } => return false,
        }
        true
    }

    /// The value a piece has here, which is zero where nothing wrote it.
    fn value(&mut self, func: &mut Func, k: usize, current: &[Option<Value>]) -> Value {
        match current[k] {
            Some(value) => value,
            None => self.zero(func, k),
        }
    }

    fn zero(&mut self, func: &mut Func, k: usize) -> Value {
        if let Some(value) = self.zeros[k] {
            return value;
        }
        let value = self.pattern(func, self.top, self.plan.pieces[k], 0);
        self.zeros[k] = Some(value);
        value
    }

    /// The piece with every byte set to `byte`, which is what a `memset` leaves in it.
    fn pattern(&self, func: &mut Func, before: Inst, piece: Piece, byte: u8) -> Value {
        let bits = (0..piece.size).fold(0u128, |bits, _| bits << 8 | u128::from(byte));
        if piece.ty.is_vector() {
            // Every byte the same, so every lane is the low one.
            let lane = piece.ty.lane();
            let at = func.add_imm(Imm::int(i128::from_ne_bytes(bits.to_ne_bytes()), lane));
            let data = InstData { extra: Extra::Imm(at), ..InstData::new(Opcode::Splat) };
            return emit(func, before, data, piece.ty);
        }
        if piece.ty.is_float() {
            let at = func.add_imm(Imm::from_bits(bits));
            let data = InstData { extra: Extra::Imm(at), ..InstData::new(Opcode::FConst) };
            return emit(func, before, data, piece.ty);
        }
        let int = if piece.ty.is_ptr() { integer(piece.size) } else { piece.ty };
        let value = constant(func, before, int, i128::from_ne_bytes(bits.to_ne_bytes()));
        convert(func, before, value, int, piece.ty)
    }

    /// Integer pieces shifted into place and put together, for an integer read over several.
    fn compose(
        &mut self,
        func: &mut Func,
        before: Inst,
        several: &[usize],
        at: u64,
        ty: Type,
        current: &[Option<Value>],
    ) -> Value {
        let mut total = None;
        for &k in several {
            let piece = self.plan.pieces[k];
            let narrow = integer(piece.size);
            let value = self.value(func, k, current);
            let mut part = convert(func, before, value, piece.ty, narrow);
            if narrow != ty {
                part = unary(func, before, Opcode::ZExt, part, ty);
            }
            if piece.at > at {
                let by = constant(func, before, ty, i128::from((piece.at - at) * 8));
                part = binary(func, before, Opcode::Shl, part, by, ty);
            }
            total = Some(match total {
                None => part,
                Some(sum) => binary(func, before, Opcode::Or, sum, part, ty),
            });
        }
        total.expect("an access covers one piece at least")
    }

    /// The address `delta` bytes past `base`.
    fn offset(&self, func: &mut Func, before: Inst, base: Value, delta: u64) -> Value {
        if delta == 0 {
            return base;
        }
        let int = integer(self.target.pointer.unwrap_or(WIDEST_FILLER));
        let by = constant(func, before, int, i128::from(delta));
        let args = func.push_values(&[base, by]);
        emit(func, before, InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR)
    }
}

/// The value each piece has at one point of the renaming, and what to put back on the way out.
struct Current {
    values: Vec<Option<Value>>,
    /// Each piece that was set and what it held before, latest last.
    undo: Vec<(usize, Option<Value>)>,
}

impl Current {
    fn set(&mut self, k: usize, value: Value) {
        self.undo.push((k, self.values[k]));
        self.values[k] = Some(value);
    }

    /// Puts back everything set since the undo list was this long.
    fn undo_to(&mut self, mark: usize) {
        while self.undo.len() > mark {
            let (k, was) = self.undo.pop().expect("longer than the mark");
            self.values[k] = was;
        }
    }
}

/// Points every reader of a value in the map at the value it now stands for, and moves the names
/// along, which is [`crate::uses::substitute`] with the readers already known.
fn substitute(func: &mut Func, forward: &Map<Value, Value>, readers: &mut Readers) {
    // Instructions this local's rewrite made may read a load it is replacing, a store of what an
    // earlier load of it read being the usual one.
    readers.catch_up(func);
    let with = |value: Value| crate::uses::chase(forward, value);
    for &from in forward.keys() {
        let Some(list) = readers.of.get_mut(from.index()) else {
            continue;
        };
        let list = std::mem::take(list);
        let to = with(from);
        for inst in list {
            if func.block_of(inst).is_none() {
                continue;
            }
            let args = func[inst].args;
            func.rewrite(args, with);
            for at in func.target_list(inst).iter() {
                let args = func[at].args;
                func.rewrite(args, with);
            }
            readers.push(to, inst);
        }
    }
    crate::uses::rename(func, forward);
}

/// The alignment an access `delta` bytes past an address aligned to `align` still has.
fn aligned(align: u32, delta: u64) -> u32 {
    if delta == 0 {
        return align;
    }
    u32::try_from(1u64 << delta.trailing_zeros()).map_or(align, |low| low.min(align))
}

/// The same bytes as another type, which [`convertible`] said could be done.
fn convert(func: &mut Func, before: Inst, value: Value, from: Type, to: Type) -> Value {
    if from == to {
        return value;
    }
    let opcode = if from.is_ptr() {
        Opcode::PtrToInt
    } else if to.is_ptr() {
        Opcode::IntToPtr
    } else {
        Opcode::Bitcast
    };
    unary(func, before, opcode, value, to)
}

fn constant(func: &mut Func, before: Inst, ty: Type, value: i128) -> Value {
    let at = func.add_imm(Imm::int(value, ty.lane()));
    let data = InstData { extra: Extra::Imm(at), ..InstData::new(Opcode::IConst) };
    emit(func, before, data, ty)
}

fn unary(func: &mut Func, before: Inst, opcode: Opcode, value: Value, ty: Type) -> Value {
    let args = func.push_values(&[value]);
    emit(func, before, InstData { args, ..InstData::new(opcode) }, ty)
}

fn binary(
    func: &mut Func,
    before: Inst,
    opcode: Opcode,
    lhs: Value,
    rhs: Value,
    ty: Type,
) -> Value {
    let args = func.push_values(&[lhs, rhs]);
    emit(func, before, InstData { args, ..InstData::new(opcode) }, ty)
}

/// What a plain access that is not the local's own carries.
const fn plain(align: u32) -> MemInfo {
    MemInfo {
        size: 0,
        align,
        order: MemOrder::NotAtomic,
        tbaa: None,
        owns: 0,
        restrict: Restrict::NONE,
    }
}

fn load(func: &mut Func, before: Inst, ty: Type, address: Value, align: u32) -> Value {
    let mem = func.add_mem(plain(align));
    let args = func.push_values(&[address]);
    let data = InstData { args, extra: Extra::Mem(mem), ..InstData::new(Opcode::Load) };
    emit(func, before, data, ty)
}

fn store(func: &mut Func, before: Inst, value: Value, address: Value, align: u32) {
    let mem = func.add_mem(plain(align));
    let args = func.push_values(&[value, address]);
    let data = InstData { args, extra: Extra::Mem(mem), ..InstData::new(Opcode::Store) };
    let span = func.span(before);
    let inst = func.create_inst(data, &[], span);
    func.insert_before(inst, before);
}

/// Puts an instruction in front of another one and gives back the value it produces.
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
    use rucc_ir::{Module, parse, verify_func};

    use super::*;
    use crate::outside::Outside;
    use crate::stats::Kind;

    const HEADER: &str = "\
; ModuleID = 'sroa.c'
; format 0
target triple = \"x86_64-unknown-linux-gnu\"
target datalayout = \"e-p:64:64-i64:64-f80:128-S128\"
";

    fn wrap(signature: &str, body: &str) -> String {
        format!(
            "{HEADER}\nfunc @g(ptr), linkage(external);\n\nfunc @f{signature}, linkage(external) {{\n{body}}}\n"
        )
    }

    fn run(text: &str) -> (Module, Stats) {
        with(text, &mut Fuel::unlimited())
    }

    /// Runs the pass over `@f` and insists the result verifies, which is where most of the
    /// strength of these tests is: a parameter without its arguments, a load left reading a
    /// removed address or a conversion of the wrong width are all things the verifier refuses.
    fn with(text: &str, fuel: &mut Fuel) -> (Module, Stats) {
        built(text, fuel, false)
    }

    /// The same on a build with the vector registers, which is what lets a vector be a value.
    fn on_sse2(text: &str) -> (Module, Stats) {
        built(text, &mut Fuel::unlimited(), true)
    }

    fn built(text: &str, fuel: &mut Fuel, vectors: bool) -> (Module, Stats) {
        let mut names = Interner::new();
        let mut module = parse(text, &mut names).expect("the text parses");
        let id = module.funcs().last().expect("one function");
        let outside = Arc::new(Outside::of(&module).with_vectors(vectors));
        let mut an = crate::machine::fixtures::analyses().about(outside);
        let stats = Sroa.run(&mut module[id], &mut an, fuel);
        if let Err(errors) = verify_func(&module, &module[id], &names) {
            panic!("{errors:#?}");
        }
        (module, stats)
    }

    fn body(module: &Module) -> &Func {
        &module[module.funcs().last().expect("one function")]
    }

    fn count_of(func: &Func, opcode: Opcode) -> usize {
        func.blocks()
            .flat_map(|block| func.insts(block))
            .filter(|&inst| func[inst].opcode == opcode)
            .count()
    }

    fn params(func: &Func, block: usize) -> usize {
        let block = func.blocks().nth(block).expect("that many blocks");
        func[block].params.len()
    }

    #[test]
    fn a_struct_set_to_zero_and_counted_round_a_loop_becomes_values() {
        // The loop from tamnd/rucc#2204, `for (struct { const List *l; int i; } s = {l, 0}; ...)`,
        // with the counter read at the top of the loop and written at the bottom.
        let text = wrap(
            "(ptr, i32)",
            "block0(%0: ptr, %1: i32):
    %2 = alloca, size 16, align 16
    %3 = iconst.i8 0
    memset %2, %3, size 16, align 8
    store %0 -> %2, align 8
    %4 = iconst.i64 8
    %5 = ptr_add %2, %4
    %6 = iconst.i32 0
    store %6 -> %5, align 4
    jump block1

block1:
    %7 = load.i32 %5, align 4
    %8 = icmp slt %7, %1
    br_if %8, block2, block3

block2:
    %9 = load %2, align 8
    call @g(%9) : (ptr)
    %10 = iconst.i32 1
    %11 = add %7, %10
    store %11 -> %5, align 4
    jump block1

block3:
    return
",
        );
        let (module, stats) = run(&text);
        assert_eq!(stats.count(Kind::Optimized, SCALARIZED), 1);
        let func = body(&module);
        for opcode in [Opcode::Alloca, Opcode::Load, Opcode::Store, Opcode::Memset] {
            assert_eq!(count_of(func, opcode), 0, "{} is left", opcode.name());
        }
        // The counter needs a parameter at the top of the loop and the pointer does not, since
        // nothing writes it inside the loop.
        assert_eq!(params(func, 1), 1);
        assert_eq!(params(func, 3), 0);
    }

    #[test]
    fn a_pointer_read_back_as_an_integer_goes_through_ptrtoint() {
        // `make2` from tamnd/rucc#2204: a union of a pointer and an integer, written as one and
        // passed on as the other.
        let text = wrap(
            "(ptr) -> i64",
            "block0(%0: ptr):
    %1 = alloca, size 8, align 8
    store %0 -> %1, align 8
    %2 = load.i64 %1, align 8
    return %2
",
        );
        let (module, stats) = run(&text);
        assert!(stats.changed());
        let func = body(&module);
        assert_eq!(count_of(func, Opcode::Alloca), 0);
        assert_eq!(count_of(func, Opcode::PtrToInt), 1);
    }

    #[test]
    fn a_float_read_as_an_integer_goes_through_bitcast() {
        let text = wrap(
            "(f32) -> i32",
            "block0(%0: f32):
    %1 = alloca, size 4, align 4
    store %0 -> %1, align 4
    %2 = load.i32 %1, align 4
    return %2
",
        );
        let (module, stats) = run(&text);
        assert!(stats.changed());
        let func = body(&module);
        assert_eq!(count_of(func, Opcode::Alloca), 0);
        assert_eq!(count_of(func, Opcode::Bitcast), 1);
    }

    #[test]
    fn a_struct_copied_and_swapped_becomes_shifts() {
        // `swap_sum` from tamnd/rucc#2204. The argument arrives as one integer, is copied into a
        // second local, and read back as two halves. The first local goes and leaves a store of
        // the whole integer into the second, which then goes too, and its halves are the low and
        // the high half of the argument.
        let text = wrap(
            "(i64) -> i32",
            "block0(%0: i64):
    %1 = alloca, size 8, align 4
    %2 = alloca, size 8, align 4
    store %0 -> %1, align 4
    memcpy %2, %1, size 8, align 4
    %3 = load.i32 %2, align 4
    %4 = iconst.i64 4
    %5 = ptr_add %2, %4
    %6 = load.i32 %5, align 4
    store %6 -> %2, align 4
    store %3 -> %5, align 4
    %7 = iconst.i32 3
    %8 = mul %6, %7
    %9 = add %8, %3
    return %9
",
        );
        let (module, stats) = run(&text);
        assert_eq!(stats.count(Kind::Optimized, SCALARIZED), 2);
        let func = body(&module);
        for opcode in [Opcode::Alloca, Opcode::Load, Opcode::Store, Opcode::Memcpy] {
            assert_eq!(count_of(func, opcode), 0, "{} is left", opcode.name());
        }
        assert_eq!(count_of(func, Opcode::LShr), 1);
        assert_eq!(count_of(func, Opcode::Trunc), 2);
    }

    #[test]
    fn the_end_of_a_local_s_lifetime_goes_with_the_local() {
        // The shape of the kernel's `scoped_seqlock_read`, whose state is a local declared in a
        // `for` that only becomes a value once the end of its lifetime does not hold it in memory.
        let text = wrap(
            "(i32) -> i32",
            "block0(%0: i32):
    %1 = alloca, size 4, align 4
    store %0 -> %1, align 4
    %2 = load.i32 %1, align 4
    lifetime_end %1
    return %2
",
        );
        let (module, stats) = run(&text);
        assert_eq!(stats.count(Kind::Optimized, SCALARIZED), 1);
        let func = body(&module);
        for opcode in [Opcode::Alloca, Opcode::Load, Opcode::Store, Opcode::LifetimeEnd] {
            assert_eq!(count_of(func, opcode), 0, "{} is left", opcode.name());
        }
    }

    #[test]
    fn a_local_is_carried_along_the_edges_of_an_asm_goto() {
        // The kernel's static branch is an `asm goto`, and it is in nearly every function that
        // has a tracepoint. The local is written on one side of it and read where the two meet.
        let text = wrap(
            "(i32) -> i32",
            "block0(%0: i32):
    %1 = alloca, size 4, align 4
    store %0 -> %1, align 4
    inline_asm.volatile \"jmp %l0\", \"\", \"\"(), labels [block1, block2]

block1:
    %2 = iconst.i32 1
    store %2 -> %1, align 4
    jump block2

block2:
    %3 = load.i32 %1, align 4
    return %3
",
        );
        let (module, stats) = run(&text);
        assert_eq!(stats.count(Kind::Optimized, SCALARIZED), 1);
        let func = body(&module);
        for opcode in [Opcode::Alloca, Opcode::Load, Opcode::Store] {
            assert_eq!(count_of(func, opcode), 0, "{} is left", opcode.name());
        }
        assert_eq!(params(func, 2), 1);
    }

    #[test]
    fn a_copy_in_becomes_loads_from_the_source() {
        let text = wrap(
            "(ptr) -> i32",
            "block0(%0: ptr):
    %1 = alloca, size 8, align 4
    memcpy %1, %0, size 8, align 4
    %2 = iconst.i64 4
    %3 = ptr_add %1, %2
    %4 = load.i32 %3, align 4
    return %4
",
        );
        let (module, stats) = run(&text);
        assert!(stats.changed());
        let func = body(&module);
        assert_eq!(count_of(func, Opcode::Alloca), 0);
        assert_eq!(count_of(func, Opcode::Memcpy), 0);
        // One load, of the one piece anything reads, four bytes into the source.
        assert_eq!(count_of(func, Opcode::Load), 1);
        assert_eq!(count_of(func, Opcode::PtrAdd), 1);
    }

    #[test]
    fn a_copy_out_writes_every_byte_the_local_was_given() {
        // Zeroed, one field set, then copied out whole. The bytes around the field are ones only
        // the `memset` wrote, and they still have to reach the destination.
        let text = wrap(
            "(ptr, i32)",
            "block0(%0: ptr, %1: i32):
    %2 = alloca, size 16, align 8
    %3 = iconst.i8 0
    memset %2, %3, size 16, align 8
    %4 = iconst.i64 4
    %5 = ptr_add %2, %4
    store %1 -> %5, align 4
    memcpy %0, %2, size 16, align 8
    return
",
        );
        let (module, stats) = run(&text);
        assert!(stats.changed());
        let func = body(&module);
        assert_eq!(count_of(func, Opcode::Alloca), 0);
        assert_eq!(count_of(func, Opcode::Memcpy), 0);
        // Bytes 0 to 4, the field at 4 to 8, and 8 to 16 in one piece.
        assert_eq!(count_of(func, Opcode::Store), 3);
    }

    #[test]
    fn a_float_over_half_of_an_integer_keeps_the_local() {
        let text = wrap(
            "(f32) -> i32",
            "block0(%0: f32):
    %1 = alloca, size 8, align 4
    store %0 -> %1, align 4
    %2 = iconst.i64 2
    %3 = ptr_add %1, %2
    %4 = load.i32 %3, align 4
    return %4
",
        );
        let (module, stats) = run(&text);
        assert!(!stats.changed());
        assert_eq!(stats.count(Kind::Missed, OVERLAP), 1);
        assert_eq!(count_of(body(&module), Opcode::Alloca), 1);
    }

    #[test]
    fn an_integer_over_two_halves_is_put_together_from_them() {
        let text = wrap(
            "(i32, i32) -> i64",
            "block0(%0: i32, %1: i32):
    %2 = alloca, size 8, align 8
    store %0 -> %2, align 4
    %3 = iconst.i64 4
    %4 = ptr_add %2, %3
    store %1 -> %4, align 4
    %5 = load.i64 %2, align 8
    return %5
",
        );
        let (module, stats) = run(&text);
        assert!(stats.changed());
        let func = body(&module);
        assert_eq!(count_of(func, Opcode::Alloca), 0);
        assert_eq!(count_of(func, Opcode::ZExt), 2);
        assert_eq!(count_of(func, Opcode::Shl), 1);
        assert_eq!(count_of(func, Opcode::Or), 1);
    }

    #[test]
    fn an_address_passed_to_a_call_keeps_the_local() {
        let text = wrap(
            "() -> i32",
            "block0:
    %0 = alloca, size 4, align 4
    %1 = iconst.i32 7
    store %1 -> %0, align 4
    call @g(%0) : (ptr)
    %2 = load.i32 %0, align 4
    return %2
",
        );
        let (module, stats) = run(&text);
        assert!(!stats.changed());
        assert_eq!(count_of(body(&module), Opcode::Alloca), 1);
    }

    #[test]
    fn a_volatile_load_keeps_the_local() {
        let text = wrap(
            "() -> i32",
            "block0:
    %0 = alloca, size 4, align 4
    %1 = iconst.i32 7
    store %1 -> %0, align 4
    %2 = load.i32.volatile %0, align 4
    return %2
",
        );
        let (module, stats) = run(&text);
        assert!(!stats.changed());
        assert_eq!(count_of(body(&module), Opcode::Load), 1);
    }

    #[test]
    fn a_read_nothing_wrote_reads_zero() {
        let text = wrap(
            "(i1) -> i32",
            "block0(%0: i1):
    %1 = alloca, size 4, align 4
    br_if %0, block1, block2

block1:
    %2 = iconst.i32 7
    store %2 -> %1, align 4
    jump block2

block2:
    %3 = load.i32 %1, align 4
    return %3
",
        );
        let (module, stats) = run(&text);
        assert!(stats.changed());
        let func = body(&module);
        assert_eq!(count_of(func, Opcode::Load), 0);
        assert_eq!(params(func, 2), 1);
    }

    /// `_mm_add_epi32` and `_mm_cvtsi128_si32` after inlining: a copy of an argument added to
    /// itself as a whole vector, one lane replaced, and one lane read back.
    const LANES: &str = "block0(%0: ptr, %1: i32):
    %2 = alloca, size 16, align 16
    memcpy %2, %0, size 16, align 16
    %3 = load.i32x4 %2, align 4
    %4 = add %3, %3
    store %4 -> %2, align 4
    %5 = iconst.i64 8
    %6 = ptr_add %2, %5
    store %1 -> %6, align 4
    %7 = iconst.i64 4
    %8 = ptr_add %2, %7
    %9 = load.i32 %8, align 4
    %10 = load.i64x2 %2, align 4
    store %10 -> %0, align 16
    return %9
";

    #[test]
    fn a_vector_read_and_written_by_lane_becomes_one_value() {
        let (module, stats) = on_sse2(&wrap("(ptr, i32) -> i32", LANES));
        assert_eq!(stats.count(Kind::Optimized, SCALARIZED), 1);
        let func = body(&module);
        assert_eq!(count_of(func, Opcode::Alloca), 0);
        assert_eq!(count_of(func, Opcode::InsertLane), 1);
        assert_eq!(count_of(func, Opcode::ExtractLane), 1);
        // The copy in is the one load left and the store of the other shape the one store.
        assert_eq!(count_of(func, Opcode::Load), 1);
        assert_eq!(count_of(func, Opcode::Store), 1);
    }

    #[test]
    fn a_vector_stays_in_memory_without_the_vector_registers() {
        let (module, stats) = run(&wrap("(ptr, i32) -> i32", LANES));
        assert_eq!(stats.count(Kind::Missed, WIDTH), 1);
        assert_eq!(count_of(body(&module), Opcode::Alloca), 1);
    }

    #[test]
    fn a_byte_of_a_vector_keeps_it_in_memory() {
        let text = wrap(
            "(ptr) -> i8",
            "block0(%0: ptr):
    %1 = alloca, size 16, align 16
    memcpy %1, %0, size 16, align 16
    %2 = load.i32x4 %1, align 4
    %3 = add %2, %2
    store %3 -> %1, align 4
    %4 = load.i8 %1, align 1
    return %4
",
        );
        let (module, stats) = on_sse2(&text);
        assert_eq!(stats.count(Kind::Missed, OVERLAP), 1);
        assert_eq!(count_of(body(&module), Opcode::Alloca), 1);
    }

    #[test]
    fn a_vector_built_lane_by_lane_round_a_loop_starts_from_a_zero_splat() {
        // `__answer[__i] = ...` in a loop the unroller has not been to, with the vector carried
        // round it.
        let text = wrap(
            "(i32) -> i32",
            "block0(%0: i32):
    %1 = alloca, size 16, align 16
    %2 = iconst.i64 4
    %3 = ptr_add %1, %2
    jump block1

block1:
    store %0 -> %3, align 4
    %4 = load.i32x4 %1, align 4
    %5 = add %4, %4
    store %5 -> %1, align 4
    %6 = load.i32 %1, align 4
    %7 = icmp slt %6, %0
    br_if %7, block1, block2

block2:
    return %6
",
        );
        let (module, stats) = on_sse2(&text);
        assert_eq!(stats.count(Kind::Optimized, SCALARIZED), 1);
        let func = body(&module);
        assert_eq!(count_of(func, Opcode::Splat), 1);
        assert_eq!(params(func, 1), 1);
    }

    /// `_mm_slli_epi32` after inlining: the vector comes in and goes out as the sixteen byte float
    /// a function passes it as, and is shifted a lane at a time in between.
    #[test]
    fn a_vector_copied_as_the_quad_float_becomes_one_value() {
        let text = wrap(
            "(f128) -> f128",
            "block0(%0: f128):
    %1 = alloca, size 16, align 16
    store %0 -> %1, align 16
    %2 = load.i32x4 %1, align 16
    %3 = add %2, %2
    store %3 -> %1, align 16
    %4 = iconst.i64 4
    %5 = ptr_add %1, %4
    %6 = load.i32 %5, align 4
    store %6 -> %1, align 4
    %7 = load.f128 %1, align 16
    return %7
",
        );
        let (module, stats) = on_sse2(&text);
        assert_eq!(stats.count(Kind::Optimized, SCALARIZED), 1);
        let func = body(&module);
        assert_eq!(count_of(func, Opcode::Alloca), 0);
        assert_eq!(count_of(func, Opcode::Load), 0);
        assert_eq!(count_of(func, Opcode::Store), 0);
        assert_eq!(count_of(func, Opcode::InsertLane), 1);
        assert_eq!(count_of(func, Opcode::ExtractLane), 1);
    }

    /// A `__float128` local on its own is one piece of that type.
    #[test]
    fn a_quad_float_on_its_own_becomes_one_value() {
        let text = wrap(
            "(f128) -> f128",
            "block0(%0: f128):
    %1 = alloca, size 16, align 16
    store %0 -> %1, align 16
    %2 = load.f128 %1, align 16
    return %2
",
        );
        let (module, stats) = on_sse2(&text);
        assert_eq!(stats.count(Kind::Optimized, SCALARIZED), 1);
        assert_eq!(count_of(body(&module), Opcode::Alloca), 0);
    }

    /// Part of a quad float is still a `__float128` someone is looking inside, which stays.
    #[test]
    fn half_of_a_quad_float_stays_in_memory() {
        let text = wrap(
            "(f128) -> f128",
            "block0(%0: f128):
    %1 = alloca, size 16, align 16
    store %0 -> %1, align 16
    %2 = load.i8 %1, align 1
    store %2 -> %1, align 1
    %3 = load.f128 %1, align 16
    return %3
",
        );
        let (module, _) = on_sse2(&text);
        assert_eq!(count_of(body(&module), Opcode::Alloca), 1);
    }

    /// `_mm_shuffle_epi32` in the header: the vector stored as the float, its lanes read one at a
    /// time, written back in another order and the whole read as the float again.
    #[test]
    fn lanes_of_a_quad_float_become_one_vector() {
        let text = wrap(
            "(f128) -> f128",
            "block0(%0: f128):
    %1 = alloca, size 16, align 16
    %2 = alloca, size 16, align 16
    store %0 -> %1, align 16
    %3 = load.i32 %1, align 16
    %4 = iconst.i64 4
    %5 = ptr_add %1, %4
    %6 = load.i32 %5, align 4
    %7 = iconst.i64 8
    %8 = ptr_add %1, %7
    %9 = load.i32 %8, align 8
    %10 = iconst.i64 12
    %11 = ptr_add %1, %10
    %12 = load.i32 %11, align 4
    store %12 -> %2, align 16
    %13 = ptr_add %2, %4
    store %3 -> %13, align 4
    %14 = ptr_add %2, %7
    store %6 -> %14, align 8
    %15 = ptr_add %2, %10
    store %9 -> %15, align 4
    %16 = load.f128 %2, align 16
    return %16
",
        );
        let (module, stats) = on_sse2(&text);
        assert_eq!(stats.count(Kind::Optimized, SCALARIZED), 2);
        let func = body(&module);
        assert_eq!(count_of(func, Opcode::Alloca), 0);
        assert_eq!(count_of(func, Opcode::ExtractLane), 4);
        assert_eq!(count_of(func, Opcode::InsertLane), 4);
    }

    /// The header's copy of a vector through a local, which only ever sees it as the float.
    #[test]
    fn a_quad_float_copied_through_stays_out_of_memory() {
        let text = wrap(
            "(ptr, ptr)",
            "block0(%0: ptr, %1: ptr):
    %2 = alloca, size 16, align 16
    memcpy %2, %0, size 16, align 16
    %3 = load.f128 %2, align 16
    store %3 -> %2, align 16
    memcpy %1, %2, size 16, align 16
    return
",
        );
        let (module, stats) = on_sse2(&text);
        assert_eq!(stats.count(Kind::Optimized, SCALARIZED), 1);
        assert_eq!(count_of(body(&module), Opcode::Alloca), 0);
    }

    #[test]
    fn no_fuel_keeps_the_local() {
        let text = wrap(
            "(i32) -> i32",
            "block0(%0: i32):
    %1 = alloca, size 4, align 4
    store %0 -> %1, align 4
    %2 = load.i32 %1, align 4
    return %2
",
        );
        let (module, stats) = with(&text, &mut Fuel::of(0));
        assert!(!stats.changed());
        assert_eq!(stats.count(Kind::Missed, NO_FUEL), 1);
        assert_eq!(count_of(body(&module), Opcode::Alloca), 1);
    }

    #[test]
    fn a_local_filled_by_one_copy_from_outside_becomes_a_load() {
        // `read_header` from tamnd/rucc#2298: `memcpy(&h, rec, sizeof(h))` and then `h` read
        // whole. The copy is the only write, so the local goes and the read is a load from `rec`.
        let text = wrap(
            "(ptr, ptr)",
            "block0(%0: ptr, %1: ptr):
    %2 = alloca, size 4, align 4
    %3 = iconst.i64 4
    memcpy %2, %0, size 4, align 1
    %4 = load.i32 %2, align 4, tbaa !1
    call_indirect %1(%4) : (i32)
    lifetime_end %2
    return
",
        ) + "!0 = tbaa \"char\", offset 0\n!1 = tbaa \"int\", parent !0, offset 0\n";
        let (module, stats) = run(&text);
        assert_eq!(stats.count(Kind::Optimized, SCALARIZED), 1, "{stats:?}");
        let func = body(&module);
        for opcode in [Opcode::Alloca, Opcode::Memcpy] {
            assert_eq!(count_of(func, opcode), 0, "{} is left", opcode.name());
        }
        assert_eq!(count_of(func, Opcode::Load), 1);
    }
}
