# Inference and machine assistance

Where annotations and invariants come from when nobody writes them, including from models, and the one rule that makes that safe.

## 9.1 The rule

> **A generator may propose an annotation, an invariant, a predicate, or a specification. It may never discharge an obligation, produce a certificate, or enter the trust set.**

Everything in this document is downstream of that sentence. A proposal is an *input* to the ladder; the ladder proves or fails as it would on a human-written input; a wrong proposal costs a failed proof or, per [`08.1`](08-annotations.md)'s rule 3, a run-time trap in testing.

This is why the specification can be relaxed about generation in a way that would be indefensible in a verification tool. [`01.6`](01-research-2026.md) records the field's own central worry, [VCoT-Bench](https://arxiv.org/html/2603.18334)'s critique that tools treat verification as a black box and measure success solely by whether the program verifies, and [KaPilot](https://arxiv.org/html/2607.21957)'s open problem of whether a specification that passes verification captures the intended property.

**Both are devastating for functional verification and irrelevant here**, because *our specification is not generated*. The obligations come from J1-J7 in [`../safe-memory/04`](../safe-memory/04-safety-model.md); they are fixed, mechanical, and identical for every program. A generator is never asked what should be proved. It is asked only for the *middle* of a proof (the loop invariant, the extent expression, the resource predicate) every one of which is checked by the layer below.

## 9.2 Non-model inference first, because it is better

Most of the yield here needs no model at all, and building the model path first would be a mistake.

**Call-site evidence.** For an unannotated parameter `void f(char *p, size_t n)`, examine every visible call site. If at each one the pointer's extent is provably equal to the value passed as `n`, propose `__counted_by(n)`. This is the inference behind [`08.7`](08-annotations.md)'s suggestion mode, it is a straightforward dataflow query, and on library code with many internal callers it should account for the large majority of correct annotations.

**Definition-side evidence.** If the body of `f` accesses `p[i]` under a guard `i < n` on every path, propose the same. Weaker (it shows consistency, not correctness) but it works for leaf functions with no visible callers, which is where call-site evidence fails.

**Allocation-site propagation.** `p = malloc(n * sizeof(T))` gives `p` extent `n` locally; propagate it through the call graph and it becomes an annotation on whatever parameter it flows to.

**Constructor-shaped field inference.** [`06.5`](06-bounds-and-refinements.md): if every store to `s->buf` is accompanied by a store to `s->len` of the matching extent, propose `char *buf __counted_by(len)`.

**Liquid invariant inference.** Layer 4's candidate-qualifier fixpoint ([`04.7`](04-the-discharge-ladder.md)). Not "inference of annotations" but inference of the invariants that would otherwise be annotations, and it is what gave Flux its zero-annotation result.

These are cheap, deterministic, reproducible, explainable, and they run in the compiler. **Model assistance is for the residue after all of them**, and if the residue turns out to be small this document's second half is unnecessary, which would be a good outcome and should be measured before the work is done.

## 9.3 Where a model helps

Three places where the non-model inferences structurally cannot reach.

**Annotating a header with no visible callers.** A library's public API, annotated from documentation and usage examples rather than from code in the build. This is the largest practical gap: the annotations that matter most are on boundaries, and boundaries are where evidence is absent.

**Loop invariants that are not in the candidate set.** Liquid inference searches a fixed lattice of qualifiers derived from the obligations. An invariant requiring a term nobody wrote down (a sum, a disjunction, a relation to a third variable) is outside it. [ExVerus](https://arxiv.org/pdf/2603.25810) identifies invariant inference as the most prevalent bottleneck in this space, and it is the one place where a generative model's ability to guess the *shape* of an assertion is a genuine advantage over search.

**Separation-logic specifications** for [`07.5`](07-separation-logic.md)'s targets. Resource predicates for a data structure are hard to write, mechanical to check, and there is [direct prior work on LLM-generated VeriFast specifications](https://arxiv.org/pdf/2411.02318) and on [Checked C annotation](https://arxiv.org/pdf/2404.01096).

## 9.4 The pipeline

Deliberately offline. Nothing here runs during a build.

```
1. Build with -fsafety-suggest-annotations
       → ranked list of undischarged obligations with attributions
2. Rank by dynamic weight (with a profile) or static count
3. For the top N, generate a candidate annotation / invariant / spec
4. Gate A: does it parse and type-check in scope?
5. Gate B: does -fsafety-proof-test pass the existing test suite with it?
6. Gate C: does it actually discharge obligations, and how many?
7. Emit a patch. A human reviews it. It is committed as source.
```

**Gate B is the one that makes this work**, and it is Fulminate's contribution ([`07.4`](07-separation-logic.md)) applied to a use case Fulminate was not designed for. A generated `__counted_by(n)` compiled into a run-time assertion and run against the project's own test suite is validated against real executions in seconds. A generated annotation that survives gate B is not proved correct, but it is not a hallucination either.

**Gate C is the economic filter.** A correct annotation that discharges nothing is noise in a patch review. Only proposals that move the count get proposed to a human.

## 9.5 Reproducibility: generated artifacts are source

**A build never invokes a model.** Generated annotations are committed to the repository, reviewed like any other patch, and thereafter are ordinary source. This is stated in [`00`](00-README.md)'s settled list and it is worth the emphasis because the alternative is seductive and wrong.

If generation happened at build time: builds would not be reproducible; the same source would produce different performance on different days; a model outage would change compilation; `-fsafety-proof-explain`'s answer to "why is there no check here" would be unanswerable; and the differential accounting of [`02.4.1`](02-the-goal.md) would compare two builds that were not comparing the same thing.

The corollary is that this is a **tooling** feature, not a compiler feature. It ships as `rucc-annotate`, a separate binary, outside the compiler's dependency graph, and the compiler has no idea it exists.

## 9.6 What is generated, and what is never

| Artifact | Generated? | Checked by |
|---|---|---|
| `__counted_by` and the rest of [`08.2`](08-annotations.md) | yes | gates A/B/C; then obligations at every call site |
| Loop invariants | yes | layer 2/4 must still prove the loop with it |
| `__nofree` | yes, when the definition is visible | verified against the definition |
| `__arena` triples | yes | gate B, plus the monitor at run time |
| Effects-table entries ([`08.6`](08-annotations.md)) | yes | **nothing**: these are trust-set entries; see §9.7 |
| CN-style specifications ([`07.3`](07-separation-logic.md)) | yes | layer 6 must prove the function with it |
| **Certificates** | **never** | none |
| **Discharge decisions** | **never** | none |
| **Trust-set membership** | **never** | none |
| **The obligation set itself** | **never** | none |

The bottom four rows are the trust boundary and they are absolute.

## 9.7 The one genuinely dangerous case

Effects-table entries are the exception in the table above and they need to be called out rather than buried.

An entry like `writes(dst, n) reads(src, n)` for an uninstrumented library function is **an assumption the monitor acts on**. There is no definition to check it against, that is why the entry exists. A wrong generated entry (`writes(dst, n)` where the function really writes `2n`) produces a **silent missed error**, which is the one failure mode this entire document set is organized to prevent.

So:

- Generated effects entries are marked `generated = true` in the TOML and **counted separately in the trust set**.
- They require a *reviewer* attestation to lose the marking, recorded in the file.
- `--emit=safety-summary` reports the count of unattested generated entries as its own line, because a program whose safety rests on forty unreviewed machine-written effect declarations should say so.
- **Default is to reject them**: `rucc-annotate` does not emit effects entries unless asked with `--unsafe-generate-effects`.

Everything else in the table is protected by the assume-check duality. This row is not, and the honest response is to make it awkward rather than to pretend the protection is uniform.

## 9.8 Measuring whether it was worth it

Reported per generation run, never as a success rate on a benchmark:

- proposals generated / passing gate A / passing gate B / passing gate C / accepted by a human;
- obligations discharged per accepted annotation;
- `R` before and after, on a workload;
- **and the count of proposals that passed gates A-C and were rejected in review as wrong**, which is the number that says how much the gates are actually filtering.

That last metric is the one to watch. If it is near zero, the gates work and review can be lightened. If it is large, generation is producing plausible-but-wrong artifacts that survive testing, which is the known failure mode, since a generated annotation is most likely to be wrong exactly on the paths the test suite does not cover, and those are the paths where the bugs are.

## 9.9 What this is not

**Not a code translator.** [`01.4`](01-research-2026.md)'s C-to-Rust line is adjacent and is a different product. We annotate C; we do not rewrite it.

**Not a bug finder.** A model asked "is this correct" produces an unfalsifiable answer. A model asked "what is this pointer's extent" produces a checkable one. Only the second question is asked.

**Not required.** Layers 0-3 run with zero annotations and produce most of the discharge in [`04.10`](04-the-discharge-ladder.md)'s funnel. If this document's machinery is never built, the specification's claims C1, C2 and C5 are unaffected; only C3's `deep` figure and [`07`](07-separation-logic.md)'s human cost improve. **This is the most cuttable document in the set** and it is placed here, after the ladder, so that cutting it removes nothing structural.

**Not a research contribution.** [`01.6`](01-research-2026.md)'s tools are ahead of anything proposed here. The contribution is the *placement*: a generator behind a hard trust boundary, feeding a compiler where a wrong answer costs a check.
