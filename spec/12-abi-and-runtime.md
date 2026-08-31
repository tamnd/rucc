# ABIs and the runtime libraries

An ABI bug is invisible until the moment you link against code someone else compiled, and then it is a wrong answer with no diagnostic. This is the part of a compiler where "mostly right" has no value at all, so this document is a list of specific obligations rather than a design discussion.

## 12.1 The ABI description

An ABI is data, held in `rucc-target` as a description consumed by document 10's lowering:

- integer, floating-point and vector argument register sequences, and the rules for exhausting them
- return value registers and the threshold and mechanism for returning through memory
- the classification algorithm mapping a C type to a sequence of argument slots
- stack argument area layout: slot size, alignment, direction, and who pops it
- caller-saved and callee-saved register sets, and the reserved registers
- red zone size, stack alignment at a call boundary
- struct and union layout rules, including bit-field allocation
- `va_list`'s representation and the `va_arg` algorithm
- TLS models and their access sequences
- name mangling, which for C means the leading-underscore question and little else
- the sizes and alignments of every fundamental type, and `long double`'s format

Five of these are implemented for 1.0: SysV AMD64, AAPCS64, Apple's arm64 variant, Windows x64, and the RISC-V LP64D psABI.

## 12.2 SysV AMD64

The classification algorithm is the hard part and it is genuinely intricate: an aggregate of at most sixteen bytes is split into eight-byte chunks, each chunk classified as INTEGER, SSE or MEMORY by a recursive walk over the fields with a merge rule, and any MEMORY result forces the whole thing to memory. The cases that catch people: a struct containing both an `int` and a `float` in one eight-byte chunk classifies as INTEGER, so the float travels in a general register; unaligned fields force MEMORY; a struct larger than sixteen bytes always goes to memory; `__int128` occupies two INTEGER chunks that must be in consecutive registers.

`long double` is x87 80-bit, stored in sixteen bytes with six bytes of padding, returned in `st(0)`. This is the reason x87 cannot be dropped from the x86-64 backend.

Varargs: `%al` holds the number of vector registers used, which variadic callees read to decide whether to save the SSE register area. `va_list` is the four-field struct with `gp_offset`, `fp_offset`, `overflow_arg_area` and `reg_save_area`, and `va_arg` is the corresponding branch on offset versus threshold. Getting the register save area's layout wrong produces garbage in the seventh argument onward and nowhere else.

The 128-byte red zone below `%rsp` is usable in leaf functions in userspace and must be disabled with `-mno-red-zone` in kernel code, because signal and interrupt handlers clobber it.

## 12.3 AAPCS64 and Apple arm64

AAPCS64 is cleaner: x0 to x7 for integers, v0 to v7 for floating point and vectors, everything above sixteen bytes indirect except homogeneous floating-point aggregates of up to four members, which travel in consecutive vector registers. `long double` is IEEE binary128, in software.

**Apple diverges, and the divergences are the bugs.** Arguments on the stack are packed at their natural size rather than promoted to eight-byte slots, so a stack `char` argument occupies one byte. Variadic arguments do not use the register sequence at all (they all go on the stack) which makes `va_list` a plain `char*` and makes a variadic call ABI-incompatible with a non-variadic one, so a function declared without a prototype and called variadically is a real failure. `long double` is `double`. And on Apple platforms x18 is reserved by the OS and must never be allocated.

These are per-target-triple facts in the ABI description, not `#ifdef`s in the lowering code.

## 12.4 Windows x64

Four argument registers only (rcx, rdx, r8, r9) with integer and floating-point positions shared, so a `(int, double, int, double)` call uses rcx, xmm1, r8, xmm3. Anything not exactly 1, 2, 4 or 8 bytes is passed by hidden reference to a caller-allocated copy. A 32-byte shadow space is allocated by the caller for the callee to spill those four registers into, always, even when the callee has no parameters. `long double` is `double`. Variadic floating-point arguments are duplicated into the corresponding integer register.

Unwinding is table-based through `.pdata`/`.xdata`, which constrains the prologue: it must consist only of instructions the unwind opcodes can describe, in a canonical order.

## 12.5 RISC-V

LP64D: a0 to a7 for integers, fa0 to fa7 for floating point, with a struct of two floating-point members passed in two FP registers and a struct of one integer and one float passed in one of each, a rule with no analogue in the other ABIs. Aggregates up to two registers wide go in registers, larger ones by reference. `long double` is binary128 passed by reference. The ILP32 and soft-float variants exist in the description but are not 1.0 targets.

## 12.6 Struct layout and bit-fields

Layout is per-ABI but the C-level rules are shared: members in declaration order, each at the next offset satisfying its alignment, struct alignment the maximum of its members', size rounded up to that alignment. `_Alignas`, `__attribute__((aligned))`, `__attribute__((packed))` and `#pragma pack` modify it; the interaction of `packed` with an aligned member is a place where GCC and Clang have historically differed and where we follow GCC, per document 04's compatibility contract.

