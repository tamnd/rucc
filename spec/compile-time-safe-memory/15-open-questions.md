# Open questions

Ranked by how much of the specification depends on the answer. The discipline is the companion's: a question here is one where **the specification is genuinely undecided**, not one where the answer is known and the code is unwritten.

## Question 1, Does discharge recover cost?

**The problem.** [`02.2.1`](02-the-goal.md). [`../safe-memory/05.5`](../safe-memory/05-representation.md) predicts the monitor's cost is dominated by metadata traffic rather than by check instructions. A discharged obligation removes a compare and a branch; it does not by itself remove the plane read that fed it or the plane write that maintained it. Those die only when *every* reader of a plane over an object is discharged ([`03.5`](03-obligations.md)).

So `R` and `D_d` can decouple arbitrarily, and a program could reach `D_d = 0.9` with `R = 0.1`.

**Why it is ranked first.** It is the premise of the document set. Every other question is about how much of something we get; this one asks whether the something is worth having. [`02.8`](02-the-goal.md)'s failure condition F1 is this question going badly, and if it does the correct response is to stop and work on the representation instead.

**Nobody has measured it, for anyone's system.** Every tool in [document 01](01-research-2026.md) either has no run-time component (so `R` is meaningless) or no static component (so there is nothing to measure). The number does not exist.

**How it gets answered:** V2, with layers 0-1 only, deliberately before layers 2-6 are built. [`13.3`](13-evaluation.md)'s M2.

**What would change the answer:** a representation in which plane maintenance is cheap enough that its survival does not matter, which is what CHERI hardware provides, and which is why [`11.7`](11-residual-and-composition.md) notes this specification's value is inversely proportional to what the hardware does.

## Question 2, Do the relational domains fit the compile-time budget?

**The problem.** Layer 2 is where the yield is ([`04.5`](04-the-discharge-ladder.md)) and octagons are cubic in the number of variables. Astrée makes them affordable by variable packing, tuned per program class over years, on synchronous control code without recursion or dynamic allocation. We propose obligation-directed packing on arbitrary C, with a 0.5x budget.

**The specific worry** is not the average case, it is [`13.5`](13-evaluation.md)'s p99: a generated parser, a 12,000-line switch, a function with 400 live pointers. A prover that is 1.4x on average and 40x on one file breaks builds, and the step budgets of [`11.3`](11-residual-and-composition.md) will cut those files off, which turns a compile-time problem into a *discharge-rate* problem concentrated in exactly the machine-generated code that is often hot.

**How it gets answered:** V3, and the pack-size and pack-count caps are the tuning knobs. The fallback if it fails is that layer 2 keeps only the induction recognizer (which is a pattern match, not a fixpoint, and is cheap) and the octagon domain moves to `deep`. That would cost most of B2 at `default`.

**Why it is not ranked first:** it has a known-adequate fallback and the fallback is measurable in advance.

## Question 3, Is flow discharge usable in multithreaded programs?

