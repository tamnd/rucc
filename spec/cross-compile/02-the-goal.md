# The goal

Five claims, each a number or a command whose exit status decides it. Parent document 02 states four axes for the compiler; these are the fifth axis stated at the same standard, because "portability" in that document means three hosts and three targets, and this document means something much larger.

## 2.1 The claims

**Claim 1, Parity with `zig cc` on reach.**

> For every target triple that `zig cc` will compile and link a hosted C program for, `rucc --target=` does the same, from every host `zig cc` runs on, out of one distributed binary, with no external toolchain, no sysroot flag, and no second download.

Falsified by: a script that enumerates `zig targets`, filters to the ones with libc support, and for each one compiles and links a fixed 400-line C program that touches stdio, string, math, malloc, threads, time and errno. Any row where zig succeeds and rucc does not is a failure. Published as a table with a count, on every release, including the count of rows where both fail.

The bar is deliberately zig's list and not GCC's, because zig's list is finite, has been validated by users, and is the list somebody could actually check us against.

**Claim 2, The generated code does not get worse to make this possible.**

> On x86-64 Linux, the code-quality and compile-throughput numbers of parent document 02 axes 2 and 3 do not regress by more than 2% as a result of any change made for this specification.

Falsified by: the existing benchmark, run on the commit before and after each target-model or driver change, with the 2% measured as median of ten with the IQR published as document 16 of the parent requires.

This is the claim that can kill the whole document, and it is stated second rather than last for that reason. The failure mode is real and specific: a target model that turns compile-time constants into run-time field loads, an ABI classification that becomes a virtual call, a predefined-macro table built from data instead of baked in. Each of those is a plausible way to lose a few percent, and a few percent taken four times is the throughput claim.

**Claim 3, A new target is data plus a rule set, and the number is published.**

> Bringing up target N takes fewer engineer-days than target N−1 for every N ≥ 4, and the per-target line count outside `rucc-target` and the target's rule set is zero.

Falsified by: `cargo xtask layers` gaining a check that no crate above rank 1 contains a match on `Arch`, `Os` or `Env`, plus a published table of dates and line counts per target. Parent document 10.8 already claims this and M10 already promises the number for target four; this document extends the promise to every subsequent target and adds the mechanical check that makes it hard to cheat.

**Claim 4, Every claimed target executes.**

> No target is listed as supported unless rung 0 and rung 1 of parent document 14 pass on it, and the report says for each target whether that was on hardware or under emulation.

Falsified by: the support table itself, which has a column for it and no "builds" state. A target that compiles hello world and has never run SQLite's test suite is tier 3, and tier 3 is spelled "not supported" in the README.

**Claim 5, The result is reproducible and the closed pieces are named.**

> Two `rucc --target=T a.c -o a.out` invocations on different hosts, for the same T and the same input, produce byte-identical output. Every input that is not in the rucc distribution is named in `rucc --print-sysroot-provenance`, with its source, its hash and its licence.

Falsified by: a CI job that cross compiles rung 1 for every tier-1 and tier-2 target from all three hosts and diffs. Parent document 03's determinism requirement already demands this within a host; the cross case is where it is actually hard, because it is where host paths, host header versions and host time leak in.

## 2.2 What each claim is protecting against

Claim 1 protects against a support table full of half-targets. The temptation in this work is enormous: an ELF writer plus an instruction encoder gets you an `.o` for a new architecture in a week, and the distance from there to a program that runs is most of the work.

Claim 2 protects against the thing that actually happened to GCC and Clang. Neither of them set out to be slow. They became slow by accumulating generality, one defensible increment at a time, and by the time it was visible the generality was load-bearing. rucc's throughput claim is its reason to exist, and a cross-compilation programme is exactly the kind of pervasive generality that eats it.

Claim 3 protects against the abstraction being a claim rather than a fact. Every compiler says its backend is retargetable. The check is whether target four cost less than target three.

Claim 4 protects against the failure that this class of tool is famous for: the binary is produced, it is wrong, and nobody finds out until it is on a device. See the `_STAT_VER` bug in document 01.

Claim 5 protects against the supply-chain question, which for a tool that ships somebody else's libc headers is not academic.

## 2.3 What "supported" means, precisely

Three words are used in this specification and they are not synonyms.

**Runs on.** rucc's own binary executes on this host. Determined by whether we build and test a release artifact for it. Today: x86-64 Linux, x86-64 macOS, aarch64 macOS, x86-64 Windows. Adding a host is a CI runner and a release job, and is nearly free compared to adding a target.

**Targets.** rucc emits objects for this triple. Requires an architecture backend, an ABI, an object format, relocations, and the target's fundamental type facts.

