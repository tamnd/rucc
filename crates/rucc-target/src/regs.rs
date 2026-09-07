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
    /// Whether the allocator may put a value in one of these.
    ///
    /// True for every class a target means the allocator to use, which is nearly all of them.
    /// False says the registers exist and are named and are not somewhere a value may be told to
    /// live, so a virtual register of this class is a mistake at the point it was made rather than
    /// a value the allocator has nowhere to put.
    ///
    /// The x87 stack is the case this exists for, and it is worth the sentence because it is not
    /// the usual reason a register is unavailable. `rsp` is unavailable because it has a job;
    /// `st0` is unavailable because the machine addresses it as a stack, so which register a name
    /// means depends on how many values are on the stack at the time, and an allocator that hands
    /// out a name has no way to say that. So nothing allocates from it, an eighty bit value lives
    /// in a stack slot between one operation and the next, and the stack is empty on both sides of
    /// every group of instructions that uses it. See `spec/10-backend.md` section 10.8, which says
    /// what a group is and why nothing the allocator inserts can get into the middle of one, and
    /// tamnd/rucc#540.
    ///
    /// A register in such a class can still be named, which is the whole reason the class is
    /// described at all: a `long double` comes back from a call in `st0` and the convention has to
    /// be able to say so.
    pub allocatable: bool,
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

    /// Whether the allocator may put a value in that class, which is [`ClassInfo::allocatable`].
    ///
    /// A class the file does not have is not one either, which is the same answer as a class
    /// nothing allocates from and is the one that keeps a caller from having to say what it means
    /// by a class number the target never gave out.
    #[must_use]
    pub fn allocatable(&self, class: RegClass) -> bool {
        self.class(class).is_some_and(|info| info.allocatable)
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
    /// The class the general purpose registers named here are in.
    ///
    /// A register is a number inside its class, so a list of them says nothing about which
    /// registers they are without this. Everything else could get the class from the operand it
    /// came off, and a frame cannot, because a saved register is not an operand of anything.
    pub int_class: RegClass,
    /// The class the vector registers named here are in.
    pub sse_class: RegClass,
    /// The general purpose registers integer arguments arrive in, in order.
    pub int_args: &'static [PhysReg],
    /// The vector registers floating point arguments arrive in, in order.
    ///
    /// Whether an argument's position counts against both lists or only against its own is
    /// [`CallRegs::shared_positions`].
    pub sse_args: &'static [PhysReg],
    /// Whether an argument's position counts against both argument lists or only against its own.
    ///
    /// False on SysV, which counts each separately, so a `double` after six integers is still in
    /// `xmm0`. True on Windows, which counts one position for both, so a `double` in the third
    /// position is in `xmm2` and `r8` is skipped.
    pub shared_positions: bool,
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
    /// What the stack pointer has to be a multiple of at the instruction that makes a call.
    ///
    /// Sixteen on every convention here, and it is a real obligation rather than a preference,
    /// because a callee is entitled to use an aligned vector store on its own frame and gets a
    /// fault rather than a wrong answer when a caller got this wrong.
    pub stack_align: u32,
    /// How many bytes the call instruction itself pushes before the callee starts running.
    ///
    /// Eight on x86-64, where the return address is on the stack, and nothing on a machine that
    /// leaves it in a register. It is what makes the stack pointer misaligned on entry by
    /// exactly one word, which every frame layout has to undo.
    pub return_address: u32,
    /// How many bytes one general purpose register takes when it is saved on the stack.
    pub word: u32,
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

/// Where one of the values a call passes is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Where {
    /// In that register.
    Reg(PhysReg),
    /// That many bytes up the argument area, which is where the stack pointer points at the
    /// instruction that makes the call and is one word above the return address in the callee.
    Stack(u32),
}

