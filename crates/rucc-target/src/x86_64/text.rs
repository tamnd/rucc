//! What each x86-64 machine instruction is in assembly.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.1.
//!
//! The compiler writes bytes rather than text, and `-S` writes text because people read it. Both
//! read this, which is what section 11.1 asks for: an assembly listing that disagrees with the
//! object file next to it is worse than no listing at all, and the only way to be sure they agree
//! is for there to be one description and not two.
//!
//! An opcode in the machine IR is one instruction to the allocator and is not always one
//! instruction to the machine, which [`Form`](crate::x86_64::Form) already says. So what is
//! written for an opcode is a list of them: a comparison is a `cmp` and a `set`, an unsigned
//! division is the clearing of the high half and then the division, and the three opcodes that
//! exist to hold a value in a register until something reads it are written as nothing at all.
//!
//! # The order the arguments are in
//!
//! AT&T syntax, which puts the source before the destination and is the reverse of the order the
//! operand vector holds them in. That is why an argument names the operand it wants by index
//! rather than the arguments being the operand vector: the two orders are different, an
//! instruction may name one operand twice, and an instruction the machine writes in the middle of
//! an opcode may name none of them.
//!
//! Intel syntax is the other order and `spec/11-asm-objects-debug.md` section 11.1 requires it as
//! an input. Writing it is a second pass over this table rather than a second table, since the
//! difference is the order of the arguments and the spelling of a memory operand, and neither is
//! a different instruction.
//!
//! # The width
//!
//! A register is one register at every width and `al`, `ax`, `eax` and `rax` are four ways of
//! writing part of `rax`, which is why the register file names only the last of them. The width
//! belongs to the instruction, so it is written on the argument, and [`gpr_name`] is where the
//! two are put together.
//!
//! It is on the argument rather than on the instruction because the two are not always the same.
//! A comparison of two sixty four bit registers sets a byte, a shift of a sixty four bit register
//! reads its count from a byte, and an eight bit multiply is done thirty two bits at a time
//! because the machine has no two-operand multiply narrower than that and the low eight bits of a
//! product depend on nothing but the low eight bits of what went into it.

use crate::regs::PhysReg;

use Arg::{Imm, Label, Mem, Named, Reg, Symbol, Through};
use Width::{Byte, Long, Quad, Word};

/// How much of a register one argument of one instruction is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Width {
    /// Eight bits, which is `al`.
    Byte,
    /// Sixteen bits, `ax`.
    Word,
    /// Thirty two bits, `eax`.
    Long,
    /// Sixty four bits, `rax`, and the spelling of any register whose class has only one.
    Quad,
}

impl Width {
    /// Which of the four spellings of a register this is, counting from the narrowest.
    #[must_use]
    pub fn index(self) -> usize {
        match self {
            Byte => 0,
            Word => 1,
            Long => 2,
            Quad => 3,
        }
    }
}

/// One argument of one assembly instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arg {
    /// The register of the operand at that index in the operand vector, that much of it.
    Reg(u8, Width),
    /// A register the instruction names itself, which is one no operand could name.
    ///
    /// There is one, which is `ah`. It is the high byte of the remainder an eight bit division
    /// gives back, and it is not an operand because a program that names it cannot also name
    /// `sil` or `r8b` in the same instruction, so an allocator that could put a value there would
    /// have to know which other registers the instruction had been given.
    Named(&'static str),
    /// The immediate the instruction carries.
    Imm,
    /// The addressing mode the instruction carries.
    Mem,
    /// The symbol the instruction names, which is what a call goes to.
    Symbol,
    /// The register a call goes through, which the assembler writes with a star in front of it.
    ///
    /// The star is what tells the two calls apart in AT&T syntax. `call f` goes to the place the
    /// name is at and `call *%rax` goes to the place the register holds, and without it a call
    /// through an address would be written as a call to whatever the register happened to be
    /// called. It is on the argument rather than in the mnemonic because it is a fact about the
    /// argument: it says the operand is where the target is, not that the target is there.
    ///
    /// The first operand the instruction reads, rather than the operand at an index, which is the
    /// one place in this table an argument is named that way. A call writes the register the
    /// value comes back in and every register the callee may destroy before it reads anything,
    /// and how many of those there are depends on the signature and the convention, so no index
    /// written here would be the same from one call to the next. Where the address is put is
    /// `rucc_codegen::abi`'s answer, which is in front of the arguments.
    ///
    /// A whole register, because an address is one. There is no width to write.
    Through,
    /// The block the instruction goes to, which is the first successor of the block it ends.
    Label,
}

/// One instruction of the machine, as an assembler reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Written {
    /// The mnemonic, with the letter that says how wide it is already on it.
    pub mnemonic: &'static str,
    /// Its arguments, in the order they are written.
    pub args: &'static [Arg],
}

/// One instruction of the machine, for the table below.
const fn spell(mnemonic: &'static str, args: &'static [Arg]) -> Written {
    Written { mnemonic, args }
}

