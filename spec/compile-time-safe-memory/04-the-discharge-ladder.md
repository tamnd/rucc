# The discharge ladder

Seven layers, cheapest first, each consuming the previous layer's residue. This document specifies what each proves, what it costs, what it is expected to yield, and how it fails.

## 4.0 Why a ladder and not one solver

The obvious design is to hand every obligation, with its path condition, to an SMT solver. It is obvious, it has been tried, and it does not work inside a compiler for three reasons.

**Cost per obligation is wrong by orders of magnitude.** A million obligations in a large translation unit at a millisecond each is twenty minutes. The budget is 2x on a build that takes seconds.

**Solver time is unpredictable**, which is [`02.8`](02-the-goal.md)'s failure condition F5 directly: an obligation discharged today and timed out tomorrow makes performance non-reproducible.

**Most obligations are trivial.** A load from a local `int` through a pointer that was just `&x`, in a function with no loops, the great majority of obligations in real code look like that. Spending solver calls on them is spending the budget where the value is not.

So: a funnel. Each layer is strictly more expensive and strictly more capable than the last, and each is only ever asked about what survived. The design goal is that **layer 0 sees everything and layer 6 sees almost nothing.**

This is Astrée's own structure (cheap domains first, expensive ones on demand) and Frama-C's plug-in architecture ([`01.3`](01-research-2026.md)) with one difference that changes everything: **their residue is an alarm list a human must triage; ours is a check the hardware executes.**

## 4.1 The invariants every layer obeys

**Monotone.** A layer may move an obligation from `Checked` to `Discharged` or `Narrowed`. It may never move one back. Discharge is therefore order-independent for correctness, though not for speed.

**Local failure.** A layer may abandon a function, a loop, or a single obligation at any point (on a timeout, on an unsupported construct, on a resource limit) and doing so is not an error. The residue is checked.

**Certificate-emitting.** Every discharge produces a certificate in that layer's format ([document 10](10-soundness-and-trust.md)). A layer that cannot explain its reasoning cannot discharge.

**Budgeted.** Every layer has a wall-clock budget expressed as a multiple of the *un-instrumented* compile time for that function. Exceeding it aborts the layer for that function, not the compilation.

**Untrusted by default.** The layer's *checker* may be in the trust set; the layer's *search* never is. This is the distinction that lets layers 4-6 use heuristics, solvers, caches and machine-generated hints.

## 4.2 The ladder

| L | Name | Scope | Budget | Level | Discharges mostly |
|---|---|---|---|---|---|
| 0 | Syntactic and type-directed | expression | ~0 | `fast` | `O.prov`, `O.align`, `O.perm`, constant `O.lo`/`O.hi` |
| 1 | Intervals and dominance | function | 0.1x | `fast` | `O.lo`, redundant everything |
| 2 | Relational and inductive | function | 0.5x | `default` | `O.hi` in loops, the big one |
| 3 | Interprocedural summaries | module / LTO | 0.3x | `default` | `O.live`, `O.init`, plane elision |
| 4 | Refinement typing | function + signatures | 2x | `deep` | `O.hi` across calls, `O.type` |
| 5 | Ownership and regions | whole program | 5x | `deep` | `O.live` on the ownable fraction |
| 6 | Separation logic | annotated function | unbounded | `verify` | everything, on 0.1% of the code |

Budgets are multiples of baseline compile time for the function, and they sum with the level's total: `default` is layers 0-3, so 1 + 0.1 + 0.5 + 0.3 ≈ **1.9x**, which is the 2x in [`02.4`](02-the-goal.md) with almost nothing to spare. That is deliberate, a budget with slack gets spent.

## 4.3 Layer 0, Syntactic and type-directed

Runs in `rucc-safety` at insertion time, on the AST-adjacent IR, before anything else. It is not really a prover; it is the recognition that most obligations are answerable from the shape of the expression.

**What it discharges:**

