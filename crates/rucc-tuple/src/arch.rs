//! The machine half of a tuple: instruction set, baseline, byte order and data model.

use core::fmt;

/// An instruction set family.
///
/// Deliberately not `#[non_exhaustive]`. Adding a variant here has to break every match that
/// needs to change, in this workspace and in anyone else's code. That is the property
/// `spec/cross-compile/02-the-goal.md` claim 3 is making when it says adding a target is a data change: the
/// compiler tells you every place the data is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Arch {
    /// x86-64. The reference target, and the one everything is measured against first.
    X86_64,
    /// 32-bit x86, from i686 upwards. Not a narrower x86-64: eight registers, a PIC base
    /// register, and x87 as the floating point unit of the base ABI.
    X86,
    /// 64-bit ARM.
    Aarch64,
    /// 32-bit ARM, including Thumb.
    Arm,
    /// ARM64EC. A 64-bit ARM instruction set with the x86-64 calling convention, its own symbol
    /// namespace and thunks between the two. `spec/cross-compile/06-abis.md` section 6.8 declines to
    /// implement it and `spec/cross-compile/04-target-matrix.md` keeps it at tier 4, so this variant exists so
    /// that the tuple parses and `--print-config` answers, and nothing emits code for it.
    Arm64Ec,
    /// 64-bit RISC-V.
    Riscv64,
    /// 32-bit RISC-V.
    Riscv32,
    /// 64-bit LoongArch.
    LoongArch64,
    /// 64-bit PowerPC. Only ELFv2 is in scope, which is what makes the little-endian
    /// spelling the common one.
    PowerPc64,
    /// IBM z/Architecture. The big-endian row, and the reason byte order is a field.
    S390x,
    /// WebAssembly with 32-bit addresses. A stack machine with structured control flow,
    /// which `spec/cross-compile/05-architectures.md` section 5.9 explains is a second back end rather
    /// than a tenth architecture.
    Wasm32,
}

impl Arch {
    /// The canonical name, which is the leading component of a tuple for every architecture
    /// whose baseline is not spelled in that component. ARM is the exception and
    /// [`crate::TargetTuple`] handles it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Arch::X86_64 => "x86_64",
            Arch::X86 => "i686",
            Arch::Aarch64 => "aarch64",
            Arch::Arm => "arm",
            Arch::Arm64Ec => "arm64ec",
            Arch::Riscv64 => "riscv64",
            Arch::Riscv32 => "riscv32",
            Arch::LoongArch64 => "loongarch64",
            Arch::PowerPc64 => "powerpc64",
            Arch::S390x => "s390x",
            Arch::Wasm32 => "wasm32",
        }
    }

    /// The width of a general purpose register, in bits.
    ///
    /// This is a property of the instruction set and it is not the pointer width. A 64-bit
    /// machine running an ILP32 data model has 64-bit registers and 32-bit pointers, which is
    /// what `x86_64-linux-gnux32` is for.
    pub const fn register_width(self) -> u32 {
        match self {
            Arch::X86_64
            | Arch::Aarch64
            | Arch::Arm64Ec
            | Arch::Riscv64
            | Arch::LoongArch64
            | Arch::PowerPc64
            | Arch::S390x => 64,
            Arch::X86 | Arch::Arm | Arch::Riscv32 | Arch::Wasm32 => 32,
        }
    }

    /// The byte order this architecture uses unless the tuple says otherwise.
    ///
    /// Every architecture here except s390x is little-endian by default, and five of them have
    /// a big-endian mode that a real target uses. That is why endianness is a tuple field and
    /// not a constant on this enum: the version of this function that returned `true` for
    /// everything was wrong for aarch64, arm, riscv, powerpc and mips at the same time.
    pub const fn default_endian(self) -> Endian {
        match self {
            Arch::S390x | Arch::PowerPc64 => Endian::Big,
            _ => Endian::Little,
        }
    }

    /// Whether this architecture has a big-endian mode any target in the table uses.
    ///
    /// x86 does not, so `x86_64eb` is a spelling error rather than a target, and saying so is
    /// better than emitting bytes for a machine that does not exist.
    pub const fn has_both_endians(self) -> bool {
        matches!(self, Arch::Aarch64 | Arch::Arm | Arch::Riscv64 | Arch::Riscv32 | Arch::PowerPc64)
    }

    /// Whether a sub-architecture may be spelled for this family.
    pub const fn takes_subarch(self) -> bool {
        matches!(self, Arch::Arm)
    }

    /// Whether this family has more than one float calling convention to choose between.
    ///
    /// False for x86-64, AArch64, s390x and PowerPC, which have exactly one, so naming a float
    /// ABI for them is a spelling error rather than a configuration. True for ARM, RISC-V and
    /// LoongArch, where it is a real choice that changes which registers arguments arrive in.
    pub const fn selects_float_abi(self) -> bool {
        matches!(self, Arch::Arm | Arch::Riscv64 | Arch::Riscv32 | Arch::LoongArch64)
    }
}

