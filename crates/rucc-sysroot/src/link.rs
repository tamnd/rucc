//! The start files and the link line for a musl link.
//!
//! Design: `spec/cross-compile/08-sysroots.md` section 8.2 and `spec/cross-compile/11-linking.md`.
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

use std::path::PathBuf;

use rucc_tuple::{Abi, Arch, DataModel, Endian};

use crate::layout::Sysroot;

/// How the program is linked, which decides the first start file and the flags.
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
    /// Against the shared libc, with musl's loader named in the program header.
    Dynamic,
}

/// The inputs to a link, in the three groups a linker needs them in.
///
/// Paths rather than strings, and no linker flavour spelling, because
/// `spec/cross-compile/11-linking.md` owns which linker is invoked and how its arguments are
/// spelled. What is here is what has to be linked and in what order, which is a target fact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkLine {
    /// The start files, before the user's objects.
    pub start: Vec<PathBuf>,
    /// The libraries, after the user's objects.
    pub libraries: Vec<PathBuf>,
    /// The end files, after the libraries.
    pub end: Vec<PathBuf>,
    /// The flags the mode needs, which is the static switch and, for a dynamic link, the loader.
    pub flags: Vec<String>,
}

impl LinkLine {
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
    #[must_use]
    pub fn musl(sysroot: &Sysroot, mode: LinkMode) -> Self {
        let lib = sysroot.lib();
        let first = match mode {
            LinkMode::Static => "crt1.o",
            LinkMode::StaticPie => "rcrt1.o",
            LinkMode::Dynamic => "Scrt1.o",
        };

        let mut flags = Vec::new();
        match mode {
            LinkMode::Static => flags.push("-static".to_string()),
            LinkMode::StaticPie => {
                flags.push("-static-pie".to_string());
            }
            LinkMode::Dynamic => {
                flags.push("-dynamic-linker".to_string());
                flags.push(musl_loader(sysroot.target()).to_string());
            }
        }
        // An executable stack is a target default nobody wants and several linkers still assume it
        // when no input says otherwise. Saying so on every line is cheaper than finding out which
        // object failed to.
        flags.push("-z".to_string());
        flags.push("noexecstack".to_string());

        LinkLine {
            start: vec![lib.join(first), lib.join("crti.o")],
            libraries: vec![lib.join("libc.a"), lib.join("librucc_builtins.a")],
            end: vec![lib.join("crtn.o")],
            flags,
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
pub fn musl_loader(target: rucc_tuple::TargetTuple) -> &'static str {
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
