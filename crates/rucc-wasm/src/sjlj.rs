//! `setjmp` and `longjmp`, as calls to the wasi-libc library `libsetjmp` and edges that catch
//! the exception that its `longjmp` throws.
//!
//! Design: the WebAssembly notes, section 7.6. Wasm has no way to go back into a frame that is
//! below the top of the stack, so a `longjmp` throws an exception with the tag `__c_longjmp`,
//! and the function that called `setjmp` catches it and goes on after its `setjmp`. That is the
//! ABI of wasi-libc and of LLVM's `WebAssemblyLowerEmscriptenEHSjLj` pass, and rucc keeps it so
//! that its objects link with the same `libsetjmp.a` as the objects of clang.
//!
//! [`prepare`] changes the IR before the translation, so that the selector only has to write a
//! `try_table` for each edge that the change adds. In each function:
//!
//! 1. A call to `longjmp`, `_longjmp` or `siglongjmp` becomes a call to `__wasm_longjmp`, which
//!    keeps the value and the buffer and throws.
//! 2. A function that calls `setjmp` gets a slot of four bytes whose address identifies this
//!    call of the function. Call `k` of `setjmp` becomes `__wasm_setjmp(env, k, id)`, which
//!    writes the number and the address into the buffer, and the block is split after it. The new
//!    block takes the value of `setjmp` as its parameter, which is zero when control goes past
//!    the call. The stack pointer is kept before the call.
//! 3. Each call that control can reach after a `setjmp` is the last instruction of its block,
//!    and an `unwound` and a `br_if` follow it. The branch goes to one new block, the dispatch,
//!    when the call throws. The selector puts the call in a `try_table` that catches
//!    `__c_longjmp`. See `select.rs`.
//! 4. The dispatch reads the buffer and the value from what the exception carries and asks
//!    `__wasm_setjmp_test` which call of `setjmp` in this call of the function wrote the buffer.
//!    For call `k`, it puts the stack pointer back and goes to the block after call `k` with the
//!    value. For no call, the `longjmp` is for a function further out, and the dispatch throws
//!    it again with `__wasm_longjmp`.
//!
//! The values of the IR are each in a local of their own, and an exception does not change a
//! local, so a value that the function computed before the `longjmp` is still there after it.
//! That is why this change does not have to repair the SSA form, as LLVM's pass does. The edges
//! from the dispatch back to the blocks after the calls of `setjmp` can make a value that a block
//! reads come from a block that no longer dominates it, but no stage after this one asks about
//! dominance, and the local holds the value that C says the variable has.

use rucc_base::Interner;
use rucc_base::hash::Map;
use rucc_ir::{
    Block, Builder, CallInfo, Extra, Func, FuncId, Inst, InstData, MemInfo, MemOrder, Module,
    Opcode, Restrict, Signature, SymbolRef, Type, Value,
};

use crate::Refusal;

/// The names that become a call to `__wasm_longjmp`.
const LONGJMP: &[&str] = &["longjmp", "_longjmp", "siglongjmp"];

/// Change the calls of `setjmp` and `longjmp` in `module` into the calls and the edges of the
/// ABI of `libsetjmp`. See the module documentation. A module with no such calls does not change.
///
/// # Errors
///
/// A [`Refusal`] for a function that calls `vfork` or another function that returns twice and is
/// not `setjmp`, that uses `__builtin_setjmp`, or that unwinds to a cleanup with
/// `-fexceptions`.
pub fn prepare(module: &mut Module, names: &mut Interner) -> Result<(), Refusal> {
    let symbols = Symbols {
        setjmp: names.intern("__wasm_setjmp"),
        test: names.intern("__wasm_setjmp_test"),
        longjmp: names.intern("__wasm_longjmp"),
    };
    for id in module.funcs() {
        if module[id].is_declaration() {
            continue;
        }
        let refuse = |why: String| Refusal {
            function: Some(names.resolve(module[id].name).to_owned()),
            why,
        };
        let calls = classify(module, id, names).map_err(refuse)?;
        if calls.longjmps.is_empty() && calls.setjmps.is_empty() {
            continue;
        }
        let func = &mut module[id];
        for &call in &calls.longjmps {
            rename(func, call, symbols.longjmp);
        }
        if !calls.setjmps.is_empty() {
            rewrite(func, &calls.setjmps, symbols);
        }
    }
    Ok(())
}

