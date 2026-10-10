//! The i386 register file, where the System V convention over it puts things, and which of the
//! x86-64 instructions it has.
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
//! The instructions are x86-64's. i386 is the same instruction set with the REX prefix taken away
//! and addresses thirty two bits wide, so rather than a second description this file holds the
//! tables that say which part of [`crate::x86_64`]'s one an i386 function may use: [`MACHINE`]
//! refuses every opcode the encoder cannot write in [`Mode::Bits32`], and [`FRAME`] and [`BRANCH`]
//! name the thirty two bit forms where x86-64's name the sixty four bit ones. The opcodes keep
//! their `x64.` prefix, since they are the same instructions, and what tells an i386 function
//! apart is its target.
//!
//! # What is not here yet
//!
//! Nothing reaches this file to generate code until `rucc_codegen::Machine::for_target` says so,
//! which waits on the cdecl call lowering: arguments on the stack and a 64-bit result in
//! `edx:eax`. The byte registers are another piece still to come, since only the first four
//! general purpose registers have one on this machine and the allocator has no way to say so yet.

use crate::branch::{BranchInsts, Fusion, Move};
use crate::counts::{BitCount, CountInst};
use crate::frame::{ClassMoves, FrameInsts, Thunks};
use crate::machine::MachineInsts;
use crate::operand::OperandDesc;
use crate::regs::{
    CallRegs, Chkstk, ClassInfo, Conventions, Guard, PhysReg, RegClass, RegFile, Segment, Trace,
};
use crate::x86_64::{Form, Kind, Mode, encoding_in, form, written};
use rucc_abi::Convention;

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
    trusted_align: 16,
    return_address: 4,
    word: 4,
    total_store_order: true,
    unaligned: true,
    byte_swaps: &[16, 32],
    push: 4,
    link: None,
    // The address of a result returned through memory is the first word of the argument area,
    // where the first argument would have gone.
    sret: None,
    chain: None,
    list: crate::VaList::CharPointer,
    dwarf: &X86_DWARF,
    dwarf_return_address: DWARF_RETURN_ADDRESS,
    // Twenty bytes into the thread's block, which is `%gs:20` in every protected function glibc
    // and musl on this target have linked.
    guard: Some(Guard::in_segment(Segment::Gs, 20)),
    // The older hook by default, which is gcc's default on this target: `-mfentry` is there, and
    // nothing asks for it without saying so.
    trace: Some(Trace { early: "__fentry__", late: "mcount", fentry: false }),
    chkstk: None,
    conventions: Conventions(&OWN_0),
};

// The registers gcc's `regparm(n)` passes the first `n` words in, in order.
static REGPARM_1_ARGS: [PhysReg; 1] = [EAX];
static REGPARM_2_ARGS: [PhysReg; 2] = [EAX, EDX];
static REGPARM_3_ARGS: [PhysReg; 3] = [EAX, EDX, ECX];

/// [`SYSV`] with the first `n` words of arguments in registers, which is gcc's `regparm(n)`, with
/// the conventions a function of it may call. The first word is the plain convention the family
/// is built on and the second the descriptions of its `regparm` counts, so that the same families
/// are written once for `-fpcc-struct-return`, the default, and once for `-freg-struct-return`.
macro_rules! regparm {
    ($base:ident, 0, $conventions:ident) => {
        CallRegs { conventions: Conventions(&$conventions), ..$base }
    };
    ($base:ident, $abis:ident, $n:literal, $args:ident, $conventions:ident) => {
        CallRegs {
            abi: &rucc_abi::abis::$abis[$n - 1],
            int_args: &$args,
            conventions: Conventions(&$conventions),
            ..$base
        }
    };
}

/// The four conventions `regparm` names, for a unit whose own is the one `-mregparm=` gave it.
///
/// One family for each default, because what a function means by the target's own convention is
/// the unit's default, and a `regparm(0)` function in a unit built with `-mregparm=3` calls an
/// ordinary function of that unit with three registers. The default's own list has no entry for
/// [`Convention::Target`], so that it is the set the command line adjusted rather than the one
/// written here; every other member's list leads back to it.
///
/// `stdcall` and `fastcall` are in every family too, since gcc keeps both outside Windows. The
/// first is the unit's own count of registers with the callee popping and the second names its own
/// two registers, so each family has one of each, and both lead back to the family's list.
macro_rules! family {
    ($regs:ident, $own:ident, $foreign:ident, $default:expr, $stdcall:ident, $fastcall:ident) => {
        static $own: [(Convention, &CallRegs); 6] = [
            (Convention::Regparm(0), &$regs[0]),
            (Convention::Regparm(1), &$regs[1]),
            (Convention::Regparm(2), &$regs[2]),
            (Convention::Regparm(3), &$regs[3]),
            (Convention::Stdcall, &$stdcall),
            (Convention::Fastcall, &$fastcall),
        ];
        static $foreign: [(Convention, &CallRegs); 7] = [
            (Convention::Target, $default),
            (Convention::Regparm(0), &$regs[0]),
            (Convention::Regparm(1), &$regs[1]),
            (Convention::Regparm(2), &$regs[2]),
            (Convention::Regparm(3), &$regs[3]),
            (Convention::Stdcall, &$stdcall),
            (Convention::Fastcall, &$fastcall),
        ];
    };
}

/// A member of a family whose callee pops, with the description and argument registers given and
/// the family's list of conventions, which is what a call it makes is looked up in.
macro_rules! pops {
    ($base:ident, $abi:expr, $args:ident, $conventions:ident) => {
        CallRegs { abi: &$abi, int_args: &$args, conventions: Conventions(&$conventions), ..$base }
    };
}

