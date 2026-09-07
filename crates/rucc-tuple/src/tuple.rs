//! The tuple itself: ten fields, one canonical spelling, and a parser that refuses to guess.

use core::fmt;
use core::str::FromStr;

use crate::{Abi, Arch, DataModel, Endian, Env, Error, ObjectFormat, Os, SubArch, Version};

/// Everything about a target that changes the bytes the compiler emits.
///
/// The membership rule, from `spec/cross-compile/03-target-model.md` section 3.2: a fact belongs here if it
/// changes how a function is called or how a struct is laid out. Everything else, the CPU
/// model, the optimization level, the instruction set extensions, is a flag and lives outside.
/// The rule is what makes the tuple usable as a cache key, and the cache is what makes the
/// distribution in `spec/cross-compile/13-distribution.md` fit in the size budget, so this is load bearing
/// rather than tidy.
///
/// Construct with [`TargetTuple::new`] or by parsing. The fields are private because six of the
/// ten are derived from the other four, and a struct literal would let a caller build a
/// combination that does not exist, such as Windows with an ELF object format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TargetTuple {
    arch: Arch,
    sub_arch: SubArch,
    endian: Endian,
    data_model: DataModel,
    os: Os,
    os_version: Option<Version>,
    env: Env,
    env_version: Option<Version>,
    abi: Abi,
    object_format: ObjectFormat,
}

/// The parts a caller supplies, with the rest derived.
///
/// A builder rather than ten arguments, because eight of the ten calls in this workspace supply
/// three of them and a function with seven defaulted parameters is a function nobody calls
/// correctly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TupleBuilder {
    arch: Arch,
    sub_arch: SubArch,
    endian: Option<Endian>,
    data_model: Option<DataModel>,
    os: Os,
    os_version: Option<Version>,
    env: Option<Env>,
    env_version: Option<Version>,
    abi: Abi,
}

impl TupleBuilder {
    /// Start from the two fields that have no default.
    pub const fn new(arch: Arch, os: Os) -> Self {
        TupleBuilder {
            arch,
            sub_arch: SubArch::None,
            endian: None,
            data_model: None,
            os,
            os_version: None,
            env: None,
            env_version: None,
            abi: Abi::Default,
        }
    }

    /// Set the baseline within the architecture family.
    pub const fn sub_arch(mut self, sub_arch: SubArch) -> Self {
        self.sub_arch = sub_arch;
        self
    }

    /// Set the byte order, overriding the architecture's default.
    pub const fn endian(mut self, endian: Endian) -> Self {
        self.endian = Some(endian);
        self
    }

    /// Set the data model, overriding the one derived from the architecture and OS. The only
    /// target in the matrix that needs this is `x86_64-linux-gnux32`.
    pub const fn data_model(mut self, data_model: DataModel) -> Self {
        self.data_model = Some(data_model);
        self
    }

    /// Set the OS version, which is a deployment target on Darwin and a preview number on WASI.
    pub const fn os_version(mut self, version: Version) -> Self {
        self.os_version = Some(version);
        self
    }

    /// Set the environment, overriding the OS default.
    pub const fn env(mut self, env: Env) -> Self {
        self.env = Some(env);
        self
    }

    /// Set the environment version, which is a glibc version or an Android API level.
    pub const fn env_version(mut self, version: Version) -> Self {
        self.env_version = Some(version);
        self
    }

    /// Set the float ABI.
    pub const fn abi(mut self, abi: Abi) -> Self {
        self.abi = abi;
        self
    }

