# The goal, on four axes

The project brief was "very high performance". That phrase hides four different claims that trade against each other, and a specification that does not separate them produces a compiler that is mediocre at all four. This document separates them, puts a number on each, and says for each one whether the number is reachable and what it costs.

The rule for the whole document: every number here is falsifiable by the methodology in document 16, and any number that turns out to be wrong gets changed here rather than quietly dropped from the README.

## Axis 1: correctness

**The claim.** At every tagged release, zero known miscompilations, where "known" is defined by an apparatus that is actively hunting for them rather than by the absence of user reports.

The apparatus, specified in document 15: differential execution against GCC and Clang over a corpus of real projects and their own test suites; continuous Csmith and YARPGen fuzzing with automatic cvise reduction; SMT verification of every middle-end rewrite rule and every instruction selection rule; a self-build under ASan, UBSan and MSan; and an IR verifier that runs after every pass in debug builds and in CI.

**Is it reachable.** Yes, and this is the axis where we can credibly be better than everything else in the from-scratch category, because the tooling to do it now exists off the shelf and nobody in this category uses it. Csmith found 325+ bugs in GCC and LLVM; YARPGen found 220+ more. Those tools are sitting there. Crocus found a 9.9-severity bug in Cranelift's lowering rules by asking an SMT solver. That technique is published with an artifact.

The honest qualification is that "zero known miscompilations" is a statement about our search, not about the compiler. Csmith cannot break CompCert's verified core; it can break everything else, including us, given enough CPU time. What we are promising is that the search runs continuously, that findings are reduced automatically, and that we publish the pass rates rather than the pass/fail bit.

**What it costs.** Every ISLE lowering rule needs an SMT specification for each term it uses, written at the same time as the rule. That is a real tax. Crocus's authors found retrofitting annotations to an existing rule set to be the expensive path, and it is the reason document 10 forbids hand-written `match`-arm lowering from the first commit. It also costs a fleet: continuous fuzzing wants machines, and document 15 budgets for it.

**Why this is axis one.** Because a fast wrong compiler has no users, and because correctness is the only axis where a small project can beat a large one. GCC and Clang have thousands of engineer-years of optimization we cannot match. They also have decades of accumulated hand-written lowering code that nobody has ever proved correct. That asymmetry is the opening.

## Axis 2: generated code quality

**The claim.** At 1.0, within 10% of `gcc -O2` on integer and pointer-heavy scalar workloads, measured as geometric mean over the benchmark set in document 16.

**The explicit non-claim.** Not within 10% of `gcc -O3` or `clang -O3` on auto-vectorized floating point. We expect to be 2x to 4x behind on FP kernels that the big compilers vectorize well, and document 16 requires that number to be reported separately rather than folded into an average that hides it.

**Is it reachable.** The scalar claim is reachable but it is the longest grind in the project, and the shape of the curve is worth stating because it determines the milestone plan.

The reference points from document 01 give us the curve. TCC, with no IR and no optimizer, produces code running at roughly half of GCC's. Call it 50%. QBE, a small but real SSA backend, states a design goal of roughly 70% of LLVM. That is the shape: the first 70% comes from having an SSA IR at all plus about a dozen textbook passes, and it arrives fast. The move from 70% to 90% is forty more passes, each worth one to three percent, each needing a benchmark to justify it. There is no single trick in that range.

Concretely, the passes that buy the first 70% are mem2reg, SCCP, GVN, DCE, simplify-CFG, LICM, strength reduction, induction variable simplification, inlining, and a peephole set. The passes that buy the next 20% are memory-SSA-backed load/store elimination, partial redundancy elimination, alias analysis good enough to disambiguate real pointer code, tail duplication and jump threading, loop unrolling with a real cost model, if-conversion, machine-level scheduling for the target's pipeline, and a register allocator with live-range splitting. Document 09 and document 10 sequence these and document 17 puts the 90% target at M9, not M5.

The vectorization non-claim deserves its reasoning rather than an apology. Auto-vectorization is not one feature; it is a dependence analysis, a legality checker, a cost model, a set of idiom recognizers, and per-target knowledge of which of forty shuffle instructions is cheapest, and GCC and LLVM have each spent more than fifteen years on it. Minotaur's result is the tell: a superoptimizer applied on top of LLVM's already-mature autovectorizer still found 7.3% on GMP, which means even LLVM is leaving material on the table there. We will implement SLP vectorization and simple innermost-loop vectorization because they are worth having and because the kernel and SQLite barely care, and we will not pretend that gets us to parity on numerical code.

**What it costs.** It costs the compile-throughput axis, directly and unavoidably. An ægraph middle end, a memory SSA, a backtracking register allocator and a machine scheduler are all expensive. The resolution is that they are all off at `-O0`, which is why document 09 specifies genuinely separate pipelines rather than one pipeline with flags, the mistake the prior art in document 01 made and named as a limitation.

## Axis 3: compile throughput

**The claim.** 2x faster than `clang -O0` at `-O0`, and 1.5x faster than `clang -O2` at `-O2`. Preprocessed lines per second, single threaded, same machine, cold cache, measured per document 16.

