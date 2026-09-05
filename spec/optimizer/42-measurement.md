# 42. Measurement

Documents 12 through 41 each end with a list of things to measure. This document collects them, says
what infrastructure they need, and defines the corpus they run against.

The reason it is a separate document rather than a section in each: **an optimization's measurement
list is written by the person who is enthusiastic about the optimization, and the list is therefore
biased toward showing that it works.** Collecting them lets the experiments be prioritised against
each other by cost and by how much they would change the plan, which is a different ordering from the
one they were written in.

Spec 16 already fixes the methodology: median of ten runs with the interquartile range, the whole
suite including losses, no cross-machine comparison, per-benchmark noise-derived thresholds, peak RSS
alongside every time, instruction counts alongside cycle counts. **None of that is restated here.**
This document adds what M4 specifically needs, which is per-optimization attribution rather than
whole-compiler numbers.

## 42.1 The three questions

For every transformation in documents 12 through 39 there are three questions and they are routinely
confused.

**Does it fire?** How many times, on what code. A pass that fires zero times on the corpus is either
unnecessary or broken, and the two look identical in a run-time measurement. This is the cheapest
question and the one most often skipped.

**Does it help?** Turn it off, measure the corpus. This is the only honest answer and it is expensive,
because it needs a full run of the code-quality suite per pass.

**What does it cost?** Compile time, in the pass and in the passes downstream that now have more or
less work. The second half is the one that gets missed: an inliner that makes everything faster and
triples the work of every subsequent pass has a cost that does not appear in its own timer.

**The three answers can disagree**, and the interesting case is the pass that fires thousands of times
and changes nothing measurable. Documents 15, 21 and 37 all predicted instances of this. A firing
count is not evidence of value and treating it as one is how a compiler accumulates passes.

## 42.2 What GCC provides, and what to copy

**Statistics counters.** `gcc/statistics.h:68`:

```c
extern void statistics_counter_event (struct function *, const char *, int);
extern void statistics_histogram_event (struct function *, const char *, int);
```

A named counter and a named histogram, per function, dumped per pass. The counts of call sites:
`combine.cc` 12, `tree-ssa-math-opts.cc` 9, `tree-ssa-phiopt.cc` 9, `tree-sra.cc` 7,
`gimple-ssa-store-merging.cc` 6, `tree-ssa-reassoc.cc` 6, `tree-ssa-forwprop.cc` 6,
`tree-ssa-sccvn.cc` 5, `tree-ssa-pre.cc` 4, `tree-ssa-dce.cc` 4, and roughly twenty other files with
one to three each.

**That is about a hundred instrumented events across a compiler with three hundred passes**, which is
the honest state of the art and is not good enough. The distribution is also telling: the
instrumented passes are the ones that were hard to debug, so instrumentation is retrofitted where
somebody was already suffering.

**rucc's version, and this is the one structural improvement worth making over GCC:** the counter is
not a function a pass calls, it is a **return value the pass is required to produce**. Spec 09's pass
interface should have every pass return a statistics record, so that a pass that reports nothing is
visible as a pass that reports nothing rather than as a pass nobody instrumented. The record is
`(event_name, count)` pairs and it costs the pass one line per transformation site. This makes the
question "does it fire" answerable for every pass on day one instead of for a third of them after ten
years.

**`-ftime-report`** (`gcc/common.opt:3154`) and `-ftime-report-details` at :3158 give the per-pass
time breakdown; spec 16.2 already commits rucc to recording it in CI.
`contrib/compare_two_ftime_report_sets` exists to diff two of them, which is the tool that turns a
compile-time regression into a pass name.

**`-fmem-report`** at :2402 and `-fmem-report-wpa` at :2406 do the same for allocation. rucc's arena
design (spec 03) makes this both easier and more important: arena high-water marks per pass are a
direct measurement and a pass whose arena use is superlinear in function size is visible immediately.

**`-fopt-info`** (`gcc/doc/invoke.texi:20403`) reports what fired and, importantly, **what did not**:
its keyword set is `optimized`, `missed`, `note`, `all`. The `missed` category is the one that
matters for tuning, because it converts "this loop was not vectorized" from a mystery into a message.
rucc should have the same three categories from the start and should hold passes to reporting misses,
because a pass that only reports successes cannot be tuned.