/// The symbols of `libsetjmp` that the calls go to.
#[derive(Clone, Copy)]
struct Symbols {
    setjmp: rucc_base::Symbol,
    test: rucc_base::Symbol,
    longjmp: rucc_base::Symbol,
}

/// The calls of one function that this change rewrites.
#[derive(Default)]
struct Calls {
    setjmps: Vec<Inst>,
    longjmps: Vec<Inst>,
}

/// Find the calls of `setjmp` and `longjmp` in the function `id`, and refuse what this change
/// cannot do.
fn classify(module: &Module, id: FuncId, names: &Interner) -> Result<Calls, String> {
    let func = &module[id];
    let mut calls = Calls::default();
    for block in func.blocks() {
        for inst in func.insts(block) {
            let data = &func[inst];
            match data.opcode {
                Opcode::SetjmpMarker | Opcode::LongjmpMarker => {
                    return Err("`__builtin_setjmp` and `__builtin_longjmp` are not translated \
                                for wasm, use `setjmp` and `longjmp`"
                        .into());
                }
                Opcode::Unwound | Opcode::Landing => {
                    return Err("unwinding to a cleanup with `-fexceptions` is not translated \
                                for wasm yet"
                        .into());
                }
                Opcode::Call => {}
                _ => continue,
            }
            let Extra::Call(info) = data.extra else { continue };
            let Some(callee) = func[info].callee else { continue };
            let name = names.resolve(callee);
            if LONGJMP.contains(&name) {
                calls.longjmps.push(inst);
            } else if module.returns_twice(callee, names) {
                if !matches!(name.trim_start_matches('_'), "setjmp" | "sigsetjmp") {
                    return Err(format!(
                        "`{name}` returns twice, and wasm can only do that for `setjmp`"
                    ));
                }
                if func[data.args].is_empty() || data.results().count() != 1 {
                    return Err(format!("a call to `{name}` that does not take the buffer"));
                }
                calls.setjmps.push(inst);
            }
        }
    }
    // A function that the module declares with one of the names and does not define is the one
    // of the C library, and is the case above. A function that the module defines with one of
    // these names is a program's own `setjmp`, which is an ordinary function.
    let own = |inst: &Inst| {
        let Extra::Call(info) = func[*inst].extra else { return false };
        let callee = func[info].callee.expect("a direct call");
        matches!(module.lookup(callee), Some(SymbolRef::Func(f)) if !module[f].is_declaration())
    };
    calls.setjmps.retain(|inst| !own(inst));
    calls.longjmps.retain(|inst| !own(inst));
    Ok(calls)
}

/// Make the call `inst` go to `callee`, with the same signature and arguments.
fn rename(func: &mut Func, inst: Inst, callee: rucc_base::Symbol) {
    let Extra::Call(info) = func[inst].extra else { return };
    let info = CallInfo { callee: Some(callee), ..func[info] };
    let info = func.add_call(info);
    func[inst].extra = Extra::Call(info);
}

/// The access of an `i32` or a pointer at a known place, with nothing known about it.
fn plain(size: u64) -> MemInfo {
    MemInfo {
        size,
        align: 4,
        order: MemOrder::NotAtomic,
        tbaa: None,
        owns: 0,
        restrict: Restrict::NONE,
    }
}

/// Move the instructions after `inst` in its block into a new block, and give the new block.
fn split_after(func: &mut Func, inst: Inst) -> Block {
    let block = func.create_block();
    let mut next = func.next_inst(inst);
    while let Some(moved) = next {
        next = func.next_inst(moved);
        func.remove_inst(moved);
        func.append_inst(block, moved);
    }
    block
}