/// The family of a unit with no `-mregparm=`, whose own convention is [`SYSV`].
static REGPARM_0: [&CallRegs; 4] = [&SYSV, &REGPARM_0_1, &REGPARM_0_2, &REGPARM_0_3];
static REGPARM_0_1: CallRegs = regparm!(SYSV, I386_SYSV_REGPARM, 1, REGPARM_1_ARGS, FOREIGN_0);
static REGPARM_0_2: CallRegs = regparm!(SYSV, I386_SYSV_REGPARM, 2, REGPARM_2_ARGS, FOREIGN_0);
static REGPARM_0_3: CallRegs = regparm!(SYSV, I386_SYSV_REGPARM, 3, REGPARM_3_ARGS, FOREIGN_0);
static REGPARM_0_STDCALL: CallRegs =
    pops!(SYSV, rucc_abi::abis::I386_SYSV_STDCALL, SYSV_INT_ARGS, FOREIGN_0);
static REGPARM_0_FASTCALL: CallRegs =
    pops!(SYSV, rucc_abi::abis::I386_SYSV_FASTCALL, FASTCALL_INT_ARGS, FOREIGN_0);
family!(REGPARM_0, OWN_0, FOREIGN_0, &SYSV, REGPARM_0_STDCALL, REGPARM_0_FASTCALL);

/// The family of a unit built with `-mregparm=1`.
static REGPARM_1: [&CallRegs; 4] = [&REGPARM_1_0, &REGPARM_1_1, &REGPARM_1_2, &REGPARM_1_3];
static REGPARM_1_0: CallRegs = regparm!(SYSV, 0, FOREIGN_1);
static REGPARM_1_1: CallRegs = regparm!(SYSV, I386_SYSV_REGPARM, 1, REGPARM_1_ARGS, OWN_1);
static REGPARM_1_2: CallRegs = regparm!(SYSV, I386_SYSV_REGPARM, 2, REGPARM_2_ARGS, FOREIGN_1);
static REGPARM_1_3: CallRegs = regparm!(SYSV, I386_SYSV_REGPARM, 3, REGPARM_3_ARGS, FOREIGN_1);
static REGPARM_1_STDCALL: CallRegs =
    pops!(SYSV, rucc_abi::abis::I386_SYSV_REGPARM_STDCALL[0], REGPARM_1_ARGS, FOREIGN_1);
static REGPARM_1_FASTCALL: CallRegs =
    pops!(SYSV, rucc_abi::abis::I386_SYSV_FASTCALL, FASTCALL_INT_ARGS, FOREIGN_1);
family!(REGPARM_1, OWN_1, FOREIGN_1, &REGPARM_1_1, REGPARM_1_STDCALL, REGPARM_1_FASTCALL);

/// The family of a unit built with `-mregparm=2`.
static REGPARM_2: [&CallRegs; 4] = [&REGPARM_2_0, &REGPARM_2_1, &REGPARM_2_2, &REGPARM_2_3];
static REGPARM_2_0: CallRegs = regparm!(SYSV, 0, FOREIGN_2);
static REGPARM_2_1: CallRegs = regparm!(SYSV, I386_SYSV_REGPARM, 1, REGPARM_1_ARGS, FOREIGN_2);
static REGPARM_2_2: CallRegs = regparm!(SYSV, I386_SYSV_REGPARM, 2, REGPARM_2_ARGS, OWN_2);
static REGPARM_2_3: CallRegs = regparm!(SYSV, I386_SYSV_REGPARM, 3, REGPARM_3_ARGS, FOREIGN_2);
static REGPARM_2_STDCALL: CallRegs =
    pops!(SYSV, rucc_abi::abis::I386_SYSV_REGPARM_STDCALL[1], REGPARM_2_ARGS, FOREIGN_2);
static REGPARM_2_FASTCALL: CallRegs =
    pops!(SYSV, rucc_abi::abis::I386_SYSV_FASTCALL, FASTCALL_INT_ARGS, FOREIGN_2);
family!(REGPARM_2, OWN_2, FOREIGN_2, &REGPARM_2_2, REGPARM_2_STDCALL, REGPARM_2_FASTCALL);

/// The family of a unit built with `-mregparm=3`, which is every 32 bit x86 Linux kernel.
static REGPARM_3: [&CallRegs; 4] = [&REGPARM_3_0, &REGPARM_3_1, &REGPARM_3_2, &REGPARM_3_3];
static REGPARM_3_0: CallRegs = regparm!(SYSV, 0, FOREIGN_3);
static REGPARM_3_1: CallRegs = regparm!(SYSV, I386_SYSV_REGPARM, 1, REGPARM_1_ARGS, FOREIGN_3);
static REGPARM_3_2: CallRegs = regparm!(SYSV, I386_SYSV_REGPARM, 2, REGPARM_2_ARGS, FOREIGN_3);
static REGPARM_3_3: CallRegs = regparm!(SYSV, I386_SYSV_REGPARM, 3, REGPARM_3_ARGS, OWN_3);
static REGPARM_3_STDCALL: CallRegs =
    pops!(SYSV, rucc_abi::abis::I386_SYSV_REGPARM_STDCALL[2], REGPARM_3_ARGS, FOREIGN_3);
static REGPARM_3_FASTCALL: CallRegs =
    pops!(SYSV, rucc_abi::abis::I386_SYSV_FASTCALL, FASTCALL_INT_ARGS, FOREIGN_3);
family!(REGPARM_3, OWN_3, FOREIGN_3, &REGPARM_3_3, REGPARM_3_STDCALL, REGPARM_3_FASTCALL);

