//! Arc counters for `-fprofile-arcs`, laid out the way gcc's libgcov and the kernel's gcov read them.
//!
//! Every function with a body gets an array of 64-bit counters and an increment on enough of its
//! edges that the count of every other edge can be worked out afterwards. The unit gets one
//! `gcov_info` record that lists the functions and their arrays, and a constructor that hands the
//! record to `__gcov_init`. Whatever provides that function owns the counts from then on: libgcov
//! writes them to the `.gcda` file named in the record when the program exits, and the kernel puts
//! them under debugfs.
//!
//! # Which edges are counted
//!
//! The ones off a spanning tree of the graph, which is gcc's rule from `gcc/profile.cc`. The graph
//! has two nodes past the blocks, one every call enters from and one every return goes to, and an
//! edge from the second back to the first, so that what flows in equals what flows out at every
//! node. An edge on the tree is then the sum of the others at either end of it and needs no
//! counter. The tree takes the edges that cannot hold an increment first, which are the ones into
//! the exit node from a block that never returns and the ones out of an `indirect_br` or an
//! `asm goto`, then the critical edges, which would each need a block of their own, then the rest.
//!
//! # Where the increment goes
//!
//! At the end of the source block when it has one way out, at the top of the destination when it
//! has one way in, and otherwise in a new block on the edge. A load, an add and a store of the
//! counter, not atomic, which is `-fprofile-update=single` and what gcc does without being asked.
//!
//! # Order
//!
//! Before the inliner, as in gcc. A body copied into a caller takes its increments along, and those
//! add to the counters of the function they were written in, which is where a coverage report wants
//! them.

use rucc_base::{Idx, Interner, Symbol};
use rucc_ir::{
    AttrSet, Block, BlockCall, Datum, Extra, Func, FuncId, Global, Imm, Inst, InstData, Linkage,
    MemInfo, MemOrder, Module, Opcode, Reloc, Restrict, Signature, Type, Value,
};

/// What the unit's record says and where its constructor goes, which the driver works out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Coverage {
    /// The `.gcda` file the counts are written to, as the record holds it.
    pub counts: String,
    /// The gcc version the unit claims to be built by, as `__GNUC__` and `__GNUC_MINOR__`. The
    /// record's layout and its version word follow it, because the runtime reading the record was
    /// built for the same version the headers were told about. The kernel's `gcc_4_7.c` picks its
    /// own copy of the layout by `__GNUC__`.
    pub gnuc: (u32, u32),
    /// The section the pointer to the constructor goes in, or `None` where the format has none.
    pub ctor: Option<String>,
    /// The section the pointer to the destructor goes in. Where this is `None` and the constructor
    /// has one, the constructor hands `__gcov_exit` to `atexit` instead.
    pub dtor: Option<String>,
}

impl Coverage {
    /// The version word in the record and at the top of the `.gcda` file.
    ///
    /// Four characters, from `gcc/gcov-iov.cc`: the major version in tens as a letter from `A`
    /// and in units as a digit, the minor version as a digit, and `*` for a release.
    #[must_use]
    pub fn version(&self) -> u32 {
        let (major, minor) = self.gnuc;
        let letters = [b'A' as u32 + major / 10, b'0' as u32 + major % 10, b'0' as u32 + minor];
        letters.iter().fold(0, |word, &letter| (word << 8) | (letter & 0xff)) << 8 | u32::from(b'*')
    }

    /// How many merge functions the record holds, one per kind of counter the version knows.
    ///
    /// gcc 14 added condition counters to the eight that 10 had. The ones before 10 had nine of a
    /// different set, which only matters for the length here.
    #[must_use]
    pub fn counters(&self) -> u64 {
        match self.gnuc.0 {
            14.. => 9,
            10..=13 => 8,
            _ => 9,
        }
    }

    /// Whether the record has the checksum gcc 12 put after the stamp.
    #[must_use]
    pub fn checksum(&self) -> bool {
        self.gnuc.0 >= 12
    }
}

