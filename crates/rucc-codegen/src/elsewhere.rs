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

use std::collections::HashSet;

use rucc_base::Symbol;
use rucc_ir::Module;

/// The names whose address only the linker knows.
///
/// Functions and not objects, which is not an oversight. The address of a `static` or of anything
/// this file defines is in this file, so there is a distance and no table is needed. The address of
/// an object another file defines is also reachable that way in an executable, because the linker
/// answers a reference to one by making room for it in this program and copying it there, so the
/// name really does end up somewhere this file can measure to. A function cannot be copied: it has
/// exactly one address that every object in the program has to agree on, or two pointers to it
/// compare unequal, so the one address is what the table holds and what everything reads.
///
/// A name this module has never heard of is not in here. Nothing the front end writes produces one,
/// and treating an unknown name as a function would put the addresses the instrumentation takes of
/// its own tables through a table of their own for no reason.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Elsewhere {
    names: HashSet<Symbol>,
}

impl Elsewhere {
    /// The functions a module declares and does not define.
    #[must_use]
    pub fn of(module: &Module) -> Self {
        module.funcs().filter(|&id| module[id].is_declaration()).map(|id| module[id].name).collect()
    }

    /// Whether the address of that name has to be read out of the global offset table.
    #[must_use]
    pub fn holds(&self, name: Symbol) -> bool {
        self.names.contains(&name)
    }
}

/// The same set, written out by hand.
///
/// [`Elsewhere::of`] is how the driver builds one and is the only way a compilation does. This is
/// for a test that wants to lower one function and say what is outside the file without building a
/// module for it to be outside of.
impl FromIterator<Symbol> for Elsewhere {
    fn from_iter<T: IntoIterator<Item = Symbol>>(names: T) -> Self {
        Self { names: names.into_iter().collect() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use rucc_base::Interner;
    use rucc_ir::{Func, Signature};
    use rucc_target::{Arch, Env, Os, TargetInfo, Triple};

    /// A module of one function with a body and one without.
    fn module(names: &mut Interner) -> Module {
        let target = TargetInfo::new(Triple::new(Arch::X86_64, Os::Linux, Env::Gnu));
        let mut module = Module::new(names.intern("test.c"), &target);
        let mut defined = Func::new(names.intern("here"), Signature::new());
        defined.create_block();
        module.add_func(defined);
        module.add_func(Func::new(names.intern("exit"), Signature::new()));
        module
    }

    #[test]
    fn a_function_this_file_only_declares_is_reached_through_the_table() {
        let mut names = Interner::new();
        let module = module(&mut names);
        let elsewhere = Elsewhere::of(&module);
        assert!(elsewhere.holds(names.intern("exit")));
    }

    #[test]
    fn a_function_this_file_defines_is_not() {
        let mut names = Interner::new();
        let module = module(&mut names);
        let elsewhere = Elsewhere::of(&module);
        assert!(!elsewhere.holds(names.intern("here")));
    }

    #[test]
    fn a_name_the_module_does_not_carry_at_all_is_not() {
        let mut names = Interner::new();
        let module = module(&mut names);
        let elsewhere = Elsewhere::of(&module);
        assert!(!elsewhere.holds(names.intern("nowhere")));
    }
}
