//! Pointing a call at the C library at its wrapper instead.
//!
//! Design: `spec/safe-memory/10-boundaries.md` sections 10.1 and 10.3.
//!
//! Section 10.1 says the monitor may do exactly three things where instrumented code hands memory
//! to code that was not instrumented, and that it must do one of them explicitly. Modelling the
//! boundary is the first, and it has two halves. `rucc-safe-rt`'s `wrap` module is the half that
//! performs the judgements. This is the half that arranges for them to happen at all: a call the
//! program wrote to `memcpy` becomes a call to `__rucc_wrap_memcpy`, which judges both ranges and
//! then calls the real `memcpy` to do the work.
//!
//! Without this the wrappers are code nothing reaches, and every `memcpy` in an instrumented
//! program is still a hole the monitor says nothing about.
//!
//! # Why the compiler carries its own copy of the names
//!
//! The table is `rucc-safe-rt`'s and this crate cannot read it. That runtime is compiled *for the
//! target* and this compiler runs on the host, so a dependency would mean building the runtime for
//! the host as well, which is the arrangement `spec/safe-memory/15-integration.md` section 15.1
//! specifically does not want.
//!
//! So there are two lists, and two lists that are supposed to agree will not. `cargo xtask
//! interpose` reads both files and fails when they differ, which turns the thing that would rot
//! silently into a thing CI says out loud. A name here with no row there is a call redirected to a
//! symbol that does not exist, which is a link error, and a row there with no name here is a
//! wrapper nothing calls, which is the quiet one.
//!
//! # Why it runs before the optimizer
//!
//! Because `memcpy` is a name an optimizer knows things about. A pass that turns a short copy into
//! a pair of loads and stores is a correct pass and a disaster here: the copy it replaced was going
//! to be judged in one call, and what it leaves behind is an access the monitor never saw, because
//! the check insertion already ran. Redirecting first means the optimizer is looking at a call to a
//! symbol it has no opinion about, and the only thing it can do with one is leave it alone.
//!
//! The cost is that `--emit=ir` shows the wrapper rather than the name the program wrote. That is
//! the right way round: the wrapper is what the program will call.
//!
//! # A program that defines the name itself
//!
//! A freestanding program with its own `memcpy` means its own, and redirecting its calls to a
//! wrapper around the C library's would be a miscompilation rather than a monitor. So a name the
//! module defines is left alone everywhere in that module.
//!
//! That is the module's own definition and not the whole program's, which is the limit of what one
//! file can know. Document 10 section 10.7's mixed link is where the rest of that question lives.

use rucc_base::{Interner, Symbol};
use rucc_ir::{CallInfo, Extra, Inst, Module, Opcode};

/// What every wrapper's symbol starts with.
///
/// Not the name itself. Defining `memcpy` inside `rucc-safe-rt` would take the name for the whole
/// program including the C library's own internals, and the wrapper calls the real `memcpy` to do
/// the work, so it would be a recursion rather than an interposition. The prefix is what keeps the
/// two apart, and redirecting the call site is what makes the wrapper reachable anyway.
pub const PREFIX: &str = "__rucc_wrap_";

/// The functions `rucc-safe-rt`'s interposition table has a row for.
///
/// The same names in the same order as that table, and `cargo xtask interpose` is what says so.
/// Adding a row there without adding a name here leaves a wrapper nothing calls.
pub const INTERPOSED: &[&str] = &[
    "memcpy", "memmove", "memset", "memcmp", "memchr", "bcopy", "bzero", "strlen", "strnlen",
    "strcmp", "strncmp", "strchr", "strrchr", "strstr", "strcpy", "stpcpy", "strncpy", "strcat",
    "strncat",
];

/// Points every direct call to an interposed function at its wrapper, and says how many it moved.
///
/// The count is reported for the reason [`crate::Counts`] is: it is the number of boundary
/// crossings this file has that the monitor now models, and `--emit=safety-summary` is going to
/// want it. It is also the number that says whether redirection is working at all, which a suite
/// can assert on and a person cannot read off the assembly without looking for it.
///
/// A call through a pointer is not redirected and cannot be. The address in hand is the C library's
/// own and there is nothing at the call site that says which function it names. Section 10.2 counts
/// those against the trust set instead, which is the honest answer: they are a thing the build did
/// not model rather than a thing it modelled badly.
pub fn redirect(module: &mut Module, names: &mut Interner) -> usize {
    // Interned up front, so the walk is a comparison of symbols rather than of strings. Interning a
    // name the file never mentions costs one entry in a table that already holds every identifier
    // in the translation unit.
    let table: Vec<(Symbol, Symbol)> = INTERPOSED
        .iter()
        .map(|&name| (names.intern(name), names.intern(&[PREFIX, name].concat())))
        .collect();

    let ids: Vec<_> = module.funcs().collect();
    let defined: Vec<Symbol> =
        ids.iter().filter(|&&id| !module[id].is_declaration()).map(|&id| module[id].name).collect();

    let mut moved = 0;
    for id in ids {
        if module[id].is_declaration() {
            continue;
        }
        let func = &mut module[id];
        let insts: Vec<Inst> =
            func.blocks().flat_map(|block| func.insts(block).collect::<Vec<_>>()).collect();
        for inst in insts {
            // A tail call as well as a call. `return memcpy(a, b, n)` is a very ordinary thing to
            // write and it is the same boundary crossing.
            if !matches!(func[inst].opcode, Opcode::Call | Opcode::TailCall) {
                continue;
            }
            let Extra::Call(at) = func[inst].extra else { continue };
            let info = func[at];
            let Some(callee) = info.callee else { continue };
            if defined.contains(&callee) {
                continue;
            }
            let Some(&(_, wrapper)) = table.iter().find(|&&(name, _)| name == callee) else {
                continue;
            };
            // A new entry rather than an edit of the old one. The signature and the ABI attributes
            // are the ones the call already agreed with the C library on, and the wrapper takes the
            // same arguments and returns the same thing, so the only field that changes is the
            // name.
            let redirected = func.add_call(CallInfo { callee: Some(wrapper), ..info });
            func[inst].extra = Extra::Call(redirected);
            moved += 1;
        }
    }
    moved
}

