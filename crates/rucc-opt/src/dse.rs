//! Dead store elimination: a store nothing reads before it is overwritten goes away.
//!
//! Design: `spec/optimizer/17-dce-and-dse.md` section 17.3, and tamnd/rucc#2353.
//!
//! ```c
//! void fill(struct S *s) { memset(s, 0, sizeof *s); s->a = 1; s->b = 2; s->c = 3; s->d = 4; }
//! ```
//!
//! The fill writes sixteen bytes and the four stores after it write the same sixteen, so nothing
//! can ever see what the fill wrote and it can go. The redundant load passes already take a load
//! that reads what a store just wrote. This is the other direction: section 17.3 quotes gcc's
//! remark that the two are the same transformation on the two views of the control flow graph,
//! one walking forwards from a load's question and this one walking forwards from a store's answer.
//!
//! # How it decides
//!
//! From each store the pass walks forwards along every path, holding the bytes of it that are
//! still live. A later store to the same object, at an offset both of them know, takes the bytes it
//! covers out of them, from the front, the back or the middle, and a path ends when nothing is
//! left. The middle matters because the stores that overwrite a fill need not come in order:
//! `cp_new_stat` in fs/stat.c zeroes `__pad0` in the middle of its `struct stat` before it writes
//! the fields on either side, and under `-ftrivial-auto-var-init=zero` the fill in front of all of
//! them is only dead once the hole that store left is remembered. A load, a
//! call, or anything else the alias oracle cannot rule out records the bytes it may read, and a
//! return records all of them unless the store is to a local, whose storage is gone once the
//! function has returned. A store where no path records a byte is dead. A `memset` or a `memcpy`
//! where every byte recorded is at one end is narrowed to the bytes that are, which is the trimming
//! of section 17.3 and what `memset(s, 0, sizeof *s)` followed by stores to the first half of `s`
//! wants.
//!
//! Two things stop the walk and keep the store. The walk is limited to
//! [`DSE_WALK_LIMIT`] instructions per store, gcc's limit on the alias queries one store may ask.
//! And a path that comes into a block dominating the store is a path that goes round a loop, and on
//! that path an address the store computed may be a different address the next time. `*p = 0` with
//! `p` moving on each trip writes a new word each time, and without this rule the walk would meet
//! the same store again and take it for one that overwrites the first.
//!
//! What counts as covering is only a store whose address has the same origin as this one and a
//! known offset from it, which is the same address. The oracle's "may" is never enough to kill,
//! only to keep, so a question the oracle cannot answer costs an optimization and not a program.
//!
//! # Stores of zero into zeros
//!
//! The other half of gcc's definition is the redundant store, one that writes the value memory
//! already holds. The case that matters is zero. Under `-ftrivial-auto-var-init=zero` every local
//! is filled with zeros first, and an initializer or a run of `vms->x = 0` after it writes the
//! same zeros again: `do_vmi_align_munmap` in mm/vma.c wrote zero twice into most of the fields
//! of its two structs. After the walks, one pass over the blocks in reverse postorder follows which
//! bytes of which objects hold zero, and a store of zero, or a fill with zero, into bytes that all
//! do on every path into it goes. A block entered from a block not yet seen, which is a loop
//! coming round, starts knowing nothing. The type rule is gcc's: bytes a fill zeroed take a later
//! store of any type, and bytes a store zeroed only take a later one with the same type node. A
//! fill the walks found dead zeroes nothing, and one they narrowed zeroes only what is left of it,
//! so the zeros followed are those of the function after the walks' edits. Without that the fill
//! in `cp_new_stat` would go because the stores of zero after it cover it, and then those stores
//! would go because the fill had zeroed their bytes.

use rucc_base::hash::{Map, Set};
use rucc_cost::heuristics::DSE_WALK_LIMIT;
use rucc_ir::{
    Block, Def, Extra, Flags, Func, Imm, Inst, InstData, MemInfo, MemOrder, Meta, Opcode, Restrict,
    Type, Value,
};

use crate::alias::{Access, Alias, Origin};
use crate::cfg::Cfg;
use crate::dom::Dominators;
use crate::{Analyses, Analysis, Fuel, Pass, Phase, Preserved, Stats};

/// What this pass is called, which the pipeline matches on to decide whether to build the module
/// facts the oracle asks for.
pub const NAME: &str = "dse";

/// Recorded for a store removed because nothing can read what it wrote.
const REMOVED: &str =
    "store removed, nothing reads it before it is overwritten or goes out of scope";

/// Recorded for a store of zero into bytes that already hold zero.
const SAME: &str = "store removed, the bytes it writes already hold zero";

/// The most spans of zeros followed at once, which keeps a block's meeting of its predecessors
/// small. A fill and the stores that cut it up after it are a handful.
const ZEROS: usize = 32;

/// Recorded for a fill or a copy narrowed to the bytes something may read.
const TRIMMED: &str = "fill or copy narrowed to the bytes something may still read";

/// Recorded for a store kept because the walk from it reached its limit.
const TOO_FAR: &str = "store kept, the walk for what reads it reached its limit";

/// The most alignment a narrowed fill or copy keeps at its new start, in bytes, which is the widest
/// move the block lowering writes one out with.
const WORD: u32 = 8;

/// Recorded for a store that would have gone or been narrowed if there had been fuel for it.
const NO_FUEL: &str = "dead store kept, the pass ran out of fuel";

/// The pass.
#[derive(Debug)]
pub struct Dse;

