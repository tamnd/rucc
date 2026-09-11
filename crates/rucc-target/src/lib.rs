//! Target descriptions: triples, and the facts about a target that the rest of the
//! compiler reads rather than hard-codes.
//!
//! Design: `spec/12-abi-and-runtime.md`. Layer rank 2, see `spec/18-package-layout.md`.
//!
//! The rule from `spec/18-package-layout.md` section 18.2 is that there is no
//! target-specific code outside this crate, `rucc-tuple`, `rucc-abi`, `rucc-sysroot` and the
//! per-target rule sets. Those four are one group rather than four exceptions: the tuple names
//! a machine, `rucc-abi` says what its types look like and how its calls are made,
//! `rucc-sysroot` says where its headers and libraries are, and this crate is what the rest of
//! the compiler reads all of it through. Everything a pass
//! needs to know about a target is a field it can read here. That rule is what makes the
//! claim in `spec/10-backend.md` testable, namely that a new target is a rule set and a few
//! data files, and `M10` brings up a fourth target specifically to put a number on it.
//!
//! [`TargetInfo::call`] is the other half of that rule and the one with teeth. How a structure
//! travels between a caller and a callee is the target's answer rather than C's, so the walk to
//! the IR flattens a C type into a [`Shape`] and asks here what form it takes. Every psABI rule
//! is behind [`Call`] and nothing outside this crate matches on an architecture to find one.
//! The rules themselves are `rucc-abi`'s, as data rather than as code, and this crate hands the
//! question over to them. It answers [`None`] on a target whose ABI is not written down yet,
//! which today is AArch64 on Windows and nothing else.
//!
//! # Status
//!
//! Triple parsing and the basic data model are real, which is what `rucc --print-config`
//! reports, and so is the argument classification of every psABI in
//! `spec/12-abi-and-runtime.md` sections 12.2 to 12.5, which `rucc-abi` describes as data and
//! this crate selects between. x86-64's register file is written down,
//! in [`x86_64`], along with what each of the two conventions over it does with each register,
//! what each of its machine instructions does with its operands, and which instructions a frame
//! is made of, which is [`FrameInsts`]. AArch64's and RISC-V's arrive with their backends.
//! Machine models land in `M6`.
//!
//! This crate is tier 3 in `spec/18-package-layout.md` section 18.5: its Rust API is
//! explicitly unstable and will change without a major version bump.

#![doc(html_root_url = "https://docs.rs/rucc-target/0.10.24")]

use std::fmt;
use std::str::FromStr;

use rucc_abi::DataLayout;
use rucc_base::float::Format;
use rucc_tuple::{self as tuple, TargetTuple};

mod abi;
mod branch;
mod frame;
mod operand;
mod regs;
pub mod x86_64;

pub use crate::abi::{Arg, Call, Kind, Pass, Piece, Scalar, Shape, Slot};
pub use crate::branch::{BranchInsts, Fusion};
pub use crate::frame::{ClassMoves, FrameInsts, Probe};
pub use crate::operand::{Constraint, OperandDesc, Role};
pub use crate::regs::{
    CallRegs, ClassInfo, Guard, PhysReg, Places, RegClass, RegFile, Segment, Trace, Where,
};

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
    /// WebAssembly, which is a format for a module rather than for a machine's object file and
    /// is in this list because the target table has two rows that emit one.
    Wasm,
}

