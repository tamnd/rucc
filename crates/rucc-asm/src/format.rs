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

use rucc_object::{Binding, Place};
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

    /// What is said about a function before its first instruction.
    ///
    /// Every function is global, because a machine function does not carry the linkage the C did
    /// and nothing below the driver could ask. That is wrong for a `static` function and is the
    /// reason `-S` output is a thing to read rather than a thing to link, until the object writer
    /// gives the machine IR somewhere to keep it.
    ///
    /// `align` is in bytes and is a power of two, and the padding is `0x90` because the space in
    /// front of a function is reached by falling off the end of the one before it.
    pub fn open(self, out: &mut String, name: &str, align: u32) {
        let symbol = self.symbol();
        let _ = writeln!(out, "\t.p2align\t{}, 0x90", align.max(1).trailing_zeros());
        let _ = writeln!(out, "\t.globl\t{symbol}{name}");
        match self {
            Directives::Elf => {
                let _ = writeln!(out, "\t.type\t{name}, @function");
            }
            // Windows says the same thing as a storage class and a type code: two is external and
            // thirty two is a function, and the two numbers together are what ELF's one word says.
            Directives::Coff => {
                let _ = writeln!(out, "\t.def\t{name}\n\t.scl\t2\n\t.type\t32\n\t.endef");
            }
            Directives::MachO => {}
        }
        let _ = writeln!(out, "{symbol}{name}:");
    }

    /// The directive that opens the section a variable goes in.
    ///
    /// The three formats disagree about the names and about how much has to be said. ELF and COFF
    /// have a directive per section that every assembler knows, and both want the flags spelled
    /// out for a section the program named, since nothing else says whether it may be written to.
    /// Mach-O has one directive and a segment in front of every section name.
    pub fn section(self, out: &mut String, place: &Place) {
        match (self, place) {
            // A tentative definition is not in a section at all, and the caller is what decides
            // that. It is answered here as the section it would otherwise have gone in, so that
            // the match stays about sections and nothing has to be said twice.
            (Directives::Elf | Directives::Coff, Place::Written | Place::Merged) => {
                out.push_str("\t.data\n");
            }
            (Directives::Elf | Directives::Coff, Place::Zero) => out.push_str("\t.bss\n"),
            (Directives::Elf, Place::ReadOnly) => out.push_str("\t.section\t.rodata\n"),
            (Directives::Coff, Place::ReadOnly) => out.push_str("\t.section\t.rdata,\"dr\"\n"),
            (Directives::Elf, Place::Named(name)) => {
                let _ = writeln!(out, "\t.section\t{name},\"aw\",@progbits");
            }
            (Directives::Coff, Place::Named(name)) => {
                let _ = writeln!(out, "\t.section\t{name},\"dw\"");
            }
            (Directives::MachO, Place::ReadOnly) => out.push_str("\t.section\t__TEXT,__const\n"),
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
    pub fn variable(self, out: &mut String, var: &Variable) -> bool {
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
        self.section(out, &var.place);
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

#[cfg(test)]
mod tests {
    use rucc_object::FUNC_ALIGN;

    use super::*;

    #[test]
    fn a_mach_o_symbol_is_the_c_name_with_an_underscore_in_front_of_it() {
        let mut out = String::new();
        Directives::MachO.open(&mut out, "main", 16);
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
        Directives::Elf.open(&mut out, "main", 16);
        Directives::Elf.close(&mut out, "main");
        assert!(out.contains("\t.type\tmain, @function\n"), "{out}");
        assert!(out.contains("\t.size\tmain, .-main\n"), "{out}");
    }

    #[test]
    fn a_function_that_asked_to_be_more_aligned_is_written_at_that_alignment() {
        let mut out = String::new();
        Directives::Elf.open(&mut out, "f", 256);
        // The directive counts in powers of two and the attribute counts in bytes, and two
        // hundred and fifty six bytes is eight of them.
        assert!(out.contains("\t.p2align\t8, 0x90\n"), "{out}");
        let mut plain = String::new();
        Directives::Elf.open(&mut plain, "f", FUNC_ALIGN);
        assert!(plain.contains("\t.p2align\t4, 0x90\n"), "{plain}");
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
            directives.open(&mut out, "f", 16);
            directives.close(&mut out, "f");
            directives.end(&mut out);
            assert!(out.ends_with('\n'), "{format:?} left a line unfinished");
        }
    }
}
