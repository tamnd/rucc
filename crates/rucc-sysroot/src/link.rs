//! The start files, the libraries and the loader for one target's link.
//!
//! Design: `spec/cross-compile/08-sysroots.md` section 8.2 and `spec/cross-compile/11-linking.md`.
//!
//! What is here is what has to be linked, in what order, and which loader will start the result.
//! How that is spelled for a particular linker is [`crate::argv`], which is the division
//! `spec/cross-compile/11-linking.md` draws: the files are a fact about the target and the flags are
//! a fact about the linker.
//!
//! # Why musl is first
//!
//! `spec/cross-compile/09-libc-stubs.md` section 9.3 is the argument. musl exercises the header
//! tree, the search paths, the start files, the compiler runtime and the link line, and it does
//! that without symbol versioning and without stub generation, which are the two hardest pieces of
//! the glibc path. If a musl cross link works end to end then the pipeline is right and what is
//! left for M9.5 is the glibc specific parts rather than the shape of the thing.
//!
//! # Why the line has three parts and not one
//!
//! `crtn.o` goes after the libraries and `crti.o` goes before them, because between them they open
//! and close the `.init` and `.fini` sections and anything contributing to those has to land in the
//! middle. A link line that is one list gets this wrong in a way that produces a binary which links,
//! runs, and does not run its static constructors, so the three parts are three fields here rather
//! than a comment on an ordering somebody has to preserve.

use std::path::{Path, PathBuf};

use rucc_tuple::{Abi, Arch, DataModel, Endian, Env, ObjectFormat, Os, TargetTuple};

use crate::layout::Sysroot;

/// How the program is linked, which decides the first start file and the flags.
///
/// Five cases rather than two booleans for static and position independent, because the two are not
/// independent and the start file is a different file in four of the five. A pair of flags would
/// admit a sixth combination, a shared object that is not position independent, which is not a thing
/// any of these linkers will produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LinkMode {
    /// Everything in the binary, no interpreter, no relocation at load. The default for musl, and
    /// the mode `spec/cross-compile/02-the-goal.md`'s exit criterion names.
    #[default]
    Static,
    /// Static, and position independent, so the loader may place it anywhere. A different first
    /// start file, because the program has to relocate itself before `main` and `rcrt1.o` is what
    /// does that.
    StaticPie,
    /// Against the shared libc, position independent, with the libc's loader named in the program
    /// header. What every distribution builds today and what `-pie` asks for.
    Dynamic,
    /// Against the shared libc, at a fixed address, which is `-no-pie`.
    ///
    /// The same link as [`LinkMode::Dynamic`] with a different start file, because the reference to
    /// `main` in `crt1.o` is an absolute one and the reference in `Scrt1.o` is not. Build systems
    /// that pass `-no-pie` are usually doing it because something in them takes the address of a
    /// function and compares it, and they get the file that matches.
    DynamicNoPie,
    /// A shared object rather than a program, which is `-shared`.
    ///
    /// No start file at all, since nothing starts a shared object and it has no `main` to be
    /// started at, and no loader named either: the program that loads this one carries that.
    Shared,
}

impl LinkMode {
    /// Whether the result is linked against a shared libc, which decides whether a loader is named.
    #[must_use]
    pub const fn is_dynamic(self) -> bool {
        matches!(self, LinkMode::Dynamic | LinkMode::DynamicNoPie | LinkMode::Shared)
    }

    /// Whether the result may be placed anywhere in memory.
    #[must_use]
    pub const fn is_pie(self) -> bool {
        matches!(self, LinkMode::StaticPie | LinkMode::Dynamic | LinkMode::Shared)
    }
}

