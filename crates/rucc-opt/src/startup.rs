//! Which functions run only once, when the program starts or when it stops.
//!
//! gcc marks `main` and every constructor as run at startup and every destructor as run at exit,
//! and from `-O2` it puts the first kind in `.text.startup` and the second in `.text.exit`. The
//! kernel reads those names: its linker script keeps `.text.startup` with the init code and
//! `.text.exit` with the exit code, and `arch/x86/boot/main.o` is one object where it shows.
//!
//! A constructor is a function whose address is in `.init_array` and a destructor one whose
//! address is in `.fini_array`, numbered or not, which is how the front end writes both
//! attributes. That is all gcc goes by too: it does not follow the calls out of them without a
//! profile.

use rucc_base::Interner;
use rucc_base::hash::Set;
use rucc_ir::{Datum, FuncId, Module, SymbolRef};

/// The functions that run at startup and the ones that run at exit.
#[derive(Debug, Default)]
pub struct Startup {
    /// `main` and the constructors.
    pub start: Set<FuncId>,
    /// The destructors.
    pub exit: Set<FuncId>,
}

/// Every function in the module that runs only at startup or only at exit.
#[must_use]
pub fn functions(module: &Module, names: &Interner) -> Startup {
    let mut out = Startup::default();
    for id in module.funcs() {
        if !module[id].is_declaration() && names.resolve(module[id].name) == "main" {
            out.start.insert(id);
        }
    }
    for id in module.globals() {
        let global = &module[id];
        let Some(section) = global.section.map(|s| names.resolve(s)) else { continue };
        let into = if is_table(section, ".init_array") {
            &mut out.start
        } else if is_table(section, ".fini_array") {
            &mut out.exit
        } else {
            continue;
        };
        for datum in global.init.map(|list| &module[list]).unwrap_or_default() {
            let Datum::Addr(reloc) = *datum else { continue };
            if let Some(SymbolRef::Func(func)) = module.lookup(module[reloc].symbol) {
                if !module[func].is_declaration() {
                    into.insert(func);
                }
            }
        }
    }
    out
}

/// Whether a section is that table, or one of its numbered parts.
fn is_table(section: &str, table: &str) -> bool {
    section.strip_prefix(table).is_some_and(|rest| rest.is_empty() || rest.starts_with('.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_numbered_part_is_the_table_and_a_longer_name_is_not() {
        assert!(is_table(".init_array", ".init_array"));
        assert!(is_table(".init_array.00100", ".init_array"));
        assert!(!is_table(".init_arrays", ".init_array"));
        assert!(!is_table(".fini_array", ".init_array"));
    }
}
