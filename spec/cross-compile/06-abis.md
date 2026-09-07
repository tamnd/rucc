# The ABIs

Parent document 12 describes five psABIs, SysV AMD64, AAPCS64, Darwin arm64, Windows x64, RISC-V LP64D, and does it well. This document is the part that only appears when there are thirty targets rather than five: what the *next* ten ABIs cost, which ones are traps, and how the ABI stops being code and becomes a described thing that can be checked.

The organizing claim: **an ABI is the only part of a target where being subtly wrong produces a program that works for months and then does not.** Everything else, such as a bad encoding or a missing relocation, fails loudly. This is why document 02 claim 4 exists and why document 14's differential harness is scheduled before the fourth ABI rather than after.

## 6.1 The full ABI list

| ABI | tuple spelling | status | the thing that bites |
|---|---|---|---|
| SysV AMD64 | `x86_64-*-linux-*`, BSD | done | eightbyte classification; `va_list` register save area |
| AAPCS64 | `aarch64-*-linux-*` | M6 | HFA/HVA; 16-byte indirect threshold |
| Darwin arm64 | `aarch64-apple-darwin` | M6 | **four divergences, §6.3** |
| Windows x64 | `x86_64-*-windows-*` | planned | shadow space; everything not 1/2/4/8 bytes is indirect |
| RISC-V LP64D | `riscv64-*` | M7 | the mixed int/float struct rule |
| i386 SysV | `i686-*-linux-*` | new | stack-only args; `long double` on the stack; struct return via hidden pointer with **callee pops** |
| i386 Windows | `i686-*-windows-*` | new | `__stdcall`/`__fastcall`/`__thiscall` are real and name-mangled |
| AAPCS32 soft | `arm*-*-*eabi` | new | doubles in register pairs, r1:r2 alignment |
| AAPCS32 hard | `arm*-*-*eabihf` | new | VFP argument registers; HFA rules differ from AAPCS64's |
| Windows ARM64 | `aarch64-*-windows-msvc` | new | AAPCS64 with different varargs and x18 = TEB |
| ARM64EC | `arm64ec-*-windows-msvc` | **no** | §6.6 |
| LoongArch LP64D | `loongarch64-*` | new | close to RISC-V; `$r21` reserved |
| ELFv2 (ppc64) | `powerpc64le-*`, `powerpc64-*bsd` | new | TOC, dual entry points, 32-byte parameter save area |
| s390x ELF | `s390x-*-linux-*` | new | big-endian; args in r2 to r6; register save area at +160 |
| wasm32 | `wasm32-*` | new | multivalue vs single return; three incompatible OS ABIs |

Fifteen. Parent document 12 has five of them. The count is the argument for §6.7.

## 6.2 What a psABI actually specifies

Enumerated because "implement the ABI" hides how many independent decisions it is, and because every one of these is a place a target can differ:

1. **Fundamental type sizes, alignments and signedness.** `char`'s signedness (unsigned on AArch64 Linux, ARM, ppc64le, s390x; signed on x86). `long`'s width (`DataModel`). `long double`'s width *and format*, which are separate, 16 bytes of which 10 are x87 on x86-64 Linux, 8 bytes of `double` on Darwin and Windows, IEEE binary128 on AArch64/s390x/RISC-V, and IBM double-double on legacy ppc64.
2. **Struct, union and array layout.** Usually derivable from member alignment, with exceptions that are not: i386's 4-byte `long long` alignment, ARM's historical 8-byte struct alignment cap, s390x's high-order-first bitfield allocation.
3. **Bit-field allocation.** Direction, whether a zero-width field forces alignment, whether a field may straddle a storage unit, and what the *access* width is, parent document 12 covers this and it is where C's rules and the ABI's rules meet badly.
4. **Argument classification.** The recursive part. Which arguments go in which registers, which go on the stack, which are passed by hidden reference, and how aggregates decompose.
5. **Return value convention**, including the hidden-pointer case and *who owns the pointer's register on return* (x86-64 returns it in `rax`; some ABIs do not).
6. **Variadic calls**, which on most targets are the same as non-variadic ones and on Darwin arm64 are not, and `va_list`'s concrete layout, a struct of two pointers and two counters on x86-64 SysV, a plain `char*` on Darwin, a five-field struct on AAPCS64, an array-of-struct on some.
7. **Stack alignment at the call boundary**, and the red zone (128 bytes on SysV, forbidden in kernels, absent on Windows).
8. **Callee-saved and caller-saved register sets**, plus the reserved registers: x18 on Darwin and Windows ARM64, `$r21` on LoongArch, `r2` on ppc64, `gp`/`tp` on RISC-V, `%ebx` under i386 PIC.
9. **The frame and unwinding contract**: frame pointer required or not, and how unwind information is expressed (DWARF CFI, or Windows `.pdata`/`.xdata`, which additionally *constrains the prologue's shape*).
10. **TLS models** and their relocation sequences, which differ per architecture more than any other item on this list.
11. **Symbol naming**: leading underscore on Darwin and i386 Windows, the `@N` suffix on i386 `__stdcall`, and none on ELF elsewhere.
12. **Function pointer representation**, which is ordinary everywhere we target, ppc64 ELFv1's function descriptors are the reason we do not target ELFv1.

The first three plus item 11 determine whether a *header* is interpreted correctly. Items 4 to 9 determine whether a *call* works. The differential harness in document 14 tests 4 to 6 directly; 1 to 3 are tested by `_Static_assert` corpora, which is cheaper and catches more.

## 6.3 Darwin arm64, restated as failure modes

Parent document 12.3 lists the divergences. Here they are as the bugs they become, because that is the form in which they will be encountered:

| divergence | the symptom |
|---|---|
| stack args packed at natural size | functions with >8 args return garbage in the 9th onward; only on Darwin |
| **variadic args always on the stack** | `printf("%d", x)` prints garbage; `va_list` is a `char*` not a struct |
| a variadic call is ABI-incompatible with a non-variadic one | calling an unprototyped function works until it does not; K&R declarations are a hazard |
| `long double` is `double` | `%Lf` disagrees; `LDBL_MAX` is wrong; math library calls resolve to the wrong symbol |
| x18 reserved | intermittent corruption under any OS callback; not reproducible under load |

The second row is the one that makes Darwin arm64 a *separate ABI* rather than AAPCS64 with notes, and it is why document 03 puts `Abi` in the tuple rather than deriving it from `(arch, os)`.

## 6.4 Windows x64, restated the same way

- **32 bytes of shadow space** allocated by the caller for the four register arguments, whether or not the callee uses them. Forgetting it corrupts the caller's frame.
- **Anything not exactly 1, 2, 4 or 8 bytes is passed by hidden reference** to a caller-allocated temporary. This is much simpler than SysV's classification and much easier to get *almost* right, because the common cases coincide.
- **Variadic floats go in both** the SSE register and the corresponding integer register.
- **No red zone.**
- `.pdata`/`.xdata` unwind information encodes the prologue as a sequence of unwind codes, and only a restricted set of prologue shapes is expressible. The prologue emitter is therefore constrained by the *exception* format, which is a coupling that does not exist on ELF.
- `long double` is `double`, and the leading-underscore rule differs between i386 and x64.

## 6.5 i386, the one that looks easy

Everything on the stack, 4-byte slots, `long long` in a pair. The traps:

- **Struct return uses a hidden pointer that the *callee* pops** on Linux/SysV, a 4-byte stack adjustment difference that desynchronizes the stack if only one side is wrong, and the wrongness is silent until several calls in.
- **`__stdcall`, `__fastcall`, `__thiscall` and `__cdecl` coexist on Windows**, differ in who pops, and are encoded in the symbol name (`_f@8`). Calling-convention attributes are therefore load-bearing on this target and ignorable on every other one we support.
- **Stack alignment is 16 bytes at the call boundary on modern Linux i386** and was 4 historically; libraries built either way exist.
- `long double` is 12 bytes on Linux i386 and 16 on x86-64, both 80 bits of content.

## 6.6 ARM64EC: declined, with the reason

ARM64EC is Microsoft's emulation-compatible ABI: AArch64 code that is call-compatible with x64, with two symbol namespaces, thunks between them, and ARM64X binaries containing both. It requires the linker to synthesize entry and exit thunks, a mangled second namespace, and relocation forms that exist nowhere else.

**Not in scope for 1.0.** The cost is a linker feature, not a backend feature, and rucc shells out to a linker before 1.0 anyway (parent README), so the decision is really "do we require a linker that supports ARM64EC". Recorded here rather than in document 02's non-goals because it is the *only* Windows ABI we decline, and someone will ask.

## 6.7 The ABI as a described thing

Fifteen ABIs of hand-written classification code is somewhere between six and ten thousand lines of the most bug-prone code in the compiler, and document 02 claim 3 says the per-target line count outside `rucc-target` and the rule set must be zero.

**The position.** Argument classification is expressed as a small declarative description per ABI, a table of type-shape predicates and their dispositions, interpreted by one shared classifier, rather than as fifteen recursive functions. Type layout is expressed the same way, as a data-layout description that the layout engine reads.

**The objection, taken seriously.** This is exactly the shape of change document 02 claim 2 warns about: a compile-time decision becoming a run-time table walk, on the hot path of every call site. The classifier runs once per call and once per function signature, not per instruction, so the exposure is bounded, but "bounded" is a prediction, not a measurement.

**Therefore:** the classifier description is *compiled*, not interpreted. The per-ABI description is the source of truth for the tests and the documentation, and codegen (a build-time step over the description, or `const` evaluation) produces the same monomorphic code a hand-written classifier would. The check is document 02's benchmark at the migration point, and the fallback if it fails is that the description remains the source of truth for tests and the classifier stays hand-written per ABI, which loses claim 3's "line count is zero" but keeps claim 2, and claim 2 outranks claim 3.

**What must not be a table.** Prologue and epilogue emission, register allocation constraints, and unwind emission. Those are per-architecture code with per-ABI parameters, and pretending otherwise produces an abstraction that is more expensive than the duplication.

## 6.8 How each ABI is validated

Four mechanisms, in ascending cost and ascending evidence:

1. **`_Static_assert` corpus.** Generated from the ABI description: for a few thousand generated types, assert `sizeof`, `_Alignof`, and every member offset. Compiled by rucc and by GCC/Clang for the same target; the compile either succeeds for both or the disagreement is the bug. Costs nothing to run, needs no execution, and catches every item 1 to 3 error in §6.2. **Do this first for every new ABI.**
2. **Differential ABI testing**, per document 14 and document 01.8's evidence. Generate function signatures, compile the caller with rucc and the callee with GCC and vice versa, link, run, compare. This is what catches items 4 to 6. ABI Cafe's result, that on x64 Linux, the most-exercised ABI in existence, all three of GCC, Clang and rustc disagreed on something, is the reason this is not optional and the reason "we implemented the document" is not evidence.
3. **Link rucc-compiled objects into a distribution's programs.** Parent document 12.10 already has this. It is the only mechanism that tests the ABI against code nobody wrote for the test.
4. **Rung 1 through rung 3 execution** on the target, which is document 02 claim 4.

Mechanism 1 is the one that scales to fifteen ABIs and it is the one to build first. Mechanisms 2 and 3 are per-target CI cost, which document 14 budgets.

## 6.9 The disagreement policy

When rucc and GCC disagree about an ABI on a target where a GCC has been running for twenty years, **GCC is the ABI** regardless of what the document says, and the divergence is filed upstream and recorded in a per-target list that ships with the compiler. When rucc and Clang disagree and GCC agrees with neither, that is a psABI bug and it gets reported. When the target is new enough that there is no incumbent, the written psABI wins.

This is not a moral position. It is that an ABI's purpose is that two independently compiled objects link, and a correct implementation of the document that does not link with the world is a worse compiler than an incorrect one that does.