/// Steps 2 to 4 of the module documentation, for one function and its calls of `setjmp`.
fn rewrite(func: &mut Func, setjmps: &[Inst], symbols: Symbols) {
    let ptr = Type::PTR;
    let i32 = Type::int(32);
    let set_sig = func.add_signature(Signature::new().with_params(&[ptr, i32, ptr]));
    let test_sig =
        func.add_signature(Signature::new().with_params(&[ptr, ptr]).with_returns(&[i32]));
    let throw_sig = func.add_signature(Signature::new().with_params(&[ptr, i32]));

    // The slot whose address identifies this call of the function, at the top of the entry.
    let entry = func.entry().expect("a defined function has an entry");
    let first = func.insts(entry).next().expect("the entry has a terminator");
    let mem = func.add_mem(plain(4));
    let alloca = InstData { extra: Extra::Mem(mem), ..InstData::new(Opcode::Alloca) };
    let alloca = func.create_inst(alloca, &[ptr], func.span(first));
    func.insert_before(alloca, first);
    let id = func[alloca].first_result.expect("an alloca has a result");

    // Each call of `setjmp`, as `__wasm_setjmp` and a jump to the block after it.
    let mut forward: Map<Value, Value> = Map::default();
    let mut after: Vec<(Value, Block)> = Vec::new();
    for (k, &call) in setjmps.iter().enumerate() {
        let span = func.span(call);
        let block = func.block_of(call).expect("the call is in a block");
        let env = func[func[call].args][0];
        let result = func[call].results().next().expect("setjmp has a result");
        let save = func.create_inst(InstData::new(Opcode::StackSave), &[ptr], span);
        func.insert_before(save, call);
        let sp = func[save].first_result.expect("stacksave has a result");
        let next = split_after(func, call);
        let value = func.append_param(next, i32);
        func.remove_inst(call);
        let mut build = Builder::new(func, block).at(span);
        let number = build.iconst(i32, k as i128 + 1);
        build.call(symbols.setjmp, set_sig, &[env, number, id]);
        let zero = build.iconst(i32, 0);
        build.jump(next, &[zero]);
        forward.insert(result, value);
        after.push((sp, next));
    }
    substitute(func, &forward);

    // The calls that control reaches after a `setjmp`, which a `longjmp` can come out of.
    let mut seen: Map<Block, ()> = Map::default();
    let mut work: Vec<Block> = after.iter().map(|&(_, block)| block).collect();
    let mut throwing = Vec::new();
    while let Some(block) = work.pop() {
        if seen.insert(block, ()).is_some() {
            continue;
        }
        for inst in func.insts(block) {
            let data = &func[inst];
            let callee = match (data.opcode, data.extra) {
                (Opcode::Call, Extra::Call(info)) => func[info].callee,
                (Opcode::CallIndirect, _) => None,
                _ => continue,
            };
            if callee != Some(symbols.setjmp) {
                throwing.push(inst);
            }
        }
        if let Some(term) = func.terminator(block) {
            work.extend(func.successors(term).map(|call| call.block));
        }
    }

    // The dispatch, the block after each call that the dispatch goes back to, and the block that
    // throws a `longjmp` for a function further out.
    let span = func.span(first);
    let dispatch = func.create_block();
    let rethrow = func.create_block();
    let mut cases = Vec::new();
    {
        let mut build = Builder::new(func, dispatch).at(span);
        let caught = build.value(InstData::new(Opcode::Landing), ptr);
        let env = build.load(ptr, caught, plain(4), Default::default());
        let four = build.iconst(i32, 4);
        let at = build.binary(Opcode::PtrAdd, caught, four, Default::default());
        let value = build.load(i32, at, plain(4), Default::default());
        let which = build.call(symbols.test, test_sig, &[env, id]);
        let which = build.func()[which].first_result.expect("the test has a result");
        for (k, &(sp, next)) in after.iter().enumerate() {
            let back = build.func().create_block();
            let mut restore = Builder::new(build.func(), back).at(span);
            let args = restore.func().push_values(&[sp]);
            restore.inst(InstData { args, ..InstData::new(Opcode::StackRestore) }, &[]);
            restore.jump(next, &[value]);
            cases.push((k as i128 + 1, back));
        }
        build.switch(which, rethrow, &cases);
        let mut build = Builder::new(func, rethrow).at(span);
        build.call(symbols.longjmp, throw_sig, &[env, value]);
        build.unreachable();
    }

    // The edge out of each call that can throw.
    for call in throwing {
        let span = func.span(call);
        let block = func.block_of(call).expect("the call is in a block");
        let next = split_after(func, call);
        let mut build = Builder::new(func, block).at(span);
        let unwound = build.value(InstData::new(Opcode::Unwound), Type::I1);
        build.br_if(unwound, dispatch, &[], next, &[]);
    }
}

/// Make each reader of a key of `forward` read its value instead, in the arguments of each
/// instruction and of each branch.
fn substitute(func: &mut Func, forward: &Map<Value, Value>) {
    let with = |value: Value| forward.get(&value).copied().unwrap_or(value);
    for block in func.blocks().collect::<Vec<_>>() {
        for inst in func.insts(block).collect::<Vec<Inst>>() {
            let args = func[inst].args;
            func.rewrite(args, with);
            for call in func.successors(inst).collect::<Vec<_>>() {
                func.rewrite(call.args, with);
            }
        }
    }
}
