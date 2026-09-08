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

    /// Both include directories, in search order, most specific first.
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
    /// is `i386` in every libc's source tree whatever the tuple spells it.
    ///
    /// The data model is here because an ILP32 ABI on a 64-bit architecture cannot read the LP64
    /// tree. Every type in the headers that carries a pointer or a `long` is a different size, so
    /// it gets its own directory, and `x86_64-linux-gnux32` is the row in the table that proves it.
    /// Any future ILP32-on-64 row gets a suffixed name from the same rule rather than quietly
    /// reading the LP64 headers, which is the failure this method is arranged to make impossible.
    #[must_use]
    pub fn header_arch(&self) -> &'static str {
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