// The comparison in front of a set, which reads its second source and then its first. The
// destination is not one of its arguments, because what a comparison writes is the flags and what
// the byte at index zero gets is written by the set behind it.
static CMP_8: [Arg; 2] = [Reg(2, Byte), Reg(1, Byte)];
static CMP_16: [Arg; 2] = [Reg(2, Word), Reg(1, Word)];
static CMP_32: [Arg; 2] = [Reg(2, Long), Reg(1, Long)];
static CMP_64: [Arg; 2] = [Reg(2, Quad), Reg(1, Quad)];
// What the set writes, which is a byte whatever was compared to produce it.
static SET: [Arg; 1] = [Reg(0, Byte)];

// The divisor, which is the one register a division names. Everything else it touches is a fixed
// register the opcode carries as an operand so that the allocator keeps out of it.
static DIVISOR_8: [Arg; 1] = [Reg(3, Byte)];
static DIVISOR_16: [Arg; 1] = [Reg(3, Word)];
static DIVISOR_32: [Arg; 1] = [Reg(3, Long)];
static DIVISOR_64: [Arg; 1] = [Reg(3, Quad)];

// An unsigned division divides a pair of registers by one, so the high half of the pair has to be
// zero before it runs, and a signed one sign extends into it instead, which is what `cltd` and the
// three like it are for. Which operand the high half is depends on which answer the opcode gives
// back: the quotient's opcode carries it as the clobber at index one and the remainder's opcode
// carries it as the destination at index zero, and both are `rdx`.
static CLEAR_FOR_QUOTIENT: [Arg; 2] = [Reg(1, Long), Reg(1, Long)];
static CLEAR_FOR_REMAINDER: [Arg; 2] = [Reg(0, Long), Reg(0, Long)];

// An eight bit division is the one that is not a pair of registers. The dividend is the whole of
// `ax`, the quotient comes back in `al` and the remainder in `ah`, so the dividend is widened into
// `ax` first and the remainder is moved down out of `ah` afterwards. The widening is written here
// only for the unsigned pair, because the signed pair has an instruction of its own for it.
static WIDEN_FOR_QUOTIENT: [Arg; 2] = [Reg(2, Byte), Reg(0, Long)];
static WIDEN_FOR_REMAINDER: [Arg; 2] = [Reg(2, Byte), Reg(1, Long)];
static HIGH_HALF: [Arg; 2] = [Named("ah"), Reg(0, Byte)];