**`-fprofile-report`** at :2738, "Report on consistency of profile", is the self-check that document
11 asked for: after every pass, is the profile still consistent, meaning do block frequencies still
sum correctly across edges. **This is a correctness check for a thing that has no correctness
consequence**, which is why it needs its own reporting: a pass that corrupts the profile produces
correct, slower code, and nothing else notices.

**`contrib/analyze_brprob.py`** computes "coverage and hitrate" for each branch-prediction heuristic:
what fraction of dynamic branches each heuristic claims, and how often it is right. This is exactly
the measurement document 11.2 owes and the script's existence means the methodology does not need to
be invented.

**`contrib/bench-stringop`** is the block-copy threshold harness of document 40.4.

The pattern across all six: **the measurement tooling lives in the repository next to the thing it
measures.** rucc should follow it, under `xtask`, per spec 16.1's requirement that any published
claim be reproducible by a documented command.

## 42.3 The corpus

Spec 16 names the benchmark suites. The corpus is a different thing: a body of C source used for
firing counts, compile-time measurement, size measurement and crash testing, where the programs do not
all need to run.

**The requirement is coverage of C as it is actually written**, which the benchmark suites do not
provide because they are selected for being benchmarks.

- **SQLite amalgamation.** One 250k-line file. Already in spec 16.2 for throughput; it is also the
  best single test of superlinear behaviour, per document 04's pass-manager bounds.
- **The Linux kernel at `defconfig`.** Thousands of small files, heavy GNU extension use, heavy
  `inline` and `__attribute__` use, and the most demanding user of document 13's GNU compatibility.
  It exercises `-Os` and it is where the block-layout and alignment measurements of document 38 have
  real stakes.
- **Postgres, zlib, FFmpeg, curl, OpenSSL, git, Python, Redis, musl.** Mid-size real programs with
  different idiom profiles: FFmpeg for vector and inline asm, OpenSSL for bit manipulation and
  document 20's idiom recognition, Redis and git for pointer-chasing, musl for the small
  hand-optimized functions where document 39's allocator is visible, Python for the interpreter
  dispatch loop that document 40.10's switch measurement needs.
- **A generated-code set**: at least one bison/flex output, one protobuf output, and one heavily
  macro-expanded program. Generated C has pathological shapes real C does not, notably enormous
  switches, enormous functions and enormous basic blocks, and it is where document 04's bounds get
  tested.
- **The pathological set** from spec 16.2: a 100k-line function, deep nesting, a 10k-case switch.
- **A UB-clean subset**, marked, for the flag-consistency and optimization-level-consistency jobs of
  document 41.7. Determining which programs are UB-clean is done once with sanitizers and recorded.

**The corpus is checked in as a manifest of URLs and commit hashes, not as source.** It is fetched by
`xtask` and pinned, because a corpus that drifts makes historical numbers meaningless, which is the
same failure spec 16.1's rule 5 forbids across machines.

## 42.4 The default experiment: the pass off

For most of the questions below, the experiment is the same and it is worth naming once so the
individual entries can be short.

**Pass-off**: build the corpus at `-O2` with the pass enabled and disabled, using document 41.6's
`-fdisable-<pass>`, and report the code-quality suite's geometric mean, the per-benchmark table
including regressions, the code size, and the compile-time delta.

Three refinements that matter.

**Disabling a pass and never writing it are different experiments.** A pass that is disabled still
leaves the IR in the shape the previous passes produced for it. If the plan is to not write a pass,
the measurement wanted is with the pass absent from the pipeline, which for rucc's explicit pipeline
(document 04) is a different configuration and not a flag.

**The result is a distribution, not a number.** A pass that gains 0.5% on the geometric mean and 8% on
one benchmark is a different pass from one that gains 0.5% everywhere, and the decision to keep it
differs. Spec 16.1's rule 3 already requires the table; this is why.

**Pass-off measures the pass plus everything it enables.** Documents 29.2 and 33.3 both found that
cost must be judged after the folding a transformation enables. The same applies to measurement: a
pass whose only value is what it enables downstream measures as valuable, correctly, and the
attribution to it specifically is not recoverable from this experiment. When that attribution matters,
the experiment is a firing count in the downstream pass with and without the upstream one.

