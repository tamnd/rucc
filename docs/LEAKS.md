# What the pipeline still knows about x86-64

`spec/10-backend.md` section 10.8 says a pipeline crate holds no target specific code. Until M6 there was one target, so nothing tested that, and `rucc-codegen` grew a number of places that reach for `rucc_target::x86_64` by name. This file lists them, what has been moved behind the target description, and what is still there. It is written by hand and should shrink. When a line here is fixed, delete it in the same PR.

## Moved

The instruction selector in `lower.rs` is handed a `select::Selector` instead of assuming x86-64. The selector holds the compiled rule table, the operand shapes, the address constructors, the frame and branch instructions, the address class, and the names of the fence and the trap. Before this, `lower.rs` had its own `x64.` prefix constant, a static pointing at the x86-64 table, and called `x86_64::form`, `x86_64::address`, `x86_64::FRAME`, `x86_64::BRANCH` and `x86_64::GPR` directly. `pipeline::Machine` now carries the selector next to the other tables, so `Machine::aarch64` already comes with the AArch64 rules.

The calling convention in `abi.rs` writes its pseudos, loads, stores and calls through an `abi::Insts` the selector carries, with one for each machine. The AArch64 one uses the 32 bit pseudo for anything narrower than 32 bits, since that is the register such a value lives in. The one x86 name left there is the `movq` that copies an unprototyped float into a general purpose register, which only the Windows x64 convention asks for.

The registers `__builtin_longjmp` may hold things in come from the selector's scratch list, which is `r10` and `r11` on x86-64 and `x16` and `x17` on AArch64, rather than from the x86-64 pair in `pipeline.rs`.

A symbol's address is reached through the selector's `Symbols`, which says whether the machine does it with an addressing mode, the way x86-64 does with `lea` and a load from the global offset table, or with its own instruction, the way AArch64 does with `adrp` and `add` or a load from the table.

A jump table and the address of a label are built from the selector's `Jumps`, which names the instruction that takes the address of a place in this function, the load of a cell and the add. On x86-64 those are `lea`, `movslq` and a two address `add`, and on AArch64 they are `adr`, `ldrsw` and a three address `add`.

`va_start` writes the list the convention's `VaList` names. SysV x86-64 gets its four fields, AAPCS64 gets its five with the two offsets counted up from minus what is left of each half, and Windows x64 gets a pointer. The spare registers go into the save area through the selector's own stores, so a vector register is saved sixteen bytes wide on both machines.

A thread-local variable is reached through the selector's `Symbols` too, which name the load of its offset from the thread pointer and how the thread pointer is read. On x86-64 those are a `mov` through the global offset table and a load at `%fs:0`, and on AArch64 they are `adrp` and `ldr` against `:gottprel:` and an `mrs` of `tpidr_el0`.

The address constructor enum moved from `rucc_target::x86_64` to `rucc_target::Address`, because the rule files for both machines use the same names for the same shapes. AArch64 lists the two it has.

## Still there

These are in code that runs, not in tests. A test that builds x86-64 instructions by hand to exercise a pass is not a leak, since the pass under test only sees names.

- `lower.rs` refuses a thread-local variable on Darwin, which reaches one through a descriptor call. It tells Darwin apart by asking whether the list is one pointer on a convention that counts the register files apart, which is a stand in for a real per platform answer.

- `wide.rs` does not split an `__int128` passed past the `...` on Darwin, since the split names every argument and Darwin puts the unnamed ones in memory. Such a call is refused.

- `lower.rs` still has an x87 arm for an eighty bit float, and it is reached only on x86-64, where `long double` is that format. AArch64 Linux has a 128-bit IEEE `long double` that goes through the same soft float calls `_Float128` does on x86-64, and Darwin makes it a `double`, so neither reaches the arm, but the question of which float the machine computes in is still answered by the format and not by the target.

- `lower.rs` lowers inline `asm` with the x86 template reader. A function with an `asm` statement is now refused on any other machine instead of being read as the wrong language.

- `lower.rs` jumps away with `jmp_away`, and reads a global register variable with `x86_64::gpr_named`. The last needs a register name table per machine.

- `lower.rs` assumes a 64-bit address in `ADDRESS_BITS`. That holds for every target in plan tier 1 and fails for i686, armv7 and x32.
