//! The shadow checks `-fsanitize=kernel-address` asks for, which the kernel's `CONFIG_KASAN_GENERIC`
//! puts on every unit that does not opt out.
//!
//! Design: `spec/04-driver-and-cli.md` section 4.7.
//!
//! The kernel keeps a shadow byte for every eight bytes of memory, at the address shifted right by
//! three plus the offset `-fasan-shadow-offset=` gives. Zero says the eight bytes may be used, a
//! number from one to seven says that many bytes at the start of them may, and a negative number
//! says none of them may. What is written here is the check in front of each access and the call
//! that says it failed. The runtime is the kernel's own, in `mm/kasan/`, so the calls carry the
//! names and the arguments gcc gives them.
//!
//! # The check
//!
//! An access of one byte, or of two or four that is aligned to its size, reads the shadow byte of
//! its first byte. When that byte is not zero and the offset of the last byte in its group of
//! eight is not below it, the access is bad and `__asan_report_load4_noabort` or the call of that
//! size is made with the address. An access of eight bytes that is aligned to eight is bad when
//! its shadow byte is not zero, and one of sixteen aligned to eight when its two shadow bytes are
//! not. Any other access of a known size reads the shadow of its first byte and of its last byte,
//! checks each as an access of one byte, and calls `__asan_report_load_n_noabort` with the address
//! and the size when either is bad. One whose size the program works out calls
//! `__asan_loadN_noabort` and the runtime works it out. These are gcc's rules.
//!
//! A function with as many accesses as `--param asan-instrumentation-with-call-threshold=` says
//! makes a call in front of every one instead, `__asan_load4_noabort` and the rest, which is how
//! `CONFIG_KASAN_OUTLINE` asks for it with a threshold of zero. There an access that is not one of
//! the aligned ones calls `__asan_loadN_noabort`.
//!
//! An atomic access is a call to the runtime whatever the threshold says, `__asan_load4_noabort`
//! for a load and `__asan_store4_noabort` for every other kind, which is what gcc gives the
//! `__atomic` and `__sync` builtins.
//!
//! A copy or a fill the compiler makes is checked as a read of the bytes it reads and a write of
//! the bytes it writes. One whose length the program works out is a call to `__asan_memcpy`,
//! `__asan_memmove` or `__asan_memset` under `--param asan-kernel-mem-intrinsic-prefix=1`, and so
//! is a call to `memcpy`, `memmove` or `memset` itself. Those check what they touch.
//!
//! # What is left alone
//!
//! An access to a local or to a named object at a constant offset that is inside it, since the
//! check could not fail. An access through `__seg_fs` or `__seg_gs`, whose address is not a place
//! the shadow covers. A function marked `no_sanitize_address` or `naked`. The memory of `alloca`
//! has no redzones around it yet.
//!
//! # The frame
//!
//! Under `--param asan-stack=1` the locals a check can find wrong go into one frame with a redzone
//! after each of them, laid out as gcc lays it out, and the shadow of the redzones is poisoned on
//! the way in and cleared on the way out.
//!
//! # The globals
//!
//! Under `--param asan-globals=1` each global that gcc protects is aligned to 32 bytes and has a
//! redzone of zero bytes after it, which the symbol's size leaves out. A table of eight words for
//! each of them goes to `__asan_register_globals` from a constructor, and the kernel poisons the
//! shadow of each redzone when it reads the table.
//!
//! # Order
//!
//! After the optimizer, which is where gcc's pass is. A value the optimizer keeps in a register is
//! not an access any more, and a body inlined into a function marked `no_sanitize_address` gets no
//! checks, which is what the kernel's `noinstr` code needs.

use rucc_base::hash::{Map, Set};
use rucc_base::{Interner, Symbol};
use rucc_ir::{
    AttrSet, Builder, CallInfo, Datum, Extra, Flags, Func, FuncId, Global, GlobalId, Imm, Inst,
    InstData, IntPred, Linkage, MemInfo, MemOrder, Module, Opcode, Reloc, Restrict, Signature,
    Type, Value,
};
use rucc_session::Kasan;

use crate::alias::{Escapes, Origin, origin};