## 42.5 The experiment list

Collected from documents 11 through 41. The column "settles" says which decision the result changes,
because an experiment that changes no decision should not be run.

### Analysis quality

| # | Measurement | From | Settles |
|---:|---|---|---|
| 1 | Branch-heuristic coverage and hitrate, per heuristic, via `analyze_brprob`-style analysis | 11.2 | Which static heuristics to keep |
| 2 | How often static prediction is wrong against a real profile | 35.10 | The 40.11 frequency clamp |
| 3 | Alias oracle queries per function and answer distribution (must/may/no) | 08 | Whether flow-sensitive points-to earns its cost |
| 4 | Fraction of loops with a computable trip count via SCEV | 07 | Whether the SCEV subset is large enough |
| 5 | Memory SSA size against IR size, and query depth distribution | 09 | The walk-limit constant |
| 6 | Value-range width distribution at `-O2` | 10 | Whether the range representation needs more than intervals |

### The scalar pipeline

| # | Measurement | From | Settles |
|---:|---|---|---|
| 7 | E-graph arm A/B/C: saturation quality against compile time | 12.3 | The central architectural bet |
| 8 | Rules fired against rules present, per rule | 13 | Which rules to delete |
| 9 | `sccp` at `-O1`: `__builtin_constant_p` resolution rate | 14.4 | A departure from spec 09 |
| 10 | Pass-off for copy and forward propagation, given block parameters | 15.1 | Whether the pass exists at all |
| 11 | Block-local load elimination at `-O1`: hit rate and compile-time cost | 16.2 | A departure from spec 09 |
| 12 | Full PRE against GVN alone at `-O2` | 16.3 | Whether PRE is deferred past M4 |
| 13 | Hoisting at `-Os`/`-Oz`: size delta | 16.4 | A departure from spec 09 |
| 14 | DSE trimming: stores narrowed, and store-forwarding stalls introduced | 17, 40.7 | The trim minimum width |
| 15 | SROA before the e-graph against after | 18, 03.7 | A departure from spec 09 |
| 16 | Reassociation width sweep, 1 through 4, per target | 19, 40.8 | The default width |
| 17 | Idiom recognition: each idiom's firing count on the corpus | 20 | Which idioms to implement |
| 18 | Cleanup-group repetition: fixpoint iteration count distribution | 17.5, 21 | Whether the explicit repetition count is right |
| 19 | Two phiopt instances against one | 22.7 | A departure from spec 09 |
| 20 | Jump threading: threads found, code growth, and pass-off | 23 | The growth limit |
| 21 | Switch lowering: chain against tree against table, per case count, on an interpreter loop | 24, 40.10 | The mispredict threshold |
| 22 | Tail-call conversion rate and stack-depth effect | 25 | Nothing; it is unconditionally right |

### Loops

| # | Measurement | From | Settles |
|---:|---|---|---|
| 23 | Multiple-latch canonicalization: how many loops in the corpus have one | 26.3 | A departure from spec 09 |
| 24 | LCSSA maintenance cost, given block parameters | 26.4 | Confirms a structural claim |
| 25 | LICM: invariants hoisted, and spills introduced | 27, 40.6 | The pressure margin of 2 |
| 26 | ivopts: candidates chosen against original variables kept | 28, 40.9 | The complexity tiebreak |
| 27 | Canonical-IV pass absent: what breaks | 29.3 | A departure from spec 09 |
| 28 | Partial unrolling on against off at `-O2` | 29.4 | A departure from spec 09 |
| 29 | Complete unrolling threshold sweep | 29 | The unroll limit |
| 30 | Loop restructuring absent: measured cost against `gcc -O3` on loop-heavy code | 30.4 | The no-polyhedra decision |
| 31 | Dependence test: how often each test level is needed and how often it is inconclusive | 31 | Which tests to implement |
| 32 | SLP against no vectorization at `-O2` on the corpus | 32.12 | A departure from spec 09 |

### Interprocedural

