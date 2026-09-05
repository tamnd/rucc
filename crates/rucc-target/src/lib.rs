//! Target descriptions: triples, and the facts about a target that the rest of the
//! compiler reads rather than hard-codes.
//!
//! Design: `spec/12-abi-and-runtime.md`. Layer rank 1, see `spec/18-package-layout.md`.
//!
//! The rule from `spec/18-package-layout.md` section 18.2 is that there is no
//! target-specific code outside this crate and the per-target rule sets. Everything a pass
//! needs to know about a target is a field it can read here. That rule is what makes the
//! claim in `spec/10-backend.md` testable, namely that a new target is a rule set and a few
//! data files, and `M10` brings up a fourth target specifically to put a number on it.
//!
//! [`TargetInfo::call`] is the other half of that rule and the one with teeth. How a structure
//! travels between a caller and a callee is the target's answer rather than C's, so the walk to
//! the IR flattens a C type into a [`Shape`] and asks here what form it takes. Every psABI rule
//! is behind [`Call`] and nothing outside this crate matches on an architecture to find one.
//!
//! # Status
//!
//! Triple parsing and the basic data model are real, which is what `rucc --print-config`
//! reports, and so is the argument classification of every psABI in
//! `spec/12-abi-and-runtime.md` sections 12.2 to 12.5. x86-64's register file is written down,
//! in [`x86_64`], along with what each of the two conventions over it does with each register,
//! what each of its machine instructions does with its operands, and which instructions a frame
//! is made of, which is [`FrameInsts`]. AArch64's and RISC-V's arrive with their backends.
//! Machine models land in `M6`.
//!
//! This crate is tier 3 in `spec/18-package-layout.md` section 18.5: its Rust API is
//! explicitly unstable and will change without a major version bump.

#![doc(html_root_url = "https://docs.rs/rucc-target/0.3.14")]

use std::fmt;
use std::str::FromStr;

use rucc_base::float::Format;

mod abi;
mod branch;
mod frame;
mod operand;
mod regs;
pub mod x86_64;

pub use crate::abi::{Arg, Call, Kind, Pass, Piece, Scalar, Shape, Slot};
pub use crate::branch::BranchInsts;
pub use crate::frame::{ClassMoves, FrameInsts};
pub use crate::operand::{Constraint, OperandDesc, Role};
pub use crate::regs::{CallRegs, ClassInfo, PhysReg, Places, RegClass, RegFile, Where};

/// A target architecture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
// Deliberately not `#[non_exhaustive]`. Adding a variant here has to break every
// match that needs to change, in this workspace and in anyone else's code. That is
// the property `spec/10-backend.md` section 10.8 is claiming when it says adding a
// target is a data change: the compiler tells you every place the data is read.
pub enum Arch {
    /// x86-64, the first target and the one `M3` brings up.
    X86_64,
    /// AArch64, the second target, `M6`.
    Aarch64,
    /// 64-bit RISC-V. `spec/10-backend.md` calls this the middle-end canary, because it has
    /// no condition codes and no complex addressing modes, so anything the middle end got
    /// away with on x86-64 shows up here.
    Riscv64,
}

impl Arch {
    /// Pointer width in bits.
    pub const fn pointer_width(self) -> u32 {
        match self {
            Arch::X86_64 | Arch::Aarch64 | Arch::Riscv64 => 64,
        }
    }

    /// Whether the target is little-endian.
    pub const fn is_little_endian(self) -> bool {
        match self {
            Arch::X86_64 | Arch::Aarch64 | Arch::Riscv64 => true,
        }
    }

    /// The name as it appears in a triple.
    pub const fn as_str(self) -> &'static str {
        match self {
            Arch::X86_64 => "x86_64",
            Arch::Aarch64 => "aarch64",
            Arch::Riscv64 => "riscv64",
        }
    }
}

