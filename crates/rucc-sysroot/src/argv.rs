//! The linker command line, as a function of the target and the sysroot and nothing else.
//!
//! Design: `spec/cross-compile/11-linking.md` section 11.3, which is a list of eight things a
//! system linker gets right by default on the machine it came with and gets wrong when it is asked
//! to link for another one.
//!
//! # Why this is a pure function
//!
//! Section 11.3 ends with the shape: `(tuple, sysroot, options) -> argv`, no environment reads, no
//! filesystem probing. That is not tidiness, it is what makes the highest consequence code in the
//! driver testable. A link line is the last thing that touches a binary and the first thing that
//! can quietly ruin it, and a function that reads the machine it runs on can only be tested on the
//! machine it runs on. This one is tested for every target in the table from any host, and
//! `tests/link-lines` is what it produced for each of them when it was last changed.
//!
//! The mirror of that rule is the one [`crate::search`] enforces for headers: nothing from the host
//! reaches the line. No `/usr/lib`, no `/lib64`, no `LIBRARY_PATH`, and no start file found by
//! looking around. Every path here is either under the sysroot or something the user wrote on the
//! command line themselves, and `nothing_on_the_line_comes_from_the_host` is that as a test.
//!
//! # What is not decided here
//!
//! Which linker runs. `spec/cross-compile/11-linking.md` section 11.2 picks one per format and the
//! driver spawns it, and the arguments below are the ones `ld`, `ld.lld` and `mold` all read the
//! same way. That is a real constraint rather than an aspiration: `-static-pie` is a compiler driver
//! flag that none of the three linkers has, so the mode that means it is spelled out here as the
//! three flags a linker does understand.
//!
//! Mach-O and COFF are refused rather than approximated. `ld64` wants a platform version load
//! command and a `-syslibroot`, `lld-link` wants `/MACHINE:` and an import library set, and neither
//! is a different spelling of what is below. [`Unsupported::Format`] says so by name, which is a
//! better answer than a line that looks plausible and produces nothing that runs.

use std::fmt;
use std::path::{Path, PathBuf};

use rucc_tuple::{Arch, DataModel, Endian, ObjectFormat, TargetTuple};

use crate::layout::Sysroot;
use crate::link::{Libc, LinkLine, LinkMode, libc, loader};

/// One input to the link, in the position the user wrote it.
///
/// Link order is semantic: an archive is searched for what is undefined at the moment the linker
/// reaches it, so a library named before the object that needs it contributes nothing. That is why
/// this is one ordered list rather than a list of objects and a list of libraries, which is a shape
/// that cannot represent what the user typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// A file, which is an object this compilation produced or one named on the command line.
    File(PathBuf),
    /// `-l<name>`, which the linker resolves against the search path.
    Library(String),
}

/// What the driver knows that the line needs, beyond the target and the sysroot.
///
/// A struct because most of it is empty in the common case, and because a function with nine
/// positional parameters of which seven are usually a default is a function somebody calls wrong.
#[derive(Debug, Clone, Default)]
pub struct Invocation<'a> {
    /// The objects and libraries, in the order they were written.
    pub inputs: &'a [Item],
    /// `-o`. Empty means the linker's own default, which is what a caller testing a line wants.
    pub output: Option<&'a Path>,
    /// How the program is linked, which decides the start file and four of the flags.
    pub mode: LinkMode,
    /// `-L`, in the order given. The user's own, and they come before ours, because somebody who
    /// passed `-L` meant it to win.
    pub search: &'a [PathBuf],
    /// `-Wl,` and `-Xlinker`, passed through untouched and last, so that anything the user said
    /// wins over anything decided here.
    pub passthrough: &'a [String],
    /// `-nostartfiles`, which leaves `crt1.o`, `crti.o` and `crtn.o` off.
    pub no_startfiles: bool,
    /// `-nodefaultlibs`, which leaves the libc and our runtime off.
    pub no_defaultlibs: bool,
    /// `-s`, which drops the symbol table.
    pub strip: bool,
}

