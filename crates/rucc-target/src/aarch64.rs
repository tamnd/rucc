//! The AArch64 register file, and where the two conventions over it put things.
//!
//! Design: `spec/10-backend.md` section 10.8, `spec/12-abi-and-runtime.md` section 12.3 and
//! `spec/cross-compile/06-abis.md` sections 6.2 and 6.3.
//!
//! Registers are numbered the way the instruction encoding numbers them, which on this machine is
//! also the way DWARF numbers them, so `x0` is zero and `x30` is thirty. Number thirty one is the
//! one place that needs a sentence. The encoding uses it for two different registers, the stack
//! pointer in an operand that can be one and the zero register in an operand that cannot, and
//! which it is belongs to the instruction rather than to the number. Here it is `sp`, because that
//! is the one a frame names and a calling convention has to say, and the zero register is a
//! spelling the encoder picks when an instruction reads nothing or throws its result away. Nothing
//! allocates either of them.
//!
//! The names are the sixty four bit ones for the reason [`crate::x86_64`] gives for its own: `w0`
//! is the low half of `x0` rather than a second register, and the width is the instruction's. The
//! vector registers are `v0` to `v31` for the same reason, and `d0`, `s0` and `q0` are what an
//! instruction that reads eight, four or sixteen bytes of one writes.
//!
//! # Instructions
//!
//! [`encode`] writes one instruction as its word, and [`read`] reads one written the way GNU as
//! takes it, which together are enough to check every word against the ones GNU as writes. The
//! table of what each instruction does with its operands arrives with the lowering that selects
//! them, and so does the frame's list of what a prologue is made of.
//!
//! The flags register, for the reason x86-64 leaves it out: a comparison and whatever reads it are
//! one rule. And the scalable vector and predicate registers, which arrive with the target features
//! that have them.

mod encode;
mod read;

pub use crate::aarch64::encode::{
    Addr, Arrangement, Cond, Encoded, Error, Extend, Fixup, Mode, Offset, Operator, Scalar, Shift,
    Value, Width, encode,
};
pub use crate::aarch64::read::{Error as ReadError, Line, read};

use crate::regs::{CallRegs, ClassInfo, PhysReg, RegClass, RegFile};

/// The general purpose registers.
pub const GPR: RegClass = RegClass::new(0);
/// The floating point and vector registers.
pub const FPR: RegClass = RegClass::new(1);

/// The general purpose register with that number.
///
/// # Panics
///
/// Panics at thirty one or more. Thirty one is the stack pointer and has a name of its own,
/// [`SP`], because an instruction that writes `x31` means the zero register and a frame that means
/// the stack pointer has to say so.
#[must_use]
pub const fn x(number: u8) -> PhysReg {
    assert!(number < 31, "x31 is not a general purpose register, it is sp or xzr");
    PhysReg::new(number)
}

/// The vector register with that number.
///
/// # Panics
///
/// Panics at thirty two or more.
#[must_use]
pub const fn v(number: u8) -> PhysReg {
    assert!(number < 32, "AArch64 has thirty two vector registers");
    PhysReg::new(number)
}

/// Where a call returns a structure too large for registers, which the callee writes through.
pub const X8: PhysReg = x(8);
/// The first of the two registers a linker's veneer and a PLT entry may destroy.
pub const X16: PhysReg = x(16);
/// The second of them.
pub const X17: PhysReg = x(17);
/// The platform register, which Apple and Microsoft reserve and Linux does not.
pub const X18: PhysReg = x(18);
/// The frame pointer.
pub const FP: PhysReg = x(29);
/// The link register, where a call leaves the address it came from.
pub const LR: PhysReg = x(30);
/// The stack pointer.
pub const SP: PhysReg = PhysReg::new(31);

static GPR_NAMES: [&str; 32] = [
    "x0", "x1", "x2", "x3", "x4", "x5", "x6", "x7", "x8", "x9", "x10", "x11", "x12", "x13", "x14",
    "x15", "x16", "x17", "x18", "x19", "x20", "x21", "x22", "x23", "x24", "x25", "x26", "x27",
    "x28", "x29", "x30", "sp",
];

static FPR_NAMES: [&str; 32] = [
    "v0", "v1", "v2", "v3", "v4", "v5", "v6", "v7", "v8", "v9", "v10", "v11", "v12", "v13", "v14",
    "v15", "v16", "v17", "v18", "v19", "v20", "v21", "v22", "v23", "v24", "v25", "v26", "v27",
    "v28", "v29", "v30", "v31",
];

static CLASSES: [ClassInfo; 2] = [
    ClassInfo { name: "gpr", bits: 64, regs: &GPR_NAMES, allocatable: true },
    ClassInfo { name: "fpr", bits: 128, regs: &FPR_NAMES, allocatable: true },
];

