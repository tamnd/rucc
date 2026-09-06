# Separation logic

Layer 6. Opt-in, per function, at `-fsafety-proof=verify`, expected to cover well under 1% of any codebase and to be worth building anyway.

## 7.1 Why the ladder needs a top

Layers 0-5 all share an assumption: the property to be proved is expressible in arithmetic over program variables. Three things in real C are not, and all three are load-bearing.

**Allocators manipulate their own metadata.** A `malloc` implementation computes an address, writes a header, splits a free block, and returns an interior pointer. Every one of those operations is a memory-safety obligation about a region whose structure is the allocator's own invariant. No numerical domain has anything to say about it, and layer 5's ownership inference is worse than useless because the allocator is where ownership is *created*.

**Data-structure invariants are separation properties.** "The nodes of this list are pairwise disjoint and each is a valid allocation" is precisely the assertion separating conjunction was invented for.

**Some safety properties depend on contents.** [`06.6`](06-bounds-and-refinements.md)'s NUL-terminated strings; a ring buffer's `head`/`tail` discipline; a parser's "the length field I just read is within the buffer I read it from".

The economic case: these regions are **small, stable, hot, and already trusted**. musl's `mallocng` is a few thousand lines that has not changed structurally in years, and per [`../safe-memory/10.4`](../safe-memory/10-boundaries.md) the allocator must be trusted by the monitor anyway, it is the thing that *reports* storage instances. Proving it converts a trust-set entry into a discharge, which is worth strictly more than proving a thousand lines of application code.

## 7.2 Why CN and not VeriFast

[`01.1`](01-research-2026.md) makes the argument and it is worth restating as a decision.

