//! The gcc flags the Linux kernel's build passes that nothing else in the driver answers, each with
//! what this compiler does with it.
//!
//! Design: `spec/04-driver-and-cli.md` section 4.12.
//!
//! kbuild passes some flags on every compile and probes others with `cc-option`, which compiles an
//! empty file with the flag and keeps the flag when the exit status is zero. A flag that is taken
//! and does nothing is the worst answer to that probe, because the kernel is then configured and
//! built as though the compiler did what the flag asked. So every row here is one of two things:
//! a flag whose request is already what happens, taken with the reason it is, or a flag this
//! compiler cannot honor yet, refused with the reason and the issue that would add it. The flags
//! kbuild passes that this compiler honors by doing something, such as `-fshort-wchar` and
//! `-fmin-function-alignment=`, have their own arms in `parse_args` and are in the spec's table
//! beside these.
//!
//! A row can be limited to one architecture, and on any other the flag is not in the table at
//! all, which leaves it to the rest of the parser. That is gcc's arrangement: the `-m` flags of one
//! back end are unknown options to another.

use rucc_target::Arch;

/// What this compiler does with a flag in the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Answer {
    /// Taken without changing anything, because what it asks for is already what happens. The
    /// text says why.
    Same(&'static str),
    /// Refused, with the reason and the issue that tracks honoring it, when there is one.
    Refused(&'static str, Option<u32>),
}

/// One flag, or one family of them.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Row {
    /// The flag. One that ends in `*` covers every flag starting with what comes before the `*`.
    pub(crate) flag: &'static str,
    /// The one architecture the flag is for, or `None` for all of them.
    pub(crate) arch: Option<Arch>,
    pub(crate) answer: Answer,
}

const fn same(flag: &'static str, why: &'static str) -> Row {
    Row { flag, arch: None, answer: Answer::Same(why) }
}

const fn refused(flag: &'static str, why: &'static str, issue: Option<u32>) -> Row {
    Row { flag, arch: None, answer: Answer::Refused(why, issue) }
}

const fn only(arch: Arch, row: Row) -> Row {
    Row { arch: Some(arch), ..row }
}

const X86: Arch = Arch::X86_64;
const A64: Arch = Arch::Aarch64;

const GUARD: &str = "there is no stack protector on AArch64 yet, and the canary the kernel keeps \
                     at an offset from sp_el0 is part of that work";
const BRANCH_PROTECTION: &str = "no function here signs its return address or starts with a bti \
                                 landing pad";

