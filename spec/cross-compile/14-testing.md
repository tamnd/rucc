# Testing thirty targets

Document 02 claim 4 says no target is listed as supported unless rung 0 and rung 1 execute on it. That is the whole discipline, and this document is how it is afforded. The constraint is not cleverness, it is CI minutes: naively running the parent's rung ladder on thirty targets is not affordable, so the testing has to be stratified by what each layer actually catches.

## 14.1 The layers, cheapest first

| layer | catches | cost | run |
|---|---|---|---|
| 1. tuple and driver golden files | flag parsing, link argv, search paths | ~0 | every commit, all targets |
| 2. rule-set verification (SMT) | miscompiling lowering rules | seconds to minutes | every commit, per target |
| 3. `_Static_assert` ABI corpus vs GCC | type sizes, alignments, offsets, bitfields | seconds | every commit, all targets |
| 4. stub-vs-real-libc symbol diff | wrong versions, wrong sizes, missing symbols | seconds | every commit, per arch |
| 5. assemble-and-disassemble round trip | encoder bugs | seconds | every commit |
| 6. **differential ABI execution** | calling convention bugs | minutes | every commit, tier 1; nightly, tier 2 |
| 7. rung 0 under qemu-user | codegen correctness | minutes | every commit, tier 1; nightly, tier 2 |
| 8. rung 1 (SQLite) under qemu-user | real-program correctness | tens of minutes | nightly |
| 9. rung 0 + 1 on real hardware | **the qemu gap; §14.5** | hours, scarce | per release |
| 10. rungs 2 to 4 | scale | large | per release, tier 1 only |

Layers 1 to 5 are the ones that scale to thirty targets, and they catch a genuinely large fraction of the bugs this specification can introduce, every wrong offset, every missing symbol, every bad version node, every malformed link line. They need no target machine and no emulator. **The scaling strategy is to push as much as possible down into layers 1 to 5.**

## 14.2 The static layers, in detail

**Golden link lines.** For each target, a recorded `argv` for a fixed set of invocations (`-c`, static, shared, pie, `-nostdlib`, sanitizer). A diff is a review item. This catches the entire class of document 11.3 failures without linking anything.

**The `_Static_assert` corpus** (document 06.8 mechanism 1) is the highest value-per-second test in the whole matrix. Generated types, nested structs, unions, arrays, bitfields of every width, `_Alignas`, `long double`, `__int128`, asserted for size, alignment and every member offset. Compiled by rucc for target T and by a cross-GCC for target T. Both succeed or the difference is a bug. It needs a cross-GCC to exist for T, which for the Linux targets it does in Debian's archive.

**The stub diff** (document 09.8) validates our libc description against a real distribution `libc.so` per architecture. It is the only test that finds "we claim `foo@GLIBC_2.34` exists on s390x and it does not" without running on s390x.