/// Puts the checks in every function of the module that has a body and was not marked
/// `no_sanitize_address` or `naked`, and says how many it put in.
pub fn run(module: &mut Module, names: &mut Interner, how: Kasan) -> usize {
    if !how.on {
        return 0;
    }
    let sizes: Map<Symbol, u64> =
        module.globals().map(|id| (module[id].name, module[id].size)).collect();
    let mut runtime = Runtime { names, declared: Vec::new(), frames: Vec::new() };
    let mut checks = 0;
    for id in module.funcs().collect::<Vec<FuncId>>() {
        let func = &module[id];
        if func.is_declaration()
            || func.attrs.set.contains(AttrSet::NO_ASAN)
            || func.attrs.set.contains(AttrSet::NAKED)
        {
            continue;
        }
        checks += instrument(&mut module[id], &mut runtime, &sizes, how);
    }
    if how.globals {
        globals(module, &mut runtime);
    }
    for (name, signature) in runtime.declared {
        if module.lookup(name).is_none() {
            module.add_func(Func::new(name, signature));
        }
    }
    for (name, text) in runtime.frames {
        let mut bytes = text.into_bytes();
        bytes.push(0);
        let mut global = Global::new(name, bytes.len() as u64, 1);
        global.linkage = Linkage::Internal;
        global.constant = true;
        let range = module.push_bytes(&bytes);
        global.init = Some(module.push_data(&[Datum::Bytes(range)]));
        module.add_global(global);
    }
    checks
}

/// The alignment past which a global gets no redzone, which is gcc's two redzones.
const MAX_ALIGN: u32 = 64;

/// What the start of a protected global is aligned to, and what its redzone is made a multiple
/// of. gcc's `ASAN_RED_ZONE_SIZE`.
const GLOBAL_ZONE: u64 = 32;

/// How many words the runtime reads for each global: where it is, its size, its size with the
/// redzone, its name, the name of the file, whether it has a dynamic initializer, where it is
/// in the source, and the address of its ODR indicator. `struct kasan_global` in the kernel.
const DESCRIBED: usize = 8;

/// Whether a global gets a redzone, which is gcc's `asan_protect_global`.
///
/// A definition that is not thread-local and that the program put in no section of its own,
/// with an alignment no larger than two redzones. One of no size is one too, which is what the
/// kernel's `struct lock_class_key` is without lockdep, and gcc gives each of them a redzone. One the linker can replace with
/// another definition is left alone, since the table would then describe the other one with the
/// size of this one, and so is a tentative one.
fn protected(global: &Global) -> bool {
    !global.is_declaration()
        && global.tls.is_none()
        && global.section.is_none()
        && matches!(global.linkage, Linkage::External | Linkage::Internal)
        && !global.noinit
        && !global.persistent
        && global.align <= MAX_ALIGN
}

/// The redzone after a global of that size, which takes it to a multiple of 32 bytes with at
/// least 32 bytes after it, as gcc's `asan_red_zone_size` does.
fn global_redzone(size: u64) -> u64 {
    let over = size % GLOBAL_ZONE;
    if over == 0 { GLOBAL_ZONE } else { 2 * GLOBAL_ZONE - over }
}

/// Puts a redzone after each global that gets one and the table the runtime reads them from,
/// and adds the constructor that gives the table to `__asan_register_globals` and the destructor
/// that takes it back with `__asan_unregister_globals`.
///
/// A string literal that gets one is not a literal any more, because a section the linker merges
/// strings in would merge the redzone away. The location word is zero, which the kernel does not
/// read.
fn globals(module: &mut Module, runtime: &mut Runtime<'_>) {
    let ids: Vec<GlobalId> = module.globals().filter(|&id| protected(&module[id])).collect();
    if ids.is_empty() {
        return;
    }
    let file = runtime.names.resolve(module.name).to_owned();
    let file = string(module, runtime.names.intern("__asan_gen_module"), &file);
    let mut table = Vec::with_capacity(ids.len() * DESCRIBED);
    for (index, &id) in ids.iter().enumerate() {
        let global = &mut module[id];
        global.redzone = global_redzone(global.size);
        global.align = global.align.max(GLOBAL_ZONE as u32);
        let (name, size, redzone) = (global.name, global.size, global.redzone);
        let shown = if global.literal {
            format!("*{}", runtime.names.resolve(name))
        } else {
            runtime.names.resolve(name).to_owned()
        };
        global.literal = false;
        let shown =
            string(module, runtime.names.intern(&format!("__asan_gen_name.{index}")), &shown);
        table.push(address(module, name));
        table.push(word(module, size));
        table.push(word(module, size + redzone));
        table.push(address(module, shown));
        table.push(address(module, file));
        table.extend([Datum::Zero(8), Datum::Zero(8), Datum::Zero(8)]);
    }
    let name = runtime.names.intern("__asan_gen_globals");
    let mut global = Global::new(name, (table.len() * 8) as u64, GLOBAL_ZONE as u32);
    global.linkage = Linkage::Internal;
    global.init = Some(module.push_data(&table));
    module.add_global(global);
    let count = ids.len() as i128;
    for (before, which, called) in [
        (true, "_sub_I_65535_1", "__asan_register_globals"),
        (false, "_sub_D_65535_0", "__asan_unregister_globals"),
    ] {
        let (callee, signature) = runtime.callee(called, &[Type::PTR, Type::int(64)], &[]);
        let func_name = runtime.names.intern(which);
        let mut func = Func::new(func_name, Signature::new());
        func.linkage = Linkage::Internal;
        func.attrs.set |= AttrSet::NO_ASAN;
        let block = func.create_block();
        let mut build = Builder::new(&mut func, block);
        let signature = build.func().add_signature(signature);
        let data = InstData { extra: Extra::Symbol(name), ..InstData::new(Opcode::GlobalAddr) };
        let at = build.value(data, Type::PTR);
        let count = build.iconst(Type::int(64), count);
        build.call(callee, signature, &[at, count]);
        build.ret(&[]);
        module.add_func(func);
        let section = if before { ".init_array" } else { ".fini_array" };
        let kind = if before { "ctor" } else { "dtor" };
        let pointer = runtime.names.intern(&format!("__rucc_{kind}.{which}"));
        let mut global = Global::new(pointer, 8, 8);
        global.linkage = Linkage::Internal;
        global.section = Some(runtime.names.intern(section));
        let entry = address(module, func_name);
        global.init = Some(module.push_data(&[entry]));
        module.add_global(global);
    }
}