**Bit-fields are the worst-specified part of the C ABI.** The allocation unit, whether a zero-width field forces alignment, whether a field may straddle a storage unit boundary, whether the declared type affects the containing object's alignment, and how a bit-field is *accessed*, the width of the load or store the compiler emits, all vary. The last one has a correctness consequence beyond layout: C11 introduced the memory model rule that adjacent non-bit-field members are separate memory locations, so a bit-field store must not write bytes belonging to a neighbouring non-bit-field member. Compilers had this wrong for years and it produces data races in correct code.

Our rule: follow the target psABI for layout, follow GCC for access width, and never widen a store past the end of the bit-field's allocation unit. Validated by a generated test suite that emits several thousand structs with randomized field types and widths, prints every offset and width via `offsetof` and `sizeof` under both compilers, and diffs, the same technique GCC and Clang use against each other, and the only way to get this right.

## 12.7 TLS

Four models: global-dynamic, local-dynamic, initial-exec, local-exec, selected by `-ftls-model=` and by visibility, with the linker permitted to relax a general model to a more specific one when it can prove the symbol is in the executable. The relaxations require emitting the exact instruction sequences the linker recognizes, byte for byte. A semantically equivalent but differently spelled sequence silently fails to relax, or worse, gets relaxed incorrectly.

`__thread` and C23's `thread_local` are the same thing. The kernel uses no TLS; userspace uses it everywhere.

## 12.8 The builtins library

Every compiler needs a support library for operations the target cannot do in one instruction. Ours is `rucc-builtins`, the equivalent of compiler-rt's builtins or libgcc, and we ship it because depending on the platform's is a portability dependency we said we would not have.

Contents: 64-bit and 128-bit division and modulo on targets lacking them; `__int128` arithmetic everywhere; software floating point for `f128` on all targets and for `f16` conversions; float-to-integer and integer-to-float conversions the hardware does not provide; the `__sync` and `__atomic` library calls for atomics wider than the target's atomic instructions; `memcpy`/`memset`/`memmove`/`memcmp` for freestanding targets; and the unwinder's personality-routine support.

It is written in Rust with `#![no_std]`, compiled by us for the target, and, importantly, **it is ABI-compatible with libgcc's and compiler-rt's**, meaning identical symbol names and calling conventions, so an object we produce links against a libgcc-based program and vice versa. The soft-float paths are differentially tested against a reference implementation over randomized inputs including the whole hazard list: subnormals, NaN payloads, signed zeros, the rounding-mode boundary cases, and the ties-to-even cases at every exponent.

`-fno-builtins-lib` suppresses linking it, for people who want libgcc.

## 12.9 Sanitizer runtimes

The instrumentation is in the compiler; the runtime is a library. We implement:

**UBSan.** The check set from document 07's UB table: signed overflow, shift amount, null and misaligned pointers, out-of-bounds array indices with a known bound, `bool` and enum range, division by zero, invalid float-to-integer conversion, `__builtin_unreachable` reached, and the two novel ones, `-fsanitize=alias` for effective-type violations and `-fsanitize=restrict` for `restrict` contract violations. These two are the interesting contribution: the aliasing and `restrict` rules are the UBs that most often produce a "the compiler broke my correct code" bug report, and neither GCC nor Clang has a reliable dynamic checker for them. The runtime is a shadow-memory scheme recording the effective type of each byte and the base pointer of each `restrict`-qualified access, and it will be slow. Slow and correct is fine for a debugging tool.

**ASan.** Shadow memory at the standard scale and offset so that our instrumentation is compatible with the existing runtime, redzones around globals and stack objects, quarantine on free. We implement the instrumentation and can either ship a runtime or interoperate with LLVM's; interoperating first is the cheaper path and is what M8 does.

**MSan** requires instrumenting every load and every store and propagating shadow through arithmetic, and it only works if *all* linked code is instrumented, which makes it much more expensive to get right. Post-1.0.

**`-fsanitize=cfi`** and the fine-grained forward-edge checks are post-1.0. `-fstack-protector`, `-fstack-clash-protection`, `-fcf-protection` and `-mbranch-protection` are not sanitizers and are in for 1.0, since production builds and the kernel use them.

## 12.10 Validating the ABI

Three mechanisms, in increasing order of strength.

**Struct layout diffing**, as in 12.6, generated and compared against GCC per target.

**Call ABI differential testing.** Generate a function with a randomized parameter list drawn from the full type space (every scalar type, structs and unions with randomized members, arrays, `__int128`, `long double`, vectors, and variadic tails) compile the caller with `rucc` and the callee with GCC, then the reverse, and check that every parameter arrives with the value it was sent. This is the only test that finds classification bugs, and it finds them immediately. It runs per target under QEMU for the cross targets.

**Linking against the world.** Building SQLite with `rucc` and linking it into a GCC-built program, and vice versa, is a weaker but broader test that the corpus in document 15 performs continuously. An ABI bug that survives the first two mechanisms will surface here as a mysterious failure in a real program, which is exactly the outcome the first two exist to prevent.
