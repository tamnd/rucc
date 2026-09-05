# 14. Constant propagation

The oldest optimization there is, and still one of the three or four that pay for themselves on
every function. It matters here for two reasons beyond the obvious: it is the pass that decides
whether `__builtin_constant_p` says yes, which glibc's headers depend on and which therefore is a
GCC-compatibility question rather than a performance one; and GCC's version is not the textbook
algorithm at all, it is a known-bits analysis that happens to also compute constants, which is a
better design and is not widely known.

`gcc/tree-ssa-ccp.cc` is 3,291 lines and `gcc/tree-ssa-propagate.cc`, the shared propagation
engine, is 1,310.

## 14.1 Sparse conditional constant propagation

The algorithm is Wegman and Zadeck's, cited at `gcc/tree-ssa-ccp.cc:112`. The reason it is better
than iterating a simple forward analysis is in the name: *conditional*. It tracks which CFG edges
are executable, and it does so optimistically, assuming an edge is unreachable until proven
otherwise. That is what lets it prove `a_11 = PHI(a_9, a_10)` is 100 when the branch to `a_9` has a
constant-false predicate, an example the source spells out at `gcc/tree-ssa-ccp.cc:74`.

The optimism is the essential and slightly counterintuitive part. A pessimistic analysis that
assumes every edge executable and iterates to a fixpoint gets a strictly worse answer, because it
can never recover from an initial assumption of `VARYING`. Running constant folding and dead branch
elimination alternately to a fixpoint does not reach the same result either, which is the standard
demonstration that pass ordering cannot substitute for a properly formulated analysis. This is the
strongest single argument in the whole optimizer for the "a pass is an analysis, not a loop"
discipline.

The lattice has four values (`gcc/tree-ssa-ccp.cc:30`): `UNINITIALIZED` as a pass-internal
convenience, `UNDEFINED` meaning not yet known, `CONSTANT`, and `VARYING`. Two shortcuts in the phi
meet, both at `gcc/tree-ssa-ccp.cc:74` onwards: arguments arriving on non-executable edges are
ignored, and `UNDEFINED` arguments are ignored because an uninitialized local may be assumed to hold
whatever is convenient.

That second shortcut deserves a warning. It is standard, GCC has done it for twenty years, and it
is the mechanism by which reading an uninitialized variable produces surprising results rather than
merely garbage. rucc will do the same, and per document 07.5's precedent every such assumption
must be dumpable with the variable named.

## 14.2 The design worth copying: it is really a known-bits analysis

The lattice value is not a constant. From `gcc/tree-ssa-ccp.cc:178`:

> with a CONSTANT lattice value X & ~mask == value & ~mask. The zero bits in the mask cover
> constant values. The ones mean no information.

So the element is a pair `(value, mask)`: a bit whose mask bit is clear is known and equals the
corresponding bit of `value`; a bit whose mask bit is set is unknown. A fully-known value is a
constant. A fully-unknown value is `VARYING`. **Constants are the special case, not the general
one**, and the general case is strictly more useful.

The transfer functions are `bit_value_unop` and `bit_value_binop` at `gcc/tree-ssa-ccp.cc:1339`
and `gcc/tree-ssa-ccp.cc:1505`. They are the standard known-bits rules: `and` clears a bit when
either operand's is known zero, `or` sets it when either is known one, a shift by a known amount
moves the mask, addition propagates unknowns leftward through the carry.

**What this buys over constants alone.** That a value is even, so a division by two is exact. That a
pointer's low three bits are zero, so an eight-byte access is aligned, which is what
`get_value_from_alignment` at `gcc/tree-ssa-ccp.cc:604` extracts. That a value fits in 16 bits, so
a comparison against 100,000 is constant. That a switch operand's top bits are clear, so half the
cases are dead. None of those is a constant and all of them change code.

**And it composes with document 10.** GCC's `irange` carries an `irange_bitmask` for exactly this
reason (document 10.2), and CCP's mask feeds it. rucc should not build two known-bits lattices. The
range analysis owns the representation, and constant propagation is the pass that populates it
through the SCCP fixpoint and then substitutes where the mask is fully clear.

That is a real simplification of spec 9.5, which lists "SCCP" and "bit-CCP" as if they were two
things. They are one analysis with one lattice, and the pass is: run the conditional fixpoint,
write the results into the range analysis, substitute the values that came out fully known, and
mark the non-executable edges for document 21 to delete.

Note also `gcc/tree-ssa-ccp.cc:310`: `ipcp_get_parm_bits` pulls known bits of *parameters* from
interprocedural constant propagation into the local lattice. That is document 34's contribution to
this pass and it is the cheapest interprocedural win available, because a parameter that is always
a small constant is extremely common in real C.

## 14.3 `__builtin_constant_p`, which is a compatibility obligation

GCC folds this in `gcc/gimple-fold.cc:5530` and in `gcc/builtins.cc:8124`, and the interesting
detail is `gcc/ipa-fnsummary.cc:3052`, which special-cases it in the inliner's cost model on the
grounds that its result will always be resolved. So the answer depends on inlining, which depends
on the cost model, which depends on the answer.

Why this is not merely a performance question. glibc's headers, and a great deal of other real C,
are written as:

```c
#if __OPTIMIZE__
# define foo(x) (__builtin_constant_p(x) ? __foo_constant(x) : __foo_generic(x))
#endif
```

`__foo_constant` is often a form that only compiles, or only links, when the argument really is
constant. So answering "no" where GCC answers "yes" is not slower code, it is a build failure or a
link error. Document 03.1 already notes that `__OPTIMIZE__` must be defined under `-Os` for the
same family of reasons.

