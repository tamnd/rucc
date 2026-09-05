# 22. Phi optimization and if-conversion

Turning control flow into data flow. A diamond where both arms compute a value becomes a select; a
diamond where both arms store becomes a conditional store; a loop body full of branches becomes
straight-line predicated code the vectorizer can handle.

This is the highest-variance group of transformations in the compiler. Done right it removes a
mispredicted branch worth twenty cycles. Done wrong it removes a perfectly predicted branch and
forces the machine to execute both arms, which is slower, and it is not detectable without running
the program. Every compiler gets this wrong somewhere and the honest position is that the cost model
is the whole problem and the transformation is easy.

GCC spends 15,289 lines: `gcc/tree-ssa-phiopt.cc` at 4,282, `gcc/ifcvt.cc` at 6,539 and
`gcc/tree-if-conv.cc` at 4,468. Three passes at three different levels for the same idea.

## 22.1 Phiopt, the tree-level version

The input shape is a diamond: a block ending in a conditional branch, two arms, and a join block
whose phi merges values from the two arms. Phiopt matches the shape and rewrites it.

The transformations, from the function names in `gcc/tree-ssa-phiopt.cc`:

**`match_simplify_replacement`** (`gcc/tree-ssa-phiopt.cc:947`), which the source calls "the main
work". The diamond becomes `cond ? a : b`, then the folder is asked whether that simplifies. If it
does, keep it; if it does not, put the branch back. This is the right architecture: the pattern
matching lives in the rule set, and the pass only supplies the shape.

**`value_replacement`** (`gcc/tree-ssa-phiopt.cc:1345`). The case where the condition already
implies the value: `if (x == 0) y = 0; else y = x;` is `y = x` unconditionally. No select needed,
the branch just goes away. This one is unambiguously good, it needs no cost model, and it is
surprisingly common in real code because programmers write the redundant test.

**`factor_out_conditional_operation`** (`gcc/tree-ssa-phiopt.cc:310`). Both arms apply the same
operation to different operands, so the operation moves below the join and the phi merges the
operands. `cond ? f(a) : f(b)` becomes `f(cond ? a : b)`. Halves the code and often exposes further
simplification.

**Min, max, abs.** A diamond comparing two values and selecting one is `min` or `max`. A diamond
testing a sign and negating is `abs`. Both map to single instructions on every target rucc supports.

**`cond_store_replacement`** (`gcc/tree-ssa-phiopt.cc:3002`) and its two-sided cousin
`cond_if_else_store_replacement` (3365). `if (c) *p = a;` becomes `*p = c ? a : *p`, which is a load,
a select and a store, and is only legal when the store is known safe: `p` must be dereferenced
unconditionally somewhere, or otherwise proven valid, or the transformation introduces a store on a
path that had none. This is the same safe-to-speculate predicate as documents 15.6, 16.6 and 27.

**`spaceship_replacement`** (`gcc/tree-ssa-phiopt.cc:1898`) recognises the three-way comparison
idiom. C++-motivated; not M4.

**`hoist_adjacent_loads`** (`gcc/tree-ssa-phiopt.cc:3589`), under `-fhoist-adjacent-loads`, hoists
`p->a` and `p->b` out of the two arms when they are in the same cache line, on the theory that the
second load is free once the first has been issued. Target-dependent and speculative in both senses.
Not M4, noted because it is an unusual heuristic and a good example of a cost model reasoning about
the memory hierarchy rather than about instruction counts.

## 22.2 What rucc builds at the IR level

The shape matcher plus five transformations, in a pass of perhaps 400 lines, at `-O1` and above.

The pass finds a diamond, canonicalizes it into `select` form, and asks the rule engine what it
becomes. `select` is an opcode rucc's IR needs and it is the right lowering target: every backend
has a conditional move or a way to synthesise one, and the rule set can then contain
`select(c, a, a) -> a`, `select(c, 1, 0) -> zext(c)`, `select(c, a, b)` with `a` and `b` constants
becoming arithmetic, and the min/max/abs recognitions, all as ordinary verified rules from document
13.

