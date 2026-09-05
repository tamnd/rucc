# 07. Loops, and the evolution of values inside them

This is the analysis that unlocks documents 26 through 32, and it is also the analysis whose GCC
implementation is most out of proportion to its apparent complexity. Finding the loops is three
hundred lines. Knowing how many times one runs is 5,734 lines
(`gcc/tree-ssa-loop-niter.cc`). Knowing how a value changes across iterations is 4,066 more
(`gcc/tree-scalar-evolution.cc`) plus 1,902 for the algebra it operates on
(`gcc/tree-chrec.cc`). Add induction variable optimization at 8,261 lines
(`gcc/tree-ssa-loop-ivopts.cc`) and loop manipulation at 1,479, and GCC's loop infrastructure is
about 23,600 lines before any actual loop transformation happens.

rucc will not write 23,600 lines here and does not need to. But the reason GCC's is that large is
instructive and most of it is not accidental, so this document is careful to say which parts are
essential, which are earning their keep for GCC and not for us, and which are there because
somebody needed a benchmark to vectorize.

## 7.1 Finding loops

`flow_loops_find` at `gcc/cfgloop.cc:416` is the textbook construction: a back edge is an edge
whose head dominates its tail, the natural loop of a back edge is the set of blocks that reach the
tail without passing through the head, and loops nest because natural loops with different headers
are either disjoint or one contains the other. This needs the dominator tree from document 06 and
nothing else.

Two complications are real and rucc will hit both.

**Multiple latches.** Two back edges to the same header. GCC's `find_subloop_latch_edge` at
`gcc/cfgloop.cc:690` tries to work out whether one of them is really an inner loop's latch, using
the profile if there is one (`gcc/cfgloop.cc:598`) and induction variables if there is not
(`gcc/cfgloop.cc:638`), and `disambiguate_loops_with_multiple_latches` at `gcc/cfgloop.cc:829`
applies the answer. rucc should not do this. rucc should require a single latch as a
canonicalization (7.3) and let the canonicalizer create one, which is a CFG edit rather than a
guess and is always right.

**Irreducible regions.** A loop with two entries has no header, so it is not a natural loop of
anything. GCC marks the blocks and edges involved (`LOOPS_HAVE_MARKED_IRREDUCIBLE_REGIONS`,
`gcc/cfgloop.h:311`) and every loop pass then avoids them. rucc does exactly the same and document
06.4 already committed to not transforming irreducible regions into reducible ones. The analysis
reports a set of blocks that are in a cycle but not in any natural loop, and every loop pass
checks it.

## 7.2 What GCC's loop structure guarantees

`gcc/cfgloop.h:309` lists five flags a consumer can require, and `LOOPS_NORMAL` at
`gcc/cfgloop.h:319` bundles the three that matter: preheaders, simple latches, and marked
irreducible regions. `gcc/tree-ssa-loop.cc:357` shows what the loop pass group actually asks for:

```c
loop_optimizer_init (LOOPS_NORMAL | LOOPS_HAVE_RECORDED_EXITS);
```

That is the real specification of loop canonical form and rucc should adopt it nearly verbatim.
The recorded exits are a cached list of edges leaving the loop, which matters because
`get_loop_exit_edges` at `gcc/cfgloop.cc:1196` is otherwise a walk of the loop body and every
loop pass wants it.

`verify_loop_structure` at `gcc/cfgloop.cc:1392` checks the whole thing and is enabled under
checking builds. rucc needs the equivalent and document 04.3's lying-pass check is not a
substitute for it: that check catches a pass that says it preserved the loop forest when it did
not, and this one catches a pass that legitimately rebuilt the forest into something malformed.

## 7.3 The canonical form rucc requires

Four properties, checked by a verifier, established by the canonicalization pass in document 26.

**A preheader.** Exactly one non-latch predecessor of the header, and it is the only one. This is
where LICM puts things and without it every hoist has to split an edge first.

**A single latch.** Exactly one back edge, and the latch block's only successor is the header.
This is what makes "the last thing that happens in an iteration" a well-defined place.