/// The operating system a target runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
// Deliberately not `#[non_exhaustive]`. Adding a variant here has to break every
// match that needs to change, in this workspace and in anyone else's code. That is
// the property `spec/10-backend.md` section 10.8 is claiming when it says adding a
// target is a data change: the compiler tells you every place the data is read.
pub enum Os {
    /// Linux, hosted or freestanding.
    Linux,
    /// Apple platforms. `spec/12-abi-and-runtime.md` section 12.3 lists the four places
    /// Apple diverges from AAPCS64, and every one of them is a real bug if missed.
    Darwin,
    /// Windows.
    Windows,
    /// No operating system, which is what `-ffreestanding` kernel work looks like.
    None,
}

impl Os {
    /// The name as it appears in a triple.
    pub const fn as_str(self) -> &'static str {
        match self {
            Os::Linux => "linux",
            Os::Darwin => "darwin",
            Os::Windows => "windows",
            Os::None => "none",
        }
    }

    /// The object file format this operating system uses.
    pub const fn object_format(self) -> ObjectFormat {
        match self {
            Os::Linux | Os::None => ObjectFormat::Elf,
            Os::Darwin => ObjectFormat::MachO,
            Os::Windows => ObjectFormat::Coff,
        }
    }
}

/// The C runtime and ABI variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
// Deliberately not `#[non_exhaustive]`. Adding a variant here has to break every
// match that needs to change, in this workspace and in anyone else's code. That is
// the property `spec/10-backend.md` section 10.8 is claiming when it says adding a
// target is a data change: the compiler tells you every place the data is read.
pub enum Env {
    /// The default for the operating system.
    None,
    /// glibc.
    Gnu,
    /// musl.
    Musl,
    /// The MSVC ABI.
    Msvc,
}

impl Env {
    /// The name as it appears in a triple, if it appears at all.
    pub const fn as_str(self) -> &'static str {
        match self {
            Env::None => "none",
            Env::Gnu => "gnu",
            Env::Musl => "musl",
            Env::Msvc => "msvc",
        }
    }
}

/// The object file format to emit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
// Deliberately not `#[non_exhaustive]`. Adding a variant here has to break every
// match that needs to change, in this workspace and in anyone else's code. That is
// the property `spec/10-backend.md` section 10.8 is claiming when it says adding a
// target is a data change: the compiler tells you every place the data is read.
pub enum ObjectFormat {
    /// ELF.
    Elf,
    /// Mach-O.
    MachO,
    /// COFF.
    Coff,
}

impl ObjectFormat {
    /// The name used in diagnostics and in `--print-config`.
    pub const fn as_str(self) -> &'static str {
        match self {
            ObjectFormat::Elf => "elf",
            ObjectFormat::MachO => "macho",
            ObjectFormat::Coff => "coff",
        }
    }
}

/// A target triple.
///
/// We accept the LLVM-style `arch-vendor-os-env` form because that is what build systems
/// pass, and we normalise it to the three fields we actually branch on. The vendor field is
/// parsed and discarded: no decision in the compiler depends on it, and keeping it would
/// invite one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Triple {
    /// The architecture.
    pub arch: Arch,
    /// The operating system.
    pub os: Os,
    /// The runtime and ABI variant.
    pub env: Env,
}

impl Triple {
    /// A triple from its three parts.
    pub const fn new(arch: Arch, os: Os, env: Env) -> Self {
        Self { arch, os, env }
    }

    /// The triple of the machine this compiler is running on.
    ///
    /// Used as the default target, which is what makes `rucc hello.c` work with no flags.
    /// Unknown host combinations are not an error here: they are reported by the driver,
    /// where there is somewhere to report them to.
    pub fn host() -> Option<Self> {
        let arch = match std::env::consts::ARCH {
            "x86_64" => Arch::X86_64,
            "aarch64" => Arch::Aarch64,
            "riscv64" => Arch::Riscv64,
            _ => return None,
        };
        // Which libc this is matters, and `std::env::consts` does not say. A compiler built on
        // Alpine and defaulting to `x86_64-unknown-linux-gnu` describes a machine it is not
        // running on: musl and glibc disagree about `int_fast16_t` among other things, and a
        // header that is written out of the predefined type names picks the disagreement up.
        // The libc rucc itself was linked against is the best evidence available about the one
        // the code it compiles will be linked against, and it is right on every machine where
        // rucc was built for the machine it runs on.
        let linux = if cfg!(target_env = "musl") { Env::Musl } else { Env::Gnu };
        let (os, env) = match std::env::consts::OS {
            "linux" => (Os::Linux, linux),
            "macos" => (Os::Darwin, Env::None),
            "windows" => (Os::Windows, Env::Msvc),
            _ => return None,
        };
        Some(Self::new(arch, os, env))
    }
}

