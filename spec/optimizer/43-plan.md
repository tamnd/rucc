# 43. The plan

Documents 02 through 42 are forty one documents describing what to build. This one says in what
order, what has to be true at the end, and where the preceding forty one disagree with the
specification they are elaborating.

Spec 17 gives M4 three to five months and defines it as: "the pass manager with fuel and dumps, the
ægraph and its extraction with GCM, the rewrite rule set, alias analysis, Memory SSA, the scalar
pipeline, and the `-O1`/`-O2` level definitions. The backtracking register allocator and its checker.
Scheduling and block layout." That is the scope and nothing here widens it.

## 43.1 The shape of the work

Three properties of M4 determine the sequence and none of them is obvious from the pass list.

**The e-graph experiment is on the critical path and it needs the rule set first.** Spec 19's open
question one is answered by building two rewriters over one rule set (documents 12 and 13). The rule
set is therefore the earliest large piece of work and the experiment cannot start until enough of it
exists to be representative. This inverts the natural instinct, which is to build a rewriter and add
rules to it.

**The back end and the middle end are independent for most of M4.** Documents 36 through 39 depend on
the MIR and on the target descriptions, not on anything in documents 12 through 35. They can proceed
in parallel from week one and the only synchronisation point is the interface between the optimizer's
output and selection's input, which spec 08 already fixes.

**The measurement infrastructure is a prerequisite, not a deliverable.** Document 42's first-tier
experiments answer questions that change the design, and they need the corpus, the statistics records,
the pass-disable flags and the benchmark harness. Building those in month four means the design
decisions in months one through three were made without evidence.

## 43.2 The sequence

Phases, not weeks, because the durations depend on how many people. Within a phase the items are
roughly parallel; between phases the dependency is real.

**Phase 0. The floor.** Nothing optimizes yet.

| Item | Document | Why first |
|---|---|---|
| Pass manager: ordering, properties, invalidation, fuel, dumps, determinism | 04 | Everything registers with it |
| The statistics record in the pass return type | 42.2 | Retrofitting it is the mistake GCC made |
| `-fdisable-<pass>[=range]`, `-fenable-<pass>[=range]`, `-fopt-info` with `missed` | 41.6, 42.2 | Debugging interface before there is anything to debug |
| Verifiers: SSA, block-parameter arity, CFG, types, driven by IR properties | 41.4 | Every pass after this is checked |
| The cost type, the two target tables, the constant file | 40.2, 40.12 | A pass with an inline constant is a pass to rewrite later |
| CFG, dominators, frontiers, post-dominance | 06 | Everything |
| The corpus manifest and the `xtask` harness | 42.3 | First-tier experiments need it |

**Phase 1. Analyses.** Still nothing optimizes.

| Item | Document |
|---|---|
| Loop forest, canonical form, trip counts, SCEV | 07 |
| Alias analysis: the layers rucc builds, points-to, TBAA with its clean off switch | 08, 41.9 |
| Memory SSA and the clobber walk, with its walk limit as a named constant | 09 |
| Value ranges | 10 |
| Static prediction, block frequency, the profile-quality field | 11 |
| The register-pressure model, one function, four consumers | 40.6 |
| The purity classifier, exhaustive, `Opaque`-defaulting | 41.3 |

**Phase 2. The value level, and the experiment.** The largest phase.

| Item | Document |
|---|---|
| The rule DSL and enough of the rule set to be representative | 13 |
| SMT verification of the rules, with the unverified list as a checked-in file | 41.8 |
| Arm A: the acyclic e-graph, extraction, GCM | 12 |
| Arm B: the conventional apply-once pipeline over the same rules | 12.3 |
| **Experiment 7: measure both.** Answer spec 19's Q1 | 12.3, 42.5 |
| SCCP, bit-CCP, alignment, `__builtin_constant_p` | 14 |
| SROA, before the e-graph | 18, 03.7 |
| GVN, block-local load elimination at `-O1` | 16 |
| DCE, control-dependent DCE, DSE with trimming | 17 |
| Reassociation, division and `pow` expansion, widening multiply, bswap | 19 |
| Idioms and libcall recognition | 20 |

**Phase 3. Control flow.** Cheap, high-yield, and mostly independent of phase 2's outcome.

Documents 21 through 25: CFG simplification and the explicit cleanup group, phiopt and if-conversion,
jump threading, switch lowering, tail calls.

**Phase 4. Loops.**

Documents 26 through 31: canonicalization, LICM, induction variables, complete unrolling and peeling,
the unswitching and versioning subset, dependence analysis to the depth document 31 scopes.