- `O.prov` wherever the pointer's definition is an `alloca`, a global address, a string literal, or the result of a successful null-checked allocation. Provenance is syntactically evident.
- `O.align` wherever the access type's alignment divides the base object's alignment and the offset. Pure arithmetic on constants.
- `O.perm` for every access whose storage class is known at the definition site, the overwhelming majority.
- `O.lo` and `O.hi` for constant-index access into a fixed-extent object: `a[3]` where `a` is `int[8]`. This is the single largest bucket in the table.
- `O.type` for accesses through a pointer whose type has not been laundered through a cast, in a translation unit compiled without `-fno-strict-aliasing`.
- `O.init` for a load from a local that a dominating store in the same block wrote.

**Cost:** a walk, with a small map. Effectively free.

**Expected yield: 45-60% of static obligations.** **[unverified]** If it is below 40% the ladder's economics change and layer 1's budget should rise.

**Failure mode:** none interesting. It either recognizes a form or does not.

**Why it is not "just the frontend being sensible":** because it emits certificates and its results are counted. An optimizer that folds `a[3]`'s check away as an arithmetic identity produces the same code and no accounting, and [`03.6`](03-obligations.md)'s four consumers all need the accounting.

## 4.4 Layer 1, Intervals and dominance

The first real analysis. Two things run together because they share a traversal.