| # | Measurement | From | Settles |
|---:|---|---|---|
| 33 | Inliner badness: decisions with a real profile against with static frequencies | 33.4, 40.11 | The frequency clamp value |
| 34 | Inlining growth against gain curve, sweeping the size limits | 33 | The growth parameters |
| 35 | IPA feature-by-feature: cloning, ICF, splitting, devirtualization, each absent | 34.6 | A departure from spec 09 |
| 36 | Cross-unit inlining's upper bound, via compiling the corpus as a single unit | 35.10 | Whether LTO is worth M5 |
| 37 | PGO's upper bound, via a foreign profile | 35.10 | The PGO-before-LTO ordering |

### The backend

| # | Measurement | From | Settles |
|---:|---|---|---|
| 38 | Selection's share of `-O2` compile time | 36.10 | The DAG-selection design |
| 39 | Generated matcher size, and rules present against rules fired | 36.10 | The 600 to 900 rule estimate |
| 40 | Frame size against `gcc -O2` | 36.10 | The unified slot allocator |
| 41 | Redundant extensions per thousand instructions after selection | 37.8 | Whether `ext-dce` is needed |
| 42 | Compare-elimination hit rate | 37.8 | Whether it is a pass or a rule |
| 43 | If-conversion on against off, and the budget sweep | 37.8, 40.5 | The 20/40 budgets |
| 44 | Addressing-mode folding: instruction count reduction | 37.8 | Whether it is a separate pass |
| 45 | Combine window depth 2, 3, 4: marginal value each | 37.3, 37.8 | The window bound |
| 46 | The machine-level group's share of compile time | 37.8 | The ten-pass decision |
| 47 | Scheduling on against off, per target | 38.8 | Whether each target needs it |
| 48 | Pipeline-model sensitivity: real model against a uniform-latency model | 38.8 | How accurate the first model must be |
| 49 | Post-allocation against pre-allocation scheduling | 38.6, 38.8 | The one-scheduler decision |
| 50 | Layout on against off, at each level including `-O0` | 38.3, 38.8 | Layout at `-O0` |
| 51 | Hot/cold partitioning on against off | 38.8 | Whether M4 does it |
| 52 | Alignment on against off, with the size cost | 38.5, 38.8 | The alignment thresholds |
| 53 | Allocator's share of compile time at `-O2` and at `-O0` | 39.9 | The two-allocator design |
| 54 | Spills and reloads per thousand instructions against `gcc -O2` | 39.9 | Allocator quality |
| 55 | Moves remaining after coalescing | 39.9 | Whether the cost-vector mechanism works |
| 56 | Single-pass allocator's code-quality penalty, against TPDE's 1.64x | 39.9, 05.4 | The `-O0` path |
| 57 | Degradation threshold: the knee in compile time against function size | 39.4, 39.9 | The threshold value |
| 58 | Rematerialization on against off | 39.3, 39.9 | Whether it is in the allocator |
| 59 | Legalization fixpoint: iteration count distribution | 39.9 | The iteration cap |

### Cross-cutting

| # | Measurement | From | Settles |
|---:|---|---|---|
| 60 | Sensitivity sweep at half and double for every constant in 40.12's table | 40.14 | Which constants need tuning at all |
| 61 | Block-copy move threshold, both directions | 40.7, 40.14 | `move_ratio` and `clear_ratio` |
| 62 | Profile consistency after every pass, `-fprofile-report`-style | 11, 42.2 | Which passes corrupt the profile |
| 63 | Rules verified, unverified and timed out | 41.8 | Whether the unverified list is growing |
| 64 | Firing counts for every pass, on the whole corpus, every night | 42.1 | Which passes are dead |

**Sixty-four experiments.** Most are cheap: a firing count is one build of the corpus with counters
on. The expensive ones are the pass-off measurements, since each needs a full code-quality run, and
the sensitivity sweep of number 60, which is two runs per constant against nineteen constants.

## 42.6 Priority

The ordering is by how much the answer changes the plan, not by how interesting it is.

**First, and before much of M4 is written**: number 7, the e-graph arm experiment, because it is the
central architectural bet and everything after document 12 assumes an answer. Number 56, the
single-pass allocator's penalty, because it sets whether the `-O0` path is viable. Number 37, PGO's
upper bound via a foreign profile, because it is cheap and it bounds a whole milestone.

