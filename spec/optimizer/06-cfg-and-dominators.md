# 06. The control flow graph and dominance

Every analysis in documents 07 through 11 depends on this one and most of them depend on it
transitively through the dominator tree, so it is worth more care than its size suggests. It is
also the analysis most likely to be quietly wrong, because a dominator tree that is stale rather
than absent produces a miscompilation rather than a crash, and because the two natural algorithms
for computing it disagree about what "fast" means in a way that only shows up at scale.

## 6.1 What a CFG is in rucc, and what it is not

`crates/rucc-ir/src/func.rs` holds the whole representation and three of its decisions determine
this document.

**Block parameters, not phi nodes.** A terminator names its successors as `BlockCall` values, each
a block plus an argument list (`func.rs:922`). This is Cranelift's and Swift's and MLIR's choice
and it is the right one: a phi node is a pseudo-instruction whose operands are positionally tied
to a predecessor list that lives somewhere else, and every pass that edits the CFG has to keep the
two in step. With block parameters the argument travels on the edge that carries it, so splitting
an edge, redirecting a jump or deleting a predecessor is a local edit that cannot desynchronise
anything. The cost is paid at instruction selection, where the parallel copy on each edge has to
be sequenced, and document 36 pays it.

**Successors are derivable, predecessors are not stored.** `func.successors(inst)` at
`func.rs:370` reads the terminator's target list. There is no predecessor list anywhere in `Func`.
The verifier builds one by scanning every instruction of every block
(`crates/rucc-ir/src/verify.rs:1306`), which is O(instructions) and is correct exactly once.

**`block_addr` is an edge.** `verify.rs:1299` counts a block named by `block_addr` as a successor
of the block containing that instruction, and the comment is careful about why: the edge is not
where control actually flows, but it can only *add* predecessors and therefore only *remove*
dominance, so treating it as an edge makes the dominance check stricter and never looser. This is
sound for the verifier. It is not sound as a general CFG, because the real edge runs from every
`indirect_br` that the address can reach to the labelled block, and document 21 needs the real
one to know whether a block is dead. The analysis has to model computed gotos properly and the
verifier's approximation does not carry over.

## 6.2 What GCC does, and the one place it is cleverer than everybody

GCC's `gcc/dominance.cc` implements Lengauer and Tarjan, and says so in the file header at
`gcc/dominance.cc:21`. The simple-eval variant with path compression runs in O(E log V); the
sophisticated variant with union by rank is O(E alpha(E,V)). It is asymptotically better than the
iterative algorithm and it is, on ordinary function-sized CFGs, slower, which is the entire
argument of Cooper, Harvey and Kennedy's 2001 paper and the reason rucc's verifier uses theirs
(`verify.rs:1280`).

The interesting part of GCC's implementation is not the algorithm. It is that a `basic_block`
carries `bb->dom[2]`, two pointers into an **ET-forest** (`gcc/et-forest.h`), a data structure
that maintains a tree under edge insertion and deletion with logarithmic updates and
polylogarithmic ancestor queries. That gives GCC something rucc does not currently plan for:
a dominator tree that can be *edited* rather than recomputed. `set_immediate_dominator` calls
`et_set_father` (`gcc/dominance.cc:888`), so a pass that redirects one edge and knows the
consequence can update the tree in place.

GCC then layers a second representation on top for query speed. `compute_dom_fast_query` at
`gcc/dominance.cc:667` walks the ET-forest assigning `dfs_num_in` and `dfs_num_out` to each node,
after which `dominated_by_p` (`gcc/dominance.cc:1125`) is two integer comparisons:

```c
if (dom_computed[dir_index] == DOM_OK)
  return (n1->dfs_num_in >= n2->dfs_num_in
          && n1->dfs_num_out <= n2->dfs_num_out);
return et_below (n1, n2);
```

So there are three states, not two: no dominators, dominators available but only queryable in
polylogarithmic time through the ET-forest, and `DOM_OK` where queries are O(1) but any structural
edit invalidates the numbering. This tri-state is the design GCC arrived at after years of passes
wanting incremental updates and other passes wanting fast queries, and it is genuinely good.

