//! The calling convention variant and the object format.

use core::fmt;

use crate::{Arch, DataModel, Env, Os, SubArch};

/// The variant of the platform ABI that decides where floating point arguments go.
///
/// This is a separate field from the environment because on ARM it is genuinely orthogonal to
/// the libc, and because it is the clearest case of `spec/cross-compile/03-target-model.md`'s rule: it changes
/// how a function is called, so it is in the tuple. GCC fuses it into the environment component
/// as `gnueabihf`, and [`crate::TargetTuple`] reproduces that spelling on output rather than
/// storing the fused form.
///
/// The names are the ones the RISC-V and LoongArch `-mabi=` flags use, minus the data model
/// prefix that those flags carry redundantly. `-mabi=lp64d` is
/// [`DataModel::Lp64`](crate::DataModel::Lp64) plus [`Abi::DoubleFloat`], and splitting it means
/// the data model is written down once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub enum Abi {
    /// The psABI default for this target. Resolved by [`Abi::resolve`], which is what code
    /// generation should call rather than matching on this variant.
    #[default]
    Default,
    /// Floating point arguments in integer registers, and no FPU assumed. `-mfloat-abi=soft`,
    /// `-mabi=lp64`, `-mabi=ilp32`.
    SoftFloat,
    /// Floating point arguments in integer registers, but FPU instructions are emitted for
    /// arithmetic. ARM's `softfp`, which is ABI compatible with soft float and faster.
    SoftFp,
    /// Single precision floating point arguments in float registers. `-mabi=lp64f`. RISC-V
    /// only; no target in the matrix uses it, and it exists because leaving it out would make
    /// the enumeration a lie about the ABI space.
    SingleFloat,
    /// Double precision floating point arguments in float registers. `-mfloat-abi=hard`,
    /// `-mabi=lp64d`, `-mabi=ilp32d`.
    DoubleFloat,
}

impl Abi {
    /// The name used in diagnostics, in `--print-config` and in `-mabi=` reconstruction.
    pub const fn as_str(self) -> &'static str {
        match self {
            Abi::Default => "default",
            Abi::SoftFloat => "soft",
            Abi::SoftFp => "softfp",
            Abi::SingleFloat => "single",
            Abi::DoubleFloat => "hard",
        }
    }

    /// The concrete ABI for a target that did not name one.
    ///
    /// Every case here is a psABI reading rather than a preference. AArch64, x86-64 and Darwin
    /// have one float ABI so there is nothing to choose. RISC-V and LoongArch Linux are LP64D by
    /// convention and by every distribution's build. Bare metal ARM is soft float because the
    /// core may have no FPU, and ARM Linux is hard float because every ARM distribution shipping
    /// today is, which is why the matrix rows are all `eabihf`.
    pub const fn resolve(self, arch: Arch, sub_arch: SubArch, os: Os, env: Env) -> Abi {
        if !matches!(self, Abi::Default) {
            return self;
        }
        match arch {
            Arch::Arm => {
                if sub_arch.is_thumb_only() || matches!(os, Os::None) {
                    Abi::SoftFloat
                } else if matches!(env, Env::Gnu | Env::Musl | Env::Android) {
                    Abi::DoubleFloat
                } else {
                    Abi::SoftFloat
                }
            }
            Arch::Riscv64 | Arch::Riscv32 | Arch::LoongArch64 => match os {
                Os::None => Abi::SoftFloat,
                _ => Abi::DoubleFloat,
            },
            _ => Abi::DoubleFloat,
        }
    }

    /// Whether this ABI may be named for this architecture.
    ///
    /// Naming a float ABI on x86-64 is a spelling error rather than a configuration, because
    /// SysV AMD64 has one convention and there is nothing to select. Saying so is the whole
    /// point of validating the tuple: the alternative is accepting the flag and ignoring it.
    pub const fn is_valid_for(self, arch: Arch) -> bool {
        match self {
            Abi::Default => true,
            Abi::SoftFp => matches!(arch, Arch::Arm),
            Abi::SingleFloat => matches!(arch, Arch::Riscv64 | Arch::Riscv32),
            Abi::SoftFloat | Abi::DoubleFloat => arch.selects_float_abi(),
        }
    }

    /// The `-mabi=` value GCC would take for this ABI on this architecture, which is what the
    /// driver has to reproduce when it hands work to an external assembler or linker.
    pub fn to_mabi(self, arch: Arch, model: DataModel) -> Option<String> {
        let resolved = self;
        match arch {
            Arch::Riscv64 | Arch::Riscv32 | Arch::LoongArch64 => {
                let base = match model {
                    DataModel::Lp64 => "lp64",
                    DataModel::Ilp32 | DataModel::Ilp32On64 => "ilp32",
                    DataModel::Llp64 => return None,
                };
                let suffix = match resolved {
                    Abi::SoftFloat => "",
                    Abi::SingleFloat => "f",
                    Abi::DoubleFloat => "d",
                    Abi::Default | Abi::SoftFp => return None,
                };
                Some(format!("{base}{suffix}"))
            }
            _ => None,
        }
    }
}

impl fmt::Display for Abi {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The container the compiler writes.
///
/// One per OS, with freestanding taking its format from the architecture. rucc writes all four
/// itself rather than handing text to an assembler, which is why this is a first class field:
/// every one of them is a different section model, a different relocation table and a different
/// symbol table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ObjectFormat {
    /// ELF. Linux, the BSDs, illumos, and freestanding on every architecture but wasm.
    Elf,
    /// Mach-O. Darwin, and the format with a version treadmill: chained fixups and
    /// `__init_offsets` are recent and are not optional on current systems.
    MachO,
    /// COFF, in its PE flavour. Windows, both environments, with unwind information in
    /// `.pdata` and `.xdata` rather than in a DWARF section.
    Coff,
    /// The WebAssembly object format. A different shape from the other three, and
    /// `spec/cross-compile/05-architectures.md` section 5.9 treats reaching it as a second back end.
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

    /// The suffix an object file gets on this format.
    pub const fn object_extension(self) -> &'static str {
        match self {
            ObjectFormat::Coff => "obj",
            _ => "o",
        }
    }

    /// The suffix a static archive gets.
    pub const fn archive_extension(self) -> &'static str {
        match self {
            ObjectFormat::Coff => "lib",
            _ => "a",
        }
    }

    /// Whether symbols are prefixed with an underscore.
    ///
    /// True on Mach-O and on 32-bit COFF, false on ELF and 64-bit COFF. Getting this wrong
    /// produces a link error naming a symbol that is visibly present in the object, which is one
    /// of the more confusing ways to spend an afternoon.
    pub const fn leading_underscore(self, data_model: DataModel) -> bool {
        match self {
            ObjectFormat::MachO => true,
            ObjectFormat::Coff => matches!(data_model, DataModel::Ilp32),
            ObjectFormat::Elf | ObjectFormat::Wasm => false,
        }
    }
}

impl fmt::Display for ObjectFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