**Second, as each pass lands**: its firing count, number 64, which should be automatic. Its pass-off,
which should be a required column in the pull request that adds it.

**Third, once the pipeline is complete**: numbers 60 and 61, the constant sweeps, because tuning
constants against an incomplete pipeline produces values that are wrong once the pipeline changes.
This is the most common sequencing error in compiler tuning and it is worth stating: **do not tune
until the pipeline is stable, and then tune all of it at once.**

**Continuously**: numbers 62, 63 and 64, as CI jobs with the ratchet discipline. The unverified rule
list may not grow, the set of passes that corrupt the profile may not grow, and a pass whose firing
count drops to zero is a build failure.

## 42.7 The compile-time budget

Spec 16.4 sets `-O2` throughput at 1.5x `clang -O2`. M4 spends that budget and the spending needs a
plan, because every pass in documents 12 through 39 costs something and the total is not checked
anywhere until it is too late.

The budget as a per-phase allocation, to be checked against `-ftime-report` at each milestone:

| Phase | Share of `-O2` |
|---|---:|
| Front end and semantic analysis | 25% |
| Analyses (alias, memory SSA, ranges, loops, SCEV) | 15% |
| The e-graph and rewriting | 15% |
| The remaining scalar and loop pipeline | 20% |
| Selection and lowering | 8% |
| Register allocation | 12% |
| Scheduling, layout, machine-level passes | 5% |

These are targets, not predictions, and their purpose is that a pass exceeding its phase's share is a
finding rather than an accepted fact. GCC's own distribution differs substantially, notably in
spending far more in the machine-level group, which is the direct consequence of document 37.1's
repetition.

**The single number to watch is the analyses' share**, because analysis cost is what turns a compiler
superlinear, and because it is the cost that grows silently as more passes want more precision.

## 42.8 How this is wrong

**The corpus is not the user's code.** Nine open-source C programs and a kernel are a sample, and
every constant tuned against them is tuned against that sample. The mitigation is breadth and the
honest statement of what was measured, per spec 16.1's rule 1.

**Pass-off measurements are not additive.** Turning off two passes is not the sum of turning off each,
because passes enable each other. The list above produces a set of single-pass numbers that will be
read as a decomposition and are not one. This should be stated wherever the numbers appear.

**Firing counts reward passes that fire.** A pass instrumented to count every rewrite looks busier
than one that counts only the rewrites that mattered, and the temptation to instrument generously is
real. The counter names should be specific enough that a reader can tell which they are looking at.

**Compile-time attribution is confounded by memory.** A pass that allocates heavily slows down the
passes after it through cache effects, and `-ftime-report` charges that to the wrong pass. The arena
high-water mark of 42.2 is the partial defence.

**Nightly measurement drifts.** Machines get microcode updates, kernels change, and a year of nightly
numbers contains discontinuities that look like regressions. Spec 16.5's per-benchmark noise
thresholds handle the noise; the discontinuities need the machine's state recorded with every row, and
that is a thing to build in rather than reconstruct.

**Measuring the wrong level.** Most of the list above is at `-O2`. Spec 16.4 notes that most
compilations in the world are `-O0` or `-O1`, and the M4 experiment list is, as written, biased toward
the level M4 is about. The correction is that numbers 50, 53 and 56 are `-O0` measurements and they
are in the first priority tier for that reason.

## 42.9 The decision

Every pass returns a statistics record; the corpus is a pinned manifest of eleven real programs plus a
generated set and a pathological set; the sixty-four experiments above are the M4 measurement plan
with the three-tier priority of 42.6; the compile-time budget of 42.7 is checked at each milestone;
and four things run nightly with a ratchet: firing counts, profile consistency, rule verification, and
the code-quality suite.

**The finding that shapes it:** GCC has about a hundred instrumented statistics events across three
hundred passes, and the instrumented ones are the ones that were hard to debug. Instrumentation is
almost always retrofitted, and retrofitting it is what makes the question "is this pass doing
anything" unanswerable for most of a compiler's life. Making the statistics record part of the pass
interface costs one line per transformation site and buys the answer permanently. It is the cheapest
structural decision in this document and it has to be made before the passes are written, not after.