impl Pass for Dse {
    fn name(&self) -> &'static str {
        NAME
    }

    fn describe(&self) -> &'static str {
        "a store overwritten before anything reads it goes, and a fill half overwritten is narrowed"
    }

    fn phase(&self) -> Phase {
        Phase::Scalar
    }

    fn preserves(&self) -> Preserved {
        // No block is added or removed and no edge moves. What goes is stores, which are never
        // terminators, and what comes in front of a narrowed fill is a constant and an address.
        Preserved::ALL.without(Analysis::Liveness)
    }

    fn run(&self, func: &mut Func, an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        let mut fates: Vec<(Inst, Fate)> = Vec::new();
        // The oracle borrows the function, so every question is asked first and every edit made
        // after. A store removed here cannot change what another store's walk would have found,
        // because it is removed only when the bytes it wrote are overwritten before anything reads
        // them, and those are bytes no other walk records either.
        {
            let mut walk = Walk {
                func,
                cfg: an.cfg(func),
                dom: an.dominators(func),
                alias: Alias::new(func, an.outside()).knowing(an.modref()),
            };
            for block in func.blocks() {
                for (at, inst) in func.insts(block).enumerate() {
                    let Some(store) = walk.candidate(inst) else { continue };
                    match walk.fate(&store, block, at + 1) {
                        Fate::Kept => {}
                        Fate::TooFar => stats.missed(TOO_FAR),
                        fate => fates.push((inst, fate)),
                    }
                }
            }
            // The zeros are followed through the function as it is once the walks' edits are
            // made, which is what makes taking a store out on top of those edits sound.
            let decided: Map<Inst, Fate> = fates.iter().copied().collect();
            let same = walk.zeroed(&decided);
            let gone: Set<Inst> = same.iter().copied().collect();
            fates.retain(|(inst, _)| !gone.contains(inst));
            fates.extend(same.into_iter().map(|inst| (inst, Fate::Same)));
        }
        // The distance a narrowed fill moves its address by is an integer as wide as an address,
        // since a wider one is a value a 32-bit target has no instruction to add. Sixty four when
        // there is no module to ask.
        let bits = an.outside().pointer_bytes().map_or(64, |bytes| bytes * 8);
        let bits = u32::try_from(bits).expect("an address narrower than four billion bits");
        for (inst, fate) in fates {
            if !fuel.take() {
                // Out of fuel stops the edits and not the looking, so the count of what could have
                // gone is the same at every fuel setting.
                stats.missed(NO_FUEL);
                continue;
            }
            match fate {
                Fate::Dead => {
                    func.remove_inst(inst);
                    stats.optimized(REMOVED);
                }
                Fate::Same => {
                    func.remove_inst(inst);
                    stats.optimized(SAME);
                }
                Fate::Trim { skip, size } => {
                    trim(func, inst, skip, size, bits);
                    stats.optimized(TRIMMED);
                }
                Fate::Kept | Fate::TooFar => {}
            }
        }
        stats
    }
}

/// What the walk from one store decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fate {
    /// Nothing reads a byte of it.
    Dead,
    /// It writes zero into bytes that already hold zero.
    Same,
    /// Only these bytes may be read, this many past its start, and they are all at one end.
    Trim { skip: i64, size: u64 },
    /// Something may read it, or a path from it goes round a loop.
    Kept,
    /// The walk reached [`DSE_WALK_LIMIT`] first.
    TooFar,
}

/// One store the pass is asking about.
struct Store {
    /// The instruction.
    inst: Inst,
    /// The bytes it writes, which a question to the oracle is narrowed from.
    access: Access,
    /// Where those bytes start and end within the origin.
    lo: i128,
    hi: i128,
    /// Whether it writes a local whose address never left the function, which nothing but this
    /// function can read.
    private: bool,
    /// Whether it is a fill or a copy, which is what can be narrowed.
    bulk: bool,
}

/// The walk, and what it reads the function with.
struct Walk<'a> {
    func: &'a Func,
    cfg: &'a Cfg,
    dom: &'a Dominators,
    alias: Alias<'a>,
}

