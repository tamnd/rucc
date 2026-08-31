# The target ladder

The ladder is the project's spine. Each rung is a real project, chosen because compiling it demands something the previous rung did not, and each has an exit criterion that is a command whose exit status is either zero or not. There is no partial credit and no "compiles except for one file".

The rungs map onto the milestones in document 17; this document defines what each rung *requires*, which is the part that determines the work.

## 14.0 The rule

For every rung, at every level, the criterion is the same four-part conjunction:

1. **It builds** with `CC=rucc` and the project's own unmodified build system, with **no source patches**.
2. **Its own test suite passes**, at the same level as a GCC build passes it, not "passes the tests it passed last week".
3. **It passes at `-O0`, `-O1`, `-O2`, `-Os` and with `-flto`**, and under `-fsanitize=undefined` where the project is clean under GCC's UBSan.
4. **It is byte-identical across two runs** of the same build, per document 03's determinism requirement.

A rung is not climbed until all four hold on all three host platforms for the native target and under QEMU for the cross targets.

## 14.1 Rung 0: the bootstrap corpus

Before any real project, a set of small programs that exercise the pipeline end to end: the [c-testsuite](https://github.com/c-testsuite/c-testsuite), chibicc's and TCC's test files, a subset of GCC's `gcc.c-torture` execution tests, and our own.

This rung exists to make the compiler *usable* before it is capable. Its exit criterion is 100% of c-testsuite and the torture execution tests that do not require unimplemented extensions, with the exclusion list checked in and shrinking. An exclusion added without a matching issue number is a CI failure, which is the mechanism that keeps the list from becoming a place to hide.

**What it demands:** the whole pipeline, and nothing large.

## 14.2 Rung 1: SQLite

[SQLite](https://sqlite.org) is the right first real target: the amalgamation is one 250k-line translation unit of clean, portable, heavily tested C, its test suite is exceptionally thorough, and it has essentially no build system complexity to fight.

**What it demands:** correctness at scale, and compile throughput on a single enormous function-dense file. It uses a moderate set of GNU extensions and a lot of computed control flow. SQLite's bytecode interpreter is a large switch that will exercise document 10's jump-table lowering and document 09's block layout harder than any synthetic test. It also has a large body of `assert()`-heavy code that only behaves correctly if the optimizer respects side effects precisely.

**Exit criterion:** the amalgamation builds; `make test` passes fully at every optimization level; the TH3 or the public test harness reports zero failures; and `speedtest1` runs within the code-quality bound of document 02 against `gcc -O2`. The performance number here is the first real data point on axis 2 and it is a good one, because SQLite is scalar integer and pointer code, which is exactly what our axis claims to be competitive on.

## 14.3 Rung 2: the mid tier

A breadth rung. No single project here is as demanding as SQLite in any one dimension, but collectively they cover the extension surface that the two hard rungs above assume.

- **zlib** and **libpng**: old, portable C with unusual idioms and a lot of pointer arithmetic.
- **musl libc**: a freestanding-adjacent build, inline assembly, `weak_alias` everywhere, and it produces the libc we then compile other things against, which makes it a self-checking test.
- **Lua**: computed goto, `setjmp`/`longjmp`, aggressive macro use, and its own excellent test suite.
- **BusyBox** and **toybox**: thousands of small files, heavy `-Os` pressure, and a lot of `__attribute__((section))`.
- **git** and **curl**: realistic build systems, autoconf feature probing (which is where a compiler's *diagnostics* become semantically load-bearing, because configure tests decide features based on whether a compile fails), and dependency chains.
- **FFmpeg** and **OpenSSL**: hand-written assembly in volume, SIMD intrinsics, `-mavx2` and equivalents, and function multi-versioning. These are what force document 13's intrinsic headers to be real.
- **CPython**: a large C codebase with computed goto in its interpreter loop, plus a test suite that finds floating-point and edge-case bugs.

**What it demands:** the GNU extension matrix, the intrinsic headers, inline assembly at volume, autoconf-grade diagnostic fidelity, and `-Os`.

**Exit criterion:** all of them build and pass their own suites, at all levels, on all hosts. This rung is also where document 15's corpus becomes continuous rather than milestone-gated: once a project is on the ladder it is built on every commit.

## 14.4 Rung 3: PostgreSQL

[PostgreSQL](https://www.postgresql.org) is a different kind of hard from SQLite. It is roughly 1.5 million lines across a real build with generated sources, it uses a wide slice of GNU C, its regression suite is enormous and behavioral, and, the part that matters, it is *sensitive to codegen quality* in a way that shows up as measurable throughput differences.

**What it demands:** everything from rungs 1 and 2, plus `dlopen`-based extension loading (so our shared objects must be correct, including visibility and the symbol table), a working `configure` against a system libc, correct `long double` and floating-point behavior across a large numeric surface, and enough optimizer quality that `pgbench` numbers are not embarrassing.

Postgres also has a JIT path built on LLVM. We do not need to support that (it is a build option, and it is not our JIT) but the build must handle it being disabled cleanly.

**Exit criterion:** `make check-world` passes; `pgbench` in a fixed configuration is within the axis-2 bound of a `gcc -O2` build; an extension built with GCC loads into a `rucc`-built server and vice versa, which is the strongest available ABI test outside document 12's dedicated one.

## 14.5 Rung 4: the Linux kernel

The operational definition from document 02, restated here as the criterion:

**Level A: `defconfig` boots.** `make CC=rucc defconfig && make CC=rucc` on x86-64 produces a kernel that boots to a shell under QEMU. No patches to the tree. `objtool` runs on our objects and reports no warnings.

**Level B: `allmodconfig` builds.** Every module in the tree compiles. This is the real breadth test and it is where the long tail of extensions, attributes and inline assembly gets found; the difference in surface area between `defconfig` and `allmodconfig` is most of the kernel.

**Level C: it is a real kernel.** Boots on all three architectures. Passes a kernel selftest run and an LTP run at parity with a GCC-built kernel. Survives a `make -j` build of a userspace under itself. Builds with the hardening options on: KASAN, stack protector, `CONFIG_RETPOLINE`, `CONFIG_UNWINDER_ORC`, `CONFIG_LTO` if we get there.

**What it demands** that nothing below it does:

- The full `-f`/`-m` flag set from document 13.7, working rather than accepted.
- `asm goto` with outputs, and inline assembly used as a code-generation mechanism: the alternatives infrastructure, static keys, static calls, and paravirt patching all build tables of instruction addresses at assembly time and rewrite the text at boot. Our assembler's section and symbol handling has to be exactly right or the machine faults during boot with no diagnostic.
- **ORC unwinding via `objtool`.** `objtool` decodes every instruction in every object, validates control flow, and generates ORC data. It is, in effect, a third-party static verifier of our code generation, and it will reject constructs GCC never emits. This is a gift disguised as an obstacle: `objtool` failures are early warnings of real codegen problems, and getting them to zero is worth more than the unwind data itself.
- Correct handling of a tree with per-file flag overrides, per-directory flags, generated headers, a two-pass link, linker scripts of real complexity, and `--emit-relocs`.
- Behavior under `-fno-strict-aliasing` and `-fno-delete-null-pointer-checks`, which the kernel sets globally.

**What "no source patches" means and why it is non-negotiable.** The kernel already supports Clang, so the tree contains compiler abstraction layers, and the honest position is that we build under the *existing* GCC path, which means being GCC-compatible enough that `compiler-gcc.h` is correct about us. If we find a genuine kernel bug we report it upstream like anyone else; we do not carry a patch queue, because a patch queue turns "we compile the kernel" into a claim nobody can verify.

## 14.6 What the ladder deliberately excludes

**C++.** Not at any rung, not after 1.0. It is a different compiler.

**Anything requiring a JIT**, per document 10.

**Chromium, LLVM, GCC itself**: all substantially C++.

**Windows kernel-mode code and the MSVC extension dialect.** We target the Windows *host* and the Windows x64 ABI for userspace; `__declspec`, SEH's `__try`/`__except`, and the MSVC preprocessor's divergences are out of scope for 1.0 and are recorded as such in document 19.

## 14.7 Why this order

Each rung's failures are diagnosable because the rung below it works. When a Postgres build fails, we know it is not a basic codegen bug, because SQLite passes on every commit. That property is the entire value of a ladder over a wish list, and it is why a rung is never skipped even when the next one looks more interesting.

The corollary is a rule about regressions: **a rung, once climbed, is CI.** Rung 1 runs on every commit, rung 2 on every commit once reached, rung 3 nightly, rung 4 nightly at level A and weekly at levels B and C. A commit that breaks a climbed rung is reverted, not fixed forward, because the alternative is a ladder that is only ever true on the day it was measured.