**Loop-closed SSA.** Every value defined inside the loop and used outside it is used through a
block parameter of a block outside the loop. With block parameters rather than phi nodes this is
easy to state and easy to check, and it is what makes deleting or duplicating a loop a local edit
instead of a global one. GCC maintains this as LCSSA and it is one of the pieces of loop
infrastructure that is genuinely load-bearing rather than incidental.

**A dedicated exit.** Every exit edge goes to a block whose only predecessors are inside the loop.
This is what lets a transformation put code on the way out.

The cost of canonicalization is blocks: a preheader per loop, a latch per loop that needed one,
an exit block per exit that was shared. Most of them are empty and CFG simplification removes them
afterwards, which means the canonicalizer and the simplifier fight, and the resolution is that
canonical form is established once before the loop pipeline and destroyed once after it, exactly
as GCC does with `loop_optimizer_init` and `loop_optimizer_finalize`
(`gcc/tree-ssa-loop.cc:483`). Document 03.4's `-O2` list is arranged so this happens once.

## 7.4 Scalar evolution, and how much of it to build

GCC represents how a value changes across iterations as a **chain of recurrences**, cited in the
file header at `gcc/tree-scalar-evolution.cc:32` to Zima and Van Engelen. The notation
`{base, +, step}_loop` means a value that is `base` on the first iteration and increases by `step`
each time, and the representation composes: a chrec's base or step can itself be a chrec of an
outer loop, giving nested affine functions, and `{a, +, b, +, c}` gives polynomials.

This is a genuinely good representation and its power is that it is *closed under the operations
you want*. Adding two chrecs of the same loop adds componentwise. Multiplying gives a
higher-degree chrec. Applying a chrec at iteration `n` (`chrec_apply`) evaluates it. That
closure is why GCC can answer "what is this value on the last iteration" without any special
casing, and it is why the alternative, pattern-matching `i = i + 1` in a block parameter, runs out
of road the first time somebody writes `j = 2 * i + 3`.

**What rucc builds: affine chrecs only, one level of nesting at a time.** A value is either
loop-invariant, or `{base, +, step}` for this loop with `base` and `step` invariant in it, or
unknown. Addition, subtraction, multiplication by an invariant, and sign or zero extension where
the extension provably does not wrap. Nothing polynomial, no `chrec_apply` on symbolic exponents,
no mutual recursion between two evolving values.

That subset covers every induction variable a C programmer writes, every array subscript that
dependence analysis in document 31 can do anything with, and every trip count that is not a
`while` loop over a linked list. The parts of GCC's 4,066 lines that it omits are the parts
serving Fortran and the polyhedral framework, and rucc has neither.

The one deliberate extension beyond affine is **pointer chrecs**, because C loops walk pointers
rather than indices and `p = p + 1` is the same thing as `i = i + 1` with a scale factor. Treating
`ptr_add` as addition with the element size as the step is three lines and is the difference
between analysing half of real C loops and analysing almost all of them.

## 7.5 Trip counts, and the uncomfortable part

`number_of_iterations_exit` at `gcc/tree-ssa-loop-niter.cc:3368` is the entry point and
`number_of_iterations_cond` at `gcc/tree-ssa-loop-niter.cc:1794` does the work: given a condition
comparing an affine chrec against something invariant, solve for the iteration at which it first
fails. For `i < n` with `i = {0, +, 1}` the answer is `n`, subject to `n >= 0` and subject to `i`
not wrapping.

Those two conditions are the whole difficulty and they generalise into an *assumptions* mechanism.
`number_of_iterations_exit_assumptions` at `gcc/tree-ssa-loop-niter.cc:3217` returns not a trip
count but a trip count plus a predicate under which it holds, and the consumer either proves the
predicate, emits a runtime check for it, or gives up. rucc must have this. A trip count returned
without its assumptions is a miscompilation generator, and the temptation to return one is strong
because the assumptions are usually true.

**The undefined-behaviour question.** `infer_loop_bounds_from_undefined` at
`gcc/tree-ssa-loop-niter.cc:4553` derives loop bounds from the premise that the program has no
undefined behaviour: if the body does `a[i]` and `a` has 100 elements, the loop runs at most 100
times, because more would be UB. `infer_loop_bounds_from_signedness` at
`gcc/tree-ssa-loop-niter.cc:4499` does the same from signed overflow being undefined, which is
what makes `for (int i = 0; i <= n; i++)` finite.

