//! Calls to `memcpy`, `memmove` and `memset` as `memory.copy` and `memory.fill`.
//!
//! With the bulk memory feature, wasm has two instructions that the engine runs as one copy or one
//! fill of native code. A call to the C library does the same work in wasm code, a word at a time,
//! and that costs more on each call and much more on a long one. clang writes the three calls as
//! the two instructions, and [`bulk`] does the same. `memory.copy` is right when the two areas
//! overlap, so it does the work of `memmove` as well as the work of `memcpy`.
//!
//! The call becomes the bulk operation of the IR with the length as its third operand, and the
//! selector writes that operation as the instruction, or as loads and stores when the length is a
//! small constant. Each of the three calls gives back its destination, so each use of the answer
//! reads the destination.
//!
//! A function marked `no_builtin` keeps its calls, because the mark says that the name is whatever
//! function the link finds, and that is so under `-fno-builtin` and under `-fsafety`, where the
//! copy has to go through the body that marks the bytes as written. A module that defines one of
//! the three keeps the calls to it, because the body is the program's own.

use rucc_base::Interner;
use rucc_base::hash::Map;
use rucc_ir::{
    AttrSet, BlockCall, Extra, Func, Inst, InstData, MemInfo, MemOrder, Module, Opcode, Restrict,
    SymbolRef, Type, Value,
};
use rucc_target::wasm::{Feature, Features};

/// Change each call to `memcpy`, `memmove` and `memset` in `module` into the bulk operation of the
/// IR, when `features` has the bulk memory instructions. See the module documentation.
pub fn bulk(module: &mut Module, names: &Interner, features: Features) {
    if !features.has(Feature::BulkMemoryOpt) {
        return;
    }
    for id in module.funcs().collect::<Vec<_>>() {
        let func = &module[id];
        if func.is_declaration() || func.attrs.set.contains(AttrSet::NO_BUILTIN) {
            continue;
        }
        let calls: Vec<(Inst, Opcode)> = func
            .blocks()
            .flat_map(|block| func.insts(block))
            .filter_map(|inst| Some((inst, library(module, func, names, inst)?)))
            .collect();
        if calls.is_empty() {
            continue;
        }
        let func = &mut module[id];
        let mut answers: Map<Value, Value> = Map::default();
        for (inst, opcode) in calls {
            let &[to, with, length] = &func[func[inst].args] else { continue };
            // The length is an operand, so the payload says zero bytes. A byte of alignment is
            // all that the call says about the two addresses.
            let info = MemInfo {
                size: 0,
                align: 1,
                order: MemOrder::NotAtomic,
                tbaa: None,
                owns: 0,
                restrict: Restrict::NONE,
            };
            let extra = Extra::Mem(func.add_mem(info));
            let args = func.push_values(&[to, with, length]);
            let span = func.span(inst);
            let made =
                func.create_inst(InstData { args, extra, ..InstData::new(opcode) }, &[], span);
            func.insert_before(made, inst);
            if let Some(answer) = func[inst].first_result {
                answers.insert(answer, to);
            }
            func.remove_inst(inst);
        }
        // The destination of one call can be the answer of a call before it, so each answer is
        // followed to the end of the chain before the uses change.
        let settled = |mut value: Value| {
            while let Some(&to) = answers.get(&value) {
                value = to;
            }
            value
        };
        let insts: Vec<Inst> = func.blocks().flat_map(|block| func.insts(block)).collect();
        for inst in insts {
            func.rewrite(func[inst].args, settled);
            let arms: Vec<BlockCall> = func.successors(inst).collect();
            for arm in arms {
                func.rewrite(arm.args, settled);
            }
        }
    }
}

/// The bulk operation that `inst` is, when it is a call to `memcpy`, `memmove` or `memset` of the
/// library with the arguments and the answer that the C library gives the function.
fn library(module: &Module, func: &Func, names: &Interner, inst: Inst) -> Option<Opcode> {
    let data = &func[inst];
    if data.opcode != Opcode::Call || func.unwinds_to_pad(inst) || func.carries_mem(inst) {
        return None;
    }
    let Extra::Call(info) = data.extra else { return None };
    let callee = func[info].callee?;
    let opcode = match names.resolve(callee) {
        "memcpy" => Opcode::Memcpy,
        "memmove" => Opcode::Memmove,
        "memset" => Opcode::Memset,
        _ => return None,
    };
    if matches!(module.lookup(callee), Some(SymbolRef::Func(f)) if !module[f].is_declaration()) {
        return None;
    }
    if func[func[info].signature].variadic {
        return None;
    }
    let &[to, with, length] = &func[data.args] else { return None };
    let source = match opcode {
        Opcode::Memset => Type::int(32),
        _ => Type::PTR,
    };
    let mut results = data.results();
    let answer = results.next().is_none_or(|answer| func[answer].ty == Type::PTR);
    let fits = func[to].ty == Type::PTR
        && func[with].ty == source
        && func[length].ty == Type::int(32)
        && answer
        && results.next().is_none();
    fits.then_some(opcode)
}
