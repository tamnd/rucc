# Open questions

Everything in documents 03 through 18 is a decision. This document holds what is not decided, ranked by how much of the design depends on the answer, and each entry says what would settle it and when. A specification whose open questions are collected in one place is falsifiable; one that scatters them as hedges is not.

The first five are ranked. The rest are deferrals, which is a different thing: a deferral has an obvious default and the only question is whether to spend the effort.

## Q1: Does the ægraph carry from a Wasm JIT to an AOT C compiler?

**Why it is first.** Document 00 names this as the project's riskiest assumption, and document 09's entire middle-end structure rests on it.

Acyclic e-graphs are proven in Cranelift, which is a JIT with a compile-time budget measured in microseconds per function, a small rule set, and Wasm's already-structured, already-validated input. C at `-O2` is different in every one of those dimensions: functions after inlining are large, the rule set will be an order of magnitude bigger, and the input has aliasing, volatile, atomics and inline assembly, all of which are *outside* the e-graph by design and therefore all of which are places where the e-graph's canonical value must be reconciled with a side-effecting world.

The specific risks: e-class growth on large functions making compile time superlinear, which would put axis 3 in direct conflict with axis 2; extraction quality being worse than a well-ordered conventional pipeline because a cost model over a canonical DAG is a weaker instrument than a pass that knows what it just did; and GCM interacting badly with the loop transformations that must live outside the e-graph.

**How it gets settled.** M4 builds both, the ægraph rewriter and a conventional apply-once pipeline over the same verified rule set, and measures compile time and code quality on rung 1 and the LLVM test-suite. The shared rule set is what makes this affordable: the experiment costs the pass-ordering scaffolding, not a second optimizer.

**If the answer is no**, the rule DSL and the verification survive unchanged, the pipeline becomes conventional, and document 09's structure changes but its content mostly does not. That is the reason to run the experiment early and the reason the rules are separated from the engine that applies them.

## Q2: Do we write our own linker?

Document 11.6 states both cases. The argument for is that the kernel's link is where three external linkers disagree, that link time is a large share of a build we are claiming throughput on, and that owning it closes the portability story completely. The argument against is that `mold` is excellent and free, and that a subtly wrong linker is harder to bisect than a subtly wrong compiler.

**How it gets settled.** Two measurements taken at M11: what fraction of a kernel build's wall time is linking, and how many of the bugs found during M10 and M11 were linker-interaction bugs. Both high, and the project is justified as post-1.0 work; either low, and `mold` is the answer permanently.

**The default is no**, and the default is what ships at 1.0 regardless.

## Q3: Our own register allocator, or `regalloc2`?

Document 10.4 specifies both allocators behind `run(env, program) -> allocations + inserted_moves`, which is regalloc2's own API shape precisely so this stays open.

For `regalloc2`: it is mature, it is fast, it implements exactly the design we would implement anyway, and it ships a **checker**, an independent verifier that the allocation preserves the program's dataflow, which is worth a great deal on axis 1.

Against: it is a dependency whose priorities are Wasmtime's, its constraint model may not express everything a C backend needs (x86 two-address forms, inline assembly's constraint language from document 11.2, `asm goto`'s edge-dependent liveness), and register allocation is close enough to the core of code quality that outsourcing it caps how good we can get.

**How it gets settled.** M4 uses regalloc2 if it fits and writes our own if it does not, and the deciding test is whether the inline-assembly and `asm goto` constraints from document 13 can be expressed without contortion. Either way the checker is reimplemented if not inherited, because document 10.4 requires it unconditionally.

## Q4: Does the header cache pay for itself?

Document 05 specifies a content-addressed header cache keyed on the file's content, its resolved path, and the sorted set of macro bindings the header *queried*. The soundness argument depends on that key being complete, and the named soundness channels are the parts we know about.

Two risks. The first is cost: computing the queried-macro set is itself work, and on a header that queries fifty macros the key computation may approach the parse it replaces. The second is soundness: a channel we did not name (`__COUNTER__`, `__LINE__`-dependent expansion, `#pragma once` interacting with symlinked paths, a header that behaves differently based on include depth) silently produces a wrong compilation, which is the worst possible failure mode on axis 1.

**How it gets settled.** M5 builds it and measures it on the kernel's `defconfig` build, which is the workload it exists for. Kept only if the measured win is large enough to justify the soundness surface, and it stays behind `-fno-header-cache` and off by default until the corpus has run with it enabled for a sustained period with byte-identical output.

