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
//! is made of, which is [`FrameInsts`]. AArch64's register file and the two conventions over it,
//! AAPCS64 and Apple's, are in [`aarch64`], and its instructions arrive with its backend in `M6`.
//! RISC-V's arrive with its own. Machine models land in `M6`.
//!
//! This crate is tier 3 in `spec/18-package-layout.md` section 18.5: its Rust API is
//! explicitly unstable and will change without a major version bump.

#![doc(html_root_url = "https://docs.rs/rucc-target/0.25.0")]

use std::fmt;
use std::str::FromStr;

use rucc_abi::DataLayout;
use rucc_base::float::Format;
use rucc_tuple::{self as tuple, TargetTuple};

pub mod aarch64;
mod abi;
mod bits;
mod branch;
mod counts;
mod flags;
mod frame;
pub mod isa;
mod machine;
mod named;
mod operand;
mod regs;
mod short;
pub mod template;
mod timing;
mod typenames;
pub mod wasm;
pub mod x86;
pub mod x86_64;

pub use crate::abi::{
    AbiDescription, Arg, BitInts, Call, Cleanup, Convention, Kind, Narrow, Pass, Piece, Scalar,
    Shape, Slot, Variadic,
};
pub use crate::bits::BitInsts;
pub use crate::branch::{BranchInsts, Fusion, Move};
pub use crate::counts::{BitCount, CountInst};
pub use crate::flags::{Compare, FlagInsts, Reader, Reads, Zeroing};
pub use crate::frame::{
    Canary, ClassMoves, FrameInsts, Kept, Pair, Probe, Signing, SpillMove, Targets, Thunks,
};
pub use crate::isa::{Choices, Feature, Isa, Target, TargetRefusal};
pub use crate::machine::{Address, MachineInsts};
pub use crate::operand::{Constraint, OperandDesc, Role};
pub use crate::regs::{
    CallRegs, Chkstk, ClassInfo, Conventions, Guard, PhysReg, Places, RegClass, RegFile, Segment,
    Trace, Where,
};
pub use crate::short::{Copied, Narrowed, ShortInsts, Spread, Stepped, Tested, Zeroed};
pub use crate::timing::{Timing, TimingInsts, Unit};
pub use crate::typenames::{Lane, TypeName};

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
    /// 32-bit x86, the i386 of the psABI and the i686 of a triple, which issue #2247 brings up.
    X86,
    /// 32-bit WebAssembly. The front end knows it and there is no backend yet, so the driver
    /// stops before code generation. Design: #2863, and the WebAssembly plan, decision D1.
    Wasm32,
}

impl Arch {
    /// Every architecture, in the order of the enumeration.
    pub const ALL: [Arch; 5] =
        [Arch::X86_64, Arch::Aarch64, Arch::Riscv64, Arch::X86, Arch::Wasm32];

    /// Pointer width in bits.
    pub const fn pointer_width(self) -> u32 {
        match self {
            Arch::X86_64 | Arch::Aarch64 | Arch::Riscv64 => 64,
            Arch::X86 | Arch::Wasm32 => 32,
        }
    }

    /// Whether the target is little-endian.
    pub const fn is_little_endian(self) -> bool {
        match self {
            Arch::X86_64 | Arch::Aarch64 | Arch::Riscv64 | Arch::X86 | Arch::Wasm32 => true,
        }
    }

    /// The name as it appears in a triple.
    pub const fn as_str(self) -> &'static str {
        match self {
            Arch::X86_64 => "x86_64",
            Arch::Aarch64 => "aarch64",
            Arch::Riscv64 => "riscv64",
            Arch::X86 => "i686",
            Arch::Wasm32 => "wasm32",
        }
    }

    /// Whether code for this architecture is WebAssembly, which has a back end of its own.
    #[must_use]
    pub const fn is_wasm(self) -> bool {
        matches!(self, Arch::Wasm32)
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
    /// The WebAssembly System Interface, at one of its previews.
    Wasi(Preview),
}

/// A preview of WASI. Each one is a different set of imports and a different libc build, and
/// preview 3 also moves the stack pointer into a context, so each one is a target of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Preview {
    /// WASI 0.1, the core module interface that every engine runs.
    P1,
    /// WASI 0.2, a component that wraps a core module built as for preview 1.
    P2,
    /// WASI 0.3, with asynchronous calls and the stack pointer in the context.
    P3,
}

impl Preview {
    /// The number after the `p`.
    #[must_use]
    pub const fn number(self) -> u32 {
        match self {
            Preview::P1 => 1,
            Preview::P2 => 2,
            Preview::P3 => 3,
        }
    }
}

impl Os {
    /// Every operating system, in the order of the enumeration.
    pub const ALL: [Os; 7] = [
        Os::Linux,
        Os::Darwin,
        Os::Windows,
        Os::None,
        Os::Wasi(Preview::P1),
        Os::Wasi(Preview::P2),
        Os::Wasi(Preview::P3),
    ];

    /// The name as it appears in a triple.
    pub const fn as_str(self) -> &'static str {
        match self {
            Os::Linux => "linux",
            Os::Darwin => "darwin",
            Os::Windows => "windows",
            Os::None => "none",
            Os::Wasi(Preview::P1) => "wasip1",
            Os::Wasi(Preview::P2) => "wasip2",
            Os::Wasi(Preview::P3) => "wasip3",
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
    /// Every environment, in the order of the enumeration.
    pub const ALL: [Env; 4] = [Env::None, Env::Gnu, Env::Musl, Env::Msvc];

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
    /// The two enumerations exist because the tuple describes forty three targets and this crate
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

/// Where the program's code and static data are promised to be, which is `-mcmodel=`.
///
/// The model is a promise about addresses that the code generator is allowed to believe. It says
/// nothing about which link is coming, which is `-fPIC` and friends, and it decides only how an
/// address is written into an instruction. x86-64 is the only machine with more than one here.
///
/// Design: `spec/11-asm-objects-debug.md` section 11.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
// Deliberately not `#[non_exhaustive]`, for the reason [`Arch`] is not: a new model is a new set of
// addressing forms and every place that picks one should stop compiling until it says which.
pub enum CodeModel {
    /// Everything within 2 GiB of everything else, reached from the instruction pointer. The
    /// default on every target and every hosted program's model.
    #[default]
    Small,
    /// The top 2 GiB of the address space, from `0xffffffff80000000` up, which is where every
    /// x86-64 Linux kernel is linked. An address there is a 32 bit number sign extended, so an
    /// instruction may carry it as an immediate (`movq $sym, %rax`) or as the displacement of an
    /// indexed address (`sym(,%rdi,8)`), both with `R_X86_64_32S`.
    Kernel,
    /// AArch64's model for an image of 1 MiB, where gcc reaches a name with one `adr`. The code
    /// written is the small model's, since `adrp` and `add` reach everything `adr` does, so this
    /// only changes which `__AARCH64_CMODEL_*__` is defined. The arm64 kernel builds its vDSO
    /// with it.
    Tiny,
}

impl CodeModel {
    /// The spelling `-mcmodel=` takes, and the one `__code_model_*__` is named after.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            CodeModel::Small => "small",
            CodeModel::Kernel => "kernel",
            CodeModel::Tiny => "tiny",
        }
    }
}