/// A string the table points at, with its zero, which gets no redzone of its own.
fn string(module: &mut Module, name: Symbol, text: &str) -> Symbol {
    let mut bytes = text.as_bytes().to_vec();
    bytes.push(0);
    let mut global = Global::new(name, bytes.len() as u64, 1);
    global.linkage = Linkage::Internal;
    global.constant = true;
    let range = module.push_bytes(&bytes);
    global.init = Some(module.push_data(&[Datum::Bytes(range)]));
    module.add_global(global);
    name
}

/// A word of the table that holds the address of `symbol`.
fn address(module: &mut Module, symbol: Symbol) -> Datum {
    Datum::Addr(module.add_reloc(Reloc { symbol, addend: 0, size: 8 }))
}

/// A word of the table that holds a number.
fn word(module: &mut Module, value: u64) -> Datum {
    Datum::Scalar {
        ty: Type::int(64),
        value: module.add_imm(Imm::int(i128::from(value), Type::int(64))),
    }
}

/// The runtime functions called so far, declared once the walk is over.
struct Runtime<'a> {
    names: &'a mut Interner,
    declared: Vec<(Symbol, Signature)>,
    /// The frame records written so far, by the name of the string and its text.
    frames: Vec<(Symbol, String)>,
}

impl Runtime<'_> {
    /// The function of that name, which takes `params` and returns `returns`.
    fn callee(&mut self, name: &str, params: &[Type], returns: &[Type]) -> (Symbol, Signature) {
        let symbol = self.names.intern(name);
        let signature = Signature::new().with_params(params).with_returns(returns);
        if !self.declared.iter().any(|(declared, _)| *declared == symbol) {
            self.declared.push((symbol, signature.clone()));
        }
        (symbol, signature)
    }
}

/// One access the walk checks.
#[derive(Clone, Copy)]
struct Access {
    /// The instruction the check goes in front of.
    before: Inst,
    address: Value,
    /// How many bytes, or the value that says so.
    size: Size,
    align: u32,
    write: bool,
    /// Whether it is an atomic access, which gcc checks with a call to the runtime whatever the
    /// threshold says.
    atomic: bool,
}

#[derive(Clone, Copy)]
enum Size {
    Bytes(u64),
    Value(Value),
}

