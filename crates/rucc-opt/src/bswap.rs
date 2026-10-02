//! Byte swaps and byte by byte reads written out by hand become one `bswap` or one load.
//!
//! gcc's `pass_optimize_bswap` in `gimple-ssa-store-merging.cc`, which section 19 lists with the
//! rest of the arithmetic identities. A reader of a big endian file writes
//! `p[0] << 24 | p[1] << 16 | p[2] << 8 | p[3]`, and a hash function swaps the bytes of a word with
//! three shifts, two masks and three ors. Each is one instruction or two, and no local rule sees
//! it, because every node of the tree on its own is exactly as good as it can be.
//!
//! # How a tree is read
//!
//! From a root, which is an `or`, `xor`, `add` or `trunc`, the pass works out where every byte of
//! the result comes from, the way gcc's `find_bswap_or_nop_1` does with its symbolic number. A
//! byte is one of three things: a byte of one value, a byte of memory at a fixed offset from one
//! address, or zero. An `or`, `xor` or `add` whose two sides have no byte that is not zero in
//! common puts them together, and on such bytes the three are the same operation. A shift by a
//! whole number of bytes moves them, an `and` with a mask whose bytes are all zero or all one
//! clears some, `zext` adds zeros at the top and `trunc` drops the top. A plain load of a whole
//! number of bytes, from an address that is one pointer plus a constant, gives bytes of memory in
//! the order the target keeps them. Anything else is a value, whose bytes are its own. The walk
//! goes as deep as gcc's does, which is the root's byte count plus one plus its base two log.
//!
//! Only a node the tree alone reads, in the same block as the root, is walked into, and the rest
//! are values. So when the tree is rewritten every node under the root goes with it, and the
//! pass never leaves the old tree beside the new one.
//!
//! # What it becomes
//!
//! The bytes that are not zero have to be the low `n` of the result and all come from one place.
//! From one value, in order they are the value itself and reversed they are its `bswap`, each
//! narrowed to `n` bytes first and widened to the result after. From memory, `n` bytes at
//! neighbouring offsets in the order a load reads them are one load, and in the other order a
//! load and a `bswap`. A reversal needs `n` to be two, four or eight, the widths `bswap` has.
//!
//! The wider load reads only bytes a load in the tree read already, so it reads nothing the
//! program did not. It goes where the root is, and so the pass asks that nothing between the
//! first of the loads and the root may write memory, and that no load in the tree is volatile or
//! atomic. The rewrite is done only when it makes no more instructions than it removes.

use rucc_base::hash::{Map, Set};
use rucc_ir::{
    Block, Def, Extra, Flags, Func, Inst, InstData, MemInfo, MemOrder, Opcode, Restrict, Type,
    Value,
};

use crate::uses::{count, substitute};
use crate::{Analyses, Analysis, Fuel, Pass, Preserved, Stats};

/// What this pass is called, for the lists in [`crate::pipeline`] that name it.
pub const NAME: &str = "bswap";

/// Recorded for each tree that put the bytes of one value back in the reverse order.
const SWAPPED: &str = "bytes of a value put back in reverse order written as one byte swap";

/// Recorded for each tree that put the bytes of one value back where they were.
const IN_ORDER: &str = "bytes of a value put back in order written as the value itself";

/// Recorded for each tree of loads of neighbouring bytes written as one load.
const MERGED: &str = "loads of neighbouring bytes merged into one load";

/// Recorded for a tree that would have been rewritten if there had been fuel for it.
const NO_FUEL: &str = "byte tree left as it was, the pass ran out of fuel";

/// The pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bswap;

/// The one instance, which is what the pipelines name.
pub static BSWAP: Bswap = Bswap;

