# 21. CFG simplification

The janitor. Nothing here is clever, none of it makes a program faster on its own, and without it
every other pass in the optimizer runs on an IR full of debris and produces worse output. Document
17.5 already established that this pass is part of the cleanup group and runs several times.

GCC has two implementations, one per IR: `gcc/tree-cfgcleanup.cc` at 1,627 lines and
`gcc/cfgcleanup.cc` at 3,339. The second is larger because it can compare instructions, which
enables cross jumping. rucc has one IR through both middle end and back end, so it has one
implementation that grows a machine-level mode.

## 21.1 The transformations

`gcc/cfgcleanup.cc:21` lists them:

> - Unreachable blocks removal
> - Edge forwarding (edge to the forwarder block is forwarded to its successor...)
> - Cross jumping (tail merging)
> - Conditional jump-around-simplejump simplification
> - Basic block merging.

Five things. Taken one at a time.

**Unreachable block removal.** Compute reachability from entry, delete the rest. This is the one
that must run, because document 06.5 established that every pass may create unreachable blocks and
that no pass other than this one deletes them. It is also the one with a subtlety: deleting a block
removes its arguments from its successors' block parameters, and a block parameter left with fewer
arguments than the block has predecessors is an IR verifier failure. So deletion is a two-step
operation and doing it in the wrong order leaves a window where the IR is invalid, which matters if
the verifier runs mid-pass under `-fverify-each`.

**Edge forwarding.** A block containing nothing but an unconditional branch is a forwarder; redirect
its predecessors to its target and delete it. GCC's conditions, from `maybe_remove_forwarder_block`
at `gcc/tree-cfgcleanup.cc:499`, are worth having exactly: a single successor, the successor is not
the exit block, the successor is not the block itself (that is an infinite loop, not a forwarder),
and the outgoing edge is not abnormal.

The block-parameter equivalent has one extra condition that the phi formulation does not: a
forwarder block that passes arguments to its successor cannot always be removed, because its
predecessors may need to pass *different* arguments. If block F branches to S with argument `v`, and
F has two predecessors, removing F requires both predecessors to pass `v`, which they can if `v`
dominates them. If `v` is defined in F, it cannot. So: a forwarder is removable if it has no
instructions and its outgoing arguments all dominate every predecessor of F. That is a cleaner
statement than GCC's, which has to reason about phi nodes in the successor, and it is another small
payoff from the IR choice.

**Block merging.** A block with one successor, whose successor has one predecessor, merges into it.
`want_merge_blocks_p` at `gcc/tree-cfgcleanup.cc:884` gates it on `can_merge_blocks_p` plus policy.
This one is pure win: it enlarges basic blocks, which makes every local analysis in the compiler
more effective, and it is why the pass runs before the local passes rather than after.

**Branch simplification.** A conditional branch whose two targets are the same block becomes an
unconditional branch, and the condition becomes dead. A conditional branch with a constant condition
becomes unconditional. `cleanup_control_flow_bb` at `gcc/tree-cfgcleanup.cc:293` handles this along
with pruning exception edges that can no longer be taken.

The constant-condition case is where document 14's SCCP hands off: SCCP marks edges non-executable
and does not delete anything, per document 14.4, and this pass reads the marks. Keeping the two
separate is what lets SCCP be an analysis.

**Cross jumping**, also called tail merging: two blocks ending in the same instruction sequence and
branching to the same place have their common tails merged, with one branching to the other's tail.
This is a size optimization and it is the reason `gcc/cfgcleanup.cc` is twice the size of the tree
version, because it needs instruction equality. `min-crossjump-insns` at `gcc/params.opt:853` is
`Init(5)` and `max-crossjump-edges` at `gcc/params.opt:557` is `Init(100)`.

**rucc's position on cross jumping:** `-Os` and `-Oz` only, and at the machine level, in document 37,
where instructions are concrete and comparison is straightforward. At the IR level it fights every
value-level pass by creating merge points, and at `-O2` it costs branches. Document 38's block
layout has an interest in it too, which is another reason to place it late.

## 21.2 Redundant block parameter removal

Document 15.1 deferred this here. A block parameter receiving the same value on every incoming edge
is redundant: replace its uses with that value and drop it.

The subtlety is self-reference. A loop header parameter `x` with arguments `init` from the preheader
and `x` itself from the latch is redundant, because the only value it can hold is `init`, but a
naive "are all arguments equal" test says no. The rule is: a parameter is redundant if all its
arguments, *ignoring arguments that are the parameter itself*, are the same value. This is the same
optimistic reasoning as document 14.1 and it needs the same treatment, which is that removing one
parameter can make another redundant, so it is a worklist.

Roughly fifty lines and it must exist, because leaving redundant parameters around means every use
goes through a block parameter that document 12's hash-consing cannot see through, so two equal
values look different.

## 21.3 The loop-preserving problem

Everything in this document changes the CFG, and document 07 has a loop forest built over that CFG
with canonical properties: a preheader, a single latch, dedicated exits. CFG cleanup happily deletes
an empty preheader as a forwarder, merges a latch into its header, and destroys the canonical form.