`iterate_fix_dominators` at `gcc/dominance.cc:1392` is the third mode: recompute dominators for a
named subset of blocks, used by passes that know their damage is local. `prune_bbs_to_update_
dominators` at `gcc/dominance.cc:1240` works out which blocks in the caller's list actually need
recomputing.

Dominance frontiers are separate, in `gcc/cfganal.cc:1639`, with the iterated frontier computation
Cytron's SSA construction needs at `gcc/cfganal.cc:1680`.

## 6.3 What rucc should build

**One algorithm, Cooper-Harvey-Kennedy, and no ET-forest.** The verifier's implementation at
`verify.rs:1294` is already correct and already has the right shape: reverse postorder, a `rank`
array, an `idom` array indexed by rank, and `meet` walking two chains towards the entry
(`verify.rs:1397`). It should be lifted into `rucc-opt` as an analysis with three changes.

First, keep the postorder. It is computed anyway (`verify.rs:1315`) and half the passes in this
directory want to iterate blocks in reverse postorder or postorder, and computing it separately in
each is how a compiler acquires six subtly different traversal orders.

Second, add O(1) queries. The current `dominates` at `verify.rs:1384` walks the idom chain, which
is O(depth) and fine when the caller asks once per value definition. GVN, LICM and code motion ask
per pair, in inner loops. Take GCC's answer: a DFS over the dominator tree assigning an in and out
number, after which dominance is two comparisons. The tree is small, the walk is linear, and it is
computed once when the analysis is first requested.

Third, do not build the ET-forest, and accept that every CFG change recomputes the tree from
nothing. This is the departure from GCC and the reason is document 04.4's table: almost every
analysis is invalidated by any CFG change anyway, so an incrementally-maintained dominator tree
would be the one survivor of a clearing that took everything else. The measurement that would
change this is in 6.7, and if it says recomputation is more than 3% of `-O2` time, the response is
to cluster the CFG-changing passes harder before it is to add a data structure.

**Post-dominators.** The same algorithm on the reversed graph, with one wrinkle GCC handles at
`gcc/cfganal.cc:622` and rucc must handle too: an infinite loop has no path to the exit, so the
reverse graph is disconnected and post-dominance is undefined for its blocks. GCC connects
infinite loops to the exit with fake edges. rucc should do the same, and mark the edges fake so
that no pass mistakes one for a real path. Post-dominators are needed by aggressive DCE (document
17), by if-conversion (document 22) and by the control dependence relation.

**Dominance frontiers, probably not.** rucc builds SSA during lowering per `spec/08-ir.md`, so the
one classical consumer of frontiers is gone. Aggressive DCE wants the *post*-dominance frontier
for control dependence. Nothing else does. Build it when document 17 needs it and not before.

**Predecessors, cached.** A `Vec<Vec<Block>>` computed with the successors, invalidated with them.
The scan in `verify.rs:1306` visits every instruction to find the terminators; the analysis should
instead visit each block's last instruction, which requires the CFG analysis to trust the "exactly
one terminator, at the end" invariant that the verifier checks. That is the correct division: the
verifier proves the invariant, the analyses assume it.

## 6.4 The loop-closed question, and reducibility

C produces irreducible CFGs. Not often, but `goto` into a loop body does it, and so do some
state machines written as `switch` inside `for`, and so does every program run through certain
obfuscators. A compiler that assumes reducibility is a compiler with a class of wrong answers it
will never find in its own test suite.

rucc's dominator computation handles irreducibility correctly with no special case: the iterative
algorithm converges on any graph, it just takes more than one pass, and `verify.rs:1348` says so
in a comment. What does not survive is loop analysis, and document 07 handles it there.

