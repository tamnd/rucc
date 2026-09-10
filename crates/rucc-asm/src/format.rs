//! What an assembler is told about a function or a variable, which is the object format's answer
//! rather than the machine's.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.3, which is about the object files
//! themselves. The directives here are the same facts said in text: which section code and data go
//! in, how a symbol is spelled, which symbols leave the file, and where each one ends.
//!
//! They are not the same on the three formats and the differences are not cosmetic. A Mach-O
//! symbol carries an underscore in front of the C name and an ELF one does not, so a listing that
//! got that wrong would fail to link against every library on the machine. A local label is
//! spelled `.L` on ELF and COFF and `L` on Mach-O, and a label that is not spelled the local way
//! ends up in the symbol table, where it is a name a debugger and a backtrace will show. And ELF
//! wants a marker saying the stack is not executable, whose absence makes it executable, which
//! section 11.3 calls out as a real and recurring security bug.

use std::fmt::Write as _;

use rucc_mir as mir;
use rucc_object::{Alias, Binding, Place, Sections, Visibility};
use rucc_target::ObjectFormat;

use crate::data::Variable;

/// The directives one object format wraps a function in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Directives {
    /// ELF, which is Linux and the freestanding targets.
    Elf,
    /// Mach-O, which is Apple's.
    MachO,
    /// COFF, which is Windows.
    Coff,
}

impl Directives {
    /// The directives that go with that object format.
    #[must_use]
    pub const fn of(format: ObjectFormat) -> Directives {
        match format {
            ObjectFormat::Elf => Directives::Elf,
            ObjectFormat::MachO => Directives::MachO,
            ObjectFormat::Coff => Directives::Coff,
            // No assembler in this crate writes wasm, and the caller that asked has a target it
            // cannot emit for. ELF's directives are the ones nothing here depends on being right
            // for a target it will not reach.
            ObjectFormat::Wasm => Directives::Elf,
        }
    }

