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
