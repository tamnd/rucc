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
use rucc_ir::{Linkage, Module, Pic, Visibility};
use rucc_target::ObjectFormat;

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
/// Both ways in are shut on a format with no such table, which is COFF. See `Self::table` for why
/// the question has a different answer there rather than no answer.
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
    described: bool,
}

impl Elsewhere {
    /// The names that link cannot reach from the instruction pointer.
    #[must_use]
    pub fn of(module: &Module, pic: Pic, format: ObjectFormat) -> Self {
        let threads = module
            .globals()
            .filter(|&id| module[id].tls.is_some())
            .map(|id| module[id].name)
            .collect();
        let described = format == ObjectFormat::MachO;
        Self { threads, described, ..Self::table(module, pic, format) }
    }

    /// The half of the above that is about the global offset table, which is the older one.
    ///
    /// Empty on a format that has no such table. COFF is the one, and it is not that the question
    /// goes unanswered there: a name this file only declares is reached from the instruction
    /// pointer like any other, because whatever supplies it supplies a piece of this image to
    /// measure to. A name the link resolves out of another object is in the image, and a name that
    /// comes from a DLL arrives through an import library, which is an archive member holding a
    /// jump under the plain name, so the name still stands for an address in this image and every
    /// object that takes it gets the one the linker kept. Measured against gcc 13.2 for
    /// `x86_64-w64-mingw32`, which writes `leaq other(%rip), %rax` for the address of a function it
    /// has only seen declared. Asking for a table there instead reached the object writer as a
    /// relocation it has no way to write, which is what tamnd/rucc#1443 was.
    fn table(module: &Module, pic: Pic, format: ObjectFormat) -> Self {
        if format == ObjectFormat::Coff {
            return Self::default();
        }
        let funcs = module.funcs().filter(|&id| {
            let func = &module[id];
            func.is_declaration() || pic.replaceable(func.linkage, func.visibility)
        });
        // A weak variable nothing here defines is the one variable the copying above does not
        // cover, since there may be no definition anywhere to copy and then its address is null. The
        // distance from here to null is not a number the linker has, so lld refuses the
        // `R_X86_64_PC32` and gcc reads the address out of a slot, which the linker fills with zero.
        //
        // Mach-O does no copying at all. `dyld` has no copy relocation, so a variable a library
        // defines stays in the library and the only way to it is the slot. That is every variable
        // this file only declares, unless it is hidden and so promised to be in the same image,
        // and it is what clang writes: `_ext@GOTPAGE` on arm64 and `_ext@GOTPCREL` on x86-64.
        let uncopied = format == ObjectFormat::MachO;
        let globals = module
            .globals()
            .filter(|&id| {
                let global = &module[id];
                (global.is_declaration()
                    && (global.linkage == Linkage::Weak
                        || (uncopied && global.visibility == Visibility::Default)))
                    || pic.replaceable(global.linkage, global.visibility)
            })
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

    /// Whether a thread-local variable is reached by calling through its descriptor, which is how
    /// Mach-O does it on both architectures.
    ///
    /// The slot the table holds for such a variable is the address of the descriptor rather than an
    /// offset from the thread pointer, and the first word of the descriptor is a function that takes
    /// that address and gives back this thread's copy. So there is no thread pointer to add to,
    /// and the answer is the value the call returns.
    #[must_use]
    pub const fn described(&self) -> bool {
        self.described
    }
}

/// The same set, written out by hand.
///
/// [`Elsewhere::of`] is how the driver builds one and is the only way a compilation does. This is
/// for a test that wants to lower one function and say what is outside the file without building a
/// module for it to be outside of.
impl FromIterator<Symbol> for Elsewhere {
    fn from_iter<T: IntoIterator<Item = Symbol>>(names: T) -> Self {
        Self { names: names.into_iter().collect(), threads: HashSet::new(), described: false }
    }
}

impl Elsewhere {
    /// The same set with those names said to be thread-local, for a test that lowers one function.
    #[must_use]
    pub fn with_threads<T: IntoIterator<Item = Symbol>>(mut self, threads: T) -> Self {
        self.threads = threads.into_iter().collect();
        self
    }

    /// The same set with thread-locals reached through a descriptor, for a test that lowers one
    /// function the way Mach-O would.
    #[must_use]
    pub const fn with_descriptors(mut self) -> Self {
        self.described = true;
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
        let elsewhere = Elsewhere::of(&module, Pic::Executable, ObjectFormat::Elf);
        assert!(elsewhere.thread(names.intern("own")));
    }

    /// The question the other five ask is a different question, and a variable that is not
    /// thread-local answering yes to this one would put an offset where an address belongs.
    #[test]
    fn an_ordinary_variable_is_not() {
        let mut names = Interner::new();
        let module = module(&mut names);
        let elsewhere = Elsewhere::of(&module, Pic::Executable, ObjectFormat::Elf);
        for name in ["kept", "away", "quiet", "shy", "here"] {
            assert!(!elsewhere.thread(names.intern(name)), "{name} was called thread-local");
        }
    }

    #[test]
    fn a_function_this_file_only_declares_is_reached_through_the_table() {
        let mut names = Interner::new();
        let module = module(&mut names);
        let elsewhere = Elsewhere::of(&module, Pic::Executable, ObjectFormat::Elf);
        assert!(elsewhere.holds(names.intern("exit")));
    }

