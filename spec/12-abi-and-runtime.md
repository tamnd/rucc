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

Varargs: `%al` holds the number of vector registers used, which variadic callees read to decide whether to save the SSE register area. `va_list` is the four-field struct with `gp_offset` at zero, `fp_offset` at four, `overflow_arg_area` at eight and `reg_save_area` at sixteen, and `va_arg` is the corresponding branch on offset versus threshold. The save area is one hundred and seventy six bytes, the six general purpose registers first at eight bytes each and the eight vector ones after them at sixteen, and the two offsets a `va_start` writes are past the registers the arguments the signature does name already took. Getting the register save area's layout wrong produces garbage in the seventh argument onward and nowhere else, and the test that finds it is handing a list built here to `vfprintf`, which was compiled by somebody else and reads it by this table.

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

It is compiled by us for the target and, importantly, **it is ABI-compatible with libgcc's and compiler-rt's**, meaning identical symbol names and calling conventions, so an object we produce links against a libgcc-based program and vice versa. The soft-float paths are differentially tested against a reference implementation over randomized inputs including the whole hazard list: subnormals, NaN payloads, signed zeros, the rounding-mode boundary cases, and the ties-to-even cases at every exponent.

**What it is written in, and the fact that this section used to say something else.** It is C, compiled by rucc itself for each target. This section said Rust with `#![no_std]`, `runtime/rucc-builtins` is that crate and is what exists today, and section 10.2 of the cross-compilation specification said C and cited this section for it. Two documents describing two different pieces of work is a decision nobody made, so tamnd/rucc#912 made it, and the answer is C for two reasons that are both about thirty targets rather than about taste.

Reach. The Rust path needs a Rust target for every row of `spec/cross-compile/04-target-matrix.md`, installed on whatever machine builds the archives, and that table has rows rustc does not have. C compiled by rucc reaches every target that has a back end, by construction, and that is the bootstrapping property document 10.2 argues for: every target's runtime is then evidence that the target's codegen works, and a target whose builtins do not build is a target that is not ready.

Size. The staticlib rustc produces carries Rust's own `compiler_builtins` along with it. `cargo xtask builtins --target x86_64-unknown-linux-gnu` wrote 4.5 MB that way, for a crate whose whole content is `memcpy`, `memmove`, `memset` and `memcmp`, and tamnd/rucc#912 measured 4.6 MB for `x86_64-unknown-linux-musl`. One target's archive was therefore about half of the 10 MB that `spec/cross-compile/13-distribution.md` section 13.1 budgets for every tier-1 and tier-2 archive together, before any of the routines that section asks for are written. The same task compiles the C now and writes 1546 bytes for the same four routines on the same target, which is what an archive of four byte loops weighs and is the half of this decision that stopped needing an argument as soon as it ran.

The Rust crate stays where it is and keeps its tests, as the reference implementation the C is differentially tested against, which is the reference the paragraph above asks for and never named. What the decision costs is the routines written in C and an archive writer for the objects rucc emits, which is tamnd/rucc#991. The second of those is done: `rucc-archive` writes the container and the symbol index for both the System V flavour and Microsoft's, deterministically in the sense §13.6 asks for, and `rucc-stub` writes its import libraries through it rather than through a copy of its own. `rucc --emit=archive` is how the compiler reaches it, writing one archive out of the objects a command line compiles, which is the line `cargo xtask builtins` calls and the reason the symbol index can be written at all: the names a member defines come from the object writer that just produced it rather than from a reader of the file. The first of the two has started: `runtime/builtins/mem.c` is the four block routines, `cargo xtask builtins --target <triple>` builds the compiler and compiles them with it, and an archive produced that way is therefore evidence about the target whose name is on it. The differential this section asks for is written for those four: `cargo xtask builtins-diff` compiles one harness twice, against the archive rucc wrote and against the Rust crate built as a static library, and holds the digest of every group of cases from one against the other, which for those four is 27456 cases over every length to 80 and the powers of two and their neighbours past it. Two processes rather than two sets of symbols in one, so nothing is renamed and the thing under test is the archive a person is handed. The script reads the undefined symbols of each program back afterwards and fails on any of the four names, because a distribution with `_FORTIFY_SOURCE` on by default turns `memcpy` in the harness into a call to glibc's `__memcpy_chk`, and the first run of this check passed against a deliberately broken `memcpy` for exactly that reason. What is not written is everything else in the list above, and the word at a time paths, which want a benchmark to hold them to rather than an opinion.