/// What a produced sysroot holds for a target's C library.
///
/// Four cases, from `spec/cross-compile/08-sysroots.md` section 8.2's table, and the line differs
/// between them in what goes on it rather than in how it is spelled. The table has seven rows and two
/// of those are legal walls rather than technical ones, so what is left is these four.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Libc {
    /// Nothing, which is the freestanding row: the nine compiler headers and no link inputs at all.
    /// Our runtime is still there, because an architecture without a division instruction needs it
    /// whether there is a libc or not.
    None,
    /// A real static archive, which today means musl built from source. The only row where the code
    /// behind the names is present, so the only row a static link can use.
    Archive,
    /// A generated stub shared object: the names the platform's libc exports and none of the code.
    /// glibc is the row this was written for, and bionic, the BSDs and illumos take the same shape
    /// for the same reason, which is that their libc is a shared object on the target machine and a
    /// list of names is enough to link against one.
    Stub,
    /// A set of import libraries, which is what the same idea is called in COFF.
    ///
    /// Windows is the row this is, and it is a separate case from [`Libc::Stub`] rather than a
    /// spelling of it, for two reasons that both show up on the line. The container is different: a
    /// Windows program links against an archive of tiny objects per DLL rather than against one
    /// shared object, which is `spec/cross-compile/09-libc-stubs.md` section 9.4 and what
    /// `rucc_stub::coff` writes. And the C library is not one file: the msvcrt import library,
    /// mingw-w64's own `libmingwex.a` and `libmoldname.a`, and the Win32 libraries a CRT calls into
    /// are all on the line, where a glibc line has one `libc.so` on it.
    ///
    /// A static link against this is not refused, which is the other difference. On Windows the C
    /// library is a DLL on every machine and always has been, so `-static` there is a statement
    /// about our libraries and mingw-w64's rather than about the CRT, and a program linked that way
    /// runs. That is why the refusal in [`crate::argv::argv`] is about [`Libc::Stub`] by name.
    Import,
}

/// Which of the four cases this target is.
///
/// Asked in two places, which is why it is a function rather than a `match` in each: [`LinkLine`]
/// uses it to pick the files and [`crate::argv::argv`] uses it to refuse a static link against a
/// stub. Two copies of this rule would be two rules.
///
/// The format is asked before the environment, because what holds a libc's names is a property of
/// the object format and `Env::Gnu` means mingw-w64 on a Windows target and glibc on a Linux one.
#[must_use]
pub fn libc(target: TargetTuple) -> Libc {
    match (target.os(), target.env()) {
        (Os::None, _) => Libc::None,
        _ if target.object_format() == ObjectFormat::Coff => Libc::Import,
        (_, Env::Musl) => Libc::Archive,
        _ => Libc::Stub,
    }
}

/// Our own runtime library, which every one of the three lines below carries.
///
/// Named once because two callers ask about it by name: the line that puts it on, and
/// [`crate::argv::argv`] when `-fno-builtins-lib` asks for it to be left off. A second spelling of
/// the name in the second place is a flag that stops working the day the first one is renamed.
pub const BUILTINS: &str = "librucc_builtins.a";

/// The inputs to a link, in the three groups a linker needs them in.
///
/// Paths rather than strings, and no flags at all, because
/// `spec/cross-compile/11-linking.md` owns which linker is invoked and how its arguments are
/// spelled and [`crate::argv`] is where that happens. What is here is what has to be linked and in
/// what order, which is a target fact and the same fact whichever linker reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkLine {
    /// The start files, before the user's objects.
    pub start: Vec<PathBuf>,
    /// The libraries, after the user's objects.
    pub libraries: Vec<PathBuf>,
    /// The end files, after the libraries.
    pub end: Vec<PathBuf>,
}

impl LinkLine {
    /// The line for a link against this sysroot, whichever libc the target names.
    ///
    /// The dispatch rather than the line, and it is [`libc`] that decides: a real archive, a stub
    /// shared object, or nothing. The three methods below are the three answers. A target whose
    /// sysroot we do not produce yet still gets the right shape, because the shape follows from
    /// whether the libc on the target machine is an archive or a shared object and that is known
    /// before any of it is built.
    #[must_use]
    pub fn for_target(sysroot: &Sysroot, mode: LinkMode) -> Self {
        match libc(sysroot.target()) {
            Libc::None => LinkLine::freestanding(sysroot),
            Libc::Archive => LinkLine::musl(sysroot, mode),
            Libc::Stub => LinkLine::glibc(sysroot, mode),
            Libc::Import => LinkLine::mingw(sysroot, mode),
        }
    }

