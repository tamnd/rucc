//! The directory layout of one target's sysroot, and the cache key that names it.
//!
//! Design: `spec/cross-compile/08-sysroots.md` sections 8.2 and 8.3.
//!
//! # Why the directories are split the way they are
//!
//! Section 8.3 is about a multiplication. Headers are naively `arch x os x libc x libc-version`
//! trees, which for glibc alone is eight architectures times six versions and several hundred
//! megabytes, and `spec/cross-compile/13-distribution.md` has a size budget that number destroys.
//!
//! The fix is two splits that turn the product into a sum. The per version differences go inside
//! the files as `#if __GLIBC_MINOR__ >= n`, so one tree serves every version. The per architecture
//! differences stay in directories, because they are whole files rather than lines, but only for
//! the small part of a libc that has any: `bits/` and a handful of others. Everything else is one
//! copy.
//!
//! That is why a sysroot here has two include directories rather than one. [`Sysroot::arch_include`]
//! holds the files that differ by architecture and is searched first, and
//! [`Sysroot::generic_include`] holds the copy that every architecture shares.
//!
//! A Linux target searches four directories and not two, because the kernel's headers are a second
//! pair with the same split and a different owner. They are [`Kernel`], their root is the cache
//! rather than a sysroot, and the order is the libc's two and then the kernel's two, which is the
//! order `zig cc -E -v` prints for a glibc target. `linux/` and `asm/` are nine megabytes of files
//! that are the same for every target, so one tree is shared and only `asm/` is copied per
//! architecture.
//!
//! # Why the root is a function of the tuple
//!
//! `spec/cross-compile/02-the-goal.md` claim 5 asks for byte identical output from different hosts.
//! A sysroot that lands in a directory named after the host, or after the day it was built, or
//! after a hash of an absolute path, breaks that before anything is compiled. So the root is the
//! cache directory the caller chose plus the canonical spelling of the tuple, and nothing else.
//!
//! The canonical spelling is the key rather than a hash of it because it is already unique, it is
//! already a legal directory name, and a cache a person can read is a cache a person can debug. It
//! carries the whole ten field model, so `x86_64-linux-gnu` and `x86_64-linux-gnu.2.28` are
//! different directories, which is the point of `env_version` being in the tuple at all.

use std::path::{Path, PathBuf};

use rucc_tuple::{Arch, DataModel, Env, Os, TargetTuple};

/// One target's sysroot: where its headers are, where its link inputs are, and where the record
/// of what they are is.
///
/// Constructed rather than discovered. Nothing here checks that any of these directories exists,
/// because the caller that is about to produce a sysroot needs the same answer as the caller that
/// is about to read one, and a constructor that failed for an absent directory would give the
/// first one nothing to create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sysroot {
    target: TargetTuple,
    root: PathBuf,
}

impl Sysroot {
    /// The sysroot for this target inside this cache directory.
    ///
    /// The path is `<cache>/sysroots/<canonical tuple>`. Two hosts running this with the same
    /// cache directory and the same target get the same path, which is what makes the tuple a
    /// cache key and is the reason `spec/cross-compile/03-target-model.md` section 3.2 admits a
    /// field only when it changes how a call is made or a struct is laid out.
    #[must_use]
    pub fn in_cache(cache: &Path, target: TargetTuple) -> Self {
        let root = cache.join("sysroots").join(target.to_canonical_string());
        Sysroot { target, root }
    }

    /// A sysroot rooted at a directory the user named, with `--sysroot` or `-isysroot`.
    ///
    /// The layout below the root is the same, so a user who assembled a tree the way we lay one
    /// out is served by every other method here. A user who did not is served by
    /// [`Options::sysroot`](crate::Options::sysroot), which replaces step 3 of section 8.5
    /// wholesale rather than assuming a shape.
    #[must_use]
    pub fn at(root: PathBuf, target: TargetTuple) -> Self {
        Sysroot { target, root }
    }

    /// The target this sysroot is for.
    #[must_use]
    pub const fn target(&self) -> TargetTuple {
        self.target
    }

    /// The directory everything else here is under.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The cache key, which is the canonical spelling of the tuple.
    ///
    /// The whole tuple and not a summary of it. A key that dropped `env_version` would serve a
    /// sysroot built against glibc 2.28 to a target that pinned 2.34, and the failure would be a
    /// missing symbol at link time on one machine and not on another.
    #[must_use]
    pub fn cache_key(&self) -> String {
        self.target.to_canonical_string()
    }

    /// The headers that differ by architecture, which are searched before the generic ones.
    ///
    /// Section 8.3's second split. For musl this is `bits/`, which is a few dozen small files
    /// against a few hundred shared ones, so the copy per architecture is cheap and the
    /// alternative of one whole tree per architecture is not.
    #[must_use]
    pub fn arch_include(&self) -> PathBuf {
        self.root.join("include").join(self.header_arch())
    }

    /// The headers every architecture shares, which is almost all of them.
    #[must_use]
    pub fn generic_include(&self) -> PathBuf {
        self.root.join("include").join("generic")
    }

