# 23. Jump threading

If, on the path through block A into block B, the condition B tests is already known, then A should
branch directly to B's taken successor and skip B's test. That is the whole idea, and it removes
more branches on real C than anything else in the compiler, because C code is full of conditions
that are redundant along some paths and not along others.

It is also the pass most likely to explode. It works by *duplicating* blocks, which grows the
function, which makes every subsequent pass slower and the instruction cache colder, and the growth
compounds because each thread creates new paths on which further threading is possible. Every
parameter in GCC's threading is a limit.

GCC spends 8,784 lines across `gcc/tree-ssa-threadbackward.cc` (1,081),
`gcc/tree-ssa-threadedge.cc` (1,360), `gcc/tree-ssa-threadupdate.cc` (3,011),
`gcc/tree-ssa-dom.cc` (2,563) and `gcc/gimple-range-path.cc` (769). The split is instructive: two
files find opportunities, one file performs the CFG surgery, one provides the path-sensitive range
solver.

## 23.1 The CFG surgery, which is the part that must be exactly right

`gcc/tree-ssa-threadupdate.cc:36` states the operation precisely enough to implement from:

> Given A->B and B->C, change A->B to be A->C yet still preserve the side effects of executing B.
>
> 1. Make a copy of B (including its outgoing edges and statements). Call the copy B'. Note B' has no
>    incoming edges or PHIs at this time.
> 2. Remove the control statement at the end of B' and all outgoing edges except B'->C.
> 3. Add a new argument to each PHI in C with the same value as the existing argument associated with
>    edge B->C. Associate the new PHI arguments with the edge B'->C.
> 4. For each PHI in B, find or create a PHI in B' with an identical PHI_RESULT. Add an argument to
>    the PHI in B' which has the same value as the PHI in B associated with the edge A->B.

Six steps in total, the remaining two being the redirection of A to B' and the removal of the now
dead argument from B's phis.

**In rucc's IR this is shorter.** B' is a copy of B's instruction list with the terminator replaced
by an unconditional branch to C, carrying the arguments B's terminator would have carried on that
edge. B's block parameters become B''s block parameters. A's terminator is redirected to B' with the
same arguments it passed to B. No step 3 and no step 4, because the arguments live on the edges
rather than in the destination.

That is the third structural payoff from block parameters, after documents 09.3 and 15.1, and it is
the largest of the three: GCC's steps 3 and 4 are the source of a long tail of threading bugs
precisely because phi arguments are indexed by predecessor and every CFG change reindexes them.

**The values must still dominate.** B' is placed where B was, so anything B used still dominates it.
But the values B' *defines* now reach C along a new edge, and C's other predecessors do not see
them. That is handled by C's block parameters, which is to say, by the fact that any value crossing
a join in rucc's IR is already an explicit argument. The verifier's dominance check is the backstop.

## 23.2 Forward threading and the DOM pass

`gcc/tree-ssa-dom.cc` walks the dominator tree recording, on each edge, the equivalences that edge
implies: after `if (x == 5)`, on the true edge, `x` is 5. It uses those to simplify statements as it
descends and, when it reaches a branch whose condition is implied by the recorded equivalences, it
records a threading opportunity.

This is a good design and it is cheap: one dominator tree walk, a stack of equivalences pushed and
popped, constant folding along the way. It finds the common case, which is the same condition tested
twice on a path.

**rucc does not need a DOM pass.** Its two jobs are redundancy elimination, which document 16 covers
with hash-consing and GVN, and edge equivalences, which document 10.3's relational oracle records
per block. What is left is the threading decision, and that is this document's.

So the forward threader in rucc is: walk the dominator tree; at each conditional branch, ask
document 10 whether the condition's value is known given the ranges and relations that hold on the
path from the dominating branch; if it is, register a thread. Perhaps 200 lines on top of machinery
that exists for other reasons.

## 23.3 Backward threading, which is where the value is

The forward version only sees conditions implied by a *dominating* branch. The valuable case is the
one where a condition is determined by several different predecessors in different ways:

```c
if (a) x = 1; else x = 2;
...
if (x == 1) ...
```

Nothing dominating the second test determines it. But along the path through the `then` arm it is
true, and along the `else` arm it is false. Threading both paths removes the second branch entirely.

GCC's backward threader (`gcc/tree-ssa-threadbackward.cc`) starts at the branch and walks
*backwards* through the predecessors, maintaining a set of "imports", the SSA names whose values
determine the branch, and asking the path-sensitive range solver in `gcc/gimple-range-path.cc`
whether the branch resolves along that path. `find_paths_to_names` at
`gcc/tree-ssa-threadbackward.cc:110` is the search and it is a bounded depth-first walk.

This subsumes what used to be called FSM threading, the case of a state machine whose state variable
is a phi of constants, where threading turns an indirect dispatch into a direct one. That is the
single highest-value instance of the transformation and it is why interpreters get much faster when
this pass works.

**rucc builds the backward threader and it is the primary one.** The forward version in 23.2 is
almost free given the dominator walk and finds the easy cases; the backward version finds the ones
worth having. It needs a path-sensitive range query: given a sequence of blocks, what is the range
of this name at the end. Document 10's Ranger is queried per block; the path version is a variation
where the path constrains the incoming edge at each join. GCC implements it as a separate class
(`gcc/gimple-range-path.cc`, 769 lines) over the same range operations, and rucc should do the same,
sharing the operation table from document 10.4.

## 23.4 The limits, which are the design

