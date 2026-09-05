# 26. Loop canonicalization

Document 07.3 named four canonical properties that every loop must have before the loop pipeline
runs: a preheader, a single latch, loop-closed SSA, and dedicated exits. This document says how they
are established, what they cost, and why the alternative of handling non-canonical loops in each pass
is worse than it looks.

The relevant GCC code is `gcc/cfgloopmanip.cc` (1,990 lines), `gcc/tree-ssa-loop-manip.cc` (1,479)
and `gcc/tree-ssa-loop-ch.cc` (1,291). The first two establish the properties; the third performs
loop header copying, which is a fifth canonicalization and the one that changes the most code.

## 26.1 Why canonicalize at all

Every loop transformation needs somewhere to put code. LICM needs a block that executes exactly once
before the loop and dominates the header. Unrolling needs to place a prologue. Vectorization needs to
place a guard and a scalar epilogue. Induction variable optimization needs to place initialisers.

Without a preheader, each of those passes creates one, and each does it slightly differently, and
each has to handle the case where the header has several outside predecessors. With a preheader, each
of them writes `insert at end of preheader` and stops thinking about it.

The same argument applies to the other three properties. A single latch means "the back edge" is
well defined. Loop-closed SSA means a use outside the loop goes through a block parameter at the exit,
so changing what the loop computes touches one place. Dedicated exits mean an exit block belongs to
one loop, so a transformation can rewrite it without affecting a sibling.

The cost is a handful of empty blocks, which document 21 removes after the loop pipeline finishes.
That is the entire trade and it is obviously worth making.

## 26.2 Preheaders

`create_preheader` at `gcc/cfgloopmanip.cc:1676`, with three levels of strictness selected by flags:

- Default: the loop has a single entry edge from outside.
- `CP_SIMPLE_PREHEADERS`: additionally, the preheader block has only one successor.
- `CP_FALLTHRU_PREHEADERS`: additionally, the preheader falls through to the header and has
  predecessors only from outside the loop.

The construction is: collect the header's predecessors that are not the latch, and if there is more
than one, or the single one has other successors, insert a block and redirect them all through it.
The function also updates dominators, which is the part that is easy to forget and which document
06.5 requires.

**rucc uses the equivalent of `CP_SIMPLE_PREHEADERS`.** The fallthru variant matters for RTL block
layout and rucc's layout is document 38's, done at the machine level over a CFG that no longer has a
loop forest attached. Requiring fallthru here would constrain layout for no benefit.

One detail carries over from `gcc/cfgloopmanip.cc:1682`: whether the loop is irreducible is tracked
while building the preheader, because inserting a block must not lose the `EDGE_IRREDUCIBLE_LOOP`
marking. Document 06.4 established that rucc marks irreducible regions and refuses to optimize them,
and the marking must survive canonicalization or the refusal stops happening.

## 26.3 Single latches

`force_single_succ_latches` at `gcc/cfgloopmanip.cc:1796` is eight lines: for each loop whose latch
is not already a single-successor block distinct from the header, find the latch-to-header edge and
split it. The new block becomes the latch and it is empty.

Document 07.1 already established that rucc refuses multiple-latch loops rather than disambiguating
them as GCC does at `gcc/cfgloop.cc:829`. That refusal is really a deferral to here: a loop with
several back edges to the same header gets them all redirected through one new block, and it becomes
single-latch. Only a loop with several *headers*, which is to say an irreducible region, remains
refused.

That is a better position than the document 07.1 phrasing implied and it is worth correcting here:
rucc canonicalizes multiple back edges into a single latch, and refuses only irreducibility.

## 26.4 Loop-closed SSA

The property: if a value defined inside a loop is used outside it, the use goes through a block
parameter in the loop's exit block rather than referring to the inner definition directly.

`rewrite_into_loop_closed_ssa` at `gcc/tree-ssa-loop-manip.cc:638` establishes it, and
`verify_loop_closed_ssa` at 693 checks it, which is itself worth noting: GCC treats this as a
checkable invariant with its own verifier, not as a convention.

