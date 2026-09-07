# The architectures

What each architecture demands of the backend that the ones before it did not. Parent document 10 describes the backend; this document is the per-target delta, written so that the cost of each row in document 04's table is legible before it is paid.

The ordering is by what each one teaches, not by importance.

## 5.1 x86-64, the reference

Already implemented and already the yardstick. Three things about it distort the project's intuitions and are worth naming so that the second and third targets are not surprised:

**It hides addressing.** `[base + index*scale + disp]` folds an add, a shift and a load into one instruction. Every address computation the middle end leaves lying around is free here and is not free anywhere else. RISC-V's absence of this is precisely why parent document 10 calls it the canary.

**It hides comparison.** Flags are set as a side effect, so a compare-and-branch is two instructions that fuse, and the middle end's habit of materializing an `i1` and branching on it costs nothing. On AArch64 it costs a `cmp`/`b.cc` pair; on RISC-V a materialized boolean is a genuine `slt` plus `bnez`, two instructions where one would do. rucc issue #518 ("nothing produces one bit arithmetic") is about the same seam from the other side.

**Its `long double` is real.** x87 80-bit, sixteen bytes with six of padding, returned in `st(0)`, which is why x87 cannot be dropped and why rucc issue #540 exists. Every other architecture in document 04 either makes `long double` a `double` (Darwin, Windows) or makes it IEEE binary128 in software (AArch64 Linux, RISC-V, s390x, ppc64le with the ELFv2 default). **`long double` is the single most target-divergent fundamental type in C** and `TargetInfo` is already right to carry both its width and its format as separate fields.

**What is new and unresolved.** APX doubles the GPR file to 32 with a new destination operand, and AVX10.2 is confirmed for Nova Lake. Neither is an ABI change *yet*, and document 01.12 records the open psABI argument about 512-bit vectors. Position: implement APX as a feature (`+egpr`, `+ndd`, `+ccmp`, `+ppx`) behind `-march=`, do not make it a tuple field, and take document 03.7's conservative option on vector width.

**Sub-architecture levels.** `x86-64-v1` through `v4` are a real user-facing feature, distributions ship v3 variants, and they are macro expansions over the feature bitset, nothing more. `v5` is not defined and we do not guess.

## 5.2 AArch64: the first honest test

The second target, parent M6, and the one where the abstraction is first tested by something other than assertion.

**What is genuinely easier.** AAPCS64 is cleaner than SysV: x0 to x7, v0 to v7, everything over sixteen bytes indirect except homogeneous floating-point aggregates of up to four members. The classification algorithm is a fraction of the size of x86-64's recursive eightbyte merge.

**What is harder.** Every constant is a problem. There is no 64-bit immediate; there is `mov`/`movk` sequences, the logical-immediate encoding (a bitmask pattern, not a value), and the `adrp`/`add` pair for addresses with its 4 KB page granularity and its ±4 GB range. Address materialization is a lowering decision with several forms and a cost model, where x86-64 had one form.

**The Apple divergences**, from parent document 12.3, are the four highest-value facts in that document and they are worth restating as *bugs waiting to happen* rather than as a list:

1. Stack arguments are packed at natural size, not promoted to 8-byte slots. A stack `char` occupies one byte. Get this wrong and every function with more than eight arguments is wrong on Darwin and right on Linux.
2. **Variadic arguments never use the register sequence.** They all go on the stack, `va_list` is a plain `char*`, and a variadic call is ABI-incompatible with a non-variadic one. A function declared without a prototype and called variadically fails. This is the one that breaks `printf` in a way that looks like a codegen bug.
3. `long double` is `double`.
4. x18 is reserved by the OS and must never be allocated. Windows on ARM reserves it too, for the TEB, so this is a per-`(arch, os)` reserved-register fact and document 03.5 gives it a home.