impl fmt::Display for Arch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A baseline within an architecture family.
///
/// This carries only the baselines that appear in the leading component of a tuple, which in
/// practice means the ARM ones. `spec/cross-compile/03-target-model.md` section 3.3 also lists micro
/// architecture levels such as `x86-64-v3` and RISC-V profiles such as `rva23u64` as
/// candidates for this field, and they are not here on purpose.
///
/// The reason is the rule that section states two paragraphs later: a thing belongs in the
/// tuple if it changes how a function is called or how a struct is laid out. Two objects built
/// for `x86-64-v2` and `x86-64-v3` link together and call each other correctly, so they are
/// the same target and a different `-march`. Two objects built for `armv7a` and `armv6m` are
/// not. Keeping levels out of the tuple is what keeps the tuple's own invariant true, namely
/// that equal tuples means interchangeable objects, and that invariant is what lets a tuple be
/// a cache key. Levels and profiles arrive with the feature set in a later milestone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum SubArch {
    /// The family's own baseline, and the only legal value for every family except ARM.
    #[default]
    None,
    /// ARMv5TE, the floor for the older soft-float Linux ports.
    ArmV5Te,
    /// ARMv6.
    ArmV6,
    /// ARMv6-M, the Cortex-M0 class. Thumb only.
    ArmV6M,
    /// ARMv7-A, which is what almost every 32-bit Linux ARM target means.
    ArmV7A,
    /// ARMv7-R, the real-time profile.
    ArmV7R,
    /// ARMv7-M, the Cortex-M3 and M4 class. Thumb only.
    ArmV7M,
    /// ARMv7E-M.
    ArmV7Em,
    /// ARMv8-A in its 32-bit form.
    ArmV8A,
    /// ARMv8-M baseline and mainline, folded into one value because the difference is a
    /// feature set rather than a different tuple.
    ArmV8M,
}

impl SubArch {
    /// The text this baseline contributes to the leading component, or the empty string.
    pub const fn as_str(self) -> &'static str {
        match self {
            SubArch::None => "",
            SubArch::ArmV5Te => "v5te",
            SubArch::ArmV6 => "v6",
            SubArch::ArmV6M => "v6m",
            // The A profile baselines are spelled without the profile letter, because `armv7`
            // is what the matrix, every distribution and every other toolchain write. The
            // parser takes `v7a` and `v8a` as well.
            SubArch::ArmV7A => "v7",
            SubArch::ArmV7R => "v7r",
            SubArch::ArmV7M => "v7m",
            SubArch::ArmV7Em => "v7em",
            SubArch::ArmV8A => "v8",
            SubArch::ArmV8M => "v8m",
        }
    }

    /// Whether this baseline belongs to the given family.
    pub const fn belongs_to(self, arch: Arch) -> bool {
        match self {
            SubArch::None => true,
            _ => matches!(arch, Arch::Arm),
        }
    }

    /// Whether the baseline executes Thumb instructions only, which decides the default
    /// instruction set and, on the M profile, rules out the A profile's float ABIs.
    pub const fn is_thumb_only(self) -> bool {
        matches!(self, SubArch::ArmV6M | SubArch::ArmV7M | SubArch::ArmV7Em | SubArch::ArmV8M)
    }
}