/// The table, searched in order, so a spelling that means the default comes before the family it
/// belongs to.
pub(crate) const TABLE: &[Row] = &[
    // What the output looks like to a person or a debugger, and not what it does.
    same(
        "-fverbose-asm",
        "it puts comments in assembly output, and the instructions are the same without them",
    ),
    same("-fno-verbose-asm", "no comments in the assembly output is what happens"),
    same(
        "-fvar-tracking",
        "it is about how hard gcc works at where a variable lives in the debug information, and \
         the code is the same either way",
    ),
    same("-fno-var-tracking", "the same flag, and it changes no code either"),
    same("-fvar-tracking-assignments", "the same family, and it changes no code"),
    same("-fno-var-tracking-assignments", "the same family, and it changes no code"),
    same(
        "-femit-struct-debug-baseonly",
        "it trims type information from the debug sections of a unit that does not define the \
         type, which changes their size and nothing a debugger or the program sees",
    ),
    same(
        "-fdwarf2-cfi-asm",
        "it is about whether gcc hands the assembler .cfi directives or writes the unwind table \
         itself, and the object has the same .eh_frame either way",
    ),
    same("-fno-dwarf2-cfi-asm", "the same flag, and the object is the same either way"),
    same("-fdiagnostics-show-context", "it decides how much source a diagnostic quotes"),
    same("-fdiagnostics-show-context=*", "it decides how much source a diagnostic quotes"),
    // Permissions to do something this compiler does not do, or requests for what it does.
    same(
        "-fpartial-inlining",
        "it is gcc's pass that inlines part of a function, and this compiler has no such pass",
    ),
    same("-fno-partial-inlining", "there is no partial inlining here to turn off"),
    same(
        "-fmerge-constants",
        "whether equal constants from different units share storage is left to the compiler by \
         the standard either way",
    ),
    same("-fno-merge-constants", "the same question, and the standard allows either answer"),
    same(
        "-fno-allow-store-data-races",
        "it forbids writing to memory on a path that did not write to it, and no pass here does: \
         loop invariant motion moves no store, and the one store phiopt merges is one both arms \
         already made",
    ),
    same("-fallow-store-data-races", "it permits what no pass here does"),
    same(
        "-freg-struct-return",
        "the psABI of every target here already returns a small structure in registers",
    ),
    same(
        "-fzero-init-padding-bits=all",
        "an automatic object whose initializer does not cover every byte, padding and the rest of \
         a union included, is zeroed whole before its members are stored, which is the most any \
         of the three settings asks",
    ),
    same("-fzero-init-padding-bits=unions", "padding is zeroed, which this setting allows"),
    same("-fzero-init-padding-bits=standard", "padding is zeroed, which this setting allows"),
    same(
        "-fzero-initialized-in-bss",
        "it permits putting a variable initialized to zero in .bss, which saves space and changes \
         nothing a program sees",
    ),
    refused(
        "-fno-zero-initialized-in-bss",
        "a variable whose initializer is all zeroes, `= {}` say, is still put in .bss here, which \
         is the one thing the flag forbids",
        None,
    ),
    same("-fno-stack-check", "nothing probes the stack unless something asked for it"),
    refused(
        "-fstack-check",
        "gcc's old stack probing is not written here. -fstack-clash-protection is the probing \
         this compiler does",
        None,
    ),
    refused(
        "-fstack-check=*",
        "gcc's old stack probing is not written here. -fstack-clash-protection is the probing \
         this compiler does",
        None,
    ),
    refused(
        "-fplugin=*",
        "a gcc plugin is a shared object built against gcc's own internals, which this compiler \
         cannot load",
        None,
    ),
    refused("-fplugin-arg-*", "there are no gcc plugins here to hand an argument to", None),
    // Debug information.
    // x86-64.
    only(A64, refused("-mstack-protector-guard*", GUARD, Some(2279))),
    only(
        X86,
        same(
            "-mindirect-branch-register",
            "every indirect call and jump here already goes through a register, never through \
             memory",
        ),
    ),
    only(
        X86,
        same(
            "-mno-indirect-branch-register",
            "it permits a branch through memory, and going through a register is still allowed",
        ),
    ),
    only(
        A64,
        same(
            "-mharden-sls=none",
            "nothing is put after a return or an indirect branch, the default",
        ),
    ),
    only(
        A64,
        refused(
            "-mharden-sls=*",
            "nothing is put after a return or an indirect branch to stop speculation past it",
            None,
        ),
    ),
    only(
        X86,
        same(
            "-mskip-rax-setup",
            "it lets gcc leave %al unset before a variadic call that passes nothing in vector \
             registers, and setting it anyway, as this compiler does, is always correct",
        ),
    ),
    only(X86, same("-mno-skip-rax-setup", "%al is set before every variadic call")),
    only(
        X86,
        same(
            "-maccumulate-outgoing-args",
            "it is about whether gcc pushes a call's stack arguments or stores them into space the \
             prologue made, and they end up in the same places either way",
        ),
    ),
    only(X86, same("-mno-accumulate-outgoing-args", "the arguments end up in the same places")),
    only(
        X86,
        same(
            "-mno-apx-features=*",
            "it turns off APX, and nothing here uses the APX registers or instructions",
        ),
    ),
    only(
        X86,
        refused(
            "-mregparm=*",
            "it is about passing a 32 bit x86 function's arguments in registers, and there is no \
             32 bit x86 target here",
            None,
        ),
    ),
    // AArch64.
    only(
        A64,
        same(
            "-mno-outline-atomics",
            "every atomic operation here is written inline, and none calls a helper such as \
             __aarch64_ldadd4_acq",
        ),
    ),
    only(
        A64,
        same(
            "-ffixed-x18",
            "x18 is never given to a value on any AArch64 target here, since Apple and Windows \
             reserve it",
        ),
    ),
    only(A64, same("-mlittle-endian", "every AArch64 target here is little endian")),
    only(A64, refused("-mbig-endian", "there is no big endian AArch64 target here", None)),
    only(A64, same("-mbranch-protection=none", "no branch protection, which is what happens")),
    only(A64, refused("-mbranch-protection=*", BRANCH_PROTECTION, Some(2286))),
    only(A64, same("-msign-return-address=none", "no return address is signed")),
    only(A64, refused("-msign-return-address=*", BRANCH_PROTECTION, Some(2286))),
    only(
        A64,
        same("-mno-strict-align", "an access here may be unaligned, which is what this allows"),
    ),
    only(
        A64,
        refused(
            "-mstrict-align",
            "a load or store here may be unaligned, a packed member say, and nothing splits one",
            None,
        ),
    ),
];

