# What the pipeline still knows about x86-64

`spec/10-backend.md` section 10.8 says a pipeline crate holds no target specific code. Until M6 there was one target, so nothing tested that, and `rucc-codegen` grew a number of places that reach for `rucc_target::x86_64` by name. This file lists them, what has been moved behind the target description, and what is still there. It is written by hand and should shrink. When a line here is fixed, delete it in the same PR.

## Moved

The instruction selector in `lower.rs` is handed a `select::Selector` instead of assuming x86-64. The selector holds the compiled rule table, the operand shapes, the address constructors, the frame and branch instructions, the address class, and the names of the fence and the trap. Before this, `lower.rs` had its own `x64.` prefix constant, a static pointing at the x86-64 table, and called `x86_64::form`, `x86_64::address`, `x86_64::FRAME`, `x86_64::BRANCH` and `x86_64::GPR` directly. `pipeline::Machine` now carries the selector next to the other tables, so `Machine::aarch64` already comes with the AArch64 rules.

The calling convention in `abi.rs` writes its pseudos, loads, stores and calls through an `abi::Insts` the selector carries, with one for each machine. The AArch64 one uses the 32 bit pseudo for anything narrower than 32 bits, since that is the register such a value lives in. The one x86 name left there is the `movq` that copies an unprototyped float into a general purpose register, which only the Windows x64 convention asks for.

The registers `__builtin_longjmp` may hold things in come from the selector's scratch list, which is `r10` and `r11` on x86-64 and `x16` and `x17` on AArch64, rather than from the x86-64 pair in `pipeline.rs`.

A symbol's address is reached through the selector's `Symbols`, which says whether the machine does it with an addressing mode, the way x86-64 does with `lea` and a load from the global offset table, or with its own instruction, the way AArch64 does with `adrp` and `add` or a load from the table.

The address constructor enum moved from `rucc_target::x86_64` to `rucc_target::Address`, because the rule files for both machines use the same names for the same shapes. AArch64 lists the two it has.

## Still there

These are in code that runs, not in tests. A test that builds x86-64 instructions by hand to exercise a pass is not a leak, since the pass under test only sees names.

- `lower.rs` writes `va_start` as the SysV x86-64 `va_list` with its four fields and the register save area. AAPCS64 has a five field `va_list` with two save areas, and Darwin uses a plain pointer. The `Varargs::Pointer` case already covers the Darwin shape. A variadic definition is refused on AArch64 until its list is written.

- `lower.rs` builds a jump table and reaches a thread-local variable only the x86-64 way. Both are refused on any other machine through `Unsupported::Unported`.

- `lower.rs` lowers every `long double` operation through x87 instructions. AArch64 Linux has a 128-bit IEEE `long double` that goes through soft float calls, and Darwin makes it a `double`. The x87 arm has to become a question for the target rather than the only answer.

- `lower.rs` lowers inline `asm` with the x86 template reader. A function with an `asm` statement is now refused on any other machine instead of being read as the wrong language.

- `lower.rs` reads the thread block's slot in the global offset table with `mov_rm_64`, jumps away with `jmp_away`, and reads a global register variable with `x86_64::gpr_named`. The last needs a register name table per machine.

- `lower.rs` assumes a 64-bit address in `ADDRESS_BITS`. That holds for every target in plan tier 1 and fails for i686, armv7 and x32.