impl fmt::Display for Triple {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Always four fields, always the same spelling, because this string ends up in
        // `--print-config` output that people diff.
        write!(f, "{}-unknown-{}-{}", self.arch.as_str(), self.os.as_str(), self.env.as_str())
    }
}

/// Why a triple failed to parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseTripleError {
    /// The triple as given.
    pub input: String,
    /// What specifically was not recognised.
    pub reason: &'static str,
}

impl fmt::Display for ParseTripleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unsupported target triple `{}`: {}", self.input, self.reason)
    }
}

impl std::error::Error for ParseTripleError {}

impl FromStr for Triple {
    type Err = ParseTripleError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let err = |reason| ParseTripleError { input: s.to_owned(), reason };
        let mut parts = s.split('-');

        let arch = match parts.next() {
            Some("x86_64" | "amd64") => Arch::X86_64,
            Some("aarch64" | "arm64") => Arch::Aarch64,
            Some("riscv64") => Arch::Riscv64,
            _ => return Err(err("unknown architecture")),
        };

        // The vendor field is optional in practice. `x86_64-linux-gnu` and
        // `x86_64-unknown-linux-gnu` both occur in the wild and mean the same thing, so the
        // remaining fields are matched by content rather than by position.
        let rest: Vec<&str> = parts.collect();
        let mut os = None;
        let mut env = None;
        for part in &rest {
            match *part {
                "linux" => os = Some(Os::Linux),
                "darwin" | "macos" | "macosx" | "ios" => os = Some(Os::Darwin),
                "windows" | "win32" => os = Some(Os::Windows),
                // `none` is the one token that means different things in the two positions.
                // In `x86_64-unknown-none-elf` it is the operating system; in
                // `aarch64-apple-darwin-none` it is the environment. Which one it is depends
                // on whether an operating system has already been seen, and that rule is what
                // makes `Display` round-trip through `FromStr`.
                "none" if os.is_none() => os = Some(Os::None),
                "none" => env = Some(Env::None),
                "elf" => os = os.or(Some(Os::None)),
                "gnu" | "gnueabi" | "gnueabihf" => env = Some(Env::Gnu),
                "musl" | "musleabi" | "musleabihf" => env = Some(Env::Musl),
                "msvc" => env = Some(Env::Msvc),
                _ => {}
            }
        }

        let os = os.ok_or_else(|| err("unknown operating system"))?;
        let env = env.unwrap_or(match os {
            Os::Linux => Env::Gnu,
            Os::Windows => Env::Msvc,
            Os::Darwin | Os::None => Env::None,
        });
        Ok(Self::new(arch, os, env))
    }
}

