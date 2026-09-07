//! The system half of a tuple: operating system, environment, and the versions of each.

use core::fmt;

use crate::ObjectFormat;

/// An operating system, in the sense of the thing that defines the syscall interface, the
/// object format, the start files and the availability of a declaration.
///
/// Android is not here. It is a Linux kernel with a different libc, so it is `Os::Linux` with
/// [`Env::Android`], which is also how GCC and LLVM spell it. The rule is that the OS field
/// answers "whose kernel" and the environment field answers "whose libc", and Android is the
/// case that shows they are separate questions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Os {
    /// Linux. Every libc in the environment field is reachable from here.
    Linux,
    /// macOS.
    MacOs,
    /// iOS. A different OS from macOS and not a variant of it: a different platform value in
    /// `LC_BUILD_VERSION`, a different set of availability macros, and a different SDK.
    IOs,
    /// Windows, either environment.
    Windows,
    /// FreeBSD 14 and later.
    FreeBsd,
    /// NetBSD 10.1 and later.
    NetBsd,
    /// OpenBSD. Dynamically linked libc only, because OpenBSD does not ship a static one and
    /// breaks its libc ABI on purpose at every release.
    OpenBsd,
    /// illumos. In scope because it is open, which is the whole of the argument.
    Illumos,
    /// WASI. The preview number lives in the OS version, because `wasip1` and `wasip3` are
    /// different ABIs rather than different releases of one.
    Wasi,
    /// No operating system. Freestanding, which is the kernel target and the embedded target
    /// and the only row in the matrix that has to reach tier 1 without a libc.
    None,
}

impl Os {
    /// The canonical name, without the version.
    pub const fn as_str(self) -> &'static str {
        match self {
            Os::Linux => "linux",
            Os::MacOs => "macos",
            Os::IOs => "ios",
            Os::Windows => "windows",
            Os::FreeBsd => "freebsd",
            Os::NetBsd => "netbsd",
            Os::OpenBsd => "openbsd",
            Os::Illumos => "illumos",
            Os::Wasi => "wasi",
            Os::None => "none",
        }
    }

    /// The object format this OS uses. There is exactly one per OS, which is why this is a
    /// function of the OS and not a field anybody sets.
    ///
    /// The freestanding case is the exception and it is handled in [`crate::TargetTuple`],
    /// because a freestanding target's format follows its architecture: a wasm freestanding
    /// target emits wasm and an aarch64 one emits ELF.
    pub const fn object_format(self) -> Option<ObjectFormat> {
        match self {
            Os::Linux | Os::FreeBsd | Os::NetBsd | Os::OpenBsd | Os::Illumos => {
                Some(ObjectFormat::Elf)
            }
            Os::MacOs | Os::IOs => Some(ObjectFormat::MachO),
            Os::Windows => Some(ObjectFormat::Coff),
            Os::Wasi => Some(ObjectFormat::Wasm),
            Os::None => None,
        }
    }

    /// Whether this is one of the Darwin systems, which share a kernel, a linker, a set of ABI
    /// divergences from AAPCS64 and a licence that stops us shipping their headers.
    pub const fn is_darwin(self) -> bool {
        matches!(self, Os::MacOs | Os::IOs)
    }

    /// Whether a program on this OS is linked against a libc that the tuple names.
    ///
    /// False for Darwin and Windows-MSVC in the sense that matters here: the system C library
    /// is not a choice the user makes, so the environment field is carrying something else.
    pub const fn has_selectable_libc(self) -> bool {
        matches!(self, Os::Linux)
    }

    /// The environment used when the tuple does not name one.
    ///
    /// Windows defaults to `gnu` rather than `msvc`, and that is a decision rather than an
    /// oversight. `spec/cross-compile/04-target-matrix.md` puts `x86_64-windows-gnu` at tier 1 and
    /// `x86_64-windows-msvc` at tier 2 because mingw-w64 is the one we are allowed to ship, and
    /// a default that requires the user to fetch a Microsoft SDK is a default that fails on a
    /// fresh machine.
    pub const fn default_env(self) -> Env {
        match self {
            Os::Linux => Env::Gnu,
            Os::Windows => Env::Gnu,
            _ => Env::None,
        }
    }

    /// Whether an OS version in the tuple means something here.
    ///
    /// Darwin has a deployment target that decides which declarations exist, and WASI has a
    /// preview number that decides the ABI. Everywhere else the version that matters belongs to
    /// the libc and lives in the environment version instead.
    pub const fn takes_version(self) -> bool {
        matches!(self, Os::MacOs | Os::IOs | Os::Wasi)
    }
}