**The rules rucc must follow.** At `-O0`, `__builtin_constant_p` is always false, which is why the
headers guard on `__OPTIMIZE__`. At `-O1` and above it is true when the argument's lattice value
is fully known at the point of the call, after inlining and after this pass. It is folded here and
not in the front end, because the front end cannot know. And it must be folded *before* the branch
it feeds is eliminated, or the dead arm survives to codegen and fails to compile.

The consequence for the pipeline: constant propagation must run after early inlining, which
document 03.4's `-O1` list already has, and there must be a test compiling a realistic
`__builtin_constant_p` idiom at every level.

## 14.4 What rucc builds

One pass, `sccp`, in the `-O1` and up pipelines, doing four things.

*The fixpoint.* Two worklists, one of CFG edges and one of SSA edges, as Wegman and Zadeck
describe. Optimistic initialization: values `Undefined`, edges non-executable except the entry.

*The transfer functions.* Known bits per opcode, over the same lattice document 10 uses. The M4
opcode set is the same one document 10.4 lists, for the same reason: shared implementation.

*Substitution.* Values whose mask is fully clear become constants. This is where the pass changes
the IR and where it spends fuel.

*Edge marking.* Non-executable edges are recorded for CFG simplification. This pass does not delete
blocks, per document 06.5's rule.

**What it does not do.** It does not fold. `crates/rucc-opt/src/fold.rs` exists, the rewrite rules
in document 13 exist, and constant propagation calling into them for evaluation is right while
constant propagation containing its own arithmetic is duplication. The `evaluate_stmt` function at
`gcc/tree-ssa-ccp.cc:2231` is GCC's version of this delegation and it calls into `match.pd`.

**Where it runs.** Document 03.4 puts `sccp` in the `-O2` list only. That is worth revisiting:
`__builtin_constant_p` must work at `-O1` per 14.3, and only this pass can make it work. So
either `sccp` moves into the `-O1` list, or `-O1`'s `fold` plus rewrite rules must be sufficient for
the common idiom. The former is honest and costs a fixpoint; the latter will break on the first
header that passes the argument through an inlined wrapper, which is most of them.
**Recommendation: `sccp` runs at `-O1`.** That is a refinement to document 03.4 and it should be
folded back into it.

## 14.5 The interaction with the e-graph

Document 12's e-graph subsumes ordinary constant folding: a node all of whose operands are
constant is rewritten at construction. It does not subsume this pass, for a reason worth being
precise about.

The e-graph has no notion of an unreachable edge. Its rewriting is local to a value and its
operands. SCCP's whole power is the global, optimistic, conditional fixpoint, which reasons about
control flow the e-graph explicitly excludes (document 12.1). So the two are complementary and the
ordering in document 03.4's `-O2` list, `egraph` then `sccp` then `gvn`, is right: the e-graph
canonicalizes and folds locally, SCCP propagates globally and kills edges, and the second e-graph
round after the loop pipeline sees the result.

The one thing to guard: after SCCP substitutes a constant, the e-graph's hash-consing table holds
nodes referring to the old value. Either SCCP runs before the e-graph is built, or the e-graph is
rebuilt after it. Rebuilding is cheap and obviously correct; incremental update of a hash-cons
table under value replacement is neither. Rebuild.

## 14.6 How this is wrong

**The optimism is not undone on failure.** An optimistic analysis is only sound if it runs to a
fixpoint. A pass that stops early, for fuel or for a budget, and then substitutes what it has, is
substituting values it optimistically assumed. **Fuel must gate substitution, not propagation.**
The fixpoint runs to completion and then fuel limits how many substitutions are made. This is a
real trap: fuel is threaded through `run` per `crates/rucc-opt/src/pass.rs:21` and the obvious
implementation checks it in the wrong loop.

**`UNDEFINED` is treated as a value.** A value that is still `Undefined` at the end of the fixpoint
is in unreachable code or is genuinely uninitialized. Substituting a chosen constant for it is
legal and produces mystifying behaviour. rucc should substitute zero, dump it, and have a test.

**Known bits are computed with the wrong signedness.** A right shift's mask propagation depends on
arithmetic versus logical. Sign extension fills the mask's high bits with copies of the sign bit's
mask, not with zeros. These are the two entries a from-scratch known-bits implementation gets
wrong, and per document 10.4 they should be SMT-checked along with the range operations.

**`__builtin_constant_p` folds to true and the argument is then not constant.** This happens when
substitution is fuel-limited or when a later pass undoes something. It produces code referencing
`__foo_constant` with a non-constant argument, which fails to compile or links to nothing. The rule:
once `__builtin_constant_p` folds to true, the argument's constancy is a commitment, and the fold
happens in the same transaction as the substitution.

## 14.7 What it costs

One fixpoint over the SSA graph with two worklists. Each value is visited a bounded number of times
because the lattice has finite height: `Undefined` to partially-known to `Varying`, and the mask
only ever gains bits. The bound per value is the bit width, which sounds bad and is not, because a
mask that changes gains bits monotonically and in practice changes twice.

The cost that surprises people is substitution, which walks uses. That is linear and it is the part
fuel gates.

The measurement in document 42: how many values SCCP proves constant that the e-graph's local
folding did not, on the corpus. If that number is small, this pass is not earning its slot at `-O2`
and only `__builtin_constant_p` justifies it at `-O1`. That would be a surprising result, and it is
exactly the sort of thing spec 9.10's rule exists to find out.