/// Every register AArch64 has, as far as this compiler is concerned.
pub static REGS: RegFile = RegFile::new(&CLASSES);

/// The general purpose registers by DWARF's number for them, which is the machine's.
///
/// The DWARF supplement to AAPCS64 numbers `x0` to `x30` from zero and puts the stack pointer at
/// thirty one, which is exactly the encoding's order, so unlike x86-64 there is no permutation to
/// get wrong. The list is still written out rather than computed, because it is what an unwind
/// table is checked against.
static GPR_DWARF: [u16; 32] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25,
    26, 27, 28, 29, 30, 31,
];

/// The vector registers by DWARF's number, which start at sixty four.
static FPR_DWARF: [u16; 32] = [
    64, 65, 66, 67, 68, 69, 70, 71, 72, 73, 74, 75, 76, 77, 78, 79, 80, 81, 82, 83, 84, 85, 86, 87,
    88, 89, 90, 91, 92, 93, 94, 95,
];

static AARCH64_DWARF: [&[u16]; 2] = [&GPR_DWARF, &FPR_DWARF];

/// The column an unwind table files the return address under, which here is a real register.
///
/// A call on this machine leaves the address it came from in `x30` rather than on the stack, so
/// the return address is whatever `x30` held on entry, and the CIE names that register's column.
pub const DWARF_RETURN_ADDRESS: u16 = 30;

static INT_ARGS: [PhysReg; 8] = [x(0), x(1), x(2), x(3), x(4), x(5), x(6), x(7)];
static FP_ARGS: [PhysReg; 8] = [v(0), v(1), v(2), v(3), v(4), v(5), v(6), v(7)];
// Two for a sixteen byte integer or a small structure, and four for a homogeneous aggregate of four
// members, which is the most either bank gives back.
static INT_RETURNS: [PhysReg; 2] = [x(0), x(1)];
static FP_RETURNS: [PhysReg; 4] = [v(0), v(1), v(2), v(3)];
static NO_X87: [PhysReg; 0] = [];
// The frame pointer and the link register are not here, although a callee gives both back. The
// frame record the prologue writes saves them as a pair whether or not the function writes to
// either, so they are the frame's business rather than the allocator's, the way `rbp` is on
// x86-64.
static INT_SAVED: [PhysReg; 10] =
    [x(19), x(20), x(21), x(22), x(23), x(24), x(25), x(26), x(27), x(28)];
// Only the low sixty four bits of these are preserved, which is `d8` to `d15`. That is enough for
// everything this compiler keeps in one, which is a scalar, and it is why a prologue saves eight
// bytes of each rather than sixteen. A value that used the whole register would not survive a call
// in any of them.
static FP_SAVED: [PhysReg; 8] = [v(8), v(9), v(10), v(11), v(12), v(13), v(14), v(15)];

// The ones a call destroys first, so a value that does not live across one costs no save, then the
// ones it preserves. Left out: `x16` and `x17`, which a linker's veneer or a PLT entry may write
// between any call and the function it reaches, so nothing can live in one across a branch the
// compiler did not write; `x18`, for the reason [`X18`] gives, which on Linux is a choice to be the
// same as the other two platforms rather than an obligation; the frame pointer, the link register
// and the stack pointer. `x8` is handed out like any other, because it only has a job at the
// instant of a call that returns a large structure and the call constrains it there.
static INT_ORDER: [PhysReg; 26] = [
    x(0),
    x(1),
    x(2),
    x(3),
    x(4),
    x(5),
    x(6),
    x(7),
    x(8),
    x(9),
    x(10),
    x(11),
    x(12),
    x(13),
    x(14),
    x(15),
    x(19),
    x(20),
    x(21),
    x(22),
    x(23),
    x(24),
    x(25),
    x(26),
    x(27),
    x(28),
];
// The same idea: the eight argument registers, the sixteen above the preserved block, and the
// preserved block last.
static FP_ORDER: [PhysReg; 32] = [
    v(0),
    v(1),
    v(2),
    v(3),
    v(4),
    v(5),
    v(6),
    v(7),
    v(16),
    v(17),
    v(18),
    v(19),
    v(20),
    v(21),
    v(22),
    v(23),
    v(24),
    v(25),
    v(26),
    v(27),
    v(28),
    v(29),
    v(30),
    v(31),
    v(8),
    v(9),
    v(10),
    v(11),
    v(12),
    v(13),
    v(14),
    v(15),
];

/// Where an AAPCS64 call puts things on Linux and every other platform that uses it unchanged, per
/// `spec/12-abi-and-runtime.md` section 12.3.
pub static AAPCS64: CallRegs = aapcs64(&rucc_abi::abis::AAPCS64, 0);