    /// The line for a freestanding link against this sysroot, which is our runtime and nothing else.
    ///
    /// Section 8.2's first row: nine compiler headers and no link inputs. There is no `crt1.o`,
    /// because nothing here decides what runs before `main` or whether there is a `main` at all, and
    /// no `crti.o` or `crtn.o`, because those come from a libc too. A kernel or a bootloader brings
    /// its own start file and says so with `-nostartfiles`, which it would have to pass anyway.
    ///
    /// `librucc_builtins.a` stays, because it is ours rather than the platform's.
    /// `spec/cross-compile/10-runtime.md` is the argument: an architecture with no division
    /// instruction needs `__divti3` whether there is a libc in the picture or not, and freestanding
    /// code that does 64-bit arithmetic on a 32-bit target reaches it without asking.
    ///
    /// The mode is not a parameter because it changes nothing here. Every difference between the
    /// modes is a start file and there are none.
    #[must_use]
    pub fn freestanding(sysroot: &Sysroot) -> Self {
        LinkLine {
            start: Vec::new(),
            libraries: vec![sysroot.lib().join(BUILTINS)],
            end: Vec::new(),
        }
    }

    /// The line for a musl link against this sysroot.
    ///
    /// `crt1.o` runs before `main` and calls it. `crti.o` and `crtn.o` are the prologue and the
    /// epilogue of the `.init` and `.fini` sections, which is why one is at the front and the other
    /// is at the very back. `libc.a` carries musl's whole C library, and `librucc_builtins.a`
    /// carries the operations the architecture does not have an instruction for, which
    /// `spec/cross-compile/10-runtime.md` says has to be ours rather than the platform's.
    ///
    /// The builtins go after `libc.a` because musl calls some of them, and an archive that is
    /// searched before the thing that needs it contributes nothing.
    ///
    /// `libc.a` on every mode including the dynamic ones, because what a musl sysroot here holds is
    /// musl built static: section 9.3 takes musl first precisely because one tarball built one way
    /// exercises the whole pipeline, and a shared musl is a second build of it that buys nothing
    /// until somebody asks for a dynamically linked musl program.
    #[must_use]
    pub fn musl(sysroot: &Sysroot, mode: LinkMode) -> Self {
        let lib = sysroot.lib();
        LinkLine {
            start: start_files(&lib, mode),
            libraries: vec![lib.join("libc.a"), lib.join(BUILTINS)],
            end: vec![lib.join("crtn.o")],
        }
    }

    /// The line for a link against a generated stub, which is glibc and every other hosted libc that
    /// is not musl.
    ///
    /// Named for glibc because glibc is the case `spec/cross-compile/09-libc-stubs.md` is written
    /// about and the hard one. bionic, the BSDs and illumos reach the same line for the same reason:
    /// their libc is a shared object on the target machine, so a list of the names it exports is
    /// enough to link against it, and that is what `rucc-stub` produces. The paragraphs below about
    /// `libc_nonshared.a` and `libm` are glibc's own.
    ///
    /// The same start files as musl's and a different library, and the library is the whole
    /// difference between them here. A stub libc is linked against dynamically, so what goes on the
    /// line is the generated `libc.so` from `rucc-stub` rather than an archive, and the version nodes
    /// in it are what make a program built here run on an older machine.
    ///
    /// `libc_nonshared.a` is deliberately absent and it is a known gap rather than a decision.
    /// glibc's own `libc.so` is a linker script naming `libc.so.6`, `libc_nonshared.a` and the
    /// loader as a group, and that archive holds real compiled objects: `atexit`, `__stack_chk_fail_local`
    /// on i386, and the `stat` family on releases before 2.33. None of it can be synthesized from a
    /// description, because a stub is a list of names and these are bodies, so it has to be built
    /// from glibc's sources and that is M9.5's work. A program that needs nothing from it links
    /// today and a program that does gets an undefined symbol, which is the loud failure rather
    /// than the quiet one.
    ///
    /// `libm.so` is not here either, and that is a decision. glibc's `libm` is real code and a
    /// program that wants it passes `-lm`, which every build system that does arithmetic already
    /// does, so putting it on every line would record a dependency the program does not have.
    ///
    /// A static glibc link is not one of these: there is no `libc.a` in a sysroot whose libc is a
    /// stub, because a stub is a list of names and a static link needs bodies.
    /// [`crate::argv::argv`] refuses that combination by name rather than producing this line with
    /// `-static` in front of it.
    #[must_use]
    pub fn glibc(sysroot: &Sysroot, mode: LinkMode) -> Self {
        let lib = sysroot.lib();
        LinkLine {
            start: start_files(&lib, mode),
            libraries: vec![lib.join("libc.so"), lib.join(BUILTINS)],
            end: vec![lib.join("crtn.o")],
        }
    }

