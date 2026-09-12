//! Which names this file may not work the address of out for itself.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.3.
//!
//! Everything this compiler emits is position independent, so the address of a name is the distance
//! from the instruction asking to the name, and that distance is a number the assembler leaves a
//! hole for and the linker fills in. The linker can only fill it in when it is putting both ends in
//! the same program. A name this file only declares may turn out to be in a shared library, and
//! then there is no such distance and the link fails rather than guessing one.
//!
//! The way round it is a table: the linker gives the name one slot in the global offset table, fills
//! the slot with whatever address the name ends up at, and the code loads the address out of the
//! slot instead of working it out. The slot is in this program, so the distance to the slot is a
//! number the linker has. It costs a load, and the linker takes the load back out again when the
//! name turns out to have been in this program all along.
//!
//! Which names need it is a fact about the whole module and the code generator sees one function at
//! a time, which is why this is worked out first and handed in rather than asked at the point of
//! use.
//!
//! It is also a fact about which link is coming, which is [`rucc_ir::Pic`] and is why this is built
//! from more than the module. Under `-fPIC` the link may be one that produces a shared library, and
//! then a name this file exports is one the dynamic linker may find a different definition of, so
//! reaching it from the instruction pointer would reach the wrong one. The static linker will not
//! let that happen quietly: `R_X86_64_PC32` against a name it can see is replaceable is refused
//! when it is making a shared object, which is how tamnd/rucc#756 was found.
//!
//! A thread-local variable is the other name this file cannot work the address of out for itself,
//! and it is here for the same reason: which names are thread-local is a fact about the module and
//! the code generator sees one function at a time. It is a harder case than the one above rather
//! than a variation of it, because there is no address to work out at all. Every thread has its own
//! copy, so what the link can say is only where the variable sits inside the block a thread gets,
//! and turning that into an address is something the running program does. See [`Elsewhere::thread`].

use std::collections::HashSet;

use rucc_base::Symbol;
use rucc_ir::{Module, Pic};

/// The names whose address only the linker knows.
///
/// Two ways in, and the first one holds whichever link is coming. A function this file only
/// declares is one, because a function cannot be copied: it has exactly one address that every
/// object in the program has to agree on, or two pointers to it compare unequal, so the one address
/// is what the table holds and what everything reads. A variable can be copied, and in an
/// executable it is, since the linker answers a reference to one another object defines by making
/// room for it here and copying it there, so the name really does end up somewhere this file can
/// measure to.
///
/// The second way in is `-fPIC`, where the link may be one that produces a shared library and the
/// copying does not happen. There every replaceable name is in here, defined or not and function or
/// variable, because the definition the process ends up using may be in another object however
/// plainly this file defines it. What is not in here is what `-fPIC` costs nothing for: a `static`,
/// and a name marked hidden or protected, which is the reason `-fPIC -fvisibility=hidden` is the
/// combination a library that cares about its own speed is built with.
///
/// A name this module has never heard of is not in here. Nothing the front end writes produces one,
/// and treating an unknown name as a function would put the addresses the instrumentation takes of
/// its own tables through a table of their own for no reason.
///
/// A thread-local variable is kept separately and answered by [`Self::thread`], because the two
/// questions have different answers rather than one being a case of the other: the table slot of an
/// ordinary name holds its address and the slot of a thread-local holds an offset, and reading
/// either as though it were the other is a wrong answer rather than a slower one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Elsewhere {
    names: HashSet<Symbol>,
    threads: HashSet<Symbol>,
}

impl Elsewhere {
    /// The names that link cannot reach from the instruction pointer.
    #[must_use]
    pub fn of(module: &Module, pic: Pic) -> Self {
        let threads = module
            .globals()
            .filter(|&id| module[id].tls.is_some())
            .map(|id| module[id].name)
            .collect();
        Self { threads, ..Self::table(module, pic) }
    }

    /// The half of the above that is about the global offset table, which is the older one.
    fn table(module: &Module, pic: Pic) -> Self {
        let funcs = module.funcs().filter(|&id| {
            let func = &module[id];
            func.is_declaration() || pic.replaceable(func.linkage, func.visibility)
        });
        let globals = module
            .globals()
            .filter(|&id| pic.replaceable(module[id].linkage, module[id].visibility))
            .map(|id| module[id].name);
        // An alias is a symbol of its own with a linkage and a visibility of its own, so it answers
        // this for itself the same way it answered the visibility question in #752. What it points
        // at is a separate name and is decided separately, which is what `weak, alias,
        // visibility("hidden")` over an exported definition needs.
        let aliases = module
            .aliases()
            .filter(|&id| pic.replaceable(module[id].linkage, module[id].visibility))
            .map(|id| module[id].name);
        funcs.map(|id| module[id].name).chain(globals).chain(aliases).collect()
    }

    /// Whether the address of that name has to be read out of the global offset table.
    #[must_use]
    pub fn holds(&self, name: Symbol) -> bool {
        self.names.contains(&name)
    }

    /// Whether that name is a variable every thread has its own copy of.
    ///
    /// Asked before [`Self::holds`] and not instead of it, because the two answers are about
    /// different things: a thread-local variable that another object may define is still reached
    /// the same way, since the table slot holds an offset that is the same for every copy and the
    /// question of whose copy is answered by the segment register rather than by the link.
    #[must_use]
    pub fn thread(&self, name: Symbol) -> bool {
        self.threads.contains(&name)
    }
}

