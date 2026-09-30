//! The calls `__attribute__((error("...")))` and `warning("...")` ask to have reported.
//!
//! Design: `spec/13-gnu-compat.md` section 13.4.
//!
//! gcc reports such a call when it survives optimization, and not before. The kernel's
//! `compiletime_assert` is written against exactly that: it declares a function carrying `error`
//! and calls it under a condition that is only a constant once the inline function it sits in has
//! been inlined, and the FORTIFY checks in `fortify-string.h` do the same with `__read_overflow`
//! and its kin. Reporting the call where it is written would refuse every kernel, and reporting
//! nothing turns a real failure into a link error naming `__compiletime_assert_417`, which is
//! right and no help at all. So the question is asked here, of the module the optimizer hands
//! the back end, and a call the optimizer took out is not in it.
//!
//! Only a function something can still reach is asked about. A `static` function every call to
//! which was inlined is not emitted by gcc, and the call in its body is not a call the program
//! makes, so reporting it would be a message about code that does not exist.

use rucc_base::hash::Set;
use rucc_base::{Interner, Symbol};
use rucc_diag::Diagnostic;
use rucc_ir::{AttrSet, Datum, Extra, Func, Linkage, Module, SymbolRef};

/// The code both messages carry.
const CODE: &str = "E0752";

/// One diagnostic for each message a call still in the module asks for, in the order the
/// functions and their calls are in.
pub(crate) fn surviving_calls(module: &Module, names: &Interner) -> Vec<Diagnostic> {
    if module.funcs().all(|id| module[id].notices.is_empty()) {
        return Vec::new();
    }
    let reached = reached(module);
    let mut said = Vec::new();
    for id in module.funcs() {
        let func = &module[id];
        if func.is_declaration() || !reached.contains(&func.name) {
            continue;
        }
        for inst in func.blocks().flat_map(|block| func.insts(block)) {
            let Extra::Call(info) = func[inst].extra else { continue };
            let Some(callee) = func[info].callee else { continue };
            let Some(SymbolRef::Func(target)) = module.lookup(callee) else { continue };
            let target = &module[target];
            let name = names.resolve(target.spelled.unwrap_or(target.name));
            let span = func.span(inst);
            if let Some(message) = &target.notices.error {
                let what = format!("call to '{name}' declared with attribute error: {message}");
                said.push(Diagnostic::error(what, span).with_code(CODE));
            }
            if let Some(message) = &target.notices.warning {
                let what = format!("call to '{name}' declared with attribute warning: {message}");
                said.push(Diagnostic::warning(what, span).with_code(CODE));
            }
        }
    }
    said
}

/// The names of the functions the program can still reach, which is every one another unit may
/// call and everything those call or take the address of.
///
/// A function with internal linkage is reached only through something else that is. Anything a
/// global's image or an alias names counts as reached, which errs toward reporting: a table of
/// pointers nothing reads still puts the functions in it in the object file.
fn reached(module: &Module) -> Set<Symbol> {
    let mut reached = Set::default();
    let mut work: Vec<Symbol> = Vec::new();
    for id in module.funcs() {
        let func = &module[id];
        if func.linkage != Linkage::Internal || func.attrs.set.contains(AttrSet::USED) {
            work.push(func.name);
        }
    }
    for id in module.globals() {
        let init = module[id].init.map(|list| &module[list]).unwrap_or_default();
        for datum in init {
            if let Datum::Addr(reloc) | Datum::Away(reloc) | Datum::Apart { to: reloc, .. } = *datum
            {
                work.push(module[reloc].symbol);
            }
        }
    }
    work.extend(module.aliases().map(|id| module[id].target));
    while let Some(name) = work.pop() {
        if !reached.insert(name) {
            continue;
        }
        let Some(SymbolRef::Func(id)) = module.lookup(name) else { continue };
        work.extend(named_by(&module[id]));
    }
    reached
}

/// Every name a function's body calls or takes the address of.
fn named_by(func: &Func) -> impl Iterator<Item = Symbol> + '_ {
    func.blocks().flat_map(|block| func.insts(block)).filter_map(|inst| match func[inst].extra {
        Extra::Call(info) => func[info].callee,
        Extra::Symbol(name) => Some(name),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use rucc_base::Interner;
    use rucc_ir::SymbolRef;

    use super::surviving_calls;

    const TEXT: &str = r#"; ModuleID = 't.c'
; format 0
target triple = "x86_64-unknown-linux-gnu"
target datalayout = "e-p:64:64-i64:64-f80:128-S128"

func @bad(), linkage(external);

func @meh(), linkage(external);

func @left(), linkage(internal) {
block0:
    call @bad() : ()
    return
}

func @kept(), linkage(internal) {
block0:
    call @meh() : ()
    return
}

func @g(), linkage(external) {
block0:
    call @kept() : ()
    call @bad() : ()
    return
}
"#;

    /// What is said about the module above once `bad` carries an error and `meh` a warning.
    fn said() -> Vec<String> {
        let mut names = Interner::new();
        let mut module = rucc_ir::parse(TEXT, &mut names).expect("the fixture is IR");
        for (name, error) in [("bad", true), ("meh", false)] {
            let Some(SymbolRef::Func(id)) = module.lookup(names.intern(name)) else {
                panic!("{name} is declared");
            };
            let message = Some(format!("no {name}"));
            if error {
                module[id].notices.error = message;
            } else {
                module[id].notices.warning = message;
            }
        }
        surviving_calls(&module, &names).into_iter().map(|diag| diag.message).collect()
    }

    /// `left` is a `static` function nothing reaches, which gcc does not emit, so the call in it is
    /// not one the program makes. `kept` is reached from `g`, so its call is.
    #[test]
    fn a_call_is_reported_only_in_a_function_the_program_can_reach() {
        assert_eq!(
            said(),
            [
                "call to 'meh' declared with attribute warning: no meh",
                "call to 'bad' declared with attribute error: no bad",
            ]
        );
    }
}