That is a genuinely better factoring than GCC's, and it is available only because rucc has one
rule engine that both the e-graph and this pass call into. The pass supplies shapes; the rules
supply the transformations. If the rule set later learns a new `select` identity, this pass gets it
for free.

The five things the pass does that the rules cannot, because they are structural:

1. Recognise the diamond and build the `select`.
2. `value_replacement`: consult the branch condition to see whether one arm's value is implied. This
   needs document 10's range machinery on the edge, since "the condition implies `x == 0` on this
   edge" is exactly what the relational oracle in 10.3 records.
3. `factor_out_conditional_operation`: sink a common operation below the join.
4. Conditional store replacement, gated on the safe-to-speculate predicate.
5. Delete the branch and merge the blocks, handing off to document 21.

**And the cost decision, which is the hard part.** Converting a diamond to a `select` is only
profitable when the branch is unpredictable or the arms are trivial. The information available is
document 11's profile: a branch whose probability is near 50% is unpredictable; one at 99% is not.
Without profile data the static predictors give a guess and the guess is often wrong.

rucc's rule for M4, stated so it can be argued with: convert when both arms are empty of side
effects and the resulting `select` costs no more than the branch, which for a `select` of two
existing values means always; and additionally when the arms contain up to two cheap instructions
each *and* the branch probability is between 25% and 75% by document 11's estimate. Above that, keep
the branch. GCC's analogous numbers at the RTL level are `max-rtl-if-conversion-insns` `Init(10)`,
`max-rtl-if-conversion-predictable-cost` `Init(20)` and `max-rtl-if-conversion-unpredictable-cost`
`Init(40)` (`gcc/params.opt:741` onwards), and the fact that GCC uses a cost limit twice as large
for unpredictable branches is the same idea expressed in the other direction.

## 22.3 The other two if-conversions

**RTL if-conversion**, `gcc/ifcvt.cc`. The `noce_try_*` family at `gcc/ifcvt.cc:779` onwards is a
catalogue: `move`, `ifelse_collapse`, `store_flag`, `addcc`, `store_flag_constants`,
`store_flag_mask`, `cmove`, `cmove_arith`, `minmax`, `abs`, `sign_mask`. Each is a way of computing a
conditional value without a branch on a specific class of target instruction.

The reason this exists separately from phiopt is that the answer depends on the target's instruction
set: whether there is a conditional move, whether the condition code is already computed, whether the
target has predicated execution. That is knowledge the tree level does not have.

**rucc's version is document 37's** and it should be the smaller of the two, because the IR-level
pass has already done the shape work and produced `select` nodes. What is left at the machine level
is the reverse question: a `select` on a target with no conditional move must become a branch again,
and the machine-level pass decides which. That is lowering, not optimization, and putting it in
document 36 is defensible; the reason it belongs in 37 is that the decision wants the machine-level
cost model and the scheduler's view of the block.

**Loop if-conversion**, `gcc/tree-if-conv.cc`, is a different thing with a confusingly similar name.
Its purpose, stated at `gcc/tree-if-conv.cc:26`, is "to help the vectorizer to vectorize loops with
conditions". It converts an *entire loop body* into predicated straight-line code by propagating
conditions into a predicate list per block and turning every statement into a predicated one, so the
whole body becomes one block that the vectorizer can process.

This is document 32's and it is post-M4. It is worth separating in one's head from phiopt because
the profitability question is different: loop if-conversion is not profitable on its own at all,
since it makes the scalar loop slower, and it is only ever done because vectorization then pays for
it. GCC accordingly runs it inside the vectorizer's analysis and undoes it if vectorization fails.

## 22.4 The reverse direction: branches from arithmetic

Worth recording because it is a trap. The rule set contains `select(c, 1, 0) -> zext(c)` and similar
identities that turn control flow into arithmetic. It must not contain the reverse. If both
directions exist, the e-graph holds both forms, which is fine, and the *extraction* in document 12.4
chooses by cost, which requires the cost model to price a branch, which requires knowing its
predictability.

