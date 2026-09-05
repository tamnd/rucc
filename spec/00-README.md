# rucc: the specification

An optimizing C compiler written in Rust. Its own preprocessor, parser, SSA middle end, register allocator, assembler, object writer and debug info. No LLVM, no GCC, no external toolchain in the default build. GCC command line compatibility including `-std=gnu23` and the GNU extension surface. The target list is x86-64, AArch64 and RISC-V 64; the host list is Linux, macOS and Windows. The compile ladder is SQLite, then Postgres, then a booting Linux kernel.

Repo: `github.com/tamnd/rucc`. Crate: [`rucc`](https://crates.io/crates/rucc), reserved at 0.0.0 on 31 August 2026. Binary: `rucc`. Written 31 August 2026.

## The one thing that changed this year

In February 2026 Anthropic published [`claudes-c-compiler`](https://github.com/anthropics/claudes-c-compiler), a dependency-free C compiler in Rust written almost entirely by a fleet of sixteen Claude Opus 4.6 agents over two weeks, about [100,000 lines at launch](https://www.anthropic.com/engineering/building-c-compiler) and now carrying its own assembler, linker and DWARF emitter for four targets. It compiles a bootable Linux 6.9 on x86, ARM and RISC-V. It compiles Postgres and passes 237 regression tests. It compiles FFmpeg and passes 7,331 FATE checkasm tests. It compiles QEMU, CPython, musl, Redis, LuaJIT and about 150 other projects, and it passes roughly 99% of the GCC torture suite.

That project deleted the single largest risk in this specification, which was never a technical question but a scoping one: *is a from-scratch C compiler in Rust that compiles the Linux kernel a two-year project or a ten-year project?* The answer is now known and it is closer to two.

It also, by its authors' own account, left the interesting half of the problem untouched. The repository's own warning is "I do not recommend you use this code! None of it has been validated for correctness." The launch writeup says generated code efficiency lags significantly behind GCC. Every optimization level runs the same pipeline, so `-O0` and `-O3` produce the same code. `_Atomic` is parsed but the qualifier is not tracked through the type system. The host platform is Linux and macOS and Windows are untested.

So the frontier moved. The question is no longer "can this be built". It is "can this be built such that a reasonable engineer would run it on code they care about." Document 02 is about that distinction and it is the document that decides whether this project is worth doing.

## The goal, stated so it can be falsified

Four axes, each with a number, each measured by the methodology in document 16.

**Correctness.** Zero known miscompilations at the point of a release, where "known" is defined by a test apparatus that is actively trying to find them: differential execution against GCC and Clang on every program in a corpus of real projects, continuous Csmith and YARPGen fuzzing with automatic cvise reduction, and SMT-based verification of the mid-end rewrite rules and the instruction selection rules. This is the axis where we intend to be better than the state of the art in the from-scratch-C-compiler category, and it is achievable because the tooling to do it now exists off the shelf.

**Generated code quality.** Within 10% of `gcc -O2` on integer and pointer-heavy scalar workloads at 1.0. Not within 10% of `-O3` on auto-vectorized floating point; document 02 says plainly why that is a different and much larger problem, and document 16 reports the two separately rather than averaging them into a flattering number.

**Compile throughput.** 2x faster than `clang -O0` at `-O0`, and 1.5x faster than `clang -O2` at `-O2`, on preprocessed lines per second, single threaded, same machine. TCC gets [roughly 7 to 9x over `gcc -O0`](https://bellard.org/tcc/) by not having an IR at all, which costs it about half the runtime performance of GCC's output; that trade is the wrong one for us and document 02 explains the position we are taking instead.

**Portability.** The compiler builds and passes its full test suite on Linux, macOS and Windows, on x86-64 and AArch64 hosts, and cross-compiles to all three targets from all three hosts. Object output covers ELF, Mach-O and COFF. This is the axis nobody in this category takes seriously and it is nearly free if it is a constraint from the first commit and nearly impossible if it is retrofitted.

## Why Rust, concretely

Not because Rust is fashionable. Three specific reasons.

The bug classes that dominate compiler CVEs and compiler crashes are use-after-free in IR mutation, buffer overruns in encoding, and iterator invalidation during pass execution. An index-based, arena-allocated IR in Rust makes the first and third structurally impossible and the borrow checker makes the pass manager's aliasing rules enforceable at compile time rather than by convention.

The ecosystem now carries the boring parts. [`object`](https://crates.io/crates/object) reads and writes ELF, Mach-O, COFF and PE. [`gimli`](https://crates.io/crates/gimli) reads and writes DWARF. `rayon` gives function-level parallelism for free. `insta`, `libtest-mimic`, `arbitrary` and `cargo-fuzz` give a test apparatus that would otherwise be a year of work. We do not depend on Cranelift or LLVM for codegen, but we do depend on the object and debug format layers, and document 18 is explicit about which dependencies are load-bearing and which are replaceable.

Fearless refactoring is the actual operational argument. A compiler's middle end is rewritten three or four times over its life. In C++ that is a multi-month project with a tail of segfaults; in Rust it is a week of fighting the compiler and then it works.

## Settled decisions

**Our own backend. No LLVM, not now and not as an option.** An LLVM backend would make this a frontend, and a frontend cannot make the compile-throughput claim or the portability claim, which are two of the four axes. It would also make the correctness story LLVM's story. Document 10.

**A conventional CFG-based SSA IR, not sea of nodes.** V8 spent three years [moving off sea of nodes](https://v8.dev/blog/leaving-the-sea-of-nodes) and reported compile time roughly halved with equal or better code quality. We start where they finished. Document 08.

**The middle end is an acyclic e-graph with rewrite rules in a DSL, following Cranelift's ægraphs.** Chris Fallin's [ægraph work](https://cfallin.org/blog/2026/04/09/aegraph/) solves the pass-ordering problem in a production compiler with a CFG skeleton pinning control flow and union-find giving partial canonicalization. The rules are data, which means they can be verified by an SMT solver, generated into a matcher at build time, and read by a human. Document 09.

**Instruction selection is a declarative rule DSL, and the rules are SMT-verified.** ISLE plus [Crocus](https://cs.wellesley.edu/~avh/veri-isle-preprint.pdf) (VanHattum et al., ASPLOS 2024) is the proof that this is practical: Crocus reproduced three known Cranelift bugs including one rated 9.9 severity, found two unknown ones, and won a distinguished artifact award. Hand-written lowering in `match` arms is the single largest source of miscompilation in a from-scratch backend and we are not writing it that way. Document 10.

**Every stage has a textual form with a round-trip parser.** Tokens, preprocessed output, AST, IR, post-optimization IR, machine IR, assembly. Every stage can therefore be tested, fuzzed, diffed and bisected in isolation, which is what "modular so it is easy to extend, test and debug" actually cashes out to. Document 03 and document 15.

**Real optimization tiers.** `-O0` is a straight-line lowering with mem2reg and nothing else, and it is the fast path we make the compile-throughput claim on. `-O1`, `-O2`, `-Os`, `-Oz` and `-O3` are genuinely different pipelines. Document 09.

**GCC compatibility means `__GNUC__` is defined and therefore everything under it must work.** The trap is well understood: the moment you claim to be GCC 15, glibc's headers, the kernel's headers and every autoconf script take the GNU path, and you have signed up for the entire extension surface those paths use. Document 13 enumerates it rather than discovering it.

**Undefined behavior gets an explicit written model, including pointer provenance.** We implement PNVI-ae-udi from [WG14 N3005](https://www.open-std.org/jtc1/sc22/wg14/www/docs/n3005.pdf) as the reference semantics for the alias analysis, because a compiler that cannot say precisely what it assumes cannot be verified and cannot be debugged when it miscompiles. Document 07.

**Correctness apparatus is built in M1, not at the end.** The differential harness exists before the optimizer does. Document 15 and document 17.

## The documents

| | | |
|---|---|---|
| 00 | this file | the pitch, the settled decisions, what to read first |
| 01 | `01-research-2026.md` | the verified landscape, papers, numbers, and what each one forces |
| 02 | `02-the-goal.md` | the four axes, what is reachable on each, and against whom |
| 03 | `03-architecture.md` | layers, dataflow, the query engine, parallelism, the error model |
| 04 | `04-driver-and-cli.md` | GCC compatible driver, flags, specs, linking, cross compilation |
| 05 | `05-preprocessor.md` | translation phases, macro expansion, `#embed`, and why this is 30% of compile time |
| 06 | `06-lexer-and-parser.md` | C23 grammar, the typedef ambiguity, error recovery, diagnostics |
| 07 | `07-types-and-semantics.md` | the type system, C23 features, constant evaluation, the UB and provenance model |
| 08 | `08-ir.md` | the SSA IR, memory model, aliasing metadata, the verifier, the textual form |
| 09 | `09-optimizer.md` | the ægraph middle end, the pass set, alias analysis, loops, IPO, LTO, PGO |
| 10 | `10-backend.md` | rule-based instruction selection, scheduling, register allocation, ABI lowering |
| 11 | `11-asm-objects-debug.md` | integrated assembler, inline asm, ELF/Mach-O/COFF, DWARF 5, linker interop |
| 12 | `12-abi-and-runtime.md` | the psABIs, bitfields, varargs, TLS, atomics, the builtins library, sanitizers |
| 13 | `13-gnu-compat.md` | the GNU extension matrix, `__builtin_*`, attributes, what the kernel forces |
| 14 | `14-target-ladder.md` | SQLite, then the mid tier, then Postgres, then the kernel; per rung requirements |
| 15 | `15-testing.md` | torture suites, differential execution, fuzzing, translation validation, CI |
| 16 | `16-performance.md` | what we measure, against whom, and the rules for reporting it |
| 17 | `17-milestones.md` | M0 to M11, exit criteria, and the three places it is sane to stop |
| 18 | `18-package-layout.md` | the crate tree, dependency rules, stability tiers |
| 19 | `19-open-questions.md` | the ranked list that has to be answered, and by when |
| 20 | `20-execution-testing.md` | running the generated code: the oracles, the build paths, the limits, the coverage |

Read 02 first, then 01. Document 02 decides whether the project is honest and document 01 is the evidence it rests on.

## The optimizer, elaborated

Document 09 says what the optimizer is in eleven pages. That is the right length for a document somebody reads before deciding whether to work on this project, and it is nowhere near enough to build from.

[`spec/optimizer/`](optimizer/) is the elaboration: forty four documents against GCC 16 and the research as of 2026, one per analysis or transformation, plus a plan that says in what order and what has to be true at the end. It is a child of document 09 and it does not widen the scope document 17 gives M4. Where the two disagree, [`optimizer/43-plan.md`](optimizer/43-plan.md) sections 43.5 and 43.6 list every departure by number and document 09 is the one that gets amended.

Start at [`optimizer/00-README.md`](optimizer/00-README.md), then [`optimizer/43-plan.md`](optimizer/43-plan.md) for the order of work. The M4 sub milestones on the issue tracker are that plan, one milestone per phase.

The evidence that any of it works is in [tamnd/rucc-corpus](https://github.com/tamnd/rucc-corpus), which is a C corpus where every program is written for one named transformation and every expected answer was computed in Rust rather than taken from another compiler.

## What this is not

Not a C++ compiler. Not now and not later. C++ is a different project an order of magnitude larger and mixing them kills both.

Not a linker, at least not before 1.0. We emit objects and invoke the system linker or `mold`. Document 11 keeps an internal linker as a post-1.0 possibility because the kernel's linker script handling and the `--emit-relocs` path make a self-contained kernel build attractive, but it is not a 1.0 promise.

Not a static analyzer. We emit the diagnostics a compiler emits, we implement `-fanalyzer`-class checks nowhere, and `-fsanitize=` instrumentation is a codegen feature rather than an analysis feature.

Not a drop-in GCC replacement at 1.0. It compiles the projects on the ladder in document 14 and the ones the CI corpus covers. Everything else is best effort, and document 14 is explicit that the honest claim is "compiles these named things" rather than "compiles C."

## Honesty about scope

Somewhere between 40 and 70 engineer-months to 1.0 as specified, which is three to six person-years. The prior art moves the estimate down from where it would have been eighteen months ago, and the correctness and portability requirements move it back up, because those are exactly the requirements the prior art skipped.

The parts most likely to kill it are not the ones that sound hardest. Instruction selection and register allocation are hard but bounded, well documented in the literature, and testable in isolation. Getting a kernel to boot is a grind but it is a grind with a clear signal.

The two things that actually kill projects like this:

**The GNU extension surface has no bottom.** Document 13 enumerates about ninety extensions the kernel alone requires and that list is not complete, because the real requirement is not "the extensions the kernel uses" but "the extensions glibc's headers, autoconf's probes and every project's `configure` script use when `__GNUC__` is defined." The defence is the corpus in document 15: compile other people's real code continuously from M2 and let their build systems find the gaps instead of guessing.

**Optimizer quality is a long, flat grind with no dramatic wins.** Getting from "works" to "within 10% of `gcc -O2`" is fifty passes' worth of small percentages, and each one needs a benchmark to justify it and a verifier to keep it honest. Document 09 sequences it and document 16 sets the rule that a pass without a measured win does not ship.

The riskiest technical assumption is that the ægraph middle end, which was designed for a JIT compiling WebAssembly, carries over to an ahead-of-time compiler on C's much messier semantics, specifically that the CFG skeleton's prohibition on control flow rewrites does not cost us too much, since a large fraction of C optimization is control flow. Document 19 makes that open question one and document 17 puts the experiment in M4, before anything depends on it.

## On the name

`rucc` is Rust plus `cc`, and it sits in the naming line that runs `pcc`, `lcc`, `tcc`, `8cc`, `9cc`, `chibicc`. Four letters, unambiguous about what it is, and it types quickly, which matters for a program people invoke thousands of times a day.

The name was chosen over the alternates in full knowledge of two collisions, both of which are acceptable and neither of which is a surprise waiting to happen.

[`jyn514/saltwater`](https://github.com/jyn514/saltwater) was originally called `rcc` (one `c`, not two) and was renamed because the *binary* name `rcc` collides with Qt's resource compiler. It is dormant, last released around 2020, and its binary is `swcc`. Our binary is `rucc`, which collides with nothing.

There are several unrelated toy projects on GitHub named `rcc`, mostly following Nora Sandler's write-a-compiler series. Different name, one letter apart, no shared namespace.

The registry situation, checked directly on 31 August 2026 rather than assumed: `crates.io/crates/rucc` was free and is now ours at 0.0.0, published as an explicit placeholder that says so in its description and README. Every `rucc-*` crate name in document 18 was free at the same moment. `github.com/tamnd/rucc` was free.

The alternates from the naming discussion, all confirmed free on crates.io and unused as GitHub repository names, were `hoshicc`, `ruricc`, `kumacc` and `nagicc`. `hoshicc` was the recommendation on distinctiveness grounds and `rucc` was chosen on legibility grounds. The cheapest moment to revisit that is before M1; after the first real release the binary name is in other people's build scripts and the decision is made.
