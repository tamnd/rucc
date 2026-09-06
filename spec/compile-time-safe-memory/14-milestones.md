# Milestones

Seven, V0 through V6, interleaved with the parent's M-series and the companion's S-series. Each exit criterion is a number or a demonstration, per the companion's rule that a milestone whose exit cannot be checked is a wish.

**The ordering principle**, inherited: every measurement that can invalidate a number happens before the work that depends on it. [`13.3`](13-evaluation.md) names five such measurements and each appears below at the earliest milestone where it is possible.

## 14.1 Dependencies

| V | Needs from the parent | Needs from the companion | Earliest |
|---|---|---|---|
| V0 | the IR and its verifier (M2) | **S1 in progress**: see §14.2 | with S1 |
| V1 | the optimizer's pass manager (M4) | S1 complete | with S2 |
| V2 | none | S3's corpus builds | after S3 |
| V3 | the rule DSL, LTO (M6, M8) | **S4** | with S4 |
| V4 | LTO (M8) | S4 | after S4 |
| V5 | none | S5, S6 | after S6 |
| V6 | none | S6 | after S6 |
| V5 (kernel part) | M11 | S7 | after S7 |

## 14.2 The one hard sequencing constraint

**V0 must land before the companion's S4, not after.**

S4 is where the companion builds check elimination and where [`../safe-memory/16`](../safe-memory/16-milestones.md) says Tier E is won or lost. Per [`11.4`](11-residual-and-composition.md), those elimination passes *become* layers 0 and 1 of this ladder. If they are written first as free-standing rewrites and re-homed afterwards, the re-homing is a rewrite of the most safety-critical code in either specification.

So the obligation model, the certificate types and the no-`Open` verifier rule must exist before the first elimination rule is written. That is V0, it is small, and it is the entire reason this document set needs to be read before S4 rather than after S6.

Everything else here can slip without consequence.

## 14.3 V0: The obligation model (1 to 1.5 months, with S1)

`Obligation`, `ObligationId`, `Certificate`, `Evidence` in `rucc-ir`. Textual round-trip for all of them. The no-`Open` verifier rule. The parentage rule of [`03.6`](03-obligations.md) for duplication by inlining and unrolling. Generation in `rucc-safety`, dumb and complete, from J1-J7.

No discharge. Nothing is proved. The output is a count.

**And measurement M1**, the obligation distribution of [`03.2.1`](03-obligations.md), which needs nothing but generation and decides the ladder's layer ordering.

**Exit:** obligations round-trip through the textual IR; the verifier rejects a written list of malformed and dropped forms; `--emit=safety-summary` prints per-class counts for every project the companion's S1 can build; **M1 published, and [`03.2.1`](03-obligations.md)'s table either confirmed or replaced with the measured one.**

## 14.4 V1: Layers 0-1, certificates, and the backstop (2 months, with S2)

The cheap layers, and (more importantly) all of the trust machinery, built before there is anything ambitious to trust.

Layer 0's syntactic rules as data in `rucc-codegen/rules/safety/`, verified by `rucc-verify`. Layer 1's interval domain and dominance walk. The certificate checker. Differential proof accounting ([`10.6`](10-soundness-and-trust.md)) wired into nightly CI. Encoder differential testing, even though the encoder is trivial at this point, because the harness is what takes time.

**Exit:** `D_s` reported per layer on the corpus; zero differential divergences over four weeks; **claim D1 demonstrated** by building the corpus on two hosts and diffing certificate sets byte for byte; certificate check time measured against [`02.8`](02-the-goal.md)'s failure condition F4.

## 14.5 V2: Plane liveness, and the measurement that could end the project (1.5 months, after S3)

The `plane-liveness` pass of [`03.5`](03-obligations.md), and then the two measurements that decide whether any of this pays.

**M2, `R/D_d`.** Discharge whatever layers 0-1 can, elide the planes that die, and measure recovered cost. [`02.8`](02-the-goal.md)'s F1 lives here.