/// Where a call puts things on Apple's platforms.
///
/// The same registers as [`AAPCS64`]. What Apple changed is how a variadic argument and a stack
/// argument travel, which is in the description rather than here, and one thing that is here: a
/// leaf function may use the hundred and twenty eight bytes below the stack pointer, which Apple's
/// ABI document promises and the Linux one does not.
pub static DARWIN: CallRegs = aapcs64(&rucc_abi::abis::DARWIN_ARM64, 128);

const fn aapcs64(abi: &'static rucc_abi::AbiDescription, red_zone: u32) -> CallRegs {
    CallRegs {
        abi,
        int_class: GPR,
        sse_class: FPR,
        int_args: &INT_ARGS,
        sse_args: &FP_ARGS,
        shared_positions: false,
        int_returns: &INT_RETURNS,
        sse_returns: &FP_RETURNS,
        x87_returns: &NO_X87,
        int_saved: &INT_SAVED,
        sse_saved: &FP_SAVED,
        int_order: &INT_ORDER,
        sse_order: &FP_ORDER,
        stack_pointer: SP,
        frame_pointer: FP,
        // The frame record goes at the bottom of the saved area and the pointer is set to it once
        // the frame is taken, which is still the order that keeps the chain: the word it names is
        // the caller's frame pointer and the one above it is the return address.
        late_frame_pointer: false,
        // Nothing says how many vector registers a variadic call used. The callee saves all eight
        // in its prologue if it is variadic at all, which is cheaper on this machine than the
        // branch SysV spends a register on.
        vector_count: None,
        red_zone,
        shadow: 0,
        stack_align: 16,
        // A call leaves the return address in the link register, so it pushes nothing and the
        // stack pointer is aligned on entry, which is the thing x86-64's frame layout has to undo.
        return_address: 0,
        word: 8,
        dwarf: &AARCH64_DWARF,
        dwarf_return_address: DWARF_RETURN_ADDRESS,
        // None for now, and it is a different mechanism rather than a missing number. glibc and
        // musl on this machine keep the canary in an ordinary global, `__stack_chk_guard`, which is
        // read through the GOT like any other, where [`crate::Guard`] describes a word at a fixed
        // distance into a thread's own block. A command line that asks for a protector is told so
        // until the frame code can load a global.
        guard: None,
        // None for the same kind of reason. gcc's hook on this machine is `_mcount` called with the
        // caller's link register, and `-mfentry` does not exist here, so it waits for the frame code
        // too rather than being described as something it is not.
        trace: None,
        // Linux and Apple both grow a stack by faulting, as System V does on x86-64.
        chkstk: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_file_has_one_name_per_register() {
        assert_eq!(REGS.duplicate(), None);
        assert_eq!(REGS.len(GPR), 32);
        assert_eq!(REGS.len(FPR), 32);
        assert_eq!(REGS.name(GPR, SP), Some("sp"));
        assert_eq!(REGS.name(GPR, LR), Some("x30"));
        assert_eq!(REGS.reg_named("v31"), Some((FPR, v(31))));
    }

    #[test]
    fn dwarf_numbers_are_the_machine_numbers_and_the_vectors_start_at_sixty_four() {
        for regs in [&AAPCS64, &DARWIN] {
            assert_eq!(regs.dwarf(GPR, x(0)), Some(0));
            assert_eq!(regs.dwarf(GPR, SP), Some(31));
            assert_eq!(regs.dwarf(FPR, v(8)), Some(72));
            assert_eq!(regs.machine(FPR, 95), Some(v(31)));
            assert_eq!(regs.dwarf_return_address, 30);
        }
    }

    #[test]
    fn nothing_with_a_job_is_handed_out() {
        for reserved in [X16, X17, X18, FP, LR, SP] {
            assert!(!AAPCS64.int_order.contains(&reserved), "{reserved:?}");
        }
        // Every register the allocator may use is either one a call destroys or one the frame
        // saves, and every one the frame saves is one it may use.
        for reg in AAPCS64.int_saved {
            assert!(AAPCS64.int_order.contains(reg), "{reg:?}");
        }
        assert_eq!(AAPCS64.int_order.len(), 26);
        assert_eq!(AAPCS64.sse_order.len(), 32);
    }

    #[test]
    fn the_two_conventions_differ_in_the_red_zone_and_the_description() {
        assert_eq!(AAPCS64.red_zone, 0);
        assert_eq!(DARWIN.red_zone, 128);
        assert!(std::ptr::eq(AAPCS64.abi, &rucc_abi::abis::AAPCS64));
        assert!(std::ptr::eq(DARWIN.abi, &rucc_abi::abis::DARWIN_ARM64));
        assert_eq!(AAPCS64.int_args, DARWIN.int_args);
        assert_eq!(AAPCS64.return_address, 0);
    }
}