/// [`SYSV`] under `-freg-struct-return`, which returns a structure of one, two, four or eight bytes
/// in registers. See [`rucc_abi::abis::I386_SYSV_REG_STRUCT`].
pub static SYSV_REG: CallRegs = CallRegs {
    abi: &rucc_abi::abis::I386_SYSV_REG_STRUCT,
    conventions: Conventions(&REG_OWN_0),
    ..SYSV
};

/// Under `-freg-struct-return`, the family of a unit with no `-mregparm=`, whose own convention is [`SYSV_REG`].
static REG_STRUCT_0: [&CallRegs; 4] =
    [&SYSV_REG, &REG_STRUCT_0_1, &REG_STRUCT_0_2, &REG_STRUCT_0_3];
static REG_STRUCT_0_1: CallRegs =
    regparm!(SYSV_REG, I386_SYSV_REGPARM_REG_STRUCT, 1, REGPARM_1_ARGS, REG_FOREIGN_0);
static REG_STRUCT_0_2: CallRegs =
    regparm!(SYSV_REG, I386_SYSV_REGPARM_REG_STRUCT, 2, REGPARM_2_ARGS, REG_FOREIGN_0);
static REG_STRUCT_0_3: CallRegs =
    regparm!(SYSV_REG, I386_SYSV_REGPARM_REG_STRUCT, 3, REGPARM_3_ARGS, REG_FOREIGN_0);
static REG_STRUCT_0_STDCALL: CallRegs =
    pops!(SYSV_REG, rucc_abi::abis::I386_SYSV_REG_STRUCT_STDCALL, SYSV_INT_ARGS, REG_FOREIGN_0);
static REG_STRUCT_0_FASTCALL: CallRegs = pops!(
    SYSV_REG,
    rucc_abi::abis::I386_SYSV_REG_STRUCT_FASTCALL,
    FASTCALL_INT_ARGS,
    REG_FOREIGN_0
);
family!(
    REG_STRUCT_0,
    REG_OWN_0,
    REG_FOREIGN_0,
    &SYSV_REG,
    REG_STRUCT_0_STDCALL,
    REG_STRUCT_0_FASTCALL
);

/// Under `-freg-struct-return`, the family of a unit built with `-mregparm=1`.
static REG_STRUCT_1: [&CallRegs; 4] =
    [&REG_STRUCT_1_0, &REG_STRUCT_1_1, &REG_STRUCT_1_2, &REG_STRUCT_1_3];
static REG_STRUCT_1_0: CallRegs = regparm!(SYSV_REG, 0, REG_FOREIGN_1);
static REG_STRUCT_1_1: CallRegs =
    regparm!(SYSV_REG, I386_SYSV_REGPARM_REG_STRUCT, 1, REGPARM_1_ARGS, REG_OWN_1);
static REG_STRUCT_1_2: CallRegs =
    regparm!(SYSV_REG, I386_SYSV_REGPARM_REG_STRUCT, 2, REGPARM_2_ARGS, REG_FOREIGN_1);
static REG_STRUCT_1_3: CallRegs =
    regparm!(SYSV_REG, I386_SYSV_REGPARM_REG_STRUCT, 3, REGPARM_3_ARGS, REG_FOREIGN_1);
static REG_STRUCT_1_STDCALL: CallRegs = pops!(
    SYSV_REG,
    rucc_abi::abis::I386_SYSV_REGPARM_REG_STRUCT_STDCALL[0],
    REGPARM_1_ARGS,
    REG_FOREIGN_1
);
static REG_STRUCT_1_FASTCALL: CallRegs = pops!(
    SYSV_REG,
    rucc_abi::abis::I386_SYSV_REG_STRUCT_FASTCALL,
    FASTCALL_INT_ARGS,
    REG_FOREIGN_1
);
family!(
    REG_STRUCT_1,
    REG_OWN_1,
    REG_FOREIGN_1,
    &REG_STRUCT_1_1,
    REG_STRUCT_1_STDCALL,
    REG_STRUCT_1_FASTCALL
);

/// Under `-freg-struct-return`, the family of a unit built with `-mregparm=2`.
static REG_STRUCT_2: [&CallRegs; 4] =
    [&REG_STRUCT_2_0, &REG_STRUCT_2_1, &REG_STRUCT_2_2, &REG_STRUCT_2_3];
static REG_STRUCT_2_0: CallRegs = regparm!(SYSV_REG, 0, REG_FOREIGN_2);
static REG_STRUCT_2_1: CallRegs =
    regparm!(SYSV_REG, I386_SYSV_REGPARM_REG_STRUCT, 1, REGPARM_1_ARGS, REG_FOREIGN_2);
static REG_STRUCT_2_2: CallRegs =
    regparm!(SYSV_REG, I386_SYSV_REGPARM_REG_STRUCT, 2, REGPARM_2_ARGS, REG_OWN_2);
static REG_STRUCT_2_3: CallRegs =
    regparm!(SYSV_REG, I386_SYSV_REGPARM_REG_STRUCT, 3, REGPARM_3_ARGS, REG_FOREIGN_2);
static REG_STRUCT_2_STDCALL: CallRegs = pops!(
    SYSV_REG,
    rucc_abi::abis::I386_SYSV_REGPARM_REG_STRUCT_STDCALL[1],
    REGPARM_2_ARGS,
    REG_FOREIGN_2
);
static REG_STRUCT_2_FASTCALL: CallRegs = pops!(
    SYSV_REG,
    rucc_abi::abis::I386_SYSV_REG_STRUCT_FASTCALL,
    FASTCALL_INT_ARGS,
    REG_FOREIGN_2
);
family!(
    REG_STRUCT_2,
    REG_OWN_2,
    REG_FOREIGN_2,
    &REG_STRUCT_2_2,
    REG_STRUCT_2_STDCALL,
    REG_STRUCT_2_FASTCALL
);

