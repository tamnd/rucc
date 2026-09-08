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

**It covers fifteen of the forty two rows, and that is the first thing it found.** Laying a record out needs a `TargetInfo` and that type holds a three field triple with three architectures in it, while `rucc-abi` describes the scalar layout of all forty two rows. So the compiler knows what a `long double` is on s390x and cannot be asked what a struct containing one looks like. The generator refuses to write a file for a row it cannot reach rather than borrowing a neighbour's numbers, and it prints the count of skipped rows on every run so that the gap is a number that goes down rather than a paragraph somebody has to remember.

**Four disagreements with the reference, and all four are fixed.** Compiled with the pinned Zig 0.16 in tamnd/rucc-cross, eight of the fifteen files compiled clean on the first run and seven did not. Per section 6.9 the reference is the ABI. What the failures have in common is the shape of the whole problem: rucc had one record layout algorithm and these targets need two of them and two facts besides. All fifteen files compile clean now.

**A zero width bit-field contributes its type's alignment under AAPCS64 and nowhere else.** `struct { unsigned :0; }` has an alignment of four there and an alignment of one on x86-64 SysV, on RISC-V, on Apple's AArch64 and on Windows on AArch64. So it is not an architecture rule and it is not an operating system rule: it is AAPCS64 proper, and Apple and Microsoft both departed from it in the same direction. rucc said one everywhere. `TargetInfo::unnamed_bit_field_aligns` carries it now, and the rule is wider than the zero width case that found it: an unnamed bit-field of any width raises the record's alignment there, so `struct { char c; unsigned :20; }` is four bytes aligned to four under AAPCS64 and four aligned to one on x86-64.

**Windows lays bit-fields out by Microsoft's rule, on mingw as well as on MSVC.** A run of bit-fields is allocated into a unit the size and the alignment of the declared type, and the unit is closed both when the next member's declared type has a different size and when the field does not fit in what is left. An ordinary member closes one too, and the closed unit costs its whole declared size whether or not the bits were used. So `struct { unsigned m0:3; char m1; }` puts `m1` at offset four and is eight bytes where every other target puts it at offset one and is four. This is what GCC calls `-mms-bitfields` and it is the default on every Windows target, so a compiler that follows Itanium there produces structs that do not match the headers it just included. `TargetInfo::bit_field_style` carries it and `layout_record` runs one algorithm or the other. The `union` is the corner worth naming: Microsoft gives a union's bit-field its storage and no say in the alignment, so `union { unsigned m:3; char c; }` is four bytes aligned to one, which is an alignment smaller than either member has on its own.

**A record with no storage in it is four bytes under MSVC and nothing anywhere else.** This is the one that looked like a question rather than an answer, and it turned out to be an answer. An empty struct is not C, MSVC rejects the declaration outright with C2016, so the number cannot be measured against MSVC and has to come from whatever else compiles C for the target. On mingw that is GCC and the answer is zero. On MSVC it is clang and the answer is four with an alignment of one, and it composes: a member of that type occupies four bytes and an array of three occupies twelve. So it is a fact about the environment rather than about the operating system, which is the one place in `TargetInfo` where those two come apart in that direction, and it covers the record with no members, the one holding nothing but a zero width bit-field and the one holding nothing but a flexible array member. All four shapes were measured and all four agree.

**A trailing zero width bit-field's padding belongs to the record.** `struct { char c; int :0; }` is four bytes and rucc said one. The corpus did not find this one, because every shape in it with a zero width member had another member after it, and that member ends further along than the padding does, so the mistake had nothing to show. It came out of writing the unit test for the rule above, which is worth recording as the limit of what a corpus of this shape catches. `bits_trailing_zero_width` is in the corpus now.

The first three are facts about a target that `TargetInfo` did not carry and the fourth is arithmetic. That is why the corpus had to exist before the fix could be written: the diff of the fix is readable as exactly the set of numbers that moved. Five shapes went in with it, because the first version of the corpus was wrong about Windows in only two places and it should have been wrong in a dozen. The two bit-field rules agree on everything built out of one declared type, and almost everything hand written in the corpus was built out of one declared type, so the shapes that mix them are there deliberately now rather than by luck of the seed.

**One corner has two references and they disagree.** mingw and MSVC do not answer the same way about what `packed` does to a Microsoft bit-field: a packed struct of an `unsigned m0:3` and a `char m1` is eight bytes under `x86_64-windows-gnu` and five under `x86_64-windows-msvc`. Under MSVC packing lowers the alignment a unit is opened at and does nothing else, so the unit still costs its whole four bytes and the `char` still starts after it, and that is the rule `layout_record` implements and the rule the six packed cases measured against MSVC agree with. mingw ignores the packing for the unit entirely: the `char` lands at offset four either way, but the record keeps an alignment of four rather than dropping to one, so the five bytes round up to eight. rucc gives MSVC's answer on both rows and is therefore wrong on mingw by three bytes on that struct. The corpus holds no packed records so nothing in it asserts either number, and this is written down rather than fixed because the fix needs a decision about which reference wins when the two for one operating system point in different directions, which section 6.9 does not make for this case.