/// One function that got counters: the name of its array, how many are in it, and the numbers
/// that say which build of the function the counts are for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Counted {
    /// The function.
    pub func: Symbol,
    /// Its array of counters.
    pub array: Symbol,
    /// How many counters the array holds.
    pub count: u32,
    /// Its number in the unit, from one.
    pub ident: u32,
    /// A hash of the function's name and where it is, which the `.gcno` file repeats.
    pub lineno_checksum: u32,
    /// A hash of the shape of its graph, which the `.gcno` file repeats too.
    pub cfg_checksum: u32,
}

/// Puts counters in every function this module defines and the record and constructor beside
/// them, and says which functions were counted.
///
/// A function marked `no_profile_instrument_function` gets none. Nothing is added to a module with
/// no function left to count, which is what gcc does too.
pub fn run(module: &mut Module, names: &mut Interner, coverage: &Coverage) -> Vec<Counted> {
    let mut counted = Vec::new();
    for id in module.funcs().collect::<Vec<FuncId>>() {
        let func = &module[id];
        if func.is_declaration()
            || func.attrs.set.contains(AttrSet::NO_PROFILE)
            || func.attrs.set.contains(AttrSet::NAKED)
        {
            continue;
        }
        let name = func.name;
        let spelled = names.resolve(name).to_owned();
        let array = names.intern(&format!("__gcov0.{spelled}"));
        if module.lookup(array).is_some() {
            continue;
        }
        let (count, cfg_checksum) = instrument(&mut module[id], array);
        let ident = u32::try_from(counted.len() + 1).unwrap_or(u32::MAX);
        let mut global = Global::new(array, 8 * u64::from(count), 8);
        global.linkage = Linkage::Internal;
        global.init = Some(module.push_data(&[Datum::Zero(8 * u64::from(count))]));
        module.add_global(global);
        let lineno_checksum = crc32(spelled.as_bytes(), 0);
        counted.push(Counted { func: name, array, count, ident, lineno_checksum, cfg_checksum });
    }
    if !counted.is_empty() {
        record(module, names, coverage, &counted);
    }
    counted
}

/// A node of the graph the tree is built over.
const ENTRY: usize = 0;
/// Where every return goes.
const EXIT: usize = 1;

/// What an edge is, which decides how early the tree takes it, earliest first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    /// From the entry node to the first block, which is where the tree starts.
    Enter,
    /// One that cannot hold an increment: out of an `indirect_br` or an `asm goto`, or into the
    /// exit node from a block that never gets there.
    Fixed,
    /// From a block that returns to the exit node.
    Exit,
    /// An ordinary branch out of a block with several ways out into one with several ways in,
    /// which is the edge that would need a block of its own.
    Critical,
    /// Any other branch.
    Plain,
}

/// One edge of the graph, which is every branch target from one block to another taken together.
struct Edge {
    /// Where it leaves from.
    from: usize,
    /// Where it goes.
    to: usize,
    /// What it is.
    kind: Kind,
    /// The places in the branch's table that go along it, empty for an edge into the exit node.
    calls: Vec<Idx<BlockCall>>,
}

