# 15. Copy propagation and forward propagation

GCC spends 7,796 lines on this cluster: `gcc/tree-ssa-forwprop.cc` at 6,049,
`gcc/tree-ssa-copy.cc` at 633, `gcc/tree-ssa-phiprop.cc` at 607 and `gcc/tree-ssa-uncprop.cc` at
507. Most of it does not need to exist in rucc, and saying precisely why is more useful than
describing what it does, because the reasons are consequences of decisions already made in
documents 12 and 06 and they show those decisions paying off.

## 15.1 Copy propagation, which rucc does not have

`gcc/tree-ssa-copy.cc` propagates `a = b` by replacing uses of `a` with `b`. It exists because
GIMPLE has copy statements: they arise from lowering, from out-of-SSA, from other passes, and from
the front end.

**rucc's IR has no copy opcode.** `crates/rucc-ir/src/opcode.rs:28` lists no `Copy`, `Move` or
`Assign`. There is nothing to propagate. A C assignment `a = b` between locals becomes, after
`mem2reg`, simply the use of `b` where `a` was; there is no instruction in between.

The one construct that produces something copy-like is a block parameter. A block whose parameter
receives the same value on every incoming edge is a redundant parameter, and removing it is exactly
copy propagation on phis. That transformation is one of the things document 12.4 notes Cranelift's
ægraph cannot do, because it changes the CFG's shape. So it is a small conventional pass, perhaps
fifty lines, and it belongs in document 21 with the rest of the CFG cleanups rather than here.

This is the second concrete payoff from block parameters, after document 09.3's memory phis, and
it is worth counting: an entire GCC pass and its 633 lines are structurally absent.

## 15.2 Forward propagation, which is mostly rewrite rules

`gcc/tree-ssa-forwprop.cc`'s header comment describes its own purpose as "basically a specialized
form of tree combination", and adds, at `gcc/tree-ssa-forwprop.cc:64`, "It is hoped all of this can
disappear when we have a generalized tree combiner." That hope is twenty years old.

What it actually does falls into three groups.

**Propagating a value into the branch that tests it.** From the source:

```
x = a COND b;
if (x) ...        =>    if (a COND b) ...

x = a + c1;
if (x EQ c2) ...  =>    if (a EQ (c2 - c1)) ...

x = (typecast) a;
if (x) ...        =>    if (a != 0) ...
```

Every one of these is a rewrite rule in document 13's tiers 3 and 5. Under the e-graph they fire at
construction, they compose without a worklist, and the cascading that
`gcc/tree-ssa-forwprop.cc:131` handles with an explicit re-examination loop happens for free
because operands are rewritten before their users are built.

**Address arithmetic folding.** `ptr = &x->y->z; res = *ptr` becoming `res = x->y->z`;
`ptr = &x[0]; ptr2 = ptr + c` becoming `ptr2 = &x[c/elementsize]`; the index-times-element-size
recognition at `gcc/tree-ssa-forwprop.cc:170`. This is the group that matters most on real C and it
is *not* purely a rewrite-rule problem, because the reassociation of `base + index * scale` into a
form the target's addressing mode can use is target-dependent. Documents 19 and 37 own it: 19 does
the target-independent normalisation of pointer arithmetic, 37 does the folding into addressing
modes at the machine level, where the legal forms are known.

**Vector and builtin special cases.** Roughly half of the 6,049 lines, judging by the includes at
`gcc/tree-ssa-forwprop.cc:45` which pull in `tree-vectorizer.h`, `vec-perm-indices.h`,
`optabs-tree.h` and `tree-ssa-strlen.h`. Permutation folding, string-length propagation, internal
function recognition. Documents 20 and 32 own their respective parts, and most of it is post-M4.

**So the M4 answer to forwprop is: it is not a pass.** Its first group is document 13's rules, its
second is documents 19 and 37, its third is documents 20 and 32. If, after those exist, there is
still a residue that wants a worklist-driven local combiner, that is evidence the e-graph
experiment in document 12 came out badly, and it is worth watching for as a signal.

## 15.3 Phi propagation, which is real and is not covered

`gcc/tree-ssa-phiprop.cc`, 607 lines, does one thing:

```
addr_1 = PHI <&a, &b>
tmp_1  = *addr_1
   =>
tmp_1 = PHI <a, b>
```

Push a load through a phi of addresses. The payoff, as the source explains at
`gcc/tree-ssa-phiprop.cc:76`, is that `a` and `b` may then stop being address-taken, which unlocks
SROA and mem2reg and everything downstream. The worked example is `std::max(std::min(a0, c),
std::min(std::max(a1, c), b))` collapsing into three loads and three min/max operations.