impl ObjectFormat {
    /// The name used in diagnostics and in `--print-config`.
    pub const fn as_str(self) -> &'static str {
        match self {
            ObjectFormat::Elf => "elf",
            ObjectFormat::MachO => "macho",
            ObjectFormat::Coff => "coff",
            ObjectFormat::Wasm => "wasm",
        }
    }

    /// The same format as [`rucc_tuple::ObjectFormat`] names it.
    ///
    /// The two enumerations exist because the tuple describes forty two targets and this crate
    /// describes what the compiler emits for one, and they will stay separate for as long as that
    /// is true. This is the one place they are put side by side.
    #[must_use]
    pub const fn from_tuple(format: tuple::ObjectFormat) -> Self {
        match format {
            tuple::ObjectFormat::Elf => ObjectFormat::Elf,
            tuple::ObjectFormat::MachO => ObjectFormat::MachO,
            tuple::ObjectFormat::Coff => ObjectFormat::Coff,
            tuple::ObjectFormat::Wasm => ObjectFormat::Wasm,
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

    /// The same machine as a [`TargetTuple`], which is what the layout and ABI descriptions are
    /// written over.
    ///
    /// The tuple carries ten fields and this carries three, so this fills the other seven in from
    /// their defaults, and every one of those defaults is the answer for the targets this type can
    /// spell. There is no `x32` here and no big-endian AArch64, so the data model and the byte
    /// order follow the architecture, and the sub-architecture, the versions and the float ABI have
    /// nothing to say about any of the combinations.
    ///
    /// The environment is narrowed rather than copied across. This type will hold
    /// `Triple { os: Darwin, env: Gnu }`, because its parser takes the fields by content and
    /// `aarch64-apple-darwin-gnu` is a string somebody can type, and that is not a machine: a
    /// Darwin target has one libc and it is not glibc. A tuple refuses to describe one, so the
    /// pairs that are not machines are mapped to the environment the operating system actually
    /// has.
    ///
    /// # Panics
    ///
    /// Never, for a triple this type can hold, which `every_triple_describes_a_machine` checks by
    /// building all forty eight of them.
    #[must_use]
    pub fn tuple(self) -> TargetTuple {
        let arch = match self.arch {
            Arch::X86_64 => tuple::Arch::X86_64,
            Arch::Aarch64 => tuple::Arch::Aarch64,
            Arch::Riscv64 => tuple::Arch::Riscv64,
        };
        let os = match self.os {
            Os::Linux => tuple::Os::Linux,
            // macOS rather than iOS, because the three field triple cannot tell them apart and
            // this compiler is hosted on the one and not on the other.
            Os::Darwin => tuple::Os::MacOs,
            Os::Windows => tuple::Os::Windows,
            Os::None => tuple::Os::None,
        };
        let env = match (self.os, self.env) {
            (Os::Linux, Env::Musl) => tuple::Env::Musl,
            (Os::Linux, _) => tuple::Env::Gnu,
            // mingw-w64 is a real Windows environment and the one place `gnu` survives the
            // narrowing, because it has a different `long double` from MSVC on the same OS.
            (Os::Windows, Env::Gnu) => tuple::Env::Gnu,
            (Os::Windows, _) => tuple::Env::Msvc,
            // Darwin and freestanding have no libc to name.
            (Os::Darwin | Os::None, _) => tuple::Env::None,
        };
        TargetTuple::builder(arch, os)
            .env(env)
            .build()
            .expect("every triple this type can hold describes a machine")
    }

    /// The triple that describes the same machine as `target`, if this type can spell it.
    ///
    /// The inverse of [`Triple::tuple`], and computed by running that function over every triple
    /// there is rather than by writing the narrowing out a second time. A second table would be a
    /// second thing to keep in step, and the failure it invites is not a compile error: it is one
    /// row of the matrix quietly answering as a neighbour.
    ///
    /// It returns `None` for most of the target table, and that is the honest answer rather than a
    /// gap to be papered over. `rucc-abi` describes the scalar layout of all forty two rows, and
    /// this type holds three fields with three architectures in the first, so seventeen of those
    /// rows have a [`TargetInfo`] and the other twenty five do not. Anything that needs to lay a
    /// record out for `s390x-linux-gnu` needs that gap closed rather than an approximation of it.
    ///
    /// The environment of the answer is the narrowed one, so the triple this gives back is the
    /// canonical spelling of that machine: `Env::None` on Darwin and on a freestanding target,
    /// never the `Env::Gnu` that a parser will accept from a string somebody typed.
    #[must_use]
    pub fn from_tuple(target: TargetTuple) -> Option<Triple> {
        // Four triples narrow onto `x86_64-linux-gnu`, because a Darwin triple claiming glibc is
        // a string somebody can type and not a machine. So a match is not enough on its own: the
        // answer is the candidate whose environment came through the narrowing unchanged, and
        // anything else is only a fallback for the day a narrowing loses a spelling entirely.
        let mut fallback = None;
        for arch in [Arch::X86_64, Arch::Aarch64, Arch::Riscv64] {
            for os in [Os::Linux, Os::Darwin, Os::Windows, Os::None] {
                for env in [Env::None, Env::Gnu, Env::Musl, Env::Msvc] {
                    let candidate = Triple::new(arch, os, env);
                    if candidate.tuple() != target {
                        continue;
                    }
                    // By name rather than by a match on the pair, so that an environment added to
                    // either enumeration does not need a line here. The one name the two spell
                    // differently is the absent one, which the tuple writes as nothing.
                    let survived = match env {
                        Env::None => target.env() == tuple::Env::None,
                        _ => env.as_str() == target.env().as_str(),
                    };
                    if survived {
                        return Some(candidate);
                    }
                    fallback.get_or_insert(candidate);
                }
            }
        }
        fallback
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
    /// The machine this describes, as the ten field tuple rather than as a three field triple.
    ///
    /// It is the tuple because a record layout is a question every row of the target table has an
    /// answer to, and a triple can spell fifteen of the forty two. Nothing else in this type had
    /// to change to widen it: every field below is already derived from `rucc-abi`'s description
    /// of this tuple, and the ones that were not were the bugs.
    pub tuple: TargetTuple,
    /// The sizes, the alignments and the signedness this target's headers were written against.
    ///
    /// The widths below are views of this and the alignments are not, which is the reason it is
    /// kept whole. A `long long` is eight bytes on every row of the table and is aligned to four
    /// on System V i386 and to eight everywhere else, and no width can say that.
    pub scalars: DataLayout,
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
    /// Width of `long double` in bits: 80 bits of x87 stored in 128 on every x86-64 target but
    /// MSVC, 128 of true quad precision on AArch64 Linux and RISC-V, and 64 on Apple's AArch64 and
    /// under MSVC.
    ///
    /// Apple's x86-64 is not one of the 64-bit ones, which is the trap. The change to a `double`
    /// came with AArch64 and the Intel answer stayed as it was, so `x86_64-apple-darwin` and
    /// `x86_64-unknown-linux-gnu` agree here and `aarch64-apple-darwin` is the odd one.
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
    ///
    /// [`None`] on a machine whose widest format is a `double`, which is 32-bit ARM and wasm32.
    /// The type does not exist there and neither reference defines the macros that describe it,
    /// so the honest answer is that there is no format rather than a `double` in its place.
    pub float64x_format: Option<Format>,
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
    /// The widest access, in bits, this machine performs atomically without taking a lock.
    ///
    /// It is what `__atomic_always_lock_free` and `__atomic_is_lock_free` answer from, and it is
    /// a claim about what this compiler emits rather than about what the processor is capable of.
    /// Sixty four on every target here. x86-64 does sixteen bytes atomically with `cmpxchg16b`,
    /// which is not in the baseline the psABI names and which nothing in this compiler writes, and
    /// AArch64 does the same with its pair instructions, which nothing writes either. A target
    /// that answered yes for sixteen bytes and then called a library that has to take a lock for
    /// them would have two answers to one question, and the wrong one is the one in the header.
    pub lock_free_width: u32,
    /// The object format to emit.
    pub object_format: ObjectFormat,
    /// How bit-fields are allocated into storage, which is the one record layout question where
    /// two targets in this table run different algorithms rather than the same one over different
    /// numbers.
    pub bit_field_style: BitFieldStyle,
    /// Whether an unnamed bit-field raises the record's alignment the way a named one does.
    ///
    /// Almost everywhere it does not, which is why `struct { char c; int :20; }` is four bytes
    /// aligned to one on x86-64 and four aligned to four with the field named. AAPCS64 says
    /// otherwise and says it for the zero width member too, so `struct { unsigned :0; }` is
    /// aligned to four on AArch64 Linux and to one on Apple's AArch64, on Windows on AArch64, on
    /// x86-64 and on RISC-V. Measured with the pinned reference across every row that has one,
    /// because it is neither an architecture rule nor an operating system rule: it is the ABI, and
    /// Apple and Microsoft each dropped it.
    ///
    /// Windows says yes as well, and there it is not AAPCS64 but Microsoft's own rule, which is
    /// why the two facts are separate fields rather than one. In a `union` the Microsoft rule goes
    /// further and no bit-field contributes alignment at all, named or not, so this field is only
    /// half the answer there and [`BitFieldStyle`] carries the other half.
    pub unnamed_bit_field_aligns: bool,
    /// How large a record with no storage in it is, in bytes, before its alignment is applied.
    ///
    /// Zero everywhere but MSVC, where it is four. A `struct` with no members is not C at all, it
    /// is a GNU extension, and C++ gives it a size of one, so there is no standard to read the
    /// answer out of and the number has to come from whatever else compiles for the target. On
    /// mingw that is GCC and the answer is zero. On MSVC it is clang, because MSVC itself rejects
    /// the declaration outright, and clang's Microsoft record layout gives it four bytes and gives
    /// an array of three of them twelve. So this is a fact about the environment and not about the
    /// operating system, which is the one place in this type where those two come apart in that
    /// direction.
    ///
    /// It covers a record with no members and a record whose only members occupy nothing, which is
    /// the zero width bit-field, the zero length array and the flexible array member. All four
    /// were measured and all four agree.
    pub empty_record_size: u64,
    /// What `__builtin_va_list` is, which is the type every `va_list` in every header is a
    /// typedef of.
    ///
    /// [`None`] on a target whose answer is a type this crate does not build yet. 32-bit ARM's is
    /// a structure of one pointer and s390x's is a structure of four members, and neither is any
    /// of the four below. A target with no backend cannot compile a call to `va_arg` in any case,
    /// so saying so beats naming a neighbour's type and having a header believe it.
    pub va_list: Option<VaList>,
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

/// How a target allocates bit-fields into storage.
///
/// Everything else about laying a record out is one algorithm reading different sizes and
/// alignments per target. This is not: the two answers below place the same members at different
/// offsets and give the same struct different sizes, and no amount of changing what an `int` is
/// turns one into the other. `struct { unsigned m:3; char c; }` is four bytes with the `char` at
/// offset one under the first and eight bytes with it at offset four under the second.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
// Deliberately not `#[non_exhaustive]`, for the reason [`Arch`] is not: a third answer here is a
// third algorithm to write, and every place that chooses between them should stop compiling until
// it does.
pub enum BitFieldStyle {
    /// The Itanium C++ ABI's rule, which every psABI in this table except Windows follows. A
    /// bit-field goes at the next free bit unless that would make it span more storage than its
    /// own type occupies, in which case it starts at the next boundary of its alignment. Storage
    /// is shared between members of different types freely, so `struct { char a:3; unsigned b:3; }`
    /// is four bytes with both fields in the first one.
    Itanium,
    /// Microsoft's rule, which both Windows environments follow and not only MSVC. A run of
    /// bit-fields is allocated into a unit the size and alignment of the declared type, and the
    /// unit is closed both when the next member's declared type has a different size and when the
    /// field does not fit in what is left. An ordinary member closes a unit too, and the closed
    /// unit occupies its whole declared size whether or not the bits were used. So the same struct
    /// is eight bytes: a one byte unit for the `char` and a four byte one for the `unsigned`,
    /// aligned to four.
    Microsoft,
}

impl BitFieldStyle {
    /// The name used in `--print-config`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            BitFieldStyle::Itanium => "itanium",
            BitFieldStyle::Microsoft => "microsoft",
        }
    }
}

/// A width in bits, from a size in bytes.
///
/// The fields here are widths because that is what a predefined macro and a diagnostic say, and a
/// layout is sizes because that is what `sizeof` says. The conversion belongs at the one boundary
/// between them rather than at every reader of one of these fields.
fn bits(bytes: u64) -> u32 {
    u32::try_from(bytes * 8).expect("no standard type is four billion bits wide")
}

impl TargetInfo {
    /// The description of `triple`.
    ///
    /// The three field triple spells fifteen of the forty two rows of the target table, which is
    /// every row with a backend and every row a driver will be handed today, so this is what the
    /// compiler proper calls. [`TargetInfo::for_tuple`] is the one that answers for the whole
    /// table.
    #[must_use]
    pub fn new(triple: Triple) -> Self {
        Self::for_tuple(triple.tuple())
    }

    /// The description of `target`.
    ///
    /// Every row of the target table has one of these, whether or not there is a backend that can
    /// emit code for it, because laying a record out and reading a header are questions that do
    /// not need a backend. The fields that genuinely need one say so: [`TargetInfo::regs`] is
    /// empty and [`TargetInfo::call_regs`] is [`None`] for an architecture whose register file is
    /// not written down.
    #[must_use]
    pub fn for_tuple(target: TargetTuple) -> Self {
        // Every size, alignment and signedness below is `rucc-abi`'s answer over the ten field
        // tuple rather than a match written out here. They were written out here, and the copy was
        // wrong about `x86_64-apple-darwin`, whose `long double` is the eighty bit x87 format in
        // sixteen bytes and not a `double`: Apple made that change on AArch64 and left the Intel
        // answer alone, and a rule keyed on the operating system takes both.
        let layout = DataLayout::for_target(target);
        // AArch64, RISC-V and everything else with a row and no backend have register files and
        // this crate has not written them down yet. They arrive with the backends that need them,
        // in M6 and M7.
        let regs = match target.arch() {
            tuple::Arch::X86_64 => &x86_64::REGS,
            _ => &RegFile::EMPTY,
        };
        let call_regs = match (target.arch(), target.os()) {
            (tuple::Arch::X86_64, tuple::Os::Windows) => Some(&x86_64::WIN64),
            // Apple's x86-64 follows SysV, and its divergences from it are on AArch64.
            (tuple::Arch::X86_64, _) => Some(&x86_64::SYSV),
            _ => None,
        };
        Self {
            tuple: target,
            scalars: layout,
            pointer_width: bits(layout.pointer_size),
            little_endian: target.is_little_endian(),
            char_is_signed: layout.char_is_signed,
            long_width: bits(layout.long_size),
            long_double_width: bits(layout.long_double.size),
            long_double_format: layout.long_double.format,
            float64x_format: float64x_format(target),
            wchar_width: bits(layout.wchar_size),
            wchar_is_signed: layout.wchar_is_signed,
            bit_int_granule: bit_int_granule(target),
            // Eight bytes everywhere, for the reason the field gives: it is the widest access this
            // compiler writes an instruction for, and every one of these machines has a wider one
            // that nothing here reaches. It is a claim about the code this compiler emits, so the
            // day a backend emits a sixteen byte atomic is the day this stops being one number.
            lock_free_width: 64,
            object_format: ObjectFormat::from_tuple(target.object_format()),
            bit_field_style: bit_field_style(target),
            unnamed_bit_field_aligns: unnamed_bit_field_aligns(target),
            // The environment and not the operating system, so `x86_64-windows-gnu` keeps GCC's
            // zero while `x86_64-windows-msvc` takes clang's four.
            empty_record_size: match target.env() {
                tuple::Env::Msvc => 4,
                _ => 0,
            },
            va_list: va_list(target),
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

/// The format `_Float64x` is, where the target has one.
fn float64x_format(target: TargetTuple) -> Option<Format> {
    match target.arch() {
        // The x87 unit is on the machine whatever the operating system says a `long double` is,
        // so `x86_64-apple-darwin` and `x86_64-windows-msvc` both have an eighty bit `_Float64x`
        // and an eight byte `long double`.
        tuple::Arch::X86_64 | tuple::Arch::X86 => Some(Format::X87Extended),
        tuple::Arch::Aarch64
        | tuple::Arch::Riscv64
        | tuple::Arch::Riscv32
        | tuple::Arch::LoongArch64
        | tuple::Arch::S390x
        | tuple::Arch::PowerPc64 => Some(Format::Quad),
        // Nothing on these machines is wider than a `double`, so there is no type here to
        // describe and neither reference defines the macros that would describe it.
        tuple::Arch::Arm | tuple::Arch::Arm64Ec | tuple::Arch::Wasm32 => None,
    }
}

/// The granule a `_BitInt` wider than 64 bits is laid out in, in bits.
fn bit_int_granule(target: TargetTuple) -> u32 {
    match target.arch() {
        // AAPCS64 says a `_BitInt` above sixty four bits is an array of `__int128`, which is the
        // one psABI that departs from the register width here.
        tuple::Arch::Aarch64 | tuple::Arch::Arm64Ec => 128,
        // Everywhere else it is the width of a general purpose register, which is what the psABIs
        // that have written the rule down all say and what both references do on the rows that
        // have not.
        tuple::Arch::X86 | tuple::Arch::Arm | tuple::Arch::Riscv32 => 32,
        tuple::Arch::X86_64
        | tuple::Arch::Riscv64
        | tuple::Arch::LoongArch64
        | tuple::Arch::PowerPc64
        | tuple::Arch::S390x
        | tuple::Arch::Wasm32 => 64,
    }
}

/// How this target allocates bit-fields into storage.
///
/// Keyed on the operating system rather than the environment, because mingw's answer here is
/// Microsoft's and not GCC's. That is the whole reason it is not a guess: a rule keyed on
/// `Env::Msvc` gets `x86_64-windows-gnu` wrong by four bytes on a struct of an `unsigned :3` and a
/// `char`, and gets it wrong quietly.
fn bit_field_style(target: TargetTuple) -> BitFieldStyle {
    match target.os() {
        tuple::Os::Windows => BitFieldStyle::Microsoft,
        _ => BitFieldStyle::Itanium,
    }
}

/// Whether an unnamed bit-field raises the record's alignment the way a named one does.
///
/// AAPCS says it does, on both widths of ARM, and Apple and Microsoft each dropped that rule.
/// Microsoft then put its own rule in the same place for a `struct`, so Windows says yes again by
/// a different route, and says something else entirely for a `union`, which [`BitFieldStyle`]
/// carries rather than this.
fn unnamed_bit_field_aligns(target: TargetTuple) -> bool {
    match (target.arch(), target.os()) {
        (_, tuple::Os::Windows) => true,
        // A freestanding ARM target is AAPCS proper, so it says yes: there is no operating system
        // there to have dropped it.
        (tuple::Arch::Aarch64 | tuple::Arch::Arm | tuple::Arch::Arm64Ec, os) => !os.is_darwin(),
        _ => false,
    }
}

/// What `__builtin_va_list` is on this target, where this crate can build the type.
fn va_list(target: TargetTuple) -> Option<VaList> {
    match (target.arch(), target.os()) {
        // Windows passes every argument in one place and spills the register ones next to the
        // stack ones, so the list is an address, and Apple does the same on AArch64.
        (_, tuple::Os::Windows) => Some(VaList::CharPointer),
        (tuple::Arch::Aarch64, os) if os.is_darwin() => Some(VaList::CharPointer),
        (tuple::Arch::Aarch64, _) => Some(VaList::Aapcs),
        // The x32 ABI's list is the same structure with four byte pointers in it, which is what
        // building it out of this target's pointer type gives, so it is the same answer.
        (tuple::Arch::X86_64, _) => Some(VaList::SysV),
        (tuple::Arch::X86, _) => Some(VaList::CharPointer),
        (tuple::Arch::Riscv64 | tuple::Arch::Riscv32 | tuple::Arch::LoongArch64, _)
        | (tuple::Arch::Wasm32, _) => Some(VaList::VoidPointer),
        // 32-bit ARM's is a structure of one pointer, s390x's is a structure of four members, and
        // PowerPC's is a structure of five. None of them is any of the four types above and this
        // crate does not build them, so it says so rather than naming a neighbour's.
        (
            tuple::Arch::Arm | tuple::Arch::S390x | tuple::Arch::PowerPc64 | tuple::Arch::Arm64Ec,
            _,
        ) => None,
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
    fn apples_x86_64_is_not_one_of_the_targets_that_narrowed_long_double() {
        // The bug the layout facts moving to `rucc-abi` fixed. This crate used to decide the
        // width from the operating system, which took both Apple targets, and Apple made the
        // change on AArch64 only. `facts/x86_64-macos.facts` in tamnd/rucc-cross records
        // `long_double_format=x87_extended` with `sizeof_long_double=16`, from a reference
        // compiler, and this used to answer a sixty four bit `double`.
        //
        // It is the quiet kind of wrong. `sizeof(long double)` came out at eight where the
        // headers say sixteen, so `printf("%Lf")` read the wrong bytes and every structure with
        // a `long double` in it laid out differently from the system's own.
        let mac = TargetInfo::new("x86_64-apple-darwin".parse().unwrap());
        assert_eq!(mac.long_double_width, 128);
        assert_eq!(mac.long_double_format, Format::X87Extended);

        let linux = TargetInfo::new("x86_64-unknown-linux-gnu".parse().unwrap());
        assert_eq!(
            (mac.long_double_width, mac.long_double_format),
            (linux.long_double_width, linux.long_double_format)
        );
    }

    #[test]
    fn every_triple_describes_a_machine() {
        // `Triple::tuple` panics on a pair that is not a machine and this is what says there is
        // no such pair. All forty eight combinations, including the ones the parser will produce
        // from a string somebody can type and no machine has, such as a Darwin target claiming
        // glibc.
        let mut built = 0;
        for arch in [Arch::X86_64, Arch::Aarch64, Arch::Riscv64] {
            for os in [Os::Linux, Os::Darwin, Os::Windows, Os::None] {
                for env in [Env::None, Env::Gnu, Env::Musl, Env::Msvc] {
                    let triple = Triple::new(arch, os, env);
                    let tuple = triple.tuple();
                    assert_eq!(tuple.pointer_width(), 64, "{triple}");
                    // The one field the narrowing has to preserve, because mingw and MSVC are the
                    // same operating system with two different `long double`s.
                    if os == Os::Windows {
                        let expected = match env {
                            Env::Gnu => rucc_tuple::Env::Gnu,
                            _ => rucc_tuple::Env::Msvc,
                        };
                        assert_eq!(tuple.env(), expected, "{triple}");
                    }
                    built += 1;
                }
            }
        }
        assert_eq!(built, 48);
    }

    #[test]
    fn from_tuple_undoes_the_narrowing() {
        // Every triple's tuple comes back as a triple describing the same machine. It is not
        // always the triple it started as, because the narrowing is many to one: a Darwin target
        // claiming glibc and the same one claiming nothing are one machine, and the answer is the
        // spelling that names no libc.
        for arch in [Arch::X86_64, Arch::Aarch64, Arch::Riscv64] {
            for os in [Os::Linux, Os::Darwin, Os::Windows, Os::None] {
                for env in [Env::None, Env::Gnu, Env::Musl, Env::Msvc] {
                    let triple = Triple::new(arch, os, env);
                    let back = Triple::from_tuple(triple.tuple())
                        .unwrap_or_else(|| panic!("{triple} has a tuple and no way back"));
                    assert_eq!(back.tuple(), triple.tuple(), "{triple}");
                    assert_eq!(back.arch, arch, "{triple}");
                    assert_eq!(back.os, os, "{triple}");
                }
            }
        }
    }

    #[test]
    fn from_tuple_gives_the_canonical_environment() {
        let musl = Triple::from_tuple("aarch64-linux-musl".parse().unwrap()).unwrap();
        assert_eq!(musl, Triple::new(Arch::Aarch64, Os::Linux, Env::Musl));
        let gnu = Triple::from_tuple("x86_64-linux-gnu".parse().unwrap()).unwrap();
        assert_eq!(gnu, Triple::new(Arch::X86_64, Os::Linux, Env::Gnu));
        // Darwin and freestanding name no libc, so the answer does too, even though the parser
        // will hand this type a Darwin triple with `gnu` on the end.
        let macos = Triple::from_tuple("aarch64-macos".parse().unwrap()).unwrap();
        assert_eq!(macos, Triple::new(Arch::Aarch64, Os::Darwin, Env::None));
        let bare = Triple::from_tuple("riscv64-none".parse().unwrap()).unwrap();
        assert_eq!(bare, Triple::new(Arch::Riscv64, Os::None, Env::None));
        // The two Windows environments stay apart, which is the whole reason the narrowing keeps
        // the environment there and nowhere else.
        let mingw = Triple::from_tuple("x86_64-windows-gnu".parse().unwrap()).unwrap();
        assert_eq!(mingw.env, Env::Gnu);
        let msvc = Triple::from_tuple("x86_64-windows-msvc".parse().unwrap()).unwrap();
        assert_eq!(msvc.env, Env::Msvc);
    }

    #[test]
    fn from_tuple_says_no_rather_than_saying_something_near() {
        // Twenty five of the forty two rows have no triple, and the answer is `None` rather than
        // a neighbour. `rucc-abi` knows the scalar layout of every one of these and this type
        // cannot hold any of them, which is the gap the record layout engine inherits.
        for tuple in [
            "i686-linux-gnu",
            "armv7-linux-gnueabihf",
            "s390x-linux-gnu",
            "powerpc64le-linux-gnu",
            "loongarch64-linux-gnu",
            "x86_64-linux-gnux32",
            "aarch64-linux-android",
            "aarch64-ios",
            "wasm32-wasip1",
            "x86_64-freebsd",
        ] {
            let target = tuple.parse().unwrap();
            assert_eq!(Triple::from_tuple(target), None, "{tuple}");
        }
    }

    #[test]
    fn mingw_and_msvc_are_one_operating_system_with_two_long_doubles() {
        // The narrowing in `Triple::tuple` keeps the environment on Windows for this reason and
        // throws it away everywhere else. GCC's Windows targets keep the eighty bit `long double`
        // and Microsoft's make it a `double`, on the same processor and the same OS.
        let mingw = TargetInfo::new("x86_64-pc-windows-gnu".parse().unwrap());
        assert_eq!(mingw.long_double_width, 128);
        assert_eq!(mingw.long_double_format, Format::X87Extended);

        let msvc = TargetInfo::new("x86_64-pc-windows-msvc".parse().unwrap());
        assert_eq!(msvc.long_double_width, 64);
        assert_eq!(msvc.long_double_format, Format::Double);

        // And they agree about everything the operating system does decide.
        assert_eq!(mingw.long_width, msvc.long_width);
        assert_eq!(mingw.wchar_width, msvc.wchar_width);
        assert_eq!(mingw.object_format, msvc.object_format);
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
        assert_eq!(linux.va_list, Some(VaList::SysV));
        // x86-64 Darwin follows SysV here, and AArch64 Darwin does not follow AAPCS64.
        let mac = TargetInfo::new("x86_64-apple-darwin".parse().unwrap());
        assert_eq!(mac.va_list, Some(VaList::SysV));
        let arm_mac = TargetInfo::new("aarch64-apple-darwin".parse().unwrap());
        assert_eq!(arm_mac.va_list, Some(VaList::CharPointer));
        let arm = TargetInfo::new("aarch64-unknown-linux-gnu".parse().unwrap());
        assert_eq!(arm.va_list, Some(VaList::Aapcs));
        // Windows passes everything one way on both processors, so both get the simple one.
        let win = TargetInfo::new("x86_64-pc-windows-msvc".parse().unwrap());
        assert_eq!(win.va_list, Some(VaList::CharPointer));
        let arm_win = TargetInfo::new("aarch64-pc-windows-msvc".parse().unwrap());
        assert_eq!(arm_win.va_list, Some(VaList::CharPointer));
        let riscv = TargetInfo::new("riscv64-unknown-linux-gnu".parse().unwrap());
        assert_eq!(riscv.va_list, Some(VaList::VoidPointer));
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
        assert_eq!(x86.float64x_format, Some(Format::X87Extended));
        let arm = TargetInfo::new("aarch64-unknown-linux-gnu".parse().unwrap());
        assert_eq!(arm.float64x_format, Some(Format::Quad));
        let riscv = TargetInfo::new("riscv64-unknown-linux-gnu".parse().unwrap());
        assert_eq!(riscv.float64x_format, Some(Format::Quad));

        let mac = TargetInfo::new("aarch64-apple-darwin".parse().unwrap());
        assert_eq!(mac.long_double_format, Format::Double);
        assert_eq!(mac.float64x_format, Some(Format::Quad));
        let windows = TargetInfo::new("x86_64-pc-windows-msvc".parse().unwrap());
        assert_eq!(windows.long_double_format, Format::Double);
        assert_eq!(windows.float64x_format, Some(Format::X87Extended));
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
