# Soundness and trust

What "discharged" means, what has to be believed for it to be true, and how much of that is auditable.

## 10.1 The claim

Stated so it can be attacked, in the form [`../safe-memory/04.5`](../safe-memory/04-safety-model.md) uses.

> **Claim.** For every obligation *o* in state `Discharged(c)` in a compiled program, and every execution of the program reaching `o.site`, the predicate `o.pred` holds, provided the components enumerated in §10.4 are correct.
>
> **Corollary (the observable form).** For every program *P* and every workload *W*, the set of memory errors reported by `P` built at `-fsafety-proof=off` under *W* equals the set reported by `P` built at any other proof level under *W*.

The corollary is the useful statement, because it is *testable* and the claim is not. It is what [`02.4.1`](02-the-goal.md) calls the composition rule and what §10.6 tests nightly.

Note what the claim does **not** say. It does not say the program is memory-safe, or that the prover is complete, or that undischarged obligations indicate anything at all. It says the compiler did not lie when it removed a check.

## 10.2 Certificates

A certificate is an artifact, not an assertion. Its purpose is that **the component which decides to remove a check is not the component that is trusted**: [`04.1`](04-the-discharge-ladder.md)'s search/checker split, made concrete.

```
Certificate {
    obligation: ObligationId,
    layer:      u8,
    evidence:   Evidence,
}

Evidence ::=
  | Syntactic  { rule: RuleId }                      // layer 0
  | Dominance  { established_by: ObligationId,
                 dominator: BlockId,
                 no_invalidation: Vec<IrInst> }      // layer 1, 3
  | Numeric    { domain: Domain, invariants: Vec<AbstractState>,
                 derivation: Vec<TransferStep> }     // layer 1, 2
  | Vc         { formula: FormulaId, core: UnsatCore } // layer 4, 6
  | Summary    { callee: FuncId, summary: SummaryId,
                 justification: Box<Certificate> }   // layer 3
```

**The checking rules, one per form:**

*`Syntactic`*: re-run the named rule's premise on the site. The rules are data in `rucc-codegen/rules/safety/`, per the parent's DSL, and are individually SMT-verified by `rucc-verify` exactly as the elimination rules of [`../safe-memory/14.2`](../safe-memory/14-verification.md) are. This reuses machinery that already exists.

*`Dominance`*: verify the dominance relation on the CFG, verify the established obligation's predicate implies this one, and verify that no instruction on any path between them invalidates it. The last part is the one that goes wrong, and it is why `no_invalidation` lists the instructions rather than asserting a property: the checker re-examines each one against a closed table of invalidating operations.

*`Numeric`*: replay the transfer functions over the given abstract states and confirm the final state implies the predicate. **Crucially, the checker does not need to find the invariants**; it only confirms they are inductive and sufficient, which is linear where the analysis was a fixpoint. This is the standard proof-carrying-code asymmetry and it is what makes certificate checking affordable.

*`Vc`*: re-check the unsat core against the formula, and re-derive the formula from the IR. §10.5 explains why the second half is the load-bearing part.

*`Summary`*: recursive: the summary itself carries a certificate.

**Cost.** Certificate checking is expected to be 5-15% of the discharge cost, since checking is cheaper than searching in every form above. [`02.8`](02-the-goal.md)'s failure condition F4 is that this turns out to be wrong; the number is measured at V1, on the cheap layers, before the expensive ones exist.

**Certificates are on by default and are checked by default.** A configuration where the audited build differs from the shipped build is worse than no audit, which is why F4's fallback flag is described there as strictly worse rather than as a plan.

## 10.3 What the certificates are for, beyond soundness

Three uses that are not about correctness and that justify the format's cost on their own.

**`-fsafety-proof-explain`** answers "why is there no bounds check on line 412" with a rendered certificate. Without it, a performance-sensitive user cannot reason about what the compiler did, and a security reviewer cannot audit it at all.

**Discharge accounting** ([`03.8`](03-obligations.md)) needs to attribute discharges to layers, which requires knowing which layer discharged what, trivially available from a certificate and awkward without one.

**Regression triage.** A discharge that disappears between compiler versions is a performance regression whose cause is otherwise invisible. Diffing certificate sets across builds turns it into a one-line report.

## 10.4 The trust set

Enumerated with sizes, following [`../safe-memory/10.2`](../safe-memory/10-boundaries.md)'s discipline that a trust set which is not counted is not a trust set.

| # | Component | Size | Verified? | If wrong |
|---|---|---|---|---|
| 1 | The obligation generator | small | by inspection + [`03.3`](03-obligations.md)'s verifier invariant | missing obligations ⇒ silent gap |
| 2 | The certificate checker | ~2-4 kLoC | reviewed; the fixed point of this table | any discharge could be wrong |
| 3 | The layer-0 rule set | data | **SMT-verified** by `rucc-verify` | wrong syntactic discharge |
| 4 | The VC encoder (IR → logic) | ~2 kLoC | reviewed | §10.5, the largest hole |
| 5 | The SMT solver's unsat cores | external | re-checked, not trusted for sat | a bad core fails the check |
| 6 | Escape analysis | ~1 kLoC | reviewed | §[5.3](05-ownership-and-lifetimes.md); wrong temporal discharge |
| 7 | The invalidation table (§10.2) | data | reviewed | wrong dominance discharge |
| 8 | The effects table | data | **not verified** | [`09.7`](09-inference-and-llm.md); missed errors |
| 9 | Alias facts from the parent (TBAA) | existing | existing | wrong refinement updates |
| 10 | Pass maintenance of obligation metadata | diffuse | the verifier invariant | dropped obligations |