**The honest position:** this is the one place in the design where we accept a soundness *argument* rather than a soundness *proof*, and it is therefore the first thing to cut if it does not clearly earn its place.

## Q5: What does the no-poison model cost?

Document 08.4 makes the largest deliberate divergence from LLVM's IR design: no `poison`, no `undef`, `nsw` licensing specific proven rewrites rather than a taint that propagates.

The benefits are stated and are real: every rewrite is locally justifiable, which is what makes SMT verification of the rule set tractable, and document 15.5 notes that the refinement relation for translation validation becomes simple equality on defined behavior. The cost is unquantified: we lose optimizations depending on poison propagation, principally aggressive speculation of arithmetic across control flow.

**How it gets settled.** M4 measures it, on the LLVM test-suite and SPEC, by comparing against `gcc -O2` on the specific benchmarks where speculation matters: pointer-chasing loops with bounds checks, and code where a hoisted computation is only valid because its overflow is unreachable.

**If the cost is large**, say, more than 3% on the geometric mean, the response is not to adopt poison but to add the specific narrow constructs that recover the lost cases, most likely a `freeze`-like operation on a small closed set of speculation-enabling rewrites, each individually verified. Adopting the full poison model would forfeit the verification story, which is the project's principal claim.

## Deferrals

**Verification against authoritative ISA semantics.** Document 10.2's `spec` clauses are checked against a hand-written per-target machine model, which means a bug in the model is a bug the verifier cannot see. The follow-up work to Crocus cited in document 01 verifies against authoritative ISA semantics instead. Adopting that is post-1.0 and is the single largest available improvement to axis 1 after the rule verification itself.

**A machine outliner for `-Oz`.** Document 10.10 excludes it, with the note that it is the largest single size win available. If `-Oz` becomes important to real users, embedded, or the kernel's size-constrained configurations, it is reconsidered.

**BTF emission.** Document 11.5 relies on `pahole` reading our DWARF. Emitting BTF directly is faster and removes a build dependency, and is worth doing if the kernel work exposes DWARF fidelity problems that are easier to fix at the BTF level.

**MSVC dialect support.** Document 14.6 scopes out `__declspec`, SEH's `__try`/`__except`, and the MSVC preprocessor divergences. We target Windows as a host and the Windows x64 ABI for userspace, which is a different and much smaller claim. Whether the dialect matters depends entirely on whether anyone wants to compile Windows-native C with us, which is unknown.

**MSan**, per document 12.9, because it requires instrumenting every load and store and only works when all linked code is instrumented.

**A JIT**, per document 10.10. The IR and encoders would support one; the requirements around memory protection, patching and unwinding are a separate project.

**Polyhedral loop transformation and ML-driven heuristics**, per document 09. Both are live research areas as of 2026 and both are things a compiler with a stable IR is a good platform for. Neither belongs before 1.0, and the reason is the same in both cases: they improve code that is already correct, and axis 1 is not yet met.

**A stable Rust API** for the internal crates, per document 18.5. Deliberately deferred forever unless there is demand, and the tier-3 warning exists to keep that option open.

**C++.** Not deferred. Out of scope, permanently, per documents 00 and 14.6.

## Facts in this specification that failed verification

Recorded here rather than asserted, per document 01's closing section, so they are checked before anything depends on them.

**The Linux kernel's current minimum GCC version and its current `-std=` value.** Document 13 and document 14 both reason about kernel compatibility, and both are written to be correct regardless of the specific values, but M11's planning needs them. They change, and they must be read from the tree at the time rather than from memory.

**ccc's current size and capability.** Document 01 cites roughly 100k lines and Linux 6.9 boots on three architectures. Both were true at the time of the source consulted and both are moving; the positioning argument in document 00 does not depend on the exact numbers, but any public comparison would, and should be re-checked before it is made.

**The assembler and linker situation in ccc.** Document 01 flags an apparent drift between two sources on whether it uses external tools. Worth resolving before any comparative claim, because "dependency-free" is precisely the axis we are choosing not to compete on.

## How this document is maintained

An open question is closed by writing the answer into the document that owns the decision and replacing the entry here with one line recording what was decided, when, and on what evidence. Questions are not deleted, because the record of what was considered and rejected is the part of a specification that is most useful a year later and most often lost.

New questions are added here rather than left as hedges in the document where they arose. A hedge in a design document is a question nobody owns.