    /// Derive the remaining fields and check the combination describes a machine.
    ///
    /// # Errors
    ///
    /// Returns the specific mismatch, never a generic failure. A caller that cannot say which
    /// component the user got wrong produces a diagnostic the user cannot act on.
    pub fn build(self) -> Result<TargetTuple, Error> {
        let arch = self.arch;
        let sub_arch = self.sub_arch;
        if !sub_arch.belongs_to(arch) {
            return Err(Error::SubArchMismatch { arch, sub_arch });
        }

        let endian = self.endian.unwrap_or(arch.default_endian());
        if endian != arch.default_endian() && !arch.has_both_endians() {
            return Err(Error::EndianUnsupported { arch, endian });
        }

        let os = self.os;
        let env = self.env.unwrap_or(os.default_env());
        if !env_is_valid_for(os, env) {
            return Err(Error::EnvMismatch { os, env });
        }

        let abi = self.abi;
        if !abi.is_valid_for(arch) {
            return Err(Error::AbiMismatch { arch, abi });
        }

        if self.os_version.is_some() && !os.takes_version() {
            return Err(Error::VersionNotAccepted { component: "operating system" });
        }
        if self.env_version.is_some() && !env.is_libc() {
            return Err(Error::VersionNotAccepted { component: "environment" });
        }

        let data_model = self.data_model.unwrap_or_else(|| default_data_model(arch, os, env));
        let object_format = os.object_format().unwrap_or(match arch {
            Arch::Wasm32 => ObjectFormat::Wasm,
            _ => ObjectFormat::Elf,
        });

        Ok(TargetTuple {
            arch,
            sub_arch,
            endian,
            data_model,
            os,
            os_version: self.os_version,
            env,
            env_version: self.env_version,
            abi,
            object_format,
        })
    }
}

/// Whether an OS and an environment go together.
///
/// Two rules rather than a table of pairs. A libc is selectable only on Linux, which is the one
/// system where the user genuinely picks one. The Darwin ABI variants are legal only on Darwin.
fn env_is_valid_for(os: Os, env: Env) -> bool {
    match env {
        Env::None => true,
        Env::Gnu => matches!(os, Os::Linux | Os::Windows),
        Env::Musl => matches!(os, Os::Linux),
        Env::Android => matches!(os, Os::Linux),
        Env::Msvc => matches!(os, Os::Windows),
        Env::Simulator | Env::MacAbi => os.is_darwin(),
    }
}

/// The widths of `int`, `long` and a pointer for a target that did not name them.
///
/// Windows is the whole reason this is not just a function of the architecture. A 64-bit
/// Windows target has 64-bit pointers and a 32-bit `long`, and a compiler that reads the
/// pointer width off the architecture and assumes `long` matches it produces structures whose
/// layout disagrees with every Windows header.
fn default_data_model(arch: Arch, os: Os, _env: Env) -> DataModel {
    match arch {
        Arch::X86 | Arch::Arm | Arch::Riscv32 | Arch::Wasm32 => DataModel::Ilp32,
        _ => {
            if matches!(os, Os::Windows) {
                DataModel::Llp64
            } else {
                DataModel::Lp64
            }
        }
    }
}

impl TargetTuple {
    /// The common case: an architecture, an OS, and the OS's default environment.
    ///
    /// # Errors
    ///
    /// Returns the mismatch if the pair does not describe a machine.
    pub fn new(arch: Arch, os: Os) -> Result<Self, Error> {
        TupleBuilder::new(arch, os).build()
    }

    /// Start a builder for a target that needs more than an architecture and an OS.
    pub const fn builder(arch: Arch, os: Os) -> TupleBuilder {
        TupleBuilder::new(arch, os)
    }

    /// The instruction set family.
    pub const fn arch(self) -> Arch {
        self.arch
    }

    /// The baseline within the family.
    pub const fn sub_arch(self) -> SubArch {
        self.sub_arch
    }

    /// The byte order.
    pub const fn endian(self) -> Endian {
        self.endian
    }

    /// The widths of `int`, `long` and a pointer.
    pub const fn data_model(self) -> DataModel {
        self.data_model
    }

    /// The operating system.
    pub const fn os(self) -> Os {
        self.os
    }

    /// The OS version, which is a deployment target on Darwin and a preview number on WASI.
    pub const fn os_version(self) -> Option<Version> {
        self.os_version
    }

    /// The environment, which is the C library on Linux and the ABI variant elsewhere.
    pub const fn env(self) -> Env {
        self.env
    }

    /// The environment version, which is a glibc version or an Android API level.
    pub const fn env_version(self) -> Option<Version> {
        self.env_version
    }

