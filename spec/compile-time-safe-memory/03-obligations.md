# Obligations

The data structure the whole specification is about. One set, two dischargers, no third outcome.

## 3.1 Definition

An **obligation** is a proof goal attached to a program point:

```
Obligation {
    id:      ObligationId,    // stable across the pipeline; see 3.6
    site:    IrInst,          // the operation that generated it
    class:   Class,           // which conjunct of which judgement
    pred:    Predicate,       // the thing that must be true
    ctx:     Context,         // what may be assumed
    state:   State,           // Open | Discharged(Certificate) | Checked | Narrowed(..)
}
```

The set of obligations for a function is generated **once**, by [document 12](12-integration.md)'s `obligations` pass, immediately after `rucc-safety` insertion and before any optimization. It is generated from the judgements in [`../safe-memory/04.4`](../safe-memory/04-safety-model.md) mechanically (one obligation per conjunct per site) with no analysis, no cleverness, and no early discharge. Generation is dumb on purpose: it is the definition of the goal, and a generator that skipped "obviously true" obligations would be an unverified prover wearing the generator's clothes.

## 3.2 The classes

Derived by splitting J1-J7's conjuncts. This table is the specification's index: every later document is about moving rows from `Checked` to `Discharged`.

| Class | From | Predicate | Plane touched |
|---|---|---|---|
| `O.prov` | J1 | `cap(p) ≠ ⊥` | none (capability) |
| `O.live` | J1 | `cap(p).state = live ∧ cap(p).ver = lifetime_plane(addr)` | lifetime |
| `O.lo` | J1 | `cap(p).lo ≤ addr` | none |
| `O.hi` | J1 | `addr + n ≤ cap(p).hi` | none |
| `O.perm` | J1 | `kind ∈ cap(p).perm` | none |
| `O.align` | J1 | `addr mod align = 0` | none |
| `O.type` | J1 | `compatible(ty, type_plane(addr..addr+n))` | type |
| `O.init` | J1 | `kind = read ⇒ initialized(addr..addr+n)` | init |
| `O.derive` | J2 | `cap.lo ≤ addr' ∧ addr' ≤ cap.hi` (inclusive) | none |
| `O.free` | J6 | `state = live ∧ class = allocated ∧ addr = lo ∧ dealloc matches` | lifetime |
| `O.xfer` | J7 | `state ≠ device_owned` at every access in a transferred range | lifetime |
| `O.restrict` | 9.6 | no aliasing pair within the scope | none (block map) |
| `O.race` | 9.5 | metadata write ordered w.r.t. reader | epoch |

Two classes are deliberately absent. **J3 (expose/synthesize) generates no obligation** because it is a sanctioned downgrade, not a requirement, it generates a *counter*, per [`../safe-memory/04.4`](../safe-memory/04-safety-model.md), and a program's exposure count is reported alongside its discharge rate because the two are both measures of how much of the provenance argument survives. **J4 (begin lifetime) generates no obligation** because it is an action, not a predicate; what it generates is plane traffic, which §3.5 covers.

### 3.2.1 Where the count is

Predicted distribution, before any discharge, on ordinary C. **[unverified: to be measured at V1, and it is the first number the specification produces]**

| Class | Share of static obligations | Share of dynamic |
|---|---|---|
| `O.lo` + `O.hi` | ~40% | ~45% |
| `O.live` | ~25% | ~30% |
| `O.init` | ~15% | ~10% |
| `O.type` | ~12% | ~10% |
| `O.prov`, `O.align`, `O.perm` | ~7% | ~4% |
| everything else | ~1% | ~1% |

If that shape holds, spatial and temporal together are two thirds of the problem, which is why [documents 05](05-ownership-and-lifetimes.md) and [06](06-bounds-and-refinements.md) are the two long ones. If it does not hold, the ladder's layer ordering in [document 04](04-the-discharge-ladder.md) should be reordered to match what is actually measured, and this table exists so that the reordering is visibly driven by data.

## 3.3 The two dischargers, and the invariant

```
State ::= Open                      // during the pipeline only
        | Discharged(Certificate)   // static; costs nothing
        | Checked                   // dynamic; the monitor
        | Narrowed(Predicate, Certificate)   // partial; see 3.4
```