impl Row {
    fn covers(&self, arg: &str, arch: Arch) -> bool {
        if self.arch.is_some_and(|only| only != arch) {
            return false;
        }
        match self.flag.strip_suffix('*') {
            Some(start) => arg.starts_with(start),
            None => arg == self.flag,
        }
    }
}

/// The row that answers `arg` on `arch`, if the table has one.
pub(crate) fn row(arg: &str, arch: Arch) -> Option<&'static Row> {
    TABLE.iter().find(|row| row.covers(arg, arch))
}

/// The error a refused flag is given.
pub(crate) fn refusal(arg: &str, why: &str, issue: Option<u32>) -> String {
    match issue {
        Some(issue) => format!("{arg}: {why}. See tamnd/rucc#{issue}"),
        None => format!("{arg}: {why}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_row_limited_to_one_architecture_is_not_there_on_another() {
        assert!(row("-mno-outline-atomics", Arch::Aarch64).is_some());
        assert!(row("-mno-outline-atomics", Arch::X86_64).is_none());
        assert!(row("-mskip-rax-setup", Arch::Aarch64).is_none());
    }

    #[test]
    fn the_default_spelling_is_found_before_its_family() {
        let answer = |arg| row(arg, Arch::X86_64).map(|row| row.answer);
        assert!(matches!(answer("-fstack-check=specific"), Some(Answer::Refused(..))));
        assert!(row("-ftrivial-auto-var-init=zero", Arch::X86_64).is_none());
        // Every choice is read by the driver now, the ones that clear the vector registers too.
        assert!(row("-fzero-call-used-regs=all", Arch::X86_64).is_none());
        assert!(row("-fzero-call-used-regs=used-gpr", Arch::X86_64).is_none());
    }

    #[test]
    fn every_row_can_be_reached() {
        // A row that an earlier one always answers first is a row somebody meant to say something
        // with and that says nothing.
        for (at, row) in TABLE.iter().enumerate() {
            let arg = row.flag.strip_suffix('*').map_or(row.flag.to_owned(), |s| format!("{s}x"));
            // No row is limited to RISC-V, so a row for every architecture is looked for there,
            // where no row limited to another one can stand in front of it.
            let arch = row.arch.unwrap_or(Arch::Riscv64);
            let found = TABLE.iter().position(|other| other.covers(&arg, arch));
            assert_eq!(found, Some(at), "{} is answered by an earlier row", row.flag);
        }
    }

    #[test]
    fn the_spec_lists_every_flag_the_table_has() {
        let spec = include_str!("../../../spec/04-driver-and-cli.md");
        for row in TABLE {
            assert!(
                spec.contains(&format!("`{}`", row.flag)),
                "{} is not in section 4.12",
                row.flag
            );
        }
    }

    #[test]
    fn a_reason_does_not_end_its_own_sentence() {
        // The refusal goes on after the reason, so a full stop of its own would make two.
        for row in TABLE {
            let (Answer::Same(why) | Answer::Refused(why, _)) = row.answer;
            assert!(!why.ends_with('.'), "{}", row.flag);
        }
    }
}