/// Under `-freg-struct-return`, the family of a unit built with `-mregparm=3`, which is every 32 bit x86 Linux kernel.
static REG_STRUCT_3: [&CallRegs; 4] =
    [&REG_STRUCT_3_0, &REG_STRUCT_3_1, &REG_STRUCT_3_2, &REG_STRUCT_3_3];
static REG_STRUCT_3_0: CallRegs = regparm!(SYSV_REG, 0, REG_FOREIGN_3);
static REG_STRUCT_3_1: CallRegs =
    regparm!(SYSV_REG, I386_SYSV_REGPARM_REG_STRUCT, 1, REGPARM_1_ARGS, REG_FOREIGN_3);
static REG_STRUCT_3_2: CallRegs =
    regparm!(SYSV_REG, I386_SYSV_REGPARM_REG_STRUCT, 2, REGPARM_2_ARGS, REG_FOREIGN_3);
static REG_STRUCT_3_3: CallRegs =
    regparm!(SYSV_REG, I386_SYSV_REGPARM_REG_STRUCT, 3, REGPARM_3_ARGS, REG_OWN_3);
static REG_STRUCT_3_STDCALL: CallRegs = pops!(
    SYSV_REG,
    rucc_abi::abis::I386_SYSV_REGPARM_REG_STRUCT_STDCALL[2],
    REGPARM_3_ARGS,
    REG_FOREIGN_3
);
static REG_STRUCT_3_FASTCALL: CallRegs = pops!(
    SYSV_REG,
    rucc_abi::abis::I386_SYSV_REG_STRUCT_FASTCALL,
    FASTCALL_INT_ARGS,
    REG_FOREIGN_3
);
family!(
    REG_STRUCT_3,
    REG_OWN_3,
    REG_FOREIGN_3,
    &REG_STRUCT_3_3,
    REG_STRUCT_3_STDCALL,
    REG_STRUCT_3_FASTCALL
);

/// The convention of a unit on i386 System V built with `-mregparm=registers`, and with
/// `-freg-struct-return` when `struct_in_registers` is set, which is [`SYSV`] or [`SYSV_REG`]
/// for none and [`None`] for more than three, which gcc refuses.
#[must_use]
pub fn regparm(registers: u8, struct_in_registers: bool) -> Option<&'static CallRegs> {
    let family = match (registers, struct_in_registers) {
        (0, false) => &REGPARM_0,
        (1, false) => &REGPARM_1,
        (2, false) => &REGPARM_2,
        (3, false) => &REGPARM_3,
        (0, true) => &REG_STRUCT_0,
        (1, true) => &REG_STRUCT_1,
        (2, true) => &REG_STRUCT_2,
        (3, true) => &REG_STRUCT_3,
        _ => return None,
    };
    Some(family[usize::from(registers)])
}

/// The same convention in position independent code, where [`GOT_BASE`] is not the allocator's.
///
/// Everything but the allocation order is [`SYSV`]'s. `ebx` is still preserved across a call,
/// which is exactly why the psABI chose it: a function that loaded the table's address keeps it
/// through every call it makes.
pub static SYSV_PIC: CallRegs = CallRegs { int_order: &SYSV_PIC_INT_ORDER, ..SYSV };

/// Where an i686 Windows call puts things under mingw-w64, which is cdecl as gcc writes it for
/// `i686-w64-mingw32`.
///
/// The registers are [`SYSV`]'s. Every argument is in the argument area, a result comes back in
/// `eax`, `edx:eax` or `st0`, `ebx`, `esi`, `edi` and `ebp` survive a call and no vector register
/// does. What differs is written in the ABI half, [`rucc_abi::abis::I386_MINGW`]: a structure of
/// one, two, four or eight bytes comes back in registers, and the caller pops the address of a
/// result returned through memory.
///
/// Sixteen byte alignment at a call is what gcc for this target keeps, and so does this compiler.
/// What a function counts on at entry is less, because Windows only promises four and a callback
/// from a DLL Microsoft's compiler built arrives on four. A frame with a sixteen byte spill or local
/// realigns itself, as gcc's does with SSE on. See [`CallRegs::trusted_align`].
///
/// There is no canary and no profiling hook for the reasons [`crate::x86_64::WIN64`] gives, and a
/// frame larger than a page is reached through mingw's routine for it, which leaves the stack
/// pointer where it was, as it does on x86-64.
pub static MINGW32: CallRegs = CallRegs {
    abi: &rucc_abi::abis::I386_MINGW,
    guard: None,
    trace: None,
    // The C name. It is `___chkstk_ms` in the object, with the underscore every C name gets here,
    // and that is the same symbol libgcc defines for x86-64 where no underscore is added.
    chkstk: Some(Chkstk { name: "__chkstk_ms", size: EAX, shift: 0, moves: false }),
    conventions: Conventions(&MINGW32_CONVENTIONS),
    trusted_align: 8,
    ..SYSV
};

/// The two registers `fastcall` passes the first two small integers in, in order.
static FASTCALL_INT_ARGS: [PhysReg; 2] = [ECX, EDX];