/// The facts about a target that the compiler reads instead of hard-coding.
///
/// This is the whole of what a pass is allowed to know about where its output will run.
/// It grows, and every field added here is one fewer `#[cfg]` somewhere it should not be.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct TargetInfo {
    /// The triple this describes.
    pub triple: Triple,
    /// Width of a pointer in bits.
    pub pointer_width: u32,
    /// Whether bytes are ordered little end first.
    pub little_endian: bool,
    /// Whether a bare `char` is signed.
    ///
    /// Signed on x86-64 and unsigned on AArch64 Linux, which is the classic source of code
    /// that works on one and not the other, so it is data rather than an assumption.
    pub char_is_signed: bool,
    /// Width of `long` in bits. This is the field that separates the LP64 world from
    /// Windows LLP64.
    pub long_width: u32,
    /// Width of `long double` in bits: 80 bits of x87 stored in 128 on SysV x86-64,
    /// 64 on Apple platforms, 64 on Windows.
    pub long_double_width: u32,
    /// The format `long double` actually is, which the width does not say.
    ///
    /// It is 128 bits wide on SysV x86-64 and on AArch64 Linux and the two are not the same
    /// type: one is the x87 eighty bit format padded out to sixteen bytes and the other is
    /// true quad precision with a hundred and thirteen bits of significand. Anything that
    /// converts a constant or folds one has to know which, and the width alone cannot say.
    pub long_double_format: Format,
    /// The format `_Float64x` is, which is the widest format the target has short of a software
    /// one.
    ///
    /// It follows the architecture and not the operating system, which is what makes it worth a
    /// field of its own next to `long double`. Apple and Windows define `long double` as a
    /// `double` and neither of them takes `_Float64x` down with it: the type has to be wider
    /// than a `_Float64`, so it is the x87 eighty bit format on x86-64 and quad precision on
    /// AArch64 and RISC-V wherever it is written.
    pub float64x_format: Format,
    /// Width of `wchar_t` in bits, which decides what a wide literal is encoded in.
    ///
    /// It is 16 on Windows, so a wide string there is UTF-16 and a character outside the basic
    /// plane takes two elements, and 32 everywhere else, where a wide string is UTF-32 and no
    /// character takes more than one.
    pub wchar_width: u32,
    /// Whether `wchar_t` is signed.
    ///
    /// x86-64 Linux makes it a signed `int` and AArch64 Linux makes it an `unsigned int`,
    /// following the psABI's rule for plain `char`, so `L'\xffffffff'` is minus one on one of
    /// them and four billion on the other.
    pub wchar_is_signed: bool,
    /// The granule a `_BitInt` wider than 64 bits is laid out in, in bits.
    ///
    /// Above 64 bits the psABIs stop treating a `_BitInt` like a standard integer type and
    /// start treating it like an array of these, so its size is rounded up to a multiple of
    /// this and its alignment is this. It is 64 on x86-64 and RISC-V and 128 on AArch64, which
    /// is why `_BitInt(65)` is sixteen bytes aligned to eight on one and sixteen bytes aligned
    /// to sixteen on the other. Measured with clang 18 on x86-64 Linux and clang on AArch64
    /// Darwin rather than read off the documents.
    pub bit_int_granule: u32,
    /// The object format to emit.
    pub object_format: ObjectFormat,
    /// What `__builtin_va_list` is, which is the type every `va_list` in every header is a
    /// typedef of.
    pub va_list: VaList,
    /// The registers the machine has, which is [`RegFile::EMPTY`] for an architecture nothing
    /// has described yet.
    pub regs: &'static RegFile,
    /// Which registers the calling convention gives which job, or `None` while the
    /// architecture has no register file to name them out of.
    pub call_regs: Option<&'static CallRegs>,
}

/// The type a target's `__builtin_va_list` is.
///
/// A variable argument list is the one place a psABI dictates a C type rather than how a type
/// travels, and the four answers below are not four spellings of one thing: `sizeof(va_list)` is
/// eight bytes on Apple's AArch64 and thirty two on Linux's, and on SysV x86-64 a `va_list` is an
/// array, so a `va_list` passed to a function is passed as a pointer and one assigned to another
/// is a constraint violation rather than a copy. Code in the wild depends on all of that.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
// Deliberately not `#[non_exhaustive]`, for the reason [`Arch`] is not: a fifth answer here is
// a fifth type to build, and every place that builds one should stop compiling until it does.
pub enum VaList {
    /// `char *`, which is what a target whose arguments are all passed in one place needs: the
    /// address of the next argument and nothing else. Apple's AArch64 and both Windows targets.
    CharPointer,
    /// `void *`, which is the RISC-V psABI's spelling of the same thing.
    VoidPointer,
    /// `struct __va_list_tag { unsigned gp_offset, fp_offset; void *overflow_arg_area,
    /// *reg_save_area; } [1]`, the SysV x86-64 one. Arguments arrive in two register files and
    /// on the stack, so the list is a cursor into each, and the array of one is what makes
    /// passing it to `vfprintf` pass its address.
    SysV,
    /// `struct __va_list { void *__stack, *__gr_top, *__vr_top; int __gr_offs, __vr_offs; }`,
    /// the AAPCS64 one. The same idea as SysV's, counting down from the top of each save area
    /// rather than up from the bottom, and not an array.
    Aapcs,
}

