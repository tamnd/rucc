# Residual and composition

How the prover and the monitor fit together, what the leftover looks like, and why the leftover has to be predictable.

## 11.1 The no-silent-gap rule

> **Every obligation not `Discharged` is `Checked`, the check is emitted, and the emitted check is exactly the check the monitor would have emitted without the prover, or a narrowing of it that is implied by a certificate.**

Three consequences, each of which is a design constraint somewhere else:

- The prover cannot weaken the monitor. [`10.7`](10-soundness-and-trust.md)'s no-new-escape-hatches.
- The prover cannot *strengthen* the monitor either. A proof does not add a report; if the prover discovers that an obligation is definitely false on some path, it does not turn that into a compile error, it leaves the check, which traps. §11.6 explains why.
- The residual is a function of what was proved, and nothing else. No heuristic decides "this check is probably fine."

## 11.2 What the residual looks like

Four shapes, in order of frequency.

**Untouched check.** The obligation was never discharged. `rucc-safety`'s lowering emits the same compare-and-branch it would have at `-fsafety-proof=off`.

**Narrowed check.** [`03.4`](03-obligations.md). One side of a bounds pair, a version compare without the plane load, a granule compare instead of a range check. Emitted from the residual predicate recorded in `Narrowed(pred, cert)`.

**Hoisted check.** [`06.3`](06-bounds-and-refinements.md)'s loop splitting: the obligation is discharged *inside* the loop conditional on a predicate checked *outside* it. Two obligations replace one (an outer `Checked` and an inner `Discharged`) with the id-parentage of [`03.6`](03-obligations.md) recording the relationship, so the summary reports one source obligation with a hoisted disposition rather than a mysterious extra check.

**Absent check with live plane.** The obligation is discharged but the plane it read is still maintained because another obligation on the same object survives. [`03.5`](03-obligations.md)'s case, the most common outcome on heap data, and the reason `R` lags `D_d`.

## 11.3 Predictability, and why timeouts must be deterministic

[`02.8`](02-the-goal.md)'s failure condition F5: if which obligations survive depends on machine load, the feature is unusable in production even when it is fast on average. A build that is 8% slower today than yesterday because the CI machine was busy and a solver timed out is a build nobody can reason about.

**The rule: no wall-clock timeouts anywhere in the discharge pipeline.**

Budgets are expressed in **deterministic step counts**: fixpoint iterations, octagon closure operations, SMT `rlimit` rather than seconds, candidate-qualifier rounds, call-graph summary passes. Every one of these is a property of the input, not of the machine.

Consequences, all of which are worth the cost:

- `-fsafety-proof=deep` on the same source with the same compiler produces byte-identical output on any machine.
- Certificate sets are diffable across builds ([`10.3`](10-soundness-and-trust.md)), which they would not be otherwise.
- Differential accounting ([`10.6`](10-soundness-and-trust.md)) compares like with like.
- A performance regression has a cause that can be found.

**The cost is real:** step budgets must be tuned per layer and they will occasionally be wrong in both directions, cutting off a proof that would have finished in 3ms and permitting one that takes 400ms. That is the correct trade. Reproducibility of the *output* matters more than tightness of the *bound*.

`-fsafety-proof-time-budget=N` exists for users who want to trade determinism for a wall-clock guarantee, and it prints a warning that the build is no longer reproducible.

## 11.4 The two eliminators problem, and its resolution

The companion specification has its own check-elimination pass, [`../safe-memory/07`](../safe-memory/07-check-elimination.md), the `safety-dce`, `safety-loop` and `safety-plane` passes of [`../safe-memory/15.3`](../safe-memory/15-integration.md), built on SMT-verified rewrite rules over the dominator tree. This specification has a discharge ladder whose layers 0-3 do substantially overlapping work.

Two independent mechanisms removing the same checks would be a maintenance disaster and a soundness risk: each would have to be correct on its own, and their interaction would be untested.