**Pointer authentication and BTI.** `-mbranch-protection=standard` is not optional on Darwin arm64e and is increasingly the Linux distribution default. It changes the prologue (`paciasp`), the epilogue (`retaa` or `autiasp`/`ret`) and every indirect branch target (`bti c`). Parent document 17 puts it in M10 with the hardening flags, which is right, but the *shape* of it, instructions inserted at every function boundary and every indirect target, has to be in the frame lowering from the start or it is a rewrite.

**SVE and SME.** Length-agnostic vectors have no `sizeof`, which C has no way to express, so they arrive as sizeless types through the ACLE headers with their own rules. This is a large surface and it is post-1.0. NEON, via the intrinsic headers parent document 13 already commits to, is not.

## 5.3 RISC-V: the canary

**Why it earns its place ahead of easier targets.** No condition codes, no complex addressing modes, no immediate large enough to be interesting (12 bits), and a two-instruction `lui`/`addi` for anything wider. Every place the middle end left an address computation, a redundant comparison or a materialized boolean, RISC-V charges for it. Anything the x86-64 backend got away with shows up as a code-quality number here first.

**Linker relaxation is the structural surprise.** RISC-V's linker deletes instructions: a `lui`/`addi` pair that turns out to be reachable by `addi` from `gp` becomes one instruction, and the linker removes the other and fixes up every offset after it. That means **the assembler cannot resolve local branch distances**, so relaxable relocations must be emitted rather than folded, and alignment directives must be emitted as `R_RISCV_ALIGN` relocations with padding for the linker to consume. A backend that resolves what it can locally, the way an x86-64 backend correctly does, produces objects that are wrong after relaxation. Debug info is affected too, DWARF fission plus relaxation was a live LLVM issue as recently as version 22.

**The psABI variants.** LP64D is the one that matters; LP64, LP64F, ILP32, ILP32F and ILP32D exist and are `Abi` values in document 03's tuple. The classification rule with no analogue elsewhere: a struct of two floating-point members travels in two FP registers, and a struct of one integer and one float travels in one of each. That rule is worth its own generated test set.

**RVA23 as the baseline.** Ratified October 2024, mandatory V, Zvbb, Zvkt, Zicond, Zimop, Zcmop, Zcb, Zfa, Zihintntl and Supm, and it is the Android RISC-V baseline. The practical consequence is that `-march=rva23u64` is a name users will write and it must expand to the right thirty-odd extensions. `Zicond` in particular is a conditional-move instruction, which changes if-conversion's cost model from "never profitable" to "usually profitable", so it is a feature that reaches the middle end.

**Compressed instructions.** `C` is in every profile, so instruction lengths are 2 or 4 bytes and the encoder must emit compressed forms where they fit, which is a peephole over the encoder rather than a selection decision. Not emitting them costs roughly 20 to 25% of text size, which is visible in the `-Os` numbers.

## 5.4 i686: the one that is not a subset

Tempting to treat as x86-64 with narrower registers. It is not.

**Eight general-purpose registers, of which several are constrained.** Register pressure is the dominant cost and the allocator's live-range splitting matters more here than anywhere else.

**PIC needs a GOT base register.** The `call .L; pop %ebx` idiom, or `__x86.get_pc_thunk.bx`, and `%ebx` is reserved for it. Nothing in a 64-bit backend prepares for a target where the addressing mode has a prerequisite.

**x87 is the floating-point unit** for the base ABI: a register stack, not a register file, with `fxch` to reach anything but the top, and 80-bit intermediate precision that changes results. `-mfpmath=sse` exists on i686 and is not the default. This is where `FLT_EVAL_METHOD == 2` is real and where excess precision is a correctness question rather than a footnote.

**`long long` alignment is 4 on Linux and 8 on Windows**, which is a struct layout divergence within one architecture, and a good test that layout is really coming from the ABI description.

**Why it is still worth doing:** it is the M10 abstraction test, it is cheap because the encoder and the object format are shared, and it is the first target where `DataModel` being real is load-bearing.