impl Pass for Bswap {
    fn name(&self) -> &'static str {
        NAME
    }

    fn describe(&self) -> &'static str {
        "a byte swap or a read of neighbouring bytes written out by hand becomes one instruction"
    }

    fn preserves(&self) -> Preserved {
        // Instructions come and go inside blocks and no edge moves.
        Preserved::ALL.without(Analysis::Liveness)
    }

    fn run(&self, func: &mut Func, an: &mut Analyses, fuel: &mut Fuel) -> Stats {
        let mut stats = Stats::new();
        let little = an.outside().little_endian();
        let uses = count(func);
        let mut done: Set<Inst> = Set::default();
        let mut forward: Map<Value, Value> = Map::default();
        let mut gone: Vec<Inst> = Vec::new();
        for block in func.blocks().collect::<Vec<Block>>() {
            // Bottom up, so that the widest tree is met first and the nodes under it are marked
            // before the walk reaches them.
            for inst in func.insts_backwards(block).collect::<Vec<Inst>>() {
                if done.contains(&inst) || !root(func, inst) {
                    continue;
                }
                let mut walk = Walk { func, uses: &uses, block, little, inside: Vec::new() };
                let Some(bytes) = walk.root(inst) else { continue };
                let Some(shape) = shape(func, &bytes, &walk.inside) else { continue };
                if shape.cost() > walk.inside.len() + 1 || !quiet(func, inst, &walk.inside) {
                    continue;
                }
                if !fuel.take() {
                    stats.missed(NO_FUEL);
                    continue;
                }
                let inside = walk.inside;
                let result = func[inst].first_result.expect("a root has a result");
                let ty = func[result].ty;
                let value = shape.build(func, inst, ty);
                stats.optimized(shape.reason());
                forward.insert(result, value);
                done.extend(inside.iter().copied());
                gone.push(inst);
                gone.extend(inside);
            }
        }
        if forward.is_empty() {
            return stats;
        }
        substitute(func, &forward);
        for inst in gone {
            func.remove_inst(inst);
        }
        stats
    }
}

/// Whether a tree may start at this instruction.
fn root(func: &Func, inst: Inst) -> bool {
    let opcode = func[inst].opcode;
    matches!(opcode, Opcode::Or | Opcode::Xor | Opcode::Add | Opcode::Trunc)
        && func[inst].first_result.is_some_and(|result| bytes_of(func[result].ty).is_some())
}

/// How many bytes a value of this type is, when it is an integer of a whole number of them that
/// fits in a register.
fn bytes_of(ty: Type) -> Option<usize> {
    let bits = ty.bits();
    (ty.is_int() && ty.is_scalar() && bits % 8 == 0 && (8..=64).contains(&bits))
        .then_some(bits as usize / 8)
}

/// Where the bytes of a tree come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    /// The bytes of one value, numbered from its least significant.
    Value(Value),
    /// The bytes of memory from an address on, numbered from it.
    Memory(Value),
}

/// Where each byte of a value comes from, least significant first.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Bytes {
    /// Where the bytes that are not zero come from, which is `None` when every one is zero.
    source: Option<Source>,
    /// For each byte, which byte of the source it is, and `None` for a byte that is zero.
    bytes: Vec<Option<i64>>,
}

impl Bytes {
    /// A value's own bytes, in order.
    fn value(value: Value, width: usize) -> Self {
        Self { source: Some(Source::Value(value)), bytes: (0..width as i64).map(Some).collect() }
    }

    /// Both sides together, when no byte is set on both and they come from one place.
    fn join(self, other: Self) -> Option<Self> {
        let source = match (self.source, other.source) {
            (Some(one), Some(two)) if one != two => return None,
            (one, two) => one.or(two),
        };
        let mut bytes = Vec::with_capacity(self.bytes.len());
        for (&one, &two) in self.bytes.iter().zip(&other.bytes) {
            bytes.push(match (one, two) {
                (Some(_), Some(_)) => return None,
                (one, two) => one.or(two),
            });
        }
        Some(Self { source, bytes })
    }
}

/// One walk down from a root.
struct Walk<'a> {
    func: &'a Func,
    /// How many times each value is read, from before the pass changed anything.
    uses: &'a [u32],
    /// The root's block, which is the only one the walk goes into.
    block: Block,
    /// The target's byte order, when the module says it.
    little: Option<bool>,
    /// The instructions under the root that the answer was worked out through.
    inside: Vec<Inst>,
}

