# 30. Loop restructuring

The transformations that change a loop nest's shape rather than its contents: unswitching,
splitting, distribution, fusion, interchange, tiling, unroll-and-jam. They share three properties.
All of them need dependence analysis, document 31's, which nothing else in M4 needs. All of them are
`-O3` in GCC. And none of them is in M4.

That last point makes this document unusual: it is a survey of what is not being built, written so
that the decision is recorded with its reasoning rather than reached by omission, and so that the
person who builds them later starts from the right place.

The relevant GCC code totals 20,390 lines: `gcc/tree-loop-distribution.cc` (4,037),
`gcc/tree-predcom.cc` (3,598), `gcc/gimple-loop-interchange.cc` (2,125),
`gcc/tree-ssa-loop-split.cc` (1,860), `gcc/tree-ssa-loop-unswitch.cc` (1,681),
`gcc/gimple-loop-jam.cc` (697), and Graphite at 6,392 across seven files.

## 30.1 Unswitching, which is the exception

```c
for (i...) { if (c) A; else B; }   =>   if (c) for (i...) A; else for (i...) B;
```

when `c` is loop-invariant. **This one does not need dependence analysis**, which puts it in a
different class from everything else here. It needs only invariance, which document 27 computes
anyway.

`gcc/tree-ssa-loop-unswitch.cc`, 1,681 lines, bounded by `max-unswitch-insns` `Init(50)`
(`gcc/params.opt:825`). GCC 16 also unswitches on switch statements and on conditions that are
invariant only after considering ranges.

The value: a branch per iteration disappears, and each copy of the loop is simpler, which helps
vectorization and scheduling. The cost: the loop is duplicated, so the code doubles per unswitched
condition, and with `k` invariant conditions the growth is `2^k`, which is why there is a nesting
limit.

**rucc's position: unswitching is the one transformation in this document worth considering for
post-M4 but pre-1.0**, and it is worth roughly 300 lines given that document 26's loop versioning
machinery would exist. It is not in M4 because M4 has no versioning machinery: unswitching is
implemented as versioning the loop on the condition and then simplifying each copy, and building
versioning for one client is not justified.

The interaction noted in document 27.2 stands: GCC's LICM deliberately overprices conditionals to
create unswitching opportunities. If rucc never builds unswitching, that cost entry should be removed
from document 40's table rather than copied without its reason.

## 30.2 Loop splitting

`gcc/tree-ssa-loop-split.cc:48` shows the transformation:

```c
for (i = 0; i < 100; i++) { if (i < 50) A; else B; }
```

becomes

```c
for (i = 0; i < 50; i++) A;
for (; i < 100; i++) B;
```

This is unswitching for a condition that is not invariant but is *monotone in the induction
variable*: true for a prefix of the iteration space and false for the suffix. The split point is
computed from the condition, and `split_at_bb_p` at `gcc/tree-ssa-loop-split.cc:73` finds it by
requiring the condition to compare two affine induction variables, one with nonzero step.

The file implements a second kind, `split_loop_on_cond` at `gcc/tree-ssa-loop-split.cc:1716`, which
handles a condition that becomes invariant after some iteration.

It is implemented by versioning, per the comment at `gcc/tree-ssa-loop-split.cc:627`, which is the
same machinery unswitching needs.

**Value on C code: low.** The pattern requires a condition on the induction variable inside a loop
with a known iteration space, which appears in numerical code and rarely elsewhere. Recorded, not
planned.

## 30.3 Distribution and fusion

**Distribution** splits one loop into several, each containing part of the body, when the parts are
independent. Document 20.4 already noted its most valuable consequence: GCC recognises `memset` and
`memcpy` by distributing a loop and classifying the pieces, which finds idioms a whole-loop matcher
misses.

**Fusion** is the reverse: merge two adjacent loops over the same iteration space into one, saving
loop overhead and improving locality when both touch the same data.

Both require dependence analysis to know which statements may be separated or combined. GCC's
distribution is 4,037 lines; it has no separate fusion pass, fusion being available through Graphite.

**rucc: post-1.0, and the idiom half is approximated by document 20.4's whole-loop matcher.** The
measurement in document 20.8, counting `memset` and `memcpy` calls GCC generates that rucc does not,
is the number that would justify building distribution.

## 30.4 Interchange, tiling, and the polyhedral question

**Interchange** swaps two loops in a nest so the inner one strides contiguously through memory. On
`for (i) for (j) a[j][i] = 0;` interchanging gives sequential access instead of strided, which on a
large array is an order of magnitude.

`gcc/gimple-loop-interchange.cc`, 2,125 lines, bounded by `loop-interchange-max-num-stmts` `Init(64)`
(`gcc/params.opt:440`) and requiring a stride ratio of at least `loop-interchange-stride-ratio`
`Init(2)` (`gcc/params.opt:444`) to be considered profitable. `-O3` only.