impl Walk<'_> {
    /// The store this instruction is, if it is one the pass can remove.
    ///
    /// A plain store, fill or copy of a size the compiler knows, to an address with a known offset
    /// from wherever it came from. A volatile or atomic one is the program asking for the write,
    /// and one carrying a memory token is one some pass is still reasoning about.
    fn candidate(&self, inst: Inst) -> Option<Store> {
        let data = &self.func[inst];
        let bulk = matches!(data.opcode, Opcode::Memset | Opcode::Memcpy | Opcode::Memmove);
        if !(bulk || data.opcode == Opcode::Store) || data.flags.contains(Flags::VOLATILE) {
            return None;
        }
        if data.results().next().is_some() || self.func.mem_in(inst).is_some() {
            return None;
        }
        if !plain(self.func, inst) {
            return None;
        }
        let access = self.alias.writes(inst)?;
        let (lo, hi) = access.range().filter(|(lo, hi)| lo < hi)?;
        let private = match access.origin {
            Origin::Local(local) => !self.alias.escapes().escaped(local),
            _ => false,
        };
        Some(Store { inst, access, lo, hi, private, bulk })
    }

    /// Whether this is the end of the lifetime of the local the store writes.
    fn ends(&self, store: &Store, inst: Inst) -> bool {
        let Origin::Local(_) = store.access.origin else { return false };
        self.alias.writes(inst).is_some_and(|ended| ended.origin == store.access.origin)
    }

    /// Walks every path from just after the store, which is in this block at this index.
    fn fate(&mut self, store: &Store, home: Block, next: usize) -> Fate {
        let mut needed: Option<(i128, i128)> = None;
        let mut steps = 0u32;
        // The live bytes each block has been entered with. A block entered again with bytes all
        // among those it was already walked with can find nothing the first walk did not, since
        // fewer live bytes can only be read less.
        let mut entered: Map<Block, Vec<Live>> = Map::default();
        let mut stack = vec![(home, next, Live::new(store.lo, store.hi))];
        while let Some((block, from, mut live)) = stack.pop() {
            let mut ended = false;
            for inst in self.func.insts(block).skip(from) {
                steps += 1;
                if steps > rucc_cost::param!(DSE_WALK_LIMIT) {
                    return Fate::TooFar;
                }
                let (lo, hi) = live.hull();
                if let Some(read) = self.read(store, inst, lo, hi) {
                    if let Some(read) = live.within(read) {
                        needed = Some(hull(needed, read));
                    }
                }
                match self.func[inst].opcode {
                    Opcode::Return | Opcode::TailCall => {
                        // A local is gone when the function is, and anything else is still there
                        // for the caller to read.
                        if !matches!(store.access.origin, Origin::Local(_)) {
                            needed = Some(hull(needed, live.hull()));
                        }
                        ended = true;
                        break;
                    }
                    // A program never gets here, so nothing is ever read here.
                    Opcode::Unreachable | Opcode::UnreachableHint => {
                        ended = true;
                        break;
                    }
                    // The local is gone from here on as much as it is after a return, and whoever
                    // was handed its address may no longer read it. The front end ends every local
                    // in front of the return, so without this a fill of a buffer before returning
                    // was kept whenever the buffer had been passed to a call.
                    Opcode::LifetimeEnd if self.ends(store, inst) => {
                        ended = true;
                        break;
                    }
                    _ => {}
                }
                if let Some((start, end)) = self.covers(store, inst) {
                    live.remove(start, end);
                    if live.is_empty() {
                        ended = true;
                        break;
                    }
                }
            }
            if ended {
                continue;
            }
            let successors = self.cfg.successors(block);
            if successors.is_empty() {
                needed = Some(hull(needed, live.hull()));
            }
            for &successor in successors {
                if self.dom.dominates(successor, home) {
                    return Fate::Kept;
                }
                let seen = entered.entry(successor).or_default();
                if seen.iter().any(|before| live.within_all(before)) {
                    continue;
                }
                seen.push(live.clone());
                stack.push((successor, 0, live.clone()));
            }
        }
        match needed {
            None => Fate::Dead,
            Some((start, end)) if store.bulk && (store.lo < start || end < store.hi) => {
                // The new start keeps the alignment the old one had, up to a word, so the bytes
                // the narrowing saves are whole words. A fill of a 64 byte block whose first byte
                // a store overwrites would otherwise start at an odd address and be written as
                // eight byte stores none of which is aligned, and a load that later reads across
                // two of them waits for both to retire rather than taking what they wrote
                // (tamnd/rucc#3586).
                let keep = i128::from(self.align(store.inst).clamp(1, WORD));
                let start = store.lo + (start - store.lo) / keep * keep;
                if start == store.lo && end == store.hi {
                    return Fate::Kept;
                }
                match (i64::try_from(start - store.lo), u64::try_from(end - start)) {
                    (Ok(skip), Ok(size)) => Fate::Trim { skip, size },
                    _ => Fate::Kept,
                }
            }
            Some(_) => Fate::Kept,
        }
    }

    /// The alignment a fill or a copy says its address has, which is one if it says none.
    fn align(&self, inst: Inst) -> u32 {
        match self.func[inst].extra {
            Extra::Mem(mem) => self.func[mem].align,
            _ => 1,
        }
    }

    /// The bytes of the store, of those still live, that this instruction may read.
    fn read(&mut self, store: &Store, inst: Inst, lo: i128, hi: i128) -> Option<(i128, i128)> {
        let data = &self.func[inst];
        let opcode = data.opcode;
        if !opcode.touches_memory() || opcode.touches_only_planes() || inst == store.inst {
            return None;
        }
        let live = Some((lo, hi));
        // An atomic access or a fence may be the one that hands what was written before it to
        // another thread, and a local nobody else has the address of is the only thing that
        // cannot be read that way.
        let ordered = matches!(opcode, Opcode::Fence | Opcode::AtomicRmw | Opcode::Cmpxchg)
            || !plain_order(self.func, inst);
        if ordered && !store.private {
            return live;
        }
        let narrowed = Access {
            offset: i64::try_from(lo).ok(),
            size: u64::try_from(hi - lo).ok(),
            ..store.access
        };
        match opcode {
            // A write reads nothing, and neither does the end of a local's lifetime, which is a
            // write of the whole of it that nothing may read back.
            Opcode::Store | Opcode::Memset | Opcode::AtomicStore | Opcode::LifetimeEnd => None,
            Opcode::Call | Opcode::CallIndirect | Opcode::TailCall => {
                if self.alias.read_by(&narrowed, inst).is_no() { None } else { live }
            }
            _ => {
                let Some(access) = self.alias.reads(inst) else { return live };
                // The same object at offsets both know is a question the offsets answer, and the
                // answer is which of the live bytes, not only whether.
                let same = access.origin == store.access.origin;
                if let Some((start, end)) = access.range().filter(|_| same) {
                    let (start, end) = (start.max(lo), end.min(hi));
                    return (start < end).then_some((start, end));
                }
                if self.alias.query(&narrowed, &access).is_no() { None } else { live }
            }
        }
    }

    /// The bytes this instruction certainly overwrites, when they are in the store's object at an
    /// offset the two share.
    fn covers(&self, store: &Store, inst: Inst) -> Option<(i128, i128)> {
        let data = &self.func[inst];
        let writes = matches!(
            data.opcode,
            Opcode::Store | Opcode::Memset | Opcode::Memcpy | Opcode::Memmove
        );
        if !writes || data.flags.contains(Flags::VOLATILE) || !plain(self.func, inst) {
            return None;
        }
        let access = self.alias.writes(inst)?;
        if access.origin != store.access.origin {
            return None;
        }
        access.range()
    }
}