impl Walk<'_> {
    /// The bytes of the root, when it is something this pass can say.
    fn root(&mut self, inst: Inst) -> Option<Bytes> {
        let result = self.func[inst].first_result?;
        let width = bytes_of(self.func[result].ty)?;
        let limit = width + 1 + width.next_power_of_two().trailing_zeros() as usize;
        let bytes = self.node(inst, width, limit)?;
        // A root with nothing under it has nothing to rewrite.
        (!self.inside.is_empty()).then_some(bytes)
    }

    /// The bytes of a value, which are its own when it is not a node this pass walks into.
    fn bytes(&mut self, value: Value, limit: usize) -> Option<Bytes> {
        let width = bytes_of(self.func[value].ty)?;
        let Def::Result { inst, .. } = self.func[value].def else {
            return Some(Bytes::value(value, width));
        };
        let own = self.func.block_of(inst) == Some(self.block)
            && self.uses.get(value.index()).copied() == Some(1);
        if limit == 0 || !own {
            return Some(Bytes::value(value, width));
        }
        let mark = self.inside.len();
        match self.node(inst, width, limit) {
            Some(bytes) => {
                self.inside.push(inst);
                Some(bytes)
            }
            None => {
                self.inside.truncate(mark);
                Some(Bytes::value(value, width))
            }
        }
    }

    /// The bytes of an instruction's result, worked out through it, and `None` when it is not
    /// something this pass reads through.
    fn node(&mut self, inst: Inst, width: usize, limit: usize) -> Option<Bytes> {
        let func = self.func;
        let data = &func[inst];
        let args = &func[data.args];
        let below = limit - 1;
        match (data.opcode, args) {
            (Opcode::Or | Opcode::Xor | Opcode::Add, &[one, two]) => {
                let one = self.bytes(one, below)?;
                let two = self.bytes(two, below)?;
                one.join(two)
            }
            (Opcode::Shl | Opcode::LShr, &[from, amount]) => {
                let amount = crate::discharge::constant(func, amount)?;
                if amount < 0 || amount % 8 != 0 || amount >= 8 * width as i128 {
                    return None;
                }
                let by = amount as usize / 8;
                let mut bytes = self.bytes(from, below)?;
                if data.opcode == Opcode::Shl {
                    bytes.bytes.truncate(width - by);
                    bytes.bytes.splice(0..0, std::iter::repeat_n(None, by));
                } else {
                    bytes.bytes.drain(..by);
                    bytes.bytes.extend(std::iter::repeat_n(None, by));
                }
                Some(bytes)
            }
            (Opcode::And, &[one, two]) => {
                let (from, mask) = match crate::discharge::constant(func, two) {
                    Some(mask) => (one, mask),
                    None => (two, crate::discharge::constant(func, one)?),
                };
                let mut bytes = self.bytes(from, below)?;
                for (at, byte) in bytes.bytes.iter_mut().enumerate() {
                    match (mask >> (8 * at)) & 0xff {
                        0 => *byte = None,
                        0xff => {}
                        _ => return None,
                    }
                }
                Some(bytes)
            }
            (Opcode::ZExt, &[from]) => {
                let mut bytes = self.bytes(from, below)?;
                bytes.bytes.resize(width, None);
                Some(bytes)
            }
            (Opcode::Trunc, &[from]) => {
                let mut bytes = self.bytes(from, below)?;
                bytes.bytes.truncate(width);
                Some(bytes)
            }
            (Opcode::Load, &[address]) => {
                let little = self.little?;
                let Extra::Mem(mem) = data.extra else { return None };
                if func[mem].order != MemOrder::NotAtomic || data.flags.contains(Flags::VOLATILE) {
                    return None;
                }
                let (base, offset) = split(func, address)?;
                let bytes = (0..width as i64)
                    .map(|at| Some(offset + if little { at } else { width as i64 - 1 - at }))
                    .collect();
                Some(Bytes { source: Some(Source::Memory(base)), bytes })
            }
            _ => None,
        }
    }
}

