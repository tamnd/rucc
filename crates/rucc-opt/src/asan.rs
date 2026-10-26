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
//! An access of one, two or four bytes reads the shadow byte of its first byte. When that byte is
//! not zero and the offset of the last byte in its group of eight is not below it, the access is
//! bad and `__asan_report_load4_noabort` or the call of that size is made with the address. An
//! access of eight bytes that is aligned to eight is bad when its shadow byte is not zero, and one
//! of sixteen when its two shadow bytes are not. Any other access, which is one of eight or sixteen
//! bytes that is not aligned or one of another size, calls `__asan_load8_noabort` or
//! `__asan_loadN_noabort` and the runtime works it out. These are gcc's rules, and like gcc's they
//! read the shadow of the first group only, so an access of four bytes that is not aligned and
//! runs into a bad group is not caught.
//!
//! A function with as many accesses as `--param asan-instrumentation-with-call-threshold=` says
//! makes a call in front of every one instead, `__asan_load4_noabort` and the rest, which is how
//! `CONFIG_KASAN_OUTLINE` asks for it with a threshold of zero.
//!
//! A copy or a fill the compiler makes is checked as a read of the bytes it reads and a write of
//! the bytes it writes. One whose length the program works out is a call to `__asan_memcpy`,
//! `__asan_memmove` or `__asan_memset` under `--param asan-kernel-mem-intrinsic-prefix=1`, and so
//! is a call to `memcpy`, `memmove` or `memset` itself. Those check what they touch. The front end
//! and [`crate::libcall`] keep every fill the source spells as a call, and every copy or move it
//! spells that is not one access of 1, 2, 4 or 8 bytes, as gcc does, so those are calls here too.
//!
//! # What is left alone
//!
//! An access to a local or to a named object at a constant offset that is inside it, since the
//! check could not fail. An access through `__seg_fs` or `__seg_gs`, whose address is not a place
//! the shadow covers. A function marked `no_sanitize_address` or `naked`. The stack and the globals
//! have no redzones around them yet, so a read past the end of a local array is only caught when
//! it reaches memory the kernel poisoned itself.
//!
//! # Order
//!
//! After the optimizer, which is where gcc's pass is. A value the optimizer keeps in a register is
//! not an access any more, and a body inlined into a function marked `no_sanitize_address` gets no
//! checks, which is what the kernel's `noinstr` code needs.

use rucc_base::hash::Map;
use rucc_base::{Interner, Symbol};
use rucc_ir::{
    AttrSet, CallInfo, Extra, Flags, Func, FuncId, Inst, InstData, IntPred, MemInfo, MemOrder,
    Module, Opcode, Restrict, Signature, Type, Value,
};
use rucc_session::Kasan;

use crate::alias::{Origin, origin};

/// Puts the checks in every function of the module that has a body and was not marked
/// `no_sanitize_address` or `naked`, and says how many it put in.
pub fn run(module: &mut Module, names: &mut Interner, how: Kasan) -> usize {
    if !how.on {
        return 0;
    }
    let sizes: Map<Symbol, u64> =
        module.globals().map(|id| (module[id].name, module[id].size)).collect();
    let mut runtime = Runtime { names, declared: Vec::new() };
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
    for (name, signature) in runtime.declared {
        if module.lookup(name).is_none() {
            module.add_func(Func::new(name, signature));
        }
    }
    checks
}

/// The runtime functions called so far, declared once the walk is over.
struct Runtime<'a> {
    names: &'a mut Interner,
    declared: Vec<(Symbol, Signature)>,
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
                if data.opcode != Opcode::Memset {
                    accesses.push(Access {
                        before: inst,
                        address: bulk.with,
                        size,
                        align: 1,
                        write: false,
                    });
                }
                accesses.push(Access {
                    before: inst,
                    address: bulk.to,
                    size,
                    align: 1,
                    write: true,
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
    let outline = accesses.len() as u64 >= u64::from(how.call_threshold);
    let suffix = if how.abort { "" } else { "_noabort" };
    for &access in &accesses {
        check(func, runtime, access, outline, suffix, how.offset);
    }
    for &inst in &routines {
        prefixed(func, runtime, inst);
    }
    accesses.len() + routines.len()
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
    let inline = match access.size {
        Size::Bytes(size @ (1 | 2 | 4)) => Some(size),
        Size::Bytes(size @ (8 | 16)) if u64::from(access.align) >= GROUP => Some(size),
        _ => None,
    };
    match (access.size, inline) {
        (Size::Bytes(size), Some(size_inline)) if !outline => {
            debug_assert_eq!(size, size_inline);
            let name = format!("__asan_report_{kind}{size}{suffix}");
            let (callee, signature) = runtime.callee(&name, &[Type::PTR], &[]);
            shadow(func, access, size, offset, callee, signature);
        }
        (Size::Bytes(size @ (1 | 2 | 4 | 8 | 16)), _) => {
            let name = format!("__asan_{kind}{size}{suffix}");
            let (callee, signature) = runtime.callee(&name, &[Type::PTR], &[]);
            call(func, callee, signature, &[access.address], access.before);
        }
        (size, _) => {
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
    let mut build = rucc_ir::Builder::new(func, block).at(span);
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
        let mut build = rucc_ir::Builder::new(func, partial).at(span);
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
    let mut build = rucc_ir::Builder::new(func, reports).at(span);
    build.call(report, signature, &[access.address]);
    build.jump(after, &[]);
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
    let imm = func.add_imm(rucc_ir::Imm::int(value, ty));
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

    #[test]
    fn a_value_takes_the_bytes_its_type_is_wide() {
        assert_eq!(bytes(Type::int(8)), 1);
        assert_eq!(bytes(Type::int(32)), 4);
        assert_eq!(bytes(Type::PTR), 8);
        assert_eq!(bytes(Type::int(128)), 16);
        assert_eq!(bytes(Type::vector(Type::int(32), 4)), 16);
    }
}
