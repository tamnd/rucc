//! The register file: what registers a target has, and what classes they fall into.
//!
//! Design: `spec/10-backend.md` section 10.8.
//!
//! A register file is data rather than code, which is the same claim the rest of this crate
//! makes and the one `M10` puts a number on. A class is a set of registers that an operand of
//! that class may be assigned to, and a physical register is its number inside its class, so
//! the allocator works in dense small integers and only the printer and the parser ever deal in
//! names.
//!
//! The file lives here rather than in `rucc-mir` because more than one thing reads it. The
//! machine IR needs it to print, the allocator needs the set it may assign from, and the ABI
//! description needs to name the registers arguments arrive in. All three are above this crate,
//! and the alternative is the register file living in whichever of them happens to be lowest,
//! which is how a layering ends up describing itself as historical.
//!
//! Names are unique across the whole file, not merely inside a class. That is what lets a
//! register be written `$rax` in a dump rather than `$gpr.0`, and it is a real constraint on a
//! target that gives one register two classes: it has to say which class it is in, or use two
//! names. [`RegFile::duplicate`] is what a target's own test asks to find out.

use std::fmt;

/// One class of registers, and the registers in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClassInfo {
    /// What the class is called in a dump, such as `gpr`.
    pub name: &'static str,
    /// How wide one of its registers is, in bits.
    pub bits: u32,
    /// The registers, in the order their numbers run, without the sigil a dump writes.
    pub regs: &'static [&'static str],
}

/// Which class a register or an operand belongs to.
///
/// A number into the file's classes rather than a name, because it is on every operand of every
/// instruction and it is compared far more often than it is printed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RegClass(u8);

impl RegClass {
    /// The class with that number.
    #[must_use]
    pub const fn new(number: u8) -> Self {
        Self(number)
    }

    /// Its number, which is what indexes the file.
    #[must_use]
    pub const fn number(self) -> u8 {
        self.0
    }
}

/// One physical register, as its number inside its class.
///
/// The class is not in here. An operand carries its class already, and a fixed-register
/// constraint is a constraint on an operand, so repeating the class would be a second copy of
/// something that can disagree with the first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PhysReg(u8);

impl PhysReg {
    /// The register with that number in its class.
    #[must_use]
    pub const fn new(number: u8) -> Self {
        Self(number)
    }

    /// Its number inside its class.
    #[must_use]
    pub const fn number(self) -> u8 {
        self.0
    }
}

/// Every register a target has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegFile {
    classes: &'static [ClassInfo],
}

impl RegFile {
    /// The file of a target whose registers nothing has described yet.
    ///
    /// A target reaches 1.0 with a real one. Until it has one, the honest answer to what
    /// registers it has is that nobody has written them down, and that is a file with no
    /// classes in it rather than a panic or a plausible guess.
    pub const EMPTY: Self = Self::new(&[]);

    /// A file made of those classes, numbered in the order they are given.
    #[must_use]
    pub const fn new(classes: &'static [ClassInfo]) -> Self {
        Self { classes }
    }

    /// Its classes, each with the number it is known by.
    pub fn classes(&self) -> impl Iterator<Item = (RegClass, &'static ClassInfo)> + use<> {
        self.classes.iter().enumerate().map(|(number, info)| (RegClass::new(number as u8), info))
    }

    /// What is in one class.
    #[must_use]
    pub fn class(&self, class: RegClass) -> Option<&'static ClassInfo> {
        self.classes.get(usize::from(class.number()))
    }

    /// The class of that name, such as `gpr`.
    #[must_use]
    pub fn class_named(&self, name: &str) -> Option<RegClass> {
        self.classes().find(|(_, info)| info.name == name).map(|(class, _)| class)
    }

    /// How many registers are in a class, which is one past the largest number in it.
    #[must_use]
    pub fn len(&self, class: RegClass) -> usize {
        self.class(class).map_or(0, |info| info.regs.len())
    }

    /// Whether the file has no classes at all, which is a target that has not described one.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.classes.is_empty()
    }

    /// What one register is called.
    #[must_use]
    pub fn name(&self, class: RegClass, reg: PhysReg) -> Option<&'static str> {
        self.class(class)?.regs.get(usize::from(reg.number())).copied()
    }

    /// The register of that name, and the class it is in.
    ///
    /// The name is written without the sigil, so `rax` rather than `$rax`.
    #[must_use]
    pub fn reg_named(&self, name: &str) -> Option<(RegClass, PhysReg)> {
        for (class, info) in self.classes() {
            if let Some(number) = info.regs.iter().position(|&reg| reg == name) {
                return Some((class, PhysReg::new(number as u8)));
            }
        }
        None
    }

    /// A name this file gives to two registers, if it gives one to two.
    ///
    /// Reading a dump back needs every name to say which register it means, and a target that
    /// breaks that produces text that cannot be parsed rather than an error at the point of the
    /// mistake. So every target's own test asks this, which is why it is here and public.
    #[must_use]
    pub fn duplicate(&self) -> Option<&'static str> {
        let mut seen: Vec<&'static str> = Vec::new();
        for (_, info) in self.classes() {
            for &reg in info.regs {
                if seen.contains(&reg) {
                    return Some(reg);
                }
                seen.push(reg);
            }
        }
        None
    }
}