**The resolution: `safety-dce` becomes a client of the obligation model rather than a peer.**

Concretely, the companion's elimination passes are re-expressed as **layer 0 and layer 1 of this ladder**. The rewrite rules in `rucc-codegen/rules/safety/` become layer-0 `Syntactic` certificate rules and layer-1 `Dominance` rules, unchanged in content and unchanged in verification method, `rucc-verify` still proves each one, exactly as [`../safe-memory/14.2`](../safe-memory/14-verification.md) specifies. What changes is that they now emit certificates and are counted.

This is the reverse of the naive integration, in which the prover would run and then the existing eliminator would clean up. It is better for four reasons: there is one mechanism, so there is one thing to verify; the accounting is complete rather than covering only the new half; [`../safe-memory/14.3`](../safe-memory/14-verification.md)'s differential check accounting and [`10.6`](10-soundness-and-trust.md)'s differential proof accounting become the same test; and the companion's `-fsafety-proof=off` build is then genuinely check-everything, which is what makes it a valid reference.

**The cost to the companion specification**: [`../safe-memory/15.3`](../safe-memory/15-integration.md)'s pipeline gains an obligation-generation pass and its three elimination passes gain a certificate output. Nothing is deleted and no decision is reversed, which is consistent with that document's claim that nothing reverses a parent decision. [Document 12](12-integration.md) §12.5 states the diff precisely.

## 11.5 Pass ordering

The contract with the optimizer, extending [`../safe-memory/15.3`](../safe-memory/15-integration.md):

```
  rucc-lower        TAST → IR
+ rucc-safety       insertion: checks, plane maintenance, aux traffic
+ obligations       generate the obligation set (dumb, complete)
+ discharge L0      syntactic
  rucc-opt mem2reg  promotion; many obligations vanish with their locals
+ discharge L1      intervals and dominance
  rucc-opt inline   caller facts reach callee obligations
+ discharge L1'     rerun on the inlined bodies
+ discharge L2      relational; loop splitting and peeling
  rucc-lto          summaries
+ discharge L3      interprocedural
+ discharge L4-6    deep / verify only
+ plane-liveness    elide dead plane maintenance          ← the R pass
  rucc-opt ægraph   the ordinary middle end
+ certificate check verify every certificate
+ obligations       verify the no-Open invariant
+ rucc-safety lower checks → compares and branches
  rucc-codegen      selection, scheduling, frames
```

**Four ordering decisions that are not arbitrary:**

*Generation before `mem2reg`, not after.* Obligations must be generated on the un-promoted IR so that the set is complete and mechanical. Promotion then deletes many of them along with their memory operations, and that deletion is *accounting*, not discharge, the summary reports promoted-away obligations separately, because a program with 40% of its obligations promoted away has a different profile from one with 40% proved.

*Discharge before the ægraph, not after.* [`../safe-memory/17`](../safe-memory/17-open-questions.md) question 2 notes that checks live in the CFG skeleton and outside the e-graph. Narrowed checks are ordinary arithmetic, so running discharge first lets the e-graph CSE, fold and share whatever the narrowing produced. Running it after would leave that value on the table.

*Plane-liveness after all discharge and before the ægraph.* It is the pass that converts discharge into performance ([`03.5`](03-obligations.md)) and it needs the complete discharge picture; the ægraph then cleans up the newly-dead address arithmetic.

*Certificate checking last, on the final IR.* Checking early would validate certificates against IR that later passes rewrite. Checking at the end means the checker sees exactly what the backend will lower, which is the only version that matters.

## 11.6 Report quality and attribution

Two effects on the user-visible behavior of the monitor, both minor and both worth stating because a surprise here reads as a bug.

**A discharged obligation is not a report site.** If a program somehow violated a discharged obligation, nothing would report it, that is the soundness claim, and if it fails, [`10.6`](10-soundness-and-trust.md) catches it. No user-visible change when the prover is correct.