/// A function declared `stdcall` under mingw-w64, which is most of the Windows API.
///
/// [`MINGW32`]'s registers exactly. The difference is the callee taking the argument area off the
/// stack, and that is the ABI half's to say, in [`rucc_abi::abis::I386_MINGW_STDCALL`].
pub static MINGW32_STDCALL: CallRegs =
    CallRegs { abi: &rucc_abi::abis::I386_MINGW_STDCALL, ..MINGW32 };

/// A function declared `fastcall` under mingw-w64: [`MINGW32_STDCALL`] with the first two
/// integers of a word or less in `ecx` and `edx`.
pub static MINGW32_FASTCALL: CallRegs =
    CallRegs { abi: &rucc_abi::abis::I386_MINGW_FASTCALL, int_args: &FASTCALL_INT_ARGS, ..MINGW32 };

/// The conventions a function on i686 Windows under mingw-w64 may have: cdecl, which is its own,
/// and the two whose callee pops.
static MINGW32_CONVENTIONS: [(Convention, &CallRegs); 3] = [
    (Convention::Target, &MINGW32),
    (Convention::Stdcall, &MINGW32_STDCALL),
    (Convention::Fastcall, &MINGW32_FASTCALL),
];

/// Where an i686 Windows call puts things under Microsoft's runtime.
///
/// [`MINGW32`] with Microsoft's ABI, whose one difference is a structure holding a lone `float`
/// or `double`, and Microsoft's routine for a large frame. That routine is `__chkstk` in the object
/// and it is not mingw's with another name: on this machine it moves the stack pointer down by the
/// size itself, so the frame is taken by the call and nothing is subtracted after it.
pub static MSVC32: CallRegs = CallRegs {
    abi: &rucc_abi::abis::I386_MSVC,
    chkstk: Some(Chkstk { name: "_chkstk", size: EAX, shift: 0, moves: true }),
    conventions: Conventions(&MSVC32_CONVENTIONS),
    ..MINGW32
};

/// A function declared `stdcall` under Microsoft's runtime.
pub static MSVC32_STDCALL: CallRegs =
    CallRegs { abi: &rucc_abi::abis::I386_MSVC_STDCALL, ..MSVC32 };

/// A function declared `fastcall` under Microsoft's runtime.
pub static MSVC32_FASTCALL: CallRegs =
    CallRegs { abi: &rucc_abi::abis::I386_MSVC_FASTCALL, int_args: &FASTCALL_INT_ARGS, ..MSVC32 };

/// [`MINGW32_CONVENTIONS`] under Microsoft's runtime.
static MSVC32_CONVENTIONS: [(Convention, &CallRegs); 3] = [
    (Convention::Target, &MSVC32),
    (Convention::Stdcall, &MSVC32_STDCALL),
    (Convention::Fastcall, &MSVC32_FASTCALL),
];

// Aligned vector moves for the reason `crate::x86_64` gives. The frame keeps its sixteen byte
// alignment here too, which [`SYSV`] says the psABI promises.
static X86_MOVES: [ClassMoves; 2] = [
    ClassMoves { mov: "mov_rr_32", load: "mov_rm_32", store: "mov_mr_32", narrow: &[] },
    ClassMoves { mov: "movaps_rr", load: "movaps_rm", store: "movaps_mr", narrow: &[] },
];

/// What an i386 prologue, epilogue, spill and reload are made of.
///
/// [`crate::x86_64::FRAME`] at thirty two bits: a register and an address are both four bytes, so
/// every instruction that moves one, adjusts the stack pointer or computes an address is the `l`
/// form rather than the `q` one. The probe is the same instruction, since it touches a byte.
pub static FRAME: FrameInsts = FrameInsts {
    prefix: "x64.",
    classes: &X86_MOVES,
    push: "push_32",
    pop: "pop_32",
    pair: None,
    kept: None,
    add: "add_ri_32",
    sub: "sub_ri_32",
    grow: "sub_rr_32",
    scaled: None,
    insert: None,
    align: "and_ri_32",
    imm: "mov_ri_32",
    lea: "lea_32",
    sum: "add_rr_32",
    ret: "ret",
    ret_pop: Some("ret_pop"),
    iret: None,
    clear_direction: None,
    differ: "cmp_set_ne_32",
    differs_from: Some("cmp_set_ne_rm_32"),
    not_below: "cmp_set_ae_32",
    away: Some("jmp_away"),
    call: "call",
    probe: Some(crate::x86_64::PROBE),
    landing: Some("endbr32"),
    signing: None,
    canary: None,
    targets: None,
    pad: Some("nop"),
    step_bits: None,
    reaches: None,
    thunks: Some(THUNKS),
    // The load in front of a call here is the thirty two bit one, which the fold does not know.
    through: None,
};

/// What the speculation hardening flags rewrite branches into on i386.
///
/// The names are x86-64's with the thirty two bit register on the end, which is what the kernel's
/// `arch/x86/lib/retpoline.S` defines when it is built for this machine. No register takes a REX
/// byte here, so none of the calls needs the override that pads one.
pub static THUNKS: Thunks = Thunks { regs: &GPR_NAMES, padded_from: 8, ..crate::x86_64::THUNKS };

/// What an i386 instruction has to look like for this machine to have one.
///
/// [`crate::x86_64::MACHINE`] with one more question: whether every instruction the opcode writes
/// is one the encoder has in [`Mode::Bits32`]. That is what takes out the sixty four bit forms,
/// whose `q` suffix is a REX.W byte this machine does not have, and `pushq` and `popq`, which have
/// no thirty two bit encoding at all. The answer is the encoder's rather than a list kept here, so
/// an opcode added to the description is on this machine exactly when it can be written for it.
///
/// The addressing modes are the same four scales with an index and a displacement together.
pub static MACHINE: MachineInsts = MachineInsts {
    prefix: "x64.",
    operands: machine_operands,
    takes_imm: machine_takes_imm,
    takes_mem: machine_takes_mem,
    touches_mem: machine_touches_mem,
    calls: machine_calls,
    commutes: machine_commutes,
    scales: &[1, 2, 4, 8],
    index_and_disp: true,
};