/// Checks the accesses of one function, and says how many.
fn instrument(
    func: &mut Func,
    runtime: &mut Runtime<'_>,
    sizes: &Map<Symbol, u64>,
    how: Kasan,
) -> usize {
    let mut accesses = Vec::new();
    let mut routines = Vec::new();
    let insts: Vec<Inst> = func.blocks().flat_map(|block| func.insts(block)).collect();
    for inst in insts {
        let data = func[inst];
        if data.flags.contains(Flags::SEG_FS) || data.flags.contains(Flags::SEG_GS) {
            continue;
        }
        match data.opcode {
            Opcode::Load => {
                let Some(result) = data.first_result else { continue };
                let Extra::Mem(mem) = data.extra else { continue };
                let address = func[data.args][0];
                let size = bytes(func[result].ty);
                let align = func[mem].align;
                accesses.push(Access {
                    before: inst,
                    address,
                    size: Size::Bytes(size),
                    align,
                    write: false,
                    atomic: false,
                });
            }
            Opcode::Store => {
                let Extra::Mem(mem) = data.extra else { continue };
                let &[value, address, ..] = &func[data.args] else { continue };
                let size = bytes(func[value].ty);
                let align = func[mem].align;
                accesses.push(Access {
                    before: inst,
                    address,
                    size: Size::Bytes(size),
                    align,
                    write: true,
                    atomic: false,
                });
            }
            // An atomic load is checked as a read and every other atomic as a write, a
            // compare and exchange too, as gcc checks the builtins. The value a compare and
            // exchange expects is not an access here, since the front end has read it already.
            Opcode::AtomicLoad | Opcode::AtomicStore | Opcode::AtomicRmw | Opcode::Cmpxchg => {
                let (Extra::Mem(mem) | Extra::Rmw(_, mem)) = data.extra else { continue };
                let args = &func[data.args];
                let (address, ty) = match data.opcode {
                    Opcode::AtomicLoad => {
                        let Some(result) = data.first_result else { continue };
                        (args[0], func[result].ty)
                    }
                    Opcode::AtomicStore => (args[1], func[args[0]].ty),
                    _ => (args[0], func[args[1]].ty),
                };
                accesses.push(Access {
                    before: inst,
                    address,
                    size: Size::Bytes(bytes(ty)),
                    align: func[mem].align,
                    write: data.opcode != Opcode::AtomicLoad,
                    atomic: true,
                });
            }
            Opcode::Memcpy | Opcode::Memmove | Opcode::Memset => {
                let Some(bulk) = func.bulk(inst) else { continue };
                let Extra::Mem(mem) = data.extra else { continue };
                let size = match bulk.length {
                    Some(_) if how.mem_prefix => {
                        routines.push(inst);
                        continue;
                    }
                    Some(length) => Size::Value(length),
                    None => Size::Bytes(func[mem].size),
                };
                // The alignment of a copy is known for both of its ends, so a copy of a 16 byte
                // `struct path` aligned to eight gets the check of an aligned 16 byte access, as
                // it does from gcc.
                let align = func[mem].align;
                if data.opcode != Opcode::Memset {
                    accesses.push(Access {
                        before: inst,
                        address: bulk.with,
                        size,
                        align,
                        write: false,
                        atomic: false,
                    });
                }
                let to = bulk.to;
                accesses.push(Access {
                    before: inst,
                    address: to,
                    size,
                    align,
                    write: true,
                    atomic: false,
                });
            }
            Opcode::Call if how.mem_prefix => {
                let Extra::Call(info) = data.extra else { continue };
                let Some(callee) = func[info].callee else { continue };
                if matches!(runtime.names.resolve(callee), "memcpy" | "memmove" | "memset") {
                    routines.push(inst);
                }
            }
            _ => {}
        }
    }
    accesses.retain(|access| !inside(func, sizes, access));
    // The locals the checks can find wrong, which are the ones an access reaches at an offset
    // that is not known to be inside, and the ones whose address leaves the function.
    let mut guarded = Set::default();
    if how.stack {
        let escapes = Escapes::of(func);
        for inst in func.blocks().flat_map(|block| func.insts(block)) {
            if func[inst].opcode == Opcode::Alloca && escapes.escaped(inst) {
                guarded.insert(inst);
            }
        }
        for access in &accesses {
            if let (Origin::Local(slot), _) = origin(func, access.address) {
                guarded.insert(slot);
            }
        }
    }
    let outline = accesses.len() as u64 >= u64::from(how.call_threshold);
    let suffix = if how.abort { "" } else { "_noabort" };
    for &access in &accesses {
        check(func, runtime, access, outline, suffix, how.offset);
    }
    for &inst in &routines {
        prefixed(func, runtime, inst);
    }
    let framed = frame(func, runtime, &guarded, how.offset);
    accesses.len() + routines.len() + framed
}

/// How many bytes a value of that type takes in memory, on a 64-bit target.
fn bytes(ty: Type) -> u64 {
    if ty.is_ptr() {
        return 8;
    }
    (u64::from(ty.bits()) * u64::from(ty.lanes())).div_ceil(8)
}

/// Whether the access is to a local or a named object at a constant offset inside it, which no
/// check can find wrong.
fn inside(func: &Func, sizes: &Map<Symbol, u64>, access: &Access) -> bool {
    let Size::Bytes(size) = access.size else { return false };
    let (from, Some(offset)) = origin(func, access.address) else { return false };
    let room = match from {
        Origin::Local(slot) => match func[slot].extra {
            Extra::Mem(mem) => func[mem].size,
            _ => 0,
        },
        Origin::Global(name) => sizes.get(&name).copied().unwrap_or(0),
        Origin::Unknown(_) => 0,
    };
    u64::try_from(offset)
        .is_ok_and(|offset| offset.checked_add(size).is_some_and(|end| end <= room))
}

/// The bytes one shadow byte says are good or bad, which is the shift of three in the shadow
/// address.
const GROUP: u64 = 8;

