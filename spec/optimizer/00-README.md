# Ox: the optimizer, phase by phase

`spec/09-optimizer.md` is nine pages and it describes the optimizer the way a prospectus describes
a building. It names the pipelines, it settles the architectural bets, and it is right about all of
it. What it does not do, and was never meant to do, is tell somebody sitting down on a Monday
morning what to type. This directory does that. One document per phase, forty four of them, each
detailed enough to implement from and each grounded in two things that did not exist when the
parent spec was written: a complete local copy of the GCC 16.2.0 source tree, and eighteen months
of research that has moved several of the parent spec's open questions.

These documents supersede nothing. Where one of them disagrees with `spec/09-optimizer.md` it says
so in a section called *Where this departs from spec 09*, and the parent document is the one that
gets amended. The departures are collected in document 43, which is also where the amendments to the
parent documents are staged: twenty from `spec/09-optimizer.md` and nine from `spec/10-backend.md` as
of this writing, with the tables in 43.5 and 43.6 as the record rather than a count in prose.

## What changed since spec 09 was written

Three things, and each one is load-bearing.

**GCC 16 shipped.** 16.1 on 30 April 2026 and 16.2 on 7 August 2026. The pinned tree in
`gcc-internals/vendor/gcc` is `releases/gcc-16.2.0`, commit `78d4ac73d`, and every claim in these
documents about what GCC does points at a file and a line in it. That tree contains 386 pass
instances over 297 distinct passes, and document 02 is all of them, generated rather than
recalled. GCC 16 is also the first release in some years to add genuinely new middle-end passes
rather than refine old ones: `crc_optimization`, `sccopy`, `expand_pow`, `ipa_locality_cloning`,
`ext_dce`, `late_combine`, `fold_mem_offsets` and `avoid_store_forwarding` are all recent, and
several of them are cheap enough that rucc should have them in M4 rather than later.

**The e-graph question got evidence.** Spec 09.2 calls the ægraph the largest architectural bet in
the project and spec 19 makes it open question one. Since then the EGRAPHS workshop has run twice
more and, more usefully, `E-Graphs as a Persistent Compiler Abstraction` (arXiv 2602.16707)
reports 1.18x on a software case study from keeping the e-graph alive across abstraction levels
rather than saturating and discarding it. Document 12 rewrites the M4 experiment in light of it.
The experiment still happens and it is still a real decision point; what changed is that we now
know what the failure mode looks like and can measure for it directly instead of discovering it.

**Verification tooling matured.** Alive2's memory-model encoding covers the whole of LLVM's
intraprocedural memory optimization surface, Minotaur's OOPSLA 2024 numbers (7.3% on GMP, 1.5% on
SPEC CPU 2017, every rewrite formally verified) established that synthesised peepholes are worth
shipping, and the 2026 CGO cluster on verification-guided optimization says plainly what the field
learned: the generation of a rewrite is the easy half. Document 13 and document 41 are where this
lands.

## The rules these documents follow

**Every claim about GCC carries a citation that resolves.** The form is `gcc/tree-ssa-ccp.cc:2431`
and it means line 2431 of that file in `releases/gcc-16.2.0`. Document 01 says how they are
checked and how they are re-checked when the tree moves. A claim about GCC without a citation is
a claim about GCC 4.4 that somebody has been repeating, which is the specific failure mode the
`gcc-internals` project exists to fight, and it would be embarrassing to reproduce it here.

**Every claim about rucc points at code or says it does not exist yet.** As of this writing
`rucc-opt` is 1,202 lines: the pass manager, the fuel counter, six pipelines and one pass. Nearly
everything in these documents is unbuilt. Where a document says rucc *does* something it means
today; where it says rucc *will* it means M4 and there is an entry in document 43's sequence.

**Every phase says what it costs.** Not "this is a standard optimization" but a compile-time
budget as a fraction of the pipeline and an expected win on the benchmark set, because spec 9.10's
rule is that a pass without a measured win does not ship, and a document that does not predict a
number cannot be wrong, which makes it useless.

**Every phase says how it is wrong.** A section on the miscompilation it will cause, what fuel
bisection looks like for it, and what the verifier or the SMT obligation catches. This is the
section that makes these documents worth more than the GCC manual.

## Reading order

If you are implementing M4, read 43 first, then 04, then follow its sequence. Document 43 is the
work plan and it orders these documents by dependency rather than by number.

If you are trying to understand the design, read 00, 03, 12 and 40, in that order. Those four
carry the whole argument: what the levels mean, how the value-level optimizer works, and what the
cost models say. Everything else is a phase.

If you are here because a program came out wrong, read 41.

## The documents

**The frame.**

| | | |
|---|---|---|
| 00 | this file | the map, what changed, the standing rules |
| 01 | `01-method-and-citations.md` | how these documents were made, how to regenerate them |
| 02 | `02-gcc16-pass-inventory.md` | all 386 pass instances, annotated, generated |
| 03 | `03-optimization-levels.md` | GCC's `-O` tables and rucc's, side by side |
| 04 | `04-pass-manager.md` | the pass and analysis managers, fuel, dumps, determinism |
| 05 | `05-research-2026.md` | what the literature says now and what each result forces |

