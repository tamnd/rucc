# Milestones

Twelve milestones, M0 through M11. Each has an exit criterion that is checkable rather than judged, and an effort estimate in engineer-months that is a guess with the uncertainty stated. The estimates sum to the 40 to 70 range document 00 gives; the spread is wider at the far end because the last three milestones are the ones whose difficulty is least knowable in advance.

The ordering has one governing property: **the compiler is end-to-end runnable from M3 onward.** Everything after that is making a working compiler better, which means every subsequent milestone can be tested against real programs, and no milestone is a large body of code that has never executed.

## M0: Skeleton (0.5 to 1 month)

The workspace from document 18, the layer rule enforced by `xtask`, the `Session` and diagnostic infrastructure from document 03, the CLI parser and driver phase graph from document 04, the target description scaffolding, and CI: build, clippy, format, and the determinism check, on all three hosts from day one.

Nothing compiles C. The point is that the scaffolding that everything else depends on exists before anything depends on it, and that the three-host requirement is a constraint from the first commit rather than a porting project later.

**Exit:** `rucc --print-config` works and is correct on Linux, macOS and Windows; CI is green on all three; `cargo xtask layers` passes.

## M1: Preprocessor and lexer (1.5 to 2.5 months)

Document 05 in full: the five translation phases, hide-set macro expansion, conditional evaluation, include resolution, `#pragma once` and the multiple-include optimization, `_Pragma`, `#embed`, and the `__has_*` family driven by an initial `features.toml`. `-E` output fidelity. The lexer's performance work: mmap, dispatch table, SIMD skipping, interning during scan.

Not the header cache; that is M5, once there is something to measure it against.

**Exit:** `-E` on the SQLite amalgamation and on every header in glibc and musl produces a token stream equal to GCC's after normalization; the throughput floor benchmark from document 16 is measured and recorded as the baseline.

## M2: Frontend (3 to 4 months)

Documents 06 and 07 and 08: the parser with the typedef disambiguation, the type system with interning and `_Atomic` as a real type, the full C23 semantic surface, the constant evaluator with software floating point, the typed AST, SSA construction by Braun's algorithm, the IR with its printer and parser, and **the verifier**.

This is the largest single milestone and the one where quality compounds most. A type system shortcut taken here is paid for in every subsequent milestone.

**Exit:** `--emit=tast` and `--emit=ir` on the whole of rung 0 and the SQLite amalgamation; IR round-trips byte-for-byte; the verifier passes on all of it; the accept/reject suite passes at every `-std=` level.

## M3: First code (2 to 3 months)

The x86-64 backend at `-O0` only: the initial ~150 lowering rules, the rule compiler `rucc-rules` and the verifier `rucc-verify` (both built here, because retrofitting verification is the thing document 10 says not to do), the single-pass register allocator, the integrated assembler's x86-64 encoder, ELF output, and the driver's link invocation.

**Exit:** `rucc hello.c -o hello && ./hello`. Then rung 0 of document 14 in full: c-testsuite at 100% modulo a checked-in exclusion list, and the GCC torture execution tests at whatever fraction extension coverage permits. Every lowering rule discharged by `rucc-verify`.

This is the milestone that proves the architecture. If the rule-based selection and its verification do not work in practice, we find out here, with three months invested rather than fifteen.

## M4: The optimizer (3 to 5 months)

Document 09: the pass manager with fuel and dumps, the ægraph and its extraction with GCM, the rewrite rule set, alias analysis, Memory SSA, the scalar pipeline, and the `-O1`/`-O2` level definitions. The backtracking register allocator and its checker. Scheduling and block layout.

**The ægraph experiment happens here**, and it is a real decision point: build the ægraph rewriter and a conventional apply-once pass pipeline over the same rule set, measure both on compile time and code quality, and take the winner. Document 19's open question one is answered at the end of this milestone, and the answer is written down whichever way it goes.

**Exit:** rung 0 passes at every optimization level; `-fpass-fuel` bisection demonstrably localizes an injected miscompilation; the first code-quality measurement against `gcc -O2` on the LLVM test-suite, published whatever it says.

## M5: SQLite (1.5 to 2.5 months)

Rung 1 of document 14, which in practice means chasing the specific extensions, corner cases and codegen bugs that a quarter-million lines of real C finds. Csmith and cvise come online here and run continuously from now on. The header cache from document 05 is built and measured, and kept only if it earns its complexity.

**Exit:** SQLite builds unpatched, `make test` passes at all levels, `speedtest1` is within document 02's bound, and the compile-throughput claim against `clang -O0` is measured on the amalgamation.

**This is the first sane stopping point.** What exists at the end of M5 is a correct, fast, optimizing C compiler for portable C on x86-64 Linux that compiles a serious real program and is fuzzed continuously. That is a genuinely useful artifact and a defensible place to stop, publish, and reconsider.

## M6: Second target, second host (2 to 3 months)

AArch64: the rule set, the encoder, AAPCS64 and Apple's divergences from document 12, and Mach-O output. macOS as a *host*, not just a target. Cross-compilation and QEMU-based execution testing in CI.