impl VaList {
    /// The name used in `--print-config`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            VaList::CharPointer => "char-pointer",
            VaList::VoidPointer => "void-pointer",
            VaList::SysV => "sysv",
            VaList::Aapcs => "aapcs",
        }
    }
}

impl TargetInfo {
    /// The description of `triple`.
    pub fn new(triple: Triple) -> Self {
        let char_is_signed = match (triple.arch, triple.os) {
            // The AArch64 and RISC-V psABIs make plain `char` unsigned, and x86-64 SysV
            // makes it signed. Apple and Windows both override that back to signed on
            // AArch64, which is the kind of divergence that only ever surfaces as a bug
            // report from someone whose lexer compares a `char` against a negative value.
            (Arch::Aarch64 | Arch::Riscv64, Os::Linux | Os::None) => false,
            _ => true,
        };
        let long_width = match triple.os {
            // Windows is LLP64: `long` stays 32 bits on a 64-bit target.
            Os::Windows => 32,
            _ => triple.arch.pointer_width(),
        };
        let long_double_width = match triple.os {
            // Apple defines `long double` as `double`, per spec/12-abi-and-runtime.md
            // section 12.3, and Windows does the same. On the SysV targets it is a distinct
            // type: 80 bits of x87 stored in 128 on x86-64, and true quad precision on
            // AArch64 and RISC-V.
            Os::Darwin | Os::Windows => 64,
            Os::Linux | Os::None => 128,
        };
        let long_double_format = match (triple.arch, long_double_width) {
            (_, 64) => Format::Double,
            // The one place two targets agree on the width and disagree on the type.
            (Arch::X86_64, _) => Format::X87Extended,
            (Arch::Aarch64 | Arch::Riscv64, _) => Format::Quad,
        };
        let float64x_format = match triple.arch {
            Arch::X86_64 => Format::X87Extended,
            Arch::Aarch64 | Arch::Riscv64 => Format::Quad,
        };
        let bit_int_granule = match triple.arch {
            Arch::Aarch64 => 128,
            Arch::X86_64 | Arch::Riscv64 => 64,
        };
        // Windows makes `wchar_t` 16 bits so that a wide string is UTF-16, and AArch64 Linux
        // makes it unsigned the way it makes plain `char` unsigned. Neither follows from
        // anything else here, which is why both are their own field.
        let wchar_width = if triple.os == Os::Windows { 16 } else { 32 };
        let wchar_is_signed = !matches!(
            (triple.arch, triple.os),
            (_, Os::Windows) | (Arch::Aarch64, Os::Linux | Os::None)
        );
        let va_list = match (triple.arch, triple.os) {
            // Windows passes every argument in one place and spills the register ones next to
            // the stack ones, so the list is an address, and Apple does the same on AArch64.
            (_, Os::Windows) | (Arch::Aarch64, Os::Darwin) => VaList::CharPointer,
            (Arch::X86_64, _) => VaList::SysV,
            (Arch::Aarch64, _) => VaList::Aapcs,
            (Arch::Riscv64, _) => VaList::VoidPointer,
        };
        // AArch64 and RISC-V have register files and this crate has not written them down yet.
        // They arrive with the backends that need them, in M6 and M7.
        let regs = match triple.arch {
            Arch::X86_64 => &x86_64::REGS,
            Arch::Aarch64 | Arch::Riscv64 => &RegFile::EMPTY,
        };
        let call_regs = match (triple.arch, triple.os) {
            (Arch::X86_64, Os::Windows) => Some(&x86_64::WIN64),
            // Apple's x86-64 follows SysV, and its divergences from it are on AArch64.
            (Arch::X86_64, _) => Some(&x86_64::SYSV),
            (Arch::Aarch64 | Arch::Riscv64, _) => None,
        };
        Self {
            triple,
            pointer_width: triple.arch.pointer_width(),
            little_endian: triple.arch.is_little_endian(),
            char_is_signed,
            long_width,
            long_double_width,
            long_double_format,
            float64x_format,
            wchar_width,
            wchar_is_signed,
            bit_int_granule,
            object_format: triple.os.object_format(),
            va_list,
            regs,
            call_regs,
        }
    }

