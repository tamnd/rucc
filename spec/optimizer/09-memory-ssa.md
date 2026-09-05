# 09. Memory SSA

Spec 9.4 asks for "Memory SSA, in LLVM's style", built at `-O2` and above. This document says what
that means concretely, and it opens by disposing of a misconception: GCC has this too, has had it
since 2004, and calls it something else. Understanding that the two designs are the same idea with
one difference makes the design decision much smaller than it looks.

## 9.1 GCC's version, which is virtual operands

A GIMPLE statement that reads memory carries a **VUSE** and one that writes memory carries a
**VDEF**, and both are SSA names of a single artificial variable called `.MEM`
(`gcc/tree-ssa-operands.cc:239`). Memory is one variable. Every store defines a new version of it,
every load uses a version, and the ordinary SSA machinery in `gcc/tree-into-ssa.cc` puts phi nodes
for `.MEM` at joins exactly as it would for any other variable. `gcc/tree-ssa-operands.cc` is 1,438
lines and most of it is bookkeeping for keeping the operand caches in step.

That is LLVM's MemorySSA. `MemoryDef`, `MemoryUse`, `MemoryPhi` are VDEF, VUSE and the `.MEM` phi
under different names. The idea in both is the same and it is the right one: *reuse the scalar SSA
machinery for memory by pretending memory is one scalar*, then recover precision by asking the
alias analysis whether a particular def actually affects a particular use.

The consequence, in both compilers, is that the def-use chain over memory is maximally
conservative. Every store kills every load, structurally. Precision comes entirely from *walking*.

## 9.2 The walk, and its budget

`walk_non_aliased_vuses` at `gcc/tree-ssa-alias.cc:3915` is where the real work happens. Given a
reference and the version of `.MEM` a load sees, it walks backwards through defs, asking the alias
analysis at each one whether that def could have written the reference, and stops at the first one
that could. The result is the *clobbering definition*: the store this load actually sees.

Two parts of its interface are worth copying.

**`translate`.** When the walk hits a def it cannot see past, the caller gets a callback and may
*adjust the reference* and continue. This is what lets value numbering follow a load through a
`memcpy` by rewriting the reference to the copy's source, and it is the mechanism behind a
surprising fraction of GCC's memory optimization. Without it the walk is a stopping condition;
with it, it is a way to rewrite the question.

**`limit`.** The walk is budgeted, and the budget is a parameter: `sccvn-max-alias-queries-per-
access`, default 1000 (`gcc/params.opt:1020`). Exceeding it returns "unknown", not a wrong answer.

That budget is the single most important design fact in this document. The walk is worst-case
quadratic: every load can walk back through every store, and each step is an alias query. In a
function with a thousand loads and a thousand stores and no disambiguation, that is a million
alias queries per pass that uses it, and there are four such passes. Compilers that skipped the
budget discovered this as a pathological compile-time bug report, and both GCC and LLVM ended up
with one.

Note the TODO at `gcc/tree-ssa-alias.cc:3911`: "Cache the vector of equivalent vuses per ref, vuse
pair." GCC has not done it. LLVM has, in the form of the caching walker, and that is the one real
difference between the two implementations. It is also a large part of LLVM's MemorySSA
complexity and a well-known source of subtle invalidation bugs.

## 9.3 What rucc builds

**The representation.** Memory is a value like any other. Each memory-writing instruction produces
a memory value; each memory-reading instruction consumes one; joins take a memory block parameter.
rucc's block parameters make this cleaner than either GCC or LLVM manage, because the memory phi
is not a special node type: it is an ordinary block parameter of a distinguished type.

That is worth dwelling on. In GCC, `.MEM` phis are real GIMPLE phi statements and every pass that
manipulates the CFG must know about them. In LLVM, `MemoryPhi` lives in a side table indexed by
block and must be updated in lockstep with the CFG. In rucc, if memory is a value of type `mem`
and block parameters already exist, then splitting an edge or deleting a predecessor updates
memory SSA by the same code that updates everything else, and there is no side table to get out of
step. This is the second time block parameters have paid for themselves, after document 06.1.

**The cost of that choice** is that memory becomes an explicit operand and result on every load,
store, call and atomic in the IR, which is one extra `Value` per memory instruction. On a function
that is 30% memory operations that is perhaps 5% more IR. It also means `-O0` and `-O1`, which do
not build memory SSA, need memory operations that do not carry it, so the memory operand is
optional and its absence means "unordered with respect to everything, ask the alias analysis
directly".

The alternative, a side table from instruction to memory version, keeps the IR smaller and
reintroduces exactly the update problem block parameters were meant to solve. Take the explicit
operand.

**The walk.** As GCC's, with the `translate` callback and with a budget. The budget is a parameter
with the same name GCC uses, because a user who knows to raise `sccvn-max-alias-queries-per-access`
should not have to learn a second name.

**The cache, eventually.** Build the uncached walk first, instrument how many alias queries a
`-O2` compilation makes, and add caching only if that number is a measurable fraction of compile
time. GCC has run without it for twenty years. The instrumentation is the deliverable in M4; the
cache is not.

