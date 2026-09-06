# The optimizer

## 9.1 The pipelines

There is one pipeline per optimization level, written out explicitly rather than assembled from flags. The prior art in document 01 ran the same pipeline at every level and named it as a limitation; the whole compile-throughput axis in document 02 lives or dies here.

**`-O0`.** SSA construction (which document 08 does during lowering, so it is free), mem2reg for the `alloca`s that remain, and simplify-CFG. Nothing else. No dominator tree is built, and the only analysis computed is the CFG that simplify-CFG reads. Straight to document 10's fast instruction selector and linear-scan allocator. This is the path the 2x-over-`clang -O0` claim is made on and it is protected by a benchmark that fails CI on regression.

Simplify-CFG is at `-O0` because a branch on a condition that is a constant is not a missed optimization. The arm nothing takes still holds calls, and those calls reach the linker, so a program that guards a call to a function it does not link with `if (0)` fails to link at `-O0` and links at every other level. That is issue 359, which is `execute/medce-1.c` out of the gcc torture suite, and gcc removes it at `-O0` as well. The pass builds a CFG, reads terminators, and touches nothing else, so it costs one walk over the blocks.

**`-Og`.** `-O0` plus the transformations that do not move code across statement boundaries: constant folding, local CSE within a block, dead code elimination, and simplify-CFG restricted to removing empty and unreachable blocks. Debug quality is the constraint, not speed.

**`-O1`.** The e-graph runs once. Inlining at a conservative threshold. Simplify-CFG, SROA, GVN, DCE, LICM, and the loop canonicalizations. Dominators, loops and a cheap alias analysis are computed.

**`-O2`.** Two e-graph rounds around the loop pipeline. Full inlining with the cost model in 9.7. Memory SSA and the full alias analysis stack. PRE, load and store elimination, jump threading, tail duplication, loop unrolling, induction variable simplification, if-conversion, SLP vectorization, and machine-level scheduling in document 10.

**`-O3`.** `-O2` plus loop vectorization, more aggressive inlining and unrolling thresholds, loop interchange and distribution where the dependence analysis is confident, and function specialization.

**`-Os` and `-Oz`.** `-O2`'s pipeline with a size cost model: inlining only when it shrinks, no unrolling, no vectorization, and in `-Oz` additionally the outliner and a preference for smaller encodings in instruction selection.

Every pass has `-f<name>` and `-fno-<name>`, and the level tables are printed by `rucc --print-pipeline -O2`.

## 9.2 The e-graph middle end

The value-level optimizer is an acyclic e-graph following Cranelift's ægraphs, as surveyed in document 01. This replaces what would otherwise be a constant-folding pass, a peephole pass, a GVN pass, a reassociation pass and an instcombine pass, all with a pass-ordering problem between them.

**How it works here.** The e-graph is represented in the IR itself, not as a separate structure: an e-class is a union-find set over `ValueId`s, and the "canonical" value of a class is the union-find root. A new instruction is inserted by first canonicalizing its operands, then hash-consing it, then applying every rewrite rule that matches, adding each result to the same e-class. Rules are applied **once at node creation**, in the cascades style, rather than to fixpoint. Because we only ever build upward from operands that are already canonical, the graph stays acyclic and terminates.

**The CFG skeleton.** Control flow is pinned. The blocks, the terminators and the edges are fixed while the e-graph runs, and rewrites may span blocks but may not change them. This is Cranelift's constraint and we inherit it.

**Extraction and placement.** After rewriting, each e-class is extracted to a single representative by a cost model, instruction count weighted by an estimated latency per opcode, with loop depth as a multiplier so that a value used in a loop is willing to be more expensive at its definition if that definition can be hoisted. Placement then runs Cliff Click's global code motion (PLDI 1995): schedule each value as early as its inputs allow, then sink it to the latest point that dominates all its uses and has the lowest loop depth. GCM is where LICM and partial CSE fall out for free, which is a large part of why the e-graph pays for itself.

**What is not in the e-graph.** Anything that changes control flow: simplify-CFG, jump threading, tail duplication, loop rotation, if-conversion, block layout, and the loop transformations. Anything involving memory dependence: load/store elimination, PRE over memory, dead store elimination. And inlining, which changes the CFG wholesale. These are conventional passes running before and after e-graph rounds.

That split is the main technical risk in this document. A meaningful fraction of C optimization is control flow, and the e-graph cannot touch it. Document 19 makes this open question one, with the M4 experiment being: implement the e-graph and a conventional instcombine-plus-GVN pipeline behind the same interface, measure both on compile time and output quality over the benchmark set, and keep the winner. Building both is perhaps three extra weeks and it removes the largest architectural bet in the project.

## 9.3 The rewrite rules

Rules are data, in a DSL compiled to a matcher at build time by `rucc-rules`. This is the same mechanism document 10 uses for instruction selection, and one implementation serves both.

