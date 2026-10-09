//! The calls `-fsanitize-coverage=trace-pc` and `-fsanitize-coverage=trace-cmp` ask for.
//!
//! Design: `spec/04-driver-and-cli.md` section 4.7.
//!
//! A fuzzer learns which inputs were worth keeping from these calls, and the kernel's kcov is the
//! one that matters here: `CONFIG_KCOV` puts `-fsanitize-coverage=trace-pc` on every unit that does
//! not opt out, and `CONFIG_KCOV_ENABLE_COMPARISONS` adds `trace-cmp`. The runtime is kcov's own,
//! in `kernel/kcov.c`, so what is written here is only the calls, with the names and arguments gcc
//! gives them.
//!
//! `trace-pc` is a call to `__sanitizer_cov_trace_pc` at the top of every block, which records the
//! address it returns to. `trace-cmp` is a call before every comparison of two integers of one,
//! two, four or eight bytes, to `__sanitizer_cov_trace_cmp1` up to `cmp8`, or to the `const_cmp`
//! one of the same width with the constant first when one side is a constant. A comparison of two
//! `float`s or two `double`s calls `__sanitizer_cov_trace_cmpf` or `cmpd`, and a `switch` calls
//! `__sanitizer_cov_trace_switch` with the value and a table of the count of cases, the width in
//! bits and the cases. Comparisons of pointers are left alone, as gcc leaves them.
//!
//! A unit that declares or defines one of these itself, which is what a runtime that counts the
//! calls in user space does, is called with the parameters it wrote, so a `cmp1` taking two
//! `unsigned char`s is handed two bytes and not two words. One whose parameters cannot hold the
//! values, or that returns something, gets no calls of that kind.
//!
//! # Order
//!
//! After the optimizer, which is where gcc's pass is. A block the optimizer merged away is not a
//! place a run can reach that another block does not also reach, so a call there would be one more
//! call on a path that already has one, and the instrumented kernel is slow enough. It also means
//! a body inlined into a function marked `no_sanitize_coverage` gets no calls, which is what the
//! kernel's `noinstr` code needs: the entry code runs before kcov can be called safely and uses
//! `__always_inline` helpers that say nothing about coverage.

use rucc_base::{Interner, Symbol};
use rucc_ir::{
    AttrSet, Block, CallInfo, Datum, Def, Extra, Float, Func, FuncId, Global, Imm, Inst, InstData,
    Linkage, Module, Opcode, Param, Signature, Type, Value,
};

/// What `-fsanitize-coverage=` asked for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Sancov {
    /// `trace-pc`, a call at the top of every block.
    pub pc: bool,
    /// `trace-cmp`, a call before every comparison and every `switch`.
    pub cmp: bool,
}

impl Sancov {
    /// Whether anything was asked for.
    #[must_use]
    pub const fn any(self) -> bool {
        self.pc || self.cmp
    }
}

/// Puts the calls `how` asks for in every function of the module that has a body and was not
/// marked `no_sanitize_coverage` or `naked`, and says how many calls it put in.
pub fn run(module: &mut Module, names: &mut Interner, how: Sancov) -> usize {
    if !how.any() {
        return 0;
    }
    let mut runtime = Runtime::default();
    for id in module.funcs().collect::<Vec<FuncId>>() {
        let func = &module[id];
        let name = names.resolve(func.name);
        if name.starts_with("__sanitizer_cov_trace_") {
            runtime.known.push((func.name, func.signature().clone()));
        }
    }
    let mut tables = Vec::new();
    let mut calls = 0;
    for id in module.funcs().collect::<Vec<FuncId>>() {
        let func = &module[id];
        if func.is_declaration()
            || func.attrs.set.contains(AttrSet::NO_SANCOV)
            || func.attrs.set.contains(AttrSet::NAKED)
        {
            continue;
        }
        let mut wants = Wants { runtime: &mut runtime, names, tables: &mut tables };
        if how.cmp {
            calls += comparisons(&mut module[id], &mut wants);
        }
        if how.pc {
            calls += blocks(&mut module[id], &mut wants);
        }
    }
    for (name, signature) in runtime.declared {
        if module.lookup(name).is_none() {
            module.add_func(Func::new(name, signature));
        }
    }
    for (name, cases) in tables {
        let data: Vec<Datum> = cases
            .iter()
            .map(|&case| {
                let value = module.add_imm(Imm::int(i128::from(case), Type::int(64)));
                Datum::Scalar { ty: Type::int(64), value }
            })
            .collect();
        let mut global = Global::new(name, 8 * data.len() as u64, 8);
        global.linkage = Linkage::Internal;
        global.constant = true;
        global.init = Some(module.push_data(&data));
        module.add_global(global);
    }
    calls
}