/// The counts i386 has an instruction for, which are x86-64's at thirty two bits.
///
/// A sixty four bit count is two words here, so it has no instruction of its own and is written
/// out. The searches are guarded for the reason [`crate::x86_64::COUNTS`] gives, and every
/// processor this target builds for from the i686 on has the conditional move they end in.
pub const COUNTS: &[CountInst] = &[
    CountInst {
        of: BitCount::Ones,
        feature: "popcnt",
        widths: &[32],
        guarded: false,
        vector: false,
    },
    CountInst {
        of: BitCount::LeadingZeros,
        feature: "lzcnt",
        widths: &[32],
        guarded: false,
        vector: false,
    },
    CountInst {
        of: BitCount::TrailingZeros,
        feature: "bmi",
        widths: &[32],
        guarded: false,
        vector: false,
    },
    CountInst {
        of: BitCount::LeadingZeros,
        feature: "",
        widths: &[32],
        guarded: true,
        vector: false,
    },
    CountInst {
        of: BitCount::TrailingZeros,
        feature: "",
        widths: &[32],
        guarded: true,
        vector: false,
    },
];

/// The form of an opcode this machine has, or `None` if it has no such opcode or cannot write it.
#[must_use]
pub fn form_here(name: &str) -> Option<Form> {
    let found = form(name)?;
    let insts = written(name)?;
    insts
        .iter()
        .all(|inst| {
            let args: Vec<Kind> = inst.args.iter().map(|&arg| Kind::of(arg)).collect();
            // A row with the REX wide bit is found in either mode and refused only when it is
            // written, so it is refused here too.
            encoding_in(Mode::Bits32, inst.mnemonic, &args, 0).is_some_and(|row| !row.size.wide())
        })
        .then_some(found)
}

#[must_use]
fn machine_operands(name: &str) -> Option<&'static [OperandDesc]> {
    form_here(name).map(Form::operands)
}

#[must_use]
fn machine_takes_imm(name: &str) -> bool {
    form_here(name).is_some_and(Form::takes_imm)
}

#[must_use]
fn machine_takes_mem(name: &str) -> bool {
    form_here(name).is_some_and(Form::takes_mem)
}

#[must_use]
fn machine_touches_mem(name: &str) -> bool {
    form_here(name).is_some_and(Form::touches_mem)
}

#[must_use]
fn machine_calls(name: &str) -> bool {
    matches!(form_here(name), Some(Form::Call | Form::CallMem))
}

#[must_use]
fn machine_commutes(name: &str) -> bool {
    form_here(name).is_some() && (crate::x86_64::MACHINE.commutes)(name)
}

/// What an i386 conditional branch becomes once the blocks are in an order.
///
/// [`crate::x86_64::BRANCH`] without the sixty four bit comparisons and selects, which this
/// machine does not have. The test, the jumps and the conditions are the same instructions.
pub static BRANCH: BranchInsts =
    BranchInsts { fused: &FUSED, moves: &MOVES, ..crate::x86_64::BRANCH };

/// Whether an opcode's name ends in the width this machine has no registers of.
const fn is_wide(name: &str) -> bool {
    let bytes = name.as_bytes();
    let n = bytes.len();
    n >= 3 && bytes[n - 3] == b'_' && bytes[n - 2] == b'6' && bytes[n - 1] == b'4'
}

/// The x86-64 fusions whose comparison is at eight, sixteen or thirty two bits.
const fn narrow_fused<const N: usize>(all: &[Fusion]) -> [Fusion; N] {
    let mut out = [all[0]; N];
    let (mut at, mut kept) = (0, 0);
    while at < all.len() {
        if !is_wide(all[at].set) {
            out[kept] = all[at];
            kept += 1;
        }
        at += 1;
    }
    assert!(kept == N, "the count of narrow comparisons");
    out
}

/// The x86-64 moves whose select is at eight, sixteen or thirty two bits.
const fn narrow_moves<const N: usize>(all: &[Move]) -> [Move; N] {
    let mut out = [all[0]; N];
    let (mut at, mut kept) = (0, 0);
    while at < all.len() {
        if !is_wide(all[at].select) {
            out[kept] = all[at];
            kept += 1;
        }
        at += 1;
    }
    assert!(kept == N, "the count of narrow selects");
    out
}

/// Ten conditions at three widths, against a register, a constant and memory, and the test of a
/// register and of memory against a constant for equal and not equal at the same three.
static FUSED: [Fusion; 132] = narrow_fused(&crate::x86_64::FUSED);

