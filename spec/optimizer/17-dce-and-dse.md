# 17. Dead code, dead stores and sinking

Three transformations that all answer "is this needed" and answer it in three different directions:
forwards over values, backwards over memory, and downwards over control flow. GCC spends 4,973
lines: `gcc/tree-ssa-dce.cc` at 2,257, `gcc/tree-ssa-dse.cc` at 1,816 and `gcc/tree-ssa-sink.cc` at
900.

Of everything in this directory, dead code elimination has the highest ratio of value to
implementation cost. It is also the pass that runs most often: document 03.4's `-O2` list has it
three times, because every other pass leaves debris and no other pass cleans up after itself.

## 17.1 Dead code elimination, the simple version

An instruction is dead if it has no side effects and its result is unused. Remove it, and its
operands may become dead in turn.

The implementation is a worklist over uses, counted from the end. Roughly forty lines. It should
exist in `rucc-opt` before anything else in this document, and it should be in the `-Og` pipeline,
because it is the only transformation that unambiguously improves both size and debuggability.

Two things it must get right and both are about "no side effects".

*The whitelist rule again.* As with escape analysis in document 08.6: an instruction is removable
only if its opcode is in an enumerated pure set. Every other opcode, including opcodes added later,
is retained. Writing it the other way means the next person to add an opcode silently makes it
deletable.

*Trapping is a side effect only sometimes.* An integer division by a possibly-zero divisor traps on
x86-64 and is undefined behaviour in C. Deleting a dead division is therefore legal in C terms and
changes observable behaviour on a program that was going to crash. rucc should delete it, and
`-fsanitize=integer-divide-by-zero` should mark the operation as having a side effect so it
survives. This is the general pattern: sanitizers work by making operations non-removable, which
means the purity predicate consults the flags on the instruction, not only its opcode.

## 17.2 Aggressive dead code elimination

The simple version cannot remove a branch, because a branch has a side effect: it decides where
control goes. So a loop that computes nothing and a conditional whose arms are both empty survive.

The aggressive version, from Cytron et al. and adapted in GCC by Bosscher
(`gcc/tree-ssa-dce.cc:5`), inverts the question. Instead of marking dead things and removing them,
it marks *necessary* things and removes everything else. The three phases, from
`gcc/tree-ssa-dce.cc:38`:

1. Mark as necessary everything known to be: calls, stores, returns, volatile accesses,
   inline assembly.
2. Propagate: anything producing an operand of a necessary statement is necessary, and any
   *branch that a necessary statement is control dependent on* is necessary.
3. Delete everything unmarked, and redirect each deleted branch to its nearest necessary
   post-dominator.

Step 2's second clause is the whole trick and it needs the **control dependence** relation: block B
is control dependent on edge `(A, C)` if B post-dominates C but does not post-dominate A. Which is
to say, that branch decides whether B runs.

This is the one consumer of post-dominators and post-dominance frontiers in the whole optimizer,
which document 06.3 anticipated. `gcc/tree-ssa-dce.cc:98` notes the implementation detail that
matters: "We expect each block to be control dependent on very few edges", so the relation is
stored sparsely per block rather than as a matrix.

**Two traps, both about loops.**

An infinite loop with an empty body is control dependent on nothing and produces nothing necessary,
so aggressive DCE deletes it. In C this is legal for a loop with no side effects and a
non-constant controlling expression, by C11 6.8.5p6, and it is the source of one of the most
notorious classes of surprise. GCC handles this specially, marking loop latches necessary in some
configurations (`gcc/tree-ssa-dce.cc:618`). **rucc should not delete potentially-infinite loops in
M4.** Mark every latch necessary. The optimization is worth little and the surprise is worth a lot,
and `-ffinite-loops` can turn it on for people who want it, which is what GCC's flag does.

And connecting infinite loops to the exit, per document 06.3, is a prerequisite: without the fake
edges, post-dominance is undefined for blocks in an infinite loop and the control dependence
relation is garbage.

**rucc's position.** Simple DCE from the start, at every level including `-Og`. Aggressive DCE at
`-O2`, once, late, after the CFG passes have run, because it is the only thing that removes a
computation whose only use was a branch that jump threading proved constant.

## 17.3 Dead store elimination

`gcc/tree-ssa-dse.cc:1` states the definitions precisely and they are worth having exactly:

> A dead store is a store into a memory location which will later be overwritten by another store
> without any intervening loads.
> A redundant store is a store into a memory location which stores the exact same value as a prior
> store to the same location.

And the observation at `gcc/tree-ssa-dse.cc:33` is the one to internalise: dead store elimination
and redundant load elimination "are the same transformation applied to different views of the CFG".
One walks memory SSA forwards, the other backwards.

The mechanism, at `gcc/tree-ssa-dse.cc:14`: if a store's memory definition has exactly one use, and
that use is a later store to the same location which post-dominates it, the first store is dead.
Single use guarantees no intervening aliasing load. Post-dominance guarantees the later store
actually executes.