**The 128-bit division and modulo.** `runtime/builtins/div.c` is the second piece, and it is second because it is the one arithmetic a 64-bit target cannot do at all. `crates/rucc-codegen/src/wide.rs` splits a 128-bit add, shift or compare into instructions over the halves, and a quotient is not a function of the halves of its operands, so a divide that survives to codegen is a call whatever the optimizer made of it. The six names are libgcc's: `__udivti3`, `__umodti3`, `__udivmodti4`, `__divti3`, `__modti3` and `__divmodti4`. The C is a bit at a time, 128 rounds of shift, compare and subtract, with one shortcut for the pair that both fit in sixty four bits, which is the case almost every program with an `__int128` in it is in. The reference in `runtime/rucc-builtins/src/div.rs` is long division in base 2^32 with Knuth's normalization and his add-back, a different algorithm on purpose, because a reference that shares its reasoning with the thing it checks only catches typing mistakes. It also cannot be written the obvious way: a `/` on a 128-bit value is a call to `__udivti3`, which that crate defines, so every division in it is on a `u64` and the base is picked to keep it there.

The differential covers them as it covers the block routines, and the whole run is now 578632 cases in 429 groups. The division part is 551176 of those: random pairs whose widths are themselves random, because the implementations branch on how many digits an operand has and a stream of full width values would only ever ask about one of those paths; every pair of 41 values at the ends of the type and the digit boundaries of both implementations; and one less than the divisor, which is the pair that makes a long division estimate a quotient digit one too big and then have to take it back. That last family is there because of something the work measured: with the add back taken out of the reference by hand, every group of random pairs still agreed, and the only groups that noticed were the corner table and these. Knuth puts the case at about two in 2^32 for random operands, so it is a case to construct rather than one to sample. All six entry points are reached, four of them because gcc emits them for a `/` or a `%` and the two that hand back both answers at once because the harness calls them by name.

The guard against testing somebody else's routine is the other way round here, and reading undefined symbols would not have found it. gcc puts libgcc on every link line, libgcc is a static archive, so a link that resolved `__udivti3` there rather than in the archive under test has the routine inside the program rather than undefined outside it, and both sides would quietly agree about libgcc's answer. So the script reads what each archive defines and fails on any of the ten names that is not in there. Compiling the file at all is the other thing this measured: it is `unsigned __int128` parameters, a variable shift, a 128-bit compare and a loop, which is most of the wide pass's list used at once, and the archive rucc wrote has all six symbols in it. What is still not written is the faster division, which wants the same benchmark the word at a time block routines want.

**The code that calls them, and the first program at this width that anybody ran.** The routines landed before anything called them. `crates/rucc-codegen/src/wide.rs` listed the four divisions among the opcodes it does not understand, and one of them anywhere in a function left the whole function unsplit and refused by the selector by name, which is what every `__int128` division in the torture corpus had been getting since the pass was written. It rewrites them now, into a call whose operands are the halves already: four parameters of sixty four bits and two results, which is exactly what `split_signature` turns the routine's own definition into on the way in, so both ends of the call agree by construction rather than by a second description of the convention sitting somewhere else. Dividing by zero is not special cased, because it is undefined in C and a test in front of the call would be the compiler deciding what an undefined program does.

What runs it is `cargo xtask wide`, and it exists because of what the evidence for this pass looked like before it. The pass had unit tests that build a function, run the pass and read the IR back, which is the shape of check section 9.8 of the cross-compilation document warns about for the stub writer: the reader and the writer hold the same belief and agree with each other. No program compiled at this width had been run at all. So the task compiles `tests/wide/arithmetic.c` with the compiler this tree builds at `-O0`, `-O1` and `-O2`, compiles it again with gcc, runs all four programs and holds every group of cases from each of ours against the reference's. That is 260772 cases in 124 groups, and the groups are the operations rather than the divisions alone: adding, subtracting, multiplying, the three shifts at every count below the width, the twelve comparisons packed a bit each, dividing by a constant, the four divisions over pairs whose widths are random, and every pair of 41 values at the ends of the type and the boundaries between the halves. The answers are gcc's rather than a table written here, for the reason the differential above gives.

Three optimization levels because the one bug this pass has had was a level apart. tamnd/rucc#1054 was the splitting walking the blocks in the order the function holds them, which at `-O0` is the order they run in and above `-O0` is not, since a pass that gives a loop a preheader puts a block that runs early at the end of the list, so the same program compiled at `-O0` and refused at `-O1`. A harness at one level would not have known that. The guard is the other thing worth recording: every digest would still agree if a divide stopped being a call tomorrow and became arithmetic that happened to be right, so the script reads the undefined symbols of each object back and fails when any of the four names is not there, which is the same question the archive guard above asks from the other end.

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