**Which check fires first can change.** A hoisted check (§11.2) traps *before* the loop rather than on iteration 900. The violation is the same; the reported line moves. [`../safe-memory/08.5`](../safe-memory/08-temporal-safety.md) already admits report quality for temporal violations is best-effort; this adds a small spatial case.

The mitigation is that the hoisted check's `.rucc_safety_desc` record carries the *original* obligation's source location alongside the hoist's, so the report reads:

```
error: out-of-bounds write
  at foo.c:412 (write of 4 bytes at offset 4096 of a 4096-byte object)
  check hoisted from the loop at foo.c:410-414
```

**Why the prover does not report definite violations.** If layer 2 proves `i ≥ extent(a)` on some path, that path has a bug, and the prover stays silent, because the check will trap and report it precisely, at run time, with the actual values. Turning it into a compile-time diagnostic would mean a diagnostic that fires on unreachable paths, which is the false-positive problem that [`00`](00-README.md)'s no-alarms rule exists to avoid. `-fsafety-proof-explain` will show it to anyone who asks.

## 11.7 Composition with the tiers

| Companion tier | Which layers run | What the prover mainly buys |
|---|---|---|
| **D** (detect) | 0-3 at `default` | modest: type and init planes discharge poorly and dominate D's cost |
| **E** (enforce) | 0-3, or 0-5 in release | **the reason for the specification**; [`02.5`](02-the-goal.md) |
| **K1/K2** (kernel) | 0-3, plus §[5.6](05-ownership-and-lifetimes.md)'s RCU intervals | temporal discharge in RCU sections; `__counted_by` refinements |
| **K3** (MTE) | 0-1 | little; the hardware already made it cheap |
| **V** | none | this specification *is* Tier V |

**The hardware interaction, worth a note.** Under Tier K3's MTE lowering, a bounds check is a tag store rather than a compare, so discharging it saves a store rather than a branch, a different and generally smaller saving. Under a future CHERI lowering ([`../safe-memory/05.4`](../safe-memory/05-representation.md)), bounds are checked by the hardware on every access at zero marginal cost, and **static bounds discharge buys nothing at all**. That is not an argument against the work; it is an observation that the value of this specification is inversely proportional to how much the hardware does, and on the hardware that exists today the hardware does very little.

## 11.8 Separate compilation and LTO

Layer 3 is where discharge becomes link-configuration-dependent, and this needs to be said plainly because it surprises people.

- **Without LTO**, summaries are module-local. A call into another translation unit is `nofree`-unknown unless annotated, which per §[5.2](05-ownership-and-lifetimes.md) closes the free-free interval and costs temporal discharge at every such call. Codebases with many small translation units and chatty cross-module calls will discharge materially worse.
- **With LTO**, summaries cross the whole link unit and layer 3 does its real job.
- **Shared libraries are always a boundary.** A call into a `.so` is never `nofree` without an annotation, ever, regardless of LTO.

So `-fsafety-proof`'s effectiveness depends on build configuration, and the summary reports which regime it ran in. The practical guidance (LTO plus `__nofree` on hot library APIs) is the same advice that makes the companion's temporal checking affordable, so the two specifications recommend the same build configuration for the same reason.

## 11.9 The determinism claims

**D1.** For fixed source, flags and compiler version, the set of `Discharged` obligation ids is identical across machines and runs. Tested by building the corpus twice on different hosts and diffing certificate sets.

**D2.** Adding an annotation never *decreases* the discharge set. Monotonicity of the annotation surface; tested by an annotation-ablation run over the corpus.

**D3.** Raising `-fsafety-proof` never decreases the discharge set. Same test, over levels.

D2 and D3 are not automatic, an annotation could change layer 4's inference in a way that loses an unrelated invariant, and a higher level could change pass ordering effects. Where they fail, the failure is a bug, and the tests exist because these are the properties users will assume without being told.