/// Counts the edges of one function, and says how many counters it took and a hash of its graph.
fn instrument(func: &mut Func, array: Symbol) -> (u32, u32) {
    // A node for each block, numbered in layout order from two, after the entry and exit nodes.
    let blocks: Vec<Block> = func.blocks().collect();
    let mut node = vec![usize::MAX; blocks.iter().map(|b| b.index() + 1).max().unwrap_or(0)];
    for (n, block) in blocks.iter().enumerate() {
        node[block.index()] = n + 2;
    }
    let mut edges = vec![Edge { from: ENTRY, to: 2, kind: Kind::Enter, calls: Vec::new() }];
    for &block in &blocks {
        let Some(term) = func.terminator(block) else { continue };
        let from = node[block.index()];
        let mine = edges.len();
        match func[term].opcode {
            Opcode::Return | Opcode::TailCall => {
                edges.push(Edge { from, to: EXIT, kind: Kind::Exit, calls: Vec::new() });
            }
            Opcode::Unreachable => {
                edges.push(Edge { from, to: EXIT, kind: Kind::Fixed, calls: Vec::new() });
            }
            opcode => {
                let fixed =
                    opcode == Opcode::IndirectBr || matches!(func[term].extra, Extra::Asm(_));
                let kind = if fixed { Kind::Fixed } else { Kind::Plain };
                let list = func.target_list(term);
                let first = list.as_usize_range().start;
                for (k, call) in func[list].iter().enumerate() {
                    let at = Idx::from_usize(first + k);
                    let to = node[call.block.index()];
                    match edges[mine..].iter_mut().find(|edge| edge.to == to) {
                        Some(edge) => edge.calls.push(at),
                        None => edges.push(Edge { from, to, kind, calls: vec![at] }),
                    }
                }
            }
        }
    }
    let mut outs = vec![0_usize; blocks.len() + 2];
    let mut ins = vec![0_usize; blocks.len() + 2];
    for edge in &edges {
        outs[edge.from] += 1;
        ins[edge.to] += 1;
    }
    for edge in &mut edges {
        if edge.kind == Kind::Plain && outs[edge.from] > 1 && ins[edge.to] > 1 {
            edge.kind = Kind::Critical;
        }
    }
    let mut order: Vec<usize> = (0..edges.len()).collect();
    order.sort_by_key(|&i| edges[i].kind);
    let mut sets: Vec<usize> = (0..blocks.len() + 2).collect();
    unite(&mut sets, EXIT, ENTRY);
    let mut tree = vec![false; edges.len()];
    for i in order {
        tree[i] = unite(&mut sets, edges[i].from, edges[i].to);
    }
    let mut checksum = 0;
    for edge in &edges {
        for end in [edge.from, edge.to] {
            checksum = crc32(&u32::try_from(end).unwrap_or(u32::MAX).to_le_bytes(), checksum);
        }
    }
    let mut count = 0_u32;
    for (i, edge) in edges.iter().enumerate() {
        // The edge out of the entry node is always on the tree, being the first one it takes
        // that reaches past the two nodes it starts with.
        if tree[i] || edge.from < 2 {
            continue;
        }
        let slot = u64::from(count);
        count += 1;
        let source = blocks[edge.from - 2];
        let term =
            func.terminator(source).expect("a block with an edge out of it ends in a branch");
        if edge.to == EXIT || outs[edge.from] == 1 {
            bump(func, array, slot, term);
        } else if ins[edge.to] == 1 || edge.kind == Kind::Fixed {
            // An edge out of an `indirect_br` has nowhere else to go, since the branch reaches its
            // blocks by address. Counting it at the top of a block with other ways in counts those
            // too, which is the best there is and the case the tree avoids by taking it first.
            let first = func.insts(blocks[edge.to - 2]).next().expect("a block holds its branch");
            bump(func, array, slot, first);
        } else {
            for &at in &edge.calls {
                split(func, array, slot, term, at);
            }
        }
    }
    (count, checksum)
}

/// Joins the sets two nodes are in, and says whether they were apart.
fn unite(sets: &mut [usize], a: usize, b: usize) -> bool {
    let (a, b) = (find(sets, a), find(sets, b));
    sets[a] = b;
    a != b
}

/// The node standing for the set this one is in.
fn find(sets: &mut [usize], mut n: usize) -> usize {
    while sets[n] != n {
        sets[n] = sets[sets[n]];
        n = sets[n];
    }
    n
}

/// Puts an edge's increment in a block of its own, between the branch and where it went.
fn split(func: &mut Func, array: Symbol, slot: u64, term: Inst, at: Idx<BlockCall>) {
    let list = func.target_list(term);
    let call = func[list][at.index() - list.as_usize_range().start];
    let args: Vec<Value> = func[call.args].to_vec();
    let span = func.span(term);
    let between = func.create_block();
    let jump = rucc_ir::Builder::new(func, between).at(span).jump(call.block, &args);
    bump(func, array, slot, jump);
    let args = func.push_values(&[]);
    func.set_block_call(at, BlockCall { block: between, args, hint: call.hint });
}