/// A target, or a combination of a target and a mode, that has no line here.
///
/// Two variants and they are different kinds of answer. A format is not supported yet and will be.
/// A static glibc link is not a thing this scheme can produce at all, and the distinction matters to
/// somebody reading the message, because one of them is worth waiting for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unsupported {
    /// The target's object format is not ELF, and the other two linkers want a different line
    /// rather than a different spelling of this one.
    Format {
        /// The target that was asked for.
        target: String,
        /// Its object format, in the spelling `--print-config` uses.
        format: &'static str,
    },
    /// A static link against a libc that is a stub.
    ///
    /// `spec/cross-compile/09-libc-stubs.md` section 9.1 is the reason: a stub carries the names a
    /// library exports and none of the code behind them, which is everything a dynamic link needs
    /// and nothing a static one does. glibc's own `libc.a` is several megabytes of objects that
    /// cannot be synthesized from a description of an interface, so this combination is refused
    /// here rather than failing later with several thousand undefined symbols.
    ///
    /// Every [`crate::link::Libc::Stub`] target, which is glibc and also bionic, the BSDs and
    /// illumos. musl is the exception rather than the rule here, because musl is the one whose libc
    /// we build from source.
    StaticStub {
        /// The target that was asked for.
        target: String,
    },
}

impl fmt::Display for Unsupported {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Unsupported::Format { target, format } => write!(
                f,
                "there is no cross link line for {target} yet, because its object format is \
                 {format} and that linker takes a different line rather than a different spelling \
                 of this one"
            ),
            Unsupported::StaticStub { target } => write!(
                f,
                "{target} cannot be linked statically against a generated sysroot, because its \
                 libc there is a stub: it carries the names the platform's libc exports and none of \
                 the code behind them, which is what a dynamic link reads and not what a static one \
                 needs. Link it dynamically, or use a musl target, which ships a real libc.a"
            ),
        }
    }
}

impl std::error::Error for Unsupported {}

/// The whole linker command line for this target, not counting the linker itself.
///
/// Section 11.3's eight items, in the order a linker wants them:
///
/// 1. `-m`, the output format, because a linker built for more than one machine guesses from its
///    first input otherwise and a link of no objects has nothing to guess from.
/// 2. `--sysroot`, and every `-L` rooted inside it.
/// 3. `-dynamic-linker`, the one string on the line that describes the target's filesystem rather
///    than ours.
/// 4. The start files, by absolute path, in the order the two nested pairs need.
/// 5. The default libraries, which are ours rather than the host's.
/// 6. `librucc_builtins.a` for the target, which [`LinkLine`] puts after the libc.
/// 7. No host paths at all.
/// 8. The format's own extras, which on ELF is the hardening and reproducibility set below.
///
/// # Errors
///
/// [`Unsupported::Format`] for a target whose object format is not ELF, and
/// [`Unsupported::StaticStub`] for a static link against a libc that is a stub.
pub fn argv(
    target: TargetTuple,
    sysroot: &Sysroot,
    options: &Invocation<'_>,
) -> Result<Vec<String>, Unsupported> {
    let format = target.object_format();
    if format != ObjectFormat::Elf {
        return Err(Unsupported::Format {
            target: target.to_canonical_string(),
            format: format.as_str(),
        });
    }
    let statically = matches!(options.mode, LinkMode::Static | LinkMode::StaticPie);
    if statically && libc(target) == Libc::Stub {
        return Err(Unsupported::StaticStub { target: target.to_canonical_string() });
    }

    let mut args = Vec::new();
    if let Some(output) = options.output {
        args.push("-o".to_owned());
        args.push(output.display().to_string());
    }
    if let Some(name) = emulation(target) {
        args.push("-m".to_owned());
        args.push(name.to_owned());
    }
    // Not because anything below needs it, since every path here is absolute and complete, but
    // because a linker script inside the sysroot resolves the names in it against this. On a real
    // distribution `libc.so` is such a script, and without this the names in one found under a
    // sysroot are looked for on the host.
    args.push(format!("--sysroot={}", sysroot.root().display()));

    args.extend(mode_flags(target, options.mode));
    args.extend(hardening());
    if options.strip {
        args.push("-s".to_owned());
    }

    let line = LinkLine::for_target(sysroot, options.mode);
    if !options.no_startfiles {
        args.extend(shown(&line.start));
    }

    // The user's search directories first and ours second, which is the order they are written in a
    // native link too, so that `-L` in front of a sysroot behaves the way somebody passing it
    // expects. And then nothing else: step 7 is that there is no host directory here at all.
    for dir in options.search {
        args.push(format!("-L{}", dir.display()));
    }
    args.push(format!("-L{}", sysroot.lib().display()));

    for input in options.inputs {
        match input {
            Item::File(path) => args.push(path.display().to_string()),
            Item::Library(name) => args.push(format!("-l{name}")),
        }
    }

    if !options.no_defaultlibs {
        args.extend(shown(&line.libraries));
    }
    if !options.no_startfiles {
        args.extend(shown(&line.end));
    }

    args.extend(options.passthrough.iter().cloned());
    Ok(args)
}