/// Byte order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Endian {
    /// Least significant byte first.
    Little,
    /// Most significant byte first. s390x, and the big-endian modes of five other families.
    Big,
}

impl Endian {
    /// The name used in diagnostics and in the print queries.
    pub const fn as_str(self) -> &'static str {
        match self {
            Endian::Little => "little",
            Endian::Big => "big",
        }
    }
}

impl fmt::Display for Endian {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The widths of `int`, `long` and a pointer, as one choice rather than three.
///
/// Only a handful of combinations exist and every other combination is a bug, so this is an
/// enumeration and not three independent numbers. The individual widths are still readable,
/// because that is what the type layout wants, but they are read from here rather than
/// guessed from the architecture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DataModel {
    /// 32-bit `int`, 32-bit `long`, 32-bit pointer, on a 32-bit machine. i686 and armv7.
    Ilp32,
    /// 32-bit `int`, 64-bit `long`, 64-bit pointer. Every 64-bit Unix.
    Lp64,
    /// 32-bit `int`, 32-bit `long`, 64-bit pointer. Windows, and only Windows.
    Llp64,
    /// 32-bit `int`, 32-bit `long`, 32-bit pointer, on a machine with 64-bit registers.
    /// `x86_64-linux-gnux32` and the ILP32 modes of AArch64 and RISC-V.
    ///
    /// This exists in the enumeration mostly to prove the field is real. A model that is
    /// derived from the architecture cannot express it, and the version of this code that
    /// derived it silently produced 64-bit pointers for a target whose pointers are 32 bits.
    Ilp32On64,
}

impl DataModel {
    /// The name used in diagnostics and in the print queries.
    pub const fn as_str(self) -> &'static str {
        match self {
            DataModel::Ilp32 => "ilp32",
            DataModel::Lp64 => "lp64",
            DataModel::Llp64 => "llp64",
            DataModel::Ilp32On64 => "ilp32-on-64",
        }
    }

    /// The width of `int` in bits, which is 32 on every target in the table.
    pub const fn int_width(self) -> u32 {
        32
    }

    /// The width of `long` in bits. This is the field that separates the LP64 world from
    /// Windows.
    pub const fn long_width(self) -> u32 {
        match self {
            DataModel::Lp64 => 64,
            DataModel::Ilp32 | DataModel::Llp64 | DataModel::Ilp32On64 => 32,
        }
    }

    /// The width of `long long` in bits, which is 64 everywhere.
    pub const fn long_long_width(self) -> u32 {
        64
    }

    /// The width of a pointer in bits.
    pub const fn pointer_width(self) -> u32 {
        match self {
            DataModel::Lp64 | DataModel::Llp64 => 64,
            DataModel::Ilp32 | DataModel::Ilp32On64 => 32,
        }
    }

    /// The number of bits of a pointer that are an address.
    ///
    /// Equal to [`DataModel::pointer_width`] for every target in the table, and separate from
    /// it because on CHERI it is not: a purecap pointer is 128 bits carrying a 64-bit address
    /// plus bounds, permissions and a tag. `spec/cross-compile/03-target-model.md` section 3.9 puts CHERI out
    /// of scope and keeps this accessor, because adding the distinction later is an audit of
    /// every use of the pointer width and adding it now costs one function.
    pub const fn address_width(self) -> u32 {
        self.pointer_width()
    }
}

impl fmt::Display for DataModel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