**Phase 5. Interprocedural.**

Documents 33 and 34: early inlining, the IPA inliner with the badness function in profile-guess form,
and the IPA analyses that survive document 34.6's cut.

**Phase B. The back end, in parallel from phase 0.**

| Item | Document |
|---|---|
| The named pre-selection lowering group | 36.1 |
| The capability table, one table, three columns | 36.4 |
| The rule-driven selector and its generated matcher | 36.5, spec 10.2 |
| The unified stack-slot allocator | 36.7 |
| The backtracking register allocator: pressure, spill, assign, legalize | 39.7 |
| The allocation checker | 39.6, spec 10.4 |
| The ten machine-level passes | 37.9 |
| Block layout, at every level | 38.6 |
| The list scheduler, after allocation, one instance | 38.6 |

**Phase 6. Measurement and tuning.** Only now.

Document 42's second and third tiers: pass-off measurements per pass, then the constant sweeps of
experiments 60 and 61, in that order, after the pipeline is stable. Document 42.6 states the reason
explicitly and it is the sequencing error most likely to be made: **tuning constants against an
incomplete pipeline produces values that are wrong once the pipeline changes.**

## 43.3 What to do first if there is only time for a little

If M4 runs short, the order in which things get cut matters more than the order in which they were
planned. Ranked by contribution to spec 02's within-10%-of-`gcc -O2` target, which is roughly spec
16.6's ordering confirmed by documents 33, 08, 39 and 12:

**Cannot be cut**: the pass manager, the verifiers, alias analysis, memory SSA, inlining, GVN, DCE,
SROA, the register allocator, selection. This set alone is most of the target.

**Cut last**: LICM, SCCP, reassociation, jump threading, switch lowering, complete unrolling, the
machine-level ten.

**Cut first, in this order**: the vectorizer (already out of M4 per document 32.12), loop
restructuring beyond unswitching, the IPA analyses beyond inlining and pure/const, PRE beyond GVN
(already deferred per 16.3), code hoisting outside `-Os` (already so per 16.4), predictive commoning.

**The e-graph experiment is not cuttable**, because a milestone that ends without answering spec 19's
Q1 has not delivered the thing spec 17 says it delivers, and because arm B is not wasted work under
either outcome: it is the fallback pipeline if the answer is no, and it is the correctness oracle if
the answer is yes.

## 43.4 Exit criteria

Spec 17's three, restated and sharpened, plus five more that the preceding documents made necessary.

1. **Rung 0 passes at every optimization level.** Spec 17's.
2. **`-fpass-fuel` bisection demonstrably localizes an injected miscompilation.** Spec 17's. Document
   41.6 adds that `-fdisable-<pass>=<range>` must localize it to a function as well as to a pass.
3. **The first code-quality measurement against `gcc -O2` on the LLVM test-suite, published whatever
   it says.** Spec 17's, and spec 16.1's reporting rules govern how.
4. **Spec 19's Q1 is answered in writing**, with the measurement, whichever way it goes, and the
   losing arm's disposition recorded.
5. **Spec 19's Q3 is answered in writing.** Document 39.6 recommends writing our own allocator and
   reimplementing regalloc2's checker; the milestone must either confirm that against the
   inline-assembly and `asm goto` constraint test spec 19 names, or record the other answer.
6. **Spec 19's Q5 is measured.** The no-poison model's cost, per spec 19's own method, on the
   benchmarks where speculation matters. This is cheap and it has been open since the IR was designed.
7. **The compile-time budget of document 42.7 is met, or the overage is attributed to a named
   phase.** A milestone that lands at 3x `clang -O2` without knowing where the time went has failed
   axis 3 silently.
8. **The unverified rule list exists and every entry has a reason.** Document 41.8. The list may be
   long; it may not be absent.
9. **Every pass returns a statistics record and the nightly firing-count job runs.** Document 42.2.
   A pass that fires zero times on the corpus at the end of M4 is deleted or explained.
10. **The departures below are reconciled into spec 09 and spec 10.** Not into these documents.

## 43.5 The departures from spec 09

Spec 09 is the parent document and per document 00's rule it is the one that gets amended. Twenty
departures, grouped.

**Levels and structure.**

| # | Departure | Document |
|---:|---|---|
| 1 | `-Og` is absent from spec 09 and must be defined | 03 |
| 2 | Function-outermost traversal rather than pass-outermost | 04 |
| 3 | Explicit cleanup-group repetition with a stated count, not an implicit fixpoint | 17.5 |
| 4 | Every pass returns a statistics record; the pass interface changes | 42.2 |

