//! What the DWARF unwinder in libgcc asks the compiler about the machine it is built for.
//!
//! libgcc's `unwind-dw2.c` is written against five builtins that answer questions about the frame
//! and the register file, and three of the answers are facts about the target rather than about
//! the program: which DWARF column is the stack pointer, which registers carry the exception and
//! its selector into a landing pad, and how many bytes the unwinder keeps for each column. gcc
//! answers them from the target's macros. This is the same three answers, as data, for each
//! machine this compiles for that gcc builds the DWARF unwinder for.
//!
//! Every number here was read off gcc 13.3.0 and its cross compilers for aarch64, i686 and
//! riscv64: what the two constant builtins answer when the program prints them, and the stores
//! `__builtin_init_dwarf_reg_size_table` becomes at `-O2`. A byte gcc does not store is not in the
//! list, which is why i686 leaves column 10 alone, and a zero gcc does store is in it, which is
//! the columns AArch64 gives the predicates and the vector registers no call preserves.

use rucc_tuple::Arch;

/// What the unwinder asks about one machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unwinder {
    /// The DWARF column of the stack pointer, which is what `__builtin_dwarf_sp_column` answers.
    pub sp_column: i32,
    /// The DWARF column of each register a landing pad is handed the exception in, in the order
    /// `__builtin_eh_return_data_regno` numbers them. A number past the end of this answers `-1`.
    pub data_regs: &'static [i32],
    /// What `__builtin_init_dwarf_reg_size_table` writes, as runs of columns that all get the same
    /// byte: the first column, the last one, and the byte.
    pub reg_sizes: &'static [(u16, u16, u8)],
}

/// x86-64, where the columns are rax, rdx, rcx, rbx, rsi, rdi, rbp, rsp, r8 to r15 and the return
/// address, and every one of those seventeen is eight bytes.
static X86_64: Unwinder = Unwinder { sp_column: 7, data_regs: &[0, 1], reg_sizes: &[(0, 16, 8)] };

/// i386, where the stack pointer is column 4 and the second data register is edx, column 2. The
/// eight x87 registers start at column 11, and the first six of them fit under the seventeen
/// columns the unwinder keeps, at the twelve bytes of an `XFmode` value.
static X86: Unwinder =
    Unwinder { sp_column: 4, data_regs: &[0, 2], reg_sizes: &[(0, 9, 4), (11, 16, 12)] };

/// AArch64, whose ninety seven columns are x0 to x30 and sp, the vector length at 46, the
/// predicates and the vector registers, and the return address column at 96. The vector registers
/// a call preserves, v8 to v15, keep eight bytes and the rest are written as nothing.
static AARCH64: Unwinder = Unwinder {
    sp_column: 31,
    data_regs: &[0, 1, 2, 3],
    reg_sizes: &[(0, 31, 8), (46, 46, 8), (48, 71, 0), (72, 79, 8), (80, 95, 0), (96, 96, 8)],
};

/// RISC-V with the double precision extension, where the stack pointer is x2, the four data
/// registers are a0 to a3, and the return address column is 64.
static RISCV64: Unwinder =
    Unwinder { sp_column: 2, data_regs: &[10, 11, 12, 13], reg_sizes: &[(0, 64, 8), (96, 127, 4)] };

/// What the unwinder asks about a machine, or nothing for one gcc does not build the DWARF
/// unwinder for or this has not measured.
#[must_use]
pub const fn unwinder(arch: Arch) -> Option<&'static Unwinder> {
    match arch {
        Arch::X86_64 => Some(&X86_64),
        Arch::X86 => Some(&X86),
        Arch::Aarch64 => Some(&AARCH64),
        Arch::Riscv64 => Some(&RISCV64),
        Arch::Arm
        | Arch::Arm64Ec
        | Arch::Riscv32
        | Arch::LoongArch64
        | Arch::PowerPc64
        | Arch::S390x
        | Arch::Wasm32 => None,
    }
}

impl Unwinder {
    /// What `__builtin_eh_return_data_regno` answers for that number.
    #[must_use]
    pub fn data_regno(&self, number: i128) -> i32 {
        usize::try_from(number).ok().and_then(|at| self.data_regs.get(at)).copied().unwrap_or(-1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four answers the issue names, which are gcc's `DWARF_FRAME_REGNUM` of the stack pointer.
    #[test]
    fn the_stack_pointer_column_is_the_one_gcc_answers() {
        let column = |arch| unwinder(arch).map(|it| it.sp_column);
        assert_eq!(column(Arch::X86_64), Some(7));
        assert_eq!(column(Arch::Aarch64), Some(31));
        assert_eq!(column(Arch::Riscv64), Some(2));
        assert_eq!(column(Arch::X86), Some(4));
        assert_eq!(column(Arch::Arm), None);
    }

    #[test]
    fn a_data_register_past_the_last_one_is_minus_one() {
        let x86_64 = unwinder(Arch::X86_64).expect("measured");
        assert_eq!([0, 1, 2, -1].map(|n| x86_64.data_regno(n)), [0, 1, -1, -1]);
        let x86 = unwinder(Arch::X86).expect("measured");
        assert_eq!([0, 1, 2].map(|n| x86.data_regno(n)), [0, 2, -1]);
        let riscv = unwinder(Arch::Riscv64).expect("measured");
        assert_eq!([0, 3, 4].map(|n| riscv.data_regno(n)), [10, 13, -1]);
    }

    /// The runs are in order and do not overlap, so that the stores they become write each byte
    /// once, and the last column is under the count gcc keeps for the machine.
    #[test]
    fn the_runs_of_the_size_table_are_in_order_and_under_the_column_count() {
        for (arch, columns) in
            [(Arch::X86_64, 17), (Arch::X86, 17), (Arch::Aarch64, 97), (Arch::Riscv64, 129)]
        {
            let table = unwinder(arch).expect("measured").reg_sizes;
            assert!(table.windows(2).all(|pair| pair[0].1 < pair[1].0), "{arch:?}");
            assert!(table.iter().all(|&(first, last, _)| first <= last), "{arch:?}");
            assert!(table.last().is_some_and(|&(_, last, _)| last < columns), "{arch:?}");
        }
    }
}