**Tiling** restructures a nest to work on blocks that fit in cache. `loop-block-tile-size`
(`gcc/params.opt:436`) is its parameter and Graphite implements it.

**Unroll and jam** (`gcc/gimple-loop-jam.cc`, 697 lines) unrolls an outer loop and fuses the inner
copies, which is register-level tiling.

**Graphite** is GCC's polyhedral framework: 6,392 lines plus a dependency on the external ISL
library, representing loop nests as integer polyhedra and computing schedules by linear programming.
It subsumes interchange, tiling, fusion and distribution in one framework, and it is off by default
at every optimization level.

**That last fact is the decision.** GCC has had Graphite for eighteen years and does not enable it at
`-O3`, because the transformations it performs help a narrow class of dense numerical loop nests and
the analysis cost and bug surface are large. A compiler whose stated target is scalar integer and
pointer C code, per spec 00, has no business building it.

**And rucc has an additional constraint: no dependencies.** ISL is a large external library. Writing
a polyhedral scheduler from scratch is a multi-year project on its own.

**Decision, recorded: rucc does not build a polyhedral framework, at any milestone.** Interchange as a
standalone transformation, for the specific two-deep perfectly nested case with constant bounds, is
perhaps 400 lines on top of dependence analysis and could be justified after 1.0 if the corpus shows
loop nests that want it. The general framework is out of scope permanently, and saying so now is more
useful than leaving it as an open question.

## 30.5 Predictive commoning

`gcc/tree-predcom.cc`, 3,598 lines, deserves a mention because it is the one transformation in this
document that helps ordinary C.

```c
for (i = 1; i < n; i++) a[i] = a[i-1] + a[i];
```

`a[i-1]` in iteration `i` is `a[i]` from iteration `i-1`, already in a register. Predictive commoning
keeps a rotating set of registers across iterations and eliminates the reload. It is really redundant
load elimination across the back edge, which is exactly what document 16.2's within-iteration version
cannot do.

It requires dependence analysis to establish the distance between the two references, and it requires
unrolling by the reuse distance so the rotating registers can be named.

**Post-1.0, and worth more than most of this document** on the kinds of loops that appear in codecs,
filters and string processing. The measurement that would justify it: on the corpus, loops where the
same address is loaded in consecutive iterations.

## 30.6 What M4 actually gets from this area

Nothing directly, and two things indirectly.

**Document 20.4's whole-loop idiom matcher** gets the `memset` and `memcpy` cases without
distribution.

**Document 26's canonicalization** builds the block-splitting and edge-redirection utilities that
versioning would need, so when unswitching arrives it is not starting from nothing.

And there is a third thing worth stating: **the absence of these passes is measurable and should be
measured**. Document 42's comparison against `gcc -O3` on the loop-heavy part of the corpus prices
the whole area at once. If the gap is 2%, this document's decisions stand. If it is 20%, the corpus
contains numerical code that rucc's stated scope did not anticipate and the scope needs revisiting,
not the pass list.

## 30.7 How this would be wrong, when it is built

Recorded now because the failure modes are shared and are not obvious.

**A dependence is missed and statements are reordered.** Every transformation here reorders memory
accesses, and dependence analysis returning "independent" when it should not is a miscompilation with
no local symptom. Document 31 owns the analysis and owns the requirement that its default answer is
"dependent".

**Versioning's guard is wrong.** Unswitching, splitting and interchange under a guard all emit a
runtime test selecting between versions. A guard that admits the optimized version in a case it does
not handle is wrong code, and the guard is easy to get subtly wrong because it usually encodes an
overflow or aliasing condition.

**The two versions diverge.** After versioning, later passes optimize both copies, and a bug in one
copy is only reachable on one path. This makes testing harder in a specific way: coverage of the
optimized version requires inputs that satisfy the guard.

**Code size doubles per transformation and they compose.** Unswitching two conditions in a versioned
loop that was also split gives eight copies. Every one of GCC's parameters here is a defence against
this and they must be adopted along with the transformations.

**Loop-closed SSA and the loop forest.** Same as document 29.6, more so, since these transformations
create loops rather than merely duplicating bodies.

## 30.8 The single number that governs all of it

Document 42 should report, for the corpus: the fraction of total run time spent in loops that are
perfectly nested at depth two or more with affine subscripts. That is the population every
transformation in this document serves.

On the SPEC integer suites and on ordinary systems C, that fraction is in the low single digits. On
numerical code it approaches 100%. rucc's stated scope says the first, and the number should be
collected rather than assumed, because it is the one measurement that would overturn this entire
document's conclusions and it costs one instrumented run to obtain.