    /// The largest an object may be on this target, in bytes.
    ///
    /// `PTRDIFF_MAX`, which is what C 6.5.6 needs it to be: subtracting two pointers into one
    /// object has to have an answer, and the answer has a `ptrdiff_t` to fit in. So an object
    /// of exactly this many bytes is allowed and one byte more is not, which is the line GCC
    /// draws too. It is the only size limit in the compiler and every layout question that has
    /// one asks here rather than at whatever its own arithmetic happens to overflow at.
    #[must_use]
    pub const fn max_object_size(&self) -> u64 {
        (1u64 << (self.pointer_width - 1)) - 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_four_field_triple() {
        let t: Triple = "x86_64-unknown-linux-gnu".parse().unwrap();
        assert_eq!(t, Triple::new(Arch::X86_64, Os::Linux, Env::Gnu));
    }

    #[test]
    fn parses_a_triple_with_no_vendor() {
        let t: Triple = "aarch64-linux-musl".parse().unwrap();
        assert_eq!(t, Triple::new(Arch::Aarch64, Os::Linux, Env::Musl));
    }

    #[test]
    fn accepts_the_common_aliases() {
        let a: Triple = "arm64-apple-darwin".parse().unwrap();
        let b: Triple = "aarch64-apple-darwin".parse().unwrap();
        assert_eq!(a, b);
        assert_eq!(a.env, Env::None);
    }

    #[test]
    fn fills_in_the_default_environment() {
        let t: Triple = "x86_64-unknown-linux".parse().unwrap();
        assert_eq!(t.env, Env::Gnu);
        let w: Triple = "x86_64-pc-windows".parse().unwrap();
        assert_eq!(w.env, Env::Msvc);
    }

    #[test]
    fn rejects_what_it_does_not_support() {
        let e = "sparc64-unknown-linux-gnu".parse::<Triple>().unwrap_err();
        assert_eq!(e.reason, "unknown architecture");
        let e = "x86_64-unknown-plan9".parse::<Triple>().unwrap_err();
        assert_eq!(e.reason, "unknown operating system");
    }

    #[test]
    fn displays_in_a_normalised_form() {
        let t: Triple = "amd64-linux-gnu".parse().unwrap();
        assert_eq!(t.to_string(), "x86_64-unknown-linux-gnu");
    }

    #[test]
    fn display_round_trips_through_parse() {
        for s in [
            "x86_64-unknown-linux-gnu",
            "aarch64-unknown-darwin-none",
            "riscv64-unknown-linux-musl",
        ] {
            let t: Triple = s.parse().unwrap();
            assert_eq!(t.to_string().parse::<Triple>().unwrap(), t);
        }
    }

    #[test]
    fn char_signedness_follows_the_psabi() {
        let x86 = TargetInfo::new("x86_64-unknown-linux-gnu".parse().unwrap());
        let arm = TargetInfo::new("aarch64-unknown-linux-gnu".parse().unwrap());
        let mac = TargetInfo::new("aarch64-apple-darwin".parse().unwrap());
        assert!(x86.char_is_signed);
        assert!(!arm.char_is_signed);
        assert!(mac.char_is_signed, "Apple overrides AAPCS64 back to a signed char");
    }

    #[test]
    fn windows_is_llp64() {
        let win = TargetInfo::new("x86_64-pc-windows-msvc".parse().unwrap());
        assert_eq!(win.pointer_width, 64);
        assert_eq!(win.long_width, 32);
    }

    #[test]
    fn the_largest_object_is_ptrdiff_max() {
        // Half the address space less one, which is what a pointer subtraction across the whole
        // of one object has to fit in. gcc 16 on x86-64 prints this same number when it refuses
        // an array, and takes an object of exactly this many bytes.
        for triple in ["x86_64-unknown-linux-gnu", "aarch64-apple-darwin", "x86_64-pc-windows-msvc"]
        {
            let target = TargetInfo::new(triple.parse().unwrap());
            assert_eq!(target.max_object_size(), 9_223_372_036_854_775_807, "{triple}");
        }
    }

    #[test]
    fn apple_long_double_is_double() {
        let mac = TargetInfo::new("aarch64-apple-darwin".parse().unwrap());
        assert_eq!(mac.long_double_width, 64);
        assert_eq!(mac.long_double_format, Format::Double);
        let linux = TargetInfo::new("x86_64-unknown-linux-gnu".parse().unwrap());
        assert_eq!(linux.long_double_width, 128);
    }

    #[test]
    fn wchar_t_divides_the_targets_in_two_directions_at_once() {
        // Windows narrows it to sixteen bits, which makes a wide string UTF-16 there and
        // UTF-32 everywhere else, and AArch64 Linux makes it unsigned without narrowing it.
        let windows = TargetInfo::new("x86_64-pc-windows-msvc".parse().unwrap());
        assert_eq!((windows.wchar_width, windows.wchar_is_signed), (16, false));
        let arm = TargetInfo::new("aarch64-unknown-linux-gnu".parse().unwrap());
        assert_eq!((arm.wchar_width, arm.wchar_is_signed), (32, false));
        let linux = TargetInfo::new("x86_64-unknown-linux-gnu".parse().unwrap());
        assert_eq!((linux.wchar_width, linux.wchar_is_signed), (32, true));
        // Apple keeps it signed on the same processor where Linux does not, in the same way it
        // keeps plain `char` signed there.
        let mac = TargetInfo::new("aarch64-apple-darwin".parse().unwrap());
        assert_eq!((mac.wchar_width, mac.wchar_is_signed), (32, true));
    }

    #[test]
    fn va_list_is_the_psabis_type_and_not_one_type_with_four_spellings() {
        let linux = TargetInfo::new("x86_64-unknown-linux-gnu".parse().unwrap());
        assert_eq!(linux.va_list, VaList::SysV);
        // x86-64 Darwin follows SysV here, and AArch64 Darwin does not follow AAPCS64.
        let mac = TargetInfo::new("x86_64-apple-darwin".parse().unwrap());
        assert_eq!(mac.va_list, VaList::SysV);
        let arm_mac = TargetInfo::new("aarch64-apple-darwin".parse().unwrap());
        assert_eq!(arm_mac.va_list, VaList::CharPointer);
        let arm = TargetInfo::new("aarch64-unknown-linux-gnu".parse().unwrap());
        assert_eq!(arm.va_list, VaList::Aapcs);
        // Windows passes everything one way on both processors, so both get the simple one.
        let win = TargetInfo::new("x86_64-pc-windows-msvc".parse().unwrap());
        assert_eq!(win.va_list, VaList::CharPointer);
        let arm_win = TargetInfo::new("aarch64-pc-windows-msvc".parse().unwrap());
        assert_eq!(arm_win.va_list, VaList::CharPointer);
        let riscv = TargetInfo::new("riscv64-unknown-linux-gnu".parse().unwrap());
        assert_eq!(riscv.va_list, VaList::VoidPointer);
    }

    #[test]
    fn two_targets_agree_on_the_width_of_long_double_and_not_on_the_type() {
        // Sixteen bytes on both, and a different number in them: the x87 format has sixty four
        // bits of significand and quad precision has a hundred and thirteen, so a constant
        // converted for one is the wrong bits for the other.
        let x86 = TargetInfo::new("x86_64-unknown-linux-gnu".parse().unwrap());
        let arm = TargetInfo::new("aarch64-unknown-linux-gnu".parse().unwrap());
        assert_eq!(x86.long_double_width, arm.long_double_width);
        assert_eq!(x86.long_double_format, Format::X87Extended);
        assert_eq!(arm.long_double_format, Format::Quad);
        assert_eq!(x86.long_double_format.precision(), 64);
        assert_eq!(arm.long_double_format.precision(), 113);
        // Windows keeps the name and drops the type, the way Apple does.
        let windows = TargetInfo::new("x86_64-pc-windows-msvc".parse().unwrap());
        assert_eq!(windows.long_double_format, Format::Double);
    }

    #[test]
    fn float64x_follows_the_processor_where_long_double_follows_the_operating_system() {
        // `_Float64x` is the widest format the hardware has, and no ABI takes it away the way
        // Apple and Windows take `long double` away. So the two fields say the same thing on
        // Linux and disagree everywhere else, which is the whole reason there are two of them.
        let x86 = TargetInfo::new("x86_64-unknown-linux-gnu".parse().unwrap());
        assert_eq!(x86.float64x_format, Format::X87Extended);
        let arm = TargetInfo::new("aarch64-unknown-linux-gnu".parse().unwrap());
        assert_eq!(arm.float64x_format, Format::Quad);
        let riscv = TargetInfo::new("riscv64-unknown-linux-gnu".parse().unwrap());
        assert_eq!(riscv.float64x_format, Format::Quad);

        let mac = TargetInfo::new("aarch64-apple-darwin".parse().unwrap());
        assert_eq!(mac.long_double_format, Format::Double);
        assert_eq!(mac.float64x_format, Format::Quad);
        let windows = TargetInfo::new("x86_64-pc-windows-msvc".parse().unwrap());
        assert_eq!(windows.long_double_format, Format::Double);
        assert_eq!(windows.float64x_format, Format::X87Extended);
    }

    #[test]
    fn the_object_format_follows_the_operating_system() {
        assert_eq!(Os::Linux.object_format(), ObjectFormat::Elf);
        assert_eq!(Os::Darwin.object_format(), ObjectFormat::MachO);
        assert_eq!(Os::Windows.object_format(), ObjectFormat::Coff);
    }

    #[test]
    fn a_target_carries_its_registers_and_says_so_when_it_has_none() {
        let of = |triple: &str| TargetInfo::new(triple.parse().unwrap());
        let linux = of("x86_64-unknown-linux-gnu");
        assert_eq!(linux.regs.reg_named("rdi"), Some((x86_64::GPR, x86_64::RDI)));
        assert_eq!(linux.call_regs.map(|regs| regs.int_args[0]), Some(x86_64::RDI));
        // Apple's x86-64 is SysV and Windows is the one that is not.
        let apple = of("x86_64-apple-darwin");
        assert_eq!(apple.call_regs.map(|regs| regs.int_args[0]), Some(x86_64::RDI));
        let windows = of("x86_64-pc-windows-msvc");
        assert_eq!(windows.regs.len(x86_64::GPR), 16);
        assert_eq!(windows.call_regs.map(|regs| regs.int_args[0]), Some(x86_64::RCX));
        // Not described yet, and saying nothing is the answer rather than saying x86-64's.
        let arm = of("aarch64-unknown-linux-gnu");
        assert!(arm.regs.is_empty());
        assert!(arm.call_regs.is_none());
    }

    #[test]
    fn the_host_triple_is_one_we_support() {
        // Every host in spec/15-testing.md section 15.7 must be recognised, and CI runs on
        // all three, so a failure here means a host we claim support for stopped resolving.
        let host = Triple::host().expect("the host must be a supported target");
        assert_eq!(host.to_string().parse::<Triple>().unwrap(), host);
    }
}