**Placement within the pipeline.**

| # | Departure | Document |
|---:|---|---|
| 5 | SROA runs before the e-graph, not after | 03.7, 18 |
| 6 | `sccp` runs at `-O1`, for `__builtin_constant_p` resolution | 14.4 |
| 7 | phiprop runs at `-O2` after SROA | 15.3 |
| 8 | Block-local redundant load elimination runs at `-O1` | 16.2 |
| 9 | Code hoisting runs at `-Os` and `-Oz` only | 16.4 |
| 10 | Two phiopt instances at `-O2`, not one | 22.7 |

**Things spec 09 lists that M4 does not build.**

| # | Departure | Document |
|---:|---|---|
| 11 | Full expression PRE is deferred past M4; GVN only | 16.3 |
| 12 | No canonical induction variable pass | 29.3 |
| 13 | No partial unrolling by default | 29.4 |
| 14 | No vectorization in M4, though 03.4's `-O2` list names `slp` and 03.5's `-O3` list names `loop-vectorize` | 32.12 |
| 15 | Fixed-length vectors only; RVV is out of scope | 32.9 |
| 16 | No IPA cloning, ICF, splitting, aggregate parameter splitting, devirtualization, or bit and range IPA propagation in M4 | 34.6 |
| 17 | No polyhedral framework, ever, not merely deferred | 30.4 |

**Things spec 09 does not have that M4 adds.**

| # | Departure | Document |
|---:|---|---|
| 18 | Multiple-latch canonicalization, replacing spec 09's refusal to handle them | 26.3 |
| 19 | One shared register-pressure model, consumed by LICM, GCM, the scheduler and the spill phase | 40.6 |
| 20 | PGO before LTO in the post-M4 ordering, with GCC-compatible `.gcno`/`.gcda` emission as a separate compatibility deliverable | 35.5, 35.8 |

## 43.6 The departures from spec 10

Nine, from documents 36 through 40. Spec 10 is amended the same way.

| # | Departure | Document |
|---:|---|---|
| 1 | A named pre-selection lowering group with one entry point and one dump, membership enumerated | 36.1, 36.8 |
| 2 | One capability table with three columns, unifying the coverage exception list, the libcall list and the lowering group's membership | 36.4 |
| 3 | Out-of-SSA and temporary expression replacement are explicitly not adopted; the register allocator absorbs the problem | 36.3, 36.8 |
| 4 | One stack-slot allocator after register allocation, sharing slots between locals and spills using the allocator's own liveness | 36.7 |
| 5 | Ten machine-level passes, not GCC's eighty; `lower-subreg`, `mode-switching`, modulo scheduling, early rematerialization and delay slots declined by name | 37.5, 37.9 |
| 6 | Block layout runs at `-O0` as well, following GCC's enabling of `-freorder-blocks` at `-O1` | 38.3, 38.6 |
| 7 | One scheduler, placed after register allocation, rather than GCC's two | 38.6 |
| 8 | Assignment and legalization are separated, an axis spec 10.4 did not encode; plus a degradation threshold, rematerialization inside the allocator, and the parallel-move sequencer as a named component | 39.1, 39.4, 39.7, 39.10 |
| 9 | Two target cost tables, speed and size, differing only in numbers and never in capability, plus a boolean tuning set | 40.3, 40.4 |

**Twenty-nine departures in total.** Document 00 says six. That sentence is wrong and 43.9 says what
to do about it.

## 43.7 Spec 19's open questions

**Q1, the ægraph.** Open. It is M4's central experiment and exit criterion 4. Documents 12 and 13
specify both arms and document 42's experiment 7 specifies the measurement. What changed since spec 19
was written is that document 00 records new evidence, arXiv 2602.16707's 1.18x from keeping the
e-graph alive across abstraction levels, which means the experiment should measure a third arm: the
e-graph persisting into lowering rather than being discarded after extraction. Document 12.3 calls
these arms A, B and C.

**Q2, our own linker.** Untouched by M4. Settled at M11 per spec 19. Default remains no.

**Q3, our own allocator or `regalloc2`.** **Document 39.6 recommends our own, with regalloc2's
checker design reimplemented rather than inherited.** The reasoning: the constraint model must express
x86 two-address forms, inline assembly's constraint language and `asm goto`'s edge-dependent
liveness, which is spec 19's own deciding test, and document 39.1's assign-versus-legalize separation
is a structure regalloc2 does not have and rucc needs for the `-O0` path as well. This is recorded as
a recommendation, not a closure. Exit criterion 5 closes it.