**Compiles for.** rucc produces a *linked, running program* for this triple with no external toolchain. Requires all of "targets", plus headers, plus link-time libc, plus start files, plus a runtime library built for it, plus a linker that emits that format.

The three sets are nested and the gaps between them are where every misleading support claim in this industry lives. The support table in document 04 has a column for each.

## 2.4 The comparison set, and why each is not the answer

| | reach | self-contained | optimizing | our objection |
|---|---|---|---|---|
| `zig cc` | ~40 triples | yes | yes (LLVM) | carries 100 MB+ of LLVM; that is the whole of rucc's thesis about why compilers are slow |
| cross-gcc packages | very wide | no | yes | one binary per target pair; nothing is self-contained; a distro is required |
| clang + sysroot | wide | no | yes | still finds a GCC installation; still needs somebody to build the sysroot |
| `cargo-zigbuild`, `hermetic_cc_toolchain` | zig's | yes | yes | wrappers around `zig cc`; same objection |
| `cargo-xwin` | Windows only | fetches | yes | solves one platform, and correctly; document 13 copies it |
| TCC | several | partly | no | no optimizer; parent document 00 already declines this trade |
| chibicc, cproc, slimcc, kefir | one to three | no | no | portable *on*, not *for*; and none optimizes |
| Cosmopolitan / APE | one binary, many OSes | yes | via clang | a different idea, one *output* everywhere, not one compiler for everywhere |

The empty cell in that table is the row rucc wants: wide reach, self-contained, optimizing, small. Nothing occupies it. That is the argument for doing this, and it is the same shape as parent document 00's argument for the compiler itself.

## 2.5 Non-goals, stated so they stay non-goals

**Every GCC target.** No ia64, no vax, no pdp11, no m68k, no SPARC, no HPPA, no Alpha, no SuperH, no Blackfin, no the other thirty. Zig's own attempt at the tail, "basic support for Alpha, KVX, MicroBlaze, OpenRISC, PA-RISC and SuperH, requiring GCC or an external LLVM fork", is a demonstration of what the tail costs even with LLVM underneath.

**C++, and therefore libc++, libstdc++ and the Itanium C++ ABI.** Parent document 14.6. The cross-compilation case for C++ is much harder than for C and it is not our compiler.

**The MSVC *dialect*.** We target the Windows x64 and ARM64 ABIs and link against the MSVC CRT when the user provides it, and we do not implement `__declspec`, SEH's `__try`/`__except`, or the MSVC preprocessor's divergences. Parent document 14.6 already says this; nothing here changes it. On Windows the default environment for a cross build is mingw-w64, as it is for zig.

**GPU offload.** No AMDGCN, no NVPTX, no SPIR-V, no OpenMP target regions, no OpenACC. GCC 16 does substantial work here; it is a different compiler.

**A populated third-party sysroot.** We ship a libc and the compiler's own runtime. We do not ship OpenSSL, zlib or curl for thirty targets and we do not become a distribution. Kelley's framing is right and we adopt it: for a large dependency tree you want a full cross environment, and this tool is for when you do not.

**CHERI.** Out of scope for 1.0. Document 03 records what the target model would need (a pointer that is not an integer of pointer width, and an address space in the data layout) so that the model does not actively preclude it, and stops there.

**A JIT, and therefore in-process cross execution.** Parent document 10.

## 2.6 The order these get delivered in

The claims are not equally reachable and the order is chosen so that the expensive risk is retired early.

1. **The target model** (document 03), because everything else reads it and changing it later is a whole-workspace edit. Cheap, and blocking.
2. **musl on the existing architectures**, because musl needs no stub generation and no version node, so it tests the entire sysroot pipeline with the libc problem at its easiest.
3. **The ABI differential harness** (document 14), before the fourth ABI rather than after, on document 01.8's evidence that it finds bugs immediately.
4. **glibc stubs** (document 09), the largest single piece of the sysroot work, and the one with the most ways to be subtly wrong.
5. **The remaining architectures**, in the order document 04's tiers give.
6. **Darwin and Windows**, last among the hosted platforms, because both are gated on a fetch step and a legal boundary rather than on compiler work.

Document 15 puts dates and milestones on this.

## 2.7 The condition under which this document is abandoned

If claim 2 fails, if the throughput or code-quality numbers regress past 2% and the regression is structural rather than a bug, then the target model is rolled back to the three-field triple, the cross work is limited to the three targets parent document 17 already plans, and this specification is closed with the measurement attached.

That is stated here, at the front, because a specification that cannot be abandoned is a wish. rucc's reason to exist is that most compilations in the world are `-O0` and `-O1` and that is the case GCC and Clang optimize least. Reaching thirty targets by becoming the thing we are arguing against would be a strange way to win.