## 5.5 ARM32: the ABI in the environment field

`armv7-linux-gnueabihf` versus `armv7-linux-gnueabi` differ in how floats are passed, and that is why the float ABI is spelled in the environment. Three variants, soft, softfp, hard, with soft meaning no FPU instructions at all, softfp meaning FPU instructions with integer-register argument passing, and hard meaning VFP registers for arguments.

**Thumb-2 is a second instruction encoding for the same architecture**, selected per function by an attribute or globally by the target, with interworking on branches. Most Linux userspace is Thumb-2 for size.

**The constant pool.** ARM32 has no way to materialize an arbitrary 32-bit constant, so constants over the 8-bit-rotated immediate range go in a pool in `.text` reachable by a PC-relative load, and the pool must be placed within range, which means the *assembler* has a placement problem with a distance constraint. rucc's integrated assembler has not met anything like this. It is the strongest argument for doing i686 as the fourth target rather than ARM32.

**AEABI helper calling conventions diverge from the generic ones**, per document 01.11: `__aeabi_idivmod` and friends return two values in two registers and are the documented case where compiler-rt and libgcc genuinely differ. `rucc-builtins` must implement the AEABI spellings on this target, not the generic ones.

## 5.6 LoongArch

New enough to have clean documentation and old enough to have a settled psABI. LP64D is the only ABI in production; GCC 16 added LA32 with ilp32d/f/s and it is not something to chase.

Three facts that matter for the backend: `$r21` is reserved by the psABI and used by Linux as the percpu base, so it is another reserved-register row; LSX is 128-bit and LASX 256-bit; and the object-file ABI v0→v1 migration is complete, so we emit v1 and nothing else.

The realistic constraint is document 04.6's: hardware is not obtainable outside China, so this is a qemu-user tier-2 target permanently unless that changes.

## 5.7 ppc64le and s390x

**ppc64le / ELFv2.** The TOC is the structural feature: a per-module table addressed through `r2`, with each function having a *global entry point* that establishes `r2` and a *local entry point* two instructions later for callers in the same module. Getting the two entry points and the `.localentry` annotation right is the whole of the ABI's difficulty, and it is invisible until a cross-module call has the wrong `r2` and loads a global from the wrong table. ELFv2 is not synonymous with little-endian, FreeBSD/powerpc64 uses it big-endian, and document 01.12 records that inconsistency as a live source of lld and LTO bugs.

**s390x is the big-endian row and that is why it is on the list.** Everything else it demands is ordinary: 16 general registers, 16 floating-point, a straightforward linkage convention with a register save area. What it demands that nothing else does is that **every byte we write is written in the target's order, and every constant we fold is folded in the target's order**. Bitfield allocation runs from the high-order end. Union punning gives different answers. A `char*` walk over an `int` sees the bytes reversed. Struct layout does not change but the contents do.

The specific rucc hazard: the object writers, the constant folder, the initializer emitter, and the software floating-point in the constant evaluator all have to be endianness-parameterized rather than host-native. Rust makes this easy to get right (`to_le_bytes` versus `to_be_bytes` is explicit) and easy to get wrong (`as` casts and `transmute` are not). **The check is a CI job that builds rung 0 for s390x from an s390x host**, which is document 04.4's argument for the big-endian host row: little-endian host to big-endian target and big-endian host to little-endian target are different bugs.

## 5.8 The freestanding targets

`x86_64-none`, `aarch64-none`, `riscv64-none`. No libc, nine compiler-provided headers, `-ffreestanding`, `-nostdlib`. rucc already models this as `Os::None` and the header lookup already correctly returns nothing for it.

This is not a lesser target. It is the *kernel* target, parent rung 4 depends on it, and it demands things hosted targets do not: `-mcmodel=kernel`, `-mno-red-zone`, `-mgeneral-regs-only` as a hard constraint that must make float use an error rather than a fallback, `-fno-pic` with absolute addressing, custom linker scripts, and section attributes used as a code-generation mechanism. Document 04 puts it at tier 1 for that reason.