**Round-tripping the encoder** through a third-party disassembler (`llvm-objdump`, or the vendor's) catches encoding bugs at the instruction level and localizes them, which execution testing does not, an execution failure on a new architecture is a needle in the whole backend.

## 14.3 Differential ABI testing

Document 01.8's result is the argument: ABI Cafe found GCC, Clang and rustc disagreeing on x86-64 Linux, the most exercised ABI that exists. "We implemented the psABI document" is not evidence.

The harness: generate a function signature from the type grammar; emit a caller in one compiler and a callee in the other; each side checks every argument's value and the return; run; report. Both directions, because a caller bug and a callee bug are different bugs. Reference implementations are GCC and Clang for the target.

Scheduled **before the fourth ABI**, per document 02.6, so that it is available when the ABI count starts growing rather than being retrofitted after the bugs have shipped.

Its coverage is the thing static assertions cannot reach: register assignment, stack layout, the indirect-passing threshold, variadic handling, and struct return, items 4 to 6 of document 06.2, which is where Darwin arm64's variadic divergence and Windows' shadow space live.

## 14.4 Emulation

qemu-user with `binfmt_misc` makes cross execution nearly free in CI, and it is what makes claim 4 affordable for targets we have no hardware for. It is also, per document 01.10, not the same thing as the target.

Where qemu is known to diverge: unimplemented or approximated instructions on newer extensions; memory ordering, because qemu-user does not faithfully model weak memory and will hide races that real AArch64, ppc64 and RISC-V hardware expose; floating-point corner cases and denormal/rounding-mode handling; `/proc` and syscall coverage; timing-dependent behaviour; and self-modifying code.

**The consequence is a policy, not a caveat.** Every target's support statement records whether its evidence is hardware or emulation, and a **known-qemu-divergence list** is maintained per target, every case where a test was skipped, adjusted or expected to differ under emulation, with the reason. That list is the honest measure of how much the emulated evidence is worth, and it is published. A target whose divergence list is long is a target whose tier claim should be re-read.

Concurrency and atomics testing in particular is *not* considered validated under qemu-user, and targets whose only evidence is emulated say so.

**The first entry in that list is not about an instruction, it is that qemu drops programs on the floor.** Running the differential harness of section 14.10 on an arm64 macOS developer machine, where the x86-64 container runs under qemu, the program segfaults at random about once in every one hundred and thirty runs. Seven times in nine hundred for the build where both halves are the reference compiler and there is no disagreement in it to find, and at the same rate for the builds with rucc on one side or both. It is not the compiler and it is not the corpus, and the only reason anybody knows that is that the harness runs a build whose answer is known in advance. Any harness that runs under emulation and treats a crash as a finding will send somebody after a bug that is not there, so this one tries a crash again and believes a wrong value the first time.

## 14.5 Hardware

Real hardware is scarce, so it is spent where emulation is weakest, memory ordering, atomics, floating point, and the platform's actual dynamic linker.

Available cheaply: x86-64 and aarch64 Linux runners; macOS runners for both architectures; Windows x86-64 runners; RISC-V hardware is now purchasable and CI providers are beginning to offer it. Available through community infrastructure: s390x and ppc64le, and the s390x host job of document 04.4 depends on it. Not available: LoongArch outside China, per document 04.6, permanently a qemu-only target unless that changes, and the table says so.

Per release, tier-1 targets run rungs 0 and 1 on hardware. That is the cadence claim 4 needs, and it is affordable.

## 14.6 What is generated

Csmith and YARPGen for random C programs, differentially executed against GCC and Clang for the same target. On a new backend these find bugs immediately and keep finding them for a long time, and they are the cheapest source of new test cases per engineer-hour.

The cross-specific value is that a *random program cross compiled and executed under qemu* tests the whole pipeline at once, headers, ABI, codegen, runtime, link, and a failure is bisectable by the layers above. Reduction (C-Reduce or cvise) is part of the harness, not a manual step, because an unreduced random-program failure is not actionable.

Generated ABI type corpora (§14.2, §14.3) come from the same generator infrastructure, which is why building it once pays three times.

## 14.7 The parity script

Document 02 claim 1's falsifier is itself a test and it belongs in CI. Enumerate zig's libc-supporting targets, compile and link a fixed hosted C program for each with both `zig cc` and `rucc`, tabulate. Published per release with the count, including the rows where both fail. It is the single number that says how far this specification has got, and it is not a number anyone can argue about.

## 14.8 The budget

Per commit: layers 1 to 5 for all targets, layers 6 to 7 for tier 1. Target under twenty minutes; these are mostly compile-only or emulator-seconds.

Nightly: layers 6 to 8 for tiers 1 and 2, plus generated-program runs. Hours, off the critical path.

Per release: layer 9 on hardware for tier 1, layer 10, the reproducibility diff of claim 5 across three hosts, the parity script, and regeneration checks for the header tree and abilist blob (document 13.6).

If that budget is exceeded, the thing that gives is target *count* in the nightly emulated tiers, not the per-target depth. A shallow claim about many targets is what document 02 exists to prevent; fewer targets tested properly is the correct failure mode.

## 14.9 The record layout corpus, and what its first run found

Section 14.2 asks for the `_Static_assert` corpus and this is it, built as `cargo xtask abi-corpus` and checked in under `tests/abi-corpus`. One C file per target, a few hundred records, and an assertion for every size, every alignment and every ordinary member's offset. The shapes are the same in every file, half of them written by hand for the cases section 14.2 names and half drawn from a fixed seed, so a diff between two targets' files is a list of the layout decisions those two targets make differently and nothing else.

Every number in it comes from `rucc_types::layout_record`, the function the compiler calls when it parses a `struct`, rather than from a second description written for the corpus. A corpus that checks a copy against a reference says nothing about the compiler, which is section 4.7's rule one level down.

**It covers all forty two rows, and the gap it used to have was the first thing it found.** Laying a record out needed a `TargetInfo`, and that type held a three field triple with three architectures in it while `rucc-abi` described the scalar layout of all forty two rows, so the compiler knew what a `long double` is on s390x and could not be asked what a struct containing one looks like. The generator refused to write a file for a row it could not reach rather than borrowing a neighbour's numbers, and it printed the count on every run so that the gap was a number that goes down. `TargetInfo` holds the tuple now and `TargetInfo::for_tuple` builds one for any row, so there are forty two files and no count. Thirty nine of them compile against the pinned Zig 0.16, and the other three are `arm64ec-windows-msvc`, `wasm32-wasip3` and `x86_64-illumos`, which zig has no spelling for and which the driver in tamnd/rucc-cross names and counts rather than compiling against a neighbour.

**`__int128` is the one thing the files differ about declaring.** i686, both ARM rows and RISC-V 32 do not have the type, so the two shapes that name it are simply absent there, and the type is kept out of the seeded half of the grammar entirely. That is what keeps every `gen_NN` declaration identical across all forty two files, which is the property that makes a diff between two of them a list of layout decisions rather than a list of which types exist. The two hand written shapes are declared past the point where a shape may be nested, so leaving them out cannot move an index the seeded half refers to.

**Widening it to the whole table found one more disagreement, and it was `long double` on wasm32.** That target has a sixteen byte IEEE quad aligned to sixteen, and `rucc-abi` grouped it with the thirty two bit machines and gave it a `double`. `facts/wasm32-none.facts` in tamnd/rucc-cross had recorded `long_double_format=quad` and `sizeof_long_double=16` from the day the facts were taken, so the fact was on disk and the rule contradicted it, which is the failure mode a corpus catches and a fact file on its own does not. The assumption underneath was that a machine with four byte pointers has nothing wider than a `double` in it, and that is the same assumption `__int128` on that same target had already falsified, so this is the second half of a mistake whose first half the layout corpus in tamnd/rucc-cross found when an assertion there tied having the type to having eight byte pointers. RISC-V 32 has the quad too, measured by running the reference rather than assumed from the family, and it is not a row of the table so nothing here would have caught it.

**Four disagreements with the reference, and all four are fixed.** Compiled with the pinned Zig 0.16 in tamnd/rucc-cross, eight of the fifteen files compiled clean on the first run and seven did not. Per section 6.9 the reference is the ABI. What the failures have in common is the shape of the whole problem: rucc had one record layout algorithm and these targets need two of them and two facts besides. All fifteen files compile clean now.

**A zero width bit-field contributes its type's alignment under AAPCS64 and nowhere else.** `struct { unsigned :0; }` has an alignment of four there and an alignment of one on x86-64 SysV, on RISC-V, on Apple's AArch64 and on Windows on AArch64. So it is not an architecture rule and it is not an operating system rule: it is AAPCS64 proper, and Apple and Microsoft both departed from it in the same direction. rucc said one everywhere. `TargetInfo::unnamed_bit_field_aligns` carries it now, and the rule is wider than the zero width case that found it: an unnamed bit-field of any width raises the record's alignment there, so `struct { char c; unsigned :20; }` is four bytes aligned to four under AAPCS64 and four aligned to one on x86-64.

**Windows lays bit-fields out by Microsoft's rule, on mingw as well as on MSVC.** A run of bit-fields is allocated into a unit the size and the alignment of the declared type, and the unit is closed both when the next member's declared type has a different size and when the field does not fit in what is left. An ordinary member closes one too, and the closed unit costs its whole declared size whether or not the bits were used. So `struct { unsigned m0:3; char m1; }` puts `m1` at offset four and is eight bytes where every other target puts it at offset one and is four. This is what GCC calls `-mms-bitfields` and it is the default on every Windows target, so a compiler that follows Itanium there produces structs that do not match the headers it just included. `TargetInfo::bit_field_style` carries it and `layout_record` runs one algorithm or the other. The `union` is the corner worth naming: Microsoft gives a union's bit-field its storage and no say in the alignment, so `union { unsigned m:3; char c; }` is four bytes aligned to one, which is an alignment smaller than either member has on its own.

**A record with no storage in it is four bytes under MSVC and nothing anywhere else.** This is the one that looked like a question rather than an answer, and it turned out to be an answer. An empty struct is not C, MSVC rejects the declaration outright with C2016, so the number cannot be measured against MSVC and has to come from whatever else compiles C for the target. On mingw that is GCC and the answer is zero. On MSVC it is clang and the answer is four with an alignment of one, and it composes: a member of that type occupies four bytes and an array of three occupies twelve. So it is a fact about the environment rather than about the operating system, which is the one place in `TargetInfo` where those two come apart in that direction, and it covers the record with no members, the one holding nothing but a zero width bit-field and the one holding nothing but a flexible array member. All four shapes were measured and all four agree.

**A trailing zero width bit-field's padding belongs to the record.** `struct { char c; int :0; }` is four bytes and rucc said one. The corpus did not find this one, because every shape in it with a zero width member had another member after it, and that member ends further along than the padding does, so the mistake had nothing to show. It came out of writing the unit test for the rule above, which is worth recording as the limit of what a corpus of this shape catches. `bits_trailing_zero_width` is in the corpus now.

The first three are facts about a target that `TargetInfo` did not carry and the fourth is arithmetic. That is why the corpus had to exist before the fix could be written: the diff of the fix is readable as exactly the set of numbers that moved. Five shapes went in with it, because the first version of the corpus was wrong about Windows in only two places and it should have been wrong in a dozen. The two bit-field rules agree on everything built out of one declared type, and almost everything hand written in the corpus was built out of one declared type, so the shapes that mix them are there deliberately now rather than by luck of the seed.

**One corner has two references and they disagree.** mingw and MSVC do not answer the same way about what `packed` does to a Microsoft bit-field: a packed struct of an `unsigned m0:3` and a `char m1` is eight bytes under `x86_64-windows-gnu` and five under `x86_64-windows-msvc`. Under MSVC packing lowers the alignment a unit is opened at and does nothing else, so the unit still costs its whole four bytes and the `char` still starts after it, and that is the rule `layout_record` implements and the rule the six packed cases measured against MSVC agree with. mingw ignores the packing for the unit entirely: the `char` lands at offset four either way, but the record keeps an alignment of four rather than dropping to one, so the five bytes round up to eight. rucc gives MSVC's answer on both rows and is therefore wrong on mingw by three bytes on that struct. The corpus holds no packed records so nothing in it asserts either number, and this is written down rather than fixed because the fix needs a decision about which reference wins when the two for one operating system point in different directions, which section 6.9 does not make for this case.

## 14.10 The signature corpus, and what its first run found

Section 14.3 asks for the differential harness and this is the first half of it, built as `cargo xtask abi-signatures` and checked in under `tests/abi-signatures`. One program in four files: `callee.c` defines fifty eight functions and each definition checks every parameter against the value the caller promised and returns the value the caller expects, `caller.c` calls them all and checks what came back, `abi.h` is the aggregate types and the prototypes and is all the two sides share, and `report.c` holds the failure counter and the one `fprintf`.

`report.c` is separate because rucc has no built-in system include directories, so a file under test that said `#include <stdio.h>` would be testing whichever headers the machine happened to have rather than a calling convention. It is always built by the reference compiler, and the two sides reach it through a function taking two `const char *` and returning `void`, which is the one signature every ABI in the table agrees about.

The values are generated rather than written, which is what section 14.3 asks for. Every scalar in the program gets its own number, so a value that arrives in the wrong place is a value that belongs to some other parameter rather than a coincidence, and a failure names the function, the parameter and the member. The ranges are picked so that every value is exact in every type it can be written in: nothing that has to fit a `long` goes past thirty two bits, because a `long` is four bytes under LLP64, and every floating point value is a small number plus a quarter.

Ten signatures are written by hand and forty eight are drawn from a fixed seed. The hand written ten are the cases somebody named: ten integers, ten doubles and twelve of them alternating, all past the end of every argument register file here, the aggregates that fit in registers, the homogeneous floating point ones that AAPCS64 puts in vector registers and SysV packs into SSE eightbytes, the twenty four byte one that is past every threshold, a large aggregate returned through the hidden pointer that moves every other argument along, a function returning `void` with the registers full, a `long double` between two integers, and the nested struct and the union that a classification rule has to flatten before it can decide anything. The drawn forty eight are for the orders nobody thinks to write down.

**It runs four builds, and two of them are controls.** Both sides by the reference compiler, both sides by rucc, and one each way round. The two mixed builds are the point and the two matching ones are what make a failure readable, because a corpus that is wrong about C fails all four and saying that in one line is better than four paragraphs about registers.

**The first run found nothing about rucc and something about the apparatus.** All four builds agree on every value, which is the answer x86-64 System V should give and is worth having as a number rather than as an assumption. What the run also found is the emulation crash rate recorded in section 14.4, and it found it because the all-reference control crashes at the same rate as everything else. A harness without that control would have reported an intermittent rucc bug that does not exist.

**Variadic signatures are the next piece and they are not here yet.** They are where Darwin arm64 and Windows diverge from everyone else, they need `rucc_lower::abi::plan` to adopt `Call::variadic_argument`, and they are the half of item 6 of document 06.2 that this corpus does not reach. `__int128` is out for now too, for the same reason it is out of the seeded half of the record corpus: three rows of the table do not have the type, and one source that serves every target is worth more than a corpus that needs a preprocessor conditional to say which types exist.

**One target runs it, and that is the honest scope today.** The corpus is the same C for all forty two rows and one of them has a back end, so what section 14.1 layer 6 covers right now is x86-64 System V. The rest of the matrix is waiting on the emulation layer and on the back ends, and the corpus is written so that widening it is a matter of pointing it at another target rather than writing another corpus.