/// The same set, written out by hand.
///
/// [`Elsewhere::of`] is how the driver builds one and is the only way a compilation does. This is
/// for a test that wants to lower one function and say what is outside the file without building a
/// module for it to be outside of.
impl FromIterator<Symbol> for Elsewhere {
    fn from_iter<T: IntoIterator<Item = Symbol>>(names: T) -> Self {
        Self { names: names.into_iter().collect(), threads: HashSet::new() }
    }
}

impl Elsewhere {
    /// The same set with those names said to be thread-local, for a test that lowers one function.
    #[must_use]
    pub fn with_threads<T: IntoIterator<Item = Symbol>>(mut self, threads: T) -> Self {
        self.threads = threads.into_iter().collect();
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use rucc_base::Interner;
    use rucc_ir::{Alias, Func, Global, Linkage, Signature, TlsModel, Visibility};
    use rucc_target::{Arch, Env, Os, TargetInfo, Triple};

    /// A module with one of everything: a function with a body and one without, a variable with an
    /// image and one without, a `static`, a hidden export, an alias and a thread-local.
    fn module(names: &mut Interner) -> Module {
        let target = TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu));
        let mut module = Module::new(names.intern("test.c"), &target);
        let mut defined = Func::new(names.intern("here"), Signature::new());
        defined.create_block();
        module.add_func(defined);
        module.add_func(Func::new(names.intern("exit"), Signature::new()));

        let mut kept = Global::new(names.intern("kept"), 4, 4);
        kept.init = Some(module.push_data(&[]));
        module.add_global(kept);
        module.add_global(Global::new(names.intern("away"), 4, 4));

        let mut quiet = Global::new(names.intern("quiet"), 4, 4);
        quiet.init = Some(module.push_data(&[]));
        quiet.linkage = Linkage::Internal;
        module.add_global(quiet);

        let mut shy = Global::new(names.intern("shy"), 4, 4);
        shy.init = Some(module.push_data(&[]));
        shy.visibility = Visibility::Hidden;
        module.add_global(shy);

        let mut own = Global::new(names.intern("own"), 4, 4);
        own.init = Some(module.push_data(&[]));
        own.tls = Some(TlsModel::GlobalDynamic);
        module.add_global(own);

        module.add_alias(Alias::new(names.intern("second"), names.intern("here")));
        module
    }

    #[test]
    fn a_variable_every_thread_has_its_own_copy_of_is_one() {
        let mut names = Interner::new();
        let module = module(&mut names);
        let elsewhere = Elsewhere::of(&module, Pic::Executable);
        assert!(elsewhere.thread(names.intern("own")));
    }

    /// The question the other five ask is a different question, and a variable that is not
    /// thread-local answering yes to this one would put an offset where an address belongs.
    #[test]
    fn an_ordinary_variable_is_not() {
        let mut names = Interner::new();
        let module = module(&mut names);
        let elsewhere = Elsewhere::of(&module, Pic::Executable);
        for name in ["kept", "away", "quiet", "shy", "here"] {
            assert!(!elsewhere.thread(names.intern(name)), "{name} was called thread-local");
        }
    }

    #[test]
    fn a_function_this_file_only_declares_is_reached_through_the_table() {
        let mut names = Interner::new();
        let module = module(&mut names);
        let elsewhere = Elsewhere::of(&module, Pic::Executable);
        assert!(elsewhere.holds(names.intern("exit")));
    }

    #[test]
    fn a_function_this_file_defines_is_not() {
        let mut names = Interner::new();
        let module = module(&mut names);
        let elsewhere = Elsewhere::of(&module, Pic::Executable);
        assert!(!elsewhere.holds(names.intern("here")));
    }

    #[test]
    fn a_name_the_module_does_not_carry_at_all_is_not() {
        let mut names = Interner::new();
        let module = module(&mut names);
        let elsewhere = Elsewhere::of(&module, Pic::Executable);
        assert!(!elsewhere.holds(names.intern("nowhere")));
    }

    /// The whole of what an executable pays, which is one entry for the one function it calls in a
    /// library. Every variable is reached from the instruction pointer, the one it does not define
    /// included, because the linker copies that one in here.
    #[test]
    fn an_executable_pays_for_the_functions_and_for_nothing_else() {
        let mut names = Interner::new();
        let module = module(&mut names);
        let elsewhere = Elsewhere::of(&module, Pic::Executable);
        for name in ["kept", "away", "quiet", "shy", "second"] {
            assert!(!elsewhere.holds(names.intern(name)), "{name} was in the table");
        }
    }

    /// A library pays for every name it exports, defined here or not, because the definition the
    /// process uses may be in another object however plainly this file defines it.
    #[test]
    fn a_library_pays_for_every_name_something_else_may_define() {
        let mut names = Interner::new();
        let module = module(&mut names);
        let elsewhere = Elsewhere::of(&module, Pic::Library);
        for name in ["here", "exit", "kept", "away", "second"] {
            assert!(elsewhere.holds(names.intern(name)), "{name} was not in the table");
        }
    }

    /// And not for the names nothing outside can reach, which is what makes `-fvisibility=hidden`
    /// worth writing next to it.
    #[test]
    fn a_library_pays_nothing_for_a_name_nothing_outside_it_can_see() {
        let mut names = Interner::new();
        let module = module(&mut names);
        let elsewhere = Elsewhere::of(&module, Pic::Library);
        assert!(!elsewhere.holds(names.intern("quiet")));
        assert!(!elsewhere.holds(names.intern("shy")));
    }
}