/// An address as a pointer and a constant offset from it, through any number of `ptr_add`s.
fn split(func: &Func, address: Value) -> Option<(Value, i64)> {
    let mut base = address;
    let mut offset: i64 = 0;
    loop {
        let Def::Result { inst, .. } = func[base].def else { return Some((base, offset)) };
        let data = &func[inst];
        let &[from, by] = &func[data.args] else { return Some((base, offset)) };
        if data.opcode != Opcode::PtrAdd {
            return Some((base, offset));
        }
        let Some(by) = crate::discharge::constant(func, by) else { return Some((base, offset)) };
        offset = offset.checked_add(i64::try_from(by).ok()?)?;
        base = from;
    }
}

/// Whether nothing from the first load in the tree to the root may write memory, which is what
/// lets one load at the root read what they read.
fn quiet(func: &Func, root: Inst, inside: &[Inst]) -> bool {
    let loads: Set<Inst> =
        inside.iter().copied().filter(|&inst| func[inst].opcode == Opcode::Load).collect();
    if loads.is_empty() {
        return true;
    }
    let Some(block) = func.block_of(root) else { return false };
    let mut reading = false;
    for inst in func.insts(block) {
        if inst == root {
            return true;
        }
        reading |= loads.contains(&inst);
        if reading && func[inst].opcode.writes_memory() {
            return false;
        }
    }
    false
}

/// What a tree becomes.
#[derive(Clone, Copy, Debug)]
enum Shape {
    /// The low `bytes` of a value, swapped or not.
    Value { value: Value, bytes: usize, swap: bool },
    /// A load of `bytes` from an address the tree already has, swapped or not.
    Memory { address: Value, align: u32, bytes: usize, swap: bool },
}

/// What the bytes of a tree make, when they make something one or two instructions can say.
fn shape(func: &Func, bytes: &Bytes, inside: &[Inst]) -> Option<Shape> {
    let used = bytes.bytes.iter().rposition(Option::is_some)? + 1;
    let low: Vec<i64> = bytes.bytes[..used].iter().copied().collect::<Option<Vec<i64>>>()?;
    let least = *low.iter().min()?;
    let in_order = low.iter().enumerate().all(|(at, &byte)| byte == least + at as i64);
    let reversed = low.iter().enumerate().all(|(at, &byte)| byte == least + (used - 1 - at) as i64);
    let swap = !in_order;
    if !in_order && !(reversed && matches!(used, 2 | 4 | 8)) {
        return None;
    }
    match bytes.source? {
        Source::Value(value) => {
            let width = bytes_of(func[value].ty)?;
            (least == 0 && used <= width).then_some(Shape::Value { value, bytes: used, swap })
        }
        Source::Memory(_) => {
            if !matches!(used, 2 | 4 | 8) {
                return None;
            }
            // The address of the lowest byte is the address of a load in the tree that starts
            // there, and that load says how aligned it is.
            let first = inside.iter().copied().find(|&inst| {
                func[inst].opcode == Opcode::Load
                    && split(func, func[func[inst].args][0]).is_some_and(|(_, at)| at == least)
            })?;
            let Extra::Mem(mem) = func[first].extra else { return None };
            let address = func[func[first].args][0];
            let align = func[mem].align.min(used as u32);
            Some(Shape::Memory { address, align, bytes: used, swap })
        }
    }
}

impl Shape {
    /// The most instructions [`Shape::build`] makes for it, so that a tree is only rewritten when
    /// that is fewer than it had.
    fn cost(self) -> usize {
        match self {
            Self::Value { swap, .. } => 2 + usize::from(swap),
            Self::Memory { swap, .. } => 2 + usize::from(swap),
        }
    }

    fn reason(self) -> &'static str {
        match self {
            Self::Value { swap: true, .. } => SWAPPED,
            Self::Value { swap: false, .. } => IN_ORDER,
            Self::Memory { .. } => MERGED,
        }
    }

    /// The instructions for it, in front of the root, and the value that replaces the root's.
    fn build(self, func: &mut Func, before: Inst, ty: Type) -> Value {
        let (mut value, bytes, swap) = match self {
            Self::Value { value, bytes, swap } => {
                (resize(func, before, value, Type::int(8 * bytes as u32)), bytes, swap)
            }
            Self::Memory { address, align, bytes, swap } => {
                let mem = func.add_mem(MemInfo {
                    size: 0,
                    align,
                    order: MemOrder::NotAtomic,
                    tbaa: None,
                    owns: 0,
                    restrict: Restrict::NONE,
                });
                let args = func.push_values(&[address]);
                let data = InstData { args, extra: Extra::Mem(mem), ..InstData::new(Opcode::Load) };
                (emit(func, before, data, Type::int(8 * bytes as u32)), bytes, swap)
            }
        };
        if swap {
            let args = func.push_values(&[value]);
            let data = InstData { args, ..InstData::new(Opcode::Bswap) };
            value = emit(func, before, data, Type::int(8 * bytes as u32));
        }
        resize(func, before, value, ty)
    }
}