/// Every x86-64 opcode, and the instructions the machine writes for it.
///
/// In the same order as [`INSTS`](crate::x86_64::INSTS) and grouped the same way, because the two
/// are read together and a name that is in one and not the other is a mistake either way round.
///
/// An empty list is an opcode that is not an instruction. Three of them exist to say where a
/// value already is or has to be, which is a fact the allocator needs and the machine does not,
/// and a fourth is the condition a block leaves on, which the block layout takes back out and
/// replaces with a test and a jump.
static TEXT: &[(&str, &[Written])] = &[
    // Constants. The sixty four bit form is written as an ordinary move and the assembler is the
    // one that reaches for the ten byte encoding when the number does not fit in four.
    ("mov_ri_8", &[spell("movb", &[Imm, Reg(0, Byte)])]),
    ("mov_ri_16", &[spell("movw", &[Imm, Reg(0, Word)])]),
    ("mov_ri_32", &[spell("movl", &[Imm, Reg(0, Long)])]),
    ("mov_ri_64", &[spell("movq", &[Imm, Reg(0, Quad)])]),
    // Arithmetic, register with register. The destination is the first source, which the allocator
    // has arranged by the time any of this is written, so the first source is not an argument.
    ("add_rr_8", &[spell("addb", &[Reg(2, Byte), Reg(0, Byte)])]),
    ("add_rr_16", &[spell("addw", &[Reg(2, Word), Reg(0, Word)])]),
    ("add_rr_32", &[spell("addl", &[Reg(2, Long), Reg(0, Long)])]),
    ("add_rr_64", &[spell("addq", &[Reg(2, Quad), Reg(0, Quad)])]),
    ("sub_rr_8", &[spell("subb", &[Reg(2, Byte), Reg(0, Byte)])]),
    ("sub_rr_16", &[spell("subw", &[Reg(2, Word), Reg(0, Word)])]),
    ("sub_rr_32", &[spell("subl", &[Reg(2, Long), Reg(0, Long)])]),
    ("sub_rr_64", &[spell("subq", &[Reg(2, Quad), Reg(0, Quad)])]),
    ("and_rr_8", &[spell("andb", &[Reg(2, Byte), Reg(0, Byte)])]),
    ("and_rr_16", &[spell("andw", &[Reg(2, Word), Reg(0, Word)])]),
    ("and_rr_32", &[spell("andl", &[Reg(2, Long), Reg(0, Long)])]),
    ("and_rr_64", &[spell("andq", &[Reg(2, Quad), Reg(0, Quad)])]),
    ("or_rr_8", &[spell("orb", &[Reg(2, Byte), Reg(0, Byte)])]),
    ("or_rr_16", &[spell("orw", &[Reg(2, Word), Reg(0, Word)])]),
    ("or_rr_32", &[spell("orl", &[Reg(2, Long), Reg(0, Long)])]),
    ("or_rr_64", &[spell("orq", &[Reg(2, Quad), Reg(0, Quad)])]),
    ("xor_rr_8", &[spell("xorb", &[Reg(2, Byte), Reg(0, Byte)])]),
    ("xor_rr_16", &[spell("xorw", &[Reg(2, Word), Reg(0, Word)])]),
    ("xor_rr_32", &[spell("xorl", &[Reg(2, Long), Reg(0, Long)])]),
    ("xor_rr_64", &[spell("xorq", &[Reg(2, Quad), Reg(0, Quad)])]),
    // The eight bit multiply is done thirty two bits at a time, because the machine has no
    // two-operand multiply narrower than sixteen and the low eight bits of a product are decided
    // by the low eight bits of what went into it. Whatever ends up above them is not part of an
    // eight bit value and nothing that reads one looks there.
    ("imul_rr_8", &[spell("imull", &[Reg(2, Long), Reg(0, Long)])]),
    ("imul_rr_16", &[spell("imulw", &[Reg(2, Word), Reg(0, Word)])]),
    ("imul_rr_32", &[spell("imull", &[Reg(2, Long), Reg(0, Long)])]),
    ("imul_rr_64", &[spell("imulq", &[Reg(2, Quad), Reg(0, Quad)])]),
    // Arithmetic, register with immediate.
    ("add_ri_8", &[spell("addb", &[Imm, Reg(0, Byte)])]),
    ("add_ri_16", &[spell("addw", &[Imm, Reg(0, Word)])]),
    ("add_ri_32", &[spell("addl", &[Imm, Reg(0, Long)])]),
    ("add_ri_64", &[spell("addq", &[Imm, Reg(0, Quad)])]),
    ("sub_ri_8", &[spell("subb", &[Imm, Reg(0, Byte)])]),
    ("sub_ri_16", &[spell("subw", &[Imm, Reg(0, Word)])]),
    ("sub_ri_32", &[spell("subl", &[Imm, Reg(0, Long)])]),
    ("sub_ri_64", &[spell("subq", &[Imm, Reg(0, Quad)])]),
    ("and_ri_8", &[spell("andb", &[Imm, Reg(0, Byte)])]),
    ("and_ri_16", &[spell("andw", &[Imm, Reg(0, Word)])]),
    ("and_ri_32", &[spell("andl", &[Imm, Reg(0, Long)])]),
    ("and_ri_64", &[spell("andq", &[Imm, Reg(0, Quad)])]),
    ("or_ri_8", &[spell("orb", &[Imm, Reg(0, Byte)])]),
    ("or_ri_16", &[spell("orw", &[Imm, Reg(0, Word)])]),
    ("or_ri_32", &[spell("orl", &[Imm, Reg(0, Long)])]),
    ("or_ri_64", &[spell("orq", &[Imm, Reg(0, Quad)])]),
    ("xor_ri_8", &[spell("xorb", &[Imm, Reg(0, Byte)])]),
    ("xor_ri_16", &[spell("xorw", &[Imm, Reg(0, Word)])]),
    ("xor_ri_32", &[spell("xorl", &[Imm, Reg(0, Long)])]),
    ("xor_ri_64", &[spell("xorq", &[Imm, Reg(0, Quad)])]),
    // A multiply by a constant is the one three-operand instruction on this machine, so its
    // source is written even though the destination is tied to it and they are the same register.
    ("imul_ri_8", &[spell("imull", &[Imm, Reg(1, Long), Reg(0, Long)])]),
    ("imul_ri_16", &[spell("imulw", &[Imm, Reg(1, Word), Reg(0, Word)])]),
    ("imul_ri_32", &[spell("imull", &[Imm, Reg(1, Long), Reg(0, Long)])]),
    ("imul_ri_64", &[spell("imulq", &[Imm, Reg(1, Quad), Reg(0, Quad)])]),
    // Negation and complement.
    ("neg_r_8", &[spell("negb", &[Reg(0, Byte)])]),
    ("neg_r_16", &[spell("negw", &[Reg(0, Word)])]),
    ("neg_r_32", &[spell("negl", &[Reg(0, Long)])]),
    ("neg_r_64", &[spell("negq", &[Reg(0, Quad)])]),
    ("not_r_8", &[spell("notb", &[Reg(0, Byte)])]),
    ("not_r_16", &[spell("notw", &[Reg(0, Word)])]),
    ("not_r_32", &[spell("notl", &[Reg(0, Long)])]),
    ("not_r_64", &[spell("notq", &[Reg(0, Quad)])]),
    // Division and remainder, signed and unsigned. The four widening instructions have no operands
    // at all: each of them reads one fixed register and writes another, and the opcode carries
    // both of those as operands so that the allocator leaves them alone.
    ("idiv_quo_8", &[spell("cbtw", &[]), spell("idivb", &DIVISOR_8)]),
    ("idiv_quo_16", &[spell("cwtd", &[]), spell("idivw", &DIVISOR_16)]),
    ("idiv_quo_32", &[spell("cltd", &[]), spell("idivl", &DIVISOR_32)]),
    ("idiv_quo_64", &[spell("cqto", &[]), spell("idivq", &DIVISOR_64)]),
    ("idiv_rem_8", &[spell("cbtw", &[]), spell("idivb", &DIVISOR_8), spell("movb", &HIGH_HALF)]),
    ("idiv_rem_16", &[spell("cwtd", &[]), spell("idivw", &DIVISOR_16)]),
    ("idiv_rem_32", &[spell("cltd", &[]), spell("idivl", &DIVISOR_32)]),
    ("idiv_rem_64", &[spell("cqto", &[]), spell("idivq", &DIVISOR_64)]),
    ("div_quo_8", &[spell("movzbl", &WIDEN_FOR_QUOTIENT), spell("divb", &DIVISOR_8)]),
    ("div_quo_16", &[spell("xorl", &CLEAR_FOR_QUOTIENT), spell("divw", &DIVISOR_16)]),
    ("div_quo_32", &[spell("xorl", &CLEAR_FOR_QUOTIENT), spell("divl", &DIVISOR_32)]),
    ("div_quo_64", &[spell("xorl", &CLEAR_FOR_QUOTIENT), spell("divq", &DIVISOR_64)]),
    (
        "div_rem_8",
        &[
            spell("movzbl", &WIDEN_FOR_REMAINDER),
            spell("divb", &DIVISOR_8),
            spell("movb", &HIGH_HALF),
        ],
    ),
    ("div_rem_16", &[spell("xorl", &CLEAR_FOR_REMAINDER), spell("divw", &DIVISOR_16)]),
    ("div_rem_32", &[spell("xorl", &CLEAR_FOR_REMAINDER), spell("divl", &DIVISOR_32)]),
    ("div_rem_64", &[spell("xorl", &CLEAR_FOR_REMAINDER), spell("divq", &DIVISOR_64)]),
    // Shifts by a constant.
    ("shl_ri_8", &[spell("shlb", &[Imm, Reg(0, Byte)])]),
    ("shl_ri_16", &[spell("shlw", &[Imm, Reg(0, Word)])]),
    ("shl_ri_32", &[spell("shll", &[Imm, Reg(0, Long)])]),
    ("shl_ri_64", &[spell("shlq", &[Imm, Reg(0, Quad)])]),
    ("shr_ri_8", &[spell("shrb", &[Imm, Reg(0, Byte)])]),
    ("shr_ri_16", &[spell("shrw", &[Imm, Reg(0, Word)])]),
    ("shr_ri_32", &[spell("shrl", &[Imm, Reg(0, Long)])]),
    ("shr_ri_64", &[spell("shrq", &[Imm, Reg(0, Quad)])]),
    ("sar_ri_8", &[spell("sarb", &[Imm, Reg(0, Byte)])]),
    ("sar_ri_16", &[spell("sarw", &[Imm, Reg(0, Word)])]),
    ("sar_ri_32", &[spell("sarl", &[Imm, Reg(0, Long)])]),
    ("sar_ri_64", &[spell("sarq", &[Imm, Reg(0, Quad)])]),
    // Shifts by a register, which is `cl` and nothing else, so the count is a byte however wide
    // the thing being shifted is.
    ("shl_rcl_8", &[spell("shlb", &[Reg(2, Byte), Reg(0, Byte)])]),
    ("shl_rcl_16", &[spell("shlw", &[Reg(2, Byte), Reg(0, Word)])]),
    ("shl_rcl_32", &[spell("shll", &[Reg(2, Byte), Reg(0, Long)])]),
    ("shl_rcl_64", &[spell("shlq", &[Reg(2, Byte), Reg(0, Quad)])]),
    ("shr_rcl_8", &[spell("shrb", &[Reg(2, Byte), Reg(0, Byte)])]),
    ("shr_rcl_16", &[spell("shrw", &[Reg(2, Byte), Reg(0, Word)])]),
    ("shr_rcl_32", &[spell("shrl", &[Reg(2, Byte), Reg(0, Long)])]),
    ("shr_rcl_64", &[spell("shrq", &[Reg(2, Byte), Reg(0, Quad)])]),
    ("sar_rcl_8", &[spell("sarb", &[Reg(2, Byte), Reg(0, Byte)])]),
    ("sar_rcl_16", &[spell("sarw", &[Reg(2, Byte), Reg(0, Word)])]),
    ("sar_rcl_32", &[spell("sarl", &[Reg(2, Byte), Reg(0, Long)])]),
    ("sar_rcl_64", &[spell("sarq", &[Reg(2, Byte), Reg(0, Quad)])]),
    // The comparisons, ten conditions at four widths, each of them a comparison and the byte it
    // sets. The condition is in the second of the two and the width is in the first, which is why
    // neither of them alone is the instruction.
    ("cmp_set_e_8", &[spell("cmpb", &CMP_8), spell("sete", &SET)]),
    ("cmp_set_e_16", &[spell("cmpw", &CMP_16), spell("sete", &SET)]),
    ("cmp_set_e_32", &[spell("cmpl", &CMP_32), spell("sete", &SET)]),
    ("cmp_set_e_64", &[spell("cmpq", &CMP_64), spell("sete", &SET)]),
    ("cmp_set_ne_8", &[spell("cmpb", &CMP_8), spell("setne", &SET)]),
    ("cmp_set_ne_16", &[spell("cmpw", &CMP_16), spell("setne", &SET)]),
    ("cmp_set_ne_32", &[spell("cmpl", &CMP_32), spell("setne", &SET)]),
    ("cmp_set_ne_64", &[spell("cmpq", &CMP_64), spell("setne", &SET)]),
    ("cmp_set_l_8", &[spell("cmpb", &CMP_8), spell("setl", &SET)]),
    ("cmp_set_l_16", &[spell("cmpw", &CMP_16), spell("setl", &SET)]),
    ("cmp_set_l_32", &[spell("cmpl", &CMP_32), spell("setl", &SET)]),
    ("cmp_set_l_64", &[spell("cmpq", &CMP_64), spell("setl", &SET)]),
    ("cmp_set_le_8", &[spell("cmpb", &CMP_8), spell("setle", &SET)]),
    ("cmp_set_le_16", &[spell("cmpw", &CMP_16), spell("setle", &SET)]),
    ("cmp_set_le_32", &[spell("cmpl", &CMP_32), spell("setle", &SET)]),
    ("cmp_set_le_64", &[spell("cmpq", &CMP_64), spell("setle", &SET)]),
    ("cmp_set_g_8", &[spell("cmpb", &CMP_8), spell("setg", &SET)]),
    ("cmp_set_g_16", &[spell("cmpw", &CMP_16), spell("setg", &SET)]),
    ("cmp_set_g_32", &[spell("cmpl", &CMP_32), spell("setg", &SET)]),
    ("cmp_set_g_64", &[spell("cmpq", &CMP_64), spell("setg", &SET)]),
    ("cmp_set_ge_8", &[spell("cmpb", &CMP_8), spell("setge", &SET)]),
    ("cmp_set_ge_16", &[spell("cmpw", &CMP_16), spell("setge", &SET)]),
    ("cmp_set_ge_32", &[spell("cmpl", &CMP_32), spell("setge", &SET)]),
    ("cmp_set_ge_64", &[spell("cmpq", &CMP_64), spell("setge", &SET)]),
    ("cmp_set_b_8", &[spell("cmpb", &CMP_8), spell("setb", &SET)]),
    ("cmp_set_b_16", &[spell("cmpw", &CMP_16), spell("setb", &SET)]),
    ("cmp_set_b_32", &[spell("cmpl", &CMP_32), spell("setb", &SET)]),
    ("cmp_set_b_64", &[spell("cmpq", &CMP_64), spell("setb", &SET)]),
    ("cmp_set_be_8", &[spell("cmpb", &CMP_8), spell("setbe", &SET)]),
    ("cmp_set_be_16", &[spell("cmpw", &CMP_16), spell("setbe", &SET)]),
    ("cmp_set_be_32", &[spell("cmpl", &CMP_32), spell("setbe", &SET)]),
    ("cmp_set_be_64", &[spell("cmpq", &CMP_64), spell("setbe", &SET)]),
    ("cmp_set_a_8", &[spell("cmpb", &CMP_8), spell("seta", &SET)]),
    ("cmp_set_a_16", &[spell("cmpw", &CMP_16), spell("seta", &SET)]),
    ("cmp_set_a_32", &[spell("cmpl", &CMP_32), spell("seta", &SET)]),
    ("cmp_set_a_64", &[spell("cmpq", &CMP_64), spell("seta", &SET)]),
    ("cmp_set_ae_8", &[spell("cmpb", &CMP_8), spell("setae", &SET)]),
    ("cmp_set_ae_16", &[spell("cmpw", &CMP_16), spell("setae", &SET)]),
    ("cmp_set_ae_32", &[spell("cmpl", &CMP_32), spell("setae", &SET)]),
    ("cmp_set_ae_64", &[spell("cmpq", &CMP_64), spell("setae", &SET)]),
    // The conversions between widths. Widening to sixty four bits from thirty two is a thirty two
    // bit move, because every instruction that writes a thirty two bit register clears the half
    // above it, and taking the low bits of anything is a move of that many bits.
    ("movzx_8_16", &[spell("movzbw", &[Reg(1, Byte), Reg(0, Word)])]),
    ("movzx_8_32", &[spell("movzbl", &[Reg(1, Byte), Reg(0, Long)])]),
    ("movzx_8_64", &[spell("movzbq", &[Reg(1, Byte), Reg(0, Quad)])]),
    ("movzx_16_32", &[spell("movzwl", &[Reg(1, Word), Reg(0, Long)])]),
    ("movzx_16_64", &[spell("movzwq", &[Reg(1, Word), Reg(0, Quad)])]),
    ("mov_32_to_64", &[spell("movl", &[Reg(1, Long), Reg(0, Long)])]),
    ("movsx_8_16", &[spell("movsbw", &[Reg(1, Byte), Reg(0, Word)])]),
    ("movsx_8_32", &[spell("movsbl", &[Reg(1, Byte), Reg(0, Long)])]),
    ("movsx_8_64", &[spell("movsbq", &[Reg(1, Byte), Reg(0, Quad)])]),
    ("movsx_16_32", &[spell("movswl", &[Reg(1, Word), Reg(0, Long)])]),
    ("movsx_16_64", &[spell("movswq", &[Reg(1, Word), Reg(0, Quad)])]),
    ("movsxd_32_64", &[spell("movslq", &[Reg(1, Long), Reg(0, Quad)])]),
    // Widening a truth value. The byte it is in has its other seven bits zero, so widening the
    // byte is widening the bit and these are the byte widenings again, spelled the same and
    // named apart so the model can say what each of them means about the bit. Widening to a
    // byte is the move that puts it in the destination register and nothing more.
    ("bit_to_8", &[spell("movb", &[Reg(1, Byte), Reg(0, Byte)])]),
    ("bit_to_16", &[spell("movzbw", &[Reg(1, Byte), Reg(0, Word)])]),
    ("bit_to_32", &[spell("movzbl", &[Reg(1, Byte), Reg(0, Long)])]),
    ("bit_to_64", &[spell("movzbq", &[Reg(1, Byte), Reg(0, Quad)])]),
    ("low_8", &[spell("movb", &[Reg(1, Byte), Reg(0, Byte)])]),
    ("low_16", &[spell("movw", &[Reg(1, Word), Reg(0, Word)])]),
    ("low_32", &[spell("movl", &[Reg(1, Long), Reg(0, Long)])]),
    // The address computation the addressing modes are reached through.
    ("lea_64", &[spell("leaq", &[Mem, Reg(0, Quad)])]),
    // Reading and writing memory. The width is the width of what is moved rather than of the
    // address, which is sixty four bits in every one of them.
    ("mov_rm_8", &[spell("movb", &[Mem, Reg(0, Byte)])]),
    ("mov_rm_16", &[spell("movw", &[Mem, Reg(0, Word)])]),
    ("mov_rm_32", &[spell("movl", &[Mem, Reg(0, Long)])]),
    ("mov_rm_64", &[spell("movq", &[Mem, Reg(0, Quad)])]),
    ("mov_mr_8", &[spell("movb", &[Reg(0, Byte), Mem])]),
    ("mov_mr_16", &[spell("movw", &[Reg(0, Word), Mem])]),
    ("mov_mr_32", &[spell("movl", &[Reg(0, Long), Mem])]),
    ("mov_mr_64", &[spell("movq", &[Reg(0, Quad), Mem])]),
    // The three that are not instructions. A return value, an argument and the condition a block
    // leaves on are each one register and one claim about it, and the claim is for the allocator.
    ("ret_val_8", &[]),
    ("ret_val_16", &[]),
    ("ret_val_32", &[]),
    ("ret_val_64", &[]),
    ("ret_val_f32", &[]),
    ("ret_val_f64", &[]),
    ("arg_val_8", &[]),
    ("arg_val_16", &[]),
    ("arg_val_32", &[]),
    ("arg_val_64", &[]),
    ("arg_val_f32", &[]),
    ("arg_val_f64", &[]),
    ("br_cond_8", &[]),
    // A call goes to a name. What it passes and what comes back are in the operand vector and are
    // not written, because the machine does not read them and a reader of the assembly can see
    // them in the instructions above it.
    ("call", &[spell("call", &[Symbol])]),
    // A call through an address, which is the same mnemonic and a different instruction. The star
    // is the whole of the difference in the text and the addressing byte is the whole of it in the
    // bytes, and both come from the argument being a register rather than a place in the program.
    ("call_reg", &[spell("call", &[Through])]),
    // What a condition and the block layout come to.
    ("test_rr_8", &[spell("testb", &[Reg(0, Byte), Reg(0, Byte)])]),
    ("jcc_e", &[spell("je", &[Label])]),
    ("jcc_ne", &[spell("jne", &[Label])]),
    ("jmp", &[spell("jmp", &[Label])]),
    // What a copy, a prologue, an epilogue, a spill and a reload are made of. A vector register is
    // moved with the aligned form for the reason `crate::x86_64::FRAME` gives.
    ("mov_rr_64", &[spell("movq", &[Reg(1, Quad), Reg(0, Quad)])]),
    ("push_64", &[spell("pushq", &[Reg(0, Quad)])]),
    ("pop_64", &[spell("popq", &[Reg(0, Quad)])]),
    ("ret", &[spell("ret", &[])]),
    ("movaps_rr", &[spell("movaps", &[Reg(1, Quad), Reg(0, Quad)])]),
    ("movaps_rm", &[spell("movaps", &[Mem, Reg(0, Quad)])]),
    ("movaps_mr", &[spell("movaps", &[Reg(0, Quad), Mem])]),
    // The arithmetic is two address, so the destination is not written: it is the first source and
    // the allocator has already made the two the same register.
    ("addss_rr", &[spell("addss", &[Reg(2, Quad), Reg(0, Quad)])]),
    ("addsd_rr", &[spell("addsd", &[Reg(2, Quad), Reg(0, Quad)])]),
    ("subss_rr", &[spell("subss", &[Reg(2, Quad), Reg(0, Quad)])]),
    ("subsd_rr", &[spell("subsd", &[Reg(2, Quad), Reg(0, Quad)])]),
    ("mulss_rr", &[spell("mulss", &[Reg(2, Quad), Reg(0, Quad)])]),
    ("mulsd_rr", &[spell("mulsd", &[Reg(2, Quad), Reg(0, Quad)])]),
    ("divss_rr", &[spell("divss", &[Reg(2, Quad), Reg(0, Quad)])]),
    ("divsd_rr", &[spell("divsd", &[Reg(2, Quad), Reg(0, Quad)])]),
];

