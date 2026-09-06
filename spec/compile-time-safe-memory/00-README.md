# Compile-time memory safety for rucc

A specification for proving memory-safety obligations at compile time, so that the checks in [`../safe-memory/`](../safe-memory/00-README.md) do not have to run.

## The thesis

The companion specification builds a run-time monitor. It inserts a check for every conjunct of every judgement at every memory operation and then spends its longest document ([07, check elimination](../safe-memory/07-check-elimination.md)) removing as many as it can with SMT-verified rewrite rules. That document opens by saying the entire cost budget is won or lost there, and it calibrates against CCured, PICO and CHOP.

What it does not say, and what this specification says, is that **check elimination is program proof under another name, and it should be built as program proof rather than as a peephole.** A rewrite rule that removes a bounds check because a dominating check established the range is a one-line proof. The literature has spent thirty years building machinery for the rest of that proof (abstract interpretation, refinement types, ownership inference, separation logic) and 2026 is the first year in which that machinery is both automatic enough and fast enough to sit inside a compiler.

The companion document already reserved the name. Its [document 02](../safe-memory/02-the-goal.md) defines:

> **Tier V, Verified.** Not a tier the user selects. It is the set of checks that were discharged statically and therefore cost nothing.

**This specification is the machinery that makes Tier V large.** It is not a competing design and it does not replace anything. It is the other half.

## The one structural idea

**One obligation set, two dischargers.**

The safety model in [`../safe-memory/04`](../safe-memory/04-safety-model.md) defines seven judgements J1 through J7. Every memory operation in a program generates *proof obligations*: the conjuncts of those judgements, at that site, in that context. That set is the specification of what must be true, and it is written down once.

Then there are two ways to discharge an obligation:

- **Statically**, by a proof, at compile time, at zero run-time cost. This document set.
- **Dynamically**, by a check, at run time, at the cost in [`../safe-memory/13`](../safe-memory/13-performance.md). The companion.

Every obligation is discharged one way or the other, and the count of each is reported. There is no third outcome. **An obligation the prover cannot discharge is not an error and is not a warning, it silently becomes a run-time check.**

That single decision is what makes this design different from every static analyzer in [document 01](01-research-2026.md), and section "The four decisions" below explains why it is the decision that matters most.

## Why the risk profile is inverted, and why that changes everything

The companion's document 07 opens with an asymmetry that governs its whole design:

> An unsound check elimination produces a *correct* answer on every test and an undetected vulnerability in production. Nothing observes it.

That is true when elimination is the *only* thing standing between the program and an unchecked access. It is why every elimination rule there must be SMT-verified, why differential check accounting runs nightly, and why the design is conservative to the point of leaving performance on the table.

Under the design here, the asymmetry inverts. A proof that succeeds when it should not is still catastrophic, so the *checker* is in the trust set and is small. But a proof that **fails** when it should have succeeded costs nothing but a run-time check. So:

- The prover may be arbitrarily incomplete. Incompleteness is a performance bug.
- The prover may use heuristics, profile data, unsound-but-checked shortcuts, search, and, per [document 09](09-inference-and-llm.md), machine-generated annotations, because **none of these are trusted**. They propose; the checker disposes.
- The prover may give up on a hard function, a hard loop, or a hard file, at any granularity, at any time, including on a timeout.

That last property is what makes this shippable inside a compiler. Astrée and Frama-C's Eva cannot give up: they are sound analyzers whose output is an alarm list, so an obligation they cannot discharge becomes a false alarm the user must triage, and the false-alarm rate is the reason sound static analysis has never been adopted at scale outside avionics. **We have no alarms.** [Document 02](02-the-goal.md) makes this the central claim and [document 11](11-residual-and-composition.md) makes it precise.

## The four decisions

**1. Obligations come from the companion's judgements, not from a new model.** There is exactly one definition of memory safety across both specifications, in [`../safe-memory/04`](../safe-memory/04-safety-model.md), founded on [gradual allocator independence](https://arxiv.org/abs/2507.11282) and PNVI-ae-udi. A prover and a monitor that disagree about what safety means would be worse than either alone.

**2. Discharge is a ladder, cheapest first.** Seven layers in [document 04](04-the-discharge-ladder.md), from frontend type-directed discharge (free, catches the majority) up through interval and relational domains, ownership inference, refinement typing, and separation logic (expensive, opt-in, for the code that deserves it). Each layer receives the previous layer's residue. Compile time is a first-class budget: **layers 0 through 3 must not slow the compiler by more than 2x**, or nobody turns them on.

**3. Every discharge carries a certificate.** Not a claim, an artifact: which layer, which rule, which facts, and for the SMT layers an unsat core that an independent checker can replay. This is what [`../safe-memory/07.8`](../safe-memory/07-check-elimination.md)'s `--emit=safety-summary` consumes, and it is what makes "why is there no bounds check on line 412" answerable. [Document 10](10-soundness-and-trust.md).