/// That value in an integer type of another width, cut down or with zeros put on top.
fn resize(func: &mut Func, before: Inst, value: Value, ty: Type) -> Value {
    let opcode = match func[value].ty.bits().cmp(&ty.bits()) {
        std::cmp::Ordering::Equal => return value,
        std::cmp::Ordering::Greater => Opcode::Trunc,
        std::cmp::Ordering::Less => Opcode::ZExt,
    };
    let args = func.push_values(&[value]);
    emit(func, before, InstData { args, ..InstData::new(opcode) }, ty)
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
    use rucc_ir::{Module, parse, print, verify_func};

    use super::*;
    use crate::outside::Outside;

    const HEADER: &str = "\
; ModuleID = 'bswap.c'
; format 0
target triple = \"x86_64-unknown-linux-gnu\"
target datalayout = \"e-p:64:64-i64:64-f80:128-S128\"
";

    fn wrap(signature: &str, body: &str) -> String {
        format!("{HEADER}\nfunc @f{signature}, linkage(external) {{\n{body}}}\n")
    }

    /// Runs the pass over `@f`, insists the result verifies, and gives back its text.
    fn with(text: &str, fuel: &mut Fuel) -> String {
        let mut names = Interner::new();
        let mut module: Module = parse(text, &mut names).expect("the text parses");
        let id = module.funcs().last().expect("one function");
        let outside = Arc::new(Outside::of(&module));
        let mut an = crate::machine::fixtures::analyses().about(outside);
        BSWAP.run(&mut module[id], &mut an, fuel);
        if let Err(errors) = verify_func(&module, &module[id], &names) {
            panic!("{errors:#?}\n{}", print(&module, &names));
        }
        print(&module, &names)
    }

    fn run(text: &str) -> String {
        with(text, &mut Fuel::unlimited())
    }

    fn count(text: &str, what: &str) -> usize {
        text.matches(what).count()
    }

    /// `(x >> 24) | ((x >> 8) & 0xff00) | ((x << 8) & 0xff0000) | (x << 24)`.
    const SWAP32: &str = "block0(%0: i32):
    %1 = iconst.i32 24
    %2 = lshr %0, %1
    %3 = iconst.i32 8
    %4 = lshr %0, %3
    %5 = iconst.i32 65280
    %6 = and %4, %5
    %7 = or %2, %6
    %8 = shl %0, %3
    %9 = iconst.i32 16711680
    %10 = and %8, %9
    %11 = or %7, %10
    %12 = shl %0, %1
    %13 = or %11, %12
    return %13
";

    #[test]
    fn the_bytes_of_a_value_put_back_the_other_way_round_are_a_byte_swap() {
        let out = run(&wrap("(i32) -> i32", SWAP32));
        assert_eq!(count(&out, " = bswap"), 1, "{out}");
        assert_eq!(count(&out, " = or "), 0, "{out}");
        assert_eq!(count(&out, " = shl "), 0, "{out}");
    }

    #[test]
    fn a_swap_of_the_low_half_of_a_wider_value_narrows_it_first() {
        // `(uint16_t)((x >> 8) | (x << 8))` for an `x` that came in as an int.
        let out = run(&wrap(
            "(i32) -> i16",
            "block0(%0: i32):
    %1 = iconst.i32 8
    %2 = iconst.i32 255
    %3 = lshr %0, %1
    %4 = and %3, %2
    %5 = shl %0, %1
    %6 = or %4, %5
    %7 = trunc.i16 %6
    return %7
",
        ));
        assert_eq!(count(&out, " = trunc.i16 %0"), 1, "{out}");
        assert_eq!(count(&out, " = bswap "), 1, "{out}");
        assert_eq!(count(&out, " = or "), 0, "{out}");
    }

    /// `p[0] << 24 | p[1] << 16 | p[2] << 8 | p[3]` when `big`, and the other way round when not,
    /// with `middle` put in front of the last load.
    fn read(big: bool, middle: &str) -> String {
        let [a, b, c, d] = if big { [24, 16, 8, 0] } else { [0, 8, 16, 24] };
        wrap(
            "(ptr) -> i32",
            &format!(
                "block0(%0: ptr):
    %1 = load.i8 %0, align 1
    %2 = iconst.i64 1
    %3 = ptr_add %0, %2
    %4 = load.i8 %3, align 1
    %5 = iconst.i64 2
    %6 = ptr_add %0, %5
    %7 = load.i8 %6, align 1
    %8 = iconst.i64 3
    %9 = ptr_add %0, %8
{middle}    %10 = load.i8 %9, align 1
    %11 = zext.i32 %1
    %12 = zext.i32 %4
    %13 = zext.i32 %7
    %14 = zext.i32 %10
    %15 = iconst.i32 {a}
    %16 = shl %11, %15
    %17 = iconst.i32 {b}
    %18 = shl %12, %17
    %19 = iconst.i32 {c}
    %20 = shl %13, %19
    %21 = iconst.i32 {d}
    %22 = shl %14, %21
    %23 = or %16, %18
    %24 = or %23, %20
    %25 = or %24, %22
    return %25
"
            ),
        )
    }

    #[test]
    fn a_big_endian_read_of_four_bytes_is_one_load_and_a_swap() {
        let out = run(&read(true, ""));
        assert_eq!(count(&out, " = load.i32 %0, align 1"), 1, "{out}");
        assert_eq!(count(&out, " = load.i8 "), 0, "{out}");
        assert_eq!(count(&out, " = bswap "), 1, "{out}");
    }

    #[test]
    fn a_little_endian_read_of_four_bytes_is_one_load() {
        let out = run(&read(false, ""));
        assert_eq!(count(&out, " = load.i32 %0, align 1"), 1, "{out}");
        assert_eq!(count(&out, " = load.i8 "), 0, "{out}");
        assert_eq!(count(&out, " = bswap "), 0, "{out}");
        assert_eq!(count(&out, " = or "), 0, "{out}");
    }

    #[test]
    fn a_store_between_the_loads_keeps_them() {
        let out = run(&read(true, "    store %2 -> %9, align 1\n"));
        assert_eq!(count(&out, " = load.i8 "), 4, "{out}");
        assert_eq!(count(&out, " = load.i32 "), 0, "{out}");
    }

    #[test]
    fn a_volatile_load_keeps_the_tree() {
        let text = read(true, "").replace("%10 = load.i8 %9", "%10 = load.i8.volatile %9");
        let out = run(&text);
        assert_eq!(count(&out, " = load.i8"), 4, "{out}");
        assert_eq!(count(&out, " = load.i32 "), 0, "{out}");
    }

    #[test]
    fn a_node_something_else_reads_keeps_the_tree() {
        let body = SWAP32.replace("return %13", "%14 = add %13, %11\n    return %14");
        let out = run(&wrap("(i32) -> i32", &body));
        assert_eq!(count(&out, " = bswap "), 0, "{out}");
    }

    #[test]
    fn a_mask_that_splits_a_byte_keeps_the_tree() {
        let text = wrap("(i32) -> i32", &SWAP32.replace("iconst.i32 65280", "iconst.i32 65281"));
        let out = run(&text);
        assert_eq!(count(&out, " = bswap "), 0, "{out}");
    }

    #[test]
    fn fuel_stops_the_rewriting() {
        let out = with(&wrap("(i32) -> i32", SWAP32), &mut Fuel::of(0));
        assert_eq!(count(&out, " = bswap "), 0, "{out}");
        let out = with(&wrap("(i32) -> i32", SWAP32), &mut Fuel::of(1));
        assert_eq!(count(&out, " = bswap "), 1, "{out}");
    }
}
