# Performance: methodology and reporting

Two different numbers are called "compiler performance" and confusing them is the most common failure in this area. **Compile throughput** is how fast `rucc` runs; **code quality** is how fast its output runs. Axis 2 and axis 3 in document 02 make separate claims about them, they trade off against each other, and they need separate methodology.

This document also exists to constrain what we are allowed to say. A benchmark number without its methodology is marketing, and the project's credibility on axis 1 is not separable from its honesty on axes 2 and 3.

## 16.1 Reporting rules

These apply to the README, the release notes, the dashboard, and any talk or post.

1. **State the comparison exactly.** Which GCC, which Clang, which version, which flags, which host, which target, which libc. "Faster than GCC" is not a claim, it is a mood.
2. **Report the distribution, not the best run.** Median of at least ten runs, with the interquartile range. Never a minimum, and never a single run.
3. **Report the whole suite, including the losses.** A geometric mean plus the per-benchmark table. If we are 40% behind on one benchmark, that number appears in the same table as the wins.
4. **State what is excluded and why.** If a benchmark does not build, it is listed as not building, not omitted silently.
5. **Never compare across machines**, and never compare a number measured today against one measured on different hardware six months ago.
6. **Any claim in a public artifact must be reproducible by a documented command.** `cargo xtask bench --suite=X --compare=gcc-15` and a machine description.

Document 02's specific promise, competitive on scalar integer and pointer code, explicitly *not* at parity on vectorized floating point, must be restated wherever numbers appear, because a reader who sees a SPEC-style geometric mean without it will infer a claim we did not make.

## 16.2 Compile throughput

**What is measured:** wall time from process start to output written, including everything: process startup, argument parsing, header reading, and the linker invocation reported separately.

**The workloads:**

- **SQLite amalgamation**, single translation unit, ~250k lines. The single most useful number in the project, because it is one big file with no build system noise, it is what everyone benchmarks, and it is directly comparable to published numbers for other compilers.
- **The kernel's `defconfig` build**, `-j1` and `-j$(nproc)`. Thousands of small files, which measures per-invocation overhead and header processing, a completely different profile from SQLite, and the one where the header cache from document 05 either pays for itself or does not.
- **A header-heavy microbenchmark**: a file that includes `<stdio.h>`, `<stdlib.h>`, `<string.h>` and does nothing. This measures the floor, and on a real build the floor is a surprisingly large fraction of the total.
- **Postgres**, as a mid-size realistic build with a configure step.
- **Pathological inputs**: a 100k-line function, deeply nested expressions, a switch with 10k cases, heavy macro expansion. These catch superlinear behavior, which is the failure mode that turns a fast compiler into an unusable one on somebody's generated code.

**Both cold and warm cache**, reported separately, because a cold-cache number on a build machine is the number that matters to CI and a warm-cache number is the one that matters to an interactive edit-compile loop.

**Peak RSS is reported alongside every time**, because trading memory for speed is easy and mostly wrong. A compiler that needs 8 GB per process cannot run at `-j16`, so memory is a first-class result and not a footnote.

**Attribution.** `-ftime-report` gives a per-phase breakdown, and the CI benchmark records it, so a regression comes with the phase it is in. Below that, `perf` and `samply` profiles are collected for the two main workloads on every nightly and stored, so a regression that appears over a month of commits can be attributed without re-bisecting by hand.

## 16.3 Code quality

**The suites:**

- **SPEC CPU 2017**, integer subset, where a license is available. It is the standard and it is what everyone else's numbers are on. The floating-point subset is run and reported, and we expect to lose on it; reporting it anyway is the point of rule 3.
- **The [LLVM test-suite](https://github.com/llvm/llvm-test-suite)**, which is freely available, includes a broad set of application-like benchmarks, and has correctness checking built in, so it is simultaneously a code-quality suite and an execution test.
- **Project-native benchmarks**: SQLite's `speedtest1`, Postgres's `pgbench`, zlib compression throughput, and FFmpeg encode timings. These are the numbers a user of those projects would actually care about, which makes them more meaningful than any synthetic suite and less comparable to other compilers' published results. Both properties are worth having.
- **Microbenchmark sets** for specific optimizations, where the point is diagnosis rather than a headline number: does this loop get unrolled, does this call get inlined, does this bounds check get eliminated.
- **Code size** on the whole corpus at `-Os` and `-Oz`, which is a code-quality axis in its own right and matters enormously for embedded and kernel users.

**The methodology:** pinned CPU frequency with turbo disabled, isolated cores, ASLR disabled for the measurement runs, ten runs, median and IQR reported. Statically linked against the same libc for every compiler under test, so we are measuring code generation and not somebody's `memcpy`. The same source, the same `-O2` (or the documented nearest equivalent), and the flags recorded verbatim in the output.

**Instruction counts alongside cycle counts.** Cycle counts are what matter and are noisy; instruction counts are stable and diagnostic. A regression that moves instructions but not cycles is usually uninteresting; one that moves cycles but not instructions is a memory or branch-prediction effect and is usually the interesting kind.

## 16.4 The tradeoff, made explicit

Axis 2 and axis 3 pull against each other and the resolution is document 09's per-level pipelines. The budget:

| Level | Throughput target | Code quality target |
|---|---|---|
| `-O0` | 2x `clang -O0` | no target; must be correct and debuggable |
| `-O1` | ~1.2x `clang -O1` | within 25% of `gcc -O2` |
| `-O2` | 1.5x `clang -O2` | within 10% of `gcc -O2` on scalar |
| `-Os` | ~`-O2` | within 10% of `gcc -Os` on size |

The `-O0` number is the one with the most leverage, because most compilations in the world are `-O0` or `-O1` (every CI run, every incremental developer build, every configure test) and because it is the level where our design decisions (SSA construction without mem2reg per document 08.5, the single-pass backend per document 10.3, the header cache per document 05) were specifically made to pay off. If we do not win at `-O0`, those decisions did not earn their complexity, and that is a finding worth publishing too.

## 16.5 Tracking

Every nightly writes a row: commit, host, suite, benchmark, metric, value, IQR. The dashboard plots them and CI flags any move outside a threshold. Thresholds are per-benchmark and derived from that benchmark's own measured noise, not a global percentage, because a benchmark with 5% run-to-run variance and one with 0.3% need different gates and a single global threshold makes one of them useless.

Regressions block merges. The escape hatch is that the PR states the tradeoff explicitly: "this costs 2% on `-O2` compile time and fixes a miscompilation" is an acceptable and, on axis 1, an obviously correct trade. The point of the gate is not to prevent regressions but to prevent *unnoticed* ones.

## 16.6 Where the performance actually comes from

For the record, and so that optimization effort goes to the right place. Compile throughput, in rough order of leverage: not allocating (arenas, interning, no `String` past the lexer, per document 03); not doing work twice (the header cache, the multiple-include optimization); doing work in parallel (per-file and per-function, both levels); and only then micro-optimizing the hot loops (the SIMD lexer scan, the 16-byte token). The ordering matters because the last item is the fun one and the first item is where the time is.

Code quality, in rough order: inlining, then alias analysis quality, then the scalar pipeline as a whole, then register allocation, then instruction selection, then scheduling and layout. This ordering is why document 09 spends its length on the first three and document 10 spends comparatively little on the last two.