/// Which functions sign the return address they were called with, which is the `pac-ret` part of
/// `-mbranch-protection=` and the whole of `-msign-return-address=`. AArch64 only.
///
/// A signed address is checked again just before the return, so one an overflow wrote over faults
/// instead of being returned to. The signature goes in the bits above the address and is made from
/// the address, the stack pointer and a key the program never sees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SignReturn {
    /// No function signs it, which is the default.
    #[default]
    None,
    /// The functions that save it to the stack, which is every one that calls something and every
    /// one that keeps a frame record. A leaf keeps it in `x30` the whole time, where nothing can
    /// write over it.
    NonLeaf,
    /// Every function, which is `+leaf`. What the arm64 kernel asks for.
    All,
}

/// What `-mbranch-protection=` asks for on AArch64: which functions sign their return address, and
/// whether every place an indirect branch may land opens with a `bti`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct BranchProtection {
    /// Which functions sign their return address.
    pub sign: SignReturn,
    /// Whether a function opens with `bti c` and every label an indirect jump may reach with
    /// `bti j`, so that a machine that checks branch targets faults on a jump anywhere else.
    pub bti: bool,
}

impl SignReturn {
    /// Parses the part after `-msign-return-address=`, which is `none`, `non-leaf` or `all`.
    ///
    /// # Errors
    ///
    /// Anything else, with the reason in words.
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "none" => Ok(Self::None),
            "non-leaf" => Ok(Self::NonLeaf),
            "all" => Ok(Self::All),
            _ => Err(format!(
                "`{s}` is not a scope to sign return addresses in, which is none, \
                 non-leaf or all"
            )),
        }
    }
}

impl BranchProtection {
    /// Parses the part after `-mbranch-protection=`, which is `none`, `standard`, or `pac-ret`
    /// with `+leaf` after it and `bti`, joined with `+` in any order, as gcc reads it.
    ///
    /// # Errors
    ///
    /// The B key and the guarded control stack, which are not written yet, and anything gcc does
    /// not know, with the reason in words.
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "none" => return Ok(Self::default()),
            "standard" => return Ok(Self { sign: SignReturn::NonLeaf, bti: true }),
            _ => {}
        }
        let mut out = Self::default();
        let mut leaf = false;
        for part in s.split('+') {
            match part {
                "pac-ret" => out.sign = SignReturn::NonLeaf,
                "leaf" => leaf = true,
                "bti" => out.bti = true,
                "b-key" | "gcs" => {
                    return Err(format!(
                        "`{part}` is not supported yet, only pac-ret, leaf and bti are"
                    ));
                }
                _ => {
                    return Err(format!(
                        "`{s}` is not a branch protection, which is none, standard, or pac-ret, \
                         leaf and bti joined with +"
                    ));
                }
            }
        }
        if leaf {
            if out.sign == SignReturn::None {
                return Err(format!("`{s}` says leaf without pac-ret, which leaf is a part of"));
            }
            out.sign = SignReturn::All;
        }
        Ok(out)
    }

    /// Whether it asks for anything at all.
    #[must_use]
    pub fn any(self) -> bool {
        self != Self::default()
    }
}

/// What the x86 speculation hardening flags ask of indirect branches and returns.
///
/// Each field is one flag's request and all of them are off by default, which is gcc's default.
/// The kernel turns them on for `MITIGATION_RETPOLINE`, `MITIGATION_RETHUNK` and `MITIGATION_SLS`,
/// and objtool then checks that every branch it asked about was written the way gcc writes it.
/// Nothing here is written by the compiler into a section of its own: `.retpoline_sites` and
/// `.return_sites` are objtool's, built from the calls and jumps it finds.
///
/// Design: `spec/04-driver-and-cli.md` section 4.12.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Speculation {
    /// `-mindirect-branch=`: where a call or jump through a register goes instead, which for
    /// `thunk-extern` is `call __x86_indirect_thunk_rax` for `call *%rax`.
    pub indirect: Thunk,
    /// `-mindirect-branch-cs-prefix`: a call or jump to the thunk for `r8` to `r15` has a code
    /// segment override in front of it.
    pub padded: bool,
    /// `-mfunction-return=`: where a return goes instead, which for `thunk-extern` is
    /// `jmp __x86_return_thunk`.
    pub returns: Thunk,
    /// `-mharden-sls=return` or `all`: an `int3` after every return.
    pub after_return: bool,
    /// `-mharden-sls=indirect-jmp` or `all`: an `int3` after every jump through a register, and
    /// after the jump to a thunk that takes its place.
    pub after_jump: bool,
}

impl Speculation {
    /// Whether anything at all is asked for.
    #[must_use]
    pub const fn any(self) -> bool {
        self.indirect.taken() || self.returns.taken() || self.after_return || self.after_jump
    }
}

/// The four answers gcc takes for `-mindirect-branch=` and `-mfunction-return=`.
///
/// The three that are not `keep` all send the branch through the same few instructions, a call
/// that pushes a return address, a loop that catches a processor guessing where the return goes,
/// and a return to the real address. What differs is where those instructions are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Thunk {
    /// `keep`: the branch is left alone.
    #[default]
    Keep,
    /// `thunk-extern`: the branch goes to a thunk the program links in from somewhere else,
    /// which is how the kernel builds.
    Extern,
    /// `thunk`: the branch goes to the same thunk, and the unit carries its own copy of it in a
    /// COMDAT group, so the linker keeps one.
    Comdat,
    /// `thunk-inline`: the thunk's instructions are written where the branch was, which is how
    /// the kernel builds its vDSO.
    Inline,
}

impl Thunk {
    /// Whether the branch is rewritten at all.
    #[must_use]
    pub const fn taken(self) -> bool {
        !matches!(self, Self::Keep)
    }