The deciding factor is **not** proof automation. It is that [CN](https://dl.acm.org/doi/10.1145/3571194) is built on the Cerberus C semantics, which is the same body of work that produced PNVI-ae-udi, which the parent's document 07 adopts, which [`../safe-memory/04`](../safe-memory/04-safety-model.md)'s judgements are stated against, and which every obligation in [document 03](03-obligations.md) is ultimately a predicate over.

A verifier with a different memory model would mean two definitions of what a pointer is, and every proof would carry an unstated translation between them. That is a soundness gap that no amount of engineering closes, and it would sit in the trust set forever.

Secondary reasons, all pointing the same way: CN's first-class resources make pointer arithmetic and aliasing expressible rather than excluded, which is what allocator code needs; its resource inference for iterated separating conjunction is what array reasoning needs; its syntactic restriction on ghost variables guarantees their inference succeeds; and it has been used on the **pKVM buddy allocator**, which is within noise of our first target.

**What we take, precisely:** the specification language's design and its semantic foundation. **What we do not take:** the implementation, wholesale, as a dependency. `rucc` is a Rust workspace with a controlled dependency graph and CN is an OCaml tool. The realistic paths are (a) an out-of-process CN invocation for the `verify` layer, treated as an untrusted proposer whose output is checked by our own VC checker, or (b) a reimplementation of the fragment we need. **(a) first**, because it gets the layer working in weeks rather than quarters, and because [`04.1`](04-the-discharge-ladder.md)'s "search is untrusted, checker is trusted" split makes an external tool architecturally unremarkable.

## 7.3 The annotation surface

Specifications live in comments, per [document 08](08-annotations.md)'s absolute rule that annotated source must compile on other toolchains.

```c
/*@ requires  take Buf = each (u64 i; i < n) { Owned<char>(p + i) };
              n > 0;
    ensures   take Buf2 = each (u64 i; i < n) { Owned<char>(p + i) };
              return < n;
@*/
size_t scan(char *p, size_t n);
```

Three kinds of clause, and no more:

- **`requires` / `ensures`**: pre- and postconditions, including resource ownership.
- **`invariant`**: loop invariants, where inference fails.
- **`predicate`**: named recursive resource predicates for data structures.

Two kinds deliberately excluded: `assigns`-style frame clauses (the resource discipline already gives framing, which is the point of separation logic) and pure functional-correctness postconditions unrelated to memory safety. The latter is a *soft* exclusion (`return < n` above is a functional property that a safety proof needs downstream) but the rule is that a clause must earn its place by discharging a memory-safety obligation somewhere.

## 7.4 The workflow, which is the part that decides adoption

Copied from [Fulminate](https://dl.acm.org/doi/10.1145/3704879) and the [PLDI 2026 CN workflow paper](01-research-2026.md), because the historical failure of deductive verification is not that proofs are hard, it is that **a specification you cannot debug is a specification you abandon.**

**Stage 1: write the specification. Do not prove it. Test it.**

`-fsafety-proof-test` compiles the `requires`/`ensures`/`invariant` clauses into ordinary run-time assertions and runs the existing test suite against them. A wrong specification fails on iteration three of the unit tests, in two seconds, with a stack trace, instead of failing as an unhelpful "could not prove" after four minutes of solver time.

This mode has independent value even for users who never prove anything: it is a precondition checker for the annotations of [document 08](08-annotations.md), and it is how a team validates inferred or [machine-generated](09-inference-and-llm.md) specifications before trusting them enough to try proving.

**Stage 2: prove.** `-fsafety-proof=verify`. Obligations in the annotated function are attempted at layer 6.

**Stage 3: on failure, degrade.** The function compiles, monitored, exactly as if unannotated. A diagnostic is emitted, the one place in the specification where a failed proof is reported to the user, justified because the user asked. `-fsafety-proof-require` turns it into an error for CI on code that is supposed to stay proved.

**Stage 4: on success, the residue is nothing.** Every obligation in the function is `Discharged`, so per [`03.5`](03-obligations.md) every plane over the function's private data dies, and the function runs at full speed with no instrumentation.

Stage 4 is the reason to do this at all, and it is why the targets in §7.5 are chosen as *whole* small components rather than as hot functions inside large ones: partial discharge inside a module leaves the planes alive.

## 7.5 The targets, in order

**T1, A `malloc` implementation.** musl's `mallocng` first, because musl is already the milestone-S6 instrumentation target and because it is small and well-structured. Success means the allocator's own accesses are proved, the allocator's `__rucc_alloc_*` reports are proved to correspond to its actual metadata, and [`02.6`](02-the-goal.md)'s claim C6 holds.

Following [CN's pKVM buddy allocator result](https://dl.acm.org/doi/10.1145/3571194), this is a known-feasible target rather than a hope, which is why it is first.

**T2, `copy_to_user` / `copy_from_user`.** [`../safe-memory/11.4`](../safe-memory/11-kernel.md) calls the init check here the highest-yield single check in the kernel. It is also a tiny, stable, well-understood function whose specification, "the destination range is user-accessible, the source range is initialized and in-bounds, and on partial copy the return value bounds what was written", is short enough to write in an afternoon.

**T3, The string and memory library functions.** There is a published deductive-verification benchmark of 26 unmodified kernel library string and memory functions ([`01.7`](01-research-2026.md)), so this is a target with a baseline. It pays twice: proving `memcpy` and `strlen` improves the [effects table](../safe-memory/10-boundaries.md) from a *declaration* to a *theorem*, which removes trust-set entries that every instrumented program depends on.

**T4, Ring buffers and lock-free queues.** The kernel's `kfifo`, `ptr_ring`, and the per-CPU ring the monitor's own reporter uses ([`../safe-memory/11.7`](../safe-memory/11-kernel.md)). Hardest of the four because it is concurrent, and the reason it is on the list is that the *monitor's own reporter* being proved is worth having.

**T5, The kernel's buddy and slab allocators.** The real prize, following pKVM. After T1 through T4, and only with the caveat that a full slab allocator is an order of magnitude beyond a buddy allocator in complexity.

**Not on the list:** anything in an application. Parsers are mentioned in the document map as a plausible target and on reflection they are not one, a parser's safety depends on its input format, so the specification is the format, and that is a research project per format rather than a compiler feature.

## 7.6 Concurrency

T4 needs it and nothing else on the list does. The position:

- Single-threaded proofs are the default and cover T1-T3.
- For T4, the available mechanisms are lock-based resource invariants (a lock owns a resource; acquiring transfers it) and nothing else. Fine-grained lock-free reasoning (the RustBelt/Iris machinery) is out of scope, and a lock-free ring buffer that cannot be proved with a lock invariant is simply not proved.
- The monitor's epoch plane ([`../safe-memory/09.5`](../safe-memory/09-type-init-and-races.md)) remains for whatever is not proved, which is the correct fallback.

Stated so that "we will do concurrency later" is not implied.

## 7.7 Trust

Layer 6 is where the largest amount of unverified machinery would enter the trust set if it were not explicitly kept out.

**The rule:** the external prover produces a proof object; **our checker validates it**; only the checker is trusted. For an SMT-based discharge that means the unsat core, re-checked against the VC we generated from *our* IR, not against a VC the external tool generated from its own parse of the C source. The distinction matters enormously: a proof about a *different program* than the one we compile is worthless, and an out-of-process C verifier necessarily re-parses.

**The consequence:** VC generation must happen on our side, from our IR, and the external tool must be fed the VCs rather than the source. That is a real constraint on choosing path (a) in §7.2 and it is the main engineering cost of the layer.

**What remains trusted regardless:** the encoding from our IR into the VC logic, and the solver's unsat core checker. [Foundational VeriFast's hinted mirroring](https://arxiv.org/html/2601.13727) is the technique that would shrink even this (replaying the symbolic execution in a proof assistant) and it is a post-1.0 aspiration, not a plan. [Document 10](10-soundness-and-trust.md) §10.5.

## 7.8 Cost, honestly

**Per function proved: hours to days of human effort**, by someone who knows separation logic. That is the finding from every project in [`01.1`](01-research-2026.md) and there is no evidence anything has changed it, [document 09](09-inference-and-llm.md)'s automation notwithstanding, the machine-assistance results are on benchmark-scale functions, not on allocators.

**Per layer, to build:** 3-5 engineer-months for path (a), most of it in VC generation and certificate checking rather than in the logic.

**Expected discharge contribution: ~0%** of the corpus's obligation count, per [`04.10`](04-the-discharge-ladder.md)'s funnel where layer 6 moves the cumulative rate not at all.

That last number is the reason this layer is last and optional, and the reason it is *still* worth building is that its value is not measured in discharge rate. It is measured in **trust-set entries removed**: a proved allocator and a proved `memcpy` change what every other program's safety argument rests on, and [`../safe-memory/10.2`](../safe-memory/10-boundaries.md)'s counted trust set is where that shows up.

## 7.9 Non-goals

**Proving a program.** Functions, and small ones.

**Requiring proofs.** No part of the corpus, the CI, or the milestone criteria requires a layer-6 proof except claim C6's single allocator.

**A general-purpose verification IDE.** `-fsafety-proof-test` and a diagnostic. Users who want more should use CN directly, which is why §7.3's syntax is deliberately close to CN's.

**Competing with CN, VeriFast or VST.** They are better at this than we will be. We are a compiler that can consume their results.