    /// The libc's include directories, in search order, most specific first.
    ///
    /// Two rather than four. A Linux target also needs the kernel's own headers, which are
    /// [`Kernel`] and are not under this root, because they are the same files for every target
    /// that shares an architecture and carrying a copy of them per tuple is nine megabytes times
    /// the size of the table.
    #[must_use]
    pub fn includes(&self) -> Vec<PathBuf> {
        vec![self.arch_include(), self.generic_include()]
    }

    /// The link inputs: the start files, the libc archive or its generated stubs, and the
    /// compiler's own runtime for this target.
    #[must_use]
    pub fn lib(&self) -> PathBuf {
        self.root.join("lib")
    }

    /// The manifest naming every input with its source, its hash and its licence.
    ///
    /// A file rather than a directory, and at the top rather than beside the libraries, because
    /// the thing a person does with it is read it first.
    #[must_use]
    pub fn manifest_path(&self) -> PathBuf {
        self.root.join("manifest")
    }

    /// The name the target's libc gives to its per architecture header directory.
    ///
    /// Not the architecture component of the canonical tuple, which carries a baseline the headers
    /// do not care about: `armv7a-linux-musleabihf` and `armv5te-linux-musleabi` read the same
    /// `arm` directory, because a header does not know which instructions the chip has. 32-bit x86
    /// is `i386` in musl's source tree whatever the tuple spells it.
    ///
    /// # Why the libc is part of the answer
    ///
    /// The two libcs do not split their headers at the same place, and the name has to follow the
    /// libc rather than a scheme of ours, because the producer installs what the libc's own build
    /// system installs and the compiler has to look where that put it.
    ///
    /// musl splits per architecture and per ABI, which is what `arch/` in its source tree is, so
    /// `x86_64`, `i386` and `x32` are three directories. glibc splits per architecture family and
    /// handles the rest inside the files: one `x86` directory serves i386, x86-64 and x32, and 22
    /// of the 31 files in its `bits/` branch on `__x86_64__`, `__ILP32__` or `__WORDSIZE` to do it,
    /// starting with `bits/wordsize.h`. Checked against Zig 0.16, which ships twelve glibc
    /// directories named after families and seventeen musl directories named after architectures.
    ///
    /// # The rule this is here to enforce
    ///
    /// An ILP32 ABI on a 64-bit architecture cannot read the LP64 headers. Every type that carries
    /// a pointer or a `long` is a different size, and `x86_64-linux-gnux32` is the row that proves
    /// it. For musl that is a separate directory, which is what the suffix below is. For glibc it
    /// is a branch inside glibc's own files, so the directory is shared and the thing that checks
    /// it is section 8.4's structural equivalence corpus rather than a path.
    #[must_use]
    pub fn header_arch(&self) -> &'static str {
        if self.target.env() == Env::Gnu {
            return self.header_family();
        }
        let narrow = self.target.data_model() == DataModel::Ilp32On64;
        match (self.target.arch(), narrow) {
            // x32 is what everyone calls it, including musl and glibc, so it does not get the
            // suffix the rule below would give it.
            (Arch::X86_64, true) => "x32",
            (Arch::X86_64, false) => "x86_64",
            (Arch::X86, _) => "i386",
            (Arch::Aarch64 | Arch::Arm64Ec, true) => "aarch64_ilp32",
            (Arch::Aarch64 | Arch::Arm64Ec, false) => "aarch64",
            (Arch::Arm, _) => "arm",
            (Arch::Riscv64, true) => "riscv64_ilp32",
            (Arch::Riscv64, false) => "riscv64",
            (Arch::Riscv32, _) => "riscv32",
            (Arch::S390x, true) => "s390x_ilp32",
            (Arch::S390x, false) => "s390x",
            (Arch::PowerPc64, true) => "powerpc64_ilp32",
            (Arch::PowerPc64, false) => "powerpc64",
            (Arch::LoongArch64, true) => "loongarch64_ilp32",
            (Arch::LoongArch64, false) => "loongarch64",
            (Arch::Wasm32, _) => "wasm32",
        }
    }

    /// The architecture family, which is how glibc names its per architecture header directory.
    ///
    /// The data model is not in it, on purpose, for the reason [`Sysroot::header_arch`] gives: the
    /// family's files carry the branch themselves. `s390x` and `loongarch` are spelled the way
    /// glibc's own `sysdeps` tree spells them, which is not the same shortening for both.
    fn header_family(&self) -> &'static str {
        match self.target.arch() {
            Arch::X86_64 | Arch::X86 => "x86",
            // Arm64EC is a Windows ABI and never has glibc headers. It answers with the family it
            // belongs to rather than with a word that is not a directory anywhere.
            Arch::Aarch64 | Arch::Arm64Ec => "aarch64",
            Arch::Arm => "arm",
            Arch::Riscv64 | Arch::Riscv32 => "riscv",
            Arch::S390x => "s390x",
            Arch::PowerPc64 => "powerpc",
            Arch::LoongArch64 => "loongarch",
            // There is no glibc for wasm. The arm of the match exists because the type is closed
            // and a wildcard here would quietly name a directory for a future architecture.
            Arch::Wasm32 => "wasm32",
        }
    }
}