impl fmt::Display for Os {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The environment component of a tuple.
///
/// For Linux this is the C library, and it is the field that decides the syscall wrappers, the
/// start files, the symbol versions and whether a static link is supported. For Darwin it is
/// the ABI variant, simulator or Mac Catalyst, which is not a libc at all. For Windows it is
/// both at once: `gnu` means mingw-w64 and msvcrt, `msvc` means the Microsoft SDK and the
/// universal CRT, and the two produce different object code for the same source.
///
/// One field carrying two meanings is not ideal, and it is what every existing toolchain does,
/// so a tuple that spelled it differently would not round-trip through the tools everyone else
/// uses. `spec/cross-compile/03-target-model.md` section 3.5 takes the compatibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Env {
    /// No environment component. Darwin, the BSDs, illumos, WASI and freestanding.
    #[default]
    None,
    /// glibc. Versioned, and the version is load bearing: `spec/cross-compile/09-libc-stubs.md` is mostly
    /// about the fact that a symbol here has a version node attached to it.
    Gnu,
    /// musl. The default for static linking and the first cross target, because it is one
    /// tarball with no version nodes and no `abilist`.
    Musl,
    /// The Microsoft toolchain: the Windows SDK headers and the universal CRT.
    Msvc,
    /// Bionic. A Linux kernel with an API level instead of a libc version.
    Android,
    /// A Darwin simulator ABI. Same architecture as the host, different platform value in
    /// `LC_BUILD_VERSION`, different SDK.
    Simulator,
    /// Mac Catalyst. An iOS ABI hosted on macOS.
    MacAbi,
}

impl Env {
    /// The canonical name, without the version and without any ABI suffix.
    ///
    /// The suffixes that GCC fuses into this component, `eabihf` on ARM and `x32` on x86-64, are
    /// not here. They come from the ABI and the data model, and [`crate::TargetTuple`] puts them
    /// back when it formats the tuple. Storing them here as well would be the same fact in two
    /// places, which is the shape of every bug where a target is configured half one way.
    pub const fn as_str(self) -> &'static str {
        match self {
            Env::None => "",
            Env::Gnu => "gnu",
            Env::Musl => "musl",
            Env::Msvc => "msvc",
            Env::Android => "android",
            Env::Simulator => "simulator",
            Env::MacAbi => "macabi",
        }
    }

    /// Whether this environment is a C library whose version the tuple may pin.
    pub const fn is_libc(self) -> bool {
        matches!(self, Env::Gnu | Env::Musl | Env::Msvc | Env::Android)
    }

    /// Whether a fully static link is supported.
    ///
    /// glibc technically permits one and it is a trap: `dlopen`, NSS and `iconv` all stop
    /// working, quietly, at run time on the user's machine rather than at link time on ours.
    /// `spec/cross-compile/10-runtime.md` section 10.6 says the driver warns instead of refusing, because a
    /// program that uses none of those three is fine and it is not our place to say which.
    pub const fn supports_static_link(self) -> bool {
        matches!(self, Env::Musl | Env::None | Env::Msvc)
    }

    /// Whether a version attached to this environment is an API level rather than a release
    /// number, which changes how it is compared and how the stub set is chosen.
    pub const fn version_is_api_level(self) -> bool {
        matches!(self, Env::Android)
    }
}

impl fmt::Display for Env {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A version attached to an OS or to an environment.
///
/// Minor and patch are optional and absent is not zero. `macos.13` and `macos.13.0` are the
/// same deployment target, and a version type that normalized one to the other would not round
/// trip, so both spellings survive formatting and compare equal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Version {
    major: u32,
    minor: Option<u32>,
    patch: Option<u32>,
}

impl Version {
    /// A version with only a major component. `linux-gnu.2` is not useful and `wasip1` is
    /// exactly this.
    pub const fn major(major: u32) -> Self {
        Version { major, minor: None, patch: None }
    }

    /// A two component version, which is what a glibc version is.
    pub const fn new(major: u32, minor: u32) -> Self {
        Version { major, minor: Some(minor), patch: None }
    }

    /// A three component version, which is what an SDK version is.
    pub const fn full(major: u32, minor: u32, patch: u32) -> Self {
        Version { major, minor: Some(minor), patch: Some(patch) }
    }

    /// The major component.
    pub const fn major_part(self) -> u32 {
        self.major
    }

    /// The minor component, if the tuple gave one.
    pub const fn minor_part(self) -> Option<u32> {
        self.minor
    }

    /// The patch component, if the tuple gave one.
    pub const fn patch_part(self) -> Option<u32> {
        self.patch
    }

    /// The version as a three component tuple with absent components read as zero, which is the
    /// form to compare in. Do not use it to format: formatting from this loses the distinction
    /// between `13` and `13.0`.
    pub const fn to_triple(self) -> (u32, u32, u32) {
        let minor = match self.minor {
            Some(m) => m,
            None => 0,
        };
        let patch = match self.patch {
            Some(p) => p,
            None => 0,
        };
        (self.major, minor, patch)
    }

    /// Whether this version is at least `other`, comparing on the zero filled form.
    pub const fn at_least(self, other: Version) -> bool {
        let (a0, a1, a2) = self.to_triple();
        let (b0, b1, b2) = other.to_triple();
        if a0 != b0 {
            return a0 > b0;
        }
        if a1 != b1 {
            return a1 > b1;
        }
        a2 >= b2
    }

    /// The LLVM spelling, which always has three components. `macosx13.0.0` rather than
    /// `macos.13`.
    pub fn to_llvm_string(self) -> String {
        let (major, minor, patch) = self.to_triple();
        format!("{major}.{minor}.{patch}")
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.to_triple().cmp(&other.to_triple())
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.major)?;
        if let Some(minor) = self.minor {
            write!(f, ".{minor}")?;
            if let Some(patch) = self.patch {
                write!(f, ".{patch}")?;
            }
        }
        Ok(())
    }
}