**The invariant, enforced by the IR verifier at every pass boundary after the discharge pipeline:**

> No obligation is in state `Open` at the point where checks are lowered. Every obligation is `Discharged`, `Checked`, or `Narrowed`, and every `Narrowed` obligation has a check emitted for its residual predicate.

This is the whole design in one rule, and putting it in the *verifier* rather than in a document is what keeps it true. A pass that drops an obligation (by deleting an instruction without accounting for the obligations attached to it, by rewriting a site and losing the link) trips the verifier in debug builds rather than silently deleting a safety check. Given [`../safe-memory/07`](../safe-memory/07-check-elimination.md)'s observation that an unsound elimination is invisible in testing, an assertion that makes it visible is worth more than a great deal of proof machinery.

**There is no `Failed` state.** A prover that cannot discharge an obligation does not record failure; it simply does not change the state, and the obligation reaches lowering as `Checked`, which is its initial value. This is not a naming quibble: a `Failed` state invites a diagnostic, a diagnostic invites a flag to promote it to a warning, and a warning list is the thing [`00`](00-README.md) says we do not have.

## 3.4 Partial discharge, which is where much of the value is

An obligation's predicate is usually a conjunction, and proving one conjunct while failing another should make the check *cheaper*, not leave it unchanged. That is the `Narrowed` state and it is underexploited by every tool in [document 01](01-research-2026.md), all of which treat a verification condition as pass/fail.

Three cases that matter, in descending order of expected value.

**Bounds, one side.** `O.lo` and `O.hi` are separate classes for exactly this reason. Loop induction gives the lower bound almost free (`i ≥ 0` from the initializer and the monotone update) while the upper bound needs the loop guard related to the object's extent. Discharging `O.lo` alone halves the compare count in the hottest obligation class in the table. Fil-C and ASan both emit a two-sided check unconditionally; a one-sided check is a direct, easily-verified win.

**Liveness, the version compare but not the state read.** `O.live` is `state = live ∧ ver = plane(addr)`. Proving no free can occur between two accesses (the `nofree` summary of [`../safe-memory/08.8`](../safe-memory/08-temporal-safety.md)) discharges the *second* access's plane load while leaving the first. The plane load is the expensive half, because it is the memory traffic §2.2.1 says dominates. **This is the single highest-value narrowing in the specification** and it is a layer-3 interprocedural fact, not a deep proof.

**Type, the granule but not the byte.** `O.type` over an *n*-byte access decomposes into per-granule checks. Proving the access is granule-aligned and the granule homogeneous ([`../safe-memory/17`](../safe-memory/17-open-questions.md) question 6) reduces a range check to a single word compare.

Narrowing composes with the ordinary optimizer, which is the reason [document 12](12-integration.md) runs discharge *before* the ægraph rather than after: a narrowed check is ordinary arithmetic and the middle end will CSE, hoist and fold it like anything else.

## 3.5 Plane traffic is not an obligation, and this is the specification's sharpest edge

An obligation is a *predicate to prove*. The plane writes that J4, J5 and every store perform are *actions to elide*, and no amount of proving makes them go away by itself.

Concretely: prove every `O.type` obligation on an allocation and the type-plane *reads* die. The type-plane *writes* (one per store, maintaining a plane nothing now reads) remain until an ordinary dead-store analysis notices no reader remains.

So the pipeline needs a step no verification tool has, because no verification tool emits code:

> **Plane-liveness elision.** After discharge, a plane is dead over a storage instance's range if every obligation that reads it, over every access to that instance in the analyzed scope, is `Discharged`. The maintenance code for a dead plane is then removed.

This is why §2.2.1 says whole-*object* discharge is the unit of value rather than whole-program discharge, and it is why one un-analyzable escaping use of an object can cost more than a hundred proved accesses elsewhere: a single surviving reader keeps the whole plane alive. It also means `R` and `D_d` decouple, which is [document 15](15-open-questions.md)'s first question.

The corollary for [document 05](05-ownership-and-lifetimes.md): **escape analysis is worth more here than ownership inference**, because escape is what decides whether plane maintenance can be elided, and it is enormously cheaper to compute.

## 3.6 Identity, and why obligations must survive the optimizer

`ObligationId` is stable from generation to lowering. It has to be, for four consumers:

1. **The verifier's no-`Open` invariant** (§3.3), which needs to enumerate the set at a pass boundary.
2. **`--emit=safety-summary`**, which reports per-class, per-layer discharge counts and must not double-count an obligation duplicated by loop unrolling or inlining.
3. **Differential accounting** ([`02.4.1`](02-the-goal.md)), which compares the report sets of a `proof=off` and a `proof=deep` build and needs to say *which* obligation diverged.
4. **`-fsafety-proof-explain`**, which answers "why is there no bounds check on line 412" with the certificate.

Duplication is the interesting case. Inlining, unrolling, loop peeling and tail duplication all copy sites. The rule: **a copied obligation gets a fresh id and records its parent**, so the summary reports 1 source obligation with *k* instances, and an obligation discharged in the peeled first iteration but not in the loop body is reported honestly rather than as a partial success on one number.

This forces obligations to be *IR-attached metadata that passes are required to maintain*, exactly like debug-info source locations and with the same failure mode when a pass forgets. The parent already has that machinery and the same discipline applies; [document 12](12-integration.md) §12.2 lists the passes that must be taught.

## 3.7 Context: what a prover may assume

`ctx` is the second half of a proof goal and getting its definition wrong is how unsound provers happen.

A prover discharging obligation *o* may assume:

- **The path condition** reaching `o.site`, the conjunction of branch conditions on every path from entry, or a sound over-approximation.
- **Facts established by dominating obligations that are themselves `Checked`.** This is the subtle one and it is sound: a `Checked` obligation's predicate *holds on every path that continues past it*, because the check traps otherwise. A dominating bounds check is an assumption, and this is precisely the mechanism [`../safe-memory/07`](../safe-memory/07-check-elimination.md)'s redundancy elimination already uses, restated as a proof rule.
- **Facts established by dominating obligations that are `Discharged`**: a proof is at least as strong as a check.
- **Annotations** ([document 08](08-annotations.md)) on entities in scope, *including annotations on the enclosing function's parameters*, because those become obligations at every call site.
- **The parent's UB table** ([`../safe-memory/04.7`](../safe-memory/04-safety-model.md)). Signed overflow does not wrap; `restrict` does not alias when `-fsafety-races` is not proving otherwise. This makes the prover's assumptions *identical* to the optimizer's, which is a correctness requirement and not merely tidy: a prover that assumed less than the optimizer would prove things the optimized code does not satisfy.

A prover may **not** assume:

- That an unproved obligation holds. Circularity; the reason `Open` is not `true`.
- That memory not written by the analyzed code is unchanged, across a call that may free, a signal handler, or another thread, unless the fact came from a call summary that establishes it.
- **Anything a machine-generated artifact asserts.** [Document 09](09-inference-and-llm.md); an inferred annotation is a *proposal* that becomes an assumption only at the call sites where it has itself been discharged as an obligation.

### 3.7.1 The assume-check duality

Every annotation in [document 08](08-annotations.md) is simultaneously an assumption inside a function and an obligation at every call site. `void f(int *__counted_by(n) p, size_t n)` lets the body assume `p` has extent `n`, and generates an obligation at each caller that it does. This is the only way annotations can be sound while remaining optional: an unproved caller-side obligation becomes a check, the check traps if the annotation was a lie, and **a wrong annotation therefore causes a false positive at run time rather than a missed error**: a loud failure, not a silent one.

That property is what licenses [document 09](09-inference-and-llm.md)'s machine-generated annotations. A model that hallucinates `__counted_by(n)` on a pointer that is really `n/2` long produces a trap in testing, not a vulnerability in production.

## 3.8 What the summary reports

Per translation unit, per class, from `--emit=safety-summary`:

```toml
[obligations.O_hi]
generated      = 18442
discharged     = 13901
narrowed       = 1204
checked        = 3337
by_layer       = { frontend = 8112, interval = 4102, relational = 1489, refine = 402 }
dyn_weighted   = { discharged = 0.91, checked = 0.09 }   # only with a profile

[planes]
type_elided_bytes  = 4102992
lifetime_elided_bytes = 118400
```

The `planes` block is the one to read first, per §3.5, and it is the one no other tool prints. A summary showing a 90% discharge rate and zero elided plane bytes is a report of work that did not pay.
