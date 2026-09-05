# 35. Link-time optimization and profile-guided optimization

Two ways of giving the optimizer information it does not have: what the rest of the program looks
like, and what the program actually did when it ran. They are separate features, they are usually
discussed together because they are the two things you turn on when `-O2` is not enough, and they
have opposite cost profiles. LTO is expensive to implement and cheap to use. PGO is cheap to
implement and expensive to use.

The code: LTO is 21,852 lines across nineteen files, largest being `gcc/lto-streamer-out.cc` (3,489),
`gcc/lto/lto-common.cc` (3,152), `gcc/lto-cgraph.cc` (2,331), `gcc/lto-wrapper.cc` (2,329),
`gcc/lto-streamer-in.cc` (2,223) and `gcc/lto/lto-partition.cc` (2,074). Profile support is 13,711
lines: `gcc/auto-profile.cc` (4,905), `gcc/tree-profile.cc` (2,126), `gcc/value-prof.cc` (1,962),
`gcc/profile.cc` (1,924), `gcc/coverage.cc` (1,410) and `gcc/mcf.cc` (1,384).

Neither is in M4. Document 11.5 already scoped PGO out and document 34.7 already made the argument
that shapes what M4 owes to LTO.

## 35.1 What LTO is, structurally

Three stages, and the names matter because every GCC diagnostic uses them.

**LGEN**, local generation. Each translation unit is compiled normally through the front end and the
early optimizers, and then, instead of emitting assembly, its GIMPLE, callgraph, types and IPA
summaries are serialized into sections of the object file. The object file is a real ELF object with
real sections; it just has the compiler's intermediate representation in it alongside (or instead of)
machine code.

**WPA**, whole-program analysis. The linker, through the linker plugin interface (`plugin-api.h`),
hands all the object files back to the compiler, which reads every unit's summaries, builds one
callgraph for the entire program, runs all the interprocedural passes of document 34 on it, decides
what to inline and clone and specialize, and then *partitions* the program into chunks.

**LTRANS**, local transformation. Each partition is read back, the recorded decisions are applied,
the ordinary optimizers run, and machine code is emitted. These run in parallel, which is the reason
partitioning exists.

`gcc/lto-wrapper.cc`, 2,329 lines, is the piece that orchestrates this from inside the linker's
process, including reconciling the command-line options that each unit was compiled with, which is
harder than it sounds and is a whole class of LTO bugs.

**The consequence for pass design, and it is the whole reason document 34.7 said what it did:** an
IPA pass under LTO cannot analyse and transform in one traversal, because the analysis happens in
WPA with only summaries available and the transformation happens in LTRANS with only one partition's
bodies available. Every IPA pass in GCC is written in three phases for this reason and for no other.

## 35.2 Streaming

`gcc/lto-streamer.h:32` lists what a function's encoding contains: a header, field declarations,
function declarations, global variable declarations, type declarations, types, label names, SSA
names, the control flow graph, GIMPLE for local declarations, GIMPLE for the function, and strings.

The hard part is not the GIMPLE, it is the types and declarations. Two translation units that both
included the same header have two copies of every type in it, and they must be unified, or the
program has two incompatible `struct stat`s. `gcc/lto/lto-common.cc` does this type merging and
`gcc/lto/lto-symtab.cc` (1,134 lines) does the equivalent for symbols, resolving which definition
wins when several units define the same name. `gcc/ipa-free-lang-data.cc` (1,229 lines) exists to
strip language-specific information from types before streaming so the merging has less to compare.

**For a C-only compiler this is meaningfully easier than for GCC**, which must make C, C++, Fortran
and Ada types meet in the middle. C's type identity rules are structural and the set of types is
small. That is one of the few places where rucc's narrower scope is a genuine implementation
advantage rather than just less work.

`gcc/lto-compress.cc` (414 lines) compresses the sections, because IR is large;
`gcc/lto-ltrans-cache.cc` (436 lines) is newer and caches LTRANS outputs by SHA-1 of their inputs, so
that an incremental rebuild does not recompile every partition. That cache is the answer to LTO's
worst practical property, which is that changing one line relinks and re-optimizes the world.

## 35.3 Partitioning

`gcc/lto/lto-partition.cc` divides the callgraph into partitions that are compiled independently and
in parallel. The parameters:

| Parameter | Line | Value |
|---|---:|---:|
| `lto-partitions` | 492 | 512 |
| `lto-min-partition` | 488 | 10,000 estimated instructions |
| `lto-max-partition` | 480 | 1,000,000 estimated instructions |
| `lto-max-streaming-parallelism` | 484 | 32 |