    /// The line for a mingw-w64 link against this sysroot.
    ///
    /// One start file and no end file, which is the first thing that is different from every ELF
    /// line above. `crt2.o` runs before `main` and calls it, `dllcrt2.o` is its counterpart for a
    /// DLL, and there is no `crti.o` and no `crtn.o` because PE has no `.init` and `.fini` sections
    /// for a pair of files to open and close. What those two bracket on ELF is done on Windows by a
    /// table of pointers in the `.CRT$XC` sections, which the linker sorts by section name, so the
    /// ordering problem the three groups exist for does not arise here.
    ///
    /// `crtbegin.o` and `crtend.o` are deliberately absent. They are GCC's files rather than
    /// mingw-w64's, they bracket GCC's own list of constructors, and a toolchain that is not GCC
    /// writes that list the way the platform writes it instead. Ours is not written yet: a mingw
    /// link runs `main` and does not run a file scope constructor, which is a known gap that belongs
    /// with the sysroot build rather than with the line, and the gap is in the codegen for the
    /// format rather than here.
    ///
    /// The libraries are a set rather than one file, because the C library on Windows is several
    /// DLLs and the CRT calls into the system ones. `libmingw32.a` holds the start code `crt2.o`
    /// calls, `libmoldname.a` is the layer that gives the old unprefixed spellings of the names
    /// Microsoft deprecated, `libmingwex.a` is everything C requires that msvcrt does not have, and
    /// `libmsvcrt.a` is the import library for the CRT itself. Then the four Win32 libraries that
    /// mingw-w64's own code calls into, which are on the line for the same reason they are on gcc's:
    /// a program that uses none of them directly still reaches `kernel32` through `malloc`.
    ///
    /// The order is the one a single pass linker needs, which is GNU ld's PE port: a library after
    /// everything that calls into it. `librucc_builtins.a` is last for the reason it is last on the
    /// musl line, which is that the things before it call it and it calls none of them. lld's COFF
    /// linker resolves archives to a fixed point and does not care about any of this, and writing
    /// the line for the stricter of the two is what makes one line serve both.
    #[must_use]
    pub fn mingw(sysroot: &Sysroot, mode: LinkMode) -> Self {
        let lib = sysroot.lib();
        let start = match mode {
            LinkMode::Shared => "dllcrt2.o",
            _ => "crt2.o",
        };
        let libraries = [
            "libmingw32.a",
            "libmoldname.a",
            "libmingwex.a",
            "libmsvcrt.a",
            "libadvapi32.a",
            "libshell32.a",
            "libuser32.a",
            "libkernel32.a",
            BUILTINS,
        ];
        LinkLine {
            start: vec![lib.join(start)],
            libraries: libraries.iter().map(|name| lib.join(name)).collect(),
            end: Vec::new(),
        }
    }