/// Bytes of one object known to hold zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Zeros {
    origin: Origin,
    lo: i128,
    hi: i128,
    /// `None` when a fill zeroed them, after which a store of zero of any type into them may go,
    /// and the type node of the store that did otherwise, which a later store must have too.
    tbaa: Option<Option<Meta>>,
}

impl Zeros {
    /// The bytes as a question to the oracle, with no type, which can only make it answer may.
    fn access(&self) -> Access {
        Access {
            origin: self.origin,
            offset: i64::try_from(self.lo).ok(),
            size: u64::try_from(self.hi - self.lo).ok(),
            tbaa: None,
            restrict: Restrict::NONE,
            volatile: false,
        }
    }
}

impl Walk<'_> {
    /// The stores of zero into bytes that hold zero on every path to them, in the function as it
    /// is once the stores the walks decided on are removed or narrowed.
    fn zeroed(&mut self, decided: &Map<Inst, Fate>) -> Vec<Inst> {
        let mut same = Vec::new();
        let mut out: Map<Block, Vec<Zeros>> = Map::default();
        let order: Vec<Block> = self.cfg.reverse_postorder().collect();
        for block in order {
            let mut known = self.entered(block, &out);
            for inst in self.func.insts(block) {
                let opcode = self.func[inst].opcode;
                if !opcode.writes_memory() || opcode.touches_only_planes() {
                    continue;
                }
                let mut zeros = self.zeros(inst);
                match decided.get(&inst) {
                    Some(Fate::Dead) => continue,
                    Some(&Fate::Trim { skip, size }) => {
                        zeros = zeros.map(|zeros| {
                            let lo = zeros.lo + i128::from(skip);
                            Zeros { lo, hi: lo + i128::from(size), ..zeros }
                        });
                    }
                    _ => {}
                }
                if zeros.is_some_and(|zeros| holds(&known, &zeros)) {
                    same.push(inst);
                    continue;
                }
                self.forget(inst, &mut known);
                if let Some(zeros) = zeros.filter(|_| known.len() < ZEROS) {
                    known.push(zeros);
                }
            }
            out.insert(block, known);
        }
        same
    }

    /// The zeros every edge into this block brings, which is nothing if an edge comes from a block
    /// not yet seen.
    fn entered(&self, block: Block, out: &Map<Block, Vec<Zeros>>) -> Vec<Zeros> {
        let mut preds = self.cfg.predecessors(block).iter().map(|pred| out.get(pred));
        let Some(Some(first)) = preds.next() else { return Vec::new() };
        let mut known = first.clone();
        for pred in preds {
            let Some(pred) = pred else { return Vec::new() };
            known = meet(&known, pred);
        }
        known
    }

    /// The zeros this instruction writes, when it is a plain store of zero or a fill with zero of
    /// bytes the compiler knows.
    fn zeros(&self, inst: Inst) -> Option<Zeros> {
        let data = &self.func[inst];
        let value = match data.opcode {
            Opcode::Store => self.func[data.args][0],
            Opcode::Memset => self.func.bulk(inst)?.with,
            _ => return None,
        };
        if data.flags.contains(Flags::VOLATILE) || data.results().next().is_some() {
            return None;
        }
        if self.func.mem_in(inst).is_some() || !plain(self.func, inst) || !zero(self.func, value) {
            return None;
        }
        let access = self.alias.writes(inst)?;
        let (lo, hi) = access.range().filter(|(lo, hi)| lo < hi)?;
        let tbaa = (data.opcode == Opcode::Store).then_some(access.tbaa);
        Some(Zeros { origin: access.origin, lo, hi, tbaa })
    }

    /// Takes out of what is known the bytes this instruction may write.
    fn forget(&mut self, inst: Inst, known: &mut Vec<Zeros>) {
        if known.is_empty() {
            return;
        }
        let opcode = self.func[inst].opcode;
        // What another thread wrote may become visible here, to anything but a local nobody else
        // has the address of.
        if matches!(opcode, Opcode::Fence | Opcode::AtomicRmw | Opcode::Cmpxchg)
            || !plain_order(self.func, inst)
        {
            known.retain(|zeros| match zeros.origin {
                Origin::Local(local) => !self.alias.escapes().escaped(local),
                _ => false,
            });
        }
        let before = std::mem::take(known);
        if matches!(opcode, Opcode::Call | Opcode::CallIndirect | Opcode::TailCall) {
            for zeros in before {
                if self.alias.clobbered_by(&zeros.access(), inst).is_no() {
                    known.push(zeros);
                }
            }
            return;
        }
        let Some(write) = self.alias.writes(inst) else { return };
        for zeros in before {
            if zeros.origin != write.origin {
                if self.alias.query(&zeros.access(), &write).is_no() {
                    known.push(zeros);
                }
                continue;
            }
            // The same object, where only the bytes the write covers go, and all of them when the
            // write does not say which.
            let Some((start, end)) = write.range() else { continue };
            if end <= zeros.lo || zeros.hi <= start {
                known.push(zeros);
                continue;
            }
            if zeros.lo < start {
                known.push(Zeros { hi: start, ..zeros });
            }
            if end < zeros.hi {
                known.push(Zeros { lo: end, ..zeros });
            }
        }
    }
}