The tension: a function inlined into callers in three different partitions is emitted three times, so
partitioning interacts directly with the inliner's decisions, and a partition boundary drawn in the
wrong place undoes the inlining that LTO was for.

`gcc/ipa-locality-cloning.cc` (1,531 lines) is the newest work in this area and it is a different
objective entirely: partition for *code locality* rather than for balance. Its header at
`gcc/ipa-locality-cloning.cc:20` describes placing frequently executed call chains together in
memory, cloning a function into a second partition when it is needed by two chains that cannot be
co-located. Controlled by `lto-partition-locality-cloning` with three models, `no`,
`non_interposable` and `maximal`, defaulting to maximal (`gcc/params.opt:508`), plus frequency and
size cutoffs at 511 and 515.

This is worth noticing as a trend: **the recent work in LTO is not about better interprocedural
analysis, it is about instruction cache behaviour.** Large programs are front-end bound, and moving
code so that hot paths are contiguous is worth more than another round of constant propagation. The
same observation drives document 38's block layout and it is the same insight at a different scale.

## 35.4 What LTO buys, and what it costs

**Buys:** cross-translation-unit inlining, which is the main thing; interprocedural analysis over the
whole program, so `static`-like conclusions become available for non-`static` functions; removal of
functions and variables never referenced anywhere; and a single view for the partitioner to lay out.

**Costs, and they are not small:**

- Build system integration. The linker must load a plugin, `ar` and `nm` and `ranlib` must understand
  plugin-carrying objects, and every one of those is a place a build breaks.
- Memory. WPA holds summaries for the whole program in one process. This is the practical limit on
  LTO for large programs and it is why partitioning and streaming parallelism have parameters.
- Build time, especially incremental, mitigated but not solved by the LTRANS cache.
- Debuggability. A bug that only appears under LTO is a bug in a program the user cannot easily
  reproduce compiling.
- Option reconciliation. Units compiled with different flags must be merged into partitions with some
  flag set, and GCC's rules for this are intricate.

**And the honest accounting of the benefit:** LTO's gain over `-O2` on typical C programs is in the
low single digits of run time and can be larger for size, because unreferenced code removal is very
effective. It is not the multiple that its cost suggests. That is worth stating clearly because LTO
is a feature people ask for by name.

## 35.5 Profile instrumentation

`gcc/profile.cc:23` describes the instrumentation, and the algorithm is elegant enough to state in
full:

> First, the BB graph is closed with one entry (function start), and one exit (function exit). Any
> ABNORMAL_EDGE cannot be instrumented (because there is no control path to place the code). We close
> the graph by inserting fake EDGE_FAKE edges to the EXIT_BLOCK... To optimize the instrumentation we
> generate the BB minimal span tree, only edges that are not on the span tree (plus the entry point)
> need instrumenting. From that information all other edge counts can be deduced.

**The spanning tree trick** is the whole idea: in a graph with `V` blocks and `E` edges, only
`E - V + 1` counters are needed, because flow conservation at each block determines the rest. On a
typical CFG that is a large reduction, and it is what makes instrumented builds merely slow rather
than unusable. Critical edges are preferentially placed on the tree so they need not be split.

Two files are produced: `.gcno` at compile time, describing the CFG and the counter assignment, and
`.gcda` at run time, holding the counts. The format is in `gcc/gcov-io.h`.

**This is a compatibility surface, not just an implementation choice.** `gcov`, `lcov`, and every
coverage tool in the C ecosystem read those files. A compiler claiming GCC compatibility that emits
its own format has not implemented `-fprofile-arcs`, it has implemented something else with the same
spelling. rucc should emit GCC's `.gcno` and `.gcda` formats, and that is a bounded, testable
deliverable independent of whether rucc ever consumes a profile itself.

The flags around it are where the real-world difficulty lives:

- `-fprofile-update=[single|atomic|prefer-atomic]` (`gcc/common.opt:2658`). A counter incremented
  non-atomically from several threads loses updates, producing a profile that is wrong in a way that
  looks plausible. `atomic` is correct and slow.
- `-fprofile-reproducible=[serial|parallel-runs|multithreaded]` (2682), defaulting to `serial`,
  controlling how much reproducibility of the gathered profile is required.
- `-fprofile-partial-training` (2722): "Do not assume that functions never executed during the train
  run are cold." Without it, a function the training run never reached is optimized for size, which
  is catastrophic if the training run simply did not cover it. This flag exists because the default
  assumption is aggressive and wrong often enough to need an escape hatch.