Every parameter GCC has here is a bound on explosion:

- `max-jump-thread-duplication-stmts` `Init(15)` (`gcc/params.opt:677`): a block with more than
  fifteen statements is not duplicated.
- `max-jump-thread-paths` `Init(64)` (`gcc/params.opt:681`): the backward search space limit.
- `max-fsm-thread-path-insns` `Init(100)` (`gcc/params.opt:601`): total instructions copied along an
  FSM thread path.
- `fsm-scale-path-stmts` `Init(2)` (`gcc/params.opt:161`): statements on a path crossing a loop back
  edge count double.

That last one encodes a real asymmetry: threading across a back edge is more dangerous than threading
within straight-line code, because it interacts with loop structure.

**rucc adopts these values and the fuel mechanism.** Fuel per document 04 already gives a global
bound; these give a local one, and the local one is what stops a single pathological function from
consuming the whole budget.

## 23.5 The loop problem, which is the reason this pass runs where it does

Threading a path that enters a loop other than through its header **creates an irreducible loop**.
GCC checks for this explicitly: `profitable_path_p` takes a `creates_irreducible_loop` output
parameter (`gcc/tree-ssa-threadbackward.cc:776`) and the comment at 787 explains the test, which is
whether the threaded path enters the loop somewhere other than the header, or leaves the latch.

Document 06.4 established that rucc does not perform node splitting and handles irreducible regions
by giving up on them. So rucc's rule is stronger than GCC's: **a thread that would create an
irreducible loop is not performed, at any optimization level.** Not scored down, not permitted with
a warning. Refused.

The second loop interaction: threading through a loop header can convert a rotated loop back into an
unrotated one, or destroy the single-latch property document 07.3 requires. So threading must run
either entirely before loop canonicalization or with the loop forest available and the canonical
properties treated as invariants, exactly as document 21.3 requires of CFG cleanup.

**rucc's placement.** Threading runs twice at `-O2`: once early, before the loop pipeline, when the
CFG is still ragged from the front end and inlining; and once late, after the loop pipeline and
after SCCP has killed edges, when the ranges are best. The late one respects the loop invariants.
GCC runs it four times for the same reasons and the difference is a judgment about diminishing
returns that document 42 measures.

## 23.6 The interaction with everything else

Threading is unusual in that it makes other passes *more* effective by separating paths, and less
effective by duplicating code.

**It helps** value numbering, because a value that was a phi of two things is now two separate
values; range analysis, because ranges are no longer merged at the join; and register allocation,
because live ranges shorten.

**It hurts** code size unambiguously, instruction cache behaviour probably, and compile time
measurably, since every pass after it sees a larger function.

And it interacts with document 12's e-graph in a way worth noting: threading changes the CFG
skeleton, which document 12.4 established the e-graph cannot rewrite. So threading must run outside
the e-graph, and the e-graph must be rebuilt afterwards. That is the same conclusion document 14.5
reached about SCCP and it is becoming a pattern: structural passes run between e-graph rounds, not
inside them.

At `-Os` and `-Oz`, threading is restricted to the case where the duplicated block is empty, which is
pure edge redirection with no growth. That is a small subset and it is the only part that is free.

## 23.7 How this is wrong

**A thread is performed on a path the condition does not actually determine.** The path-sensitive
range query returning a wrong answer produces a branch to the wrong target, which is a
miscompilation with no local symptom. This is the highest-severity failure mode in the document and
it is why the range operations in document 10.4 are SMT-verified: a wrong entry in that table
surfaces here as wrong code rather than as a missed optimization.

**An irreducible loop is created.** 23.5's refusal. Without the check, the loop forest becomes wrong
rather than absent, and every loop pass downstream operates on a lie.

**Block parameter arguments are dropped or duplicated during the surgery.** Mitigated by the IR, per
23.1, and checked by the verifier.

**Profile counts are not split.** When A->B becomes A->B', B's count must decrease by A's
contribution and B' must gain it. Getting this wrong makes every downstream heuristic wrong in a way
that is invisible. Document 11.1's quality tracking means the result is at least marked as degraded,
and document 21.6's verifier check on count sums applies here too.

**The pass runs to a fixpoint.** Threading enables threading. GCC's response is a fixed number of
instances in `passes.def`; rucc's, per document 04.7, is the same. Anything else makes compile time
unbounded on adversarial input, and threading is the pass where adversarial input is easiest to
construct.

**Code size explodes on generated code.** A switch-heavy state machine emitted by a parser generator
is the worst case and it is common. The per-path and per-block limits are the defence and they should
be tested against a real generated parser, not against hand-written code.

## 23.8 What it costs

The forward threader is one dominator tree walk with a stack, effectively free given that document
10's ranges are computed anyway.

The backward threader is a bounded backward search per conditional branch, with a path-sensitive
range query at each step. It is the expensive part, `max-jump-thread-paths` bounds it, and in GCC it
is one of the more visible entries in a `-ftime-report`.

The cost that is not in the pass's own time is what it does to every pass after it. A function that
grows 20% from threading costs 20% more in every subsequent pass. That is the real price and it
argues for threading late rather than early, against which argues the fact that early threading
cleans up the front end's output and makes everything else better. GCC's answer is both. rucc's is
both, twice instead of four times.

The measurement in document 42: the size growth attributable to threading on the corpus, and the
run-time change from turning it off at `-O2`. Both numbers, because this is a transformation where
one improves and the other degrades and the trade needs to be visible.