/// The flags that say how the result is linked, and the loader when there is one.
///
/// `-static-pie` is not among them and that is the point of this function being separate. It is a
/// compiler driver flag, and the three linkers this line has to suit take three flags instead: the
/// link is static, the result is position independent, and there is explicitly no interpreter,
/// because a static binary that names one gets one mapped and then relocates itself twice.
fn mode_flags(target: TargetTuple, mode: LinkMode) -> Vec<String> {
    let mut args = Vec::new();
    match mode {
        LinkMode::Static => args.push("-static".to_owned()),
        LinkMode::StaticPie => {
            args.push("-static".to_owned());
            args.push("-pie".to_owned());
            args.push("--no-dynamic-linker".to_owned());
        }
        LinkMode::Dynamic => args.push("-pie".to_owned()),
        LinkMode::DynamicNoPie => args.push("-no-pie".to_owned()),
        LinkMode::Shared => args.push("-shared".to_owned()),
    }
    // A shared object is started by whatever loads it, so it names no interpreter even though it is
    // linked dynamically. That is the one place `is_dynamic` is not the condition.
    if matches!(mode, LinkMode::Dynamic | LinkMode::DynamicNoPie) {
        if let Some(path) = loader(target) {
            args.push("-dynamic-linker".to_owned());
            args.push(path.to_owned());
        }
    }
    args
}

/// The flags that are on every ELF line, whatever the target and whatever the mode.
///
/// Five answers to defaults nobody wants. An executable stack is a target default several linkers
/// still assume when no input object says otherwise. `relro` and `now` make the relocation tables
/// read only before `main` runs, which is the cheapest hardening there is. The unwind table header
/// is needed by every crash handler and by `backtrace`, in a C program with no exceptions in it.
/// The GNU hash table is the one a loader from this century reads.
///
/// `--build-id=none` is the reproducibility one and it is the interesting one.
/// `spec/cross-compile/11-linking.md` section 11.4 wants byte identical output from two hosts, and a
/// build id computed over the inputs carries their absolute paths into the binary. A deterministic
/// one would also do, and it is a linker's own idea of deterministic rather than ours, so the
/// absence of one is the answer that holds on all three linkers.
fn hardening() -> Vec<String> {
    [
        "--eh-frame-hdr",
        "--hash-style=gnu",
        "-z",
        "relro",
        "-z",
        "now",
        "-z",
        "noexecstack",
        "--build-id=none",
    ]
    .iter()
    .map(|flag| (*flag).to_owned())
    .collect()
}

/// Paths as the line carries them.
fn shown(paths: &[PathBuf]) -> Vec<String> {
    paths.iter().map(|path| path.display().to_string()).collect()
}