GCC handles this by having two entry points. `cleanup_tree_cfg_noloop` at
`gcc/tree-cfgcleanup.cc:1146` does the work without regard for loops; the wrapper at
`gcc/tree-cfgcleanup.cc:1335` runs it and then repairs the loop structure. And there is a specific
guard at `gcc/tree-cfgcleanup.cc:664`: do not remove a preheader that `cleanup_tree_cfg_noloop` just
created.

**rucc's rule, which is simpler and should be stated as an invariant.** Between the loop
canonicalization pass in document 26 and the end of the loop pipeline, the canonical properties are
part of the IR's invariants and the verifier checks them. CFG cleanup running in that window
consults the loop forest and refuses to delete a preheader, merge a latch, or remove a dedicated
exit block. Outside that window there is no loop forest and it does whatever it likes.

The alternative, deleting and then repairing, is what GCC does and it is a permanent source of bugs,
because "repair" means recomputing the forest and hoping the identities of loops are preserved, which
they are not. Refusing is cheaper and the cost is a handful of empty blocks surviving until the loop
pipeline ends, which the next cleanup round removes.

## 21.4 What runs when

Per document 03.4 and the refinement in 17.5, the cleanup group is DCE, DSE, simplify-cfg, DCE, run
twice at `-O2` and once at `-O1`. Within `simplify-cfg`, the order is:

1. Unreachable removal, first, because it makes everything else cheaper and because other passes'
   marks are consumed here.
2. Constant-condition branch simplification, which creates unreachable blocks, so a second
   reachability sweep is folded into this step rather than run separately.
3. Forwarder removal and redundant block parameter removal, interleaved on one worklist, because
   each enables the other.
4. Block merging, last, because it is the one whose result the following passes consume and because
   the earlier steps create merge opportunities.

Not run to a fixpoint, per document 04.7. One pass of each in that order, and the observation from
`gcc/tree-cfgcleanup.cc:1268` that block merging creates new `cleanup_control_flow_bb` opportunities
"so we have to repeat" is exactly the fixpoint temptation the discipline forbids. The measurement in
document 42 decides whether one round leaves anything behind that matters.

## 21.5 What is deliberately not here

**Jump threading** duplicates blocks to eliminate branches and is document 23's. It is often
described as CFG cleanup and it is not: cleanup only ever makes the CFG smaller, and threading makes
it larger. Keeping the distinction sharp is worth doing because "the CFG only shrinks here" is an
invariant that makes this pass easy to reason about and cheap to run repeatedly.

**Block layout and branch inversion** are document 38's, at the machine level, driven by profile
data. Reordering blocks in the middle end achieves nothing because nothing downstream cares about
block order until layout runs.

**If-conversion** is document 22's.

**Tail duplication** and any other form of block cloning. Same reason as jump threading.

## 21.6 How this is wrong

**A block parameter's argument list stops matching its predecessors.** The single most common bug in
this document, and it happens in every one of the five transformations, because all of them change
the predecessor set of some block. The IR verifier's check that every block's argument count matches
its parameter count on every edge catches it, and that check must run after this pass every time,
not only under `-fverify-each`.

**A forwarder is removed whose argument does not dominate a predecessor.** 21.1's extra condition.
Getting it wrong produces a use before a definition, which the verifier's dominance check catches.
That is two verifier checks doing real work, which is the argument for the verifier being cheap
enough to run always.

**A loop is destroyed and the loop pipeline silently does nothing.** 21.3. This one does not produce
wrong code, it produces code that is quietly not optimized, and it is invisible without a test that
asserts the loop forest is intact after each pass. That test should exist.

**An abnormal edge is forwarded.** Edges out of `setjmp`, into computed-goto targets, and out of
potentially-throwing operations are not ordinary control flow and forwarding across them changes
where control can arrive. GCC checks `EDGE_ABNORMAL` explicitly at `gcc/tree-cfgcleanup.cc:513` and
rucc needs the same edge flag for the same reason, which document 09.5 already required for
`setjmp`.

**Unreachable code containing a definition is deleted and a live use remains.** This cannot happen
if the use was genuinely reachable, since a reachable use of a definition in an unreachable block
would violate dominance. It can happen if the reachability computation is wrong, which is why
deletion recomputes rather than trusting a stale marking. Document 06.5's rule that a stale
dominator tree is a use-after-free applies identically to reachability.

**Profile counts stop summing.** Merging two blocks or redirecting an edge must move the counts.
Document 11.5 requires the verifier to check that a block's count equals the sum of its incoming
edges' counts, and this pass is the one most likely to break it. Note the check must be tolerant:
counts are integers, they do not divide evenly, and document 11.1's quality tracking exists so that
a degraded sum is recorded rather than being an error.

## 21.7 What it costs

Every step is linear in blocks and edges. Reachability is one traversal. Forwarding and merging are
worklist algorithms over blocks where each block is processed a bounded number of times. Redundant
parameter removal is a worklist over parameters.

The real cost is that it runs four to six times per function at `-O2`, and the incremental cost of
each run after the first is small only if it exits early when there is nothing to do. So each step
checks a cheap precondition before allocating anything, and the pass reports "changed: false" so
document 04's pass manager can skip the verifier and the dominator invalidation.

That last point generalises. A pass that reports it changed nothing lets the pass manager skip
recomputing everything, and in a pipeline where cleanup runs six times and typically does something
twice, that is worth more than any optimization within the pass itself.