    /// What goes in front of a C name to make the name the linker sees.
    ///
    /// Mach-O keeps the underscore that every Unix linker once had, so `main` in C is `_main` in
    /// the object, and a listing that leaves it off refers to a symbol nothing defines.
    #[must_use]
    pub const fn symbol(self) -> &'static str {
        match self {
            Directives::Elf | Directives::Coff => "",
            Directives::MachO => "_",
        }
    }

    /// What goes in front of a label that belongs to one function and leaves no symbol behind.
    #[must_use]
    pub const fn local(self) -> &'static str {
        match self {
            Directives::Elf | Directives::Coff => ".L",
            Directives::MachO => "L",
        }
    }

    /// The directive that opens the section code goes in.
    #[must_use]
    pub const fn text(self) -> &'static str {
        match self {
            Directives::Elf | Directives::Coff => "\t.text",
            Directives::MachO => "\t.section\t__TEXT,__text,regular,pure_instructions",
        }
    }

    /// The directive that opens the section one function goes in, and nothing at all when they
    /// are all going in the same one.
    ///
    /// Nothing on Mach-O either, whatever was asked for. Every Mach-O object ends with
    /// `.subsections_via_symbols`, which tells the linker it may split a section at each symbol in
    /// it and drop the parts nothing reaches, so the format does by default what the flag asks a
    /// linker to be able to do and there is nothing left for it to change. Clang takes both flags
    /// on an Apple target and writes one text section, which is the same answer.
    ///
    /// ELF names the section after the function and COFF gives one name to several sections and
    /// tells the linker which symbol each belongs to. The COFF form is a COMDAT, which is more
    /// than the ELF one says: a linker keeps one section out of every group that names the same
    /// symbol. That is what a Windows toolchain does with `/Gy`, and it is what clang writes for
    /// `-ffunction-sections` on a Windows target, so it is what a Windows linker is expecting.
    pub fn code(self, out: &mut String, name: &str, sections: Sections) {
        if !sections.functions {
            return;
        }
        match self {
            Directives::Elf => {
                let _ = writeln!(out, "\t.section\t.text.{name},\"ax\",@progbits");
            }
            Directives::Coff => {
                let _ = writeln!(out, "\t.section\t.text,\"xr\",one_only,{name}");
            }
            Directives::MachO => {}
        }
    }

    /// What is said about a function before its first instruction.
    ///
    /// The binding is written the way it is written for a variable, and a local one gets no
    /// directive at all: a name no directive mentions is still in the symbol table, as a local,
    /// which is what `static` is. Windows says the same thing as a storage class, where three is
    /// the local one and two the rest.
    ///
    /// `align` is in bytes and is a power of two, and the padding is `0x90` because the space in
    /// front of a function is reached by falling off the end of the one before it.
    pub fn open(
        self,
        out: &mut String,
        name: &str,
        align: u32,
        binding: Binding,
        visibility: Visibility,
    ) {
        let symbol = self.symbol();
        let _ = writeln!(out, "\t.p2align\t{}, 0x90", align.max(1).trailing_zeros());
        match binding {
            Binding::Global => {
                let _ = writeln!(out, "\t.globl\t{symbol}{name}");
            }
            Binding::Weak => {
                let _ = writeln!(out, "\t.weak\t{symbol}{name}");
            }
            Binding::Local => {}
        }
        self.seen(out, name, binding, visibility);
        match self {
            Directives::Elf => {
                let _ = writeln!(out, "\t.type\t{name}, @function");
            }
            // Windows says the storage class and the type code, and thirty two is a function.
            Directives::Coff => {
                let scl = if binding == Binding::Local { 3 } else { 2 };
                let _ = writeln!(out, "\t.def\t{name}\n\t.scl\t{scl}\n\t.type\t32\n\t.endef");
            }
            Directives::MachO => {}
        }
        let _ = writeln!(out, "{symbol}{name}:");
    }

    /// What is said about how far a name reaches outside a shared library, which is nothing at
    /// all in the ordinary case.
    ///
    /// A local name gets no directive whatever was asked for. `static` is already invisible to
    /// everything outside the file, so there is no dynamic symbol table for it to be in or out of,
    /// and gcc writes no visibility directive for one either.
    ///
    /// ELF says both of the other two and says them the same way an assembler expects. Mach-O has
    /// one of them: `.private_extern` is a symbol that leaves this object and does not leave the
    /// library, which is what hidden means, and there is no Mach-O spelling of protected because
    /// the format has no way to say a symbol is exported and cannot be interposed. COFF has
    /// neither, since what leaves a Windows DLL is decided by an export table the linker is given
    /// rather than by a bit on each symbol.
    pub fn seen(self, out: &mut String, name: &str, binding: Binding, visibility: Visibility) {
        if binding == Binding::Local || visibility == Visibility::Default {
            return;
        }
        let symbol = self.symbol();
        match (self, visibility) {
            (Directives::Elf, Visibility::Hidden) => {
                let _ = writeln!(out, "\t.hidden\t{name}");
            }
            (Directives::Elf, Visibility::Protected) => {
                let _ = writeln!(out, "\t.protected\t{name}");
            }
            (Directives::MachO, Visibility::Hidden) => {
                let _ = writeln!(out, "\t.private_extern\t{symbol}{name}");
            }
            (Directives::MachO, Visibility::Protected) | (Directives::Coff, _) => {}
            (_, Visibility::Default) => unreachable!("returned above"),
        }
    }

    /// The directive that opens the section a variable goes in when it is being given one of its
    /// own, and nothing at all when it is not.
    ///
    /// The name is worked out once, in [`Place::split`], so that the listing and the object file
    /// cannot come to disagree about it. What is left here is the flags, which are the flags the
    /// section it was split off from carries: splitting changes which section header a symbol
    /// points at and must not quietly change whether the page it lands in is writable.
    ///
    /// Nothing on Mach-O, for the reason [`Directives::code`] gives.
    fn split(self, out: &mut String, place: &Place, name: &str) -> bool {
        let Some(named) = place.split(name) else { return false };
        match self {
            Directives::Elf => {
                // `@nobits` for the zero filled one, because a section that says nothing about it
                // is one the assembler writes the bytes of into the file, and the point of that
                // section is that the file carries none of them. The rest of the flags are what
                // gcc 16 writes, which is a shorter spelling than the one it uses elsewhere: no
                // `@progbits`, since that is what a section is when nothing says otherwise.
                let flags = match place {
                    Place::Zero => "\"aw\",@nobits",
                    Place::ReadOnly => "\"a\"",
                    _ => "\"aw\"",
                };
                let _ = writeln!(out, "\t.section\t{named},{flags}");
            }
            // COFF gives every one of them the name of the section it came out of and tells the
            // linker which symbol the group is about, which is the same COMDAT the code above is.
            Directives::Coff => {
                let (named, flags) = match place {
                    Place::Zero => (".bss", "\"bw\""),
                    Place::ReadOnly | Place::RelocReadOnly { .. } => (".rdata", "\"dr\""),
                    _ => (".data", "\"dw\""),
                };
                let _ = writeln!(out, "\t.section\t{named},{flags},one_only,{name}");
            }
            Directives::MachO => return false,
        }
        true
    }

    /// The directive that opens the section a variable goes in.
    ///
    /// The three formats disagree about the names and about how much has to be said. ELF and COFF
    /// have a directive per section that every assembler knows, and both want the flags spelled
    /// out for a section the program named, since nothing else says whether it may be written to.
    /// Mach-O has one directive and a segment in front of every section name.
    ///
    /// `name` is the variable's, which matters only when it is being given a section of its own.
    pub fn section(self, out: &mut String, place: &Place, name: &str, sections: Sections) {
        if sections.data && self.split(out, place, name) {
            return;
        }
        match (self, place) {
            // A tentative definition is not in a section at all, and the caller is what decides
            // that. It is answered here as the section it would otherwise have gone in, so that
            // the match stays about sections and nothing has to be said twice.
            (Directives::Elf | Directives::Coff, Place::Written | Place::Merged) => {
                out.push_str("\t.data\n");
            }
            (Directives::Elf | Directives::Coff, Place::Zero) => out.push_str("\t.bss\n"),
            (Directives::Elf, Place::ReadOnly) => out.push_str("\t.section\t.rodata\n"),
            (Directives::Elf, Place::RelocReadOnly { local }) => {
                let name = if *local { ".data.rel.ro.local" } else { ".data.rel.ro" };
                let _ = writeln!(out, "\t.section\t{name},\"aw\",@progbits");
            }
            // COFF has no section of this kind and needs none. A Windows image is relocated as a
            // whole rather than a symbol at a time, and the loader makes whatever pages it has to
            // write writable for as long as it is writing them and puts them back afterwards, so
            // an address in a read only section costs a base relocation and nothing else.
            (Directives::Coff, Place::ReadOnly | Place::RelocReadOnly { .. }) => {
                out.push_str("\t.section\t.rdata,\"dr\"\n");
            }
            (Directives::Elf, Place::Named(name)) => {
                let _ = writeln!(out, "\t.section\t{name},\"aw\",@progbits");
            }
            (Directives::Coff, Place::Named(name)) => {
                let _ = writeln!(out, "\t.section\t{name},\"dw\"");
            }
            (Directives::MachO, Place::ReadOnly) => out.push_str("\t.section\t__TEXT,__const\n"),
            // Mach-O has the same problem and the same answer under a different name. A section in
            // `__TEXT` is never writable, so a constant holding an address goes in `__DATA,__const`
            // instead, which `dyld` writes and then protects. There is no `.local` half: the layout
            // hint is an ELF linker's, and this one has nothing to do with it.
            (Directives::MachO, Place::RelocReadOnly { .. }) => {
                out.push_str("\t.section\t__DATA,__const\n");
            }
            // A Mach-O section name carries the segment it is in, so a program that named one
            // named both halves and there is nothing to add to it.
            (Directives::MachO, Place::Named(name)) => {
                let _ = writeln!(out, "\t.section\t{name}");
            }
            (Directives::MachO, _) => out.push_str("\t.section\t__DATA,__data\n"),
        }
    }

    /// What is said about a variable before its image, and whether an image follows.
    ///
    /// Two kinds of variable are one directive rather than a section, a label and bytes. A
    /// tentative definition is a request to the linker for that much zeroed space on every format,
    /// and on Mach-O so is a variable whose image is all zeros, because the section that would
    /// hold it is one nothing may write bytes into.
    pub fn variable(self, out: &mut String, var: &Variable, sections: Sections) -> bool {
        let symbol = self.symbol();
        let align = var.align.max(1).trailing_zeros();
        match (self, &var.place) {
            (_, Place::Merged) => {
                let comm = if var.binding == Binding::Local { ".lcomm" } else { ".comm" };
                let name = &var.name;
                let _ = writeln!(out, "\t{comm}\t{symbol}{name},{},{}", var.size, var.align);
                return false;
            }
            (Directives::MachO, Place::Zero) => {
                let name = &var.name;
                let _ =
                    writeln!(out, "\t.zerofill\t__DATA,__bss,{symbol}{name},{},{align}", var.size);
                return false;
            }
            _ => {}
        }
        self.section(out, &var.place, &var.name, sections);
        match var.binding {
            Binding::Global => {
                let _ = writeln!(out, "\t.globl\t{symbol}{}", var.name);
            }
            Binding::Weak => {
                let _ = writeln!(out, "\t.weak\t{symbol}{}", var.name);
            }
            // Nothing, which is what makes it invisible outside the file. A name no directive
            // mentions is still in the symbol table as a local one, which is what `static` is.
            Binding::Local => {}
        }
        self.seen(out, &var.name, var.binding, var.visibility);
        let _ = writeln!(out, "\t.p2align\t{align}");
        if self == Directives::Elf {
            let _ = writeln!(out, "\t.type\t{}, @object", var.name);
        }
        let _ = writeln!(out, "{symbol}{}:", var.name);
        true
    }

    /// What is said about a function after its last instruction.
    ///
    /// The size, on the format that has one. It is written as the distance from the label to here
    /// rather than as a number, because the assembler is the one that knows how long an
    /// instruction turned out to be and this file is what it is about to find out from.
    pub fn close(self, out: &mut String, name: &str) {
        if self == Directives::Elf {
            let _ = writeln!(out, "\t.size\t{name}, .-{name}");
        }
    }

    /// A second name for something the file already wrote down.
    ///
    /// The binding and then `.set`, which is all gcc writes and all an assembler needs: the type
    /// and the size of the new symbol are taken from the old one, so writing them again would
    /// only be a second chance to disagree. Nothing opens a section first, because the symbol is
    /// an entry in a table rather than a byte of anything, and no `.size` closes it for the same
    /// reason.
    pub fn alias(self, out: &mut String, alias: &Alias) {
        let symbol = self.symbol();
        match alias.binding {
            Binding::Global => {
                let _ = writeln!(out, "\t.globl\t{symbol}{}", alias.name);
            }
            Binding::Weak => {
                let _ = writeln!(out, "\t.weak\t{symbol}{}", alias.name);
            }
            Binding::Local => {}
        }
        self.seen(out, &alias.name, alias.binding, alias.visibility);
        let _ = writeln!(out, "\t.set\t{symbol}{},{symbol}{}", alias.name, alias.target);
    }

    /// What is said once, after every function.
    pub fn end(self, out: &mut String) {
        match self {
            // Without this the stack is executable, which is not a default anybody chose.
            Directives::Elf => out.push_str("\t.section\t.note.GNU-stack,\"\",@progbits\n"),
            // What lets the linker throw away a function nothing calls, which it cannot do
            // without being told that the boundaries between them are real.
            Directives::MachO => out.push_str("\t.subsections_via_symbols\n"),
            Directives::Coff => {}
        }
    }
}