    /// The float ABI as the tuple named it, which is [`Abi::Default`] unless the user was
    /// explicit. Call [`TargetTuple::resolved_abi`] to get the one code generation uses.
    pub const fn abi(self) -> Abi {
        self.abi
    }

    /// The float ABI with the target's default filled in.
    pub const fn resolved_abi(self) -> Abi {
        self.abi.resolve(self.arch, self.sub_arch, self.os, self.env)
    }

    /// The container the compiler writes for this target.
    pub const fn object_format(self) -> ObjectFormat {
        self.object_format
    }

    /// The width of a pointer in bits, read from the data model rather than from the
    /// architecture.
    pub const fn pointer_width(self) -> u32 {
        self.data_model.pointer_width()
    }

    /// Whether bytes are stored least significant first.
    pub const fn is_little_endian(self) -> bool {
        matches!(self.endian, Endian::Little)
    }

    /// Whether plain `char` is signed.
    ///
    /// Signed on x86 and wasm, unsigned on ARM, AArch64, RISC-V, LoongArch, PowerPC and s390x.
    /// This is the classic first cross compilation bug, because a corpus written and tested on
    /// x86-64 contains code that assumes `char` holds negative values and it passes until the day
    /// it runs on ARM.
    ///
    /// s390x is the row worth naming, because the obvious guess is wrong. It is a big-endian
    /// mainframe architecture with a signed everything else, and its `char` is unsigned, which the
    /// ELF ABI supplement says and which `__CHAR_UNSIGNED__` from a cross compiler confirms.
    ///
    /// The operating system overrides the architecture twice. Windows says signed everywhere
    /// because the Microsoft ABI does, and Darwin says signed on AArch64 because Apple kept it
    /// that way for source compatibility with the Intel Macs, against what AAPCS64 says. So
    /// `aarch64-macos` and `aarch64-linux-gnu` are the same architecture with opposite answers,
    /// which is the pair most likely to catch a corpus out.
    pub const fn char_is_signed(self) -> bool {
        match self.arch {
            Arch::X86_64 | Arch::X86 | Arch::Wasm32 => true,
            Arch::Arm | Arch::Aarch64 | Arch::Arm64Ec => {
                matches!(self.os, Os::Windows) || self.os.is_darwin()
            }
            Arch::Riscv64 | Arch::Riscv32 | Arch::LoongArch64 | Arch::PowerPc64 | Arch::S390x => {
                false
            }
        }
    }

    /// Whether symbols carry a leading underscore.
    pub const fn leading_underscore(self) -> bool {
        self.object_format.leading_underscore(self.data_model)
    }

    /// The leading component of the canonical spelling, which is the architecture with its
    /// baseline and byte order folded in.
    ///
    /// The folding is not decoration. `powerpc64le` and `armv7` and `aarch64_be` are what every
    /// other toolchain writes, and a tuple that spelled them as separate components would not
    /// paste into anybody's build script.
    pub fn arch_component(self) -> String {
        let mut s = String::new();
        match self.arch {
            Arch::PowerPc64 => {
                s.push_str("powerpc64");
                if matches!(self.endian, Endian::Little) {
                    s.push_str("le");
                }
            }
            Arch::Arm => {
                s.push_str("arm");
                s.push_str(self.sub_arch.as_str());
                if matches!(self.endian, Endian::Big) {
                    s.push_str("eb");
                }
            }
            Arch::Aarch64 => {
                s.push_str("aarch64");
                if matches!(self.endian, Endian::Big) {
                    s.push_str("_be");
                }
            }
            other => {
                s.push_str(other.as_str());
                if self.endian != other.default_endian() {
                    s.push_str("eb");
                }
            }
        }
        s
    }

    /// The OS component of the canonical spelling, with its version.
    pub fn os_component(self) -> String {
        match (self.os, self.os_version) {
            (Os::Wasi, Some(v)) => format!("wasip{}", v.major_part()),
            (Os::Wasi, None) => "wasi".to_string(),
            (os, Some(v)) => format!("{os}.{v}"),
            (os, None) => os.as_str().to_string(),
        }
    }