    #[test]
    fn a_function_this_file_defines_is_not() {
        let mut names = Interner::new();
        let module = module(&mut names);
        let elsewhere = Elsewhere::of(&module, Pic::Executable, ObjectFormat::Elf);
        assert!(!elsewhere.holds(names.intern("here")));
    }

    #[test]
    fn a_name_the_module_does_not_carry_at_all_is_not() {
        let mut names = Interner::new();
        let module = module(&mut names);
        let elsewhere = Elsewhere::of(&module, Pic::Executable, ObjectFormat::Elf);
        assert!(!elsewhere.holds(names.intern("nowhere")));
    }

    /// The whole of what an executable pays, which is one entry for the one function it calls in a
    /// library. Every variable is reached from the instruction pointer, the one it does not define
    /// included, because the linker copies that one in here.
    #[test]
    fn an_executable_pays_for_the_functions_and_for_nothing_else() {
        let mut names = Interner::new();
        let module = module(&mut names);
        let elsewhere = Elsewhere::of(&module, Pic::Executable, ObjectFormat::Elf);
        for name in ["kept", "away", "quiet", "shy", "second"] {
            assert!(!elsewhere.holds(names.intern(name)), "{name} was in the table");
        }
    }

    /// A weak variable nothing defines may be at zero, which no distance from the code reaches.
    #[test]
    fn a_weak_variable_this_file_only_declares_is_reached_through_the_table() {
        let mut names = Interner::new();
        let mut module = module(&mut names);
        let mut maybe = Global::new(names.intern("maybe"), 4, 4);
        maybe.linkage = Linkage::Weak;
        module.add_global(maybe);
        let elsewhere = Elsewhere::of(&module, Pic::Executable, ObjectFormat::Elf);
        assert!(elsewhere.holds(names.intern("maybe")));
    }

    /// Mach-O never copies a variable into the executable, so the one this file only declares is
    /// read through the table even in a program, and the ones it defines are still reached
    /// directly.
    #[test]
    fn a_mach_o_executable_pays_for_the_variables_it_does_not_define_as_well() {
        let mut names = Interner::new();
        let mut module = module(&mut names);
        let mut near = Global::new(names.intern("near"), 4, 4);
        near.visibility = Visibility::Hidden;
        module.add_global(near);
        let elsewhere = Elsewhere::of(&module, Pic::Executable, ObjectFormat::MachO);
        assert!(elsewhere.holds(names.intern("away")));
        for name in ["kept", "quiet", "shy", "near"] {
            assert!(!elsewhere.holds(names.intern(name)), "{name} was in the table");
        }
    }

    /// A library pays for every name it exports, defined here or not, because the definition the
    /// process uses may be in another object however plainly this file defines it.
    #[test]
    fn a_library_pays_for_every_name_something_else_may_define() {
        let mut names = Interner::new();
        let module = module(&mut names);
        let elsewhere = Elsewhere::of(&module, Pic::Library, ObjectFormat::Elf);
        for name in ["here", "exit", "kept", "away", "second"] {
            assert!(elsewhere.holds(names.intern(name)), "{name} was not in the table");
        }
    }

    /// A format with no table asks nothing of anybody, which is not the same as asking and being
    /// told no. The name of a function this file only declares stands for an address in the image
    /// on this format whether the link finds it in another object or in an import library, so the
    /// instruction pointer reaches it and there is nothing left over to put in a table. gcc writes
    /// the same `leaq other(%rip)` for the same declaration.
    #[test]
    fn a_format_with_no_table_puts_nothing_in_one() {
        let mut names = Interner::new();
        let module = module(&mut names);
        let elsewhere = Elsewhere::of(&module, Pic::Executable, ObjectFormat::Coff);
        for name in ["here", "exit", "kept", "away", "quiet", "shy", "second"] {
            assert!(!elsewhere.holds(names.intern(name)), "{name} was in the table");
        }
    }

    /// And the flag that fills the table on the other format does not fill it here either, since
    /// there is no interposition on this one for it to be about.
    #[test]
    fn a_format_with_no_table_does_not_grow_one_under_the_library_flag() {
        let mut names = Interner::new();
        let module = module(&mut names);
        let elsewhere = Elsewhere::of(&module, Pic::Library, ObjectFormat::Coff);
        for name in ["here", "exit", "kept", "away", "second"] {
            assert!(!elsewhere.holds(names.intern(name)), "{name} was in the table");
        }
    }

    /// The other question this type answers is not the table's, so it keeps its answer whatever the
    /// format. What a target with no thread-local storage does about it is the writer's refusal
    /// rather than a name quietly left out here.
    #[test]
    fn a_format_with_no_table_still_says_which_variable_every_thread_has_a_copy_of() {
        let mut names = Interner::new();
        let module = module(&mut names);
        let elsewhere = Elsewhere::of(&module, Pic::Executable, ObjectFormat::Coff);
        assert!(elsewhere.thread(names.intern("own")));
    }

    /// And not for the names nothing outside can reach, which is what makes `-fvisibility=hidden`
    /// worth writing next to it.
    #[test]
    fn a_library_pays_nothing_for_a_name_nothing_outside_it_can_see() {
        let mut names = Interner::new();
        let module = module(&mut names);
        let elsewhere = Elsewhere::of(&module, Pic::Library, ObjectFormat::Elf);
        assert!(!elsewhere.holds(names.intern("quiet")));
        assert!(!elsewhere.holds(names.intern("shy")));
    }
}
