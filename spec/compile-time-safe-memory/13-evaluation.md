# Evaluation

What is measured, on what, and what would falsify the specification.

The corpus is the companion's, [`../safe-memory/12.2`](../safe-memory/12-corpus-and-evidence.md)'s tier-1 list and [`../safe-memory/12.3`](../safe-memory/12-corpus-and-evidence.md)'s CVE suite. Building a second corpus would be waste, and using the same one means every measurement here is directly comparable to a measurement there.

## 13.1 The deliverable

Stated first because it is easy to lose under the metrics.

> **No source found in [document 01](01-research-2026.md) reports what fraction of a real C program's memory-safety obligations can be discharged statically.** Nobody has published it, because nobody has built a system that generates a complete obligation set and then reports what happened to all of it.

The primary deliverable of this specification is that number, per class, per layer, over a public corpus, with the residual counted. It is worth publishing even if `R` disappoints, and it should be published in the form that makes it reusable: **the obligation sets themselves, as data**, so that another tool can be measured against the same denominator.

That last point is the one that makes this a contribution rather than a benchmark run. A discharge rate is only meaningful relative to a denominator, and the field has no agreed denominator at all, which is why Astrée's "zero false alarms," CCured's "most pointers," CHOP's "80% of dynamic checks" and Flux's "zero annotations" cannot be compared to one another.

## 13.2 Required metrics

Every corpus build reports, and nothing is published without all of them:

| Metric | From | Note |
|---|---|---|
| `D_s` per class per layer | `--emit=safety-summary` | [`03.8`](03-obligations.md) |
| Residual check count | same | the denominator's other half |
| Obligations promoted away by `mem2reg` | same | accounted separately, never as discharge |
| `D_d` per class | counting build + workload | only where a workload exists |
| **`R`** | three timed builds | [`02.2`](02-the-goal.md); the headline |
| Plane bytes elided, per plane | same | [`03.5`](03-obligations.md); explains `R` |
| Compile time, per layer | pass timers | against the 2x budget |
| Peak compiler RSS | none | a prover that needs 8 GB is not shippable |
| Certificate count, size, check time | none | [`10.2`](10-soundness-and-trust.md)'s F4 |
| Trust-set counts | `--emit=safety-summary` | assumptions, `__unsafe_indexable`, generated effects |
| Annotation counts and their attributed discharge | same | [`08.9`](08-annotations.md) |

**The reporting rules of [`../safe-memory/13.4`](../safe-memory/13-performance.md) apply unchanged**, with one addition: **no discharge rate is ever reported without the residual count and the plane-elision figure next to it.** A 90% discharge rate with zero plane elision is a report of work that did not pay, and the format must make that visible rather than requiring the reader to notice its absence.

## 13.3 The five measurements that decide the design

Each is placed at the earliest milestone where it is possible, per the companion's ordering principle that a measurement which can invalidate a number happens before the work depending on it.

**M1, The obligation distribution.** [`03.2.1`](03-obligations.md)'s predicted shares. Requires only generation, no discharge. **First thing built, first thing measured**, because it decides the ladder's layer ordering and the whole document set assumes it.

**M2, `R/D_d`.** [`02.2.1`](02-the-goal.md)'s central risk and [`02.8`](02-the-goal.md)'s failure condition F1. Measurable with layers 0-1 only: discharge what is easy, run plane-liveness, measure. **If this comes back near zero, stop and redesign the representation instead.** Scheduled at V2, deliberately before layers 2-6 exist.

**M3, Init-plane elision coverage.** [`06.10`](06-bounds-and-refinements.md)'s claim B4, and the cheapest large contributor to `R`. Also measurable at V2, and it is the best early evidence for or against M2's answer generalizing.

**M4, Layer 2's yield on the S2 loop shape.** [`06.10`](06-bounds-and-refinements.md)'s B2. If loops do not discharge, nothing else matters. V3.

**M5, Layer 5's temporal yield.** [`05.10`](05-ownership-and-lifetimes.md)'s T4, the explicit cut criterion. V4.

## 13.4 Soundness testing

Three techniques, in ascending strength, all inherited or adapted from [`../safe-memory/14`](../safe-memory/14-verification.md).

**Differential proof accounting.** [`10.6`](10-soundness-and-trust.md). Nightly, whole corpus plus CVE suite, `off` versus `deep`, report sets compared in both directions. Any divergence is a P0.

**Randomized violation injection.** The stronger technique, and the one that reaches paths the tests do not. Take a corpus program; pick a site whose obligation was `Discharged`; inject a violation of exactly that obligation (an index shifted past the bound, a pointer used after a free, a read of an unwritten byte); require the `deep` build to report it.

> **If a build with a proof enabled fails to report an injected violation at a site it claimed to have proved, the proof was unsound.**

This is the cleanest oracle in either specification: no expected output to maintain, no triage, a binary verdict, and a free second oracle in that a run with no injection must produce no reports. It should run continuously, and it is the primary defense for row 4 of [`10.4`](10-soundness-and-trust.md)'s trust set (the IR-to-logic encoding) because an encoder bug shows up as exactly this.

**Encoder differential testing.** [`10.5`](10-soundness-and-trust.md)'s mitigation 2: random IR fragments, encoded to formulas, satisfying assignments compared against concrete interpretation of the IR. Catches encoder bugs before they reach a discharge.

## 13.5 Compile time

Its own section because it is a first-class budget and because provers historically ignore it.

- Measured per layer, per file, as a multiple of the `-fsafety-proof=off` build of the same file.
- Reported as a distribution, not a mean. **The p99 file is what breaks a build, not the average one**, and a prover that is 1.4x on average and 40x on one 12,000-line generated parser is unusable.
- The step budgets of [`11.3`](11-residual-and-composition.md) are tuned against the p99, not the mean.
- **Every corpus project's full build time is reported at each level**, because that is the number a maintainer decides on.