/// Adds one to a counter, just before an instruction.
fn bump(func: &mut Func, array: Symbol, slot: u64, before: Inst) {
    let span = func.span(before);
    let place = |func: &mut Func, data: InstData, ty: Option<Type>| -> Option<Value> {
        let results: &[Type] = match &ty {
            Some(ty) => std::slice::from_ref(ty),
            None => &[],
        };
        let inst = func.create_inst(data, results, span);
        func.insert_before(inst, before);
        func[inst].results().next()
    };
    let base = InstData { extra: Extra::Symbol(array), ..InstData::new(Opcode::GlobalAddr) };
    let mut addr = place(func, base, Some(Type::PTR)).expect("an address");
    if slot > 0 {
        let at = func.add_imm(Imm::int(i128::from(8 * slot), Type::int(64)));
        let by = InstData { extra: Extra::Imm(at), ..InstData::new(Opcode::IConst) };
        let by = place(func, by, Some(Type::int(64))).expect("a constant");
        let args = func.push_values(&[addr, by]);
        let moved = InstData { args, ..InstData::new(Opcode::PtrAdd) };
        addr = place(func, moved, Some(Type::PTR)).expect("an address");
    }
    let access = MemInfo {
        size: 8,
        align: 8,
        order: MemOrder::NotAtomic,
        tbaa: None,
        owns: 0,
        restrict: Restrict::NONE,
    };
    let mem = func.add_mem(access);
    let args = func.push_values(&[addr]);
    let read = InstData { args, extra: Extra::Mem(mem), ..InstData::new(Opcode::Load) };
    let old = place(func, read, Some(Type::int(64))).expect("a value");
    let one = func.add_imm(Imm::int(1, Type::int(64)));
    let one = InstData { extra: Extra::Imm(one), ..InstData::new(Opcode::IConst) };
    let one = place(func, one, Some(Type::int(64))).expect("a constant");
    let args = func.push_values(&[old, one]);
    let sum = place(func, InstData { args, ..InstData::new(Opcode::Add) }, Some(Type::int(64)));
    let mem = func.add_mem(access);
    let args = func.push_values(&[sum.expect("a value"), addr]);
    let write = InstData { args, extra: Extra::Mem(mem), ..InstData::new(Opcode::Store) };
    place(func, write, None);
}

/// The bytes of a record as they are put together, a field at a time at the alignment of each.
struct Image {
    /// What is written so far.
    data: Vec<Datum>,
    /// How many bytes that is.
    size: u64,
    /// How wide an address is, in bytes.
    pointer: u64,
}

impl Image {
    /// An empty record for addresses that wide.
    fn new(pointer: u64) -> Self {
        Self { data: Vec::new(), size: 0, pointer }
    }

    /// Pads up to a multiple of `align`.
    fn align(&mut self, align: u64) {
        let pad = self.size.next_multiple_of(align) - self.size;
        if pad > 0 {
            self.data.push(Datum::Zero(pad));
            self.size += pad;
        }
    }

    /// A 32-bit number.
    fn word(&mut self, module: &mut Module, value: u32) {
        self.align(4);
        let value = module.add_imm(Imm::int(i128::from(value), Type::int(32)));
        self.data.push(Datum::Scalar { ty: Type::int(32), value });
        self.size += 4;
    }

    /// The address of a symbol, or null for `None`.
    fn address(&mut self, module: &mut Module, symbol: Option<Symbol>) {
        self.align(self.pointer);
        self.data.push(match symbol {
            Some(symbol) => {
                let size = u32::try_from(self.pointer).unwrap_or(8);
                Datum::Addr(module.add_reloc(Reloc { symbol, addend: 0, size }))
            }
            None => Datum::Zero(self.pointer),
        });
        self.size += self.pointer;
    }

