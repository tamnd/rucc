# 27. Loop invariant code motion

Move a computation whose operands do not change in the loop to the preheader, so it executes once
instead of `n` times. The oldest loop optimization, conceptually trivial, and the version that
actually pays lives entirely in the two hard parts: proving a memory reference is invariant, and
deciding when moving something is worse than leaving it.

`gcc/tree-ssa-loop-im.cc` is 3,879 lines. Almost none of that is the hoisting.

## 27.1 The three-way legality answer

`gcc/tree-ssa-loop-im.cc:325` defines the whole legality question as an enum:

> `MOVE_IMPOSSIBLE` -- No movement, side effect expression.
> `MOVE_PRESERVE_EXECUTION` -- Must not cause the non-executed statement become executed, memory
> accesses, ...
> `MOVE_POSSIBLE` -- Unlimited movement.

The middle case is the interesting one and it is the shape every speculation question in this
directory takes. A load is invariant and can be hoisted, but hoisting it into the preheader executes
it even when the loop runs zero times, and if the pointer is invalid that faults. Similarly a
division by an invariant divisor, which traps on zero.

So the rule is: `MOVE_PRESERVE_EXECUTION` statements may be hoisted only if they are executed on
every iteration, which means their block dominates the latch, **and** the loop is known to execute at
least once.

That second condition is why document 26.6 spent so long on loop header copying. After header
copying with a provable entry condition, every loop runs at least once, and the entire
`MOVE_PRESERVE_EXECUTION` category collapses into `MOVE_POSSIBLE` for statements that dominate the
latch. Without it, most useful hoists are blocked. This is the clearest instance in the whole
optimizer of one pass existing to make another pass work, and it is worth stating plainly: **loop
header copying is a prerequisite for LICM being useful, not a separate nicety.**

The alternative is loop versioning: emit a guarded copy of the loop where the hoist is legal and fall
back otherwise. That is document 30's and it is a much bigger hammer.

**The shared predicate.** Documents 15.6, 16.6 and 22.6 all need "is this safe to speculate". Here is
its definition, and it is this document's to own because this is where it is most exercised:

A value is safe to speculate at a point P if evaluating it at P cannot trap, cannot fault, and cannot
be observed. Concretely: pure arithmetic other than division and remainder is always safe; division
and remainder are safe when the divisor is known non-zero by document 10's ranges, and additionally
when signed, when the operands cannot be `INT_MIN / -1`; a load is safe when the address is known
dereferenceable, which in M4 means the object is a local whose size is known and the offset is in
range, or the same address is unconditionally loaded elsewhere in the region; a store is never safe;
a call is safe only if it is `const`; `volatile` and atomic operations are never safe.

One function, in `rucc-opt`, consulted by four passes. Its default answer is no.

## 27.2 The cost model, which is more interesting than expected

`stmt_cost` at `gcc/tree-ssa-loop-im.cc:611` opens by admitting "The values here are just ad-hoc
constants, similar to costs for inlining", and then makes several decisions worth copying:

- A conditional or a phi costs `LIM_EXPENSIVE` unconditionally, with the comment "Always try to create
  possibilities for unswitching". So the cost model deliberately overprices conditionals, not because
  they are expensive to compute but because hoisting them enables a different transformation. That is
  a cost model encoding a pass interaction, which is unusual and honest.
- A call costs `LIM_EXPENSIVE`, "We should be hoisting calls if possible", **except**
  `__builtin_constant_p`, which costs zero because it folds to a constant anyway and moving it is
  pointless.
- Anything referencing memory costs `LIM_EXPENSIVE`: "Hoisting memory references out should almost
  surely be a win."
- Multiplies, divisions, shifts, rotates and comparisons cost `LIM_EXPENSIVE`.
- A `CONSTRUCTOR` costs its element count.
- An SSA name or a parenthesised expression costs zero, so wrapping does not change the decision.
- Everything else costs 1.