- `-Wcoverage-mismatch` (897), `Init(1)`, warning when the profile does not match the source.

## 35.6 Value profiling

`gcc/value-prof.cc:46` lists what is profiled beyond edge counts, and the mechanism generalises:
histograms attached to statements. `gcc/value-prof.h:24` names seven types: `INTERVAL`, `POW2`,
`TOPN_VALUES`, `INDIR_CALL`, `AVERAGE`, `IOR` (used to compute expected alignment) and `TIME_PROFILE`.

The transformations they enable:

**Division and modulo specialization.** If a divisor is usually the same value, or usually a power of
two, emit a test and a fast path. Document 37 owns the division-by-constant expansion; this is the
observation that the constant can be discovered at run time rather than compile time.

**Indirect call specialization.** If an indirect call goes to the same target 90% of the time, emit
`if (fp == known) known(); else fp();`. The direct arm is then inlinable, which is the actual payoff:
value profiling's main product is not the branch, it is the inlining opportunity it creates.
`gimple_ic` (`gcc/value-prof.h:91`) performs it.

**Alignment discovery** through `HIST_TYPE_IOR`, feeding `memcpy` expansion and, where a vectorizer
exists, alignment decisions.

The workflow is stated at `gcc/value-prof.cc:69`: `gimple_find_values_to_profile` collects what to
profile, `instrument_values` inserts counters under `-fprofile-generate`, and under `-fprofile-use`
`compute_value_histograms` reads the data back and attaches histograms to statements, after which
`gimple_value_profile_transformations` drives the rewrites.

**The design point worth taking:** the histogram is attached to a statement and must be *maintained*
by every pass that moves, duplicates or deletes that statement. `gimple_duplicate_stmt_histograms`
and `gimple_move_stmt_histograms` exist for this, and `verify_histograms` checks it. This is the
same maintenance obligation as document 11.1's profile counts, one level finer, and it is a second
argument for the position document 11.5 already took: get the maintenance discipline into every pass
before the data arrives, not after.

## 35.7 Sampling and smoothing

`gcc/auto-profile.cc`, 4,905 lines, implements AutoFDO: instead of an instrumented build, take a
hardware sampling profile from `perf` on an ordinary optimized binary, map samples back to source
lines, and infer counts. It removes the instrumented build entirely, which is the reason it is used
in practice at scale, and it pays for that with a profile that is noisy, incomplete, and attached to
lines rather than edges.

Which is why `gcc/mcf.cc` exists. Its header cites Levin, Newman and Haber, "Complementing Missing
and Inaccurate Profiling Using a Minimum Cost Circulation Algorithm" (HiPEAC 2008), and Ramasamy,
Yuan, Chen and Hundt (GCC Summit 2008), and implements exactly that: a sampled profile does not
satisfy flow conservation, so the CFG is turned into a flow network and a minimum cost circulation is
computed to find the nearest set of counts that does. Six steps, listed at `gcc/mcf.cc:30`, ending in
`adjust_cfg_counts`.

**This is the correct way to think about an inaccurate profile and it should be recorded even though
rucc will not implement it for years.** The alternative, which is what naive implementations do, is
to use the sampled counts directly and let the inconsistencies propagate into every heuristic that
reads them. Document 11.1's profile quality field is the same instinct at a cruder level: know how
good the number is.

## 35.8 What rucc does, and in what order

**Neither in M4.** Both post-1.0. But the order between them is a decision worth making now, and the
recommendation is **PGO first, and by a wide margin.**

The reasons:

*PGO is per-translation-unit.* No linker plugin, no serialization format for types, no partitioner,
no build system integration. `-fprofile-generate` adds a pass that inserts counters; `-fprofile-use`
adds a pass that reads a file and sets `Frequency` values that document 11's machinery already
carries. That is perhaps 1,500 lines including the gcov file format, against LTO's 10,000-plus.

*PGO improves every heuristic at once.* Document 33's badness function, document 23's threading,
document 24's switch shape, document 29's unrolling, document 38's layout and document 39's spilling
all consult frequencies, and all of them are currently reading static guesses. Real counts improve
all of them without touching any of them.

*LTO's main product is cross-unit inlining, and there is a cheaper approximation.* rucc is a single
binary with no dependencies, so it can accept several source files on one command line and compile
them as one unit, which gives cross-unit inlining and whole-unit IPA with no serialization and no
linker involvement at all. That does not work for build systems that compile one file per process,
which is most of them, so it is not a replacement. But it makes the *analysis* work available for
testing and measurement long before the plumbing exists, which means the IPA passes of document 34
can be developed and validated at whole-program scope without LTO.