/// The instructions the opcode of that name is written as.
///
/// `None` for a name this target does not have, and an empty slice for one that is an opcode and
/// not an instruction, which are two different answers.
///
/// The name is written the way the machine IR holds it, so `add_rr_32` rather than `x64.add_rr_32`.
#[must_use]
pub fn written(name: &str) -> Option<&'static [Written]> {
    TEXT.iter().find(|(known, _)| *known == name).map(|&(_, insts)| insts)
}

/// The four spellings of each general purpose register, narrowest first.
///
/// In the order the registers are numbered, which is the encoding's order, so the row a register
/// is on is its number. The first four rows could each be spelled two ways for the byte: `al` is
/// the low byte of `rax` and `ah` is the one above it, and the second of those cannot be written
/// in an instruction that also names one of the eight registers x86-64 added. So the low byte is
/// what is here, and the one instruction that wants a high byte names it itself.
static GPR_TEXT: [[&str; 4]; 16] = [
    ["al", "ax", "eax", "rax"],
    ["cl", "cx", "ecx", "rcx"],
    ["dl", "dx", "edx", "rdx"],
    ["bl", "bx", "ebx", "rbx"],
    ["spl", "sp", "esp", "rsp"],
    ["bpl", "bp", "ebp", "rbp"],
    ["sil", "si", "esi", "rsi"],
    ["dil", "di", "edi", "rdi"],
    ["r8b", "r8w", "r8d", "r8"],
    ["r9b", "r9w", "r9d", "r9"],
    ["r10b", "r10w", "r10d", "r10"],
    ["r11b", "r11w", "r11d", "r11"],
    ["r12b", "r12w", "r12d", "r12"],
    ["r13b", "r13w", "r13d", "r13"],
    ["r14b", "r14w", "r14d", "r14"],
    ["r15b", "r15w", "r15d", "r15"],
];