This is where the target abstraction is first tested by something other than assertion. Anything target-specific that leaked into a pipeline crate surfaces now, and fixing it now is much cheaper than at M10.

**Exit:** rungs 0 and 1 pass on aarch64-linux and on macOS arm64 natively; the CI matrix from document 15.7 reaches four of its six rows.

## M7: Breadth (3 to 4 months)

Document 13 in earnest: the `features.toml` matrix populated from MaskRay's kernel inventory and the GCC manual, the attribute and builtin sets, statement expressions, labels as values, the pragma set, and the intrinsic headers for x86 and NEON. Rung 2 of document 14: musl, Lua, zlib, BusyBox, git, curl, FFmpeg, OpenSSL, CPython.

This milestone is a grind with no intellectual content and it is the one most likely to take longer than estimated, because its size is set by other people's code rather than by our design.

**Exit:** all of rung 2 builds and passes its own suites at all levels on both targets; the matrix reports its coverage; `-fgnuc-version=` is raised past the 7.0.0 that M2 measured, to the highest version this document's matrix supports honestly.

## M8: Tools (2 to 3 months)

Debug information per document 11.4, to the standard that the differential GDB/LLDB testing enforces. Sanitizers per document 12.9, including the two novel ones. LTO, both monolithic and Thin. PGO. `-Os` and `-Oz`.

**Exit:** the DWARF differential passes at `-O0` and `-O2`; UBSan finds a planted bug of each kind in its table; `-flto` builds all of rungs 1 and 2 correctly; ASan interoperates with the existing runtime.

## M9: Third target, third host, Postgres (3 to 4 months)

RISC-V 64, which document 10 calls the middle-end canary. Windows as a host and target, with COFF, the Windows x64 ABI, and SEH unwind tables. Rung 3: PostgreSQL.

**Exit:** the full six-row CI matrix; `make check-world` passes; `pgbench` within bound; a GCC-built extension loads into a `rucc`-built server.

**This is the second sane stopping point**, and the strongest one. At the end of M9 the project has met three of its four axes on three hosts and three targets against two large real databases, with continuous fuzzing and verified rewrite rules. Whether to attempt the kernel from here is a decision made with far better information than we have now.

## M10: Hardening and the fourth target (2 to 4 months)

The security and hardening flags the kernel needs: stack protector, stack clash protection, CET, PAC and BTI, the retpoline thunk modes, `-fpatchable-function-entry`. `-mgeneral-regs-only` as a hard constraint. `-mcmodel=kernel` and `-mno-red-zone`.

And the abstraction test: **bring up a fourth target** (i686 or 32-bit ARM) and record the effort number. Either it is a rule set and four data files, as document 10.8 claims, or the number tells us what leaked.

**Exit:** every flag in document 13.7 works rather than being accepted, each with a test that would fail if it were ignored; the fourth target passes rungs 0 and 1; the effort number is published.

## M11: The kernel (4 to 8 months, widest uncertainty)

Rung 4. Level A first (`defconfig` boots on x86-64 under QEMU with `objtool` clean) then level B, `allmodconfig`, which is where the long tail lives, then level C across three architectures with selftests and LTP.

The estimate's spread is honest: this is the milestone where we have the least information, and the `objtool` interaction in particular is an unknown that could be a week or a quarter.

The linker measurement from document 11.6 is taken here, and open question two in document 19 is answered.

**Exit:** the three levels of document 14.5, with no source patches.

**This is the third stopping point and the stated goal.** Reaching level A is the headline; level C is what makes it true.

## Summary

| Milestone | What | Effort |
|---|---|---|
| M0 | Skeleton, CI on three hosts | 0.5 to 1 |
| M1 | Preprocessor and lexer | 1.5 to 2.5 |
| M2 | Frontend, IR, verifier | 3 to 4 |
| M3 | x86-64 `-O0`, rung 0 | 2 to 3 |
| M4 | Optimizer, second allocator | 3 to 5 |
| M5 | **SQLite, stopping point 1** | 1.5 to 2.5 |
| M6 | AArch64, macOS host | 2 to 3 |
| M7 | GNU breadth, rung 2 | 3 to 4 |
| M8 | Debug info, sanitizers, LTO | 2 to 3 |
| M9 | RISC-V, Windows, **Postgres, stopping point 2** | 3 to 4 |
| M10 | Hardening, fourth target | 2 to 4 |
| M11 | **Linux kernel, stopping point 3** | 4 to 8 |
| | **Total** | **28 to 46** |

The total is below document 00's 40 to 70 range and that is deliberate: the table counts the milestone work and not the continuous cost of everything running alongside it: triaging fuzzer findings, keeping climbed rungs green, chasing performance regressions, and the review overhead of a rule set with a verification obligation. That continuous cost is real, it grows with the project's size, and estimates that omit it are the reason compiler projects run long. The honest number is the range in document 00.

## What is not on this list

A stable IR interface, a JIT, C++, MSVC dialect support, an internal linker, MSan, polyhedral loop transformation, and machine learning in the heuristics. Each is discussed where it belongs and each is post-1.0 or out of scope; none is a milestone, because a milestone list that contains everything anyone might want is not a plan.