## 9.4 What it is for

Four consumers and they are the entire justification.

| Consumer | Document | What it asks |
|---|---|---|
| Redundant load elimination | 16 | what store does this load see |
| Dead store elimination | 17 | is any load reachable from this store |
| PRE over memory | 16 | is this load available on all paths |
| LICM of loads | 27 | does anything in the loop write this |

Every one of those is quadratic without memory SSA and linear-ish with it. That is the whole
argument, and it is why spec 9.4 puts memory SSA at `-O2` and not below: `-O1` does not run PRE
and does not need it.

`gcc/tree-ssa-sccvn.cc` at 9,283 lines and `gcc/tree-ssa-pre.cc` at 4,730 are the two big
consumers, and `gcc/tree-ssa-dse.cc` at 1,816 is the third. The ratio is informative: the analysis
is 1,438 lines of representation plus a few hundred of walking, and the consumers are 15,800.
Memory SSA is a small piece of infrastructure that makes large pieces possible, which is the
correct shape for infrastructure.

## 9.5 The hard parts

**Calls.** A call with unknown behaviour defines memory, full stop, and every load after it walks
back only as far as the call. `const` and `pure` attributes, known libcalls, and escape analysis
from document 08.4 are what stop this from ending memory optimization at the first `printf`. A
`memcpy` is the interesting case: it defines memory, but the `translate` callback can see through
it, so the walk continues with a rewritten reference. Getting `memcpy` right is worth more than
any other single case because it is what struct assignment lowers to.

**Atomics and fences.** An acquire load orders subsequent memory operations; a release store
orders prior ones; a `seq_cst` fence orders everything. In memory SSA these are defs that nothing
walks past, and the conservative implementation is to treat every atomic and every fence as a full
memory def and a full memory use. That is correct and it is what M4 should do. It is also
pessimistic in a way that matters for lock-free code, and doing better means modelling the memory
model rather than the memory, which is post-1.0 and belongs with `spec/07-semantics.md`.

The failure mode to guard against: treating a relaxed atomic load as an ordinary load because it
does not order anything. It does not order, but it is still a load, and hoisting it out of a loop
changes an observable. Atomics are never moved. There is a bit on the instruction and every pass
checks it.

**`volatile`.** Never moved, never eliminated, never duplicated, never merged. Per document 08.1
two volatile accesses always conflict. The temptation is to treat `volatile` as a strong alias
fact and let the ordinary machinery handle it; the reason not to is that alias analysis says
nothing about *how many times* an access happens, and `volatile` constrains that too. A separate
bit, checked before anything else.

**`setjmp` and computed gotos.** A block that is a `setjmp` return point can be reached with
memory in a state no dominator analysis predicts. GCC handles this with `ABNORMAL_DISPATCHER`
edges. rucc needs the equivalent: the CFG must contain the edges, so memory SSA's phis land in the
right places, and document 06 must build them.

**Partial overlap.** A four-byte store followed by a one-byte load at offset 1. The load sees the
store, but not all of it, and it cannot be replaced by the stored value without extraction. The
walk must distinguish "definitely clobbers, and exactly covers this reference" from "definitely
clobbers, partially" from "may clobber", which is three answers and not two. Getting this to two
answers is a class of miscompilation.

## 9.6 How this is wrong

**A pass adds a store and does not thread memory through it.** With an explicit operand this is a
verifier error, because the new instruction has no memory input and its type demands one. That is
the main argument for the explicit operand over the side table, restated: the verifier catches the
mistake at the pass that made it.

**The budget is hit and the caller treats "unknown" as "no clobber".** The walk's return type must
make this impossible: `Clobber(Inst)`, `NoClobber`, and `Unknown` as three distinct variants, with
no `Option` anywhere and no default arm.

**A `translate` callback rewrites the reference incorrectly**, so the walk continues past a store
that did affect the load. This is the subtlest bug available in this document and it is
essentially untestable by unit test. The defence is differential execution per document 41: a
corpus compiled at `-O0` and `-O2` producing different output localises to a pass, and fuel
bisection localises to a site.

**Memory SSA is stale.** Per document 04.4 it is invalidated by any memory operation added or
removed, which is nearly every value-level pass. Its rebuild cost is therefore not incidental and
9.7 measures it.

## 9.7 What it costs

Construction is one pass over the function to place memory block parameters, which is the same
iterated dominance frontier computation SSA construction uses, plus a walk to thread the operands.
Linear with a dominance-frontier factor.

The walk is the cost, and it is charged to the consumers rather than to the analysis, which means
`-ftime-report` will show it under GVN and PRE. That is misleading and document 42 should fix it:
report alias query count and memory walk steps as their own counters, separate from wall time,
because they are the thing to look at when a pathological input shows up.

The number that decides whether the cache from 9.3 gets built: alias queries per `-O2`
compilation of the SQLite amalgamation, and the fraction of walks that terminate by exhausting the
budget rather than by finding a clobber. If more than 1% of walks hit the budget, the budget is
too small or the alias analysis is too weak, and both of those are better fixed than cached
around.