    /// Every input, in the order they reach the linker, with the caller's objects in the middle.
    ///
    /// The one function that knows the whole order, so that a caller cannot assemble the three
    /// groups in the wrong sequence.
    #[must_use]
    pub fn with_objects(&self, objects: &[PathBuf]) -> Vec<PathBuf> {
        let mut all = self.start.clone();
        all.extend_from_slice(objects);
        all.extend(self.libraries.iter().cloned());
        all.extend(self.end.iter().cloned());
        all
    }
}

/// The start files for one mode, in the order they go on the line.
///
/// Two files, and which the first one is says how the reference to `main` inside it is written.
/// `crt1.o` refers to it absolutely, `Scrt1.o` through the global offset table so that a loader may
/// place the program anywhere, and `rcrt1.o` does that and relocates the program itself before
/// `main` runs, which is what a static position independent executable needs because there is no
/// loader to do it. A shared object has none of them.
///
/// `crti.o` is always second and `crtn.o` is always last, which is [`LinkLine`]'s three groups
/// rather than anything here.
fn start_files(lib: &Path, mode: LinkMode) -> Vec<PathBuf> {
    let first = match mode {
        LinkMode::Static | LinkMode::DynamicNoPie => Some("crt1.o"),
        LinkMode::StaticPie => Some("rcrt1.o"),
        LinkMode::Dynamic => Some("Scrt1.o"),
        LinkMode::Shared => None,
    };
    first.map(|name| lib.join(name)).into_iter().chain([lib.join("crti.o")]).collect()
}

/// The absolute path the target's loader is installed at, or [`None`] for a target that has none.
///
/// The libc picks the table and the architecture picks the row. [`None`] is the right answer for
/// three different reasons: a freestanding target has no libc, WASI has no loader of this kind at
/// all, and Darwin and Windows have one whose path is not written on the link line.
#[must_use]
pub fn loader(target: TargetTuple) -> Option<&'static str> {
    match (target.os(), target.env()) {
        (Os::Linux, Env::Musl) => Some(musl_loader(target)),
        (Os::Linux, Env::Gnu) => Some(glibc_loader(target)),
        // Bionic's is one path per word size and not one per architecture, because Android fixes
        // the filesystem layout rather than leaving it to the port.
        (Os::Linux, Env::Android) => Some(match target.pointer_width() {
            64 => "/system/bin/linker64",
            _ => "/system/bin/linker",
        }),
        _ => None,
    }
}

/// The absolute path musl's loader is installed at on the target.
///
/// It goes in the program header of a dynamically linked binary, so it is a string about the target
/// machine's filesystem and not about ours, and it has to be right without anything to check it
/// against at link time. A wrong one produces a binary that the kernel refuses to start with a
/// message about a missing file that is on nobody's disk.
///
/// 32-bit ARM is the row with two answers, because musl names the hard float and soft float builds
/// differently and they are not interchangeable. PowerPC is the other row with two, and there the
/// endianness picks, because musl treats the two byte orders as separate ports.
#[must_use]
pub fn musl_loader(target: TargetTuple) -> &'static str {
    match target.arch() {
        Arch::X86_64 => match target.data_model() {
            DataModel::Ilp32On64 => "/lib/ld-musl-x32.so.1",
            _ => "/lib/ld-musl-x86_64.so.1",
        },
        Arch::X86 => "/lib/ld-musl-i386.so.1",
        Arch::Aarch64 | Arch::Arm64Ec => "/lib/ld-musl-aarch64.so.1",
        Arch::Arm => match target.resolved_abi() {
            Abi::DoubleFloat => "/lib/ld-musl-armhf.so.1",
            _ => "/lib/ld-musl-arm.so.1",
        },
        Arch::Riscv64 => "/lib/ld-musl-riscv64.so.1",
        Arch::Riscv32 => "/lib/ld-musl-riscv32.so.1",
        Arch::S390x => "/lib/ld-musl-s390x.so.1",
        Arch::PowerPc64 => match target.endian() {
            Endian::Little => "/lib/ld-musl-powerpc64le.so.1",
            Endian::Big => "/lib/ld-musl-powerpc64.so.1",
        },
        Arch::LoongArch64 => "/lib/ld-musl-loongarch64.so.1",
        // musl has no wasm port and wasm has no loader. The caller that gets here asked for a
        // dynamic musl link on a target with neither, which is a driver bug rather than a user
        // one, and a path that cannot exist is a better report than a plausible wrong one.
        Arch::Wasm32 => "/lib/ld-musl-none.so.1",
    }
}

