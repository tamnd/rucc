//! The i386 register file, and where the System V convention over it puts things.
//!
//! Design: `spec/10-backend.md` section 10.8 and `spec/12-abi-and-runtime.md` section 12.2, and
//! issue #2247, which is the backend this is the first piece of.
//!
//! Registers are numbered the way the instruction encoding numbers them, for the reason
//! [`crate::x86_64`] gives, so `eax` is zero, `esp` is four and `edi` is seven. On this machine
//! DWARF happens to number the general purpose registers the same way, which is not true on
//! x86-64 and is the one thing a reader coming from there should not carry across.
//!
//! The names are the thirty two bit ones, and `al` and `ax` are ways of writing part of `eax`
//! rather than registers of their own, as they are on x86-64.
//!
//! # What is not here yet
//!
//! Everything but the registers and the convention over them. The instruction tables, the frame,
//! the branches and the encoder arrive with the rules that select them, and nothing reaches this
//! file to generate code until `rucc_codegen::Machine::for_target` says so. The convention is here
//! ahead of that for the reason AArch64's was: the ABI tests and the debugging information read it.

use crate::regs::{
    CallRegs, ClassInfo, Conventions, Guard, PhysReg, RegClass, RegFile, Segment, Trace,
};

/// The general purpose registers.
pub const GPR: RegClass = RegClass::new(0);
/// The vector registers.
pub const XMM: RegClass = RegClass::new(1);
/// The x87 stack, which is where a `long double`, a `double` and a `float` come back.
pub const X87: RegClass = RegClass::new(2);

/// One general purpose register, by the number the encoding gives it.
pub const EAX: PhysReg = PhysReg::new(0);
/// One general purpose register, by the number the encoding gives it.
pub const ECX: PhysReg = PhysReg::new(1);
/// One general purpose register, by the number the encoding gives it.
pub const EDX: PhysReg = PhysReg::new(2);
/// One general purpose register, by the number the encoding gives it, and the one position
/// independent code keeps the address of the global offset table in.
pub const EBX: PhysReg = PhysReg::new(3);
/// The stack pointer.
pub const ESP: PhysReg = PhysReg::new(4);
/// The frame pointer.
pub const EBP: PhysReg = PhysReg::new(5);
/// One general purpose register, by the number the encoding gives it.
pub const ESI: PhysReg = PhysReg::new(6);
/// One general purpose register, by the number the encoding gives it.
pub const EDI: PhysReg = PhysReg::new(7);

/// The register position independent code keeps the global offset table's address in.
///
/// The i386 psABI has no addressing relative to the instruction pointer, so a function that
/// reaches a global through the table has to have the table's address in a register first, and
/// the psABI's procedure linkage table expects it in `ebx` at every call through it. So under
/// `-fpic` and `-fpie` this register is not one the allocator may hand out, which is
/// [`SYSV_PIC`].
pub const GOT_BASE: PhysReg = EBX;

/// The vector register with that number.
///
/// # Panics
///
/// Panics if there is no such register, which is eight or more: the extension that adds the
/// upper eight is the one that also makes the machine x86-64.
#[must_use]
pub const fn xmm(number: u8) -> PhysReg {
    assert!(number < 8, "i386 has eight vector registers");
    PhysReg::new(number)
}

/// The x87 register with that number, counted from the top of the stack.
///
/// # Panics
///
/// Panics if the number is eight or more, which is past the bottom of the stack.
#[must_use]
pub const fn st(number: u8) -> PhysReg {
    assert!(number < 8, "the x87 stack is eight deep");
    PhysReg::new(number)
}

static GPR_NAMES: [&str; 8] = ["eax", "ecx", "edx", "ebx", "esp", "ebp", "esi", "edi"];

static XMM_NAMES: [&str; 8] = ["xmm0", "xmm1", "xmm2", "xmm3", "xmm4", "xmm5", "xmm6", "xmm7"];

static X87_NAMES: [&str; 8] = ["st0", "st1", "st2", "st3", "st4", "st5", "st6", "st7"];

static CLASSES: [ClassInfo; 3] = [
    ClassInfo { name: "gpr", bits: 32, regs: &GPR_NAMES, allocatable: true },
    ClassInfo { name: "xmm", bits: 128, regs: &XMM_NAMES, allocatable: true },
    // Not allocatable, for the reason `crate::x86_64` gives: a name on this stack means whichever
    // register is that far from the top at the moment, which is not a register an allocator can
    // hand out. It matters more here than there, because every floating point value a function
    // returns comes back on it and not only a `long double`.
    ClassInfo { name: "x87", bits: 80, regs: &X87_NAMES, allocatable: false },
];

/// Every register i386 has, short of the MMX aliases of the x87 stack, which nothing here names.
pub static REGS: RegFile = RegFile::new(&CLASSES);

/// The general purpose registers by DWARF's number for them, indexed by the machine's.
///
/// The same order, which the i386 psABI chose and the x86-64 one did not. The table is written
/// out anyway, because the test that pins it is what stops somebody copying x86-64's permutation
/// here.
static GPR_DWARF: [u16; 8] = [0, 1, 2, 3, 4, 5, 6, 7];