/// The check in front of one access.
fn check(
    func: &mut Func,
    runtime: &mut Runtime<'_>,
    access: Access,
    outline: bool,
    suffix: &str,
    offset: u64,
) {
    let kind = if access.write { "store" } else { "load" };
    let outline = outline || access.atomic;
    // gcc's scalar access: a size it has a routine for, aligned to that size. A 16 byte access
    // needs only eight, since the two shadow bytes are read as one on the targets this has.
    let scalar = match access.size {
        Size::Bytes(1) => true,
        Size::Bytes(size @ (2 | 4 | 8)) => u64::from(access.align) >= size,
        Size::Bytes(16) => u64::from(access.align) >= GROUP,
        _ => false,
    };
    match access.size {
        Size::Bytes(size) if scalar && !outline => {
            let name = format!("__asan_report_{kind}{size}{suffix}");
            let (callee, signature) = runtime.callee(&name, &[Type::PTR], &[]);
            shadow(func, access, size, offset, callee, signature);
        }
        Size::Bytes(size) if scalar => {
            let name = format!("__asan_{kind}{size}{suffix}");
            let (callee, signature) = runtime.callee(&name, &[Type::PTR], &[]);
            call(func, callee, signature, &[access.address], access.before);
        }
        Size::Bytes(size) if !outline => {
            let name = format!("__asan_report_{kind}_n{suffix}");
            let word = Type::int(64);
            let (callee, signature) = runtime.callee(&name, &[Type::PTR, word], &[]);
            ends(func, access, size, offset, callee, signature);
        }
        size => {
            let name = format!("__asan_{kind}N{suffix}");
            let word = Type::int(64);
            let (callee, signature) = runtime.callee(&name, &[Type::PTR, word], &[]);
            let size = match size {
                Size::Bytes(size) => constant(func, word, i128::from(size), access.before),
                Size::Value(value) => widen(func, value, word, access.before),
            };
            call(func, callee, signature, &[access.address, size], access.before);
        }
    }
}

/// The inline check: the shadow read, the comparisons, a branch to the call that reports and a
/// branch back.
fn shadow(
    func: &mut Func,
    access: Access,
    size: u64,
    offset: u64,
    report: Symbol,
    signature: Signature,
) {
    let word = Type::int(64);
    let before = access.before;
    let span = func.span(before);
    let args = func.push_values(&[access.address]);
    let first =
        func.create_inst(InstData { args, ..InstData::new(Opcode::PtrToInt) }, &[word], span);
    func.insert_before(first, before);
    let address = func[first].first_result.expect("a value");
    let block = func.block_of(first).expect("the access is in a block");
    let after = func.create_block();
    func.move_after(first, after);
    let reports = func.create_block();
    let partial = (size < GROUP).then(|| func.create_block());
    let mut build = Builder::new(func, block).at(span);
    let three = build.iconst(word, 3);
    let group = build.binary(Opcode::LShr, address, three, Flags::NONE);
    let base = build.iconst(word, offset as i128);
    let at = build.binary(Opcode::Add, group, base, Flags::NONE);
    let at = build.unary(Opcode::IntToPtr, at, Type::PTR);
    // An access of two groups reads their two shadow bytes as one.
    let wide = if size == 2 * GROUP { Type::int(16) } else { Type::int(8) };
    let info = MemInfo {
        size: size.max(8) / 8,
        align: 1,
        order: MemOrder::NotAtomic,
        tbaa: None,
        owns: 0,
        restrict: Restrict::NONE,
    };
    let byte = build.load(wide, at, info, Flags::NONE);
    let zero = build.iconst(wide, 0);
    let poisoned = build.icmp(IntPred::Ne, byte, zero);
    // Most shadow bytes are zero, so the compare that a byte from one to seven needs is off the
    // path the access takes when it is good, as gcc has it.
    build.br_if(poisoned, partial.unwrap_or(reports), &[], after, &[]);
    if let Some(partial) = partial {
        let mut build = Builder::new(func, partial).at(span);
        // The offset of the last byte in its group, which has to be below the number of bytes
        // the shadow says may be used.
        let seven = build.iconst(word, 7);
        let low = build.binary(Opcode::And, address, seven, Flags::NONE);
        let low = build.unary(Opcode::Trunc, low, Type::int(8));
        let last = build.iconst(Type::int(8), size as i128 - 1);
        let last = build.binary(Opcode::Add, low, last, Flags::NONE);
        let past = build.icmp(IntPred::Sge, last, byte);
        build.br_if(past, reports, &[], after, &[]);
    }
    let signature = func.add_signature(signature);
    let mut build = Builder::new(func, reports).at(span);
    build.call(report, signature, &[access.address]);
    build.jump(after, &[]);
}

