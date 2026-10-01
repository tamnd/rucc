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
//! covers off the front or the back of that span, and a path ends when nothing is left. A load, a
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

use rucc_base::hash::Map;
use rucc_cost::heuristics::DSE_WALK_LIMIT;
use rucc_ir::{
    Block, Extra, Flags, Func, Imm, Inst, InstData, MemInfo, MemOrder, Opcode, Type, Value,
};

use crate::alias::{Access, Alias, Origin};
use crate::cfg::Cfg;
use crate::dom::Dominators;
use crate::{Analyses, Analysis, Fuel, Pass, Preserved, Stats};

/// What this pass is called, which the pipeline matches on to decide whether to build the module
/// facts the oracle asks for.
pub const NAME: &str = "dse";

/// Recorded for a store removed because nothing can read what it wrote.
const REMOVED: &str =
    "store removed, nothing reads it before it is overwritten or goes out of scope";

/// Recorded for a fill or a copy narrowed to the bytes something may read.
const TRIMMED: &str = "fill or copy narrowed to the bytes something may still read";

/// Recorded for a store kept because the walk from it reached its limit.
const TOO_FAR: &str = "store kept, the walk for what reads it reached its limit";

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

    /// Walks every path from just after the store, which is in this block at this index.
    fn fate(&mut self, store: &Store, home: Block, next: usize) -> Fate {
        let mut needed: Option<(i128, i128)> = None;
        let mut steps = 0u32;
        // The spans each block has been entered with. A block entered again with a span inside
        // one it was already walked with can find nothing the first walk did not, since fewer live
        // bytes can only be read less.
        let mut entered: Map<Block, Vec<(i128, i128)>> = Map::default();
        let mut stack = vec![(home, next, store.lo, store.hi)];
        while let Some((block, from, mut lo, mut hi)) = stack.pop() {
            let mut ended = false;
            for inst in self.func.insts(block).skip(from) {
                steps += 1;
                if steps > DSE_WALK_LIMIT {
                    return Fate::TooFar;
                }
                if let Some(read) = self.read(store, inst, lo, hi) {
                    needed = Some(hull(needed, read));
                }
                match self.func[inst].opcode {
                    Opcode::Return | Opcode::TailCall => {
                        // A local is gone when the function is, and anything else is still there
                        // for the caller to read.
                        if !matches!(store.access.origin, Origin::Local(_)) {
                            needed = Some(hull(needed, (lo, hi)));
                        }
                        ended = true;
                        break;
                    }
                    // A program never gets here, so nothing is ever read here.
                    Opcode::Unreachable | Opcode::UnreachableHint => {
                        ended = true;
                        break;
                    }
                    _ => {}
                }
                if let Some((start, end)) = self.covers(store, inst) {
                    if start <= lo && hi <= end {
                        ended = true;
                        break;
                    }
                    if start <= lo && lo < end {
                        lo = end;
                    } else if start < hi && hi <= end {
                        hi = start;
                    }
                }
            }
            if ended {
                continue;
            }
            let successors = self.cfg.successors(block);
            if successors.is_empty() {
                needed = Some(hull(needed, (lo, hi)));
            }
            for &successor in successors {
                if self.dom.dominates(successor, home) {
                    return Fate::Kept;
                }
                let spans = entered.entry(successor).or_default();
                if spans.iter().any(|&(start, end)| start <= lo && hi <= end) {
                    continue;
                }
                spans.push((lo, hi));
                stack.push((successor, 0, lo, hi));
            }
        }
        match needed {
            None => Fate::Dead,
            Some((start, end)) if store.bulk && (store.lo < start || end < store.hi) => {
                match (i64::try_from(start - store.lo), u64::try_from(end - start)) {
                    (Ok(skip), Ok(size)) => Fate::Trim { skip, size },
                    _ => Fate::Kept,
                }
            }
            Some(_) => Fate::Kept,
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
            // A write reads nothing.
            Opcode::Store | Opcode::Memset | Opcode::AtomicStore => None,
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
        let ids: Vec<_> = module.funcs().collect();
        for id in ids {
            if module[id].is_declaration() {
                continue;
            }
            let mut an = crate::machine::fixtures::analyses();
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
        let outside = std::sync::Arc::new(crate::Outside::of(&module));
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

    /// Fuel stops the removal and not the walk.
    #[test]
    fn fuel_stops_the_removing() {
        assert_eq!(stores(&run(TWICE, &mut Fuel::of(0))), 2);
        assert_eq!(stores(&run(TWICE, &mut Fuel::of(1))), 1);
    }
}