**Is it reachable.** The `-O0` number, yes, and with room. TCC reaches 7 to 9x over `gcc -O0` by having no IR at all. We are not doing that, but the gap between TCC and Clang is mostly not the IR. It is that Clang's frontend carries a C++ compiler's data structures, its preprocessor allocates aggressively, and its `-O0` path still goes through the full LLVM IR construction and a real, if fast, instruction selector. A C-only frontend with interned identifiers, arena-allocated AST nodes, an index-based IR with 32-bit indices, and a single-pass linear scan allocator has a great deal of headroom against that. 2x is a conservative target and document 19 records that we may be underclaiming.

The `-O2` number is harder and is the one at real risk, because it is in direct tension with axis 2. Every pass we add to close the code-quality gap costs compile time. The mitigations are structural rather than heroic: the ægraph applies rewrites once at node creation rather than running a pass to fixpoint, which is cheaper than a traditional pass pipeline for the same result; function-level parallelism across cores is nearly free in Rust with `rayon` and the big compilers largely do not do it within a translation unit; and the IR layout is designed for cache behavior (structure-of-arrays, dense indices, no pointer chasing) which is worth more on a modern machine than any individual algorithmic improvement.

**Where the time actually goes.** In real C builds, a large fraction of total compile time is the preprocessor re-reading the same headers in every translation unit. Document 05 treats this as a first-class performance problem rather than a parsing detail, because a 30% win there is larger than anything available in the middle end.

**What it costs.** It costs architectural freedom. "Fast" here means the `-O0` path never allocates a `Box` per AST node, never builds a hash map keyed by a `String`, and never runs a pass whose output it does not use. Those constraints are cheap if they are in place from the first commit and expensive later, which is why document 03 fixes the data representation before any pass is written.

## Axis 4: portability

**The claim.** The compiler builds and passes its full test suite on Linux, macOS and Windows; on x86-64 and AArch64 hosts; and cross-compiles from any of those to x86-64, AArch64 and RISC-V 64. Object output covers ELF, Mach-O and COFF. Cross-compilation is not a second-class mode: the host and target are separate parameters everywhere and there is no `#[cfg(target_os)]` in the code generator.

**Is it reachable.** Yes, and it is close to free if it is a constraint from the first commit. Rust cross-compiles natively, the `object` crate already writes all three formats, and a compiler is a pure computation over bytes. What makes this expensive in existing compilers is decades of accumulated host assumptions, and we do not have those yet.

The interesting portability work is not the compiler, it is the ABI and platform detail per target: Apple's AArch64 varargs rules differ from AAPCS64, `long double` is 80-bit on x86 Linux and 128-bit quad on AArch64 Linux and plain `double` on Apple platforms, Windows x64 has its own calling convention and its own structure return rules, and bitfield layout differs between the Itanium-derived model and MSVC's. Document 12 enumerates these rather than discovering them one segfault at a time.

**Why this axis is in the top four.** Because "runs on any modern platform" was in the brief, because the prior art is Linux-only and untested elsewhere, and because a compiler that only runs on the machine its authors use is a research artifact.

## What "compiles the Linux kernel" is allowed to mean

The ultimate goal in the brief needs an operational definition, because "compiles the kernel" is claimed loosely by projects that mean very different things.

Our definition, and the M11 exit criterion in document 17: a `defconfig` and an `allmodconfig` build of a named stable kernel version, from a clean tree, with no source patches, on x86-64, AArch64 and RISC-V 64, producing an image that boots to a shell under QEMU and passes a named subset of the kernel selftests. `objtool` must accept the generated objects, which is a much stronger constraint than "it links": objtool validates stack frame correctness and generates ORC unwinder data, and it will reject code generation that is merely unusual.

"No source patches" is the part that does the work. Every project that has claimed this with patches has claimed something weaker, and document 14 holds the line.

## The tensions, stated explicitly

**Axis 2 against axis 3.** Every optimization costs compile time. Resolved by genuinely separate pipelines per `-O` level, and by the rule in document 16 that a pass which does not pay for itself on the benchmark set does not ship at that level.

**Axis 1 against everything.** Verification is a build-time and CI-time cost, not a compile-time cost: the rules are proved once when they are written, not when they are applied. This is the one tension that resolves cleanly, and it is the main argument for the rule-DSL architecture over hand-written lowering.

**Axis 4 against schedule.** Three hosts times three targets is nine configurations in CI from M1, and it will find bugs that a single-platform project never sees. That is the point, and it is also why the milestone plan front-loads the target abstraction rather than adding a second target at M6.

**Axis 2 against the ægraph decision.** The ægraph's CFG skeleton forbids control flow rewrites, and a meaningful share of C optimization is control flow. Document 09 keeps control flow passes conventional and outside the e-graph. If that split turns out to cost more than the e-graph buys, the middle end reverts to a conventional pass pipeline; document 19 makes this open question one and document 17 schedules the decision at M4.

## What we are not claiming, in the words we will use publicly

We are not claiming to beat GCC or Clang on generated code. We are claiming to get close on scalar code and to be honest about where we are not close.

We are not claiming to compile arbitrary C. We are claiming to compile the named projects in document 14 and whatever the CI corpus covers on the day of the claim.

We are not claiming production readiness before 1.0, and the placeholder crate on crates.io says so in its own description.

A project that ships a benchmark chart with an asterisk survives contact with the internet. A project that claims parity on everything gets taken apart in an afternoon.