/// Where the values a call passes are, worked out one after another.
///
/// [`crate::abi::Call`] answers a different question: whether a value travels in registers at all
/// and in how many, which is what decides the shape of a signature and is settled before the IR
/// for a function exists. This answers the question after it. Given values in the order the
/// signature holds them, it says which register each one is in and how far up the argument area
/// the ones that got no register are. Both count registers, and they agree about how many fit
/// because they read the same lists, but they run at opposite ends of the compiler and neither
/// can be the other.
///
/// Ask about each value in the order the signature holds them. Asking out of order answers about
/// a different signature, because where a value is depends on every value before it.
#[derive(Debug, Clone)]
pub struct Places<'a> {
    regs: &'a CallRegs,
    int: usize,
    sse: usize,
    stack: u32,
}

impl<'a> Places<'a> {
    /// Where the first value is, for a call under that convention.
    #[must_use]
    pub fn new(regs: &'a CallRegs) -> Self {
        Self { regs, int: 0, sse: 0, stack: regs.shadow }
    }

    /// Where the next value is, when it travels in a general purpose register.
    pub fn integer(&mut self) -> Where {
        match self.regs.int_args.get(self.position(false)) {
            Some(&reg) => {
                self.int += 1;
                Where::Reg(reg)
            }
            None => self.on_stack(self.regs.word, self.regs.word),
        }
    }

    /// Where the next value is, when it travels in a vector register.
    pub fn float(&mut self) -> Where {
        match self.regs.sse_args.get(self.position(true)) {
            Some(&reg) => {
                self.sse += 1;
                Where::Reg(reg)
            }
            None => self.on_stack(self.regs.word, self.regs.word),
        }
    }

    /// Where the next value is, when it travels in memory whatever is left.
    ///
    /// Every argument area is a run of whole words, so a value narrower than one still takes one
    /// and a value that is not a whole number of them is rounded up. An alignment wider than a
    /// word is respected, which is what a sixteen byte aligned structure passed by value needs.
    pub fn on_stack(&mut self, size: u32, align: u32) -> Where {
        let word = self.regs.word;
        let at = self.stack.next_multiple_of(align.max(word));
        self.stack = at.saturating_add(size.max(word).next_multiple_of(word));
        Where::Stack(at)
    }

    /// How many bytes of argument area the values so far need, shadow space included.
    #[must_use]
    pub fn size(&self) -> u32 {
        self.stack
    }

    /// How many general purpose argument registers the values so far took.
    ///
    /// What a variadic callee needs and nothing else does. `va_start` has to record how far into
    /// each of the two register sequences the arguments the signature names got, because the first
    /// argument it does not name is the one after them, and asking here is the only way to know
    /// that is the same count the caller worked from.
    #[must_use]
    pub fn integers(&self) -> usize {
        self.int
    }

    /// How many vector argument registers the values so far took.
    #[must_use]
    pub fn floats(&self) -> usize {
        self.sse
    }