/// The absolute path glibc's loader is installed at on the target.
///
/// A different table from musl's and not a different spelling of it. musl names every loader after
/// the architecture in one directory; glibc's names come from each port's history, so three of them
/// are called `ld64.so` with a number that means something different per architecture, two are in
/// `/lib64` rather than `/lib`, and i386's carries no architecture in its name at all because it was
/// the only one when it was named.
///
/// The rows with more than one answer are the ones where the loader and the program have to agree
/// about register usage. 32-bit ARM has the hard float and soft float split, RISC-V and LoongArch
/// spell the float ABI and the data model into the name, and AArch64 has a byte order in it.
/// Getting one wrong produces a binary the kernel will not start, with a message about a missing
/// file, and it is a string nothing at link time can check.
#[must_use]
pub fn glibc_loader(target: TargetTuple) -> &'static str {
    let narrow = target.data_model() == DataModel::Ilp32On64;
    let hard = matches!(target.resolved_abi(), Abi::DoubleFloat);
    match target.arch() {
        Arch::X86_64 if narrow => "/libx32/ld-linux-x32.so.2",
        Arch::X86_64 => "/lib64/ld-linux-x86-64.so.2",
        Arch::X86 => "/lib/ld-linux.so.2",
        Arch::Aarch64 | Arch::Arm64Ec => match (target.endian(), narrow) {
            (Endian::Little, false) => "/lib/ld-linux-aarch64.so.1",
            (Endian::Little, true) => "/lib/ld-linux-aarch64_ilp32.so.1",
            (Endian::Big, false) => "/lib/ld-linux-aarch64_be.so.1",
            (Endian::Big, true) => "/lib/ld-linux-aarch64_be_ilp32.so.1",
        },
        // The one row where the number differs rather than the name. ARM's loader went to 3 when
        // EABI replaced OABI, and the hard float build is a separate file because passing a double
        // in a float register is not compatible with passing it in a pair of integer ones.
        Arch::Arm if hard => "/lib/ld-linux-armhf.so.3",
        Arch::Arm => "/lib/ld-linux.so.3",
        Arch::Riscv64 if hard => "/lib/ld-linux-riscv64-lp64d.so.1",
        Arch::Riscv64 => "/lib/ld-linux-riscv64-lp64.so.1",
        Arch::Riscv32 if hard => "/lib/ld-linux-riscv32-ilp32d.so.1",
        Arch::Riscv32 => "/lib/ld-linux-riscv32-ilp32.so.1",
        // `ld64` here means 64-bit z/Architecture and the 1 is glibc's ABI version for the port,
        // which is not the 2 on PowerPC's file of the same name. It is in `/lib` and PowerPC's is in
        // `/lib64`, so the two rows have nothing in common but the stem.
        Arch::S390x => "/lib/ld64.so.1",
        // ELFv2, both byte orders, which is the only PowerPC ABI
        // `spec/cross-compile/06-abis.md` admits. The ELFv1 big-endian world uses `ld64.so.1` and
        // is out of scope, so a wrong answer here is impossible rather than merely unlikely.
        Arch::PowerPc64 => "/lib64/ld64.so.2",
        Arch::LoongArch64 if hard => "/lib64/ld-linux-loongarch-lp64d.so.1",
        Arch::LoongArch64 => "/lib64/ld-linux-loongarch-lp64s.so.1",
        // There is no glibc for wasm and no loader for it either. Same reasoning as the musl table
        // above: a path nothing will ever open beats a plausible one.
        Arch::Wasm32 => "/lib/ld-linux-wasm32.so.1",
    }
}