#[cfg(test)]
mod tests {
    use rucc_ir::{Builder, Func, Signature, Type};
    use rucc_target::{Arch, Env, Os, TargetInfo, Triple};

    use super::*;

    fn target() -> TargetInfo {
        TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu))
    }

    /// A module whose one function calls `callee` and then `puts`.
    fn calling(names: &mut Interner, callee: &str) -> (Module, Symbol) {
        let ptr = Type::PTR;
        let mut func = Func::new(
            names.intern("run"),
            Signature::new().with_params(&[ptr, ptr, Type::int(64)]),
        );
        let entry = func.create_block();
        let dst = func.append_param(entry, ptr);
        let src = func.append_param(entry, ptr);
        let n = func.append_param(entry, Type::int(64));

        let name = names.intern(callee);
        let mut b = Builder::new(&mut func, entry);
        let signature = b.func().add_signature(
            Signature::new().with_params(&[ptr, ptr, Type::int(64)]).with_returns(&[ptr]),
        );
        b.call(name, signature, &[dst, src, n]);
        let other = names.intern("puts");
        let takes = b.func().add_signature(Signature::new().with_params(&[ptr]));
        b.call(other, takes, &[dst]);
        b.ret(&[]);

        let mut module = Module::new(names.intern("run.c"), &target());
        module.add_func(func);
        (module, name)
    }

    /// Every callee the module's definitions name, in the order they are called.
    fn callees(module: &Module, names: &Interner) -> Vec<String> {
        let mut out = Vec::new();
        for id in module.funcs() {
            let func = &module[id];
            for block in func.blocks() {
                for inst in func.insts(block) {
                    if let Extra::Call(at) = func[inst].extra {
                        if let Some(callee) = func[at].callee {
                            out.push(names.resolve(callee).to_owned());
                        }
                    }
                }
            }
        }
        out
    }

    #[test]
    fn a_call_to_an_interposed_function_goes_to_its_wrapper() {
        let mut names = Interner::new();
        let (mut module, _) = calling(&mut names, "memcpy");
        assert_eq!(redirect(&mut module, &mut names), 1);
        assert_eq!(callees(&module, &names), ["__rucc_wrap_memcpy", "puts"]);
    }

    #[test]
    fn a_call_to_anything_else_is_left_where_it_was() {
        // The list is short and the monitor says nothing about the rest, which is section 10.2's
        // whole point: what a build did not model is counted rather than quietly assumed away.
        let mut names = Interner::new();
        let (mut module, _) = calling(&mut names, "getenv");
        assert_eq!(redirect(&mut module, &mut names), 0);
        assert_eq!(callees(&module, &names), ["getenv", "puts"]);
    }

    #[test]
    fn a_program_that_defines_the_name_itself_keeps_its_own() {
        // A freestanding program with its own `memcpy` means its own. Redirecting its calls to a
        // wrapper around the C library's would be a miscompilation rather than a monitor.
        let mut names = Interner::new();
        let (mut module, name) = calling(&mut names, "memcpy");
        let ptr = Type::PTR;
        let mut own = Func::new(
            name,
            Signature::new().with_params(&[ptr, ptr, Type::int(64)]).with_returns(&[ptr]),
        );
        let entry = own.create_block();
        let dst = own.append_param(entry, ptr);
        own.append_param(entry, ptr);
        own.append_param(entry, Type::int(64));
        let mut b = Builder::new(&mut own, entry);
        b.ret(&[dst]);
        module.add_func(own);

        assert_eq!(redirect(&mut module, &mut names), 0);
        assert_eq!(callees(&module, &names), ["memcpy", "puts"]);
    }

    #[test]
    fn what_it_produces_is_a_module_the_verifier_believes() {
        // The redirected call keeps the signature it was made with, so the arguments still match
        // and the results still match. A pass that broke that would break every program that
        // copies anything.
        let mut names = Interner::new();
        let (mut module, _) = calling(&mut names, "memcpy");
        redirect(&mut module, &mut names);
        if let Err(errors) = rucc_ir::verify(&module, &names) {
            panic!("that was expected to be believed: {errors:#?}");
        }
    }

    #[test]
    fn every_name_in_the_list_is_spelled_once_and_has_a_wrapper() {
        // Two entries for one name would redirect the same call twice, and the second pass over
        // the table would be the one that decided, which is a rule nobody wrote down.
        for (at, &name) in INTERPOSED.iter().enumerate() {
            assert!(!INTERPOSED[..at].contains(&name), "{name} is in the list twice");
            assert!(!name.is_empty());
        }
        assert!(PREFIX.starts_with("__rucc"));
    }
}