    /// Whether the branch goes to a thunk by name, which is every answer but `keep` and
    /// `thunk-inline`.
    #[must_use]
    pub const fn named(self) -> bool {
        matches!(self, Self::Extern | Self::Comdat)
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

    /// Whether the architecture runs on the operating system. wasm runs on WASI or on nothing,
    /// and WASI runs nothing but wasm. Every other pair is a machine.
    #[must_use]
    pub const fn pairs(arch: Arch, os: Os) -> bool {
        match (arch, os) {
            (Arch::Wasm32, Os::Linux | Os::Darwin | Os::Windows) => false,
            (Arch::Wasm32, Os::None | Os::Wasi(_)) => true,
            (_, Os::Wasi(_)) => false,
            _ => true,
        }
    }

    /// The object file format for this target.
    ///
    /// It follows the operating system, except with no operating system, where it follows the
    /// architecture: freestanding wasm is a wasm module and every other freestanding target is ELF.
    #[must_use]
    pub const fn object_format(self) -> ObjectFormat {
        match (self.os, self.arch) {
            (Os::Wasi(_), _) | (Os::None, Arch::Wasm32) => ObjectFormat::Wasm,
            (Os::Linux | Os::None, _) => ObjectFormat::Elf,
            (Os::Darwin, _) => ObjectFormat::MachO,
            (Os::Windows, _) => ObjectFormat::Coff,
        }
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
    /// building all eighty of them whose architecture pairs with the operating system.
    #[must_use]
    pub fn tuple(self) -> TargetTuple {
        let arch = match self.arch {
            Arch::X86_64 => tuple::Arch::X86_64,
            Arch::Aarch64 => tuple::Arch::Aarch64,
            Arch::Riscv64 => tuple::Arch::Riscv64,
            Arch::X86 => tuple::Arch::X86,
            Arch::Wasm32 => tuple::Arch::Wasm32,
        };
        let os = match self.os {
            Os::Linux => tuple::Os::Linux,
            // macOS rather than iOS, because the three field triple cannot tell them apart and
            // this compiler is hosted on the one and not on the other.
            Os::Darwin => tuple::Os::MacOs,
            Os::Windows => tuple::Os::Windows,
            Os::None => tuple::Os::None,
            Os::Wasi(_) => tuple::Os::Wasi,
        };
        let env = match (self.os, self.env) {
            (Os::Linux, Env::Musl) => tuple::Env::Musl,
            (Os::Linux, _) => tuple::Env::Gnu,
            // mingw-w64 is a real Windows environment and the one place `gnu` survives the
            // narrowing, because it has a different `long double` from MSVC on the same OS.
            (Os::Windows, Env::Gnu) => tuple::Env::Gnu,
            (Os::Windows, _) => tuple::Env::Msvc,
            // Darwin and freestanding have no libc to name, and WASI has one libc, wasi-libc.
            (Os::Darwin | Os::None | Os::Wasi(_), _) => tuple::Env::None,
        };
        let mut builder = TargetTuple::builder(arch, os).env(env);
        // The tuple keeps the preview as the version of the operating system.
        if let Os::Wasi(preview) = self.os {
            builder = builder.os_version(tuple::Version::major(preview.number()));
        }
        builder.build().expect("every triple this type can hold describes a machine")
    }

    /// The triple that describes the same machine as `target`, if this type can spell it.
    ///
    /// The inverse of [`Triple::tuple`], and computed by running that function over every triple
    /// there is rather than by writing the narrowing out a second time. A second table would be a
    /// second thing to keep in step, and the failure it invites is not a compile error: it is one
    /// row of the matrix quietly answering as a neighbour.
    ///
    /// It returns `None` for most of the target table, and that is the honest answer rather than a
    /// gap to be papered over. `rucc-abi` describes the scalar layout of all forty three rows, and
    /// this type holds three fields with four architectures in the first, so only the rows on
    /// those four have a [`TargetInfo`] and the rest do not. Anything that needs to lay a
    /// record out for `s390x-linux-gnu` needs that gap closed rather than an approximation of it.
    ///
    /// The environment of the answer is the narrowed one, so the triple this gives back is the
    /// canonical spelling of that machine: `Env::None` on Darwin and on a freestanding target,
    /// never the `Env::Gnu` that a parser will accept from a string somebody typed. A deployment
    /// target or a glibc release does not change which triple a tuple narrows to, so
    /// `aarch64-macos.13` is the Darwin triple rather than a miss.
    #[must_use]
    pub fn from_tuple(target: TargetTuple) -> Option<Triple> {
        // The preview is the one version that names a different target, so it stays.
        let target = match (target.os(), target.os_version()) {
            (tuple::Os::Wasi, Some(preview)) => TargetTuple::builder(target.arch(), target.os())
                .env(target.env())
                .os_version(tuple::Version::major(preview.major_part()))
                .build()
                .ok()?,
            _ => target.without_versions(),
        };
        // Four triples narrow onto `x86_64-linux-gnu`, because a Darwin triple claiming glibc is
        // a string somebody can type and not a machine. So a match is not enough on its own: the
        // answer is the candidate whose environment came through the narrowing unchanged, and
        // anything else is only a fallback for the day a narrowing loses a spelling entirely.
        let mut fallback = None;
        for arch in Arch::ALL {
            for os in Os::ALL {
                for env in Env::ALL {
                    if !Triple::pairs(arch, os) {
                        continue;
                    }
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
            "x86" => Arch::X86,
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
        // Windows is gnu whichever ABI rucc itself was built for. An `rucc.exe` built with MSVC
        // still has no Windows SDK to link against on a fresh machine, and it can fetch the
        // mingw-w64 sysroot, so `rucc hello.c` works there only if the default is the one it can
        // fetch. `--target=x86_64-windows-msvc` is still there for somebody who has the SDK.
        let (os, env) = match std::env::consts::OS {
            "linux" => (Os::Linux, linux),
            "macos" => (Os::Darwin, Env::None),
            "windows" => (Os::Windows, Env::Gnu),
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
            Some("i386" | "i486" | "i586" | "i686" | "x86") => Arch::X86,
            Some("wasm32") => Arch::Wasm32,
            Some("wasm64") => return Err(err("wasm64 is not supported, only wasm32")),
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
                // `wasi` alone is the old name of preview 1.
                "wasi" | "wasip1" => os = Some(Os::Wasi(Preview::P1)),
                "wasip2" => os = Some(Os::Wasi(Preview::P2)),
                "wasip3" => os = Some(Os::Wasi(Preview::P3)),
                part if part.starts_with("wasip") => {
                    return Err(err("unknown WASI preview, which is wasip1, wasip2 or wasip3"));
                }
                "threads" => return Err(err("the threads variant of WASI is not supported")),
                _ => {}
            }
        }

        // LLVM writes freestanding wasm as `wasm32-unknown-unknown`, with no operating system.
        if arch == Arch::Wasm32 && os.is_none() {
            os = Some(Os::None);
        }
        let os = os.ok_or_else(|| err("unknown operating system"))?;
        if !Triple::pairs(arch, os) {
            return Err(err(if arch.is_wasm() {
                "wasm32 runs on WASI or with no operating system"
            } else {
                "WASI is an operating system for wasm32 only"
            }));
        }
        // The same defaults as `rucc_tuple::Os::default_env`, so that `x86_64-pc-windows` means
        // one target whichever of the two parsers read it, and it means the mingw-w64 one because
        // that is the one a fresh machine can link for.
        let env = env.unwrap_or(match os {
            Os::Linux | Os::Windows => Env::Gnu,
            Os::Darwin | Os::None | Os::Wasi(_) => Env::None,
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
    /// answer to, and a triple can spell fifteen of the forty three. Nothing else in this type had
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
    /// Whether the target has `_Float16`.
    ///
    /// The named types are not all universal the way `_Float32` and `_Float64` are. gcc 13 has
    /// this one on x86-64, AArch64 and RISC-V and does not have it on i686, armv7, ppc64le or
    /// s390x, which was measured by compiling a declaration of it with each of those cross
    /// compilers. The `__FLT16_*__` macros and the `f16` suffix are defined on exactly the rows
    /// where the type is, so all three ask this one field.
    ///
    /// i686 is the row worth explaining. gcc aims at the baseline of the target rather than at
    /// whatever chip is under it, and half precision on x86 needs SSE2, which is in the baseline
    /// of x86-64 and not in the baseline of i686. So the two x86 rows disagree, and a `-msse2`
    /// on the command line would move the 32-bit one, which is a thing this compiler has no
    /// place to say yet.
    pub has_float16: bool,
    /// Whether the target has `_Float128`.
    ///
    /// Every row but 32-bit ARM among the seven measured against gcc 13. x86-64 and i686 have it
    /// in software, and AArch64, RISC-V, s390x and ppc64le have it because quad precision is
    /// already the format of something on those machines. armv7 has no format wider than a
    /// `double` at all, so the type is not there and gcc says so.
    ///
    /// This is the ISO spelling. gcc's `__float128` is a narrower thing and is not this field:
    /// that name exists on x86 and PowerPC only, and on AArch64, RISC-V and s390x gcc offers
    /// `_Float128` in its place when a program writes it. `__SIZEOF_FLOAT128__` follows the
    /// vendor name rather than the type, which is why it is missing on rows where the type is
    /// there.
    pub has_float128: bool,
    /// Whether the target has `_Decimal32`, `_Decimal64` and `_Decimal128`.
    ///
    /// Only x86-64 Linux and x86-64 mingw-w64 today. gcc has the three types on more rows than
    /// that, but a decimal is a call into libgcc for everything but a move, and the only encoding
    /// the back end names routines for is the binary integer one x86 uses. mingw-w64 gcc uses the
    /// same encoding and the same routines, and passes the two narrow types in general purpose
    /// registers. Microsoft's compiler has no decimal types, so the msvc row does not either. PowerPC and s390x use the densely packed
    /// encoding and are a different set of routines, and the other rows are untested, so a
    /// program that writes one there is told the type is not available rather than handed code
    /// nobody has run.
    pub has_decimal_float: bool,
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
    /// Whether the output says where an unwind lands, which is the language specific data area
    /// beside a function's call frame information that a cleanup under `-fexceptions` needs.
    ///
    /// ELF on x86-64 and AArch64 and nothing else yet. It is a claim about what this compiler
    /// writes rather than about what the platform can do, so lowering turns down a cleanup it could
    /// not honour on the others instead of emitting one the unwinder would skip.
    pub landing_pads: bool,
    /// Whether a table in read only data may hold how far a label is from the table, as a four
    /// byte relocation measured from where it is written.
    ///
    /// x86-64 ELF, which is the one output whose writer has been taught that relocation for data.
    /// Everywhere else a jump table holds whole addresses.
    pub relative_tables: bool,
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
    /// How many words of arguments a function of this unit's own convention takes in registers,
    /// which is what `-mregparm=` says on 32 bit x86 and is zero everywhere else.
    ///
    /// [`TargetInfo::call_regs`] is the registers that go with it, and
    /// [`TargetInfo::convention_for`] is what a function type's convention comes to under it.
    pub regparm: u8,
    /// Whether a small structure comes back in registers, which is what `-freg-struct-return` says
    /// on 32 bit x86 and what the kernel builds with there. See [`TargetInfo::with_reg_struct_return`].
    pub reg_struct_return: bool,
    /// How long this machine's instructions take, or `None` for an architecture with no backend.
    ///
    /// [`None`] rather than a model of a machine nobody measured, for the reason the two fields
    /// above are: a scheduler told made up numbers about a processor has no way to find out they
    /// were made up. `--print-config` prints [`TimingInsts::model`] off this, which is the first
    /// thing anybody comparing two runs of a benchmark wants to know.
    pub timing: Option<&'static TimingInsts>,
    /// The bit counts this machine has one instruction for, each with the extension it needs.
    ///
    /// Empty for an architecture with no rules for them, which is everything but x86-64 and
    /// AArch64 today.
    /// The code generator writes a count that is not here out as arithmetic, so a pass that would
    /// put one where there was none reads this first. See [`CountInst`].
    pub counts: &'static [CountInst],
    /// Whether the code generator lowers a `select` of a pointer, a `float` or a `double`, and not
    /// only of an integer of 8, 16, 32 or 64 bits.
    ///
    /// True only on wasm, where one `select` instruction takes two operands of any number type and
    /// a pointer is an `i32`. The native rule set names a `select` only at the four integer widths,
    /// so there a choice between two pointers stays a branch. If-conversion reads this before it
    /// writes a `select`, because a `select` that no rule lowers fails at instruction selection.
    pub selects_any: bool,
    /// Whether the second inliner treats a call in a loop as hinted when its callee goes away with
    /// it: the callee is `static`, every call to it is in this caller, and the caller is not
    /// large. The hint doubles the limit the call is held to, the way gcc's loop hints do.
    ///
    /// True only on wasm. The compiler a wasm build is measured against there is clang, which
    /// inlines far more than gcc, and a call costs more in a wasm engine than on the machine,
    /// because the engine passes its context to the callee and checks the stack limit at entry.
    /// The callee's own copy is removed, so the unit grows by little. A native target keeps gcc's
    /// decisions.
    pub loop_hint: bool,
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
/// Whether `target` is the one output the unwind tables and relative jump tables are written for.
fn x86_64_elf(target: TargetTuple, pointer_size: u64) -> bool {
    target.arch() == tuple::Arch::X86_64
        && target.object_format() == tuple::ObjectFormat::Elf
        && pointer_size == 8
}

fn bits(bytes: u64) -> u32 {
    u32::try_from(bytes * 8).expect("no standard type is four billion bits wide")
}

impl TargetInfo {
    /// The description of `triple`.
    ///
    /// The three field triple spells fifteen of the forty three rows of the target table, which is
    /// every row with a backend and every row a driver will be handed today, so this is what the
    /// compiler proper calls. [`TargetInfo::for_tuple`] is the one that answers for the whole
    /// table.
    #[must_use]
    pub fn new(triple: Triple) -> Self {
        Self::for_tuple(triple.tuple())
    }

    /// The type names this target's compiler has before any header is read, and what each is.
    ///
    /// Empty everywhere but AArch64, where gcc has the Advanced SIMD and SVE types and glibc's
    /// `<math.h>` names them.
    #[must_use]
    pub fn type_names(&self) -> &'static [(&'static str, TypeName)] {
        typenames::type_names(self.tuple.arch())
    }

    /// Whether a file-scope `register T x asm ("name")` on this target can be kept as what it says.
    ///
    /// Such a variable is the register for the whole program, so it can only be honoured for a
    /// register the code generator never hands out and nothing else writes behind its back. On
    /// AArch64 that is `x18`, which rucc keeps off every target because Windows and Apple give it
    /// to the platform, and which mingw-w64's `winnt.h` declares this way so that `NtCurrentTeb`
    /// reads the thread's TEB out of it.
    ///
    /// The stack pointer is the other one, on both machines. Nothing hands it out and nothing
    /// writes it behind the program's back, and the Linux kernel declares `current_stack_pointer`
    /// as `rsp`, `esp` or `sp` this way, to read it and to hand it to the `asm` statements that make a
    /// call so that the call is made from a frame that is set up.
    #[must_use]
    pub fn keeps_register_for_the_program(&self, name: &str) -> bool {
        match self.tuple.arch() {
            tuple::Arch::Aarch64 => matches!(name, "x18" | "sp"),
            tuple::Arch::X86_64 => name == "rsp",
            tuple::Arch::X86 => name == "esp",
            _ => false,
        }
    }

    /// What a flag output, `"=@cc<cond>"`, turns into on this target: the constraint of an output in
    /// a register and the instructions that leave the condition in it, which go after the rest of
    /// the template. The output is operand `index` and its type is `bits` wide.
    ///
    /// gcc does the same thing. The template leaves the answer in the flags, and gcc writes the
    /// `set<cond>` or `cset` that reads it after the template, into a register it picked for the
    /// output. The kernel's `CC_SET` and `CC_OUT` are the way it gets at this on both machines, and
    /// `test_bit` and every atomic that answers whether it reached zero are written with them.
    ///
    /// Nothing for a condition the target has no name for, and for a target with no flag outputs.
    #[must_use]
    pub fn flag_output(
        &self,
        cond: &str,
        index: usize,
        bits: u32,
    ) -> Option<(&'static str, String)> {
        match self.tuple.arch() {
            tuple::Arch::X86_64 | tuple::Arch::X86 => {
                const CONDITIONS: &[&str] = &[
                    "a", "ae", "b", "be", "c", "e", "g", "ge", "l", "le", "na", "nae", "nb", "nbe",
                    "nc", "ne", "ng", "nge", "nl", "nle", "no", "np", "ns", "nz", "o", "p", "pe",
                    "po", "s", "z",
                ];
                if !CONDITIONS.contains(&cond) {
                    return None;
                }
                // `set<cond>` writes one byte, and the rest of a wider output is cleared the way gcc
                // clears it, with a move that writes the low 32 bits and so the whole register.
                let mut text = format!("\n\tset{cond} %b{index}");
                if bits > 8 {
                    text.push_str(&format!("\n\tmovzbl %b{index}, %k{index}"));
                }
                Some(("=q", text))
            }
            tuple::Arch::Aarch64 => {
                const CONDITIONS: &[&str] = &[
                    "eq", "ne", "cs", "hs", "cc", "lo", "mi", "pl", "vs", "vc", "hi", "ls", "ge",
                    "lt", "gt", "le",
                ];
                if !CONDITIONS.contains(&cond) {
                    return None;
                }
                // `cset` into the 32 bit register clears the top half as well, so one width does
                // for every type.
                Some(("=r", format!("\n\tcset %w{index}, {cond}")))
            }
            _ => None,
        }
    }

    /// How an `asm` template with operands on this target writes the register called `name`,
    /// which is `%%rsp` on x86, where one `%` would start an operand, and as it is on AArch64.
    #[must_use]
    pub fn register_in_text(&self, name: &str) -> String {
        match self.tuple.arch() {
            tuple::Arch::X86_64 | tuple::Arch::X86 => format!("%%{name}"),
            _ => name.to_owned(),
        }
    }

    /// Whether an unnamed bit-field raises the record's alignment under `style`, which is the
    /// target's own rule or the one a `gcc_struct` or `ms_struct` attribute chose.
    ///
    /// Under the target's own rule it is [`TargetInfo::unnamed_bit_field_aligns`]. Microsoft's rule
    /// says yes everywhere. The Itanium rule that `gcc_struct` asks for on Windows says what it
    /// says on the same architecture's other rows: no on x86-64, and yes on AArch64, where AAPCS64
    /// says so. So `struct { char c; int :20; } __attribute__((gcc_struct))` is four bytes aligned
    /// to one from mingw-w64 gcc on x86-64 and four aligned to four from llvm-mingw's clang on
    /// AArch64, which is also what gcc for AArch64 Linux makes of it without the attribute.
    #[must_use]
    pub fn unnamed_bit_field_aligns_under(&self, style: BitFieldStyle) -> bool {
        if style == self.bit_field_style {
            return self.unnamed_bit_field_aligns;
        }
        match style {
            BitFieldStyle::Microsoft => true,
            BitFieldStyle::Itanium => {
                matches!(
                    self.tuple.arch(),
                    tuple::Arch::Aarch64 | tuple::Arch::Arm | tuple::Arch::Arm64Ec
                ) && !self.tuple.os().is_darwin()
            }
        }
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
        // RISC-V and everything else with a row and no backend have register files and this crate
        // has not written them down yet. They arrive with the backends that need them. AArch64's is
        // here ahead of its backend, because the convention over it is what the ABI tests and the
        // debugging information read, and [`TargetInfo::regs`] having it does not make anything
        // try to generate code: that is `rucc_codegen::Machine::for_target`'s decision.
        let regs = match target.arch() {
            tuple::Arch::X86_64 => &x86_64::REGS,
            tuple::Arch::Aarch64 => &aarch64::REGS,
            tuple::Arch::X86 => &x86::REGS,
            _ => &RegFile::EMPTY,
        };
        let call_regs = match (target.arch(), target.os(), target.env()) {
            // The environment, and this is the one question it decides about a convention. What the
            // two Windows runtimes disagree about is the name of the routine a large frame reaches
            // its pages by calling, which is in the runtime rather than in the compiler, so a build
            // against mingw-w64 and a build against Microsoft's runtime want different names for the
            // same routine.
            (tuple::Arch::X86_64, tuple::Os::Windows, tuple::Env::Gnu) => Some(&x86_64::MINGW64),
            (tuple::Arch::X86_64, tuple::Os::Windows, _) => Some(&x86_64::WIN64),
            // Apple's x86-64 follows SysV, and its divergences from it are on AArch64.
            (tuple::Arch::X86_64, _, _) => Some(&x86_64::SYSV),
            // Windows on AArch64 reserves `x18`, passes every argument of a variadic function in
            // the x registers and homes them at the top of the callee's frame.
            (tuple::Arch::Aarch64, tuple::Os::Windows, _) => Some(&aarch64::WINDOWS),
            (tuple::Arch::Aarch64, os, _) if os.is_darwin() => Some(&aarch64::DARWIN),
            (tuple::Arch::Aarch64, _, _) => Some(&aarch64::AAPCS64),
            // Windows is cdecl over the same registers, with an ABI of its own for each runtime
            // and a routine of its own for a large frame. Position independent code wants
            // [`x86::SYSV_PIC`], which is the code generator's to pick, since whether code is
            // position independent is a flag and not the target.
            (tuple::Arch::X86, tuple::Os::Windows, tuple::Env::Msvc) => Some(&x86::MSVC32),
            (tuple::Arch::X86, tuple::Os::Windows, _) => Some(&x86::MINGW32),
            (tuple::Arch::X86, _, _) => Some(&x86::SYSV),
            _ => None,
        };
        // The same rule as the register file. A model is a measurement of a processor, and there
        // is nothing to measure until there is a backend emitting instructions for it.
        let timing = match target.arch() {
            tuple::Arch::X86_64 => Some(&x86_64::TIMING),
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
            has_float16: has_float16(target),
            has_float128: has_float128(target),
            has_decimal_float: matches!(
                (target.arch(), target.os(), target.env()),
                (tuple::Arch::X86_64, tuple::Os::Linux, _)
                    | (tuple::Arch::X86_64, tuple::Os::Windows, tuple::Env::Gnu)
            ),
            wchar_width: bits(layout.wchar_size),
            wchar_is_signed: layout.wchar_is_signed,
            bit_int_granule: bit_int_granule(target),
            // Eight bytes everywhere, for the reason the field gives: it is the widest access this
            // compiler writes an instruction for, and every one of these machines has a wider one
            // that nothing here reaches. It is a claim about the code this compiler emits, so the
            // day a backend emits a sixteen byte atomic is the day this stops being one number.
            lock_free_width: 64,
            object_format: ObjectFormat::from_tuple(target.object_format()),
            landing_pads: x86_64_elf(target, layout.pointer_size)
                || (target.arch() == tuple::Arch::Aarch64
                    && target.object_format() == tuple::ObjectFormat::Elf),
            relative_tables: x86_64_elf(target, layout.pointer_size),
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
            regparm: 0,
            reg_struct_return: false,
            timing,
            counts: match target.arch() {
                tuple::Arch::X86_64 => x86_64::COUNTS,
                tuple::Arch::Aarch64 => aarch64::COUNTS,
                _ => &[],
            },
            selects_any: matches!(target.arch(), tuple::Arch::Wasm32),
            loop_hint: matches!(target.arch(), tuple::Arch::Wasm32),
        }
    }

    /// The same target with the first `registers` words of every function's arguments in
    /// registers, which is `-mregparm=`, and [`None`] where gcc has no such option or refuses
    /// the number.
    #[must_use]
    pub fn with_regparm(mut self, registers: u8) -> Option<Self> {
        if self.tuple.arch() != tuple::Arch::X86 || self.tuple.os() == tuple::Os::Windows {
            return (registers == 0).then_some(self);
        }
        self.call_regs = Some(x86::regparm(registers, self.reg_struct_return)?);
        self.regparm = registers;
        Some(self)
    }

    /// The same target with a structure of one, two, four or eight bytes returned in registers,
    /// which is `-freg-struct-return`, or through memory, which is `-fpcc-struct-return`.
    ///
    /// Only i386 System V changes. Every other target's ABI already says where a small structure
    /// comes back and gcc takes the flag there without doing anything, which this does too.
    #[must_use]
    pub fn with_reg_struct_return(mut self, in_registers: bool) -> Self {
        if self.tuple.arch() != tuple::Arch::X86 || self.tuple.os() == tuple::Os::Windows {
            return self;
        }
        if let Some(regs) = x86::regparm(self.regparm, in_registers) {
            self.call_regs = Some(regs);
            self.reg_struct_return = in_registers;
        }
        self
    }

    /// The convention a function of this type is called with, given what its type says and
    /// whether it is variadic.
    ///
    /// Only `regparm` changes anything. A function type that says nothing has the unit's own
    /// count, one that says `regparm(n)` has `n`, and a variadic one has none whatever it says,
    /// which is what gcc does. A count that is the unit's own is written [`Convention::Target`],
    /// so a `regparm(3)` written in a unit built with `-mregparm=3` changes nothing.
    #[must_use]
    pub fn convention_for(&self, convention: Convention, variadic: bool) -> Convention {
        let registers = match convention {
            Convention::Target if self.regparm == 0 => return convention,
            Convention::Target => self.regparm,
            Convention::Regparm(registers) => registers,
            other => return other,
        };
        let registers = if variadic { 0 } else { registers };
        if registers == self.regparm { Convention::Target } else { Convention::Regparm(registers) }
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

/// Whether the target has `_Float16`.
fn has_float16(target: TargetTuple) -> bool {
    match target.arch() {
        // Half precision is in the baseline of these: SSE2 on x86-64, the FP16 storage format
        // every ARMv8 has, and RISC-V, where gcc gives the type whether or not the hardware has
        // the instructions to go with it.
        tuple::Arch::X86_64
        | tuple::Arch::Aarch64
        | tuple::Arch::Arm64Ec
        | tuple::Arch::Riscv64
        | tuple::Arch::Riscv32 => true,
        // i686 for the reason the field gives, which is the baseline and not the chip, and the
        // rest are machines gcc 13 has not written the type for.
        tuple::Arch::X86
        | tuple::Arch::Arm
        | tuple::Arch::LoongArch64
        | tuple::Arch::PowerPc64
        | tuple::Arch::S390x
        | tuple::Arch::Wasm32 => false,
    }
}

/// Whether the target has `_Float128`.
fn has_float128(target: TargetTuple) -> bool {
    match target.arch() {
        // Either the machine already has quad precision, which is the AArch64, RISC-V, s390x and
        // PowerPC answer, or the compiler provides it in software, which is what x86 does.
        tuple::Arch::X86_64
        | tuple::Arch::X86
        | tuple::Arch::Aarch64
        | tuple::Arch::Arm64Ec
        | tuple::Arch::Riscv64
        | tuple::Arch::Riscv32
        | tuple::Arch::LoongArch64
        | tuple::Arch::PowerPc64
        | tuple::Arch::S390x => true,
        // The same two rows that have no `_Float64x`, and for the same reason: nothing on the
        // machine is wider than a `double` and neither reference offers a type that is.
        tuple::Arch::Arm | tuple::Arch::Wasm32 => false,
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
    fn branch_protection_reads_the_way_gcc_does() {
        let read = |s: &str| BranchProtection::parse(s);
        let sign = |sign, bti| Ok(BranchProtection { sign, bti });
        assert_eq!(read("none"), sign(SignReturn::None, false));
        assert_eq!(read("standard"), sign(SignReturn::NonLeaf, true));
        assert_eq!(read("pac-ret"), sign(SignReturn::NonLeaf, false));
        assert_eq!(read("pac-ret+leaf"), sign(SignReturn::All, false));
        assert_eq!(read("pac-ret+leaf+bti"), sign(SignReturn::All, true));
        assert_eq!(read("bti+pac-ret+leaf"), sign(SignReturn::All, true));
        assert_eq!(read("bti"), sign(SignReturn::None, true));
        assert!(read("pac-ret+b-key").unwrap_err().contains("b-key"));
        assert!(read("leaf").unwrap_err().contains("without pac-ret"));
        assert!(read("pac-ret+").is_err());
        assert_eq!(SignReturn::parse("non-leaf"), Ok(SignReturn::NonLeaf));
        assert_eq!(SignReturn::parse("all"), Ok(SignReturn::All));
        assert!(SignReturn::parse("leaf").is_err());
    }

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
        assert_eq!(w.env, Env::Gnu);
    }

    #[test]
    fn rejects_what_it_does_not_support() {
        let e = "sparc64-unknown-linux-gnu".parse::<Triple>().unwrap_err();
        assert_eq!(e.reason, "unknown architecture");
        let e = "x86_64-unknown-plan9".parse::<Triple>().unwrap_err();
        assert_eq!(e.reason, "unknown operating system");
    }

    #[test]
    fn reads_the_wasm_spellings() {
        for (spelling, os) in [
            ("wasm32-wasi", Os::Wasi(Preview::P1)),
            ("wasm32-wasip1", Os::Wasi(Preview::P1)),
            ("wasm32-unknown-wasip1", Os::Wasi(Preview::P1)),
            ("wasm32-wasip2", Os::Wasi(Preview::P2)),
            ("wasm32-wasip3", Os::Wasi(Preview::P3)),
            ("wasm32", Os::None),
            ("wasm32-unknown-unknown", Os::None),
            ("wasm32-none", Os::None),
        ] {
            let t: Triple = spelling.parse().unwrap_or_else(|e| panic!("{spelling}: {e}"));
            assert_eq!((t.arch, t.os, t.env), (Arch::Wasm32, os, Env::None), "{spelling}");
            assert_eq!(t.object_format(), ObjectFormat::Wasm, "{spelling}");
        }
    }

    #[test]
    fn keeps_the_wasi_preview_through_the_tuple() {
        for preview in [Preview::P1, Preview::P2, Preview::P3] {
            let t = Triple { arch: Arch::Wasm32, os: Os::Wasi(preview), env: Env::None };
            assert_eq!(Triple::from_tuple(t.tuple()), Some(t));
        }
    }

    #[test]
    fn refuses_the_wasm_rows_it_does_not_have() {
        for (spelling, reason) in [
            ("wasm64-wasip1", "wasm64 is not supported, only wasm32"),
            ("wasm32-wasip9", "unknown WASI preview, which is wasip1, wasip2 or wasip3"),
            ("wasm32-wasip1-threads", "the threads variant of WASI is not supported"),
            ("wasm32-linux", "wasm32 runs on WASI or with no operating system"),
            ("wasm32-unknown-linux-gnu", "wasm32 runs on WASI or with no operating system"),
            ("x86_64-wasip1", "WASI is an operating system for wasm32 only"),
        ] {
            let e = spelling.parse::<Triple>().unwrap_err();
            assert_eq!(e.reason, reason, "{spelling}");
        }
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
        // no such pair. All eighty combinations whose architecture pairs with the operating
        // system, including the ones the parser will produce from a string somebody can type and
        // no machine has, such as a Darwin target claiming glibc.
        let mut built = 0;
        for arch in Arch::ALL {
            for os in Os::ALL.into_iter().filter(|&os| Triple::pairs(arch, os)) {
                for env in Env::ALL {
                    let triple = Triple::new(arch, os, env);
                    let tuple = triple.tuple();
                    assert_eq!(tuple.pointer_width(), arch.pointer_width(), "{triple}");
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
        assert_eq!(built, 80);
    }

    #[test]
    fn from_tuple_undoes_the_narrowing() {
        // Every triple's tuple comes back as a triple describing the same machine. It is not
        // always the triple it started as, because the narrowing is many to one: a Darwin target
        // claiming glibc and the same one claiming nothing are one machine, and the answer is the
        // spelling that names no libc.
        for arch in Arch::ALL {
            for os in Os::ALL.into_iter().filter(|&os| Triple::pairs(arch, os)) {
                for env in Env::ALL {
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
        // A version on either side narrows to the same triple as the tuple without it.
        let pinned = Triple::from_tuple("aarch64-macos.13".parse().unwrap()).unwrap();
        assert_eq!(pinned, macos);
        let old = Triple::from_tuple("x86_64-linux-gnu.2.28".parse().unwrap()).unwrap();
        assert_eq!(old, gnu);
    }

    #[test]
    fn from_tuple_says_no_rather_than_saying_something_near() {
        // Most of the forty three rows have no triple, and the answer is `None` rather than
        // a neighbour. `rucc-abi` knows the scalar layout of every one of these and this type
        // cannot hold any of them, which is the gap the record layout engine inherits.
        for tuple in [
            "armv7-linux-gnueabihf",
            "s390x-linux-gnu",
            "powerpc64le-linux-gnu",
            "loongarch64-linux-gnu",
            "x86_64-linux-gnux32",
            "aarch64-linux-android",
            "aarch64-ios",
            "x86_64-freebsd",
        ] {
            let target = tuple.parse().unwrap();
            assert_eq!(Triple::from_tuple(target), None, "{tuple}");
        }
        // i686 has a triple now, and it is the machine and not x86-64's.
        let i686 = Triple::from_tuple("i686-linux-gnu".parse().unwrap()).unwrap();
        assert_eq!(i686, Triple::new(Arch::X86, Os::Linux, Env::Gnu));
        assert_eq!(i686.to_string(), "i686-unknown-linux-gnu");
        // wasm32 has a triple now too, and the WASI preview stays in it.
        let wasi = Triple::from_tuple("wasm32-wasip1".parse().unwrap()).unwrap();
        assert_eq!(wasi, Triple::new(Arch::Wasm32, Os::Wasi(Preview::P1), Env::None));
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
    fn the_named_floating_types_are_not_on_every_machine() {
        // gcc 13, measured with the cross compilers rather than reasoned about. `_Float16` is on
        // three of these seven and `_Float128` is on six, and the two lists are not the same
        // list, which is why there are two fields.
        // The three field triple spells three architectures, and four of these rows are not
        // among them, so this asks the tuple the way the layout tests do.
        let of = |tuple: &str| TargetInfo::for_tuple(tuple.parse().expect("a row in the table"));
        let rows = [
            ("x86_64-linux-gnu", true, true),
            ("i686-linux-gnu", false, true),
            ("aarch64-linux-gnu", true, true),
            ("armv7-linux-gnueabihf", false, false),
            ("powerpc64le-linux-gnu", false, true),
            ("riscv64-linux-gnu", true, true),
            ("s390x-linux-gnu", false, true),
        ];
        for (tuple, float16, float128) in rows {
            let target = of(tuple);
            assert_eq!(target.has_float16, float16, "{tuple} `_Float16`");
            assert_eq!(target.has_float128, float128, "{tuple} `_Float128`");
        }
        // The operating system has nothing to do with it, the way it has nothing to do with
        // `_Float64x`, so Apple and Windows keep both types.
        assert!(of("aarch64-apple-darwin").has_float16);
        assert!(of("x86_64-pc-windows-msvc").has_float128);
    }

    #[test]
    fn the_decimal_types_are_on_the_rows_the_back_end_calls_routines_for() {
        let of = |tuple: &str| TargetInfo::for_tuple(tuple.parse().expect("a row in the table"));
        assert!(of("x86_64-linux-gnu").has_decimal_float);
        assert!(of("x86_64-pc-windows-gnu").has_decimal_float);
        for tuple in ["aarch64-linux-gnu", "x86_64-pc-windows-msvc", "aarch64-apple-darwin"] {
            assert!(!of(tuple).has_decimal_float, "{tuple}");
        }
    }

    #[test]
    fn the_object_format_follows_the_operating_system() {
        let of = |triple: &str| triple.parse::<Triple>().unwrap().object_format();
        assert_eq!(of("x86_64-linux-gnu"), ObjectFormat::Elf);
        assert_eq!(of("aarch64-apple-darwin"), ObjectFormat::MachO);
        assert_eq!(of("x86_64-pc-windows-msvc"), ObjectFormat::Coff);
        assert_eq!(of("wasm32-wasip1"), ObjectFormat::Wasm);
        // With no operating system it follows the architecture.
        assert_eq!(of("x86_64-unknown-none"), ObjectFormat::Elf);
        assert_eq!(of("wasm32-unknown-unknown"), ObjectFormat::Wasm);
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
        let arm = of("aarch64-unknown-linux-gnu");
        assert_eq!(arm.regs.len(aarch64::GPR), 32);
        assert_eq!(arm.call_regs.map(|regs| regs.int_args[0]), Some(aarch64::x(0)));
        assert_eq!(arm.call_regs.map(|regs| regs.red_zone), Some(0));
        assert_eq!(of("aarch64-apple-darwin").call_regs.map(|regs| regs.red_zone), Some(128));
        // Another stack boundary is the same convention with one number changed, made once.
        let sysv = linux.call_regs.expect("a convention");
        let eight = sysv.aligned_to(8);
        assert_eq!((eight.stack_align, eight.int_args), (8, sysv.int_args));
        assert!(std::ptr::eq(eight, sysv.aligned_to(8)));
        assert!(std::ptr::eq(sysv, sysv.aligned_to(16)));
        // Windows on AArch64 has registers of its own rather than Linux's, both runtimes alike.
        for triple in ["aarch64-pc-windows-msvc", "aarch64-pc-windows-gnu"] {
            let regs = of(triple).call_regs.expect("a convention");
            assert!(std::ptr::eq(regs, &aarch64::WINDOWS), "{triple}");
        }
        // i386 has its registers everywhere and a convention wherever `rucc-abi` has one.
        let i386 = of("i686-unknown-linux-gnu");
        assert_eq!(i386.regs.reg_named("ebx"), Some((x86::GPR, x86::EBX)));
        assert!(std::ptr::eq(i386.call_regs.expect("i386 SysV"), &x86::SYSV));
        let i686_windows = of("i686-pc-windows-gnu");
        assert_eq!(i686_windows.regs.len(x86::GPR), 8);
        assert!(std::ptr::eq(i686_windows.call_regs.expect("mingw"), &x86::MINGW32));
        let i686_msvc = of("i686-pc-windows-msvc");
        assert!(std::ptr::eq(i686_msvc.call_regs.expect("msvc"), &x86::MSVC32));
        let riscv = of("riscv64-unknown-linux-gnu");
        assert!(riscv.regs.is_empty());
        assert!(riscv.call_regs.is_none());
    }

    #[test]
    fn regparm_is_the_unit_s_count_and_a_variadic_function_has_none() {
        let target = |triple: &str| TargetInfo::new(triple.parse().expect("a triple"));
        let unit = target("i686-unknown-linux-gnu").with_regparm(3).expect("i386 has the flag");
        assert_eq!(unit.regparm, 3);
        let three = x86::regparm(3, false).expect("three");
        assert!(std::ptr::eq(unit.call_regs.expect("i386"), three));
        assert_eq!(unit.convention_for(Convention::Target, false), Convention::Target);
        assert_eq!(unit.convention_for(Convention::Regparm(3), false), Convention::Target);
        assert_eq!(unit.convention_for(Convention::Regparm(0), false), Convention::Regparm(0));
        assert_eq!(unit.convention_for(Convention::Target, true), Convention::Regparm(0));
        let plain = target("i686-unknown-linux-gnu");
        assert_eq!(plain.convention_for(Convention::Target, true), Convention::Target);
        assert_eq!(plain.convention_for(Convention::Regparm(2), true), Convention::Target);
        assert_eq!(plain.convention_for(Convention::Regparm(2), false), Convention::Regparm(2));
        assert!(plain.clone().with_regparm(4).is_none());
        assert!(target("x86_64-unknown-linux-gnu").with_regparm(3).is_none());
        assert!(target("x86_64-unknown-linux-gnu").with_regparm(0).is_some());
    }

    /// The two maps from a triple, held against each other.
    ///
    /// A target's registers and a target's ABI are chosen by two separate matches, one here and one
    /// in `rucc_abi::abis::for_target`, and [`CallRegs::abi`] is the link between them. Two matches
    /// that can disagree are the thing this crate must not have, so every triple with registers is
    /// asked both questions and the answers have to be the same description. What it catches is a
    /// target added to one match and not the other, which is a compiler that puts the value in the
    /// register one ABI names and the form another one asked for.
    #[test]
    fn the_registers_and_the_abi_a_target_gets_are_the_same_convention() {
        let mut checked = 0;
        for arch in Arch::ALL {
            for os in Os::ALL {
                for env in Env::ALL {
                    let triple = Triple::new(arch, os, env);
                    let info = TargetInfo::new(triple);
                    let Some(regs) = info.call_regs else { continue };
                    let described = rucc_abi::abis::for_target(info.tuple)
                        .unwrap_or_else(|| panic!("{triple} has registers and no ABI"));
                    assert!(
                        std::ptr::eq(regs.abi, described),
                        "{triple} has the registers of {} and the ABI of {}",
                        regs.abi.name,
                        described.name
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked > 0, "no target has registers, so this asserted nothing");
    }

    /// The timing model, which follows the register file: an architecture with no backend has
    /// nothing to measure and says so rather than borrowing a neighbour's numbers.
    #[test]
    fn a_target_carries_the_model_its_schedules_were_chosen_with() {
        let of = |triple: &str| TargetInfo::new(triple.parse().unwrap());
        let linux = of("x86_64-unknown-linux-gnu");
        let timing = linux.timing.expect("x86-64 has a backend and so has a model");
        assert!(timing.model.contains("Skylake"), "{}", timing.model);
        assert!(!timing.accurate, "and it says it is not a cycle accurate one");
        assert_eq!(timing.of("x64.imul_rr_64").map(|cost| cost.unit), Some(Unit::Mul));

        // The same model whatever the operating system, since a model is about the processor.
        assert_eq!(of("x86_64-apple-darwin").timing, linux.timing);
        assert_eq!(of("x86_64-pc-windows-msvc").timing, linux.timing);

        assert!(of("aarch64-unknown-linux-gnu").timing.is_none(), "nobody has measured it here");
    }

    #[test]
    fn the_host_triple_is_one_we_support() {
        // Every host in spec/15-testing.md section 15.7 must be recognised, and CI runs on
        // all three, so a failure here means a host we claim support for stopped resolving.
        let host = Triple::host().expect("the host must be a supported target");
        assert_eq!(host.to_string().parse::<Triple>().unwrap(), host);
    }
}