/// What the object file is told about a function's name, from what the machine function carries.
///
/// Two names for one set of three, because the machine IR is not allowed to know what an object
/// file is and the object writer is not allowed to know what a machine function is. This crate is
/// where they meet, which is where the two spellings are put side by side.
#[must_use]
pub(crate) fn binding(binding: mir::Binding) -> Binding {
    match binding {
        mir::Binding::Global => Binding::Global,
        mir::Binding::Local => Binding::Local,
        mir::Binding::Weak => Binding::Weak,
    }
}

/// What the object file is told about how far a name reaches outside a shared library, from what
/// the machine function carries.
///
/// Two spellings of one set of three, for the reason [`binding`] above has two.
#[must_use]
pub(crate) fn visibility(visibility: mir::Visibility) -> Visibility {
    match visibility {
        mir::Visibility::Default => Visibility::Default,
        mir::Visibility::Hidden => Visibility::Hidden,
        mir::Visibility::Protected => Visibility::Protected,
    }
}

#[cfg(test)]
mod tests {
    use rucc_object::FUNC_ALIGN;

    use super::*;

    #[test]
    fn a_mach_o_symbol_is_the_c_name_with_an_underscore_in_front_of_it() {
        let mut out = String::new();
        Directives::MachO.open(&mut out, "main", 16, Binding::Global, Visibility::Default);
        assert!(out.contains("\t.globl\t_main\n"), "{out}");
        assert!(out.contains("\n_main:\n"), "{out}");
        // No type and no size, neither of which Mach-O has.
        assert!(!out.contains(".type"), "{out}");
        let mut close = String::new();
        Directives::MachO.close(&mut close, "main");
        assert_eq!(close, "");
    }