/// The inline check of an access that is not a scalar one: the shadow of its first byte and of
/// its last byte are each read and compared as a one byte access would be, and either one that
/// is bad calls the report with the address and the size, as gcc does.
fn ends(
    func: &mut Func,
    access: Access,
    size: u64,
    offset: u64,
    report: Symbol,
    signature: Signature,
) {
    let word = Type::int(64);
    let byte = Type::int(8);
    let before = access.before;
    let span = func.span(before);
    let args = func.push_values(&[access.address]);
    let first =
        func.create_inst(InstData { args, ..InstData::new(Opcode::PtrToInt) }, &[word], span);
    func.insert_before(first, before);
    let address = func[first].first_result.expect("a value");
    let block = func.block_of(first).expect("the access is in a block");
    let after = func.create_block();
    func.move_after(first, after);
    let reports = func.create_block();
    let info = MemInfo {
        size: 1,
        align: 1,
        order: MemOrder::NotAtomic,
        tbaa: None,
        owns: 0,
        restrict: Restrict::NONE,
    };
    let mut build = Builder::new(func, block).at(span);
    let three = build.iconst(word, 3);
    let seven = build.iconst(word, 7);
    let base = build.iconst(word, offset as i128);
    let zero = build.iconst(byte, 0);
    let back = build.iconst(word, size as i128 - 1);
    let last = build.binary(Opcode::Add, address, back, Flags::NONE);
    let mut bad = None;
    for at in [address, last] {
        let group = build.binary(Opcode::LShr, at, three, Flags::NONE);
        let shadow = build.binary(Opcode::Add, group, base, Flags::NONE);
        let shadow = build.unary(Opcode::IntToPtr, shadow, Type::PTR);
        let value = build.load(byte, shadow, info, Flags::NONE);
        let poisoned = build.icmp(IntPred::Ne, value, zero);
        let low = build.binary(Opcode::And, at, seven, Flags::NONE);
        let low = build.unary(Opcode::Trunc, low, byte);
        let past = build.icmp(IntPred::Sge, low, value);
        let one = build.binary(Opcode::And, poisoned, past, Flags::NONE);
        bad = Some(match bad {
            Some(other) => build.binary(Opcode::Or, other, one, Flags::NONE),
            None => one,
        });
    }
    let bad = bad.expect("two ends were checked");
    build.br_if(bad, reports, &[], after, &[]);
    let signature = func.add_signature(signature);
    let mut build = Builder::new(func, reports).at(span);
    let length = build.iconst(word, size as i128);
    build.call(report, signature, &[access.address, length]);
    build.jump(after, &[]);
}

/// The shadow byte of the left redzone, where the frame record is.
const LEFT: u8 = 0xf1;
/// The shadow byte of a redzone between two locals.
const MIDDLE: u8 = 0xf2;
/// The shadow byte of the redzone after the last local.
const RIGHT: u8 = 0xf3;
/// What the first word of the frame says, so the runtime can find the record from an address in
/// the frame.
const MAGIC: i128 = 0x41b5_8ab3;

/// The bytes after a local of that size before the next one starts, which is gcc's
/// `asan_var_and_redzone_size` before the round up to 32.
fn redzone(size: u64) -> u64 {
    match size {
        0..=4 => 16,
        5..=128 => 32,
        129..=512 => 64,
        513..=4096 => 128,
        _ => 256,
    }
}

