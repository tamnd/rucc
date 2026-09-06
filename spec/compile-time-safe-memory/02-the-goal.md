# The goal

What compile-time proof can honestly mean, how it is measured, what the user selects, and the conditions under which this specification is wrong and should be abandoned.

The companion's [document 02](../safe-memory/02-the-goal.md) does the same job for the monitor and its discipline is inherited: state the claim, state the metric that would falsify it, state the failure conditions before starting.

## 2.1 What "proved" means here

> **An obligation is discharged statically when the compiler holds a certificate that, for every execution reaching that program point, the obligation's predicate holds, and the certificate is checkable by a component in the trust set of [document 10](10-soundness-and-trust.md) without re-running the analysis that produced it.**

Three things that definition deliberately excludes.

**It is not "the analysis found no counterexample."** Bounded model checking, fuzzing and unsound-but-useful heuristics all produce that, and none of them produce a certificate. They may propose ([document 09](09-inference-and-llm.md)); they may not discharge.

**It is not scoped to a well-behaved subset of C.** The obligation set is generated from the whole program, including the parts no analysis will touch. A function full of inline assembly and `intptr_t` arithmetic generates its obligations exactly like any other, and they all become run-time checks. This is the difference between a discharge rate and a coverage claim: [`../safe-memory/04.5`](../safe-memory/04-safety-model.md)'s three escape hatches (coverage, boundary, declared exemption) are the *monitor's*, and this specification adds none of its own. **A prover cannot create a hole, because failing to prove something leaves a check behind.**

**It is not a statement about the compiled program's absence of bugs.** It is a statement about one predicate at one site. The conjunction of all of them, over a fully instrumented program, is memory safety in the companion's sense, but only because the monitor covers the residue.

### 2.1.1 The soundness obligation, stated once

If an obligation is discharged statically and the predicate is false at run time, the program has an unchecked memory error and nothing observes it. That is the *only* way this specification can cause harm, and every design decision in [document 10](10-soundness-and-trust.md) exists to make the surface where it can happen small enough to audit.

Everything else (a missed proof, a timeout, a wrong annotation, a hallucinated invariant, a domain that widens to top on the first loop) costs performance and nothing else.

## 2.2 The metric

The number this specification exists to move is the **discharge rate**: the fraction of obligations discharged statically. It has three forms and only one of them is honest on its own.

**Static discharge rate** `D_s`, discharged obligations divided by total obligations, counted over the program text. Easy to compute, reportable from a single build, and **systematically misleading**, because obligations are not equally expensive. A thousand discharged obligations in cold error-handling paths and one surviving check in the innermost loop of a decompressor is a `D_s` near 1.0 and a performance win near zero.

**Dynamic discharge rate** `D_d`, the same fraction weighted by execution count, measured by instrumenting a build to count obligation *sites* reached rather than checks executed. This is the number that predicts performance and it requires a workload, which means it is a property of the (program, input) pair and not of the program.

**Recovered cost** `R`, the one that actually matters, and the one nobody in [document 01](01-research-2026.md) reports. Let `T_off` be the safety-off build, `T_mon` the monitor with no static discharge, and `T_both` the monitor with proof enabled. Then

```
R = (T_mon - T_both) / (T_mon - T_off)
```

`R = 1` means proof recovered the entire cost of safety. `R = 0` means it recovered nothing. **`R` is the headline metric of [document 13](13-evaluation.md)** and every claim in this specification is ultimately a claim about it.

### 2.2.1 Why `R` can be much lower than `D_d`, and why that is the specification's central risk

[`../safe-memory/05.5`](../safe-memory/05-representation.md) predicts that this design's cost is dominated by **memory traffic to the metadata planes, not by the check instructions**: a prediction [`../safe-memory/13`](../safe-memory/13-performance.md) opens by restating. A discharged check removes a compare and a branch. It does not, by itself, remove the plane read that fed it, the aux slot store that maintained the capability, or the cache line that both touched.

So a program could plausibly reach `D_d = 0.9` and `R = 0.3`.

Proof recovers cost only when it discharges *every* consumer of a piece of metadata, at which point the metadata traffic itself becomes dead and the ordinary optimizer deletes it. That makes **whole-object discharge**, not whole-program discharge, the unit of value: proving all obligations on one allocation is worth more than proving 80% of the obligations on five.

This reorients the entire design. [Document 04](04-the-discharge-ladder.md)'s layer ordering, [document 06](06-bounds-and-refinements.md)'s preference for relational domains over interval domains, and [document 12](12-integration.md)'s placement of the discharge pass *before* the aux-elision pass rather than after all follow from it. It is also open question 1 in [document 15](15-open-questions.md), because **nobody has measured the ratio `R/D_d` for any system, ever**, and if it is low then a high discharge rate is a vanity metric.