    /// The environment component of the canonical spelling, with its version and with the
    /// suffixes GCC fuses into it put back.
    ///
    /// Empty when there is nothing to say, and the caller drops the separator in that case, so
    /// `x86_64-none` has two components and `armv7m-none-eabi` has three.
    pub fn env_component(self) -> String {
        self.env_component_with(true)
    }

    /// The environment component, optionally without the dotted version.
    ///
    /// The version is dropped for the LLVM spelling because LLVM has no place to put it: an
    /// Android API level is part of the environment name there and a glibc version simply is
    /// not expressible. Emitting `gnu.2.28` into a `.ll` file would produce a triple that LLVM
    /// parses as an unknown environment, which is worse than losing the pin, and the pin is
    /// still in the canonical spelling that everything of ours reads.
    fn env_component_with(self, dotted_version: bool) -> String {
        let mut s = String::new();
        match (self.env, self.env_version) {
            (Env::Android, Some(v)) => {
                s.push_str("android");
                s.push_str(&v.major_part().to_string());
            }
            (env, Some(v)) if dotted_version => {
                s.push_str(env.as_str());
                s.push('.');
                s.push_str(&v.to_string());
            }
            (env, _) => s.push_str(env.as_str()),
        }

        if matches!(self.arch, Arch::Arm) {
            match self.resolved_abi() {
                Abi::DoubleFloat => s.push_str("eabihf"),
                _ => s.push_str("eabi"),
            }
        } else if matches!(self.data_model, DataModel::Ilp32On64) {
            s.push_str("x32");
        }
        s
    }

    /// The canonical spelling. This is what `--print-target-triple` answers and what the cache
    /// is keyed on.
    ///
    /// It has no vendor component. The vendor field in a GNU triple has carried no information
    /// since the last vendor that mattered stopped shipping a Unix, and every tool that reads
    /// one has to special case `unknown`, `pc`, `none` and `w64` to get past it. The parser
    /// accepts a vendor so that pasted triples work; the canonical form does not write one.
    pub fn to_canonical_string(self) -> String {
        let env = self.env_component();
        if env.is_empty() {
            format!("{}-{}", self.arch_component(), self.os_component())
        } else {
            format!("{}-{}-{}", self.arch_component(), self.os_component(), env)
        }
    }

    /// The LLVM spelling, with a vendor and with a three component OS version.
    ///
    /// This exists because the tuple has to leave the building. A `.ll` file, an object file's
    /// target metadata, a `--target` handed to an external tool and a user pasting from a Clang
    /// invocation all speak LLVM's dialect, and `--print-llvm-triple` is the flag that says what
    /// it would be. `spec/cross-compile/12-driver.md` section 12.3 keeps the two flags separate rather than
    /// picking one spelling and making half the users translate.
    pub fn to_llvm_string(self) -> String {
        let vendor = match self.os {
            Os::MacOs | Os::IOs => "apple",
            Os::Windows => match self.env {
                Env::Gnu => "w64",
                _ => "pc",
            },
            Os::Illumos => "pc",
            _ => "unknown",
        };

        let os = match (self.os, self.os_version) {
            (Os::MacOs, Some(v)) => format!("macosx{}", v.to_llvm_string()),
            (Os::MacOs, None) => "macosx".to_string(),
            (Os::IOs, Some(v)) => format!("ios{}", v.to_llvm_string()),
            (Os::Wasi, Some(v)) if v.major_part() == 1 => "wasi".to_string(),
            (Os::Wasi, Some(v)) => format!("wasip{}", v.major_part()),
            (Os::None, _) => "none".to_string(),
            (os, _) => os.as_str().to_string(),
        };

        let env = self.env_component_with(false);
        if env.is_empty() {
            format!("{}-{}-{}", self.arch_component(), vendor, os)
        } else {
            format!("{}-{}-{}-{}", self.arch_component(), vendor, os, env)
        }
    }
}

impl fmt::Display for TargetTuple {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_canonical_string())
    }
}

impl FromStr for TargetTuple {
    type Err = Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        crate::parse::parse(s)
    }
}