**Interval domain.** Per-variable, per-program-point, over the dominator tree, with the standard abstract-interpretation machinery restricted to make it cheap: intervals only (no relations), widening after 3 iterations with the loop guard as a threshold ([Astrée's technique](https://www.di.ens.fr/~rival/papers/erts10.pdf)), and a hard cap on the number of fixpoint iterations.

**Dominance-based redundancy.** The obligation form of [`../safe-memory/07`](../safe-memory/07-check-elimination.md)'s dominator-tree walk, restated per [`03.7`](03-obligations.md)'s rule that a dominating `Checked` obligation is an assumption. If `O.hi(p, n)` at *b* is implied by `O.hi(p, m)` at *a*, `a` dominates *b*, `m ≥ n`, and no intervening operation invalidates the capability, discharge it.

**What it discharges:** most `O.lo` (indices are non-negative and monotone far more often than not), redundant repeats of every class, and `O.init` for locals through a straight-line store-then-load.

**Cost:** one fixpoint over the CFG with a small abstract state. Linear-ish; the budget is 0.1x and it should not come close.

**Expected yield: 10-20% of the residue after layer 0.** **[unverified]**

**Failure mode, the important one.** Intervals lose everything at a join and everything at an unbounded loop. `for (i = 0; i < n; i++) a[i] = 0;` gets `i ∈ [0, ∞)` after widening unless the guard is used as a threshold, and even with the threshold it gets `i ∈ [0, n)` only as a *symbolic* fact, which an interval domain cannot represent. **Layer 1 cannot discharge the canonical array loop.** That is layer 2's entire job and it is why layer 2 gets five times the budget.

## 4.5 Layer 2, Relational and inductive

Where the money is. The canonical array-write loop is the most common shape in C and its `O.hi` obligation is the most executed obligation in most programs.

**Three mechanisms, in increasing cost.**

*Induction-variable recognition with symbolic bounds.* Recognize `i` as an affine induction variable with a guard `i < n`, and the object as having extent related to `n`, from an annotation, an allocation site `malloc(n * sizeof(T))`, or a dominating check. Discharge `O.hi` for the whole loop. This is a pattern match, not a fixpoint, and it is cheap enough that it runs first and probably catches most of the yield.

*Weakly relational domain.* Octagons (`±x ± y ≤ c`) over a **variable-packed** subset, Astrée's technique, because octagons are cubic in the number of variables and packing is what makes them affordable. The packs are chosen by the obligations themselves: for `O.hi(p + i, n)` the pack is `{i, n, extent(p)}`. This inverts the usual formulation, in which packs are chosen syntactically and the analysis hopes the properties fall out; here the goal set is known, so pack for it.

*Loop-body splitting.* Where the bound holds for all but the first or last iteration, peel or split rather than fail. The peeled iteration's obligation is a separate obligation with a fresh id per [`03.6`](03-obligations.md).

**What it discharges:** `O.hi` in loops; `O.lo`/`O.hi` for pointer walks (`while (p < end)`), which intervals cannot touch because the relation is between two pointers; `O.derive`.

**Cost:** the packed octagon domain is the expensive part and its budget is most of the 0.5x. Packs are capped in size (4 variables) and count.

**Expected yield: 30-45% of the residue after layer 1, and a much higher share of the *dynamic* residue**: this is the layer where `D_d` and `D_s` diverge most, in the good direction, because loop bodies execute.

**Failure mode:** relations through memory. `s->len` and `s->buf` is a relation between two *fields*, not two variables, and no numerical domain represents it without a heap abstraction. That is layers 4 and 6, and it is why `struct { size_t len; char *buf; }` (the single most common shape in C) needs an annotation or a refinement type to discharge. [Document 08](08-annotations.md).

## 4.6 Layer 3, Interprocedural summaries

Cheap, unglamorous, and per [`02.7`](02-the-goal.md) the highest-yield thing available for temporal obligations.

**The summaries:**

| Summary | Meaning | Discharges |
|---|---|---|
| `nofree` | this call frees nothing | `O.live` after the call, [`../safe-memory/08.8`](../safe-memory/08-temporal-safety.md)'s 5%-vs-40% |
| `noescape(p)` | the callee does not retain `p` | plane elision per [`03.5`](03-obligations.md) |
| `nowrite(p)` / `readonly` | no store through `p` | `O.init`, `O.type` after the call |
| `extent(ret) = f(args)` | the returned pointer's extent | `O.hi` at the caller |
| `requires(...)` | callee-side annotation lifted | the caller's obligation per [`03.7.1`](03-obligations.md) |
| `nonnull(p)`, `aligned(p, k)` | trivial but common | `O.prov`, `O.align` |

**Computation:** bottom-up over the call graph's SCC condensation, with recursion handled by starting at the conservative value and iterating to a fixpoint bounded at two rounds. Within a module without LTO; across modules with it, using the parent's existing summary machinery ([`../safe-memory/15.3`](../safe-memory/15-integration.md) places safety summaries in the existing LTO pass).

**Why `nofree` deserves special attention.** It is a *single bit*, it is computable by a trivial reachability query over the call graph, and it discharges the plane load that [`02.2.1`](02-the-goal.md) says is where the actual cost lives. There is no better ratio of implementation effort to recovered cost anywhere in this specification. It should be built first, before layer 2, regardless of the ladder's ordering, the ladder orders *execution*, not *construction*.

**Failure mode:** indirect calls. One call through a function pointer with no devirtualization poisons `nofree` for everything downstream. Mitigations: type-based filtering of the possible targets, `-fsafety-assume-nofree` for declared-safe callback sets, and honest accounting when it fails. On code with heavy callback use (which is most parsers and all of `qsort`-style APIs) expect this layer to yield little.

## 4.7 Layer 4, Refinement typing

`deep` only. The [Flux](https://dl.acm.org/doi/10.1145/3591283) approach applied to C, and the answer to layer 2's failure mode.

Types are refined with indices: `char * __counted_by(n)` is `{p: ptr | extent(p) = n}`, and struct fields carry refinements so `struct { size_t len; char *buf __counted_by(len); }` becomes expressible. Verification conditions are generated syntax-directed and discharged in the quantifier-free theory of linear integer arithmetic, which is decidable and fast. Loop invariants are **inferred** by liquid inference over a fixed set of candidate qualifiers derived from the obligations in scope, the property that gave Flux its zero-annotation result and its order-of-magnitude edge over Prusti.

**What it discharges:** `O.hi` through calls and through the heap; `O.type` where the refinement carries a tag; the length-carrying-struct shape that defeats layer 2.

**Cost:** solver calls, batched per function. Budget 2x, which is why this is `deep`.

**Expected yield:** high on annotated code, moderate on inferred code, near zero on code that does neither. This layer's value is a direct function of [document 08](08-annotations.md)'s annotation coverage, and the kernel (already carrying `__counted_by` throughout) is its best case.

**Failure mode:** refinements are per-value, so mutation through aliases breaks them. Flux gets strong updates from Rust's ownership; C has none, so this layer must fall back to weak updates wherever a pointer may alias, which is often. The mitigation is layer 3's `noescape` and the parent's TBAA-derived alias facts, and the honest expectation is that layer 4 in C is materially weaker than Flux in Rust for this reason.

## 4.8 Layer 5, Ownership and regions

`deep` only, and per [`02.7`](02-the-goal.md) the layer with the lowest expected yield relative to its cost. It is here because temporal obligations are 25-30% of the total and nothing else addresses them at scale.

Constraint-based inference in the [Crown](https://doi.org/10.1007/978-3-031-37709-9_18)/[&inator](https://arxiv.org/abs/2604.17261) lineage: infer, for each pointer-typed location, whether it is owning, borrowed, or neither, subject to borrow-check-like constraints; then discharge `O.live` for accesses through pointers proved borrowed from a live owner within the borrow's region.

**What is realistic:** the shapes that do own. Stack-allocated buffers passed down and not retained. Arena and pool allocations with a single owner and a bulk free. Reference-counted objects where the acquire/release pairs are syntactically evident. Tree-shaped ownership in parsers and ASTs.

**What is not:** intrusive linked lists, `container_of` upcasts, callback-held pointers, anything reachable from a global, and the entire class of code where lifetime is managed by a protocol rather than by structure. Which is, in the kernel, nearly all of it.

**Cost:** whole-program constraint solving. Crown's 500k lines in under 10 seconds is the encouraging data point and the reason the budget is 5x rather than unbounded.

**Failure mode:** &inator's own stated limitation (scaling to large programs is future work) and Scylla's finding that the tractable fraction is an *applicative subset*. If this layer's measured yield at V4 is under 10% of temporal obligations, it should be cut and its budget given to layer 3, and [document 14](14-milestones.md) makes that an explicit decision point rather than a disappointment.

## 4.9 Layer 6, Separation logic

Opt-in, per function, at `verify`. [Document 07](07-separation-logic.md) specifies it; here only its place in the ladder.

It runs on functions carrying an explicit specification, it may take minutes, and it is expected to cover well under 1% of a codebase, the allocators, the ring buffers, the parsers' inner loops, and [`02.6`](02-the-goal.md)'s claim C6.

The ladder property that matters: **layer 6's residue is still just checks.** A `verify`-annotated function whose proof fails compiles and runs, monitored, exactly like an unannotated one. It emits a diagnostic (this is the one place in the specification where a failed proof is reported, because the user explicitly asked for the proof) but it does not fail the build unless `-fsafety-proof-require` is given.

## 4.10 The funnel, end to end

Predicted, on ordinary C at `-fsafety-proof=deep`. **[unverified: this table is the specification's central prediction and V2 measures it]**

| After | Static obligations remaining | Cumulative `D_s` |
|---|---|---|
| generation | 100% | 0 |
| layer 0 | 45% | 0.55 |
| layer 1 | 36% | 0.64 |
| layer 2 | 22% | 0.78 |
| layer 3 | 16% | 0.84 |
| layer 4 | 12% | 0.88 |
| layer 5 | 10% | 0.90 |
| layer 6 | 10% | 0.90 |

`default` stops at layer 3: **`D_s ≈ 0.84`**, comfortably above claim C1's 0.70 for spatial obligations and above claim C5's 0.25 for temporal only if layer 3's `nofree` performs.

Note what the table does *not* say: it says nothing about `R`. Ten percent residual obligations spread evenly across every object in the program elides no planes at all, and `R` could be near zero with this exact funnel. **The funnel is the easy half of the measurement.**

## 4.11 What is deliberately not a layer

**Bounded model checking.** Produces no certificate and no proof, only the absence of a counterexample under a bound. Excellent for finding bugs; useless for discharging obligations. It belongs in [`../safe-memory/14`](../safe-memory/14-verification.md)'s testing, not here.

**A global SMT encoding of the function.** §4.0.

**Profile-guided discharge.** [`../safe-memory/07`](../safe-memory/07-check-elimination.md) uses CHOP-style profile data to *order* elimination effort; profiles may prioritize which obligations a layer attempts, and may never justify a discharge. A hot obligation and a cold one get the same standard of proof.

**Anything that trusts a machine-generated artifact.** [Document 09](09-inference-and-llm.md). Generated artifacts feed layers 4 and 6 as *hints*; the layer still proves.
