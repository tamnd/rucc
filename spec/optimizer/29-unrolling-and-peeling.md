# 29. Unrolling and peeling

Duplicate the loop body `n` times so the loop overhead is amortised and the copies expose
optimization across iteration boundaries. Universally understood, universally overrated, and the
place where a compiler is most likely to make code slower while making every static metric look
better.

GCC has two implementations: `gcc/tree-ssa-loop-ivcanon.cc` (1,802 lines) does complete unrolling and
peeling at the tree level, and `gcc/loop-unroll.cc` (2,134) does partial unrolling at the RTL level.
The split is not arbitrary and 29.4 explains it.

## 29.1 The four transformations

They get confused constantly, so:

**Complete unrolling.** The trip count is a known small constant. Replace the loop with that many
copies of the body and no loop at all. `for (i = 0; i < 4; i++) a[i] = i;` becomes four stores. The
induction variable becomes a constant in each copy, so every address folds, and this is by far the
highest-value member of the family.

**Partial unrolling.** The trip count is unknown. Replace the body with `k` copies and iterate
`n/k` times, plus a prologue or epilogue handling `n mod k`. Amortises the loop overhead by a factor
of `k` and costs code size plus the remainder handling.

**Peeling.** Copy the first few iterations out of the loop. Useful when the first iteration differs,
typically because a condition inside the loop is true only then, or because alignment needs fixing
before vectorization.

**Unroll and jam.** Unroll an outer loop and fuse the resulting inner loops. A restructuring
transformation, document 30's, requiring dependence analysis. `-O3` only in GCC
(`gcc/opts.cc:706`).

## 29.2 Complete unrolling, which is the one that matters

`gcc/tree-ssa-loop-ivcanon.cc` performs it and the interesting part is the cost model, because the
naive one is badly wrong.

Unrolling a 10-instruction body 8 times looks like 80 instructions. But after unrolling, the
induction variable is a constant in every copy, so array indices fold to constants, conditions on the
counter fold away, and the actual result may be 20 instructions. A cost model that prices the
unrolled loop at `n × body` refuses transformations that would have shrunk the code.

GCC's `tree_estimate_loop_size` at `gcc/tree-ssa-loop-ivcanon.cc:265` therefore returns a structure
with several fields, not one number: the overall size, the part `eliminated_by_peeling`, the part
`not_eliminatable_after_peeling`, and separate figures for the last iteration.
`estimated_unrolled_size` at 458 combines them, and the arithmetic includes a two-thirds discount on
what survives, with a comment noting that the rounding is load-bearing for testcases.

**The lesson to carry over: the cost of unrolling must be estimated after the folding it enables, not
before.** rucc's version needs the same structure. For each instruction in the body, classify it as:
folds away when the induction variable is constant, survives, or survives only in the last iteration.
The classification is a walk of the body asking, for each instruction, whether its operands become
constant. That is a cheap approximation of running constant propagation and it is what GCC does.

The parameters: `max-completely-peel-times` `Init(16)` (`gcc/params.opt:549`),
`max-completely-peeled-insns` `Init(200)` (`gcc/params.opt:553`),
`max-completely-peel-loop-nest-depth` `Init(8)` (`gcc/params.opt:545`).

**rucc builds complete unrolling at `-O2`,** with those limits, and it is one of the higher-value
loop passes because loops with small constant trip counts are extremely common in C: initialising a
fixed-size array, iterating over a struct's fields, small fixed-size matrix code.

It needs an exact trip count, which per document 07.5 means a `Bound` and not an `Estimate`. Using an
estimate here would unroll the wrong number of times and produce wrong code, which is the single
strongest justification for making those distinct types.

## 29.3 The canonical induction variable, and a pass that exists for another pass

`gcc/tree-ssa-loop-ivcanon.cc:20` describes its primary job, which is not unrolling:

> This pass detects the loops that iterate a constant number of times, adds a canonical induction
> variable (step -1, tested against 0) and replaces the exit test. This enables the less powerful rtl
> level analysis to use this information.

And then admits: "This might spoil the code in some cases (by increasing register pressure). Note
that in the case the new variable is not needed, ivopts will get rid of it."

So a pass adds a variable it knows may be harmful, relying on a later pass to remove it if unused,
because the information cannot otherwise cross the tree-to-RTL boundary.

**rucc does not need this and should not build it.** There is one IR, the trip count analysis's
result can simply be attached to the loop and carried into the machine level, and no canonical
variable needs materialising. Document 28.4's countdown rewrite is a *candidate* in the ivopts
selection, considered on its merits, not an unconditional insertion.

This is a clean example of a GCC pass that exists because of a representational boundary rucc does
not have, and it is worth putting alongside document 15.1's copy propagation in the tally.

## 29.4 Partial unrolling, and why GCC does it at RTL

`gcc/loop-unroll.cc` does partial unrolling after register allocation decisions are in view, and the
reason is register pressure: unrolling `k` times multiplies the number of simultaneously live values
in the body by up to `k`, and whether that spills is a question only the back end can answer.

There is a second reason, which is that partial unrolling's benefit comes from instruction-level
parallelism: four copies of a body give the scheduler four independent chains to interleave. That
benefit is realised by the scheduler, so unrolling immediately before scheduling lets the two
cooperate, and unrolling in the middle end means every intervening pass processes four times the
code for no benefit.

Both arguments say: **unroll late**. rucc's partial unrolling belongs at the machine level, in the
same neighbourhood as document 38's scheduling, and it is post-M4.

The parameters when it is built: `max-unroll-times` `Init(8)` (`gcc/params.opt:817`),
`max-unrolled-insns` `Init(200)` (`gcc/params.opt:821`), `max-average-unrolled-insns` `Init(80)`
(`gcc/params.opt:533`).

**And it is off by default in GCC at every level.** `-funroll-loops` is not implied by `-O2` or
`-O3`; only `-fpeel-loops` and `-floop-unroll-and-jam` are, at `-O3` (`gcc/opts.cc:706`). This is
worth stating loudly because the folk belief is the opposite. GCC does not partially unroll unless
asked, and it does not because measurement did not support it: the code growth costs instruction
cache, the ILP benefit is already captured by out-of-order execution on modern machines, and the
remainder loop costs branches.

**So rucc's default is the same: no partial unrolling at `-O2`, `-funroll-loops` available and
implemented post-M4.** That is a decision that saves a large amount of work and it is defensible by
pointing at what GCC actually does rather than at what people think it does.

## 29.5 Peeling

`try_peel_loop` at `gcc/tree-ssa-loop-ivcanon.cc:1115`, `-O3` only, bounded by
`max-peel-times` `Init(16)` (`gcc/params.opt:709`), `max-peeled-insns` `Init(100)`
(`gcc/params.opt:713`) and `max-peel-branches` `Init(32)` (`gcc/params.opt:705`).

The useful case is when the profile or the estimate says the loop typically runs a small number of
times: peel that many iterations and the loop usually never executes. The other useful case is
alignment peeling for vectorization, which is document 32's and comes with its own parameter,
`vect-max-peeling-for-alignment` (`gcc/params.opt:1274`).

**Not in M4.** Neither driver is available: rucc has no profile data in M4 per document 11.5, and no
vectorizer. Recorded for when either arrives.

There is one peeling case that is worth M4 and is not really peeling: a loop whose first iteration is
distinguished by a condition on the induction variable, `if (i == 0)`. Complete unrolling handles it
when the trip count is small; otherwise the condition is invariant-per-iteration and it is really an
unswitching opportunity, which is document 30's.

## 29.6 What unrolling breaks

Every duplicating transformation has the same list and unrolling is the most aggressive of them.