**This one rucc needs and no other mechanism provides it.** It involves memory, so the e-graph will
not touch it (document 12.7 forbids load motion under GCM). It changes block parameters, so it is
structural. And its payoff is not local: the win is that a variable stops escaping, which is an
alias-analysis fact from document 08.4 that unlocks other passes.

The transformation is only legal when the load is executed on every path that reaches it, or when
each incoming load is inserted on its own edge and is known safe there. GCC does a dominator walk
with a per-block local analysis (`gcc/tree-ssa-phiprop.cc:89`). rucc's version:

- The load must post-dominate the phi's block, or the pass inserts the load on each predecessor
  edge and must prove each is safe to speculate. M4 does only the first, which is the common case
  and needs no speculation reasoning.
- Every phi argument must be an address whose base is known: a local, a global, or an offset from
  one. An argument that is an arbitrary pointer gives up.
- Memory SSA must say the same clobbering definition reaches the load along every edge.

Perhaps 150 lines, in the `-O2` pipeline, after SROA and before the second e-graph round so the
newly-unescaped locals get promoted. That is a change to document 03.4's list.

## 15.4 Uncprop, which runs backwards and is worth knowing about

`gcc/tree-ssa-uncprop.cc`, 507 lines, is the strangest pass in this cluster and the most
instructive. It *undoes* constant propagation, late, just before out-of-SSA.

The reason: propagating a constant into a phi argument replaces a value with a literal, which
destroys the coalescing opportunity that would have let the phi's source and destination share a
register. On an edge where control flow has already proved `x == 5`, a phi argument of `5` and a
phi argument of `x` are equivalent, and the `x` form coalesces while the `5` form needs a load
immediate and a copy. So uncprop walks the dominator tree recording edge equivalences
(`gcc/tree-ssa-uncprop.cc:37`) and rewrites constants back into the SSA names that are known equal
to them at that point.

The general lesson, which applies far beyond this pass: **a transformation that is unambiguously
good in the middle end can be bad at the point of register allocation**, and the resolution is not
to weaken the middle-end transformation but to add a late pass that undoes it where the back end's
information says so. This is the same shape as rematerialization, as sinking, and as the tension
document 12.5 notes between GCM and register pressure.

rucc should not build this in M4. It should be recorded as a known follow-up to document 39, to be
built if and only if measurement shows constants in block-parameter arguments causing spills. The
cost of building it speculatively is a pass nobody can justify; the cost of not knowing about it is
diagnosing the spills for a week. That is why it is in this document.

## 15.5 What actually lands in M4

| Transformation | Where it lives | Size |
|---|---|---|
| Copy propagation | nowhere; no copy opcode exists | 0 |
| Redundant block parameter removal | document 21 | ~50 lines |
| Propagate into branch conditions | document 13, rule tiers 3 and 5 | rules |
| Address arithmetic normalisation | document 19 | pass |
| Address folding into addressing modes | document 37 | pass |
| Load through a phi of addresses | this document, 15.3 | ~150 lines |
| Uncprop | not in M4; noted for document 39 | 0 |

One new pass of about 150 lines, against GCC's 7,796. That ratio will not hold everywhere and it
holds here for identifiable reasons: no copy opcode, rewrite rules as data, and an e-graph that
handles cascading structurally.

## 15.6 How this is wrong

**Phi propagation speculates a load.** Pushing a load into predecessors executes it on paths where
it did not execute, which faults if one of those paths had a null or invalid pointer. The M4
restriction to post-dominating loads avoids this entirely; the moment somebody relaxes it, there
must be a "safe to speculate" predicate that considers null, alignment, and whether the object is
known to be at least that large, and that predicate is document 27's, where LICM needs the same
thing.

**Phi propagation forgets `volatile` or an atomic.** Neither is ever moved, per document 9.5.

**The absence of copy propagation hides a problem.** If a later pass introduces a value that is
merely a renaming of another, nothing removes it, because there is no pass looking. The defence is
that hash-consing makes an identical expression the same value automatically, and that any pass
producing a genuine copy is producing something the IR has no opcode for and therefore cannot.

**Forwprop's residue is real and nobody notices.** The failure mode here is silent: patterns GCC
catches in `tree-ssa-forwprop.cc` that rucc's rule set does not, producing slightly worse code
everywhere with no signal. The defence is document 42's comparison against `gcc -O2` on the corpus,
and specifically the practice of diffing the two compilers' output on a function that regressed and
reading it. That is tedious and it is the only thing that finds this class of gap.