This is standard-conforming and it is also the single most common source of "the compiler broke my
program" reports. rucc should do it, should do it only for signed overflow and array bounds and
pointer arithmetic (the three GCC infers from), should gate the whole thing on `-fstrict-overflow`
being in effect so `-fwrapv` turns it off, and should make every such inference dumpable under
`-fdump-loop-assumptions` naming the source line whose UB justified it. Spec 9.10's dump
philosophy applies with force here: a user who has been bitten by this deserves a command that
tells them exactly which line the compiler used against them.

Note that `estimate_numbers_of_iterations` at `gcc/tree-ssa-loop-niter.cc:4955` produces an
*estimate* used for cost decisions, distinct from `max_loop_iterations` at
`gcc/tree-ssa-loop-niter.cc:5102` which produces a bound used for correctness. Conflating the two
is a category error that costs correctness, and rucc's types should make it impossible: an
`Estimate` and a `Bound` are different structs even though both wrap an integer.

`loop_niter_by_eval` at `gcc/tree-ssa-loop-niter.cc:3659` brute-force-simulates a loop with a
small constant trip count. `number_of_iterations_popcount` at `gcc/tree-ssa-loop-niter.cc:2092`
recognises `while (x) { x &= x - 1; n++; }` and answers `popcount(x)`. Both are cute. Neither is
in M4; the popcount one belongs in document 20's idiom recognition, where it is a rewrite and not
a trip count.

## 7.6 The interface

Four questions, and the analysis answers exactly these.

| Question | Returns |
|---|---|
| What loops are there, nested how | the forest, plus the irreducible block set |
| Is this value invariant in this loop | yes, or no |
| How does this value evolve in this loop | invariant, `{base, +, step}`, or unknown |
| How many times does this loop run | unknown, or a bound plus assumptions, or a constant |

Everything a loop pass wants is one of these four. Resisting the fifth is the discipline that
keeps this from becoming 23,600 lines: when document 29's unroller wants to know something not on
this list, the answer is that it computes it itself from these four, or the list grows by an entry
that has its own tests and its own section here.

## 7.7 How this is wrong

**The step is not what it looks like.** `i += k` where `k` is invariant but zero gives a chrec
with step zero, which is a valid affine chrec describing a loop that never terminates through this
exit. Code that divides by the step to get a trip count divides by zero. The analysis must return
the chrec and the trip count computation must handle a zero step, and there must be a test.

**Wrapping.** `{0, +, 1}` in `unsigned char` is not `0, 1, 2, ...`; it is that sequence modulo 256.
Every chrec carries the type it evolves in and every operation on chrecs checks that the result
cannot wrap or degrades to unknown. This is where a naive implementation is wrong constantly and
in ways that pass every test written by the person who wrote the analysis, because they think in
`int`. The test suite needs chrecs in `unsigned char` and `short`.

**Assumptions get dropped.** The trip count is correct under three assumptions, a caller uses two
of them, and the third silently does not hold. The defence is that the type carrying a trip count
carries its assumptions inseparably and there is no accessor that returns the count alone.

**Staleness.** SCEV caches per value per loop (GCC keeps a hash table, `gcc/tree-scalar-
evolution.cc:300`), and a pass that rewrites an induction variable invalidates every cached chrec
in that loop. Per document 04.4, scalar evolution is invalidated by loop changes and IV rewrites,
and the honest default for any pass that touches a loop is `Preserved::None`.

## 7.8 What it costs

Building the loop forest is a dominator query per edge plus a walk per loop body, so linear in the
CFG with a small constant. It is cheap and it is not the concern.

SCEV is the concern, because it is demand-driven and memoized and its cost is therefore a function
of how many distinct values get queried, which is a function of how greedy the loop passes are.
GCC's mitigation is that the analysis is per loop and the cache is discarded with the loop.

The measurement in document 42 reports SCEV time separately from loop pass time, because if the
loop pipeline gets expensive the first question is whether the analysis or the transformations are
responsible, and a single combined number cannot answer it.