    #[test]
    fn an_elf_function_says_what_it_is_and_how_long_it_is() {
        let mut out = String::new();
        Directives::Elf.open(&mut out, "main", 16, Binding::Global, Visibility::Default);
        Directives::Elf.close(&mut out, "main");
        assert!(out.contains("\t.type\tmain, @function\n"), "{out}");
        assert!(out.contains("\t.size\tmain, .-main\n"), "{out}");
    }

    #[test]
    fn a_function_that_asked_to_be_more_aligned_is_written_at_that_alignment() {
        let mut out = String::new();
        Directives::Elf.open(&mut out, "f", 256, Binding::Global, Visibility::Default);
        // The directive counts in powers of two and the attribute counts in bytes, and two
        // hundred and fifty six bytes is eight of them.
        assert!(out.contains("\t.p2align\t8, 0x90\n"), "{out}");
        let mut plain = String::new();
        Directives::Elf.open(&mut plain, "f", FUNC_ALIGN, Binding::Global, Visibility::Default);
        assert!(plain.contains("\t.p2align\t4, 0x90\n"), "{plain}");
    }

    /// The two directives that say a name does not leave the shared library, or leaves it and
    /// cannot be replaced.
    ///
    /// The listing half of tamnd/rucc#733. It matters that this is written in the listing and not
    /// only in the object writer, because the two are the same compiler taking two roads out and a
    /// program built through `-S` and an assembler has to come out the same as one built straight
    /// to an object.
    #[test]
    fn a_name_that_does_not_leave_the_library_says_so_in_the_listing() {
        let mut out = String::new();
        Directives::Elf.open(&mut out, "f", 16, Binding::Global, Visibility::Hidden);
        assert!(out.contains("\t.globl\tf\n"), "still global to the static linker: {out}");
        assert!(out.contains("\t.hidden\tf\n"), "{out}");
        let mut protected = String::new();
        Directives::Elf.open(&mut protected, "f", 16, Binding::Global, Visibility::Protected);
        assert!(protected.contains("\t.protected\tf\n"), "{protected}");
        // Mach-O's one spelling of the one of these it has, and it carries the underscore every
        // other Apple symbol does.
        let mut apple = String::new();
        Directives::MachO.open(&mut apple, "f", 16, Binding::Global, Visibility::Hidden);
        assert!(apple.contains("\t.private_extern\t_f\n"), "{apple}");
    }