/// Which registers a calling convention gives which job.
///
/// This is the second half of a target description and it is separate from [`RegFile`] because
/// the two do not vary together. x86-64 has one register file and two conventions over it, and
/// they disagree about nearly everything below: `rdi` is where the first argument arrives on
/// SysV and a register a callee has to preserve on Windows, and a Windows caller reserves
/// thirty two bytes below the call that a SysV caller does not.
///
/// The allocation order is here rather than on a class because it is a consequence of what a
/// call clobbers. A value that does not live across a call belongs in a register the callee is
/// free to destroy, because putting it in a preserved one costs a push and a pop in the
/// prologue of whichever function ends up owning it.
///
/// Every register named here is a register of the file the same target describes, and each list
/// is in the order the convention uses them, so the fourth integer argument is `int_args[3]` and
/// nothing has to count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallRegs {
    /// The general purpose registers integer arguments arrive in, in order.
    pub int_args: &'static [PhysReg],
    /// The vector registers floating point arguments arrive in, in order.
    ///
    /// Whether an argument's position counts against both lists or only against its own is the
    /// convention's own answer and is not recorded here: SysV counts each separately, so a
    /// `double` after six integers is still in `xmm0`, and Windows counts one position for
    /// both, so a `double` in the third position is in `xmm2` and `r8` is skipped.
    pub sse_args: &'static [PhysReg],
    /// The general purpose registers an integer return value comes back in.
    pub int_returns: &'static [PhysReg],
    /// The vector registers a floating point return value comes back in.
    pub sse_returns: &'static [PhysReg],
    /// The x87 registers a `long double` comes back in, which is empty on a target whose
    /// `long double` is a `double`.
    pub x87_returns: &'static [PhysReg],
    /// The general purpose registers a call leaves alone, so a value in one survives it.
    pub int_saved: &'static [PhysReg],
    /// The vector registers a call leaves alone, which is none of them on SysV.
    pub sse_saved: &'static [PhysReg],
    /// The general purpose registers the allocator may hand out, in the order it prefers them.
    ///
    /// The stack pointer is never in this list, and neither is the frame pointer, which a
    /// target could allocate when nothing needs a frame and which nothing here does yet.
    pub int_order: &'static [PhysReg],
    /// The vector registers the allocator may hand out, in the order it prefers them.
    pub sse_order: &'static [PhysReg],
    /// The stack pointer.
    pub stack_pointer: PhysReg,
    /// The frame pointer, which is the register a prologue puts the old stack pointer in.
    pub frame_pointer: PhysReg,
    /// Where a variadic call says how many vector registers it passed arguments in, when the
    /// convention makes it say.
    ///
    /// SysV puts the count in `al` and a variadic callee reads it to decide whether to save the
    /// vector argument registers at all, which is what makes a call to `printf` with no
    /// floating point argument cheap.
    pub vector_count: Option<PhysReg>,
    /// How many bytes below the stack pointer a leaf function may use without moving it.
    ///
    /// A hundred and twenty eight on SysV and nothing on Windows. It is nothing in kernel code
    /// on either, because an interrupt handler runs on the interrupted stack and writes over
    /// exactly this, which is what `-mno-red-zone` is for.
    pub red_zone: u32,
    /// How many bytes a caller reserves below the call for the callee to spill its register
    /// arguments into, which is thirty two on Windows and nothing on SysV.
    pub shadow: u32,
}

impl CallRegs {
    /// Whether a call preserves that general purpose register.
    #[must_use]
    pub fn preserves_int(&self, reg: PhysReg) -> bool {
        self.int_saved.contains(&reg)
    }

    /// Whether a call preserves that vector register.
    #[must_use]
    pub fn preserves_sse(&self, reg: PhysReg) -> bool {
        self.sse_saved.contains(&reg)
    }
}

impl fmt::Display for RegFile {
    /// The file as a dump reads it, one class to a line.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (_, info) in self.classes() {
            writeln!(f, "class {} : i{} = {}", info.name, info.bits, info.regs.join(", "))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static GPR: [&str; 3] = ["rax", "rcx", "rdx"];
    static XMM: [&str; 2] = ["xmm0", "xmm1"];
    static CLASSES: [ClassInfo; 2] = [
        ClassInfo { name: "gpr", bits: 64, regs: &GPR },
        ClassInfo { name: "xmm", bits: 128, regs: &XMM },
    ];
    static FILE: RegFile = RegFile::new(&CLASSES);

    #[test]
    fn a_class_is_found_by_its_name() {
        let gpr = FILE.class_named("gpr").expect("the file has a gpr class");
        assert_eq!(FILE.len(gpr), 3);
        assert_eq!(FILE.class(gpr).map(|info| info.bits), Some(64));
        assert_eq!(FILE.class_named("vec"), None);
    }

    #[test]
    fn a_register_is_found_by_its_name_and_names_itself_back() {
        let (class, reg) = FILE.reg_named("xmm1").expect("the file has xmm1");
        assert_eq!(FILE.class(class).map(|info| info.name), Some("xmm"));
        assert_eq!(reg.number(), 1);
        assert_eq!(FILE.name(class, reg), Some("xmm1"));
        assert_eq!(FILE.reg_named("r15"), None);
    }

    #[test]
    fn a_number_past_the_end_of_a_class_has_no_name() {
        let gpr = FILE.class_named("gpr").expect("the file has a gpr class");
        assert_eq!(FILE.name(gpr, PhysReg::new(3)), None);
        assert_eq!(FILE.name(RegClass::new(7), PhysReg::new(0)), None);
    }

    #[test]
    fn a_file_that_names_two_registers_alike_says_so() {
        assert_eq!(FILE.duplicate(), None);
        static BOTH: [ClassInfo; 2] = [
            ClassInfo { name: "gpr", bits: 64, regs: &GPR },
            ClassInfo { name: "shadow", bits: 64, regs: &GPR },
        ];
        assert_eq!(RegFile::new(&BOTH).duplicate(), Some("rax"));
    }

    #[test]
    fn the_file_prints_one_class_to_a_line() {
        assert_eq!(
            FILE.to_string(),
            "class gpr : i64 = rax, rcx, rdx\nclass xmm : i128 = xmm0, xmm1\n"
        );
    }
}