## 2.3 The measurement protocol

Inherited from [`../safe-memory/13.2`](../safe-memory/13-performance.md) and extended:

- Baseline is `rucc -O2` with safety off, same commit, same flags otherwise.
- `D_s` is reported from `--emit=safety-summary` on every corpus build, per obligation class (J1-J7), per discharge layer.
- `D_d` requires a counting build and is reported only for benchmarks with a defined workload.
- `R` is reported per benchmark, never as a geomean alone, with the worst case named.
- Compile time is reported alongside, always, per layer. A layer that doubles compile time to move `R` by two points is a failure regardless of what it proves.
- **Nothing is reported without the residual count.** "97% discharged" and "3% discharged" are the same sentence with different framing unless the denominator and the surviving check count are printed next to them.

## 2.4 What the user selects

Four proof levels, orthogonal to the companion's tiers. The flag is `-fsafety-proof=`.

| Level | Layers | Compile time | For |
|---|---|---|---|
| `off` | none | 1.0x | debugging the monitor; the differential-accounting reference build |
| `fast` | 0-1 | ≤1.3x | default at `-O0`/`-O1`; the frontend and interval layers only |
| `default` | 0-3 | ≤2.0x | default at `-O2` and above |
| `deep` | 0-5 | unbounded | release builds, the corpus, and anything shipped |
| `verify` | 0-6 | unbounded | opt-in per function via annotation; separation logic |

The **2x compile-time budget on `default`** is a hard design constraint and not an aspiration. It is the number at which a build system stops being usable, and every layer in [document 04](04-the-discharge-ladder.md) has a time budget within it. A layer that exceeds its budget is demoted to `deep`, not granted more time.

`deep` is unbounded because release builds and CI are already minutes-to-hours and a prover that takes ten minutes on a library is acceptable there. `verify` is unbounded because it runs on functions a human has explicitly volunteered.

### 2.4.1 The composition rule

`-fsafety-proof` never changes program semantics. For any level *L*, a program built at `-fsafety=detect -fsafety-proof=L` reports **exactly the same set of memory errors** as one built at `-fsafety-proof=off`, differing only in speed, assuming the prover is sound, which is precisely what [document 14](14-milestones.md)'s differential accounting tests on every nightly build.

This is the same property as [`../safe-memory/14.3`](../safe-memory/14-verification.md)'s differential check accounting, applied one level up, and it is the highest-value test in *this* specification for the same reason it is there: it turns an unobservable soundness bug into an observable diff.

## 2.5 What this does for each companion tier

| Companion tier | Without proof | With proof, target | Why |
|---|---|---|---|
| **Tier D** (detect, dev builds) | 2-3x | 1.6-2.2x | modest; D builds run all planes and the type/init planes discharge poorly |
| **Tier E** (enforce, production) | 1.3x budget, [13.3](../safe-memory/13-performance.md) predicts 1.27-1.53x | **the budget becomes reachable rather than optimistic** | this is the point |
| **Tier K1** (kernel, full) | 5-8x | 3-5x | kernel code is mostly bounded loops over fixed structures, which is the best case for interval and relational domains |
| **Tier K3** (kernel, MTE) | <20% | <15% | little left to recover |
| **Tier V** | the name for the discharged set | **this specification is its implementation** | none |

The row that justifies the work is Tier E. [`../safe-memory/13.3`](../safe-memory/13-performance.md) decomposes the Tier E cost as 27-53% against a 30% budget and admits the budget sits at the optimistic end of its own prediction, which is a polite way of saying the tier is more likely to miss than hit. **This specification is what moves the distribution.**

The row that is quietly most interesting is K1. Kernel C is unusually amenable: bounded loops, `__counted_by` annotations already in the tree, few recursive structures on hot paths, and (as [`../safe-memory/11.3`](../safe-memory/11-kernel.md) notes) an existing annotation surface we consume rather than invent. If relational domains work anywhere, they work there.

## 2.6 The falsifiable claims

Stated so they can be checked against, in the form [`../safe-memory/02`](../safe-memory/02-the-goal.md) uses.

**C1.** On the tier-1 corpus of [`../safe-memory/12.2`](../safe-memory/12-corpus-and-evidence.md) at `-fsafety-proof=default`, **`D_s ≥ 0.70` for spatial obligations (J1, J2)** with compile time under 2x.

**C2.** On the same corpus at the same level, **`R ≥ 0.35`**: proof recovers at least a third of the monitor's cost. This is the claim that matters and it is the one most likely to fail, for the reason in §2.2.1.

**C3.** At `-fsafety-proof=deep`, **`D_s ≥ 0.85` for spatial obligations**, with the additional layers costing under 10x compile time.