/// Puts the locals in `guarded` into one frame with redzones between them, as gcc lays it out,
/// poisons the shadow of the redzones on the way in and clears it on the way out, and says how
/// many locals it moved.
///
/// The frame starts with 32 bytes that hold the magic word, the address of a string that names
/// each local with its offset and size, and the address of the function. The kernel reads those
/// three when it reports a bad access to the stack. Each local starts at a multiple of 32.
fn frame(func: &mut Func, runtime: &mut Runtime<'_>, guarded: &Set<Inst>, offset: u64) -> usize {
    let Some(entry) = func.entry() else { return 0 };
    let slots: Vec<Inst> = func
        .insts(entry)
        .filter(|&inst| guarded.contains(&inst) && func[func[inst].args].is_empty())
        .collect();
    if slots.is_empty() {
        return 0;
    }
    let mut places = Vec::new();
    let mut at = 32u64;
    let mut align = 16u32;
    let mut shadow = vec![LEFT; 4];
    let mut text = slots.len().to_string();
    for (n, &slot) in slots.iter().enumerate() {
        let Extra::Mem(mem) = func[slot].extra else { return 0 };
        let (size, slot_align) = (func[mem].size.max(1), func[mem].align);
        align = align.max(slot_align);
        let start = at.next_multiple_of(u64::from(slot_align).max(32));
        shadow.resize((start / 8) as usize, MIDDLE);
        let end = (start + size + redzone(size)).next_multiple_of(32);
        let name = format!("v{n}");
        text.push_str(&format!(" {start} {size} {} {name}", name.len()));
        for granule in 0..(end - start) / 8 {
            let used = size.saturating_sub(granule * 8);
            shadow.push(match used {
                0 => MIDDLE,
                1..=7 => used as u8,
                _ => 0,
            });
        }
        places.push((slot, start));
        at = end;
    }
    for byte in shadow.iter_mut().rev() {
        if *byte != MIDDLE {
            break;
        }
        *byte = RIGHT;
    }
    let word = Type::int(64);
    let first = func
        .insts(entry)
        .find(|&inst| func[inst].opcode != Opcode::MemEntry && !slots.contains(&inst))
        .expect("the entry block has a terminator");
    let info = |size: u64, align: u32| MemInfo {
        size,
        align,
        order: MemOrder::NotAtomic,
        tbaa: None,
        owns: 0,
        restrict: Restrict::NONE,
    };
    let mem = func.add_mem(info(at, align));
    let span = func.span(first);
    let data = InstData { extra: Extra::Mem(mem), ..InstData::new(Opcode::Alloca) };
    let base = func.create_inst(data, &[Type::PTR], span);
    func.insert_before(base, first);
    let base = func[base].first_result.expect("a value");
    let mut forward = Map::default();
    for &(slot, start) in &places {
        let by = constant(func, word, i128::from(start), first);
        let args = func.push_values(&[base, by]);
        let moved =
            place(func, InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR, first);
        let old = func[slot].first_result.expect("a value");
        forward.insert(old, moved);
    }
    crate::uses::substitute(func, &forward);
    for &(slot, _) in &places {
        func.remove_inst(slot);
    }
    // The record, which the kernel reads when an access in the frame is bad.
    let name = runtime.names.intern(&format!("__asan_gen_frame.{}", runtime.frames.len()));
    runtime.frames.push((name, text));
    let magic = constant(func, word, MAGIC, first);
    store(func, magic, base, 0, info(8, 8), first);
    let data = InstData { extra: Extra::Symbol(name), ..InstData::new(Opcode::GlobalAddr) };
    let record = place(func, data, Type::PTR, first);
    store(func, record, base, 8, info(8, 8), first);
    let data = InstData { extra: Extra::Symbol(func.name), ..InstData::new(Opcode::GlobalAddr) };
    let pc = place(func, data, Type::PTR, first);
    store(func, pc, base, 16, info(8, 8), first);
    // The shadow of the frame, written four bytes at a time where the four are not all zero.
    let args = func.push_values(&[base]);
    let address = place(func, InstData { args, ..InstData::new(Opcode::PtrToInt) }, word, first);
    let three = constant(func, word, 3, first);
    let args = func.push_values(&[address, three]);
    let group = place(func, InstData { args, ..InstData::new(Opcode::LShr) }, word, first);
    let start = constant(func, word, offset as i128, first);
    let args = func.push_values(&[group, start]);
    let group = place(func, InstData { args, ..InstData::new(Opcode::Add) }, word, first);
    let args = func.push_values(&[group]);
    let to = place(func, InstData { args, ..InstData::new(Opcode::IntToPtr) }, Type::PTR, first);
    shadow.resize(shadow.len().next_multiple_of(4), RIGHT);
    let words: Vec<(u64, u32)> = shadow
        .chunks(4)
        .enumerate()
        .map(|(n, four)| (4 * n as u64, u32::from_le_bytes([four[0], four[1], four[2], four[3]])))
        .filter(|&(_, bits)| bits != 0)
        .collect();
    let int = Type::int(32);
    for &(at, bits) in &words {
        let bits = constant(func, int, i128::from(bits), first);
        store(func, bits, to, at, info(4, 1), first);
    }
    let exits: Vec<Inst> = func
        .blocks()
        .filter_map(|block| func.terminator(block))
        .filter(|&inst| matches!(func[inst].opcode, Opcode::Return | Opcode::TailCall))
        .collect();
    for exit in exits {
        let zero = constant(func, int, 0, exit);
        for &(at, _) in &words {
            store(func, zero, to, at, info(4, 1), exit);
        }
    }
    places.len()
}

/// A store of `value` at `offset` bytes past `base`, just before `before`.
fn store(func: &mut Func, value: Value, base: Value, offset: u64, info: MemInfo, before: Inst) {
    let address = if offset == 0 {
        base
    } else {
        let by = constant(func, Type::int(64), i128::from(offset), before);
        let args = func.push_values(&[base, by]);
        place(func, InstData { args, ..InstData::new(Opcode::PtrAdd) }, Type::PTR, before)
    };
    let mem = func.add_mem(info);
    let args = func.push_values(&[value, address]);
    let data = InstData { args, extra: Extra::Mem(mem), ..InstData::new(Opcode::Store) };
    let span = func.span(before);
    let inst = func.create_inst(data, &[], span);
    func.insert_before(inst, before);
}