/// The runtime functions the unit has of its own, and the ones called so far that it does not,
/// declared once the walk is over.
#[derive(Default)]
struct Runtime {
    known: Vec<(Symbol, Signature)>,
    declared: Vec<(Symbol, Signature)>,
}

/// What a walk over one function needs from the module, which it cannot borrow while it holds
/// the function.
struct Wants<'a> {
    runtime: &'a mut Runtime,
    names: &'a mut Interner,
    /// The tables a `switch` call points at, each a name and its words.
    tables: &'a mut Vec<(Symbol, Vec<u64>)>,
}

impl Wants<'_> {
    /// The runtime function of that name and the signature a call to it with `values` has, which
    /// is the one the unit gave it or one taking `params`. None when the unit gave it one that
    /// cannot take `values`: as many parameters, each an integer no narrower than the integer it
    /// is handed or the very type of anything else, and nothing returned.
    fn callee(
        &mut self,
        name: &str,
        params: &[Type],
        values: &[Type],
    ) -> Option<(Symbol, Signature)> {
        let symbol = self.names.intern(name);
        if let Some((_, known)) = self.runtime.known.iter().find(|(known, _)| *known == symbol) {
            let fits = |param: &Param, &ty: &Type| match ty.is_int() && ty.is_scalar() {
                true => param.ty.is_int() && param.ty.is_scalar() && param.ty.bits() >= ty.bits(),
                false => param.ty == ty,
            };
            let takes = known.params.len() == values.len()
                && known.params.iter().zip(values).all(|(param, ty)| fits(param, ty))
                && known.returns.is_empty()
                && !known.variadic;
            return takes.then(|| (symbol, known.clone()));
        }
        let signature = Signature::new().with_params(params);
        if !self.runtime.declared.iter().any(|(declared, _)| *declared == symbol) {
            self.runtime.declared.push((symbol, signature.clone()));
        }
        Some((symbol, signature))
    }
}

/// A call to `__sanitizer_cov_trace_pc` at the top of every block, past the instructions that have
/// to stay first in the entry block.
fn blocks(func: &mut Func, wants: &mut Wants<'_>) -> usize {
    let Some((callee, signature)) = wants.callee("__sanitizer_cov_trace_pc", &[], &[]) else {
        return 0;
    };
    let mut calls = 0;
    for block in func.blocks().collect::<Vec<Block>>() {
        let Some(first) = func
            .insts(block)
            .find(|&inst| !matches!(func[inst].opcode, Opcode::MemEntry | Opcode::Alloca))
        else {
            continue;
        };
        call(func, callee, &signature, &[], first);
        calls += 1;
    }
    calls
}