    /// The position the next value of a kind is at.
    fn position(&self, sse: bool) -> usize {
        if self.regs.shared_positions {
            self.int + self.sse
        } else if sse {
            self.sse
        } else {
            self.int
        }
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
        ClassInfo { name: "gpr", bits: 64, regs: &GPR, allocatable: true },
        ClassInfo { name: "xmm", bits: 128, regs: &XMM, allocatable: true },
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
            ClassInfo { name: "gpr", bits: 64, regs: &GPR, allocatable: true },
            ClassInfo { name: "shadow", bits: 64, regs: &GPR, allocatable: true },
        ];
        assert_eq!(RegFile::new(&BOTH).duplicate(), Some("rax"));
    }

    #[test]
    fn a_class_nothing_allocates_from_is_still_a_class_in_every_other_way() {
        static WITH_STACK: [ClassInfo; 2] = [
            ClassInfo { name: "gpr", bits: 64, regs: &GPR, allocatable: true },
            ClassInfo { name: "x87", bits: 80, regs: &XMM, allocatable: false },
        ];
        let file = RegFile::new(&WITH_STACK);
        let stack = file.class_named("x87").expect("the file has an x87 class");

        assert!(!file.allocatable(stack));
        assert!(file.allocatable(file.class_named("gpr").expect("the file has a gpr class")));

        // Everything else about it works, which is the point of describing a class the allocator
        // will not touch: the registers are counted, are named, and name themselves back.
        assert_eq!(file.len(stack), 2);
        assert_eq!(file.name(stack, PhysReg::new(1)), Some("xmm1"));
        assert_eq!(file.reg_named("xmm1"), Some((stack, PhysReg::new(1))));
    }

    #[test]
    fn a_class_the_file_does_not_have_is_not_one_to_allocate_from_either() {
        assert!(!FILE.allocatable(RegClass::new(7)));
    }

    #[test]
    fn the_file_prints_one_class_to_a_line() {
        assert_eq!(
            FILE.to_string(),
            "class gpr : i64 = rax, rcx, rdx\nclass xmm : i128 = xmm0, xmm1\n"
        );
    }

    /// Two integer registers, two vector registers and nothing else, so running out of them takes
    /// three arguments rather than seven and the interesting case is the one being tested.
    fn convention(shared: bool, shadow: u32) -> CallRegs {
        static INT: [PhysReg; 2] = [PhysReg::new(0), PhysReg::new(1)];
        static SSE: [PhysReg; 2] = [PhysReg::new(10), PhysReg::new(11)];
        static NONE: [PhysReg; 0] = [];
        CallRegs {
            int_class: RegClass::new(0),
            sse_class: RegClass::new(1),
            int_args: &INT,
            sse_args: &SSE,
            shared_positions: shared,
            int_returns: &INT,
            sse_returns: &SSE,
            x87_returns: &NONE,
            int_saved: &NONE,
            sse_saved: &NONE,
            int_order: &INT,
            sse_order: &SSE,
            stack_pointer: PhysReg::new(4),
            frame_pointer: PhysReg::new(5),
            vector_count: None,
            red_zone: 0,
            shadow,
            stack_align: 16,
            return_address: 8,
            word: 8,
        }
    }

    #[test]
    fn counting_each_kind_separately_leaves_the_first_vector_register_to_the_first_float() {
        let regs = convention(false, 0);
        let mut places = Places::new(&regs);
        assert_eq!(places.integer(), Where::Reg(PhysReg::new(0)));
        assert_eq!(places.integer(), Where::Reg(PhysReg::new(1)));
        // Two integers went past, and a convention that counts separately has not spent a vector
        // register on either of them.
        assert_eq!(places.float(), Where::Reg(PhysReg::new(10)));
        assert_eq!(places.size(), 0);
    }

    #[test]
    fn counting_one_position_for_both_skips_the_register_the_other_kind_would_have_used() {
        let regs = convention(true, 0);
        let mut places = Places::new(&regs);
        assert_eq!(places.integer(), Where::Reg(PhysReg::new(0)));
        // The second position, so the second vector register, and the second integer register is
        // spent whether anything is in it or not.
        assert_eq!(places.float(), Where::Reg(PhysReg::new(11)));
        assert_eq!(places.integer(), Where::Stack(0));
    }

    #[test]
    fn running_out_of_one_kind_of_register_does_not_touch_the_other() {
        let regs = convention(false, 0);
        let mut places = Places::new(&regs);
        assert_eq!(places.integer(), Where::Reg(PhysReg::new(0)));
        assert_eq!(places.integer(), Where::Reg(PhysReg::new(1)));
        assert_eq!(places.integer(), Where::Stack(0));
        assert_eq!(places.float(), Where::Reg(PhysReg::new(10)));
        assert_eq!(places.size(), 8);
    }

    #[test]
    fn the_argument_area_starts_above_the_shadow_space_and_keeps_every_value_aligned() {
        let regs = convention(false, 32);
        let mut places = Places::new(&regs);
        // A Windows caller reserves this whether it passes anything on the stack or not, which is
        // why an empty area is thirty two bytes rather than none.
        assert_eq!(places.size(), 32);
        assert_eq!(places.on_stack(4, 4), Where::Stack(32));
        // Sixteen byte alignment skips the word at 40, which is what a vector or an over-aligned
        // structure passed by value asks for. The four byte value before it still took a whole
        // word, which is why the skipped word is there to skip.
        assert_eq!(places.on_stack(16, 16), Where::Stack(48));
        assert_eq!(places.on_stack(8, 8), Where::Stack(64));
        assert_eq!(places.size(), 72);
    }
}