/// The kernel's own headers, which are not the libc's and are shared by every target that can read
/// them.
///
/// `linux/` and `asm/` are the system call interface rather than the C library, and a sysroot
/// without them does not compile 31 of glibc's installed headers or 3 of musl's, `sys/quota.h` and
/// `net/ethernet.h` among them. So they are part of what section 8.2 calls a sysroot even though no
/// libc produced them.
///
/// # Why they are not under [`Sysroot`]
///
/// One tree serves every libc and every architecture except `asm/`, which is per architecture and
/// small. Copying the shared part into each tuple's sysroot would be nine megabytes times the
/// number of Linux rows in the table, for files that are identical in every copy. So the root is
/// the cache directory rather than a sysroot, and a sysroot that was produced with it records the
/// version in its manifest.
///
/// # Why the version is not in the path
///
/// The driver has to be able to compute this path before it reads anything, and a version in the
/// path would mean asking the cache what it has before being able to ask where it is. It is the
/// same decision [`Sysroot::in_cache`] makes about the libc version, where the tuple carries the
/// version only because `env_version` is part of the target's identity, and the same gap: a cache
/// populated by one release and read by the next gets whatever is there.
/// `spec/cross-compile/13-distribution.md` section 13.2 owns that, because the answer is the
/// content hash in the cache layout and it belongs to both.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kernel {
    arch: &'static str,
    root: PathBuf,
}

impl Kernel {
    /// The kernel headers in this cache directory for this target, when the target has any.
    ///
    /// [`None`] for everything that is not Linux with a libc we produce a tree for. Windows, the
    /// BSDs and Darwin have their own system headers and no `linux/` at all, freestanding has no
    /// system call interface by definition, and Android is Linux but bionic carries its own
    /// scrubbed copy of the uapi headers, which is a different tree from this one and not a subset
    /// of it.
    #[must_use]
    pub fn for_target(cache: &Path, target: TargetTuple) -> Option<Kernel> {
        if target.os() != Os::Linux || !matches!(target.env(), Env::Gnu | Env::Musl) {
            return None;
        }
        let arch = kernel_arch(target.arch())?;
        Some(Kernel { arch, root: cache.join("kernel-headers") })
    }

    /// The directory both of these are under.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `asm/`, which is the part of the interface that is per architecture.
    ///
    /// Named after the kernel's own architecture directory and not after ours or the libc's, which
    /// is a third spelling of the same machine and the reason this is a method rather than a format
    /// string at the call site.
    #[must_use]
    pub fn arch_include(&self) -> PathBuf {
        self.root.join(self.arch)
    }

    /// `linux/`, `asm-generic/` and the rest, which are the same files for every architecture.
    #[must_use]
    pub fn generic_include(&self) -> PathBuf {
        self.root.join("generic")
    }

    /// Both directories, in search order, most specific first.
    #[must_use]
    pub fn includes(&self) -> Vec<PathBuf> {
        vec![self.arch_include(), self.generic_include()]
    }

    /// The kernel's name for this architecture.
    #[must_use]
    pub const fn arch(&self) -> &'static str {
        self.arch
    }
}

/// What `make headers_install ARCH=` takes, which is a third naming of the machine.
///
/// `arm64` rather than `aarch64` and `s390` rather than `s390x`, because those are the directories
/// under `arch/` in the kernel's source tree, and the 31-bit s390 port leaving did not rename the
/// one that stayed. One directory serves both widths of x86, of riscv and of powerpc, the same way
/// glibc's family does and for the same reason: the uapi headers branch on the compiler's macros.
///
/// [`None`] for an architecture the kernel does not have, which is wasm.
const fn kernel_arch(arch: Arch) -> Option<&'static str> {
    match arch {
        Arch::X86_64 | Arch::X86 => Some("x86"),
        Arch::Aarch64 | Arch::Arm64Ec => Some("arm64"),
        Arch::Arm => Some("arm"),
        Arch::Riscv64 | Arch::Riscv32 => Some("riscv"),
        Arch::S390x => Some("s390"),
        Arch::PowerPc64 => Some("powerpc"),
        Arch::LoongArch64 => Some("loongarch"),
        Arch::Wasm32 => None,
    }
}

/// Whether we can produce a sysroot for this target without the user fetching anything.
///
/// Section 8.2's table has seven rows and two of them are legal walls rather than engineering.
/// The macOS SDK is restricted by the Xcode licence to Apple-branded hardware and the Windows SDK
/// is not redistributable, so for those two the answer is a path the user supplies under their own
/// licence, and `spec/cross-compile/13-distribution.md` owns the mechanism.
///
/// This returns false for those two and true for everything else, including freestanding, which
/// needs nine compiler headers and no link inputs at all.
#[must_use]
pub fn can_be_bundled(target: TargetTuple) -> bool {
    !matches!(target.os(), Os::MacOs | Os::IOs) && target.env() != Env::Msvc
}