/// A call before every comparison of two integers or two floating point numbers that has a call
/// of its own, and before every `switch` on an integer.
fn comparisons(func: &mut Func, wants: &mut Wants<'_>) -> usize {
    let mut calls = 0;
    let insts: Vec<Inst> = func.blocks().flat_map(|block| func.insts(block)).collect();
    for inst in insts {
        match func[inst].opcode {
            Opcode::ICmp => {
                let &[a, b] = &func[func[inst].args] else { continue };
                let ty = func[a].ty;
                if !ty.is_int() || !ty.is_scalar() {
                    continue;
                }
                let bytes = match ty.bits() {
                    8 => 1,
                    16 => 2,
                    32 => 4,
                    64 => 8,
                    _ => continue,
                };
                // The constant first, as gcc passes it, so that kcov can tell the two apart.
                let (name, a, b) = match (constant(func, a), constant(func, b)) {
                    (false, false) => (format!("__sanitizer_cov_trace_cmp{bytes}"), a, b),
                    (true, _) => (format!("__sanitizer_cov_trace_const_cmp{bytes}"), a, b),
                    (false, true) => (format!("__sanitizer_cov_trace_const_cmp{bytes}"), b, a),
                };
                // Under a word, each side travels as a word, zero extended the way a caller hands
                // over an `unsigned char` or an `unsigned short`.
                let word = Type::int(if bytes == 8 { 64 } else { 32 });
                let Some((callee, signature)) = wants.callee(&name, &[word, word], &[ty, ty])
                else {
                    continue;
                };
                call(func, callee, &signature, &[a, b], inst);
                calls += 1;
            }
            Opcode::FCmp => {
                let &[a, b] = &func[func[inst].args] else { continue };
                let ty = func[a].ty;
                let name = match ty.format() {
                    Some(Float::F32) if ty.is_scalar() => "__sanitizer_cov_trace_cmpf",
                    Some(Float::F64) if ty.is_scalar() => "__sanitizer_cov_trace_cmpd",
                    _ => continue,
                };
                let Some((callee, signature)) = wants.callee(name, &[ty, ty], &[ty, ty]) else {
                    continue;
                };
                call(func, callee, &signature, &[a, b], inst);
                calls += 1;
            }
            Opcode::Switch => {
                let Extra::Switch(info) = func[inst].extra else { continue };
                let &[value] = &func[func[inst].args] else { continue };
                let ty = func[value].ty;
                if !ty.is_int() || !ty.is_scalar() || ty.bits() > 64 {
                    continue;
                }
                let params = [Type::int(64), Type::PTR];
                let Some((callee, signature)) =
                    wants.callee("__sanitizer_cov_trace_switch", &params, &[ty, Type::PTR])
                else {
                    continue;
                };
                let cases: Vec<u64> =
                    func[func[info].cases].iter().map(|case| case.bits() as u64).collect();
                let mut words = vec![cases.len() as u64, u64::from(ty.bits().max(8))];
                words.extend(cases);
                let table = format!("__sancov_gen_cov_switch_values.{}", wants.tables.len());
                let table = wants.names.intern(&table);
                wants.tables.push((table, words));
                let address =
                    InstData { extra: Extra::Symbol(table), ..InstData::new(Opcode::GlobalAddr) };
                let address = place(func, address, Some(Type::PTR), inst).expect("an address");
                call(func, callee, &signature, &[value, address], inst);
                calls += 1;
            }
            _ => {}
        }
    }
    calls
}

/// Whether the value is an integer constant.
fn constant(func: &Func, value: Value) -> bool {
    let Def::Result { inst, .. } = func[value].def else { return false };
    func[inst].opcode == Opcode::IConst
}

/// The value zero extended to `to`, just before `before`, or the value itself when it is that wide
/// already.
fn widen(func: &mut Func, value: Value, to: Type, before: Inst) -> Value {
    if func[value].ty == to {
        return value;
    }
    let args = func.push_values(&[value]);
    let data = InstData { args, ..InstData::new(Opcode::ZExt) };
    place(func, data, Some(to), before).expect("a value")
}

/// An instruction just before `before`, with the result it makes.
fn place(func: &mut Func, data: InstData, ty: Option<Type>, before: Inst) -> Option<Value> {
    let span = func.span(before);
    let results: &[Type] = match &ty {
        Some(ty) => std::slice::from_ref(ty),
        None => &[],
    };
    let inst = func.create_inst(data, results, span);
    func.insert_before(inst, before);
    func[inst].results().next()
}

/// A call to the runtime just before `before`, each argument zero extended to its parameter.
fn call(func: &mut Func, callee: Symbol, signature: &Signature, args: &[Value], before: Inst) {
    let args: Vec<Value> = args
        .iter()
        .zip(&signature.params)
        .map(|(&arg, param)| widen(func, arg, param.ty, before))
        .collect();
    let signature = func.add_signature(signature.clone());
    let varargs = func.push_abis(&[]);
    let info = func.add_call(CallInfo { callee: Some(callee), signature, varargs });
    let args = func.push_values(&args);
    let data = InstData { args, extra: Extra::Call(info), ..InstData::new(Opcode::Call) };
    place(func, data, None, before);
}