Two extensions GCC has and rucc should have.

*Trimming.* A partially dead store, where a later store covers half of it, can be narrowed rather
than removed. This matters for the common pattern of zeroing a struct and then filling in fields.

*Stores dead at function exit.* A store to a local whose address does not escape is dead if nothing
reads it before the function returns. This removes a great deal of code that C programmers write
for clarity, and it depends on the escape analysis in document 08.4.

The second one has a trap with a name: `memset` of a buffer holding a key, before returning, is
dead by this rule and is removed, which is why `explicit_bzero` and `memset_s` exist. rucc must
honour whatever mechanism it provides for this, and the simplest correct one is that a call to
`explicit_bzero` is never removable, which follows from the whitelist rule in 17.1 if the function
is not marked pure.

**Where it runs.** `-O2` only, after redundant load elimination, because eliminating loads is what
makes stores dead. It needs memory SSA and post-dominators.

## 17.4 Sinking

`gcc/tree-ssa-sink.cc`, 900 lines: move a computation down to the block where it is used, so that
paths not reaching that block do not execute it. Partial dead code elimination by another name.

**Under document 12, GCM does this.** Click's late scheduling places a value at the latest position
that dominates all uses, which sinks it into the arm of a branch when all uses are there. So if arm
B or C of the e-graph experiment wins, sinking is not a pass.

If arm A wins, sinking is a pass, and it is a small one: walk blocks in reverse postorder, and for
each instruction whose uses are all in one dominated successor subtree, move it there. The
conditions are the same as GCM's late scheduling and the same register-pressure caveat from
document 12.5 applies: sinking a value extends its operands' live ranges.

Store sinking, moving a store out of a loop or down a branch, is a different and harder problem
that belongs with document 27's store motion.

## 17.5 The interaction that matters

These three, plus CFG simplification in document 21, form a cleanup group, and their ordering has a
fixed point that is worth knowing.

Removing a load can make a store dead. Removing a store can make the value it stored dead. Removing
a value can make a branch's condition dead, but not the branch. Removing a branch (aggressive DCE)
can make a block unreachable. Removing a block can merge two others, which can expose a redundant
load. So the group genuinely cycles.

GCC's response is to schedule the passes several times in `passes.def` and accept the fixpoint is
not reached. Document 04.7 forbids running to a fixpoint and gives the reason: compile time becomes
unpredictable and `--print-pipeline` becomes a lie.

**rucc's response is the same as GCC's and it should be explicit rather than accidental.** The
cleanup group is written out in the level table, twice at `-O2` and once at `-O1`, in the order:
DCE, DSE, simplify-cfg, DCE. Not because that reaches a fixpoint but because measurement on the
corpus says a third round changes almost nothing. That measurement is document 42's and it is
exactly the kind of thing spec 9.10's "a pass must earn its slot" rule is for, applied to a
repetition rather than to a pass.

## 17.6 How this is wrong

**Something with a side effect is deleted.** The whitelist rule is the defence and the test is that
every opcode is classified, with a compile-time exhaustiveness check on the match so a new opcode
does not default to pure.

**An infinite loop is deleted and the program's behaviour changes.** 17.2's rule: latches are
necessary in M4.

**A store is removed that a later `volatile` or atomic load would have seen.** Neither is ever an
"intervening load" that can be ignored, and the memory SSA walk must treat both as full clobbers per
document 09.5.

**A store to a local is removed and the local's address did escape.** This is the escape analysis
bug from document 08.6 arriving where it does damage. A single missed escape here silently deletes
a store that another function reads.

**Trimming produces a misaligned or split store.** Narrowing a 16-byte store to the 3 bytes not
covered is legal and produces terrible code. Trimming should only fire when the surviving part is a
natural width and alignment, which is a cost decision and belongs in document 40.

**Aggressive DCE deletes a branch and mis-redirects it.** Redirecting a deleted branch to its
nearest necessary post-dominator is the step that changes the CFG, and getting the target wrong
produces a program that runs the wrong code and passes most tests. The verifier's reachability and
dominance checks catch some of this; a test suite of CFG shapes catches the rest.

## 17.7 What it costs

Simple DCE is one backwards pass with a worklist. Effectively free, which is why it runs three
times.

Aggressive DCE needs post-dominators and the control dependence relation. The relation is built
from post-dominance frontiers and is sparse per `gcc/tree-ssa-dce.cc:100`. One-off cost per run,
and it runs once.

DSE is one memory SSA walk per store, with the same budget as document 09.2's load walk and for the
same reason.

The measurement in document 42: how much smaller is the IR after each cleanup round. If the third
round removes under 0.5% of instructions, it does not run. That is a number rather than an opinion
and it is how the repetition count in 17.5 gets set.
