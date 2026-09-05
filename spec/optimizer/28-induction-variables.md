# 28. Induction variables

A loop's addressing is arithmetic on the loop counter, and that arithmetic is what the loop actually
spends its time on. `a[i]` is `a + i*4`, computed fresh each iteration, when it could be a pointer
incremented by 4. Choosing what the loop increments, and how the uses are expressed in terms of it,
is induction variable optimization, and it is the single largest determinant of inner loop quality
on scalar code.

`gcc/tree-ssa-loop-ivopts.cc` is 8,261 lines, the second largest file in the tree middle end after
value numbering. `gcc/gimple-ssa-strength-reduction.cc` adds 4,162 for the cases ivopts leaves.

## 28.1 What GCC does, from its own summary

`gcc/tree-ssa-loop-ivopts.cc:20` describes four steps.

**One: find the interesting uses.** Three kinds: uses of induction variables in non-linear
expressions, addresses of arrays, and comparisons of induction variables. Uses are grouped, and
specifically "address type uses are grouped together if their iv bases are different in constant
offset", which is the recognition that `a[i]`, `a[i+1]` and `a[i+2]` should share one induction
variable.

**Two: find candidates.** The existing induction variables, plus new ones derived from the uses.

**Three: choose the optimal set** by a cost function with three parts:

- *Group and use costs.* Each use picks the best candidate and adds the cost of adapting it, "adding
  base and offset for arrays, etc."
- *Variable costs.* Each candidate costs something to increment each iteration. "The original
  variables are somewhat preferred," which is a bias toward not changing things.
- *Set cost.* "Depending on the size of the set, extra cost may be added to reflect register
  pressure."

And then: "All the costs are defined in a machine-specific way, using the target hooks and machine
descriptions to determine them."

**Four: rewrite** the uses in terms of the chosen set and let dead code elimination remove the rest.

The header adds, at `gcc/tree-ssa-loop-ivopts.cc:64`: "All of this is done loop by loop. Doing it
globally is theoretically possible, it might give a better performance... but getting all the
interactions right would be complicated."

## 28.2 Why this is a set-selection problem and not a rewriting problem

The framing is what makes ivopts hard and it is worth being precise about, because the naive
implementation is a strength reducer that replaces each multiply as it finds it, and that
implementation produces worse code than doing nothing on real loops.

Consider `for (i = 0; i < n; i++) sum += a[i] * b[i];`. The uses are `a + i*4`, `b + i*4`, and the
comparison `i < n`. Options:

- Keep `i`, compute both addresses. One increment, two multiplies (or two scaled-index addressing
  modes if the target has them, in which case this is optimal).
- Two pointers `pa`, `pb`, incremented by 4 each, compare `pa` against `a + n*4`. Two increments, no
  multiplies, no separate counter. Better on a target without scaled indexing.
- Two pointers plus `i`. Three increments. Worse than both.

Which is best depends entirely on the target's addressing modes, and the difference is roughly 30% of
the loop's instruction count. **There is no target-independent right answer**, which is why GCC's
costs come from the machine description and why this pass, uniquely in the tree middle end, is deeply
target-aware.

The optimization is a set cover: choose a set of candidates minimising total cost, where each use is
served by its cheapest available candidate. That is NP-hard in general, so GCC does greedy search
with pruning, bounded by three parameters: `iv-max-considered-uses` `Init(250)`
(`gcc/params.opt:364`), `iv-consider-all-candidates-bound` `Init(40)` (`gcc/params.opt:360`) below
which the search is exhaustive, and `iv-always-prune-cand-set-bound` `Init(10)`
(`gcc/params.opt:356`).

## 28.3 What rucc builds

Document 07.4 already commits to affine chains of recurrences plus pointer chrecs, which is the
analysis half: for each value in the loop, is it of the form `base + i*step`, and what are `base` and
`step`. That analysis is a prerequisite and it exists for trip counts anyway.

**On top of it, M4 builds a restricted ivopts of perhaps 800 lines.**