    /// A `static` name gets no visibility directive whatever it asked for.
    ///
    /// gcc writes none for one either, and an assembler that is handed `.hidden` for a name that
    /// was never `.globl` has been told something about a symbol that is not in anybody's dynamic
    /// table to begin with.
    #[test]
    fn a_static_name_is_told_nothing_about_a_dynamic_linker_it_will_never_meet() {
        for seen in [Visibility::Default, Visibility::Hidden, Visibility::Protected] {
            let mut out = String::new();
            Directives::Elf.open(&mut out, "f", 16, Binding::Local, seen);
            assert!(!out.contains(".hidden"), "{seen:?}: {out}");
            assert!(!out.contains(".protected"), "{seen:?}: {out}");
        }
    }

    /// The names are what gcc 16 writes for the same declarations, checked against it on a Linux
    /// host, and the leading `.text.` is the part that has to be right rather than decoration:
    /// `--gc-sections` and the linker scripts a kernel is linked with both match on it.
    #[test]
    fn a_function_given_a_section_of_its_own_opens_one_named_after_it() {
        let split = Sections { functions: true, data: false };
        let mut out = String::new();
        Directives::Elf.code(&mut out, "f", split);
        assert_eq!(out, "\t.section\t.text.f,\"ax\",@progbits\n");
        // Windows says it as a COMDAT, which is one name for several sections and a symbol saying
        // which of them is which. That is what clang writes for the same flag on a Windows target.
        let mut windows = String::new();
        Directives::Coff.code(&mut windows, "f", split);
        assert_eq!(windows, "\t.section\t.text,\"xr\",one_only,f\n");
        // Nothing on Mach-O, whose objects end with `.subsections_via_symbols` and so already let
        // the linker drop a function nothing reaches.
        let mut apple = String::new();
        Directives::MachO.code(&mut apple, "f", split);
        assert_eq!(apple, "");
        // And nothing anywhere when nothing asked, which is the default and is what leaves every
        // function in the one `.text` the file opens with.
        for directives in [Directives::Elf, Directives::Coff, Directives::MachO] {
            let mut plain = String::new();
            directives.code(&mut plain, "f", Sections::default());
            assert_eq!(plain, "", "{directives:?}");
        }
    }