/// Which of the formats one linker can write is meant, in the name `-m` knows it by.
///
/// The same names in `ld`, `ld.lld` and `mold`, which is why this is one table rather than one per
/// linker. They are not derivable from the architecture: three of them spell the byte order into
/// the name, two spell the data model, and the narrow modes of a 64-bit architecture are a different
/// format rather than a flag on one.
///
/// [`None`] for a target whose format is not ELF, which is every one of them rather than wasm alone.
/// An emulation is an ELF idea: `ld64` takes an architecture and a platform version, and the COFF
/// linkers take a machine, so a Mach-O target that answered `aarch64linux` here would be answering a
/// question nobody asked it in a word its linker does not know. Nothing reaches this through
/// [`argv`], which refuses a non-ELF target before asking, and the answer still has to be right for
/// the recorded files and for anybody calling it directly.
#[must_use]
pub fn emulation(target: TargetTuple) -> Option<&'static str> {
    if target.object_format() != ObjectFormat::Elf {
        return None;
    }
    let narrow = target.data_model() == DataModel::Ilp32On64;
    let little = target.endian() == Endian::Little;
    Some(match target.arch() {
        Arch::X86_64 if narrow => "elf32_x86_64",
        Arch::X86_64 => "elf_x86_64",
        Arch::X86 => "elf_i386",
        Arch::Aarch64 | Arch::Arm64Ec => match (little, narrow) {
            (true, false) => "aarch64linux",
            (true, true) => "aarch64linux32",
            (false, false) => "aarch64linuxb",
            (false, true) => "aarch64linux32b",
        },
        Arch::Arm if little => "armelf_linux_eabi",
        Arch::Arm => "armelfb_linux_eabi",
        Arch::Riscv64 if little => "elf64lriscv",
        Arch::Riscv64 => "elf64briscv",
        Arch::Riscv32 if little => "elf32lriscv",
        Arch::Riscv32 => "elf32briscv",
        // 32-bit z/Architecture is `elf32_s390` and is not a target here, so there is one row.
        Arch::S390x => "elf64_s390",
        Arch::PowerPc64 if little => "elf64lppc",
        Arch::PowerPc64 => "elf64ppc",
        Arch::LoongArch64 => "elf64loongarch",
        // Unreachable, because a wasm target's format is wasm and the check above has already
        // returned. It is here because the match is exhaustive and a wasm emulation name does not
        // exist to write in it.
        Arch::Wasm32 => return None,
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use rucc_tuple::TargetTuple;

    use super::{Invocation, Item, Unsupported, argv, emulation};
    use crate::layout::Sysroot;
    use crate::link::LinkMode;

    fn target(spelling: &str) -> TargetTuple {
        spelling.parse().expect("a tuple the table knows")
    }

    fn sysroot(spelling: &str) -> Sysroot {
        Sysroot::in_cache(Path::new("/cache"), target(spelling))
    }

    fn line(spelling: &str, mode: LinkMode) -> Vec<String> {
        let one = [Item::File(Path::new("main.o").to_path_buf())];
        let options = Invocation {
            inputs: &one,
            output: Some(Path::new("main")),
            mode,
            ..Invocation::default()
        };
        argv(target(spelling), &sysroot(spelling), &options).expect("a line")
    }

    #[test]
    fn nothing_on_the_line_comes_from_the_host() {
        // The mirror of the header search rule, and the property `spec/cross-compile/02-the-goal.md`
        // claim 5 rests on. Every path is under the sysroot or is what the caller wrote.
        for spelling in ["aarch64-linux-musl", "x86_64-linux-gnu", "riscv64-linux-musl"] {
            for mode in [LinkMode::Dynamic, LinkMode::DynamicNoPie, LinkMode::Shared] {
                for arg in line(spelling, mode) {
                    let host = ["/usr/lib", "/usr/local", "/lib64/", "/lib/x86_64"]
                        .iter()
                        .any(|bad| arg.starts_with(bad));
                    // The loader is the one absolute path that is not a path on this machine. It is
                    // read by the kernel on the target, which is why it is written in full.
                    let is_loader = arg.contains("ld-musl") || arg.contains("ld-linux");
                    assert!(!host || is_loader, "{spelling} {mode:?} {arg}");
                }
            }
        }
    }

    #[test]
    fn every_file_of_ours_is_under_the_sysroot() {
        for arg in line("aarch64-linux-musl", LinkMode::Static) {
            // The caller's own `main.o` is relative and is theirs. Everything this function named
            // is absolute, and every absolute file on the line is under the sysroot.
            let ours = arg.starts_with('/') && (arg.ends_with(".o") || arg.ends_with(".a"));
            assert!(!ours || arg.starts_with("/cache/sysroots/"), "{arg}");
        }
    }

    #[test]
    fn the_static_line_names_no_loader_because_nothing_will_start_it() {
        let args = line("aarch64-linux-musl", LinkMode::Static);
        assert!(args.contains(&"-static".to_owned()), "{args:?}");
        assert!(!args.contains(&"-dynamic-linker".to_owned()), "{args:?}");
    }

    #[test]
    fn a_static_position_independent_link_is_three_flags_and_not_the_driver_one() {
        // `-static-pie` is a gcc flag and none of the three linkers has it, which is the whole
        // reason the mode is spelled out rather than passed through.
        let args = line("x86_64-linux-musl", LinkMode::StaticPie);
        assert!(!args.iter().any(|arg| arg == "-static-pie"), "{args:?}");
        for flag in ["-static", "-pie", "--no-dynamic-linker"] {
            assert!(args.contains(&flag.to_owned()), "{flag} missing from {args:?}");
        }
    }

    #[test]
    fn a_dynamic_program_names_the_loader_that_will_start_it_and_a_shared_object_does_not() {
        let program = line("x86_64-linux-gnu", LinkMode::Dynamic);
        let at = program.iter().position(|arg| arg == "-dynamic-linker").expect("the flag");
        assert_eq!(program[at + 1], "/lib64/ld-linux-x86-64.so.2");
        let library = line("x86_64-linux-gnu", LinkMode::Shared);
        assert!(!library.contains(&"-dynamic-linker".to_owned()), "{library:?}");
        assert!(library.contains(&"-shared".to_owned()), "{library:?}");
    }

    #[test]
    fn the_start_file_of_a_program_that_moves_is_not_the_one_of_a_program_that_does_not() {
        let named = |mode| {
            line("x86_64-linux-gnu", mode)
                .iter()
                .filter_map(|arg| {
                    Path::new(arg).file_name().map(|n| n.to_string_lossy().into_owned())
                })
                .find(|name| name.ends_with("crt1.o"))
        };
        assert_eq!(named(LinkMode::Dynamic).as_deref(), Some("Scrt1.o"));
        assert_eq!(named(LinkMode::DynamicNoPie).as_deref(), Some("crt1.o"));
        assert_eq!(named(LinkMode::Shared), None);
    }

    #[test]
    fn the_library_comes_after_the_objects_that_need_it() {
        let inputs = [Item::File(Path::new("main.o").to_path_buf()), Item::Library("m".to_owned())];
        let options =
            Invocation { inputs: &inputs, mode: LinkMode::Static, ..Invocation::default() };
        let args = argv(target("x86_64-linux-musl"), &sysroot("x86_64-linux-musl"), &options)
            .expect("a line");
        let object = args.iter().position(|arg| arg == "main.o").expect("the object");
        let asked = args.iter().position(|arg| arg == "-lm").expect("the library");
        let libc = args.iter().position(|arg| arg.ends_with("libc.a")).expect("the libc");
        let end = args.iter().position(|arg| arg.ends_with("crtn.o")).expect("the end file");
        assert!(object < asked && asked < libc && libc < end, "{args:?}");
    }

    #[test]
    fn a_static_glibc_link_is_refused_by_name_rather_than_attempted() {
        // A stub has no code in it, so there is nothing for a static link to take. Saying that is
        // the whole value here: the alternative is a line that produces several thousand undefined
        // symbols and a user reading the first forty of them.
        let options = Invocation { mode: LinkMode::Static, ..Invocation::default() };
        let error = argv(target("x86_64-linux-gnu"), &sysroot("x86_64-linux-gnu"), &options)
            .expect_err("refused");
        assert!(matches!(error, Unsupported::StaticStub { .. }), "{error:?}");
        assert!(error.to_string().contains("musl"), "the way out is not in the message");
        // And a musl target links statically, which is the exit criterion of #618.
        assert!(argv(target("x86_64-linux-musl"), &sysroot("x86_64-linux-musl"), &options).is_ok());
    }

    #[test]
    fn a_format_that_is_not_elf_is_refused_by_name_rather_than_approximated() {
        for spelling in ["aarch64-macos", "x86_64-windows-gnu", "wasm32-wasi"] {
            let options = Invocation { mode: LinkMode::Dynamic, ..Invocation::default() };
            let error =
                argv(target(spelling), &sysroot(spelling), &options).expect_err("no line for it");
            assert!(matches!(error, Unsupported::Format { .. }), "{spelling} {error:?}");
        }
    }

    #[test]
    fn what_the_user_told_the_linker_comes_after_what_this_told_it() {
        let passthrough = ["--no-eh-frame-hdr".to_owned()];
        let options = Invocation {
            mode: LinkMode::Dynamic,
            passthrough: &passthrough,
            ..Invocation::default()
        };
        let args = argv(target("x86_64-linux-gnu"), &sysroot("x86_64-linux-gnu"), &options)
            .expect("a line");
        assert_eq!(args.last().map(String::as_str), Some("--no-eh-frame-hdr"));
    }

    #[test]
    fn asking_for_no_start_files_leaves_out_both_ends_of_them() {
        let options =
            Invocation { mode: LinkMode::Dynamic, no_startfiles: true, ..Invocation::default() };
        let args = argv(target("x86_64-linux-gnu"), &sysroot("x86_64-linux-gnu"), &options)
            .expect("a line");
        assert!(!args.iter().any(|arg| arg.ends_with("crt1.o")), "{args:?}");
        assert!(!args.iter().any(|arg| arg.ends_with("crtn.o")), "{args:?}");
        // And still links against the libc, because that is the other flag.
        assert!(args.iter().any(|arg| arg.ends_with("libc.so")), "{args:?}");
    }

    #[test]
    fn a_narrow_mode_of_a_wide_architecture_is_a_different_output_format() {
        // The row that proves the data model belongs in the tuple. Linking x32 as `elf_x86_64`
        // produces 64-bit pointers for a target whose pointers are 32 bits.
        assert_eq!(emulation(target("x86_64-linux-gnux32")), Some("elf32_x86_64"));
        assert_eq!(emulation(target("x86_64-linux-gnu")), Some("elf_x86_64"));
    }

    #[test]
    fn byte_order_is_in_the_output_format_name() {
        assert_eq!(emulation(target("s390x-linux-gnu")), Some("elf64_s390"));
        assert_eq!(emulation(target("powerpc64le-linux-gnu")), Some("elf64lppc"));
        assert_eq!(emulation(target("riscv64-linux-musl")), Some("elf64lriscv"));
    }

    #[test]
    fn a_format_with_no_emulation_names_none_rather_than_its_architecture_s() {
        // An emulation is an ELF idea. A Mach-O target whose architecture is also an ELF one would
        // otherwise answer `aarch64linux` here, which is a word `ld64` has never heard and exactly
        // the almost-right answer `spec/cross-compile/06-abis.md` opens by warning about.
        for spelling in
            ["aarch64-macos", "x86_64-windows-gnu", "x86_64-windows-msvc", "wasm32-wasi"]
        {
            assert_eq!(emulation(target(spelling)), None, "{spelling}");
        }
    }

    #[test]
    fn a_freestanding_link_has_no_libc_and_no_start_files_and_still_has_our_runtime() {
        // Section 8.2's first row is nine headers and no link inputs, so there is no `crt1.o` to
        // name and no `libc.a` either. The builtins stay, because a 32-bit target doing 64-bit
        // arithmetic reaches them whether a libc exists or not.
        let args = line("armv7m-none-eabi", LinkMode::Static);
        assert!(!args.iter().any(|arg| arg.ends_with("crt1.o")), "{args:?}");
        assert!(!args.iter().any(|arg| arg.ends_with("crti.o")), "{args:?}");
        assert!(!args.iter().any(|arg| arg.ends_with("crtn.o")), "{args:?}");
        assert!(!args.iter().any(|arg| arg.ends_with("libc.a")), "{args:?}");
        assert!(args.iter().any(|arg| arg.ends_with("librucc_builtins.a")), "{args:?}");
        // And it is a static link with nothing to interpret it, which is what a bare metal target is.
        assert!(args.contains(&"-static".to_owned()), "{args:?}");
        assert!(!args.contains(&"-dynamic-linker".to_owned()), "{args:?}");
    }

    #[test]
    fn the_platforms_whose_libc_we_stub_refuse_a_static_link_too_and_not_only_glibc() {
        // The refusal follows from the sysroot holding a stub rather than from the target being a
        // glibc one. bionic and the BSDs are in the same position for the same reason, and a line
        // that pretended otherwise would fail in the linker instead of here.
        for spelling in ["aarch64-linux-android", "x86_64-freebsd", "x86_64-illumos"] {
            let options = Invocation { mode: LinkMode::Static, ..Invocation::default() };
            let error = argv(target(spelling), &sysroot(spelling), &options).expect_err("refused");
            assert!(matches!(error, Unsupported::StaticStub { .. }), "{spelling} {error:?}");
        }
    }

    #[test]
    fn the_same_line_comes_out_every_time_it_is_asked_for() {
        // Claim 5 in the smallest form it has: the function reads nothing but its arguments, so
        // two calls agree and so do two hosts.
        for mode in [LinkMode::Dynamic, LinkMode::Shared] {
            assert_eq!(line("aarch64-linux-gnu", mode), line("aarch64-linux-gnu", mode));
        }
    }
}
