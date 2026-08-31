# The landscape, checked August 2026

Everything here was checked against sources during the week of 31 August 2026 and the links are inline. Where a claim is a vendor number, a single blog post or general knowledge rather than a reproducible measurement, it says so. Where something could not be confirmed it is marked **[unverified]** and document 19 tracks it.

The purpose is not to survey the field. It is that about twenty specific facts determine most of the architecture, and when one of them changes we need to know which decisions to revisit.

## 1. The result that changed the scoping

### 1.1 Claude's C compiler

[`anthropics/claudes-c-compiler`](https://github.com/anthropics/claudes-c-compiler), announced 5 February 2026. From the [engineering writeup](https://www.anthropic.com/engineering/building-c-compiler): sixteen Claude Opus 4.6 instances working in parallel containers with git synchronization and task locks, roughly 2,000 sessions over two weeks, 2 billion input tokens and 140 million output tokens, about $20,000 in API cost, producing a 100,000-line Rust compiler with no dependencies beyond the standard library.

What it does, per the repository README as of this check: preprocessor, lexer, parser, semantic analysis to a typed AST, target-independent SSA IR with mem2reg, fifteen SSA optimization passes with shared loop analysis, backends emitting assembly then machine code then ELF for x86-64, i686, AArch64 and RISC-V 64, its own assembler and linker for all four, and DWARF generated from scratch. It compiles bootable Linux 6.9 on three architectures. Its fully-passing list includes PostgreSQL with 237 regression tests, SQLite, QuickJS, zlib, Lua, libsodium, libpng, jq, libjpeg-turbo, mbedTLS, libuv, Redis, libffi, musl, TCC and DOOM, with 150+ further projects including FFmpeg at 7,331 FATE checkasm tests, coreutils, busybox, CPython, QEMU and LuaJIT. GCC torture pass rates are reported around 99%.

Note the drift between the two sources: the February writeup says there was no built-in assembler or linker and that GCC tools were still required, and that there was no independent 16-bit x86 code generator. The current README says the assembler and linker are in-tree for all four targets. The project evidently kept moving after launch, so **the writeup is a launch snapshot and the README is the current state**. Cite them accordingly.

What it explicitly does not do, in its own words and its authors': the repository says "I do not recommend you use this code! None of it has been validated for correctness." The writeup says generated code efficiency lags significantly behind GCC. All optimization levels run the same pipeline, so there is no meaningful `-O0` versus `-O2` distinction. `_Atomic` is parsed but the qualifier is not tracked through the type system. `_Complex` has edge-case failures. The host platform is Linux; macOS and Windows are untested.

**What this means for rucc.** Two things, and they point in opposite directions, which is why this is the first section.

Downward on cost: the ladder in document 14 is now known to be climbable, in Rust, at roughly 100k lines. Every estimate in document 17 is anchored to that number rather than to GCC's or Clang's, and the "can a from-scratch backend really boot a kernel" risk is retired.

Upward on scope: the three things that project skipped (validated correctness, competitive code quality, and host portability) are precisely the three things that make a compiler usable, and they are three of our four axes in document 02. We are not competing on whether the thing exists. We are competing on whether it can be trusted and whether its output is fast.

## 2. The other compilers in this category

**TCC**, Fabrice Bellard's [Tiny C Compiler](https://bellard.org/tcc/), is the compile-speed reference point and the cautionary tale. The classic benchmark compiles the Links browser, 76,936 source lines expanding to 1,950,947 preprocessed lines, in 2.27 seconds against 20.0 seconds for `gcc -O0` on a 2.4 GHz Pentium 4, about 860,000 preprocessed lines per second against 98,000. It gets there with a single-pass design that has no IR beyond a value stack, and it pays for that with output running roughly half the speed of GCC's, with pathological cases far worse: one tokenizer test with a large `switch` ran at 1.2 million lines per second against GCC's 9.0 million.

**chibicc**, by Rui Ueyama, is about 9,000 lines of C for a working C11 compiler, developed commit-by-commit as a teaching artifact. It is the best available map of the minimum viable path through the C frontend and the commit sequence is worth following directly. **[unverified]** on the exact line count.

**cproc** by Michael Forney is a C11 frontend over [QBE](https://c9x.me/compile/), Quentin Carbonneaux's compiler backend, whose stated design goal is roughly 70% of LLVM's performance in roughly 10% of the code. QBE is the existence proof that a small backend can produce respectable code, and its IL is worth reading before finalizing document 08. **kefir**, by Jevgenijs Protopopovs, is a self-hosting C17/C23 compiler in C and is currently the most complete independent C23 implementation outside the big three. **Cuik** appears in cross-language [compile speed benchmarks](https://github.com/nordlow/compiler-benchmark) close behind TCC.

**saltwater**, formerly `rcc`, by Jynn Nelson, is the previous serious attempt at a C compiler in Rust: C11-targeting, Cranelift-backed, with a JIT mode, renamed because the binary name `rcc` collided with Qt's resource compiler, and dormant since roughly 2020. Its [FAQ](https://github.com/jyn514/saltwater/blob/master/FAQ.md) records that LLVM via Inkwell was tried first and abandoned because it could not emit object files, which is a data point about binding-mediated LLVM use rather than about LLVM.

**What this means for rucc.** TCC's numbers set the compile-throughput ceiling and its runtime numbers set the price of reaching it. Document 02 takes the position that TCC's trade is the wrong one and picks a different point on the curve. QBE sets the "small backend, decent code" reference. saltwater is the argument against depending on someone else's backend through a binding.

## 3. The backend techniques we are adopting

### 3.1 Rule-based instruction selection, verified by SMT

Cranelift's ISLE is a term-rewriting DSL in which lowering rules are data compiled into a matcher at build time. [Crocus](https://cs.wellesley.edu/~avh/veri-isle-preprint.pdf) (VanHattum, Pardeshi, Fallin, Sampson and Brown, "Lightweight, Modular Verification for WebAssembly-to-Native Instruction Selection", [ASPLOS 2024](https://dl.acm.org/doi/10.1145/3617232.3624862)) models values in ISLE rules as SMT bitvectors and searches all inputs for soundness counterexamples, verifying full functional equivalence between an IR instruction and its native lowering. On the AArch64 integer lowering rules it reproduced three known bugs including one rated 9.9 severity, found two previously unknown bugs plus an underspecified compiler invariant, and won a Distinguished Artifact Award. The follow-up, ["Scaling Instruction-Selection Verification against Authoritative ISA Semantics"](https://dl.acm.org/doi/10.1145/3764383), verifies against formal ISA specifications rather than hand-written models, and the tooling lives in-tree at [`cranelift/isle/veri/`](https://github.com/bytecodealliance/wasmtime/blob/main/cranelift/isle/veri/README.md).

The requirement Crocus imposes is real and worth stating: every term used within a rule must be annotated. That is a tax on every lowering rule, paid up front.

**What this means for rucc.** Document 10 specifies lowering as a rule DSL from the first line of backend code, with per-term specifications written alongside the rules rather than retrofitted, because Crocus's own experience is that retrofitting annotations to an existing rule set is the expensive path.

### 3.2 The e-graph middle end

Cranelift rearchitected its middle end onto an e-graph representation in 2022, adapted as "acyclic e-graphs" or ægraphs; the [RFC](https://github.com/bytecodealliance/rfcs/blob/main/accepted/cranelift-egraph.md), the [EGRAPHS 2023 talk](https://pldi23.sigplan.org/details/egraphs-2023-papers/2/-graphs-Acyclic-E-graphs-for-Efficient-Optimization-in-a-Production-Compiler) and Fallin's [April 2026 writeup](https://cfallin.org/blog/2026/04/09/aegraph/) are the primary sources. It solves pass ordering, and it lets the same rewrite DSL serve the middle end and the backend.

Three engineering facts from that work matter to us. The e-graph is represented in the IR itself rather than as a separate structure requiring translation in and out. Rewrites are applied once at node creation, a strategy resembling the "cascades" approach from database query optimization, combined with union-find for partial canonicalization. And a "CFG skeleton" pins the control flow so the function can be reconstructed, which **prohibits control flow rewrites**: rewrites may span blocks, but block structure is fixed. Cranelift's ægraphs cannot represent cycles, so full equality saturation is out.

Fallin's own assessment, which we should take seriously rather than skip past, is that where ægraphs truly shine remains an open question, and whether more rules or different workloads would make cost-based extraction pay off is unresolved.

The general e-graph literature behind it is [`egg`](https://dl.acm.org/doi/10.1145/3434304) (Willsey et al., POPL 2021) for fast extensible equality saturation and [`egglog`](https://dl.acm.org/doi/10.1145/3591239) (Zhang et al., PLDI 2023) for the Datalog unification.

**What this means for rucc.** Document 09 adopts ægraphs for the value-level middle end and keeps control flow optimization (jump threading, tail duplication, block layout, loop rotation) as conventional passes outside the e-graph, because the skeleton forbids doing it inside. Document 19 makes "does the ægraph carry over from a Wasm JIT to ahead-of-time C" the first open question, with the experiment scheduled in M4.

### 3.3 Register allocation

[`regalloc2`](https://github.com/bytecodealliance/regalloc2) is roughly half a port of IonMonkey's backtracking allocator to Rust, generalized and extended, and importantly shipping with fuzzing harnesses and checkers the original lacked. Its [design document](https://github.com/bytecodealliance/regalloc2/blob/main/doc/DESIGN.md) and [Fallin's writeup](https://cfallin.org/blog/2022/06/09/cranelift-regalloc2/) are the references. The switchover in Cranelift 0.84 / Wasmtime 0.37 reported roughly 20% overall compiler speedup, because compile time was dominated by register allocation, plus code performance improvements up to 10 to 20% on register-pressure-bound benchmarks and up to 7% for `rustc_codegen_cranelift`. Live-range splitting, the same value living in different places at different points, is the technique doing most of that work. The single-pass `fastalloc` companion has had a bumpy history, disabled in 2025 and later restored, with fuzzing continuing to surface issues.

The classical literature still worth implementing from: Hack and Goos on SSA-form register allocation and chordal interference graphs, George and Appel's iterated register coalescing (TOPLAS 1996), and Wimmer and Franz on linear scan over SSA for the fast tier.

**What this means for rucc.** Document 10 specifies two allocators behind one interface: a single-pass linear-scan allocator for `-O0`, which is where the compile-throughput claim is won, and a backtracking allocator with live-range splitting for `-O1` and above. Whether the latter is our own or `regalloc2` as a dependency is open question three in document 19; the interface is designed so the answer can change.

### 3.4 Not sea of nodes

V8's [March 2025 post](https://v8.dev/blog/leaving-the-sea-of-nodes) documents a three-year migration off sea of nodes to Turboshaft, a conventional CFG IR, with compilation time roughly halved and code quality equal or better. Maglev, their mid tier, [chose](https://v8.dev/blog/maglev) traditional SSA over a CFG rather than a "more flexible but cache unfriendly sea-of-nodes representation". The [RVSDG](https://dl.acm.org/doi/10.1145/3391902) line of work (Reissmann et al., TOPLAS 2020) is the interesting alternative in the other direction and is not chosen here.

**What this means for rucc.** Document 08 is a CFG of basic blocks with block parameters instead of phi nodes. This is settled and not revisited.

## 4. Correctness tooling, which is the actual differentiator

**Csmith** (Yang, Chen, Eide and Regehr, PLDI 2011) generates random C programs free of undefined behavior by combining whole-program analysis with dynamic checks, and found [more than 325 previously unknown bugs](https://users.cs.utah.edu/~regehr/papers/pldi11-preprint.pdf) across three years. Its most cited result for our purposes is that the verified core of CompCert was the one compiler it could not break.

**YARPGen** takes the same idea but avoids UB statically by tracking types, alignments and value ranges during generation rather than wrapping operations, which matters because wrapper functions measurably hurt coverage and bug-finding. It has found 220+ bugs in GCC, LLVM and others. [YARPGen v2](https://dl.acm.org/doi/abs/10.1145/3591295) (PLDI 2023) adds generation policies that deliberately skew the distribution toward programs likely to trigger specific optimizations, and mechanisms for diverse loop code, on the explicit theory that a fuzzer that stops finding bugs is biased rather than done. It found two bugs in Alive2 itself.

**Alive2** (Lopes, Lee, Hur, Liu and Regehr, [PLDI 2021](https://users.cs.utah.edu/~regehr/alive2-pldi21.pdf)) is bounded translation validation: take a function before and after a transformation and ask an SMT solver whether the second refines the first. It descends from Alive (2015) and from Necula's translation validation work (PLDI 2000).

**Minotaur** (Liu, Mada and Regehr, [OOPSLA 2024](https://dl.acm.org/doi/10.1145/3689766), Distinguished Paper) is the synthesis direction: slice out how each SSA value is computed, enumerate rewrites, discard incorrect ones with Alive2's refinement checker, apply a cost model, cache. 7.3% average speedup on GMP's suite with a 13% max, 1.5% average on SPEC CPU 2017 with 4.5% on 638.imagick, every optimization formally verified, several landed upstream in LLVM.

**cvise** is the practical descendant of C-Reduce and is the difference between "the fuzzer found something" and "here is a nine-line reproducer."

**MLGO** is the machine-learning branch, and it is genuinely in production: [LLVM's docs](https://llvm.org/docs/MLGO.html) describe ML-driven inlining-for-size and register allocation eviction deployed across google3, Fuchsia and Chrome on Android, with 6.3% size reduction on Fuchsia C++ translation units and 0.3 to 1.5% QPS on datacenter applications.

**What this means for rucc.** Document 15 builds the differential harness, the Csmith/YARPGen loop and the cvise reducer in M1, before the optimizer exists, because retrofitting a correctness apparatus onto an optimizer that already has bugs means you spend the first month drowning in them. The ægraph rewrite rules and the ISLE lowering rules are both verified by SMT, which is the same technique Alive2 and Crocus use applied to rules rather than to programs, and is far cheaper because rules are small. Minotaur is the model for the post-1.0 peephole story and is not a 1.0 commitment. MLGO is explicitly out of scope: the gains are real but they are 1% gains that require a training pipeline, and we have 30% gains available from implementing ordinary optimizations first.

## 5. The language, as it actually stands

C23 is published as **ISO/IEC 9899:2024**, with stage 60.60 reached on [31 October 2024](https://www.iso.org/standard/82075.html). `__STDC_VERSION__` is `202311L`. The freely available draft closest to the published text is N3220. GCC has C23 support and incomplete C2Y support; Clang's C23 support is partial.

The successor is in active drafting under the informal name C2y and is now targeted as **C29** for roughly 2029. Working drafts moved from N3854 in March 2026 to [N3886 in late May 2026](https://www.open-std.org/jtc1/sc22/wg14/www/wg14_document_log). Live work includes a `defer` Technical Specification (N3853, committee draft r5, March 2026, with GCC patches already existing), undefined-behavior cleanup (N3861, Uecker), removing implementation-defined bit-field signedness (N3862, Seacord), and contracts.

Pointer provenance has a Technical Specification: [N3005](https://www.open-std.org/jtc1/sc22/wg14/www/docs/n3005.pdf), "A Provenance-aware Memory Object Model for C", working draft of ISO/IEC TS 6010, by Gustedt, Sewell, Memarian, Gomes and Uecker. The adopted model is **PNVI-ae-udi**, provenance-not-via-integers, exposed-address, user-disambiguation. Every storage instance gets an ID unique for the whole execution; addresses may be reused after a lifetime ends but IDs never are; a valid pointer's provenance is the ID of the instance it points into or one past. The academic foundation is Memarian et al.'s "Into the depths of C" (PLDI 2016) and "Exploring C semantics and pointer provenance" (POPL 2019). Worth noting: SDCC adopted the WG14 memory model study group's test suite and it immediately surfaced a silently-wrong-code bug affecting all their backends.

**What this means for rucc.** Document 07 targets `gnu23` as the default dialect, which is where GCC already is, and specifies PNVI-ae-udi as the reference semantics for what the alias analysis is allowed to assume. The C2y features are tracked but not implemented before 1.0, with one exception: `defer` is worth prototyping behind a flag because GCC patches exist and early divergence is expensive. The provenance test suite goes into the M2 test corpus.

## 6. The rungs of the ladder

**SQLite** is the first rung because the amalgamation is a single large translation unit of largely conservative C, its own test suite is extraordinary, and the one compiler-specific thing it wants is computed goto for VDBE dispatch under GCC.

**PostgreSQL** is the third rung and it has three specific requirements. It needs `-fwrapv`, because it relies on signed overflow wrapping in overflow checks that the compiler would otherwise delete; its build system adds the flag automatically where supported. It needs `-fexcess-precision=standard` to keep x87 80-bit intermediates from leaking into `float`/`double` results on 32-bit x86. And `src/backend/executor/execExprInterp.c` uses labels-as-values for expression dispatch behind `HAVE_COMPUTED_GOTO`, falling back to a `switch` otherwise. It also leans on `setjmp`/`longjmp` throughout its error handling, which constrains what the optimizer may do around calls. **[unverified]** on whether the flags are still added by `configure`/Meson in current releases exactly as described.

**The Linux kernel** is the last rung. It moved from `-std=gnu89` to `-std=gnu11` in 2022 and has moved toward newer dialects since **[unverified on the current value]**. `asm goto` has been a hard requirement on x86 for years. The `CC_HAS_ASM_GOTO` Kconfig and its detection script were removed entirely once both supported baselines exceeded GCC 4.5 and Clang 9. `asm goto` *with outputs* is the newer requirement, used for fault-handler fixups and `__get_user`-style code, and was the subject of a [2020 LLVM dev meeting talk](https://llvm.org/devmtg/2020-09/slides/Wendling_Desaulniers_asm_goto_w_outputs.pdf) by Wendling and Desaulniers. The historical documented minimums were GCC 5.1 and Clang 11; current [kernel documentation](https://docs.kernel.org/process/changes.html) no longer pins a single Clang version and says only that the latest LLVM release is supported, with GCC requirements varying by architecture.

MaskRay's [survey of GNU extensions in the kernel](https://maskray.me/blog/2024-05-12-exploring-gnu-extensions-in-linux-kernel) is the single best inventory of what a kernel-capable compiler must implement, and document 13 is built from it: statement expressions, local labels, labels as values, case ranges, `typeof`, `__auto_type`, `__builtin_types_compatible_p`, inline asm with the full constraint language, `__attribute__` for `error`, `naked`, `section`, `weak`, `aligned`, `packed`, `cleanup` and `nocf_check`, the `__builtin_constant_p` / `choose_expr` / `expect` / `object_size` / `dynamic_object_size` / `frame_address` / `offsetof` / `*_overflow` / `prefetch` / `unreachable` family, popcount and clz and ctz, `#pragma GCC diagnostic` and `visibility` and `poison`, empty structures, and the omitted-middle conditional `x ?: y`. The kernel also requires the dialect flags `-fno-strict-aliasing`, `-fno-delete-null-pointer-checks` and `-fno-strict-overflow` to be honored as semantics rather than ignored.

**What this means for rucc.** Document 14 turns each rung into an exit criterion with a named test suite, and document 13 turns MaskRay's list into a checklist with a test per item. The `-fno-*` dialect flags are not cosmetic: they change what the alias analysis and the overflow reasoning in document 09 are permitted to assume, so they are threaded through as first-class semantic switches from the start rather than bolted on.

## 7. Facts that did not survive checking

Two things worth recording so nobody re-derives them.

The exact current minimum GCC version for the kernel and the exact current `-std=` value could not be pinned down from secondary sources, and the search results contained several stale numbers presented as current. Read `Documentation/process/changes.rst` and the top-level `Makefile` in the tree you are actually targeting. Document 19 carries this.

The precise line count of `claudes-c-compiler` today is not stated in its documentation; 100,000 is the launch-writeup figure and the project has clearly grown. Do not quote a current figure without counting it.
