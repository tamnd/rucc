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

## 14.8 Where floating point sits in the folding

`crates/rucc-opt/src/fold.rs` stays away from floating point arithmetic, and the reason is not that the arithmetic is hard. `rucc_base::float` computes every operation exactly, in integer arithmetic, correctly rounded. What is missing is the environment: which rounding mode the fold should assume, and what a signalling NaN should do when the answer is computed at translation time rather than raised at run time. Under `-frounding-math` the mode is not the compiler's to assume at all, per document 41's table, and under `-ftrapping-math` the exception is part of what the program does. Those are decisions about the floating point model and they belong with the rest of that work rather than with the first pass in the pipeline.

A conversion from floating point to an integer is inside that boundary rather than an exception to it, and it is folded. 6.3.1.4 says the conversion discards the fractional part, so the rounding is fixed by the language and no dynamic mode reaches it. That leaves two cases where the value is not a number: a result the destination type has no room for, and a NaN. Both are undefined rather than wrong, so any answer would be a valid refinement of poison, and the pass declines to fold either for the same reason it declines to fold an add that overflows under `nsw`. Picking the wrapping answer quietly would hide a program that has stepped outside the language from the sanitizer that should be reporting it.

The conversion in the other direction is not folded and should not be until the model is settled. An integer wider than the significand rounds on the way into a float, so `sitofp` of a large `long` depends on the mode in a way the truncating conversion does not. `fptrunc` is the same case. `fpext` from a narrower IEEE format to a wider one is exact and could be folded on its own terms, and is not worth a special case ahead of the model.

What makes the conversion fold reachable at all is the separate question of what a load from an object nothing can write to evaluates to. The front end has already folded everything C calls a constant expression, so a floating constant meeting a conversion in the middle end is almost always a `const` object somebody read, and neither fold is worth much without the other.

## 14.9 A load from an object nothing can write to

A `const` object with static storage duration and an initializer is bytes the program cannot change, so a load from one at an offset the compiler knows has an answer before the program runs. This is the oldest optimization in the document and rucc did not have it until issue 1358. `crates/rucc-opt/src/load.rs` forwards a load from a store earlier in the same block, which is document 16's block local half, and nothing anywhere looked at what a global was initialized to, so a `const` table read at a constant index kept its load and kept the address arithmetic around it.

Which objects are believed is the same question `crates/rucc-opt/src/extents.rs` answers about sizes, and it is answered in one place rather than two: the object has to be defined here, non-empty, not thread local, external or internal linkage, and not replaceable under the interposition model in force. On top of those four there is one more, that writing through a pointer to it is undefined, which is what puts it in `.rodata`. Nothing asks whether the address is taken or whether anything else can see the object, because the question is what the bytes are rather than who is looking at them, so an exported `const` table folds for its own translation unit and is still in the output for everybody else's.

What the image answers and what it declines to are both worth stating. A run of zeroes answers zero. A scalar answers the value it holds when the access starts where the scalar does and is exactly as wide, which is the case where byte order does not arise, since the image holds a scalar as a value and which end of it comes first is decided when the object file is written. Literal bytes answer the number they spell in the order the datalayout gives, which is the only place byte order is asked about at all. A relocation answers nothing, because the address of another symbol is a promise the linker has not kept yet, and an access crossing from one piece of the image into the next answers nothing either, since bytes spanning two of them are not a value either one holds. A `volatile` load and an atomic load are left alone, because the access happening is the whole point of both.

The placement is the part of this that took a second attempt and is therefore the part worth writing down. A global's image lives on the module and a pass is handed one function, so the obvious arrangement is the one `extents` uses: run over the module once before the pipeline starts, the way the summaries in document 34 are computed. That arrangement folds almost nothing. What the front end writes for `t[2]` is not an offset, it is the index sign extended, multiplied by the element size and added to the address, because the lowering walk writes a subscript the way C defines one. The constant offset only exists after `fold` has run, and a step ahead of the pipeline would miss every array and every string in the program. `extents` can afford that cost because what it loses is a bounds check it did not discharge; here it is the whole optimization.

So it is a pass, `image`, and the module's images reach it on the analysis cache the way the machine cost model does, for the reason `crates/rucc-opt/src/machine.rs` gives: that cache is the one thing every pass is handed besides the function and its fuel, so a fact about the module that a pass needs goes there rather than onto a fourth parameter of every `run`. It runs with a `fold` on each side of it at every level that optimizes, and both neighbours are the position rather than the pass. The one ahead makes the offset exist. The one behind is the half that is easy to leave out: what the pass writes is a constant where a load stood, and standing on top of it is whatever the program did with the value it read, so `(int) one != 1` on a `const double` is a conversion and a comparison that have to fold before the branch passes look at the condition. A condition still spelled as a conversion of a constant is a branch they leave standing, and a branch left standing here is a call to a function the program never calls, which is a program that does not link. It is not in `-O0`, where a load a program wrote is a load a debugger expects to find.

The pair with 14.8 is what `gcc.c-torture/execute/20030216-1.c` is waiting on, and neither half of it is specific to that program. That test reads a `const double`, converts it to `int`, and calls a function nothing defines when the answer is not one, so it links exactly when both folds have happened.