The one thing to settle here is that rucc does **not** transform irreducible regions into reducible
ones. Node splitting can blow up code size exponentially, controlled node splitting is a research
topic rather than a solved problem, and the payoff is that a handful of loop optimizations apply
to code that is rare and usually cold. GCC does not do it either. The loop analysis reports an
irreducible region, the loop passes decline it, and the value-level passes are unaffected because
they only need dominance.

## 6.5 Reachability, and what to do with unreachable code

An unreachable block is not an error and the verifier is right not to treat it as one
(`verify.rs:1381`). Front ends produce them constantly: the block after a `return`, the block
after `__builtin_unreachable`, the arm of an `if` on a constant.

The rule for the whole optimizer, stated once here: **an unreachable block is invisible to every
analysis and every transformation, and is deleted by CFG simplification rather than by whoever
noticed it.** The reasoning is that a pass which deletes blocks as a side effect of doing
something else is a pass whose fuel accounting is wrong and whose dumps are unreadable. The
analysis exposes `reaches()` (`verify.rs:1375`), passes skip what it says no to, and document 21's
pass does the deleting.

There is a subtlety worth being explicit about because it produces real miscompilations. A value
defined in an unreachable block dominates nothing, so "is defined in a block that dominates this
use" and "is defined in a reachable block" are different questions, and a pass that uses the first
to justify moving code can move a use into a reachable position. The verifier sidesteps this by
declaring unreachable blocks vacuously dominated (`verify.rs:1384`). An optimizer must not: the
analysis should answer `dominates` as `false` when either block is unreachable, and passes should
be reading `reaches` explicitly.

## 6.6 Edge cases the tests must contain

Six graphs, in `crates/rucc-opt/tests/`, each with the dominator tree written out by hand.

A straight line. A diamond. A natural loop with a back edge to a header with two predecessors. An
irreducible graph, the classic two-entry loop, where the answer is that neither candidate header
dominates the other. A graph with an unreachable block that has a back edge to itself, which is
where a careless postorder walk loops forever. And a function whose entry block is also a loop
header, which is illegal under `func.rs:22`'s invariant that the entry has no predecessors and is
therefore a verifier test rather than a dominator test, but which somebody will write a pass to
create and which must be caught.

Plus one property test: for a randomly generated CFG, `dominates(a, b)` computed by the analysis
agrees with `dominates(a, b)` computed by the definition, which is enumerating every path from the
entry to `b` in a graph small enough for that to terminate. This is the test that would catch a
wrong `meet`, and a wrong `meet` is a silent wrong answer everywhere downstream.

## 6.7 What it costs and how that is known

Dominators are recomputed after every CFG-changing pass, and document 03.4's `-O2` list has
roughly six of those. On a function with *B* blocks and *E* edges, one computation is a postorder
walk plus a fixed point that converges in two or three sweeps on real code, so call it 4(B+E)
plus the DFS numbering.

The number to measure, reported by `-ftime-report` per document 42, is the fraction of `-O2` wall
time spent in the CFG and dominator analyses on the SQLite amalgamation. The threshold that
triggers reconsidering 6.3's no-incremental-updates decision is 3%. Below that, recomputation is
cheaper than the bugs an incremental structure invites, and this is not a close call: an
incorrectly-updated dominator tree is the single hardest class of compiler bug to find, because
the symptom appears in a pass that did nothing wrong.

## 6.8 How this is wrong

Three ways, in the order they will happen.

A pass edits the CFG and does not declare it, the dominator tree survives, and a later pass hoists
a value above its definition. The defence is document 04.3's lying-pass check, which recomputes
preserved analyses in debug builds and compares, and it is the reason that check is described
there as the highest-value piece of the manager.

The `block_addr` approximation from 6.1 gets copied out of the verifier into the analysis, and a
computed goto target is treated as dead because nothing branches to it directly. The defence is
that the analysis has its own tests and one of them is a computed goto.

Post-dominators are requested for a function containing an infinite loop and the fake exit edges
are missing, so the analysis reports garbage rather than failing. The defence is an assertion that
every block reaches the exit in the reversed graph before the fixed point runs, which is one line
and which turns a wrong answer into a crash.