```
;; x * 2^n  =>  x << n
(rule (mul (value x) (iconst k))
      (if (is_power_of_two k))
      (shl x (iconst (log2 k)))
      (spec (= (bvmul x k) (bvshl x (log2 k)))))

;; (x + c1) + c2  =>  x + (c1 + c2), only when no overflow is assumed away
(rule (add.nsw (add.nsw (value x) (iconst c1)) (iconst c2))
      (if (no_signed_overflow (add c1 c2)))
      (add.nsw x (iconst (add c1 c2)))
      (spec (=> (and (no_soverflow x c1) (no_soverflow (bvadd x c1) c2))
                (= (bvadd (bvadd x c1) c2) (bvadd x (bvadd c1 c2))))))
```

The `spec` clause is the SMT obligation, discharged by `rucc-verify` in CI against a bitvector solver. A rule without a `spec` cannot be merged. This is Crocus's technique applied to the middle end, and document 01 records that Crocus's authors found it works for middle-end rewrites as well as for lowering.

Two things this buys beyond correctness. Rules are readable, so a contributor can add a peephole without understanding the pass manager. And rules are *enumerable*, so the fuzzer in document 15 can generate an input specifically shaped to trigger each rule and check that it fires, which is how we detect rules that silently stop matching after an IR change.

## 9.4 Analyses

**Dominators** by the Cooper, Harvey and Kennedy iterative algorithm, which is simpler than Lengauer and Tarjan and faster on the CFG sizes that real C functions produce. Dominance frontiers computed on demand.

**Loops** by Tarjan's algorithm over the dominator tree, producing a loop forest with headers, latches, exits and preheaders. Loop canonicalization ensures a single preheader, dedicated exits and a single latch, because every loop transformation downstream assumes it.

**Alias analysis**, layered, each layer cheap and consulted in order until one answers:

1. *Trivially distinct storage*: two distinct `alloca`s, or an `alloca` and a global, never alias. Free.
2. *Provenance*, per document 07's PNVI-ae-udi model: two pointers with different provenance IDs do not alias. This is the layer that makes stack and heap disambiguation work.
3. *TBAA*, per the effective type rules, using the metadata tree on memory operations, disabled entirely by `-fno-strict-aliasing`.
4. *Offset-based*: two accesses derived from the same base with constant offsets that do not overlap.
5. *`restrict`*: pointers in disjoint restrict scopes do not alias. Implemented properly, with the scope tree, not as a blanket assumption.
6. *A unification-based points-to analysis* in the Steensgaard style for the whole module, computed once at `-O2`. Steensgaard rather than Andersen: near-linear time, less precise, and the precision difference on C code is smaller than the compile-time difference. Document 19 records that this may need revisiting if the measurements say otherwise.

Each layer's answer is attributable, so `-fdump-alias` says *which rule* concluded no-alias. When the alias analysis is wrong (and it will be) this turns a week into an hour.

**Memory SSA**, in LLVM's style: a parallel SSA graph over memory, with definitions at stores and clobbering calls, uses at loads, and phis at joins. It is what makes load elimination, store elimination and PRE over memory tractable instead of quadratic. Built at `-O2` and above only.

## 9.5 The scalar passes

Each one below is standard, each has a literature reference, and each must earn its slot per 9.10.

Sparse conditional constant propagation (Wegman and Zadeck, TOPLAS 1991), which finds constants and unreachable branches simultaneously and is strictly stronger than running the two separately. Aggressive dead code elimination, marking from side effects backward, which deletes dead loops that the conservative form cannot. Global value numbering, mostly subsumed by the e-graph but retained for memory operations. Partial redundancy elimination in the value-based formulation of VanDrunen and Hosking (CC 2004), which is where a surprising fraction of real-world wins live because C code recomputes address expressions constantly. SROA, splitting aggregates into scalars, which is what makes struct-heavy C code fast and which must handle the union and memcpy cases correctly. Simplify-CFG: merge, delete unreachable, fold branches on constants, turn diamonds into selects. Jump threading and tail duplication, which are what make interpreter loops fast and which Postgres's expression evaluator and SQLite's VDBE both need. Correlated value propagation over the dominator tree. Tail call elimination. Dead store elimination over memory SSA. Store-to-load forwarding.

## 9.6 Loop optimizations

Canonicalization first: rotation into do-while form, guard insertion, preheader creation, exit dedication, induction variable canonicalization.

Then LICM, which mostly falls out of GCM but needs an explicit form for memory operations that GCM cannot move. Induction variable simplification and strength reduction. Loop unrolling, full for small constant trip counts and partial otherwise, with a cost model tied to the target's loop buffer size. Loop unswitching for invariant conditions. Loop deletion for loops with no effects. Loop idiom recognition, turning a copy loop into `memcpy` and a zero loop into `memset`, worth real money on C code, and specifically disabled under `-ffreestanding` where those functions may not exist.

At `-O3`: loop interchange and loop distribution, gated on a dependence analysis. The dependence test is the GCD test plus Banerjee's inequalities, which handles the affine cases that matter; a full polyhedral framework in the Polly style is explicitly out of scope for 1.0 and recorded in document 19 as a post-1.0 possibility.

## 9.7 Inlining

Inlining is the single highest-value optimization in C and its cost model is the difference between a good compiler and a bad one.