/// A copy or a fill whose length the program works out, or a call to `memcpy`, `memmove` or
/// `memset`, turned into a call to the routine of the same name with `__asan_` in front.
fn prefixed(func: &mut Func, runtime: &mut Runtime<'_>, inst: Inst) {
    let data = func[inst];
    if data.opcode == Opcode::Call {
        let Extra::Call(info) = data.extra else { return };
        let Some(callee) = func[info].callee else { return };
        let name = format!("__asan_{}", runtime.names.resolve(callee));
        let call = func[info];
        let signature = func[call.signature].clone();
        let returns = param_types(&signature.returns);
        let (callee, _) = runtime.callee(&name, &param_types(&signature.params), &returns);
        let info = func.add_call(CallInfo { callee: Some(callee), ..call });
        func[inst].extra = Extra::Call(info);
        return;
    }
    let Some(bulk) = func.bulk(inst) else { return };
    let Some(length) = bulk.length else { return };
    let word = Type::int(64);
    let (name, with) = match data.opcode {
        Opcode::Memcpy => ("__asan_memcpy", Type::PTR),
        Opcode::Memmove => ("__asan_memmove", Type::PTR),
        _ => ("__asan_memset", Type::int(32)),
    };
    let (callee, signature) = runtime.callee(name, &[Type::PTR, with, word], &[Type::PTR]);
    let value = widen(func, bulk.with, with, inst);
    let length = widen(func, length, word, inst);
    let span = func.span(inst);
    let signature = func.add_signature(signature);
    let varargs = func.push_abis(&[]);
    let info = func.add_call(CallInfo { callee: Some(callee), signature, varargs });
    let args = func.push_values(&[bulk.to, value, length]);
    let data = InstData { args, extra: Extra::Call(info), ..InstData::new(Opcode::Call) };
    let made = func.create_inst(data, &[Type::PTR], span);
    func.insert_before(made, inst);
    func.remove_inst(inst);
}

/// The types of the parameters or the results of a signature.
fn param_types(params: &[rucc_ir::Param]) -> Vec<Type> {
    params.iter().map(|param| param.ty).collect()
}

/// An integer constant just before `before`.
fn constant(func: &mut Func, ty: Type, value: i128, before: Inst) -> Value {
    let imm = func.add_imm(Imm::int(value, ty));
    let data = InstData { extra: Extra::Imm(imm), ..InstData::new(Opcode::IConst) };
    place(func, data, ty, before)
}

/// The value zero extended or cut to `to`, just before `before`, or the value itself when it is
/// that wide already.
fn widen(func: &mut Func, value: Value, to: Type, before: Inst) -> Value {
    let from = func[value].ty;
    if from == to || !from.is_int() {
        return value;
    }
    let opcode = if from.bits() < to.bits() { Opcode::ZExt } else { Opcode::Trunc };
    let args = func.push_values(&[value]);
    place(func, InstData { args, ..InstData::new(opcode) }, to, before)
}

/// An instruction with one result just before `before`.
fn place(func: &mut Func, data: InstData, ty: Type, before: Inst) -> Value {
    let span = func.span(before);
    let inst = func.create_inst(data, &[ty], span);
    func.insert_before(inst, before);
    func[inst].first_result.expect("a value")
}

/// A call to the runtime just before `before`.
fn call(func: &mut Func, callee: Symbol, signature: Signature, args: &[Value], before: Inst) {
    let span = func.span(before);
    let signature = func.add_signature(signature);
    let varargs = func.push_abis(&[]);
    let info = func.add_call(CallInfo { callee: Some(callee), signature, varargs });
    let args = func.push_values(args);
    let data = InstData { args, extra: Extra::Call(info), ..InstData::new(Opcode::Call) };
    let inst = func.create_inst(data, &[], span);
    func.insert_before(inst, before);
}

#[cfg(test)]
mod tests {
    use super::*;
    use rucc_target::{TargetInfo, Triple};

    #[test]
    fn a_global_has_the_redzone_gcc_gives_it() {
        let sizes = [(0, 32), (4, 60), (5, 59), (8, 56), (32, 32), (40, 56), (100, 60)];
        for (size, redzone) in sizes {
            assert_eq!(global_redzone(size), redzone, "{size}");
        }
        let mut names = Interner::default();
        let target = TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().unwrap());
        let mut module = Module::new(names.intern("t.c"), &target);
        let mut empty = Global::new(names.intern("key"), 0, 1);
        empty.init = Some(module.push_data(&[]));
        assert!(protected(&empty), "a global of no size gets one");
        empty.linkage = Linkage::Weak;
        assert!(!protected(&empty), "a weak one does not");
    }

    #[test]
    fn a_value_takes_the_bytes_its_type_is_wide() {
        assert_eq!(bytes(Type::int(8)), 1);
        assert_eq!(bytes(Type::int(32)), 4);
        assert_eq!(bytes(Type::PTR), 8);
        assert_eq!(bytes(Type::int(128)), 16);
        assert_eq!(bytes(Type::vector(Type::int(32), 4)), 16);
    }
}