## 5.9 wasm32: the architecture that is not one

Worth including because it is genuinely popular and worth flagging because almost nothing in a conventional backend transfers.

**A stack machine with structured control flow.** No registers, no arbitrary branches, `block`, `loop`, `if`, `br` to a label depth. A CFG has to be *relooped* back into structured control flow, which is an algorithm (Stackifier, or the Relooper) and not a lowering rule. Irreducible control flow, which C's `goto` produces, needs a dispatch loop with a state variable.

**Locals are not registers.** There is no register allocation in the usual sense; there is local numbering and a value stack, and the allocator is a different program.

**One linear memory, and `setjmp`/`longjmp` has no natural implementation.** It needs exception handling or Asyncify. Signals do not exist.

**wasip3 is a different ABI from wasip1/p2**, per document 01.12: `__stack_pointer` and `__tls_base` become component-model `context.get`/`context.set` builtins with no opt-out on that target. So "wasm32" is at least three tuples and they are not compatible.

**The honest position.** wasm32 is the one row in document 04 where "a new target is a rule set and four data files" is false, and saying so is more useful than pretending. It is a second backend shape sharing the frontend and most of the middle end. It stays at tier 2 for wasip1 and freestanding, and the effort number for it is published separately so that it does not contaminate claim 3 of document 02.

## 5.10 Instruction selection at eight architectures

Here is the cost that scales badly and it is not the encoders. Parent document 10 makes selection a rule set that `rucc-verify` discharges with an SMT solver, and parent document 00 calls that the mechanism that closes the largest historical source of miscompilation. It is right, and **the per-rule specification burden is linear in targets**.

The 2025 to 2026 literature says what to do about it. Document 01.9's summary, applied:

**Arrival's technique is the one to adopt.** Generating ISLE specifications from vendor-validated ASL semantics rather than writing them by hand is what turns verification from a per-target tax into a property of the target description. AArch64 has ASL. RISC-V has Sail, and the Sail model is the official one. x86-64 has several formalizations of varying quality. That covers three of our eight.

**For the rest, the fallback is honest and stated.** A target whose rules are not solver-discharged is marked as such in `--print-config` and in the support table, and its rules are validated by differential execution against GCC on the generated corpus instead. That is weaker evidence and it is labelled as weaker evidence, which is the whole of the discipline parent document 16 asks for about performance numbers, applied to correctness.

**What must not happen** is that the verification requirement is quietly dropped when target four arrives because writing the specs got tedious. Either the specs are generated, or the target is labelled. Document 16 keeps this open with the measurement attached: at target four, how many engineer-days went into rule specifications, and what fraction of the rule set is discharged.

## 5.11 A summary of the deltas

| target | new object format | new ABI | new selection difficulty | new assembler difficulty |
|---|---|---|---|---|
| aarch64 | Mach-O | AAPCS64, Apple | constant materialization |, |
| riscv64 |, | LP64D | no flags, no addressing | **linker relaxation** |
| i686 |, | i386 SysV, x87 stack | register pressure, PIC base |, |
| armv7 |, | AAPCS32 × 3 float ABIs | Thumb-2 dual encoding | **constant pools with range** |
| loongarch64 |, | LP64D |, |, |
| ppc64le |, | ELFv2, TOC | dual entry points |, |
| s390x |, | s390x ELF | **big endian, everywhere** |, |
| windows (any arch) | COFF | Win64/ARM64 |, | `.pdata`/`.xdata` constrains the prologue |
| wasm32 | wasm | wasm | **relooping; not a rule set** |, |

Three cells in that table are the real work: linker relaxation on RISC-V, big-endianness on s390x, and wasm being a different shape. Everything else is volume.