/// The vector registers by DWARF's number. Eight is the return address, nine the flags and eleven
/// to eighteen the x87 stack, so the vector registers start at twenty one.
static XMM_DWARF: [u16; 8] = [21, 22, 23, 24, 25, 26, 27, 28];

/// What DWARF calls each i386 register, per class, with no column for the x87 stack for the
/// reason x86-64 has none: nothing saves one across a call.
static X86_DWARF: [&[u16]; 2] = [&GPR_DWARF, &XMM_DWARF];

/// The number the return address is filed under, which is `eip`'s column.
pub const DWARF_RETURN_ADDRESS: u16 = 8;

// Nothing. Every argument is in the argument area under cdecl, which is what the ABI description
// this is the register half of says too.
static SYSV_INT_ARGS: [PhysReg; 0] = [];
static SYSV_SSE_ARGS: [PhysReg; 0] = [];
// A 64-bit integer comes back in `edx:eax`, low half first.
static SYSV_INT_RETURNS: [PhysReg; 2] = [EAX, EDX];
// A `float`, a `double` and a `long double` all come back in `st0`, which is the difference from
// x86-64 most worth knowing: no scalar comes back in a vector register.
static SYSV_SSE_RETURNS: [PhysReg; 0] = [];
static SYSV_X87_RETURNS: [PhysReg; 1] = [st(0)];
static SYSV_INT_SAVED: [PhysReg; 4] = [EBX, ESI, EDI, EBP];
static SYSV_SSE_SAVED: [PhysReg; 0] = [];
// The three a call may destroy first, then the three it may not that the frame does not need.
static SYSV_INT_ORDER: [PhysReg; 6] = [EAX, ECX, EDX, ESI, EDI, EBX];
// The same without `ebx`, which position independent code has given to the table's address.
static SYSV_PIC_INT_ORDER: [PhysReg; 5] = [EAX, ECX, EDX, ESI, EDI];
static SSE_ORDER: [PhysReg; 8] = [xmm(0), xmm(1), xmm(2), xmm(3), xmm(4), xmm(5), xmm(6), xmm(7)];

/// Where an i386 System V call puts things: the cdecl convention on Linux and the other ELF
/// systems.
pub static SYSV: CallRegs = CallRegs {
    abi: &rucc_abi::abis::I386_SYSV,
    int_class: GPR,
    sse_class: XMM,
    int_args: &SYSV_INT_ARGS,
    sse_args: &SYSV_SSE_ARGS,
    shared_positions: false,
    int_returns: &SYSV_INT_RETURNS,
    sse_returns: &SYSV_SSE_RETURNS,
    x87_returns: &SYSV_X87_RETURNS,
    int_saved: &SYSV_INT_SAVED,
    sse_saved: &SYSV_SSE_SAVED,
    sse_kept: None,
    int_order: &SYSV_INT_ORDER,
    sse_order: &SSE_ORDER,
    stack_pointer: ESP,
    frame_pointer: EBP,
    late_frame_pointer: false,
    unwind_codes: false,
    vector_count: None,
    // None. The psABI never gave i386 one, and a signal handler runs on the interrupted stack.
    red_zone: 0,
    shadow: 0,
    home: 0,
    // Sixteen, which is what the psABI has said since gcc started assuming it, and what glibc's
    // own vector code on this target counts on.
    stack_align: 16,
    return_address: 4,
    word: 4,
    total_store_order: true,
    push: 4,
    link: None,
    // The address of a result returned through memory is the first word of the argument area,
    // where the first argument would have gone.
    sret: None,
    list: crate::VaList::CharPointer,
    dwarf: &X86_DWARF,
    dwarf_return_address: DWARF_RETURN_ADDRESS,
    // Twenty bytes into the thread's block, which is `%gs:20` in every protected function glibc
    // and musl on this target have linked.
    guard: Some(Guard { segment: Segment::Gs, at: 20, fail: "__stack_chk_fail" }),
    // The older hook by default, which is gcc's default on this target: `-mfentry` is there, and
    // nothing asks for it without saying so.
    trace: Some(Trace {
        early: "__fentry__",
        late: "mcount",
        fentry: false,
        nop: Some(crate::x86_64::NOP5),
    }),
    chkstk: None,
    conventions: Conventions::ONLY,
};

/// The same convention in position independent code, where [`GOT_BASE`] is not the allocator's.
///
/// Everything but the allocation order is [`SYSV`]'s. `ebx` is still preserved across a call,
/// which is exactly why the psABI chose it: a function that loaded the table's address keeps it
/// through every call it makes.
pub static SYSV_PIC: CallRegs = CallRegs { int_order: &SYSV_PIC_INT_ORDER, ..SYSV };

#[cfg(test)]
mod tests {
    use super::*;

    /// Every register in a list, and no register twice.
    fn covers(order: &[PhysReg], count: usize) -> bool {
        let mut seen: Vec<u8> = order.iter().map(|reg| reg.number()).collect();
        seen.sort_unstable();
        seen.dedup();
        seen.len() == order.len() && order.len() == count
    }