    /// Splitting must change which section header a symbol points at and nothing else, so each of
    /// these carries the flags of the section it came out of. The spellings are gcc 16's, which is
    /// shorter than what it writes for the unsplit sections: no `@progbits`, since that is what a
    /// section is when nothing says otherwise.
    #[test]
    fn a_variable_given_a_section_of_its_own_keeps_the_flags_it_would_have_had() {
        let split = Sections { functions: false, data: true };
        let cases = [
            (Place::Written, "\t.section\t.data.x,\"aw\"\n"),
            (Place::Zero, "\t.section\t.bss.x,\"aw\",@nobits\n"),
            (Place::ReadOnly, "\t.section\t.rodata.x,\"a\"\n"),
            (Place::RelocReadOnly { local: false }, "\t.section\t.data.rel.ro.x,\"aw\"\n"),
            (Place::RelocReadOnly { local: true }, "\t.section\t.data.rel.ro.local.x,\"aw\"\n"),
        ];
        for (place, want) in cases {
            let mut out = String::new();
            Directives::Elf.section(&mut out, &place, "x", split);
            assert_eq!(out, want, "{place:?}");
        }
    }

    /// The two kinds of variable the flag leaves alone, and the format that ignores it.
    ///
    /// A tentative definition is a request to the linker for that much zeroed space rather than an
    /// image, so there is no section to split off, and a variable the program put a section name on
    /// has the answer the source gave, which a flag must not overrule.
    #[test]
    fn a_variable_that_has_no_section_of_its_own_to_be_given_is_left_where_it_was() {
        let split = Sections { functions: false, data: true };
        let mut merged = String::new();
        Directives::Elf.section(&mut merged, &Place::Merged, "x", split);
        assert_eq!(merged, "\t.data\n");
        let named = Place::Named(".init_array".to_owned());
        let mut asked = String::new();
        Directives::Elf.section(&mut asked, &named, "x", split);
        assert_eq!(asked, "\t.section\t.init_array,\"aw\",@progbits\n");
        let mut apple = String::new();
        Directives::MachO.section(&mut apple, &Place::Written, "x", split);
        assert_eq!(apple, "\t.section\t__DATA,__data\n");
    }

    #[test]
    fn an_elf_file_says_the_stack_is_not_executable() {
        // The absence of this is what makes it executable, so the test is that it is there
        // rather than that it is spelled a particular way.
        let mut out = String::new();
        Directives::Elf.end(&mut out);
        assert!(out.contains(".note.GNU-stack"), "{out}");
    }

    #[test]
    fn every_object_format_has_directives() {
        for format in [ObjectFormat::Elf, ObjectFormat::MachO, ObjectFormat::Coff] {
            let directives = Directives::of(format);
            assert!(directives.text().starts_with('\t'));
            let mut out = String::new();
            directives.open(&mut out, "f", 16, Binding::Global, Visibility::Default);
            directives.close(&mut out, "f");
            directives.end(&mut out);
            assert!(out.ends_with('\n'), "{format:?} left a line unfinished");
        }
    }
}