**Why it is worth the extra parameters.** Consider unrolling a loop whose result is used afterwards.
Without LCSSA, the outside use names a value defined in the loop body, and after unrolling there are
four copies of that body and the use must be updated to name the right copy. With LCSSA, the outside
use names the exit block's parameter, and unrolling only has to get the argument on the exit edge
right. One update instead of many, and the many are where the bugs are.

The same argument applies to every loop transformation that duplicates the body: peeling, unrolling,
versioning, vectorization. All of them are much easier in LCSSA form, and this is why the property
exists.

**In rucc's IR this is nearly free.** A value crossing a block boundary in a loop-closed way is a
block parameter, and block parameters are already how values cross joins. The transformation is: for
each exit block, for each value defined in the loop and used at or after the exit block, add a
parameter to the exit block and pass the value on the exit edge. The uses are rewritten to the
parameter.

Note the phrase "used at or after". The uses to rewrite are those dominated by the exit block, which
needs a dominance query per use, and the ones that are not dominated by it are reached through some
other exit, which needs its own parameter. A loop with several exits therefore gets several exit
parameters for the same value, which is correct and is one reason the property costs something.

**The maintenance obligation.** Once established, every loop pass must preserve it, and the verifier
must check it after every loop pass. GCC's `checking_verify_loop_closed_ssa` at
`gcc/tree-ssa-loop-manip.cc:1313` is called from inside the transformations for exactly this reason.
rucc's verifier gets a loop-closed check that is enabled between canonicalization and the end of the
loop pipeline, which is the same window as document 21.3's invariants.

## 26.5 Dedicated exits

An exit block should have predecessors only from inside the loop being exited. Otherwise a
transformation that changes the exit affects control flow that had nothing to do with this loop, and
the exit block cannot carry the loop's exit parameters without those parameters being undefined on
the unrelated edges.

Established by splitting: an exit edge whose destination has other predecessors gets a new block on
the edge. Same mechanism as preheaders, opposite direction.

## 26.6 Loop header copying, which is the interesting one

The other four are bookkeeping. This one changes the program.

A `while (c) { body }` loop tests `c` at the top, so the CFG has the header ending in a conditional
branch, one arm entering the body and one leaving. A `do { body } while (c)` loop tests at the
bottom. The second form is better for almost every purpose: the header is not a join and a branch at
once, the body is a single region, the latch's condition is where the induction variable's final
value is known, and the loop has one entry test rather than one per iteration plus one.

Loop header copying converts the first into the second by duplicating the header before the loop:

```
if (c) { do { body } while (c); }
```

The condition is now evaluated once outside and once per iteration at the bottom, which is the same
count, and the loop body is a proper do-while.

`gcc/tree-ssa-loop-ch.cc` does this. `do_while_loop_p` at `gcc/tree-ssa-loop-ch.cc:519` tests whether
the loop is already in the desired form: the latch must be empty and must have a single predecessor.
`should_duplicate_loop_header_p` at 199 decides whether to copy, bounded by
`max-loop-header-insns` `Init(20)` (`gcc/params.opt:690`).

GCC 16 runs the pass twice, as `pass_ch` and `pass_ch_vect` (`gcc/tree-ssa-loop-ch.cc:730` and 766),
the second immediately before vectorization, because vectorization needs the do-while form and the
intervening passes may have undone it. And GCC 16's version consults the range machinery: it builds a
path query through the header (`gcc/tree-ssa-loop-ch.cc:44`) to determine whether the copied
condition is statically true, in which case the entry test disappears entirely and the loop is known
to execute at least once.

**That last point is the largest single benefit and it is worth being explicit about.** After header
copying with a provable entry condition, the loop is known to run at least one iteration. Document
07.5's trip count analysis then gives a `Bound` rather than an `Estimate`, LICM can hoist a
speculative computation without proving it safe because the loop body definitely executes, and the
vectorizer does not need a guard. Several later passes get much simpler.