*Use collection.* Addresses, comparisons, and other uses, grouped by base modulo a constant offset,
exactly as GCC does. The grouping is the cheapest large win in the pass.

*Candidate generation.* The existing induction variables, plus one candidate per distinct
(base, step) among the address uses, plus the "final value" candidate for the loop's exit comparison.

*Selection.* Greedy: start with the set of original induction variables, repeatedly consider adding
the candidate that most reduces total cost, stop when nothing improves. Bounded by GCC's three
parameters. Not exhaustive even below the 40-candidate bound, because the exhaustive search's value
over greedy has never been published and rucc can measure it later.

*Costs from document 40, target-parameterised.* This is the place where document 40's cost model must
be genuinely target-aware rather than a table of nominal instruction counts, and it needs three
things per target: the legal addressing mode forms, the cost of an address computation not expressible
as an addressing mode, and the number of allocatable registers for the set-size penalty.

*Rewriting*, then document 17's DCE removes the now-dead original variables. The pass does not delete
anything itself, which keeps it simpler and follows the general discipline of one job per pass.

## 28.4 Linear function test replacement

The sub-transformation worth naming separately, because it is where the correctness risk is.

`for (i = 0; i < n; i++) *p++ = 0;` has two induction variables, `i` and `p`, and only `p` is used in
the body. Rewriting the exit test from `i < n` to `p < limit` where `limit = p0 + n` removes `i`
entirely.

**The trap: the rewritten comparison must be equivalent, including at the boundaries.** `p < p0 + n`
where the multiplication `n * sizeof(*p)` overflows the pointer type is not equivalent to `i < n`.
And a signed counter rewritten as an unsigned pointer comparison changes behaviour when the original
would have wrapped.

GCC handles this by requiring the new comparison to be provably equivalent given the trip count
analysis, and the trip count analysis's `assumptions` field (document 07.5) is where the conditions
land. rucc does the same: the rewrite is performed only when the trip count is a `Bound` rather than
an `Estimate`, and when the derived limit provably does not overflow.

The second half of the same transformation is the **countdown form**: rewriting `i = 0; i < n; i++`
as `j = n; j != 0; j--`, so the exit test is a comparison against zero, which most targets get for
free from the decrement's flags. GCC's ivopts does this as part of doloop support
(`gcc/tree-ssa-loop-ivopts.cc:70`), adding a dedicated candidate
`{(may_be_zero ? 1 : (niter + 1)), +, -1}` for targets with hardware loop instructions.

rucc's targets are x86-64, AArch64 and RISC-V64, none of which have hardware loop counters, but all
of which have cheap compare-against-zero. So the countdown rewrite is worth doing for the flags, not
for a loop instruction, and it should be a candidate in the selection rather than an unconditional
rewrite, because it is only profitable when nothing else needs `i`.

## 28.5 Straight-line strength reduction

`gcc/gimple-ssa-strength-reduction.cc:20` opens with the best one-line description of a pass's scope
in the source tree:

> There are many algorithms for performing strength reduction on loops. This is not one of them.
> IVOPTS handles strength reduction of induction variables just fine. This pass is intended to pick
> up the crumbs it leaves behind, by considering opportunities for strength reduction along dominator
> paths.

The case: `a = b * 4; ... c = (b + 1) * 4;` becomes `c = a + 4`. Not a loop, so ivopts never sees it,
and a multiply becomes an add. It also handles multiplies implicit in addressing.

The header notes the restrictions: integer only, and division and modulo are not attempted because
"such opportunities are relatively uncommon."

**Not in M4.** The reasoning: document 12's e-graph plus document 19's reassociation already
canonicalize `(b+1)*4` into `b*4 + 4`, and hash-consing then recognises `b*4` as the existing `a`.
That is the same result reached by a mechanism that exists for other reasons. Whether it actually
happens is measurable, and document 42 should check specifically for it, because if the e-graph does
not produce this then a 4,162-line GCC pass is doing something rucc has no answer for.

This is one of the cleaner tests of the document 12 thesis and it is worth calling out as such.

## 28.6 The relationship to the rest of the loop pipeline