The model: a call site's benefit is estimated from the callee's size after specializing on the constant arguments at this site, the removal of the call overhead, and the enabling effect on the caller, a callee that returns a constant, or whose result feeds a branch condition, is worth more. Cost is the code size increase, scaled by whether the call site is hot. Call sites are ordered by benefit-to-cost across the whole module and consumed until a budget is exhausted, rather than each call site being decided independently, which is what lets the important inlines happen before the budget is gone.

The bottom-up traversal over the call graph's strongly connected components means a callee is optimized before its callers consider inlining it, so the size estimate is of the real code.

`always_inline` and `noinline` are honored absolutely. `inline` and `extern inline` follow C99/C23 semantics *and* GNU semantics under `-std=gnu89` or `__gnu_inline__`, which differ, and the kernel depends on the distinction.

Recursive inlining is bounded and off by default. Cross-translation-unit inlining requires LTO, in 9.8.

## 9.8 Link-time optimization

`-flto` writes our serialized IR into a dedicated section of the object file instead of machine code. The driver detects LTO objects at link time, deserializes, merges into one module, runs the `-O2` pipeline with the whole program visible, and generates code.

`-flto=thin` is the scalable form: each object keeps its own IR plus a summary index of its symbols, their sizes and their call graph edges. At link time only the summaries are merged, an import decision is made per function, and each object is optimized in parallel importing only what it needs. This is the only form that works on Postgres or the kernel, because monolithic LTO on a large program needs more memory than the machine has.

Two correctness obligations that are easy to get wrong and are the reason document 04 threads semantic flags into the IR rather than reading them globally. A unit compiled with `-fno-strict-aliasing` must not have TBAA applied to it after being merged with a unit compiled without. A unit compiled with `-fwrapv` must not have `nsw` inferred on its arithmetic after inlining into a unit compiled without. Both are carried per-function and per-instruction, so the merge is safe by construction.

Symbol resolution at LTO time must agree with the linker's, which means implementing the plugin interface the linker expects. `-fuse-ld=mold` and `lld` both support the LLVM gold plugin protocol, and we implement that protocol rather than inventing one.

## 9.9 Profile-guided optimization

`-fprofile-generate` instruments edge counters at the CFG level, using the minimal spanning tree placement so that only a fraction of edges need counters. `-fprofile-use` reads the profile and attaches block frequencies to the IR.

What consumes them: inlining (hot call sites get a much larger budget), block layout in document 10 (hot paths made fall-through, cold blocks moved to a `.text.unlikely` section), register allocation (spill placement in cold blocks), unrolling, and if-conversion.

`__builtin_expect` and `__builtin_expect_with_probability` feed the same block-frequency mechanism, which is how a project gets most of the benefit without a profiling run, and the kernel's `likely`/`unlikely` are exactly this.

A sampling-based mode using `perf` data, in the AutoFDO style, is post-1.0 and recorded in document 19.

## 9.10 The pass manager, and the rules for passes

The pass manager is deliberately boring: a fixed, printed sequence per level, with analyses computed on demand and invalidated by a declared dependency set per pass. No adaptive pass ordering, no pass scheduling heuristics. Predictability is worth more than the last percent, and document 03's determinism rule requires it.

**Every pass declares** which analyses it requires, which it preserves, and which it invalidates. A pass that lies is caught by a debug-mode check that recomputes an analysis it claimed to preserve and compares.

**Fuel.** `-fpass-fuel=<pass>=<n>` lets a pass perform exactly *n* transformations and then become a no-op. This is how a miscompiling transformation is bisected to a single site, and it works: a script that binary-searches fuel over a failing test finds the exact transformation in `log n` compilations. It costs a counter check per transformation and it is required for every pass, checked by a test that runs each pass at fuel 0 and confirms the output equals the input.

**Dumps.** `-fdump-ir=before-<pass>` and `after-<pass>`, `-fdump-ir=all`, writing the textual form from document 08 to numbered files. `-fdump-ir-diff` writes only what changed, which is what a human actually wants.

**Verification.** The IR verifier from document 08 runs after every pass in debug and CI builds.

**A pass must earn its slot.** The rule, enforced socially and by the benchmark job in document 16: a new pass ships at a given `-O` level only with a measurement showing it pays for its compile time on the benchmark set. Passes that are correct and useless are how compilers get slow, and every compiler has a dozen of them because nobody ever measured. We measure at merge time.

## 9.11 What is deliberately absent

No superoptimizer in the pipeline. Minotaur's results, in document 01, are real and the technique is the obvious future of the peephole layer, but it is an offline rule-synthesis tool: the right use is to *generate rules* for 9.3 offline and ship the verified rules, not to run a solver during compilation. That is a post-1.0 project and it composes cleanly with the rule DSL, which is a good reason to have the rule DSL.

No machine learning in the heuristics. MLGO's production numbers are 0.3% to 1.5% on QPS and 6.3% on size, which are real and which are also smaller than what we get from implementing the ordinary passes above properly. Revisit when the ordinary passes are done.

No polyhedral loop framework. No whole-program devirtualization, since C does not have virtual calls. No software pipelining, which is a large amount of machinery for in-order targets we are not prioritizing.