**4. Annotations are hints, never requirements.** Carried over verbatim from [`../safe-memory/07.5`](../safe-memory/07-check-elimination.md): annotating a header makes the program *faster* and never changes whether it is *safe*. This is the property that makes adoption monotone, and it is the property that Checked C and TrapC do not have, which is most of why they have not been adopted. [Document 08](08-annotations.md).

## What is settled, and what is not

**Settled.** No alarms by default. Obligations from J1-J7. Layered discharge with a compile-time budget. Certificates for every discharge. Annotations as hints. Machine-generated artifacts are untrusted inputs, checked, and committed to the repository rather than regenerated at build time. Separation logic is opt-in, per-function, for code that asks for it.

**Not settled, and ranked in [document 15](15-open-questions.md).** Whether temporal obligations are statically dischargeable at any useful rate on real C, the honest prediction is that they largely are not, which inverts the usual expectation. Whether the relational domains scale to the corpus inside the compile-time budget. Whether ownership inference in the [&inator](https://arxiv.org/abs/2604.17261)/[Crown](https://doi.org/10.1007/978-3-031-37709-9_18) lineage survives contact with code that is not in the applicative subset. What fraction of the aux-plane traffic (which [`../safe-memory/05.5`](../safe-memory/05-representation.md) predicts is the real cost, not the checks) is statically eliminable, because if the answer is "little", then a high static discharge rate does not buy the performance it appears to.

## Document map

| # | File | What |
|---|---|---|
| 00 | this | thesis, decisions, map |
| 01 | [`01-research-2026.md`](01-research-2026.md) | the landscape as of September 2026, with citations |
| 02 | [`02-the-goal.md`](02-the-goal.md) | what compile-time proof can honestly mean; the metric; the tiers |
| 03 | [`03-obligations.md`](03-obligations.md) | the obligation model: one set, two dischargers |
| 04 | [`04-the-discharge-ladder.md`](04-the-discharge-ladder.md) | the seven layers, their yield, their cost |
| 05 | [`05-ownership-and-lifetimes.md`](05-ownership-and-lifetimes.md) | static temporal safety: regions, ownership, and why this is the hard half |
| 06 | [`06-bounds-and-refinements.md`](06-bounds-and-refinements.md) | static spatial safety: intervals, relations, refinement types |
| 07 | [`07-separation-logic.md`](07-separation-logic.md) | the deep layer, opt-in, for allocators and parsers |
| 08 | [`08-annotations.md`](08-annotations.md) | the annotation surface, and why it is optional |
| 09 | [`09-inference-and-llm.md`](09-inference-and-llm.md) | inferring annotations and proofs, including with models; the trust rule |
| 10 | [`10-soundness-and-trust.md`](10-soundness-and-trust.md) | what "proved" means; certificates; the trust set |
| 11 | [`11-residual-and-composition.md`](11-residual-and-composition.md) | how proof and monitor compose; the no-silent-gap rule |
| 12 | [`12-integration.md`](12-integration.md) | crates, flags, pass placement in `rucc` |
| 13 | [`13-evaluation.md`](13-evaluation.md) | the metrics, the corpus, the falsifiable claims |
| 14 | [`14-milestones.md`](14-milestones.md) | V0 through V6, against the parent's M-series and the companion's S-series |
| 15 | [`15-open-questions.md`](15-open-questions.md) | ranked, with what would decide each |

## What this is not

**Not a verified compiler.** CompCert exists and this is not it. The parent's [document 15](../15-testing.md) already sets the disposition: verify the narrow mechanical things, test everything else hard.

**Not full functional correctness.** seL4 proves its microkernel meets an abstract specification; that is 10k lines, purpose-built for verification, and roughly 20 person-years. We prove memory-safety obligations and nothing else, on code nobody wrote for a prover.

**Not a bug finder.** An unproved obligation is not a bug report. The companion specification finds bugs; this one removes checks. Conflating the two is how static analyzers acquire alarm lists.

**Not a safe dialect.** Checked C, TrapC and `-fbounds-safety` change the language. We consume `-fbounds-safety`'s annotations because the kernel already writes them, and we add no requirement.

## Honesty about scope

The companion specification estimates 12-20 engineer-months on top of the parent compiler's 40-70, revised to 18-30 calendar months in its milestone plan. This adds **10-18 engineer-months** on top of that, and layers 0 through 3 (which is where most of the yield is) are perhaps 6 of them. Layers 4 through 6 are where the research risk lives and they are genuinely optional: a build with layers 0-3 and the monitor is a complete, shippable system.

Nothing here can begin before the companion's S1, because there are no obligations to discharge until something generates them.

The largest risk is not technical. It is that **a static prover is a thing whose value is invisible when it works.** The monitor produces bug reports, which are evidence. The prover produces the absence of instructions, which is only evidence if someone is counting, which is why [document 10](10-soundness-and-trust.md)'s certificates and the summary's discharge accounting are specified before any prover is built, and not after.