**M3, Init-plane elision** ([`06.10`](06-bounds-and-refinements.md)'s B4), the cheapest large contributor to `R` and the best early evidence for whether M2's answer generalizes.

**Exit:** `R` reported per benchmark with layers 0-1 only; plane bytes elided per plane; B4 met or missed.

**This is stopping point 1, and it is early on purpose.** If `R` is near zero with a respectable `D_s`, then the monitor's cost is metadata traffic that proof does not touch, layers 2-6 will not change that, and the correct response is to stop here and spend the effort on [`../safe-memory/05`](../safe-memory/05-representation.md)'s representation instead. Finding that out after building layer 4 would be a year wasted.

## 14.6 V3: The `default` level, complete (3.5 months, with S4)

Layer 2 (induction recognition, the packed octagon domain, loop splitting and peeling) and layer 3, summaries, with `nofree` built first per [`04.6`](04-the-discharge-ladder.md) regardless of ladder order.

This is the milestone that produces the shippable system. After it, `-fsafety-proof=default` is complete and every claim that matters is testable.

**M4, layer 2's yield on the S2 loop shape** lands here, and it is the measurement B2 depends on.

**Exit, and it is the longest list in the plan because this is the milestone that matters:**

- **C1** (`D_s ≥ 0.70` spatial), **C2** (`R ≥ 0.35`), **C5** (`D_s ≥ 0.25` temporal) reported on the tier-1 corpus.
- **T1** (flow + escape ≥ 40% of `O.live`), **T3** (lifetime-plane elision ≥ 30% of bytes).
- **B1**, **B2**.
- **D2** and **D3** by ablation.
- Build time ≤ 2.0x per project and ≤ 4x per translation unit at p99, per [`13.5`](13-evaluation.md).
- Randomized violation injection ([`13.4`](13-evaluation.md)) running continuously with zero misses.

**This is stopping point 2, and a project that reaches it has the whole realizable value of the specification.** Everything after V3 is refinement.

## 14.7 V4: Ownership, measured and probably cut (1.5 months, after S4)

Layer 5, built as a separable pass with no other consumers, measured, and cut if it fails.

**M5, layer 5's temporal yield.** Claim T4: ≥ 10% of the residual temporal obligations after layers 0-3.

**Exit:** T4 met, or **layer 5 deleted and its budget moved to layer 3**, with the measurement published either way.

Per [`05.7`](05-ownership-and-lifetimes.md) the prior is that it gets cut, and the milestone is written so that cutting it is the expected outcome rather than a failure. The reason to build it at all is that being wrong about the prior would be worth a great deal, and finding out costs one measurement on a pass designed to be deletable.

## 14.8 V5: Refinements, and the kernel (3 months, after S6; kernel part after S7)

Layer 4: the refinement type system, VC generation, liquid invariant inference, and the out-of-process solver behind `--features proof-smt`. `rucc-annotate` ([document 09](09-inference-and-llm.md)), optional and cuttable.

And the kernel work, which is small because [`08.4`](08-annotations.md) says the kernel already wrote the annotations:

- `__rcu` free-free intervals ([`05.6`](05-ownership-and-lifetimes.md)), the highest-yield kernel-specific item and a recognizer for two function names.
- `__percpu` thread-locality, without which §[5.2.1](05-ownership-and-lifetimes.md)'s free-free intervals are unusable in the kernel at all.
- `__counted_by` refinements at layer 4, on a tree that already has thousands of them.

**Exit:** **C3** (`D_s ≥ 0.85` spatial at `deep`, under 10x compile time); **B3** (≥ 60% of S4/S6 obligations with annotations, measured on the kernel); **T2** (≥ 60% of `O.live` inside RCU read-side critical sections on the networking and VFS paths); the **annotation ablation** of [`13.6`](13-evaluation.md) published, with the three numbers nobody currently has.

## 14.9 V6: Separation logic (3.5 months, after S6)

Layer 6 by path (a) of [`07.2`](07-separation-logic.md): VC generation on our side, external prover as an untrusted proposer, certificate checked against our own IR-derived formulas.

Targets in [`07.5`](07-separation-logic.md)'s order: musl's `mallocng`, then `copy_to_user`/`copy_from_user`, then the string and memory library functions. T4's ring buffers and T5's kernel allocators are explicitly beyond this milestone.

`-fsafety-proof-test` ships with it, and is arguably more valuable than the proving.

**Exit:** **C6** (one nontrivial allocator with every obligation discharged at `verify`, running with no residual checks) and a **reduction in the counted trust set** of [`../safe-memory/10.2`](../safe-memory/10-boundaries.md) that is reported as a number, because that is what this milestone actually buys.

**This is stopping point 3.**

## 14.10 Summary

| V | What | Months | Alongside |
|---|---|---|---|
| V0 | Obligation model; M1 | 1-1.5 | S1 |
| V1 | Layers 0-1; certificates; the backstop | 2 | S2 |
| V2 | **Plane liveness; M2, stopping point 1** | 1.5 | after S3 |
| V3 | **Layers 2-3; the `default` level, stopping point 2** | 3.5 | S4 |
| V4 | Ownership, measured and probably cut | 1.5 | after S4 |
| V5 | Refinements; the kernel annotations | 3 | after S6/S7 |
| V6 | **Separation logic, stopping point 3** | 3.5 | after S6 |
| | | **16-19** | |

Against [`12.8`](12-integration.md)'s 19 engineer-months and [`00`](00-README.md)'s 10-18. The three figures should be reconciled by measurement rather than argument; **19 is the number to plan against**, and the difference from document 00 is infrastructure (pass maintenance and the certificate checker) that document 00 did not price.

Note the shape: **V0 through V3 is 8.5 months and produces everything in claims C1, C2, C5, T1, T3, B1, B2, D1-D3.** V4 through V6 is another 8 and produces C3, C6, T2, B3 and a smaller trust set. The first half is the specification; the second half is the ambition.

## 14.11 What is not on this list

**Layer 6 concurrency** beyond lock-based resource invariants. [`07.6`](07-separation-logic.md).

**Foundational replay** of the VC encoder in a proof assistant. [`10.5`](10-soundness-and-trust.md)'s mitigation 3; it needs a mechanized semantics of our IR, which does not exist.

**An `__owned`/`__borrowed` annotation.** [`08.5`](08-annotations.md); it waits on V4's measurement and will most likely never be needed.

**A standalone analyzer.** [`02.9`](02-the-goal.md). There is no product here other than fewer instructions.

**Anything that begins before the companion's S1.** There are no obligations to discharge until something generates checks, and generating obligations against a monitor that does not exist would be designing against a guess.