Rows 1, 2, 4 and 6 are the ones to worry about. Rows 3 and 5 are handled by existing machinery. Row 8 belongs to the companion specification and is inherited rather than created here.

**The total is roughly 5-8 kLoC of trusted Rust plus two data tables.** That is a number a person can review, which is the point of the split, and it should be reported in the summary alongside everything else.

### 10.4.1 What is emphatically not in the trust set

Every analysis in [documents 04](04-the-discharge-ladder.md) through [07](07-separation-logic.md): the interval domain, the octagon domain, the induction recognizer, liquid inference, ownership inference, the external separation-logic prover. All of them can be wrong, buggy, or adversarial, and the worst outcome is a certificate that fails to check, which aborts the discharge and leaves a run-time check.

And, per [document 09](09-inference-and-llm.md), every generator.

## 10.5 The largest hole, named

**Row 4: the encoding from IR to logic.**

When layer 4 proves `i < extent(a)` and the checker validates an unsat core, both are reasoning about a *formula*. The claim that the formula corresponds to the IR, that `i` in the formula is the SSA value the machine will compute, that the arithmetic is the machine's arithmetic and not idealized integers, that a load in the IR became the right term, is unverified and is the point where a subtle unsoundness would live.

[`06.7`](06-bounds-and-refinements.md) already flags one instance: index arithmetic that can overflow must be encoded as bit-vectors, not integers, or the "obvious" bound is unsound. That is a *class* of bug, not one bug.

**Three mitigations, in ascending order of ambition:**

1. **Encode conservatively by default.** Bit-vector semantics for all machine arithmetic; the integer encoding only where a range proof licenses it, and that license is itself an obligation. Costs solver time, and it is the right default.
2. **Differential test the encoder.** Randomly generate IR fragments, encode them, and compare the formula's satisfying assignments against concrete IR interpretation. This is cheap, it is the same technique the parent uses for its rule DSL, and it should catch the great majority of encoder bugs.
3. **Foundational replay.** [Foundational VeriFast's hinted mirroring](https://arxiv.org/html/2601.13727), re-derive the encoding inside a proof assistant against a mechanized semantics. Post-1.0, and the honest note is that it requires a mechanized semantics of *our* IR, which does not exist and would be a large project on its own.

The specification ships with (1) and (2) and states that (3) is where the remaining risk lives.

## 10.6 The empirical backstop

Proof about the prover is not the primary defense. This is:

> **Differential proof accounting.** Nightly, build the whole corpus and the CVE suite at `-fsafety-proof=off` and at `-fsafety-proof=deep`. Run both. Assert that the report sets are identical, in both directions.

A `deep` build that misses a report the `off` build produced is an unsound discharge, the exact failure this document exists to prevent, made visible. A `deep` build that produces a report the `off` build did not is a *different* bug and equally interesting: it means a proof-derived assumption changed program behavior, which should be impossible.

This is [`../safe-memory/14.3`](../safe-memory/14-verification.md)'s differential check accounting one level up, it reuses that machinery entirely, and it is claim C4. It is the most valuable test in this document set for the same reason it is there: it converts an invisible failure into a diff.

**Its limit, stated:** it only detects unsoundness on obligations that are *violated during the test run*. An unsound discharge on a path no test exercises is invisible to it, which is why the corpus includes the CVE suite (where violations are guaranteed) and why [document 13](13-evaluation.md) adds randomized violation injection: take a corpus program, inject a bounds violation at a randomly chosen discharged site, and require the `deep` build to report it. If it does not, the discharge was wrong.

That second technique is the strongest tool available and it is directly analogous to [`../safe-memory/14.4`](../safe-memory/14-verification.md)'s randomized elimination fuzzing. It also has a free second oracle: with no injection, no reports.

## 10.7 No new escape hatches

[`../safe-memory/04.5`](../safe-memory/04-safety-model.md) has exactly three: not executed, uninstrumented code, declared exemption. **This specification adds none.**

That is a stronger statement than it sounds. It means:

- A function the prover fails on is fully checked, not partially.
- A file that exceeds the compile-time budget is fully checked.
- A solver timeout is fully checked.
- An unparseable annotation is an error, not an ignored hint that silently weakens something.
- `__unsafe_indexable` is not a new hatch, it is the existing *declared exemption*, and it is counted as one ([`08.2.1`](08-annotations.md)).

Every failure in this document set degrades to the companion's behavior exactly. That property is what makes the two specifications composable, and it is [document 11](11-residual-and-composition.md)'s subject.

## 10.8 The honest summary

What a reader should take away:

**The strong part.** The trust set is small and enumerated, the search machinery is entirely outside it, certificates are checkable and checked, and the empirical backstop catches unsoundness on any executed path.

**The weak part.** Row 4. The IR-to-logic encoding is unverified, it is where a real unsoundness would most likely live, and mitigating it properly requires a mechanized semantics we do not have.

**The part that is weak and cannot be fixed here.** Row 8, the effects table, which is inherited from the companion and is the reason a fully instrumented build is meaningfully safer than one linking uninstrumented libraries, proof or no proof.