**Ivopts runs last** among the loop passes that operate on a single loop, after LICM and before or
after unrolling depending on a judgment call.

Before unrolling: the unroller duplicates a body already expressed in good form, and each copy needs
its offsets adjusted, which is easy since they are constant offsets from one base.

After unrolling: ivopts sees the unrolled body's four uses at offsets 0, 4, 8, 12 and groups them,
which is exactly what its constant-offset grouping is for, and picks one candidate for all four.

**GCC runs ivopts after unrolling** and rucc should do the same, for the grouping reason. That means
the unroller must produce addresses in a form ivopts recognises, which is document 29's obligation.

**Ivopts and vectorization conflict.** The vectorizer wants to see the original array references,
`a[i]`, not a pointer rewritten by ivopts. GCC's answer is that the vectorizer runs before ivopts.
rucc's vectorization is post-M4, and the ordering constraint should be recorded now: ivopts is the
last thing in the loop pipeline, and anything that wants to pattern-match array accesses runs before
it.

**And there is a tension with document 19's pointer arithmetic normalisation.** Document 19.6
normalises to `base + i*scale + C`. Ivopts then rewrites that into an incremented pointer. The two
are not in conflict, because 19's form is what ivopts consumes, but it does mean that after ivopts
the pointer arithmetic is no longer in 19's canonical form and any pass running afterwards must not
assume it is. Document 37's addressing mode selection consumes what ivopts produces.

## 28.7 How this is wrong

**The rewritten exit condition is not equivalent.** 28.4. This is the highest-severity bug in the
document: an off-by-one in the exit test writes one element past the array, and it depends on the
trip count, so it fires on some inputs and not others.

**The derived limit overflows.** `p0 + n*4` where `n` is large. In C, pointer arithmetic past
one-past-the-end is undefined, so the compiler may assume it does not overflow, and then computes a
limit that wraps and a loop that never terminates. The assumption is legal and the consequence is a
hang. rucc's rule: the limit computation must be within the object's bounds by the same reasoning
that made the original loop valid, and where that cannot be shown, the rewrite is not made.

**Signedness changes in the rewritten comparison.** A signed `i < n` becoming an unsigned pointer
comparison is a different test when the original could have been negative. Header copying and range
analysis usually establish that `i >= 0`; where they do not, the rewrite waits.

**Too many induction variables are created and the loop spills.** The set cost. Same failure as
document 27.2's and it is more likely here, because ivopts's whole job is to create variables.

**The chosen set is worse than the original.** Possible whenever the cost model is wrong about the
target, and it is why GCC's cost function "somewhat prefers" the original variables. rucc should
have the same bias, and it should be a real number in document 40 rather than a tiebreak, because on
a target whose costs have not been tuned the original variables are the safer default.

**A candidate is derived from a chrec that is not actually affine.** Document 07.4's analysis must
say `Affine` or `Unknown`, never guess. A use whose evolution is not affine is not an induction
variable use and belongs in the "other" category, rewritten in terms of nothing.

**The pass runs on an irreducible region.** No loop, no preheader, no trip count. Skipped, per
document 26.2.

## 28.8 What it costs

Use collection is one walk of the loop body. Candidate generation is linear in uses. Selection is the
expensive part: greedy selection with a cost evaluation per (use, candidate) pair is
`O(uses × candidates)` per step and `O(candidates)` steps, so cubic in the worst case, which is why
all three of GCC's parameters exist. With uses capped at 250 and candidates at 40 that is bounded but
not small, and ivopts is one of the more visible passes in a GCC `-ftime-report`.

rucc's greedy version with the same caps has the same shape. The cost evaluation must be cheap, which
means the addressing-mode legality query in document 40 must be a table lookup and not a call into
instruction selection.

The measurement in document 42, and there are three worth having:

- Inner loop instruction counts against `gcc -O2` on the loop-heavy corpus. This pass is where that
  comparison is won or lost.
- How often the greedy selection differs from the original variable set, which tells whether the pass
  is doing anything.
- The straight-line strength reduction check from 28.5, testing the document 12 thesis directly.