**Analyses.** Nothing below transforms anything. Everything above depends on them.

| | | |
|---|---|---|
| 06 | `06-cfg-and-dominators.md` | the CFG, edges, dominators, frontiers, post-dominance |
| 07 | `07-loops-and-scev.md` | the loop forest, canonical form, trip counts, scalar evolution |
| 08 | `08-alias-analysis.md` | the six layers, points-to, modref, TBAA, provenance |
| 09 | `09-memory-ssa.md` | memory SSA, the clobber walk, and what it costs |
| 10 | `10-value-ranges.md` | ranges, GORI, relations, and GCC's ranger |
| 11 | `11-profile-and-frequency.md` | static prediction, block frequency, PGO, `__builtin_expect` |

**The value-level optimizer.**

| | | |
|---|---|---|
| 12 | `12-egraph.md` | the acyclic e-graph, extraction, GCM, and the M4 experiment |
| 13 | `13-rewrite-rules.md` | the rule DSL, GCC's `match.pd`, the SMT obligation |
| 14 | `14-constant-propagation.md` | SCCP, bit-CCP, alignment, `__builtin_constant_p` |
| 15 | `15-copy-and-forward-propagation.md` | copy propagation, forwprop, phiprop, SCC copy, uncprop |
| 16 | `16-gvn-and-pre.md` | value numbering, full and partial redundancy, code hoisting |
| 17 | `17-dce-and-dse.md` | dead code, control-dependent dead code, dead stores, sinking |
| 18 | `18-sroa.md` | scalar replacement of aggregates, and why C needs it most |
| 19 | `19-reassociation-and-arithmetic.md` | reassociation, division, `pow`, widening multiply, bswap, CRC |
| 20 | `20-idioms-and-libcalls.md` | string idioms, store merging, `memcpy` recognition, object sizes |

**Control flow.**

| | | |
|---|---|---|
| 21 | `21-cfg-simplification.md` | block merging, unreachable code, tail merging, tracer |
| 22 | `22-phiopt-and-if-conversion.md` | phi optimization, conditional stores, if-conversion, min/max |
| 23 | `23-jump-threading.md` | forward and backward threading, the dominator walk, the cost cap |
| 24 | `24-switch-lowering.md` | jump tables, bit tests, binary trees, switch conversion, if-to-switch |
| 25 | `25-tail-calls.md` | sibling calls, tail recursion into loops, `musttail` |

**Loops.**

| | | |
|---|---|---|
| 26 | `26-loop-canonicalization.md` | rotation, header copying, preheaders, exits, IV canonicalization |
| 27 | `27-licm.md` | invariant motion, store motion, sinking, predictive commoning |
| 28 | `28-induction-variables.md` | IV selection, address modes, strength reduction, `doloop` |
| 29 | `29-unrolling-and-peeling.md` | complete unrolling, partial, peeling, unroll-and-jam |
| 30 | `30-loop-restructuring.md` | unswitching, splitting, versioning, interchange, distribution |
| 31 | `31-dependence-analysis.md` | data references, GCD, Banerjee, the dependence graph |
| 32 | `32-vectorization.md` | loop vectorization, SLP, the cost model, and GCC 16's new ground |

**Interprocedural.**

| | | |
|---|---|---|
| 33 | `33-inlining.md` | early inlining, the IPA inliner, the cost model, the badness function |
| 34 | `34-ipa.md` | constant propagation, SRA, pure/const, modref, ICF, cloning, splitting |
| 35 | `35-lto-and-pgo.md` | monolithic and thin LTO, the plugin protocol, profiles |

**The back end's optimizer.**

| | | |
|---|---|---|
| 36 | `36-lowering-and-isel.md` | out of SSA, lowering the wide and the weird, instruction selection |
| 37 | `37-machine-level-optimization.md` | combining, extension elimination, address folding, peepholes |
| 38 | `38-scheduling-and-layout.md` | list scheduling, block reorder, alignment, hot/cold splitting |
| 39 | `39-register-allocation.md` | the backtracking allocator, coalescing, spilling, rematerialization |

**Cross-cutting.**

| | | |
|---|---|---|
| 40 | `40-cost-models.md` | the one place a number that decides a transformation is allowed to live |
| 41 | `41-correctness.md` | verification, fuel bisection, translation validation, fuzzing |
| 42 | `42-measurement.md` | the benchmark set, the regression gate, how a pass earns its slot |
| 43 | `43-plan.md` | the M4 sequence, the exit criteria, the departures from spec 09 |

## The thing to keep in mind while reading all of this

GCC's middle end is about 258,000 lines. rucc's is 1,202 and its budget is a fraction of GCC's
because there are not sixteen people. So the interesting question in every one of these documents
is not "what does GCC do" but "what is the eighty percent of GCC's win that fits in a tenth of
GCC's code", and every document answers it explicitly in a section called *The subset rucc
builds*. Spec 02's target is within 10% of `gcc -O2` on scalar integer and pointer code. That is
a number reachable by implementing roughly thirty transformations properly. It is not reachable by
implementing ninety of them badly, and the failure mode of a project like this one is always the
second thing.