    /// Puts the record in the module as an internal object of that name.
    fn place(mut self, module: &mut Module, name: Symbol, constant: bool) {
        self.align(self.pointer);
        let align = u32::try_from(self.pointer).unwrap_or(8);
        let mut global = Global::new(name, self.size, align);
        global.linkage = Linkage::Internal;
        global.constant = constant;
        global.init = Some(module.push_data(&self.data));
        module.add_global(global);
    }
}

/// The unit's `gcov_info`, a `gcov_fn_info` for each function, and what registers them.
///
/// The layout is `struct gcov_info` from libgcov and from the kernel's `gcc_4_7.c`: the version,
/// a link the runtime fills in, the stamp, the checksum from gcc 12 on, the name of the `.gcda`
/// file, a merge function for each kind of counter of which only the arcs have one, and the list
/// of functions. A function's entry points back at the record, gives its number and checksums,
/// and then the one `gcov_ctr_info` there is, which is the count and the array.
fn record(module: &mut Module, names: &mut Interner, coverage: &Coverage, counted: &[Counted]) {
    let pointer = u64::from(module.datalayout.pointer_bits / 8);
    let info = names.intern("__gcov_.LPBX0");
    let mut entries = Vec::new();
    for function in counted {
        let spelled = names.resolve(function.func).to_owned();
        let name = names.intern(&format!("__gcov_.{spelled}"));
        let mut image = Image::new(pointer);
        image.address(module, Some(info));
        image.word(module, function.ident);
        image.word(module, function.lineno_checksum);
        image.word(module, function.cfg_checksum);
        // The one `gcov_ctr_info`, which holds an address and so starts at the alignment of one.
        image.align(pointer);
        image.word(module, function.count);
        image.address(module, Some(function.array));
        image.place(module, name, false);
        entries.push(name);
    }
    let list = names.intern("__gcov_.LPBX1");
    let mut image = Image::new(pointer);
    for &entry in &entries {
        image.address(module, Some(entry));
    }
    image.place(module, list, true);
    let file = names.intern("__gcov_.LPBX2");
    let mut path = coverage.counts.clone().into_bytes();
    path.push(0);
    let bytes = module.push_bytes(&path);
    let mut global = Global::new(file, path.len() as u64, 1);
    global.linkage = Linkage::Internal;
    global.constant = true;
    global.init = Some(module.push_data(&[Datum::Bytes(bytes)]));
    module.add_global(global);
    let stamp = crc32(coverage.counts.as_bytes(), 0);
    let merge = declare(
        module,
        names,
        "__gcov_merge_add",
        Signature::new().with_params(&[Type::PTR, Type::int(32)]),
    );
    let mut image = Image::new(pointer);
    image.word(module, coverage.version());
    image.address(module, None);
    image.word(module, stamp);
    if coverage.checksum() {
        image.word(module, stamp);
    }
    image.address(module, Some(file));
    image.address(module, Some(merge));
    for _ in 1..coverage.counters() {
        image.address(module, None);
    }
    image.word(module, u32::try_from(entries.len()).unwrap_or(u32::MAX));
    image.address(module, Some(list));
    image.place(module, info, false);
    let Some(section) = &coverage.ctor else { return };
    let takes = Signature::new().with_params(&[Type::PTR]);
    let init = declare(module, names, "__gcov_init", takes.clone());
    let exit = declare(module, names, "__gcov_exit", Signature::new());
    let ctor = names.intern("_sub_I_00101_0");
    let mut func = Func::new(ctor, Signature::new());
    func.linkage = Linkage::Internal;
    func.attrs.set |= AttrSet::NO_PROFILE;
    let entry = func.create_block();
    let sig = func.add_signature(takes.clone());
    let mut build = rucc_ir::Builder::new(&mut func, entry);
    let what = build.value(address(info), Type::PTR);
    build.call(init, sig, &[what]);
    // A format with nowhere to put a destructor still writes the counts out at the end, since the
    // C library's `atexit` is there wherever its constructors are.
    if coverage.dtor.is_none() {
        let registers = takes.with_returns(&[Type::int(32)]);
        let atexit = declare(module, names, "atexit", registers.clone());
        let sig = build.func().add_signature(registers);
        let what = build.value(address(exit), Type::PTR);
        build.call(atexit, sig, &[what]);
    }
    build.ret(&[]);
    module.add_func(func);
    entry_in(module, names, ctor, section, pointer);
    let Some(section) = &coverage.dtor else { return };
    let dtor = names.intern("_sub_D_00101_1");
    let mut func = Func::new(dtor, Signature::new());
    func.linkage = Linkage::Internal;
    func.attrs.set |= AttrSet::NO_PROFILE;
    let entry = func.create_block();
    let sig = func.add_signature(Signature::new());
    let mut build = rucc_ir::Builder::new(&mut func, entry);
    build.call(exit, sig, &[]);
    build.ret(&[]);
    module.add_func(func);
    entry_in(module, names, dtor, section, pointer);
}