/// The zeros known on both of two edges.
fn meet(a: &[Zeros], b: &[Zeros]) -> Vec<Zeros> {
    let mut both = Vec::new();
    for x in a {
        for y in b.iter().filter(|y| y.origin == x.origin) {
            let tbaa = match (x.tbaa, y.tbaa) {
                (None, tbaa) | (tbaa, None) => tbaa,
                (Some(s), Some(t)) if s == t => Some(s),
                _ => continue,
            };
            let (lo, hi) = (x.lo.max(y.lo), x.hi.min(y.hi));
            if lo < hi && both.len() < ZEROS {
                both.push(Zeros { origin: x.origin, lo, hi, tbaa });
            }
        }
    }
    both
}

/// Whether every byte this writes already holds zero, in spans whose type allows it.
fn holds(known: &[Zeros], store: &Zeros) -> bool {
    let mut left = Live::new(store.lo, store.hi);
    for zeros in known.iter().filter(|zeros| zeros.origin == store.origin) {
        if zeros.tbaa.is_none() || zeros.tbaa == store.tbaa {
            left.remove(zeros.lo, zeros.hi);
        }
    }
    left.is_empty()
}

/// Whether this value is the constant zero, or the null pointer made from it, which is how the
/// front end writes a null pointer constant.
fn zero(func: &Func, value: Value) -> bool {
    let Def::Result { inst, .. } = func[value].def else { return false };
    match (func[inst].opcode, func[inst].extra) {
        (Opcode::IConst, Extra::Imm(at)) => func[at].bits() == 0,
        (Opcode::IntToPtr, _) => zero(func, func[func[inst].args][0]),
        _ => false,
    }
}

/// Whether this access is an ordinary one: not atomic, and of a length the compiler knows.
fn plain(func: &Func, inst: Inst) -> bool {
    plain_order(func, inst) && func.bulk(inst).is_none_or(|bulk| bulk.length.is_none())
}

/// Whether this access has no ordering, which an instruction with no memory operand has not.
fn plain_order(func: &Func, inst: Inst) -> bool {
    match func[inst].extra {
        Extra::Mem(mem) => func[mem].order == MemOrder::NotAtomic,
        _ => true,
    }
}

/// The bytes of a store still live on one path, as spans in order with gaps between them.
///
/// Never empty while a path is walked: the walk ends the path when the last span goes. A handful of
/// spans at most in practice, since a span is only split by a store that lands inside it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Live(Vec<(i128, i128)>);

impl Live {
    fn new(lo: i128, hi: i128) -> Self {
        Self(vec![(lo, hi)])
    }

    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The smallest span holding every live byte.
    fn hull(&self) -> (i128, i128) {
        let first = self.0.first().map_or(0, |&(lo, _)| lo);
        let last = self.0.last().map_or(0, |&(_, hi)| hi);
        (first, last)
    }

    /// Takes the bytes from `start` to `end` out.
    fn remove(&mut self, start: i128, end: i128) {
        let mut left = Vec::with_capacity(self.0.len() + 1);
        for &(lo, hi) in &self.0 {
            if end <= lo || hi <= start {
                left.push((lo, hi));
                continue;
            }
            if lo < start {
                left.push((lo, start));
            }
            if end < hi {
                left.push((end, hi));
            }
        }
        self.0 = left;
    }

    /// The smallest span holding the live bytes of `start` to `end`, if any of them are live.
    fn within(&self, (start, end): (i128, i128)) -> Option<(i128, i128)> {
        self.0
            .iter()
            .map(|&(lo, hi)| (lo.max(start), hi.min(end)))
            .filter(|&(lo, hi)| lo < hi)
            .fold(None, |so_far, span| Some(hull(so_far, span)))
    }

    /// Whether every live byte here is live in `other` as well.
    fn within_all(&self, other: &Self) -> bool {
        self.0.iter().all(|&(lo, hi)| other.0.iter().any(|&(start, end)| start <= lo && hi <= end))
    }
}

/// The smallest span holding both.
fn hull(so_far: Option<(i128, i128)>, (lo, hi): (i128, i128)) -> (i128, i128) {
    so_far.map_or((lo, hi), |(start, end)| (start.min(lo), end.max(hi)))
}

/// Narrows a fill or a copy to `size` bytes starting `skip` bytes into it.
///
/// Both of a copy's addresses move by the same amount and a fill's one address does, and the
/// alignment is what is left of the old one at the new start.
fn trim(func: &mut Func, inst: Inst, skip: i64, size: u64, bits: u32) {
    let data = func[inst];
    let Extra::Mem(mem) = data.extra else { return };
    let info = func[mem];
    let span = func.span(inst);
    let mut args = func[data.args].to_vec();
    let moved = if data.opcode == Opcode::Memset { 1 } else { 2 };
    let mut align = info.align;
    if skip != 0 {
        let ty = Type::int(bits);
        let imm = func.add_imm(Imm::int(i128::from(skip), ty.lane()));
        let data = InstData { extra: Extra::Imm(imm), ..InstData::new(Opcode::IConst) };
        let by = emit(func, inst, data, ty);
        for arg in &mut args[..moved] {
            let pair = func.push_values(&[*arg, by]);
            *arg = emit(
                func,
                inst,
                InstData { args: pair, ..InstData::new(Opcode::PtrAdd) },
                Type::PTR,
            );
        }
        align = align.min(1u32 << skip.trailing_zeros().min(31)).max(1);
    }
    let mem = func.add_mem(MemInfo { size, align, ..info });
    let args = func.push_values(&args);
    let made = func.create_inst(InstData { args, extra: Extra::Mem(mem), ..data }, &[], span);
    func.insert_before(made, inst);
    func.remove_inst(inst);
}