**The falsifiable form:** at `default`, no tier-1 project's total build time exceeds 2.0x, and no single translation unit exceeds 4x.

## 13.6 The annotation ablation

The experiment that quantifies [document 08](08-annotations.md), and it is easy because the annotations already exist in the kernel.

Build the kernel (or any annotated project) three ways: with all `__counted_by` and friends, with them macro-defined away, and with machine-generated ones from [`09.4`](09-inference-and-llm.md)'s pipeline. Compare `D_s`, `R` and residual counts.

This produces three numbers nobody has:

- what annotation is worth, in recovered cost per annotated declaration;
- how far generated annotations close the gap to hand-written ones;
- whether [`11.9`](11-residual-and-composition.md)'s claim D2 (annotations never reduce discharge) actually holds in practice.

The first of those is the number that would justify or refute the kernel's ongoing `__counted_by` effort in performance terms rather than in safety terms, and it would be of interest well outside this project.

## 13.7 Comparison protocol

Against Astrée, Frama-C's Eva, Flux and the C-to-Rust tools, with a warning attached.

**These tools do not answer the same question.** Astrée and Eva produce alarm lists over a program; we produce a discharge count over an obligation set. A direct "who proves more" comparison is close to meaningless, and presenting one would be the kind of overstatement [`../safe-memory/14.6`](../safe-memory/14-verification.md) refuses for Juliet.

**What is comparable, and how:**

*Against Frama-C Eva*, on a small program both can handle: for each of our obligations, does Eva prove the corresponding ACSL assertion? This measures precision on a shared denominator, and Eva will win on precision and lose on speed by orders of magnitude. **Publish both directions.** The interesting output is the set of obligations Eva proves and we do not, because that is a list of techniques worth adopting.

*Against Flux*, not at all directly, different language. The comparable quantity is annotation burden: Flux reports reducing annotation from 9% average to zero on Rust; our ablation (§13.6) measures the same thing on C, and the honest expectation per [`06.4`](06-bounds-and-refinements.md) is that we do worse because C has no ownership to give strong updates.

*Against the C-to-Rust line*, not at all. Different product.

*Against the companion alone*: this is the only comparison that really matters, and it is `R`.

## 13.8 The claims, collected

Every falsifiable claim in the document set, in one place, with where it is measured.

| # | Claim | From | At |
|---|---|---|---|
| C1 | `D_s ≥ 0.70` spatial at `default` | [`02.6`](02-the-goal.md) | V3 |
| C2 | **`R ≥ 0.35` at `default`** | [`02.6`](02-the-goal.md) | V3 |
| C3 | `D_s ≥ 0.85` spatial at `deep`, under 10x compile | [`02.6`](02-the-goal.md) | V5 |
| C4 | zero differential divergences over a milestone | [`02.6`](02-the-goal.md) | continuous from V1 |
| C5 | `D_s ≥ 0.25` temporal at `default` | [`02.6`](02-the-goal.md) | V3 |
| C6 | one allocator fully discharged at `verify` | [`02.6`](02-the-goal.md) | V6 |
| T1 | flow + escape ≥ 40% of static `O.live` | [`05.10`](05-ownership-and-lifetimes.md) | V3 |
| T2 | ≥ 60% of `O.live` inside RCU sections | [`05.10`](05-ownership-and-lifetimes.md) | V5 |
| T3 | lifetime-plane elision ≥ 30% of allocated bytes | [`05.10`](05-ownership-and-lifetimes.md) | V3 |
| T4 | layer 5 ≥ 10% of temporal residue, or it is cut | [`05.10`](05-ownership-and-lifetimes.md) | V4 |
| B1 | layers 0-2 ≥ 70% of static bounds obligations | [`06.10`](06-bounds-and-refinements.md) | V3 |
| B2 | layer 2 ≥ 80% of S2 loop obligations | [`06.10`](06-bounds-and-refinements.md) | V3 |
| B3 | layer 4 ≥ 60% of S4/S6 with annotations | [`06.10`](06-bounds-and-refinements.md) | V5 |
| B4 | init-plane elision ≥ 50% of allocated bytes | [`06.10`](06-bounds-and-refinements.md) | V2 |
| B5 | **no** static discharge claimed for unbounded strings or synthesized pointers | [`06.10`](06-bounds-and-refinements.md) | none |
| D1 | discharge sets are machine-independent | [`11.9`](11-residual-and-composition.md) | V1 |
| D2 | annotations never reduce discharge | [`11.9`](11-residual-and-composition.md) | V3 |
| D3 | higher levels never reduce discharge | [`11.9`](11-residual-and-composition.md) | V3 |
| none | build time ≤ 2.0x at `default`, ≤ 4x per TU | §13.5 | V3 |

**C2 is the claim to watch.** Everything else can hold while C2 fails, and if it does, [`02.8`](02-the-goal.md)'s F1 has fired and the honest conclusion is that this specification's premise was wrong.

## 13.9 What will be published even if it is bad

Committed in advance, so that selective reporting is not available later:

- The full obligation distribution, per project, including projects where discharge was poor.
- `R` per benchmark with the worst case named, never a geomean alone.
- Every claim in §13.8 marked met, missed or unmeasured.
- The compile-time p99, not the mean.
- The count of trust-set entries per project, which is the number that says how much of the "proof" rests on assumptions.
- **The gap between the predicted funnel of [`04.10`](04-the-discharge-ladder.md) and the observed one**, which is the most informative single number the project will produce, in the same way [`../safe-memory/14.7`](../safe-memory/14-verification.md) treats the ACSAC replay's predicted-versus-observed gap.