`lim-expensive` is `Init(20)` (`gcc/params.opt:428`).

**Why there is a cost model at all**, given that moving a computation out of a loop is obviously
good: because it is not. Hoisting a value creates a live range spanning the entire loop, which
consumes a register for the loop's duration. Hoist ten values and the loop body spills. The classic
failure is a loop that ran entirely in registers becoming a loop with a spill and a reload in the
body, which is strictly worse than recomputing an add.

So LICM's real decision is: is the hoisted computation more expensive than a register. GCC's answer
is the ad-hoc table, plus the dependency accumulation at `gcc/tree-ssa-loop-im.cc:600` where a
statement whose only uses are other hoisted invariants inherits their cost, on the reasoning that
hoisting a chain together creates one live range rather than several.

**rucc's version.** The same table, in document 40, with the same structure and the same admission
that the constants are ad hoc. Plus one thing GCC does not have and rucc should: a register pressure
estimate. Before hoisting, count the values already live across the loop; if that count exceeds the
target's allocatable register count minus a margin, stop hoisting anything but the genuinely
expensive operations, meaning division and calls.

That estimate is crude and it is much better than nothing, and it is the same information document
12.5's GCM heuristic needs, so it should be one shared function. Document 40 owns it.

## 27.3 Store motion

The valuable and dangerous half. A store to an invariant address inside a loop can become a load
before the loop, a register in the loop, and a store after:

```c
for (...) { ... *p = v; ... }
```

becomes `tmp = *p; for (...) { ... tmp = v; ... } *p = tmp;`. The loop body no longer touches memory,
which is often the difference between a loop that runs at memory speed and one that runs at register
speed.

**Two conditions, both hard.**

First, no other memory access in the loop may alias `*p`. This is `ref_indep_loop_p`
(`gcc/tree-ssa-loop-im.cc:264`) and it is a whole-loop alias query: every load and store in the body,
against this reference. Expensive, and the reason the pass is 3,879 lines.

Second, and this is the one that produces wrong code: **the store must not be introduced on a path
that did not have it**. `gcc/tree-ssa-loop-im.cc:2043` states it exactly:

> The store is only done if MEM has changed. We do this so no changes to MEM occur on code paths that
> did not originally store into it.

If the store is conditional inside the loop, the transformation as written stores unconditionally
after the loop, which writes to `*p` even when the condition never held. If another thread is reading
`*p`, or if `*p` is in read-only memory on that path, that is a bug. GCC's solution is a flag
variable: `lsm_flag` is set when the store would have happened, and the epilogue stores only if the
flag is set. The full expansion is at `gcc/tree-ssa-loop-im.cc:2067`.

GCC further distinguishes `sm_ord`, `sm_unord` and `sm_other` (`gcc/tree-ssa-loop-im.cc:2390`) for
ordinary stores whose order must be retained, conditionally executed stores that may be reordered,
and stores not eligible at all.

**rucc's position for M4: store motion for unconditional stores only.** If the store executes on
every iteration, the epilogue store is unconditional and correct, and no flag is needed. That is a
large fraction of the value at a small fraction of the complexity. The conditional case with the flag
variable is post-M4 and is recorded with its full expansion, because it is a transformation nobody
would reinvent correctly.

Note that under the unconditional restriction the transformation is exactly redundant load
elimination plus dead store elimination applied across iterations, and documents 16 and 17 already
own both halves. What this document adds is the loop-carried view, which those passes do not have
because they walk memory SSA within a single traversal.

## 27.4 Unswitching, mentioned here because 27.2 pointed at it

A loop containing a condition that is loop-invariant can be split: test the condition once, and emit
two copies of the loop, one with the condition true and one false, each with the branch removed.

```c
for (i...) { if (c) A; else B; }   =>   if (c) for (i...) A; else for (i...) B;
```