**Loop-closed SSA.** Values defined in the body now have `k` definitions. Document 26.4 established
that LCSSA makes this manageable: the exit parameter's argument comes from the last copy. Getting it
right is the transformation's main bookkeeping.

**The loop forest.** Complete unrolling deletes a loop, which changes the forest; partial unrolling
keeps it but changes the latch. Rebuild, per document 26.8.

**Profile counts.** The loop's count must be divided among the copies, and the epilogue loop's count
is the remainder's probability. Document 11.1's quality tracking will mark the result degraded, which
is honest.

**Debug information.** `k` copies of the body means a source line maps to `k` addresses. That is
normal and DWARF handles it; what it does to a debugger's stepping is the reason `-Og` does not
unroll.

**Code size, and this is the real one.** A completely unrolled loop is unambiguously larger unless the
folding pays for it, and at `-Os` and `-Oz` the limits shrink sharply. GCC's
`max-completely-peeled-insns` is scaled at `-Os`; rucc should do the same, and at `-Oz` complete
unrolling is off entirely except when the unrolled form is provably no larger, which happens for
trip counts of two or three.

## 29.7 How this is wrong

**The trip count is an estimate and unrolling uses it as exact.** Document 07.5's type distinction is
the defence and this is the pass it was designed for. An estimate-driven complete unroll produces a
loop body executed the wrong number of times, which is arbitrary wrong code.

**The trip count is exact but the loop has a second exit.** A `break` inside the body means the loop
may run fewer iterations, and each unrolled copy must retain the exit test. Complete unrolling of a
multi-exit loop is legal only if every copy keeps its exits, which is fine, and the last copy's
back edge is what disappears. GCC has a specific analysis at `gcc/tree-ssa-loop-ivcanon.cc:478` for
finding an edge that can be removed to make the loop always exit.

**Off by one in the number of copies.** A loop from 0 to `n` inclusive runs `n+1` times. This is the
most common bug in unrolling and the defence is that trip count is defined once, in document 07.5,
as the number of times the latch is taken, and every consumer uses that definition rather than
recomputing it.

**The epilogue is wrong.** Partial unrolling's `n mod k` handling. Not in M4, and when it is built,
the epilogue should be generated by peeling from the same code path rather than written separately.

**Unrolling an infinite loop.** A loop whose trip count analysis returned nothing is not unrolled.
Complete unrolling requires a bound; there is no "unroll a bit and hope" path.

**Register pressure explodes.** 29.4's argument for unrolling late. At `-O2` with complete unrolling
only, the risk is bounded by the 200-instruction limit and by the fact that a fully unrolled loop has
no loop-carried values.

**The unrolled body is not in a form ivopts recognises.** Document 28.6 requires the copies' addresses
to differ by constant offsets from one base, so that ivopts groups them. An unroller that recomputes
each copy's address from scratch produces `k` independent induction variables and ivopts's grouping
fails. The copies should share the original base with a constant added, which is what document 19.6's
normalisation produces anyway.

## 29.8 What it costs

Complete unrolling costs the size estimate, which is one walk of the body, plus the duplication
itself, which is proportional to the output. Bounded by the 200-instruction limit, so bounded per
loop.

The cost that is not local: everything downstream sees the unrolled code. A function with ten loops
each unrolled to 200 instructions is 2,000 instructions where it was 100, and every subsequent pass
pays. This is the same compounding argument as document 23.8's for jump threading, and it is why the
limits are as tight as they are.

The measurement in document 42: code size and run time on the corpus with complete unrolling on and
off; and separately, a check that `gcc -O2` and rucc unroll approximately the same set of loops,
since the trip counts are objective and a large divergence means the trip count analysis differs
rather than the heuristic.

And one negative measurement worth taking early: build `-funroll-loops` support and confirm on the
corpus that it does not help at `-O2`. If it does help, GCC's default is wrong for rucc's code
generator and that is a finding worth having. If it does not, the decision in 29.4 is confirmed by
rucc's own numbers rather than by GCC's.