**C4.** **Zero divergences** between `-fsafety-proof=off` and `-fsafety-proof=deep` builds on the corpus and the CVE suite, in either direction, over a full milestone period.

**C5.** For temporal obligations (J3, J4), **`D_s ≥ 0.25`** at `default`. Deliberately low, for the reason §2.7 gives.

**C6.** For at least one nontrivial allocator, the plan is musl's `mallocng` or the kernel's buddy allocator, following [CN's pKVM result](https://dl.acm.org/doi/10.1145/3571194), **every obligation in the allocator's own code is discharged at `verify`**, so the allocator runs with no residual checks. An allocator that must be trusted anyway ([`../safe-memory/10.4`](../safe-memory/10-boundaries.md)) is far more valuable proved than checked.

## 2.7 The pessimistic prediction, stated up front

**Temporal obligations will discharge poorly and this specification says so before measuring rather than after.**

The evidence is in [document 01](01-research-2026.md) §4 and it is the ownership-inference tradition's own evidence, not a skeptic's. Scylla targets an *applicative subset* because full C does not statically own. &inator's correctness criterion is an existence property modulo *dynamic* borrowing conflicts and leaks, the two things it cannot statically rule out. Cpp2Rust abandoned static ownership entirely and inserted run-time checks, paying 6x on pointer-arithmetic-dense code. Astrée excludes dynamic allocation from its scope. seL4 proves absence of use-after-free on ten thousand purpose-written lines.

Every one of those is the same finding from a different direction: **lifetime is the property real C does not carry statically.** [Document 05](05-ownership-and-lifetimes.md) is written accordingly, targeting the cases that *do* discharge (stack locals that do not escape, allocations that dominate their frees with no intervening call that could free, `nofree` call summaries) rather than pretending to a general result.

The consolation is structural and it is real: [`../safe-memory/08.8`](../safe-memory/08-temporal-safety.md) says `nofree` summaries are the difference between temporal checks costing 5% and 40%, and a `nofree` summary is a *cheap interprocedural fact*, not a proof of ownership. So the highest-yield temporal work is at layer 3, not at layers 4-6, and it is affordable.

## 2.8 Failure conditions

The conditions under which this specification is wrong. Written now so they are recognized rather than rationalized.

**F1, `R` is low even when `D_d` is high.** If proving 90% of obligations recovers under 15% of the monitor's cost, then the cost is metadata traffic that proof does not touch, and the correct response is to stop building a prover and go work on [`../safe-memory/05`](../safe-memory/05-representation.md)'s representation instead. **Measured at V2**, deliberately early, before layers 4-6 exist.

**F2, The compile-time budget cannot be met.** If layers 0-3 cannot fit in 2x, `default` becomes layers 0-2 and the specification's honest claim shrinks by however much layer 3 was contributing. Survivable; the ladder is designed so that any prefix is a valid configuration.

**F3, A soundness bug reaches a release.** A statically discharged obligation that was false, found in the field rather than by C4's differential accounting. This is the one that would end the project's credibility rather than merely its ambition, and it is why [document 10](10-soundness-and-trust.md)'s trust set is specified before any prover is written.

**F4, Certificates turn out to be unaffordable.** If emitting and checking a certificate per discharge costs more compile time than the discharge saves, the design's central honesty mechanism is too expensive, and the fallback (certificates only under `-fsafety-proof-certify`) is strictly worse because it makes the audited configuration different from the shipped one.

**F5, The residual is unpredictable.** If which obligations survive depends on optimization order, inlining decisions, or a solver's timeout in a way users perceive as arbitrary, then performance becomes non-reproducible and the feature is unusable in production even if it is fast on average. [Document 11](11-residual-and-composition.md) addresses this directly and it is underrated as a risk.

## 2.9 Non-goals

**Proving functional correctness.** Even at `verify`. The specification language exists to discharge memory-safety obligations, not to state what a function computes. Users who want the latter should use CN or VeriFast directly, and [document 07](07-separation-logic.md) is designed so the annotations are close enough to CN's that this is a real option.

**Reporting anything to the user by default.** No alarms, no warnings, no "could not prove" diagnostics unless asked with `-fsafety-proof-explain`. This is the decision from [`00`](00-README.md) and it is a non-goal because every static analyzer that acquired a default alarm list did so one well-intentioned diagnostic at a time.

**Beating a dedicated verifier on its own benchmarks.** VeriFast, CN and Flux will prove things this cannot. They are also not compilers and they do not have to finish in two seconds.

**Static analysis as a product.** There is no standalone `rucc-analyze` binary, no SARIF output, no CI integration story of its own. The output of this machinery is *fewer instructions*, and its only user interface is `--emit=safety-summary`.