Doubles the code, removes a branch per iteration, and each copy is a simpler loop that other passes
handle better. It is document 30's, being a restructuring transformation, and it appears here because
GCC's LICM cost model deliberately hoists conditions to enable it (27.2), and that interaction has to
be recorded somewhere or the odd cost entry looks like a mistake.

## 27.5 What rucc builds

One pass, in the `-O2` pipeline after loop header copying, of perhaps 500 lines.

*The invariance test.* A value is invariant in a loop if all its operands are defined outside the
loop or are themselves invariant. One fixpoint over the loop body, or, since rucc is in SSA and the
loop forest gives containment, one pass in dominator order because a definition dominates its uses.
No fixpoint needed for values; a fixpoint is needed only for memory, which is handled below.

*The legality test.* The three-way enum from 27.1, plus the shared speculation predicate.

*The cost test.* Document 40's table plus the pressure estimate.

*Hoisting.* Move the instruction to the end of the preheader. In rucc's IR this is unlinking and
relinking in a doubly linked list, which is why the IR has one.

*Store motion, unconditional case.* Per 27.3.

**And a note on GCM.** If document 12's arm B or C wins, global code motion already hoists a
loop-invariant pure value out of a loop, because the earliest block that dominates all uses is
outside the loop. So the pure-arithmetic half of LICM is subsumed, exactly as sinking was in document
17.4.

What is not subsumed: memory operations, which document 12.7 forbids GCM from moving; the cost model,
because GCM's placement rule is structural and does not consider register pressure, which is document
12.5's noted tension; and store motion, which is not code motion at all but a rewrite.

So under arms B or C, LICM shrinks to the memory half plus a pressure-driven *sinking* correction to
GCM's decisions. That is a real simplification and it should be measured rather than assumed: document
12.3's experiment should report how many hoists LICM finds that GCM did not.

## 27.6 How this is wrong

**A load is hoisted and faults on a zero-trip loop.** The `MOVE_PRESERVE_EXECUTION` rule and the
proof that the loop runs at least once. This is the classic LICM bug and it appears whenever somebody
strengthens the hoisting without checking the trip count.

**A division is hoisted and traps.** Same rule, different operation, and the one people forget
because division does not look like a memory access.

**Store motion introduces a store.** 27.3's conditional case, which M4 does not build for exactly
this reason.

**Store motion is performed and something aliases.** The whole-loop independence test. A missed alias
here means the loop reads a stale value from a register while another pointer writes memory. It is
silent, it depends on the aliasing actually occurring at run time, and it is the worst bug in this
document.

**A `volatile` access is hoisted.** Never. Every access is observable, including the count.

**An atomic or a fence is moved.** Never, per document 09.5.

**Too much is hoisted and the loop spills.** The pressure estimate. This does not produce wrong code,
it produces a regression that looks like LICM being harmful, and it is the reason a naive LICM
implementation sometimes measures worse than none.

**Something is hoisted out of an irreducible region.** There is no preheader. Document 26.2 requires
the irreducibility marking to survive and this pass to skip marked regions.

**The hoisted value is placed before its own operands.** Hoisting a chain must hoist in dependency
order. Processing the loop body in dominator order gives that for free; processing it in block order
does not.

## 27.7 What it costs

The invariance test is one pass over the loop body per loop, so linear in the function summed over
the loop nest depth, which is linear times depth.

The legality and cost tests are per candidate.

Store motion's independence test is the expensive part: for each candidate reference, an alias query
against every memory access in the loop. That is quadratic in the loop's memory operations in the
worst case and GCC bounds it with caching. rucc bounds it with the same budget mechanism as document
09.2's memory walk, and it should report budget exhaustion for document 42's 1% threshold.

The measurement in document 42: how many hoists LICM performs that GCM did not, per 27.5; and the
run-time effect on the loop-heavy portion of the corpus with store motion on and off, since store
motion is the half with the risk and it should have to justify itself.