/// Puts this instruction in front of that one and answers its one result.
fn emit(func: &mut Func, before: Inst, data: InstData, ty: Type) -> Value {
    let span = func.span(before);
    let made = func.create_inst(data, &[ty], span);
    func.insert_before(made, before);
    func[made].results().next().expect("one result was asked for")
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rucc_base::Interner;

    use super::Dse;
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
        // What says how wide an address is, which a store of a pointer needs to have a size.
        let outside = Arc::new(crate::Outside::of(&module));
        let ids: Vec<_> = module.funcs().collect();
        for id in ids {
            if module[id].is_declaration() {
                continue;
            }
            let mut an = crate::machine::fixtures::analyses().about(Arc::clone(&outside));
            Dse.run(&mut module[id], &mut an, fuel);
        }
        if let Err(errors) = rucc_ir::verify(&module, &names) {
            panic!("the pass left invalid IR, {errors:?}\n{}", rucc_ir::print(&module, &names));
        }
        rucc_ir::print(&module, &names)
    }

    fn cleaned(body: &str) -> String {
        run(body, &mut Fuel::unlimited())
    }

    fn stores(out: &str) -> usize {
        out.lines().filter(|line| line.trim_start().starts_with("store")).count()
    }

    const TWICE: &str = r#"
func @f(ptr), linkage(external) {
block0(%0: ptr):
    %1 = iconst.i32 1
    store %1 -> %0, align 4
    %2 = iconst.i32 2
    store %2 -> %0, align 4
    return
}
"#;

    /// `*p = 1; *p = 2;` is `*p = 2;`, and the second store stays because the caller may read it.
    #[test]
    fn a_store_overwritten_before_anything_reads_it_goes() {
        let out = cleaned(TWICE);
        assert_eq!(stores(&out), 1, "{out}");
        assert!(out.contains("store %2 -> %0"), "{out}");
    }

    /// A load of the same address in between reads the first store, and a load through another
    /// pointer may, so both keep it.
    #[test]
    fn a_store_something_may_read_in_between_stays() {
        for between in ["%3 = load.i32 %0, align 4", "%3 = load.i32 %1, align 4"] {
            let body = format!(
                r#"
func @f(ptr, ptr) -> i32, linkage(external) {{
block0(%0: ptr, %1: ptr):
    %2 = iconst.i32 1
    store %2 -> %0, align 4
    {between}
    %4 = iconst.i32 2
    store %4 -> %0, align 4
    return %3
}}
"#
            );
            let out = cleaned(&body);
            assert_eq!(stores(&out), 2, "{between} reads the first store, {out}");
        }
    }

    /// A fill that the four stores after it cover completely is removed.
    #[test]
    fn a_fill_the_stores_after_it_cover_goes() {
        let out = cleaned(
            r#"
func @f(ptr), linkage(external) {
block0(%0: ptr):
    %1 = iconst.i8 0
    memset %0, %1, size 16, align 4
    %2 = iconst.i32 1
    store %2 -> %0, align 4
    %3 = iconst.i64 4
    %4 = ptr_add %0, %3
    store %2 -> %4, align 4
    %5 = iconst.i64 8
    %6 = ptr_add %0, %5
    %7 = iconst.i64 0
    store %7 -> %6, align 8
    return
}
"#,
        );
        assert!(!out.contains("memset"), "{out}");
        assert_eq!(stores(&out), 3, "{out}");
    }

    /// A fill covered by stores that start in its middle is removed all the same, the way
    /// `cp_new_stat` zeroes `__pad0` before the fields around it, and one whose hole in the middle
    /// is read before anything writes it stays.
    #[test]
    fn a_fill_covered_from_the_middle_out_goes() {
        let fill = |between: &str| {
            format!(
                r#"
func @f(ptr) -> i32, linkage(external) {{
block0(%0: ptr):
    %1 = iconst.i8 0
    memset %0, %1, size 16, align 8
    %2 = iconst.i32 1
    %3 = iconst.i64 4
    %4 = ptr_add %0, %3
    store %2 -> %4, align 4
    {between}
    store %2 -> %0, align 4
    %6 = iconst.i64 8
    %7 = ptr_add %0, %6
    %8 = iconst.i64 0
    store %8 -> %7, align 8
    return %5
}}
"#
            )
        };
        let out = cleaned(&fill("%5 = iconst.i32 0"));
        assert!(!out.contains("memset"), "{out}");
        assert_eq!(stores(&out), 3, "{out}");
        let out = cleaned(&fill("%5 = load.i32 %0, align 4"));
        assert!(out.contains("memset"), "the load reads the first word, {out}");
    }

    /// A fill whose first half is overwritten becomes a fill of the second half.
    #[test]
    fn a_fill_half_overwritten_is_narrowed_to_the_other_half() {
        let out = cleaned(
            r#"
func @f(ptr), linkage(external) {
block0(%0: ptr):
    %1 = iconst.i8 0
    memset %0, %1, size 16, align 16
    %2 = iconst.i64 7
    store %2 -> %0, align 8
    return
}
"#,
        );
        assert!(out.contains("size 8, align 8"), "{out}");
        assert!(out.contains("iconst.i64 8"), "eight bytes in, {out}");
        assert!(!out.contains("size 16"), "{out}");
    }

    /// A fill whose first byte is overwritten keeps its aligned start, since narrowing it by one
    /// byte would leave every word of it misaligned, and one whose first twelve bytes are
    /// overwritten loses the first eight and no more.
    #[test]
    fn a_fill_narrowed_keeps_its_start_aligned() {
        let out = cleaned(
            r#"
func @f(ptr), linkage(external) {
block0(%0: ptr):
    %1 = iconst.i8 0
    memset %0, %1, size 64, align 8
    %2 = iconst.i8 7
    store %2 -> %0, align 8
    return
}
"#,
        );
        assert!(out.contains("memset %0, %1, size 64, align 8"), "{out}");
        assert!(!out.contains("ptr_add"), "{out}");

        let out = cleaned(
            r#"
func @f(ptr), linkage(external) {
block0(%0: ptr):
    %1 = iconst.i8 0
    memset %0, %1, size 64, align 16
    %2 = iconst.i64 7
    store %2 -> %0, align 16
    %3 = iconst.i64 4
    %4 = ptr_add %0, %3
    %5 = iconst.i64 9
    store %5 -> %4, align 4
    return
}
"#,
        );
        assert!(out.contains("size 56, align 8"), "{out}");
        assert!(out.contains("iconst.i64 8"), "eight bytes in, {out}");
    }

    /// On a 32-bit target the distance is a 32-bit integer, since a 64-bit one is a value i386
    /// has no instruction to add to an address.
    #[test]
    fn a_fill_narrowed_on_a_32_bit_target_moves_its_address_by_a_32_bit_distance() {
        let head = r#"; ModuleID = 't.c'
; format 0
target triple = "i686-unknown-linux-gnu"
target datalayout = "e-p:32:32-i64:32-f80:128-S128"
"#;
        let body = r#"
func @f(ptr), linkage(external) {
block0(%0: ptr):
    %1 = iconst.i8 0
    memset %0, %1, size 16, align 4
    %2 = iconst.i32 7
    store %2 -> %0, align 4
    return
}
"#;
        let mut names = Interner::new();
        let mut module = rucc_ir::parse(&format!("{head}{body}"), &mut names).expect("it parses");
        // What says how wide an address is, which a module the driver hands the pass always has.
        let outside = Arc::new(crate::Outside::of(&module));
        let id = module.funcs().next().expect("one function");
        let mut an = crate::machine::fixtures::analyses().about(outside);
        Dse.run(&mut module[id], &mut an, &mut Fuel::unlimited());
        if let Err(errors) = rucc_ir::verify(&module, &names) {
            panic!("the pass left invalid IR, {errors:?}");
        }
        let out = rucc_ir::print(&module, &names);
        assert!(out.contains("size 12, align 4"), "{out}");
        assert!(out.contains("iconst.i32 4"), "four bytes in, {out}");
        assert!(!out.contains("iconst.i64"), "{out}");
    }

    /// A store both arms of a branch overwrite is dead, and one only one arm overwrites is not.
    #[test]
    fn a_store_every_path_overwrites_goes_and_one_some_path_reads_stays() {
        let both = cleaned(
            r#"
func @f(ptr, i1), linkage(external) {
block0(%0: ptr, %1: i1):
    %2 = iconst.i32 0
    store %2 -> %0, align 4
    br_if %1, block1, block2
block1:
    %3 = iconst.i32 1
    store %3 -> %0, align 4
    return
block2:
    %4 = iconst.i32 2
    store %4 -> %0, align 4
    return
}
"#,
        );
        assert_eq!(stores(&both), 2, "{both}");
        assert!(!both.contains("store %2"), "{both}");

        let one = cleaned(
            r#"
func @f(ptr, i1), linkage(external) {
block0(%0: ptr, %1: i1):
    %2 = iconst.i32 0
    store %2 -> %0, align 4
    br_if %1, block1, block2
block1:
    %3 = iconst.i32 1
    store %3 -> %0, align 4
    return
block2:
    return
}
"#,
        );
        assert_eq!(stores(&one), 2, "{one}");
    }

    /// A volatile store is one the program asked for, and it stays however dead it is.
    #[test]
    fn a_volatile_store_stays() {
        let out = cleaned(
            r#"
func @f(ptr), linkage(external) {
block0(%0: ptr):
    %1 = iconst.i32 1
    store.volatile %1 -> %0, align 4
    %2 = iconst.i32 2
    store %2 -> %0, align 4
    return
}
"#,
        );
        assert_eq!(stores(&out), 2, "{out}");
    }

    /// A store to a local nothing reads before the return is dead, and one a call is handed the
    /// address of before the return is not.
    #[test]
    fn a_store_to_a_local_nothing_reads_before_the_return_goes() {
        let out = cleaned(
            r#"
func @f(i32) -> i32, linkage(external) {
block0(%0: i32):
    %1 = alloca, size 4, align 4
    store %0 -> %1, align 4
    return %0
}
"#,
        );
        assert_eq!(stores(&out), 0, "{out}");

        let out = cleaned(
            r#"
func @use(ptr), linkage(external);

func @f(i32) -> i32, linkage(external) {
block0(%0: i32):
    %1 = alloca, size 4, align 4
    store %0 -> %1, align 4
    call @use(%1) : (ptr)
    return %0
}
"#,
        );
        assert_eq!(stores(&out), 1, "{out}");
    }

    /// A fill of a local whose address a call was handed earlier is dead when the local's
    /// lifetime ends after it, which is `memset(k, 0, sizeof k)` in front of a return, and the end
    /// of another local's lifetime reads nothing of it.
    #[test]
    fn a_fill_of_a_local_whose_lifetime_ends_after_it_goes() {
        let out = cleaned(
            r#"
func @use(ptr), linkage(external);

func @f(), linkage(external) {
block0:
    %0 = alloca, size 32, align 16
    %1 = alloca, size 8, align 8
    call @use(%0) : (ptr)
    call @use(%1) : (ptr)
    %2 = iconst.i8 0
    memset %0, %2, size 32, align 16
    lifetime_end %1
    lifetime_end %0
    call @use(%1) : (ptr)
    return
}
"#,
        );
        assert!(!out.contains("memset"), "{out}");

        // The end of the other local is not the end of this one, and the call after it may read
        // this one through the address it was handed.
        let out = cleaned(
            r#"
func @use(ptr), linkage(external);

func @f(), linkage(external) {
block0:
    %0 = alloca, size 32, align 16
    %1 = alloca, size 8, align 8
    call @use(%0) : (ptr)
    %2 = iconst.i8 0
    memset %0, %2, size 32, align 16
    lifetime_end %1
    call @use(%1) : (ptr)
    lifetime_end %0
    return
}
"#,
        );
        assert!(out.contains("memset"), "{out}");
    }

    /// `*p = 0` with `p` moving on every trip writes a new word each time. The walk comes round
    /// to the same store again, and that store writes a different address that time, so the first
    /// write is not overwritten even though the store on the way out covers the last one.
    #[test]
    fn a_store_whose_address_moves_round_a_loop_is_not_overwritten_by_itself() {
        let out = cleaned(
            r#"
func @f(ptr, i1), linkage(external) {
block0(%0: ptr, %1: i1):
    jump block1(%0)
block1(%2: ptr):
    %3 = iconst.i32 0
    store %3 -> %2, align 4
    %4 = iconst.i64 4
    %5 = ptr_add %2, %4
    br_if %1, block1(%5), block2
block2:
    %6 = iconst.i32 1
    store %6 -> %2, align 4
    return
}
"#,
        );
        assert_eq!(stores(&out), 2, "{out}");
    }

    /// A fill whose middle the call reads around, then a store of zero into that middle after the
    /// two arms meet: the store goes when neither arm writes there, and stays when one arm writes
    /// something else there that a call reads.
    #[test]
    fn a_store_of_zero_into_bytes_every_path_zeroed_goes() {
        let zeroed = |left: &str| {
            format!(
                r#"
func @use(ptr), linkage(external);

func @f(ptr, i1), linkage(external) {{
block0(%0: ptr, %1: i1):
    %2 = iconst.i8 0
    memset %0, %2, size 16, align 8
    %3 = iconst.i64 4
    %4 = ptr_add %0, %3
    br_if %1, block1, block2
block1:
    {left}
    jump block3
block2:
    jump block3
block3:
    %6 = iconst.i32 0
    store %6 -> %4, align 4
    call @use(%0) : (ptr)
    return
}}
"#
            )
        };
        let out = cleaned(&zeroed("%5 = iconst.i32 0"));
        assert!(out.contains("memset"), "{out}");
        assert_eq!(stores(&out), 0, "{out}");
        let out = cleaned(&zeroed(
            "%5 = iconst.i32 7\n    store %5 -> %4, align 4\n    call @use(%0) : (ptr)",
        ));
        assert_eq!(stores(&out), 2, "the arm stores seven there, {out}");
    }

    /// A null pointer is zero too, which the front end writes as an integer zero made a pointer.
    #[test]
    fn a_store_of_a_null_pointer_into_zeros_goes() {
        let out = cleaned(
            r#"
func @use(ptr), linkage(external);

func @f(ptr), linkage(external) {
block0(%0: ptr):
    %1 = iconst.i8 0
    memset %0, %1, size 16, align 8
    %2 = iconst.i64 0
    %3 = inttoptr.ptr %2
    %4 = iconst.i64 4
    %5 = ptr_add %0, %4
    store %3 -> %5, align 4
    call @use(%0) : (ptr)
    return
}
"#,
        );
        assert!(out.contains("memset"), "{out}");
        assert_eq!(stores(&out), 0, "{out}");
    }

    /// A fill the stores after it cover goes, and then the stores of zero among them are what
    /// zeroes their bytes and stay, the way `cp_new_stat` zeroes `__pad0`.
    #[test]
    fn a_store_of_zero_into_a_fill_that_goes_stays() {
        let out = cleaned(
            r#"
func @f(ptr), linkage(external) {
block0(%0: ptr):
    %1 = iconst.i8 0
    memset %0, %1, size 8, align 8
    %2 = iconst.i32 0
    store %2 -> %0, align 4
    %3 = iconst.i64 4
    %4 = ptr_add %0, %3
    %5 = iconst.i32 1
    store %5 -> %4, align 4
    return
}
"#,
        );
        assert!(!out.contains("memset"), "{out}");
        assert_eq!(stores(&out), 2, "{out}");
    }

    /// Zero stored twice with the same type goes the second time, and with another type stays,
    /// which is gcc's rule for a store that is not a fill.
    #[test]
    fn a_store_of_zero_after_a_store_of_zero_goes_only_with_the_same_type() {
        let twice = |second: &str| {
            format!(
                r#"
func @f(ptr) -> i32, linkage(external) {{
block0(%0: ptr):
    %1 = iconst.i32 0
    store %1 -> %0, align 4, tbaa !1
    %2 = load.i32 %0, align 4, tbaa !1
    store %1 -> %0, align 4, tbaa {second}
    return %2
}}
!0 = tbaa "char", offset 0
!1 = tbaa "int", parent !0, offset 0
!2 = tbaa "float", parent !0, offset 0
"#
            )
        };
        assert_eq!(stores(&cleaned(&twice("!1"))), 1);
        assert_eq!(stores(&cleaned(&twice("!2"))), 2);
    }

    /// Fuel stops the removal and not the walk.
    #[test]
    fn fuel_stops_the_removing() {
        assert_eq!(stores(&run(TWICE, &mut Fuel::of(0))), 2);
        assert_eq!(stores(&run(TWICE, &mut Fuel::of(1))), 1);
    }
}
