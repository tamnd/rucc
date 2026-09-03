//! What an assembler is told about a function, which is the object format's answer rather than
//! the machine's.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.3, which is about the object files
//! themselves. The directives here are the same facts said in text: which section code goes in,
//! how a symbol is spelled, which symbols leave the file, and where each one ends.
//!
//! They are not the same on the three formats and the differences are not cosmetic. A Mach-O
//! symbol carries an underscore in front of the C name and an ELF one does not, so a listing that
//! got that wrong would fail to link against every library on the machine. A local label is
//! spelled `.L` on ELF and COFF and `L` on Mach-O, and a label that is not spelled the local way
//! ends up in the symbol table, where it is a name a debugger and a backtrace will show. And ELF
//! wants a marker saying the stack is not executable, whose absence makes it executable, which
//! section 11.3 calls out as a real and recurring security bug.

use std::fmt::Write as _;

use rucc_target::ObjectFormat;

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
    pub fn open(self, out: &mut String, name: &str) {
        let symbol = self.symbol();
        out.push_str("\t.p2align\t4, 0x90\n");
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
    use super::*;

    #[test]
    fn a_mach_o_symbol_is_the_c_name_with_an_underscore_in_front_of_it() {
        let mut out = String::new();
        Directives::MachO.open(&mut out, "main");
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
        Directives::Elf.open(&mut out, "main");
        Directives::Elf.close(&mut out, "main");
        assert!(out.contains("\t.type\tmain, @function\n"), "{out}");
        assert!(out.contains("\t.size\tmain, .-main\n"), "{out}");
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
            directives.open(&mut out, "f");
            directives.close(&mut out, "f");
            directives.end(&mut out);
            assert!(out.ends_with('\n'), "{format:?} left a line unfinished");
        }
    }
}