That is a real argument for arm C of the e-graph experiment, and it is the second one after document
20.6. Both places want to hold two forms and choose late. Document 12.3's measurement should be
instrumented to report how often extraction actually had a choice between a branch form and a
branchless form, because if the answer is "never" the argument evaporates.

For M4 with arms A or B, the resolution is simpler: the canonical form is branchless, this pass
produces it, and document 37 turns it back into a branch when the target lacks a conditional move.

## 22.5 The short-circuit question

`if (a && b)` is two branches. If `b` is cheap and safe to evaluate, `if (a & b)` is one branch and
one and, and on an unpredictable condition it is much faster.

GCC controls this with `LOGICAL_OP_NON_SHORT_CIRCUIT`, a target macro, because whether it pays
depends on branch cost. Document 19.4's cross-block range test optimization is the same
transformation reached from a different direction: reconstructing the range chain from short-circuit
control flow requires exactly this collapse.

**Two conditions, both mandatory.** The right operand must be safe to evaluate unconditionally,
which means no side effects, no possible trap, and no memory access that could fault. And the
transformation must not be applied when the left operand is a null check guarding the right, which
is the single most common use of `&&` in C: `if (p && p->x)` becoming `if (p & p->x)` dereferences
null. The safe-to-speculate predicate catches this if it is asked, and this is the case that
demonstrates why the predicate must consider memory accesses and not only arithmetic traps.

M4 does the collapse for arithmetic-only right operands, at `-O2`, gated on the branch being
unpredictable by document 11's estimate. Loads on the right-hand side are excluded entirely, which
gives up some of the value and removes the entire class of null-dereference bugs.

## 22.6 How this is wrong

**A store is introduced on a path that did not have one.** Conditional store replacement without the
safety proof. This is the worst bug in this document because it writes memory: the load-modify-store
form writes back the same value it read, which is *not* a no-op if another thread is writing, and is
not a no-op if the page is read-only. The C++ memory model forbids introducing stores to objects
that were not otherwise written, and C's rules are the same in practice. The predicate must require
that the location is unconditionally written on some path through the same region, not merely read.

**A load is speculated and faults.** The `&&` case in 22.5 and the arm-hoisting case. Same predicate.

**A division is speculated and traps.** `cond ? a/b : 0` where `b` may be zero. Speculating the
division executes it unconditionally. Arithmetic that can trap is not safe to speculate and integer
division is the case that matters.

**A `volatile` access is if-converted.** Never. Both the number of accesses and their order are
observable.

**The transformation is applied to a well-predicted branch and the code is slower.** The unfixable
one, in the sense that no static analysis distinguishes a predictable branch from an unpredictable
one reliably. Profile data helps and is not usually available. The defence is conservatism: convert
when the conversion is free or nearly so, and require evidence of unpredictability for anything else.
Document 42 measures the whole thing by turning the pass off on the corpus and comparing, which is
the only honest evaluation available.

**Both arms are executed and one had a side effect.** The whitelist rule from 17.1. Only instructions
in the pure set may be hoisted out of an arm.

**The `select` cost model assumes a conditional move that the target does not have.** Then the IR
looks branchless and document 37 turns it back into a branch, and the result is worse than the
original because the intermediate passes optimized for the wrong shape. The cost model in document 40
must be target-aware here, which is one of the few places in the middle end where it must be.

## 22.7 What it costs

The pass is one walk over blocks looking for the diamond shape, so linear. Each match does a bounded
amount of work. The rule engine call is a hash lookup. The expensive part is the range query for
`value_replacement`, which is document 10's on-demand machinery and is why that machinery is
on-demand.

The interaction with pass ordering: this runs early, before the loop pipeline, so that the loop
passes see straight-line bodies; and again after, because the loop passes create diamonds. Two
instances at `-O2`, which is a refinement to document 03.4's list, and the second one is cheap
because the first has already handled most shapes.

The measurement in document 42: how many diamonds are converted, and separately, the run-time
difference on the corpus with the pass on and off at `-O2`. That second number is the one that
matters and it is the only number in the entire optimizer that this author expects to come out
negative on some benchmark and positive on others, which is a result worth having explicitly rather
than assuming the transformation is good.