/// What one general purpose register is called at that width, without the sigil.
///
/// `None` for a number that is not one of the sixteen. Every other class on this target has one
/// spelling per register, which is what the register file already holds, so this is the only place
/// a width changes a name.
#[must_use]
pub fn gpr_name(reg: PhysReg, width: Width) -> Option<&'static str> {
    GPR_TEXT.get(usize::from(reg.number())).map(|names| names[width.index()])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operand::Constraint;
    use crate::x86_64::insts::{Form, INSTS, form};
    use crate::x86_64::{GPR, REGS};

    /// Every index into the operand vector that one of these arguments names.
    fn named(insts: &[Written]) -> Vec<u8> {
        let mut at: Vec<u8> = insts
            .iter()
            .flat_map(|inst| inst.args)
            .filter_map(|arg| match *arg {
                Reg(at, _) => Some(at),
                _ => None,
            })
            .collect();
        at.sort_unstable();
        at.dedup();
        at
    }

    #[test]
    fn every_opcode_is_written_once_and_in_the_order_it_is_described_in() {
        let written: Vec<&str> = TEXT.iter().map(|&(name, _)| name).collect();
        let described: Vec<&str> = INSTS.iter().map(|&(name, _)| name).collect();
        // Same order and not merely the same set, because the two tables are read side by side
        // and a reader who has to search for the other half of an opcode will stop doing it.
        assert_eq!(written, described);
    }

    /// Everything an instruction is given has to come from somewhere the instruction has.
    ///
    /// Both directions. An argument that names an operand the form does not have would be read off
    /// the end of the vector, and an immediate or an address or a label the form carries and no
    /// instruction writes would be dropped on the floor, which is the failure that produces
    /// assembly that assembles and does the wrong thing.
    #[test]
    fn every_argument_names_something_the_instruction_really_has() {
        for &(name, insts) in TEXT {
            let form = form(name).expect("every written opcode is a described opcode");
            let operands = form.operands();
            let (mut imm, mut mem, mut symbol, mut label) = (false, false, false, false);
            let mut through = false;
            for arg in insts.iter().flat_map(|inst| inst.args) {
                match *arg {
                    Reg(at, _) => assert!(
                        usize::from(at) < operands.len(),
                        "{name} names operand {at} and has {} of them",
                        operands.len()
                    ),
                    // The other way round from the rest of them. A register the register file
                    // knows is one the allocator hands out, and an instruction that wants one of
                    // those should be carrying it as an operand so that it gets one.
                    Named(register) => assert!(
                        REGS.reg_named(register).is_none(),
                        "{name} names {register}, which is a register something could be in"
                    ),
                    Imm => imm = true,
                    Mem => mem = true,
                    Symbol => symbol = true,
                    Label => label = true,
                    // The one argument that names an operand without saying which, so there is no
                    // index to check against the form. What is checked is that only a call has
                    // one, and that a call has one of these or a symbol and never both: those are
                    // the two places a call can go and an instruction that named neither would go
                    // nowhere.
                    Through => through = true,
                }
            }
            assert_eq!(imm, form.takes_imm(), "{name} and its immediate disagree");
            assert_eq!(mem, form.takes_mem(), "{name} and its addressing mode disagree");
            assert_eq!(symbol || through, form == Form::Call, "{name} and where it goes disagree");
            assert!(!(symbol && through), "{name} goes to a name and through a register at once");
            assert_eq!(
                label,
                matches!(form, Form::Jcc | Form::Jmp),
                "{name} and where it goes disagree"
            );
        }
    }

    /// The other half of the same claim: an operand nothing names is one that is not written.
    ///
    /// Two kinds of operand are deliberately not named. The first source of a two-address
    /// instruction is the destination, which the allocator has arranged by now, so writing it
    /// again would be writing the same register twice. And a division names its dividend and both
    /// of its answers in the opcode, so all it is given is the divisor.
    #[test]
    fn an_operand_no_instruction_names_is_one_that_is_not_written() {
        for &(name, insts) in TEXT {
            let form = form(name).expect("every written opcode is a described opcode");
            if insts.is_empty() || matches!(form, Form::DivQuo | Form::DivRem) {
                continue;
            }
            let named = named(insts);
            let operands = form.operands();
            for at in 0..operands.len() {
                let tied = operands.iter().enumerate().any(|(other, operand)| {
                    operand.constraint == Constraint::Reuse(at as u8)
                        && named.contains(&(other as u8))
                });
                assert!(
                    named.contains(&(at as u8)) || tied,
                    "{name} has an operand {at} that nothing written for it names"
                );
            }
        }
    }

    #[test]
    fn an_opcode_that_is_not_an_instruction_is_written_as_no_instructions() {
        for name in ["ret_val_32", "arg_val_64", "ret_val_f64", "arg_val_f32", "br_cond_8"] {
            assert_eq!(written(name), Some([].as_slice()), "{name}");
        }
        for &(name, insts) in TEXT {
            let form = form(name).expect("every written opcode is a described opcode");
            assert_eq!(
                insts.is_empty(),
                matches!(
                    form,
                    Form::RetVal | Form::ArgVal | Form::RetValVec | Form::ArgValVec | Form::BrCond
                ),
                "{name} and whether it is an instruction disagree"
            );
        }
    }

    #[test]
    fn the_widest_spelling_of_a_register_is_the_one_the_register_file_gives_it() {
        // The register file holds one name per register and this holds four, and the widest of the
        // four is that one. A target that disagreed with itself here would print a register the
        // machine IR calls one thing under another name.
        for number in 0..16u8 {
            let reg = PhysReg::new(number);
            assert_eq!(gpr_name(reg, Quad), REGS.name(GPR, reg), "register {number}");
        }
        assert_eq!(gpr_name(PhysReg::new(16), Quad), None);
    }

    #[test]
    fn a_register_is_spelled_by_how_much_of_it_an_instruction_reads() {
        use crate::x86_64::{R8, RAX, RDI};

        assert_eq!(gpr_name(RAX, Byte), Some("al"));
        assert_eq!(gpr_name(RAX, Long), Some("eax"));
        // The three that are not the first letter of the wide name with an `e` in front of it,
        // which is where a table beats a rule.
        assert_eq!(gpr_name(RDI, Byte), Some("dil"));
        assert_eq!(gpr_name(RDI, Word), Some("di"));
        assert_eq!(gpr_name(R8, Long), Some("r8d"));
    }

    #[test]
    fn an_opcode_is_written_under_the_name_the_machine_ir_holds() {
        let add = written("add_rr_32").expect("an opcode this target has");
        assert_eq!(add, [spell("addl", &[Reg(2, Long), Reg(0, Long)])]);
        assert_eq!(written("x64.add_rr_32"), None, "the prefix is not part of the opcode");
        assert_eq!(written("add_rr_128"), None);
    }

    /// A spot check of the shapes that are more than one instruction, which are the ones a reader
    /// of `-S` is most likely to be surprised by and the ones an encoder has to agree with.
    #[test]
    fn an_opcode_the_machine_has_no_single_instruction_for_is_written_as_the_ones_it_has() {
        let compare = written("cmp_set_l_32").expect("an opcode this target has");
        assert_eq!(compare.iter().map(|inst| inst.mnemonic).collect::<Vec<_>>(), ["cmpl", "setl"]);

        let divide = written("idiv_quo_64").expect("an opcode this target has");
        assert_eq!(divide.iter().map(|inst| inst.mnemonic).collect::<Vec<_>>(), ["cqto", "idivq"]);

        // The one that is three, and the one place a register is named rather than allocated.
        let remainder = written("div_rem_8").expect("an opcode this target has");
        assert_eq!(
            remainder.iter().map(|inst| inst.mnemonic).collect::<Vec<_>>(),
            ["movzbl", "divb", "movb"]
        );
        assert_eq!(remainder[2].args, [Named("ah"), Reg(0, Byte)]);
    }
}