**rucc builds header copying, at `-O1` and above, immediately after the loop forest is built and
before anything else in the loop pipeline.** It is the first loop pass. The size limit is GCC's 20
instructions and it uses document 10's ranges to try to prove the entry condition.

The cost is code growth: the header is duplicated, so a 20-instruction header costs 20 instructions
per loop. At `-Oz` this is not run. At `-Os` it runs with a much smaller limit, on the order of five
instructions, since the do-while form is also slightly smaller in the steady state.

## 26.7 What the pipeline looks like

Per document 03.4 and this document, the loop pipeline at `-O2` opens with:

1. Build the loop forest (document 07.1). Refuse irreducible regions.
2. Canonicalize: preheaders, single latches, dedicated exits, loop-closed SSA.
3. Loop header copying, then re-canonicalize the loops it changed.
4. The rest: LICM (27), induction variables (28), unrolling (29).

And it closes with the loop forest being discarded, after which document 21's cleanup removes the
empty preheaders and latches that nothing used, and the loop-closed parameters that are now redundant
are removed by document 21.2's pass.

That last point is worth noticing: loop-closed SSA introduces block parameters whose only argument is
one value, which is exactly the redundant parameter document 21.2 removes. So the cost of LCSSA is
paid back automatically at the end of the loop pipeline by a pass that exists anyway.

## 26.8 How this is wrong

**Canonicalization does not converge.** Creating a preheader can change which block is a latch;
splitting a latch can create a block that needs to be a dedicated exit. The order matters:
preheaders, then latches, then exits, then LCSSA, and LCSSA last because it is the only one that
depends on the final CFG shape. Done in that order, one pass suffices, and that should be asserted
rather than assumed by running the canonicalizer twice and checking nothing changed in a debug build.

**The loop forest is stale after canonicalization.** Every split adds a block that belongs to some
loop, and the forest must be updated incrementally or rebuilt. GCC updates incrementally, which is
faster and is where its loop bugs live. rucc rebuilds, per document 06.5's general rule that a stale
analysis is worse than an absent one, and measures whether the rebuild cost is visible. It is one
traversal and it should not be.

**Loop-closed SSA is broken by a later pass and nothing notices.** The verifier check, in the window.

**Header copying duplicates a header with a side effect.** The header contains the condition and
whatever computes it. If that includes a call or a store, duplicating it executes it twice on the
first iteration path. GCC's `should_duplicate_loop_header_p` refuses in that case and rucc's must
too, via the purity whitelist from document 17.1.

**Header copying is applied to a loop that never executes.** The copied condition is false, the loop
body is unreachable, and the code has grown for nothing. Harmless and worth catching with the range
query, which will fold the copied condition to false and let document 21 delete the loop entirely.
That is a nice outcome: header copying plus range analysis deletes provably-never-executed loops as a
side effect.

**Irreducibility marking is lost during canonicalization.** 26.2. Then a loop pass runs on an
irreducible region and produces something wrong. This is one of the few places where a bookkeeping
error becomes a miscompilation rather than a missed optimization.

## 26.9 What it costs

Preheaders, latches and exits are each one pass over the loop forest with a block split per
violation, so linear in loops.

Loop-closed SSA is the expensive one: it needs, for each value defined in a loop, the set of uses
outside it. That is a walk of all uses, which is linear in the function, plus a dominance query per
outside use. GCC restricts the work with a `changed_bbs` set (`gcc/tree-ssa-loop-manip.cc:638`) so
that re-establishing LCSSA after a transformation only examines the blocks that changed, which is a
worthwhile optimization and should be built the second time it is needed rather than the first.

Header copying duplicates up to 20 instructions per loop, plus a range query per loop.

The measurement in document 42: the fraction of `-O2` time spent in canonicalization, which should be
small and which is a pure overhead that the rest of the loop pipeline pays for; and separately, how
many loops are in do-while form after header copying, which should be nearly all of them and which,
if it is not, means the size limit is too tight.