**What M4 owes to both**, and this is the actionable part:

- The three-phase IPA structure of document 34.7, so LTO does not require rewriting every IPA pass.
- Document 11's `Frequency` type with the quality field and the maintenance discipline in every pass,
  so PGO does not require auditing every pass.
- A per-statement side table keyed stably, so value profiling histograms have somewhere to live, and
  the same duplicate/move/delete maintenance obligation as counts.
- Accepting `-flto`, `-fprofile-generate` and `-fprofile-use` on the command line and either
  implementing them or diagnosing clearly that they are not implemented. Silently ignoring `-flto` is
  the worst option, because a build that thinks it has LTO and does not is a build whose measurements
  are wrong.

**And one deliverable that is worth doing early and independently: emit GCC-compatible `.gcno` and
`.gcda`,** per 35.5. It makes `gcov` and `lcov` work against rucc, which is a real compatibility win
for the coverage use case, and it is a self-contained file-format task testable against GCC's own
tools.

## 35.9 How this is wrong

**LTO merges two incompatible types.** The classic LTO miscompilation. Two units declare `struct S`
differently, both are technically ill-formed, both worked when compiled separately, and the merged
program reads the wrong offset. GCC warns with `-Wodr` in C++ and is largely silent in C. The honest
position is that LTO is stricter about program correctness than separate compilation, and it exposes
existing bugs rather than creating them, which is true and is no comfort to the user.

**Options are reconciled wrongly across units.** A unit compiled `-fno-strict-aliasing` inlined into
one compiled with strict aliasing. GCC tracks per-function optimization nodes for this and rucc must
too, which is the same requirement document 33.8 made for inlining.

**A symbol is resolved differently than the analysis assumed.** The linker plugin tells the compiler
each symbol's resolution: prevailing definition, preempted, or unused. Ignoring that resolution and
assuming the local body wins is document 34.8's body-trust bug at whole-program scale.

**The profile does not match the source.** `-Wcoverage-mismatch`. A stale `.gcda` after a source edit
gives counts attached to the wrong blocks, and the result is not a crash, it is a program optimized
for a control flow it does not have. The warning must default on, and rucc should additionally record
a hash of the CFG structure in the `.gcno` and refuse a mismatched `.gcda` rather than warning.

**Counters are lost to races.** `-fprofile-update=single` on a multithreaded training run. Counts
come out low and inconsistent, and the inconsistency propagates through every heuristic. rucc's
default should follow GCC's, and the documentation should say what the failure looks like.

**Untrained code is treated as cold.** `-fprofile-partial-training`. A function the training run
never entered is optimized for size and placed in the cold section. If the training run was
unrepresentative, the program is now slower in exactly the paths that were not measured, which is a
regression that PGO caused.

**The profile is used where a bound is needed.** Document 07.5's distinction between `Bound` and
`Estimate` applies with full force: a profile says a loop typically runs 100 times; it does not say
it runs 100 times. Unrolling on a profile-derived trip count is wrong code.

**Sampled counts violate flow conservation and are used anyway.** 35.7. Without the smoothing step,
the block counts and edge probabilities disagree, and document 11.5's verifier check will fire, which
is the correct outcome: the verifier catches it before a heuristic consumes it.

## 35.10 What it costs, and what to measure

LTO's compile-time cost is concentrated in WPA, which is single-threaded and holds the program in
memory; LTRANS parallelises. PGO's compile-time cost is negligible on the use side and roughly a
doubling on the generate side, plus the run-time cost of the instrumented program, which is where
users actually feel it.

Document 42 owes three numbers here, none of which needs either feature implemented.

- **The upper bound on PGO's value:** compare rucc `-O2` with rucc `-O2` given perfect frequencies
  derived from an instrumented run of a *different* compiler's build of the same program, mapped by
  source line. Crude, and it brackets the achievable gain before the feature exists.
- **The upper bound on cross-unit inlining's value:** compile the corpus as a single unit via the
  multi-file command line of 35.8 and compare against per-file compilation. This is LTO's main
  product measured without LTO, and it is available as soon as document 34's passes exist.
- **How often static prediction is wrong**, per document 11.2, measured against a real profile. This
  says which of document 11's heuristics are worth improving and whether PGO's benefit is broad or
  concentrated in a few decisions.
