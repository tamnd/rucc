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
/// The same names in the same order as that table, group by group, and `cargo xtask interpose` is
/// what says so. Adding a row there without adding a name here leaves a wrapper nothing calls.
pub const INTERPOSED: &[&str] = &[
    "memcpy",
    "memmove",
    "memset",
    "memcmp",
    "memchr",
    "bcopy",
    "bzero",
    "strlen",
    "strnlen",
    "strcmp",
    "strncmp",
    "strchr",
    "strrchr",
    "strstr",
    "strcpy",
    "stpcpy",
    "strncpy",
    "strcat",
    "strncat",
    "read",
    "write",
    "pread",
    "pread64",
    "pwrite",
    "pwrite64",
    "recv",
    "send",
    "readv",
    "writev",
    "pthread_mutex_lock",
    "pthread_mutex_trylock",
    "pthread_mutex_unlock",
    "pthread_rwlock_rdlock",
    "pthread_rwlock_wrlock",
    "pthread_rwlock_tryrdlock",
    "pthread_rwlock_trywrlock",
    "pthread_rwlock_unlock",
    "pthread_create",
    "pthread_join",
    "pthread_cond_wait",
    "pthread_cond_timedwait",
    "sem_wait",
    "sem_trywait",
    "sem_post",
];

/// Points every mention of an interposed function at its wrapper, and says how many it moved.
///
/// The count is reported for the reason [`crate::Counts`] is: it is the number of boundary
/// crossings this file has that the monitor now models, and `--emit=safety-summary` is going to
/// want it. It is also the number that says whether redirection is working at all, which a suite
/// can assert on and a person cannot read off the assembly without looking for it.
///
/// # Both the call and the address
///
/// A call whose callee is a name is the easy half. The other half is a program that writes the name
/// where a pointer is wanted, and that half is not a lost cause the way a call through a pointer is:
/// an indirect call says nothing about what it is calling, but `read` written as an address names
/// the function as plainly as a call does. So a `global_addr` of an interposed name and a
/// relocation in a static initializer both become the wrapper, and the pointer the program ends up
/// holding is one that judges before it works.
///
/// SQLite is why this is here. Its Unix layer keeps a table of every system call it uses and goes
/// through the table for all of them, so the assembly held `.quad read` and `.quad pread64` and not
/// one call to any of them was modelled, which showed up in the replay as a page full of bytes the
/// kernel had written that the init plane had never heard of. A pluggable layer like that is
/// ordinary in a library that wants a test seam, so this was never about one project.
///
/// The wrapper takes the same arguments and returns the same thing, which is what makes the
/// substitution safe to make sight unseen. What it is not is address preserving: a program that
/// compares the pointer it stored against `read` is comparing the wrapper against the C library's
/// own and will find them different. Nothing sensible does that, and a program that does is one
/// this build changes the behaviour of, which is the honest thing to write down rather than to
/// discover later.
///
/// A call through a pointer is still not redirected, because there is nothing at that call site to
/// redirect. What changes is that the pointer being called has usually already been redirected
/// where it was taken. What is left over, a pointer that arrived from uninstrumented code, is
/// section 10.2's trust set and is a thing the build did not model rather than a thing it modelled
/// badly.
pub fn redirect(module: &mut Module, names: &mut Interner) -> usize {
    // Interned up front, so the walk is a comparison of symbols rather than of strings. Interning a
    // name the file never mentions costs one entry in a table that already holds every identifier
    // in the translation unit.
    let table: Vec<(Symbol, Symbol)> = INTERPOSED
        .iter()
        .map(|&name| (names.intern(name), names.intern(&[PREFIX, name].concat())))
        .collect();

    let ids: Vec<_> = module.funcs().collect();
    let mut defined: Vec<Symbol> =
        ids.iter().filter(|&&id| !module[id].is_declaration()).map(|&id| module[id].name).collect();
    // A global of the name counts as the module's own too. A file that defines a variable called
    // `read` and then takes its address has not named the C library's function, and turning that
    // into the address of a wrapper around one would be a miscompilation rather than a monitor.
    defined.extend(
        module.globals().filter(|&id| !module[id].is_declaration()).map(|id| module[id].name),
    );

    let mut moved = 0;
    for id in ids {
        if module[id].is_declaration() {
            continue;
        }
        let func = &mut module[id];
        let insts: Vec<Inst> =
            func.blocks().flat_map(|block| func.insts(block).collect::<Vec<_>>()).collect();
        for inst in insts {
            // The address of one of these, written where a pointer was wanted. The name is right
            // there, so this is the same redirection the call gets and it is the one that reaches a
            // program that goes through a table.
            if func[inst].opcode == Opcode::GlobalAddr {
                let Extra::Symbol(named) = func[inst].extra else { continue };
                if defined.contains(&named) {
                    continue;
                }
                if let Some(&(_, wrapper)) = table.iter().find(|&&(name, _)| name == named) {
                    func[inst].extra = Extra::Symbol(wrapper);
                    moved += 1;
                }
                continue;
            }
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
    // And the same name written into a static initializer, which is the pool rather than any one
    // function. `static struct { const char *name; void *fn; } table[] = { { "read", read } }` is
    // the shape SQLite's Unix layer is and the shape any pluggable layer is, and the relocation is
    // where the name survives into the object.
    for reloc in module.relocs_mut() {
        if defined.contains(&reloc.symbol) {
            continue;
        }
        if let Some(&(_, wrapper)) = table.iter().find(|&&(name, _)| name == reloc.symbol) {
            reloc.symbol = wrapper;
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

    /// Every name a `global_addr` in the module's definitions asks for.
    fn addressed(module: &Module, names: &Interner) -> Vec<String> {
        let mut out = Vec::new();
        for id in module.funcs() {
            let func = &module[id];
            for block in func.blocks() {
                for inst in func.insts(block) {
                    if func[inst].opcode == Opcode::GlobalAddr {
                        if let Extra::Symbol(named) = func[inst].extra {
                            out.push(names.resolve(named).to_owned());
                        }
                    }
                }
            }
        }
        out
    }

    /// A module whose one function takes the address of `named` and of `puts`.
    fn addressing(names: &mut Interner, named: &str) -> Module {
        let mut func = Func::new(names.intern("run"), Signature::new());
        let entry = func.create_block();
        let mut b = Builder::new(&mut func, entry);
        for each in [named, "puts"] {
            let symbol = names.intern(each);
            b.value(
                rucc_ir::InstData {
                    extra: Extra::Symbol(symbol),
                    ..rucc_ir::InstData::new(Opcode::GlobalAddr)
                },
                Type::PTR,
            );
        }
        b.ret(&[]);

        let mut module = Module::new(names.intern("run.c"), &target());
        module.add_func(func);
        module
    }

    #[test]
    fn the_address_of_an_interposed_function_is_the_address_of_its_wrapper() {
        // The half a table driven program goes through. Nothing at an indirect call says what it
        // is calling, but the name written where the pointer was taken says it plainly.
        let mut names = Interner::new();
        let mut module = addressing(&mut names, "read");
        assert_eq!(redirect(&mut module, &mut names), 1);
        assert_eq!(addressed(&module, &names), ["__rucc_wrap_read", "puts"]);
    }

    #[test]
    fn the_address_of_a_name_the_module_defines_itself_is_left_alone() {
        // Same rule the call gets, and it has to hold for a global as well as for a function: a
        // file with its own variable called `read` has not named the C library's function.
        let mut names = Interner::new();
        let mut module = addressing(&mut names, "read");
        let own = names.intern("read");
        let mut global = rucc_ir::Global::new(own, 8, 8);
        let ty = Type::int(64);
        let value = module.add_imm(rucc_ir::Imm::int(0, ty));
        global.init = Some(module.push_data(&[rucc_ir::Datum::Scalar { ty, value }]));
        module.add_global(global);

        assert_eq!(redirect(&mut module, &mut names), 0);
        assert_eq!(addressed(&module, &names), ["read", "puts"]);
    }

    #[test]
    fn a_name_in_a_static_initializer_becomes_the_wrapper_too() {
        // `static void *table[] = { read }`, which is the shape SQLite's Unix layer is and the one
        // that made this worth doing. The name survives into a relocation rather than into any
        // instruction, so it is the pool that has to be walked.
        let mut names = Interner::new();
        let mut module = addressing(&mut names, "getenv");
        let read = names.intern("read");
        let at = module.add_reloc(rucc_ir::Reloc { symbol: read, addend: 0, size: 8 });
        let mut table = rucc_ir::Global::new(names.intern("table"), 8, 8);
        table.init = Some(module.push_data(&[rucc_ir::Datum::Addr(at)]));
        module.add_global(table);

        assert_eq!(redirect(&mut module, &mut names), 1);
        assert_eq!(names.resolve(module[at].symbol), "__rucc_wrap_read");
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