    #[test]
    fn the_file_numbers_registers_the_way_the_encoding_does() {
        assert_eq!(REGS.name(GPR, EAX), Some("eax"));
        assert_eq!(REGS.name(GPR, ESP), Some("esp"));
        assert_eq!(REGS.name(GPR, EDI), Some("edi"));
        assert_eq!(REGS.reg_named("ebx"), Some((GPR, EBX)));
        assert_eq!(REGS.reg_named("xmm7"), Some((XMM, xmm(7))));
        assert_eq!(REGS.reg_named("st0"), Some((X87, st(0))));
        assert_eq!(REGS.reg_named("rax"), None, "there is no sixty four bit register here");
        assert_eq!(REGS.reg_named("xmm8"), None);
    }

    #[test]
    fn the_file_gives_no_name_to_two_registers() {
        assert_eq!(REGS.duplicate(), None);
        assert_eq!(REGS.len(GPR), 8);
        assert_eq!(REGS.len(XMM), 8);
        assert_eq!(REGS.len(X87), 8);
        assert!(REGS.allocatable(GPR) && REGS.allocatable(XMM) && !REGS.allocatable(X87));
    }

    /// The i386 psABI's table, which agrees with the machine for the general purpose registers,
    /// unlike x86-64's.
    #[test]
    fn dwarf_numbers_the_registers_the_way_the_i386_psabi_does() {
        for (reg, number) in [(EAX, 0), (ECX, 1), (EDX, 2), (EBX, 3)] {
            assert_eq!(SYSV.dwarf(GPR, reg), Some(number));
        }
        for (reg, number) in [(ESP, 4), (EBP, 5), (ESI, 6), (EDI, 7)] {
            assert_eq!(SYSV.dwarf(GPR, reg), Some(number));
        }
        assert_eq!(SYSV.dwarf_return_address, 8);
        assert_eq!(SYSV.dwarf(XMM, xmm(0)), Some(21));
        assert_eq!(SYSV.dwarf(XMM, xmm(7)), Some(28));
        assert_eq!(SYSV.dwarf(X87, st(0)), None);
    }

    #[test]
    fn a_dwarf_number_leads_back_to_the_register_it_was_given_to() {
        for reg in [EAX, ECX, EDX, EBX, ESP, EBP, ESI, EDI] {
            let number = SYSV.dwarf(GPR, reg).expect("a general purpose register has a column");
            assert_eq!(SYSV.machine(GPR, number), Some(reg));
        }
        assert_eq!(SYSV.machine(XMM, 21), Some(xmm(0)));
        assert_eq!(SYSV.machine(GPR, 8), None, "the return address is not a register here");
    }

    #[test]
    fn cdecl_passes_nothing_in_registers_and_returns_in_three_places() {
        assert!(SYSV.int_args.is_empty() && SYSV.sse_args.is_empty());
        assert_eq!(SYSV.int_returns, &[EAX, EDX]);
        assert!(SYSV.sse_returns.is_empty(), "a double comes back on the x87 stack");
        assert_eq!(SYSV.x87_returns, &[st(0)]);
        assert_eq!((SYSV.word, SYSV.push, SYSV.return_address), (4, 4, 4));
        assert_eq!((SYSV.red_zone, SYSV.shadow, SYSV.vector_count), (0, 0, None));
        assert!(std::ptr::eq(SYSV.abi, &rucc_abi::abis::I386_SYSV));
    }

    #[test]
    fn the_psabi_says_which_registers_a_call_leaves_alone() {
        for reg in [EBX, ESI, EDI, EBP] {
            assert!(SYSV.preserves_int(reg));
        }
        for reg in [EAX, ECX, EDX] {
            assert!(!SYSV.preserves_int(reg));
        }
        assert!((0..8).all(|number| !SYSV.preserves_sse(xmm(number))));
    }

    #[test]
    fn the_allocator_is_offered_every_register_but_the_ones_reserved() {
        assert!(covers(SYSV.int_order, 6));
        assert!(covers(SYSV_PIC.int_order, 5));
        for convention in [&SYSV, &SYSV_PIC] {
            assert!(covers(convention.sse_order, 8));
            assert!(!convention.int_order.contains(&ESP));
            assert!(!convention.int_order.contains(&EBP));
        }
        assert!(SYSV.int_order.contains(&EBX));
        assert!(!SYSV_PIC.int_order.contains(&GOT_BASE));
        assert!(SYSV_PIC.preserves_int(GOT_BASE), "the table's address survives a call");
    }

    #[test]
    fn a_register_a_call_destroys_is_offered_before_one_it_preserves() {
        for convention in [&SYSV, &SYSV_PIC] {
            let first_saved = convention
                .int_order
                .iter()
                .position(|&reg| convention.preserves_int(reg))
                .expect("some register in the order is preserved");
            assert!(
                convention.int_order[first_saved..]
                    .iter()
                    .all(|&reg| convention.preserves_int(reg)),
                "the preserved registers are not one run at the end"
            );
        }
    }
}