/// Ten conditions at the three widths a select has here.
static MOVES: [Move; 30] = narrow_moves(&crate::x86_64::MOVES);

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
    fn only_an_abi_whose_callee_pops_the_return_address_slot_says_so() {
        assert_eq!(SYSV.return_pointer_popped(None), 4);
        let callers_pop = CallRegs { abi: &rucc_abi::abis::WIN64, ..SYSV };
        assert_eq!(callers_pop.return_pointer_popped(None), 0, "a plain ret where the caller pops");
    }

    /// `callee_pop_aggregate_return` turns either 32-bit answer into the other, and changes
    /// nothing where the address is in `eax` or the callee pops everything anyway.
    #[test]
    fn an_attribute_says_who_pops_the_return_address_slot() {
        assert_eq!(SYSV.return_pointer_popped(Some(false)), 0);
        assert_eq!(SYSV.return_pointer_popped(Some(true)), 4);
        for windows in [&MINGW32, &MSVC32] {
            assert_eq!(windows.return_pointer_popped(Some(true)), 4, "{}", windows.abi.name);
            assert_eq!(windows.return_pointer_popped(Some(false)), 0, "{}", windows.abi.name);
        }
        let registers = regparm(3, false).expect("a family");
        let regparm = registers.under(Convention::Regparm(3)).expect("every count");
        assert_eq!(regparm.return_pointer_popped(Some(true)), 0, "the address is in eax");
        let none = registers.under(Convention::Regparm(0)).expect("every count");
        assert_eq!(none.return_pointer_popped(Some(true)), 4);
        assert_eq!(MINGW32_STDCALL.return_pointer_popped(Some(true)), 0, "popped with the rest");
    }

    #[test]
    fn windows_is_cdecl_over_the_same_registers_with_its_own_abi_and_probe() {
        for (regs, abi) in
            [(&MINGW32, &rucc_abi::abis::I386_MINGW), (&MSVC32, &rucc_abi::abis::I386_MSVC)]
        {
            assert!(std::ptr::eq(regs.abi, abi), "{}", abi.name);
            assert_eq!(regs.return_pointer_popped(None), 0, "{}: the caller pops it", abi.name);
            assert!(regs.int_args.is_empty() && regs.sse_args.is_empty(), "{}", abi.name);
            assert_eq!(regs.int_returns, SYSV.int_returns, "{}", abi.name);
            assert_eq!(regs.x87_returns, SYSV.x87_returns, "{}", abi.name);
            assert_eq!(regs.int_saved, SYSV.int_saved, "{}", abi.name);
            assert_eq!(regs.stack_align, 16, "{}", abi.name);
            assert_eq!(regs.trusted_align, 8, "{}: Windows promises four", abi.name);
            assert!(regs.guard.is_none() && regs.trace.is_none(), "{}", abi.name);
        }
        let mingw = MINGW32.chkstk.expect("mingw's routine");
        assert_eq!((mingw.name, mingw.size, mingw.moves), ("__chkstk_ms", EAX, false));
        let msvc = MSVC32.chkstk.expect("Microsoft's routine");
        assert_eq!((msvc.name, msvc.size, msvc.moves), ("_chkstk", EAX, true));
    }

    #[test]
    fn a_stack_boundary_moves_what_a_function_counts_on_only_where_it_matched() {
        assert_eq!(SYSV.aligned_to(32).trusted_align, 32, "a caller is held to the new boundary");
        assert_eq!(SYSV.aligned_to(4).trusted_align, 4);
        for regs in [&MINGW32, &MINGW32_STDCALL, &MINGW32_FASTCALL, &MSVC32, &MSVC32_STDCALL] {
            assert_eq!(regs.trusted_align, 8, "{}", regs.abi.name);
            assert_eq!(
                regs.aligned_to(32).trusted_align,
                8,
                "{}: Windows still promises four",
                regs.abi.name
            );
            assert_eq!(regs.aligned_to(4).trusted_align, 4, "{}", regs.abi.name);
        }
    }

    #[test]
    fn windows_has_stdcall_and_fastcall_and_only_fastcall_has_registers() {
        for (regs, stdcall, fastcall) in [
            (&MINGW32, &MINGW32_STDCALL, &MINGW32_FASTCALL),
            (&MSVC32, &MSVC32_STDCALL, &MSVC32_FASTCALL),
        ] {
            assert!(std::ptr::eq(regs.under(Convention::Target).expect("cdecl"), regs));
            let under_stdcall = regs.under(Convention::Stdcall).expect("stdcall");
            let under_fastcall = regs.under(Convention::Fastcall).expect("fastcall");
            assert!(std::ptr::eq(under_stdcall, stdcall));
            assert!(std::ptr::eq(under_fastcall, fastcall));
            // A function of one convention calling one of another finds the same list from there.
            assert!(std::ptr::eq(stdcall.under(Convention::Target).expect("cdecl"), regs));
            assert!(stdcall.int_args.is_empty());
            assert_eq!(fastcall.int_args, [ECX, EDX]);
            assert_eq!(stdcall.abi.cleanup, rucc_abi::Cleanup::Callee);
            assert_eq!(fastcall.abi.cleanup, rucc_abi::Cleanup::Callee);
            assert_eq!(regs.abi.cleanup, rucc_abi::Cleanup::Caller);
            assert_eq!(fastcall.chkstk, regs.chkstk);
        }
    }

    #[test]
    fn linux_has_stdcall_and_fastcall_too_and_stdcall_keeps_the_units_registers() {
        // gcc keeps both conventions outside Windows. `stdcall` there is whatever the unit's own
        // convention is with the callee popping, so under `-mregparm=3` it has three registers,
        // and `fastcall` has ecx and edx whatever the unit says. Measured with i686-linux-gnu-gcc 13.
        for reg_struct in [false, true] {
            for own in 0..=3u8 {
                let unit = regparm(own, reg_struct).expect("0 to 3 are counts");
                let stdcall = unit.under(Convention::Stdcall).expect("stdcall");
                let fastcall = unit.under(Convention::Fastcall).expect("fastcall");
                assert_eq!(stdcall.int_args, unit.int_args, "regparm {own}");
                assert_eq!(fastcall.int_args, [ECX, EDX]);
                assert_eq!(stdcall.abi.cleanup, rucc_abi::Cleanup::Callee);
                assert_eq!(fastcall.abi.cleanup, rucc_abi::Cleanup::Callee);
                assert_eq!(unit.abi.cleanup, rucc_abi::Cleanup::Caller);
                // The return rule is the unit's, which is what `-freg-struct-return` changes.
                assert_eq!(stdcall.abi.returns, unit.abi.returns);
                assert_eq!(fastcall.abi.returns, unit.abi.returns);
                // And a call either of them makes is planned in the unit's family.
                for from in [stdcall, fastcall] {
                    assert!(std::ptr::eq(from.under(Convention::Target).expect("own"), unit));
                    assert!(std::ptr::eq(from.under(Convention::Stdcall).expect("again"), stdcall));
                }
            }
        }
    }

    #[test]
    fn each_regparm_count_finds_every_other_from_where_it_is() {
        for registers in [false, true] {
            for own in 0..=3u8 {
                let unit = regparm(own, registers).expect("0 to 3 are counts");
                assert_eq!(unit.int_args.len(), usize::from(own));
                assert!(std::ptr::eq(unit.under(Convention::Target).expect("its own"), unit));
                for other in (0..=3u8).filter(|&other| other != own) {
                    let there = unit.under(Convention::Regparm(other)).expect("every count");
                    assert_eq!(there.int_args, &[EAX, EDX, ECX][..usize::from(other)]);
                    // A function of another count returns a structure the way the unit does.
                    assert_eq!(there.abi.returns.len(), unit.abi.returns.len());
                    // And a call back to an ordinary function from there is the unit's own again.
                    assert!(std::ptr::eq(there.under(Convention::Target).expect("own"), unit));
                }
            }
        }
        assert!(std::ptr::eq(regparm(0, false).expect("none"), &SYSV));
        assert!(std::ptr::eq(regparm(0, true).expect("none"), &SYSV_REG));
        assert!(regparm(4, false).is_none());
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

    /// Every instruction a prologue, an epilogue, a spill or a branch is made of is one this
    /// machine has. One that was not would compile under `-S` and fail to assemble, or worse,
    /// assemble as the sixty four bit form and do something else.
    #[test]
    fn every_opcode_the_frame_and_the_branches_name_is_one_this_machine_has() {
        let mut names = vec![
            FRAME.push,
            FRAME.pop,
            FRAME.add,
            FRAME.sub,
            FRAME.grow,
            FRAME.align,
            FRAME.imm,
            FRAME.lea,
            FRAME.sum,
            FRAME.ret,
            FRAME.differ,
            FRAME.not_below,
            FRAME.call,
        ];
        names.extend(FRAME.away);
        names.extend(FRAME.landing);
        names.extend(FRAME.pad);
        names.extend(FRAME.probe.map(|probe| probe.inst));
        for class in FRAME.classes {
            names.extend([class.mov, class.load, class.store]);
        }
        let thunks = FRAME.thunks.expect("the frame has thunks");
        names.extend([
            thunks.call_through,
            thunks.jump_through,
            thunks.call,
            thunks.jump,
            thunks.trap,
        ]);
        names.extend([BRANCH.cond, BRANCH.test, BRANCH.if_true, BRANCH.if_false]);
        names.extend([BRANCH.jump, BRANCH.indirect]);
        names.extend(BRANCH.conditional);
        for fusion in BRANCH.fused {
            names.extend([fusion.set, fusion.cmp, fusion.if_true, fusion.if_false]);
        }
        for entry in BRANCH.moves {
            names.extend([entry.select, entry.when, entry.cmov]);
        }
        for name in names {
            assert!(form_here(name).is_some(), "{name} is not an i386 instruction");
        }
    }

    /// The sixty four bit forms are x86-64's alone, and the thirty two bit frame moves are not.
    #[test]
    fn the_machine_has_the_thirty_two_bit_forms_and_not_the_sixty_four_bit_ones() {
        for name in ["add_rr_64", "mov_rr_64", "mov_ri_64", "lea_64", "push_64", "pop_64"] {
            assert_eq!(form_here(name), None, "{name}");
            assert_eq!((MACHINE.operands)(name), None, "{name}");
        }
        for name in ["add_rr_32", "mov_rr_32", "mov_ri_32", "lea_32", "push_32", "pop_32"] {
            assert!(form_here(name).is_some(), "{name}");
        }
        assert!((MACHINE.commutes)("add_rr_32"));
        assert!(!(MACHINE.commutes)("add_rr_64"));
        assert!(!(MACHINE.commutes)("sub_rr_32"));
        assert!((MACHINE.calls)("call"));
    }

    /// Every comparison this machine has can have its test taken off, and every select on one has
    /// the move for every condition, which is the x86-64 table's claim held to what is left of it.
    #[test]
    fn the_branch_tables_are_x86_64_s_at_the_widths_this_machine_has() {
        let here = |shapes: &[Form]| -> Vec<&str> {
            crate::x86_64::INSTS
                .iter()
                .filter(|&&(name, shape)| shapes.contains(&shape) && form_here(name).is_some())
                .map(|&(name, _)| name)
                .collect()
        };
        let sets = here(&[Form::CmpSet, Form::CmpSetRi, Form::CmpSetRm, Form::CmpSetMi]);
        let entries: Vec<&str> = BRANCH.fused.iter().map(|fusion| fusion.set).collect();
        assert_eq!(entries, sets);
        let selects = here(&[Form::TestCmov]);
        assert_eq!(BRANCH.moves.len(), selects.len() * 10);
        for entry in BRANCH.moves {
            assert!(selects.contains(&entry.select), "{}", entry.select);
        }
    }
}