/// What takes the address of a name.
fn address(name: Symbol) -> InstData {
    InstData { extra: Extra::Symbol(name), ..InstData::new(Opcode::GlobalAddr) }
}

/// A function of the runtime's, declared unless the unit says something about it already.
fn declare(module: &mut Module, names: &mut Interner, name: &str, signature: Signature) -> Symbol {
    let name = names.intern(name);
    if module.lookup(name).is_none() {
        module.add_func(Func::new(name, signature));
    }
    name
}

/// The pointer to a constructor or destructor in the section the runtime walks.
fn entry_in(module: &mut Module, names: &mut Interner, func: Symbol, section: &str, pointer: u64) {
    let spelled = names.resolve(func).to_owned();
    let name = names.intern(&format!("__rucc_gcov.{spelled}"));
    let mut image = Image::new(pointer);
    image.address(module, Some(func));
    let section = names.intern(section);
    let align = u32::try_from(pointer).unwrap_or(8);
    let mut global = Global::new(name, image.size, align);
    global.linkage = Linkage::Internal;
    global.section = Some(section);
    global.init = Some(module.push_data(&image.data));
    module.add_global(global);
}

/// The CRC-32 of some bytes, carried on from an earlier one, which is gcc's `crc32_string`.
fn crc32(bytes: &[u8], mut crc: u32) -> u32 {
    for &byte in bytes {
        crc ^= u32::from(byte) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 { (crc << 1) ^ 0x04c1_1db7 } else { crc << 1 };
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claiming(major: u32, minor: u32) -> Coverage {
        Coverage { counts: "a.gcda".to_owned(), gnuc: (major, minor), ctor: None, dtor: None }
    }

    #[test]
    fn the_version_word_is_the_one_gcc_writes() {
        // `B32*` and `B60*`, which is what gcc 13.2 and 16.0 put at the top of their `.gcda` files.
        assert_eq!(claiming(13, 2).version().to_be_bytes(), *b"B32*");
        assert_eq!(claiming(16, 0).version().to_be_bytes(), *b"B60*");
        assert_eq!(claiming(9, 4).version().to_be_bytes(), *b"A94*");
    }

    #[test]
    fn the_record_has_as_many_merge_slots_as_the_kernel_s_copy_of_it() {
        // The same ladder as `GCOV_COUNTERS` in the kernel's `gcc_4_7.c`.
        assert_eq!(claiming(16, 0).counters(), 9);
        assert_eq!(claiming(14, 1).counters(), 9);
        assert_eq!(claiming(13, 2).counters(), 8);
        assert_eq!(claiming(10, 1).counters(), 8);
        assert_eq!(claiming(9, 4).counters(), 9);
        assert!(claiming(12, 1).checksum());
        assert!(!claiming(11, 4).checksum());
    }

    #[test]
    fn the_checksum_is_gcc_s_crc() {
        // gcc's `crc32_string` is the most significant bit first CRC that MPEG-2 also uses, and
        // this is that one's check value.
        assert_eq!(crc32(b"123456789", 0xffff_ffff), 0x0376_e6e7);
    }
}