**Q4, the header cache.** Not M4. Settled at M5.

**Q5, the no-poison model's cost.** Unmeasured and cheap to measure. Promoted to exit criterion 6,
because it has been open since the IR was designed and every document from 12 onward assumes the
answer is small.

**One question spec 19 does not have and should.** Whether rucc emits GCC-compatible `.gcno` and
`.gcda`. Document 35.5's position is that this is a compatibility deliverable independent of whether
rucc ever consumes a profile, since a user's existing `gcov` workflow is part of what "GCC
compatible" means. It is bounded, it is testable against `gcov` itself, and nobody has decided to do
it. It belongs in spec 19 as a deferral with an obvious default.

## 43.8 Risks

**The e-graph is slow on large functions and the answer to Q1 is no.** The mitigation is structural
and already made: the rules are separate from the engine, so arm B is a complete fallback. The cost of
the wrong answer is the e-graph's implementation time, not the milestone.

**The rule set is the schedule.** Document 36.6 estimates 600 to 900 selection rules and document 13
adds the middle-end rewrite rules on top. This is the largest single quantity of work in M4 and it is
the one most likely to be underestimated, because each rule is small. The defence is that rule count
is a tracked number from week one and that document 42's experiment 8, rules fired against rules
present, runs continuously so that effort goes to rules that fire.

**The measurement infrastructure gets deferred.** The standard failure. Phase 0 puts it first
specifically because it is the thing that gets postponed when the interesting work is available, and
exit criterion 9 makes its absence a milestone failure rather than a note.

**The register allocator overruns.** Document 39.7 estimates roughly 4,000 lines across its
components and allocators are famous for taking longer. It is on phase B's critical path and it has
no fallback other than the single-pass allocator, which is a correctness fallback and not a
code-quality one. The early indicator is document 42's experiment 59, the legalization fixpoint's
iteration distribution: a fixpoint that does not converge in a small bounded number of iterations is
the sign that the constraint model is wrong, and it shows up early.

**Compile time accumulates invisibly.** Forty transformations each costing half a percent is a
compiler that missed axis 3 and nobody noticed which pass did it. Document 42.7's per-phase budget
checked at each phase boundary is the defence, and it only works if it is actually checked.

**Tuning happens too early.** Document 42.6. Constants tuned in phase 2 against a pipeline that
gains twenty more passes in phases 3 through 5 are wrong, and worse, they are wrong in a way that
looks like a working tuned compiler.

## 43.9 Corrections to document 00

Two, both mechanical, recorded here rather than silently applied so the record shows what changed.

**Document 00 line 13 says "There are six such departures so far and they are collected in document
43."** There are twenty from spec 09 and nine from spec 10, twenty-nine in total, and the count will
move again. The sentence should read: "The departures are collected in document 43, which is also
where the amendments to the parent documents are staged." A count in prose is a thing that goes stale;
the tables in 43.5 and 43.6 are the record.

**Document 01.4 says the eleven decisions forced by the research are "listed in document 05.6".** They
are section 5.7 of `05-research-2026.md`. One of the two references is wrong and 05 is the one with
the content, so 01.4 is the one to fix.

## 43.10 The decision

M4 is: phase 0's floor including the measurement infrastructure, phase 1's analyses, phase 2's rule
set and the two-arm e-graph experiment, phases 3 through 5's scalar, loop and interprocedural
pipelines, phase B's back end in parallel throughout, and phase 6's tuning only once the pipeline is
stable. It exits on ten criteria of which spec 17 supplies three. It contains twenty-nine departures
from the parent specification, all of them enumerated above and none of them discovered during
implementation.

**The claim these forty-four documents make, and the one that M4 tests:** spec 02's target of within
10% of `gcc -O2` on scalar integer and pointer code is reachable by implementing roughly thirty
transformations properly, against GCC's roughly three hundred. Every document in this directory has a
section called *The subset rucc builds*, and this document is where those subsets are added up. The
sum is about thirty passes in the middle end and about twenty in the back end, at an estimated
30,000 to 40,000 lines against GCC's 258,000 middle end and 59,616-line register allocator.

That ratio, roughly a seventh, is the bet. Document 00 states the failure mode: it is not reachable
by implementing ninety transformations badly, and that is always what happens. The defences against it
are all in this directory and they are the same three: every pass has a measured win before it ships
(spec 09.10, document 42.4), every pass reports whether it fires (document 42.2), and every pass that
was declined was declined by name with a reason (documents 34.6, 37.5, 43.5). A compiler that keeps
those three habits can afford to be small.