**The problem.** [`05.2.1`](05-ownership-and-lifetimes.md). A free-free interval is invalidated by any point at which another thread may run, for any instance another thread can reach. Absent a memory-ordering analysis, that is every point. So flow discharge (which [`05.10`](05-ownership-and-lifetimes.md)'s claim T1 says is the bulk of realizable temporal discharge) applies only to instances proved thread-local.

**Why it matters.** Every interesting target is multithreaded. nginx, SQLite in threaded mode, ffmpeg, the kernel. If thread-locality cannot be established for most heap objects, T1 holds only for stack data and freshly-allocated locals, C5 fails, and temporal obligations stay almost entirely checked.

**What might rescue it.** `__percpu` in the kernel, which is a declared thread-locality and is why [`08.4`](08-annotations.md) lists it as load-bearing. Allocation-site escape analysis in userspace, for objects never stored into shared structures. And the observation (untested) that the *hot* accesses are disproportionately to thread-local scratch.

**How it gets answered:** V3, by reporting T1 separately for single-threaded and multithreaded corpus projects. That split should be in the report from the first measurement, because a combined number would hide the effect entirely.

## Question 4, Does refinement typing survive C's aliasing?

**The problem.** [`06.4`](06-bounds-and-refinements.md). Flux's power comes from strong updates, which come from Rust's ownership. C has none, so a store through any possibly-aliasing pointer forces a weak update and the refinement is lost. The three mitigations (non-escaping locals, TBAA, `restrict`) cover a fraction, and **TBAA is unavailable on the kernel**, which builds `-fno-strict-aliasing` and is the target where layer 4's annotations already exist.

That is an uncomfortable combination: the code with the most annotations is the code where the layer that consumes them is weakest.

**How it gets answered:** V5, by reporting B3 separately for `-fstrict-aliasing` and `-fno-strict-aliasing` builds of the same projects.

**The interesting sub-question,** and it may be a small novel contribution: the monitor checks `restrict` at run time ([`../safe-memory/09.6`](../safe-memory/09-type-init-and-races.md)) while the prover *assumes* it. No shipping compiler both relies on `restrict` for static reasoning and verifies it dynamically. If that combination works, `restrict` becomes a usable strong-update mechanism for C in a way it has never been, because the usual objection (that programmers get `restrict` wrong) is answered by checking it.

## Question 5, Can the VC encoder be trusted without a mechanized IR semantics?

**The problem.** [`10.5`](10-soundness-and-trust.md), row 4 of the trust set. When layer 4 proves a formula and the checker validates an unsat core, both reason about a formula, and the correspondence between the formula and the IR is unverified. [`06.7`](06-bounds-and-refinements.md)'s overflow case is one instance of a class.

**What we ship:** conservative bit-vector encoding by default, plus encoder differential testing, plus [`13.4`](13-evaluation.md)'s randomized violation injection, which is the technique most likely to actually catch an encoder bug.

**What would settle it:** [Foundational VeriFast's hinted mirroring](https://arxiv.org/html/2601.13727) applied to our encoder, replay in a proof assistant against a mechanized semantics of the rucc IR. That semantics does not exist and building it is a large project the parent has not committed to.

**Why it is ranked here and not higher:** it only bites at layers 4-6, which are `deep` and `verify`, and the default configuration has no encoder at all. A shipped `-fsafety-proof=default` build carries none of this risk, which is a strong argument for the layer-0-through-3-without-a-solver invariant of [`12.4`](12-integration.md).

## Question 6, Is re-homing the companion's eliminator actually free?

**The problem.** [`11.4`](11-residual-and-composition.md) resolves the two-eliminators problem by making the companion's `safety-dce`, `safety-loop` and `safety-plane` passes into layers 0-1 and the plane-liveness pass of this ladder. The claim is that this is a re-homing rather than a rewrite: the rules are unchanged, their SMT verification is unchanged, and only the accounting is added.

**The worry** is that the companion's rules will turn out to encode reasoning that does not fit the certificate forms of [`10.2`](10-soundness-and-trust.md), a rewrite that removes a check by an argument that is neither syntactic, nor dominance-based, nor numeric. If so, either the certificate language grows an escape hatch (bad) or some rules must be reformulated (work).

**How it gets answered:** V0, by taking the companion's rule set as it exists at that point and classifying every rule into a certificate form. This is a day of work and it is scheduled first for that reason.

**Why the answer probably is "free":** [`../safe-memory/07`](../safe-memory/07-check-elimination.md)'s rules are already local, already verified against a stated premise, and a stated premise is a certificate.

## Question 7, Determinism versus precision in the step budgets

**The problem.** [`11.3`](11-residual-and-composition.md) forbids wall-clock timeouts, so budgets are step counts, so a proof that would have finished in 3ms may be cut off while one taking 400ms is permitted. The budgets must be tuned, and tuning them against a corpus risks overfitting to that corpus in a way that shows up as unpredictable discharge on code unlike it.

**The specific decision that is open:** whether budgets should be *per function* (simple, but a 5,000-line function gets the same allowance as a 5-line one) or *scaled by function size* (fairer, but then a small edit that grows a function changes discharge elsewhere in it).

**How it gets answered:** V1 for the shape, V3 for the tuning, and the metric is the variance of `D_s` across the corpus under small source perturbations, which is a test nobody runs and which is exactly what [`02.8`](02-the-goal.md)'s failure condition F5 is about.

## Deferrals

**Deferral 1, A verified compiler.** CompCert exists. The parent's [`spec/15-testing.md`](../15-testing.md) already sets the disposition: verify the narrow mechanical things, test everything else hard. Nothing here changes it.

**Deferral 2, Machine-learned discharge policies.** A learned policy for which layer to attempt, or which packs to build, would probably help and is not verifiable. It is also unnecessary: [`04.11`](04-the-discharge-ladder.md) permits profiles to *prioritize* effort, which captures most of the benefit with none of the trust cost.

**Deferral 3, C++.** The parent puts it out of scope and [`../safe-memory/04.6`](../safe-memory/04-safety-model.md) depends on that. Object lifetime within a storage instance is not modelled, and in C++ it would have to be.

**Deferral 4, A standalone analyzer product.** [`02.9`](02-the-goal.md).

## The companion's Deferral 2, resolved

[`../safe-memory/17`](../safe-memory/17-open-questions.md) defers sound whole-program abstract interpretation with this reasoning:

> Frama-C's Eva and Verasco are the right technology for proving checks away rather than eliminating them locally, and building one is a decade. The narrow-verified-rule approach is chosen because its failure mode is a surviving check (a performance bug) rather than a missing one. *This is also the boundary with the compile-time-proof specification at `../compile-time-safe-memory/`, which takes the opposite position and should be read against this one.*

**Both positions are correct and they are not actually opposed**, which is worth stating plainly rather than leaving as a forward reference.

The companion is right that *whole-program sound abstract interpretation* is a decade and that building one as the sole basis for check removal would be reckless. This specification does not propose one. What it proposes is **per-function, budgeted, incomplete, certificate-emitting analysis whose failure mode is exactly the companion's**: a surviving check. Layers 1 and 2 are abstract interpretation, but they are Astrée's *techniques* at a function's scale under a compiler's budget, not Astrée's *ambition* at a program's scale under an analyzer's budget.

The reconciliation, in one line: **the companion rejected whole-program soundness as a foundation; this specification rejects it too, and builds the local machinery anyway, because a local proof and a verified rewrite rule are the same object with different bookkeeping.**

## The questions the companion and parent already own

Pointers only, because they bear on this specification and should not be re-answered here.

- **Companion Q1, aliased kernel mappings.** If Tier K's claim shrinks, so does everything this specification does for the kernel.
- **Companion Q2, checks and the ægraph.** [`11.5`](11-residual-and-composition.md)'s ordering assumes checks stay in the CFG skeleton. If they became e-graph nodes, layers 0-1 would compose with every rewrite automatically and this ladder's bottom would look different.
- **Companion Q3, PICO+CHOP composition.** That is a question about how much *elimination* sources overlap; this document set's funnel ([`04.10`](04-the-discharge-ladder.md)) is the general form of the same question and V3 answers both at once.
- **Companion Q6, type-plane granule homogeneity.** Decides whether [`03.4`](03-obligations.md)'s granule narrowing is available.
- **Parent Q5, the no-poison uninitialized-read model.** [`../safe-memory/09.2.1`](../safe-memory/09-type-init-and-races.md) argues the model is a requirement for a monitor; it is also what makes `O.init` a well-defined obligation rather than a statement about a program whose behavior is already undefined.

## What is not an open question

Stated because a list of open questions reads as a list of everything uncertain.

**Obligations come from J1-J7 and are not generated by anything else.** [`03.1`](03-obligations.md). One definition of safety across both specifications.

**No alarms.** [`00`](00-README.md). An undischarged obligation is a check, never a diagnostic, and the only diagnostic modes are opt-in.

**No new escape hatches.** [`10.7`](10-soundness-and-trust.md). Every failure degrades to the companion's behavior exactly.

**Search is untrusted; the checker is trusted.** [`04.1`](04-the-discharge-ladder.md), [`10.4`](10-soundness-and-trust.md). This is what licenses heuristics, external tools and generated artifacts, and it is not revisitable without discarding [document 09](09-inference-and-llm.md) entirely.

**Annotations are hints.** [`08.1`](08-annotations.md). The adoption argument is empirical (`-fbounds-safety` shipped and Checked C did not) and it decides against a dialect.

**Layers 0-3 use no solver.** [`12.4`](12-integration.md). This is what keeps the compiler's dependency set at the two crates the parent blessed, and a technique that violates it is not a `default`-level technique.

**Generated artifacts are committed as source, never produced during a build.** [`09.5`](09-inference-and-llm.md). Reproducibility.
