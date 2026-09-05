# 19. Reassociation and arithmetic

Rewrite rules in document 13 are local: they see one node and its operands. Reassociation is what
you need when the win requires seeing an entire tree of same-operator applications and choosing a
different shape for it. `(a + b) + (c + d)` and `((a + b) + c) + d` compute the same value and are
different programs, and which one is better depends on redundancy, on register pressure, and on how
many execution units the machine has.

GCC spends 7,556 lines in `gcc/tree-ssa-reassoc.cc` and 6,699 in `gcc/tree-ssa-math-opts.cc`. Both
are worth reading in full; the first because its header comment is the most honest piece of design
writing in the tree middle end, and the second because it is where the transformations that turn
divisions into multiplications live.

## 19.1 GCC's five steps

From `gcc/tree-ssa-reassoc.cc:60`, which opens by admitting the pass "is, in part, based on the LLVM
pass of the same name":

1. **Break subtraction into addition plus negation**, where doing so promotes reassociation of the
   adds. `a - b` is `a + (-b)` and only the second form participates in an associative tree.
2. **Left-linearize.** `(a+b)+(c+d)` becomes `(((a+b)+c)+d)`, and the operands land in a flat vector
   of `operand_entry`.
3. **Optimize the operand list.** `a + -a` cancels, `a & a` collapses, repeated factors combine.
   Step 3a folds repeated multiplication into `__builtin_powi`.
4. **Rewrite the tree in rank order.**
5. **Repropagate negates**, because "nothing else will clean it up ATM."

Step 4 is the interesting one and the header spends eighty lines on why it is done the cheap way.

## 19.2 Rank, and the argument against doing it properly

The theoretically nice algorithm, spelled out at `gcc/tree-ssa-reassoc.cc:87`, builds the new tree
from leaves to root, merging operands of equal rank first, so that the maximum number of subtrees
are exposed to the redundancy eliminator as binary operations. Given operands with ranks
`a(1) b(1) c(1) d(2) e(2)`, it forms `a+b` and `d+e` first so both are visible to GVN.

Then, at `gcc/tree-ssa-reassoc.cc:154`:

> So why don't we do this?
> Because it's expensive, and rarely will help. Most trees we are reassociating have 3 or less ops.

With three operands, a single rank comparison picks the better of the two possible shapes, and if
all three ranks are equal no shape exposes more than one pair anyway. So GCC checks the three-operand
case and stops. This is a good instance of a general discipline: measure the distribution of the
input before building the general algorithm.

**What rank is.** `get_rank` at `gcc/tree-ssa-reassoc.cc:414` defines it: globals and uninitialized
values rank 0, function parameters get a pre-set rank, phis and stores and asm take the rank of
their block, and a simple operation takes the maximum rank of its operands capped at its block's
rank. Block ranks increase with depth, so rank is approximately "how deep in loops and how far
downstream was this computed". Sorting operands by rank puts loop-invariant things first, which
groups them so that LICM can hoist the whole group.

**The accumulator exception**, at `gcc/tree-ssa-reassoc.cc:311` and again at 435, is the part that
matters for performance. In

```
x_1 = phi(x_0, x_2)
b = a + x_1
c = b + d
x_2 = c + e
```

each iteration depends fully on the previous one, so the loop runs at the latency of three adds.
Ranking the loop-carried phi *high* pushes it to the end of the tree, giving `x_2 = ((a+d)+e) + x_1`,
where `(a+d)+e` is independent of the recurrence and the loop-carried chain is one add deep. This is
the single largest win reassociation delivers and it is worth building for that reason alone.

It is gated by `reassoc_bias_loop_carried_phi_ranks_p` (`gcc/tree-ssa-reassoc.cc:178`) because it
interferes with reduction chain recognition in the vectorizer, so GCC turns it off before
vectorization and on after. That is a real pass-ordering constraint and document 32 inherits it.

**Width.** `tree-reassoc-width` (`gcc/params.opt:1201`) sets how many operations the rewritten tree
should be able to execute in parallel, defaulting to a target hook. This turns a linear chain into a
balanced tree of the machine's issue width. Related: `avoid-fma-max-bits` (`gcc/params.opt:117`) and
`fully-pipelined-fma` (`gcc/params.opt:168`), which exist because contracting `a*b+c` into an FMA
lengthens the dependence chain even though it removes an instruction.

## 19.3 What rucc builds

**A reassociation pass, not rewrite rules.** Document 13's rule engine cannot express this: the
input is an arbitrary-arity tree and the output shape depends on a global rank function. It is a
pass of perhaps 400 lines and it belongs at `-O2` and above.

The M4 version does steps 1, 2, 3 and a restricted 4:

*Linearize.* For `+`, `*`, `&`, `|`, `^`, `min`, `max` on integers, collect the maximal tree of
same-operator nodes whose interior values have a single use. The single-use restriction is what stops
linearization from duplicating work; a shared subexpression is a leaf.

*Canonicalize subtraction.* `a - b` to `a + neg(b)` inside the tree only, and back out at the end.
The e-graph would otherwise oscillate between the two forms, which is exactly the cycle check
document 13.5 requires the rule compiler to catch, and it is why this is a pass rather than a rule.

*Cancel and combine.* `a + neg(a)` to zero, `a & a` to `a`, `a ^ a` to zero, repeated constants
folded into one, repeated operands of `+` into a multiply. This is cheap once the operand list is
flat and it is the step that finds things no local rule can see, because the two cancelling operands
may have been fifteen nodes apart in the original tree.

*Sort by rank and rebuild.* Rank as GCC defines it, including the loop-carried phi bias. Rebuild
left-associated, with the three-operand special case, and with the width heuristic producing a
balanced tree when the target's issue width justifies it. The width value comes from document 40.

**What is deliberately left out.** `__builtin_powi` generation and the whole floating-point half.
Reassociating floating-point arithmetic is illegal without `-ffast-math` because it is not
associative, and rucc's M4 target is scalar integer and pointer code per spec 00's code-quality axis.
The pass checks the type and refuses. The gate exists in GCC as `flag_associative_math`
(`gcc/tree-ssa-reassoc.cc:672`) and rucc's version is: integer types and `-fassociative-math` never
being on in M4 means the float path is simply absent.

## 19.4 Range tests, which is reassociation in disguise

`optimize_range_tests` and its subroutines occupy roughly 1,600 lines of
`gcc/tree-ssa-reassoc.cc` starting at line 2496, and they do something that does not sound like
reassociation until you see it: a chain of comparisons joined by `&&` or `||` is an associative tree
over `and`/`or`, and the operands are range predicates.

```c
if (x == 1 || x == 2 || x == 3 || x == 7)
```

is four comparisons and three branches. As ranges it is `x ∈ [1,3] ∪ {7}`, and `x ∈ [1,3]` is
`(unsigned)(x - 1) <= 2`, one subtract and one compare. The `{7}` case can join via a bit test:
`(1u << x) & 0b10001110` when `x` is known small, which is one shift and one and.

This is a large win on real C, because parser and lexer and state-machine code is written exactly
this way, and it is why `maybe_optimize_range_tests` also works *across basic blocks*
(`gcc/tree-ssa-reassoc.cc:2793`), reconstructing the range chain from a series of short-circuiting
branches that the front end already lowered into control flow.

**rucc's position.** The single-block version, over an `and`/`or` operand list of comparisons
against a common operand, is in M4 and is worth roughly 250 lines. It needs document 10's range
representation, which already stores multi-interval sets, so "merge these comparison ranges and
re-emit" is a use of existing machinery rather than new machinery. The cross-block version, which
requires undoing short-circuit lowering, is post-M4 and is noted in document 22 alongside the other
branch-to-arithmetic conversions, since it is the same legality question: is it safe to evaluate the
right-hand operand unconditionally.

The bit-test form has a hard precondition: `x` must be known to be less than the word width, or the
shift is undefined. Document 10's ranges supply that, and where they do not, the transformation is
not made. This is the sort of thing that is correct by construction if the range query is consulted
and a silent wrong-code bug if the width is assumed.

## 19.5 Division and the rest of math-opts

`gcc/tree-ssa-math-opts.cc` covers a family of transformations that share only the property of being
about arithmetic identities the target cares about. Three matter for M4.

**Division by a constant.** `x / 7` becomes a multiply by a magic number and a shift.
`choose_multiplier` at `gcc/expmed.cc:3728` computes the constant, following Granlund and Montgomery,
and `expand_divmod` at `gcc/expmed.cc:4262` selects among the variants. This is the single highest
value arithmetic transformation there is, because integer division costs twenty to forty cycles and
the replacement costs three or four.

It belongs in rucc at the machine level, in document 37, not here, for a reason worth stating: the
correct sequence depends on whether the target has a high-multiply instruction, on the register
width, and on whether the divisor's magic number needs an extra add to avoid overflow. It is a
lowering decision, not a canonicalization. What document 19 does is ensure the divisor is a constant
when it can be, which is document 14's job anyway.

The signed case is the one that gets written wrong. `x / 2` is not `x >> 1` for negative `x`, because
C truncates toward zero and a shift rounds toward negative infinity. The correct sequence adds a bias
first. Every compiler has had this bug; rucc's defence is that the division rules are in document
13's SMT-verified rule set with the sign case in the formula, and that document 37's lowering is
covered by an exhaustive test over small widths.

**Reciprocal replacement.** The header at `gcc/tree-ssa-math-opts.cc:20` explains it with a worked
example: several divisions by the same value become one reciprocal and several multiplies. It notes
two constraints that generalise beyond floating point. First, the transformation is not worth doing
for only two divisions because modern processors pipeline them, which is a parameter. Second, with
trapping math active, the reciprocal can only be inserted in blocks that already contain a division,
because otherwise it introduces a trap on a path that had none. That second point is the
safe-to-speculate predicate from document 16.6 appearing again, and it should genuinely be one shared
predicate.

Floating point, so not M4. Recorded because the integer analogue, replacing several divisions by a
common non-constant divisor with one division and multiplies, is not valid, and it is worth knowing
why: integer division truncates, so `(a/d)` and `a * (1/d)` are not related by any exact identity.
There is no integer reciprocal trick for a runtime divisor. The only integer win is when the divisor
is constant.

**Widening multiply and multiply-accumulate recognition.** Recognising that a 32-by-32 multiply
whose result is used at 64 bits is a widening multiply, and that `a*b+c` is one instruction. Both
are target-dependent pattern matches over the IR and both belong to document 37.

## 19.6 Pointer arithmetic normalisation, which document 15.2 assigned here

Document 15.2 split GCC's forwprop address folding in two and gave the target-independent half to
this document. That half is:

`base + i*scale + c1 + c2` in any association becomes a canonical `base + (i*scale) + C` with the
constants summed and the constant last. `&a[0] + n` becomes `&a[n]`. `(&x->y)->z` becomes a single
offset from `x`. A `ptr_add` of a `ptr_add` collapses.

This is genuinely reassociation, over `+` on the pointer-offset domain, and it uses the same
linearize-cancel-sort machinery with one extra rule: exactly one operand may be a pointer, and it
sorts first. That constraint is what makes the result well-typed and it is checkable in the
verifier.

The payoff is not the saved add. It is that document 37 can then match an addressing mode against a
canonical shape rather than against seven equivalent shapes, and that document 28's induction
variable analysis sees `base + i*scale` in the form it recognises. Both of those consumers want the
same normal form, which is the argument for doing it once here rather than twice there.

One caution: sinking the constant to the outside is right for addressing modes and wrong for
overflow reasoning, because `(p + a) + b` and `p + (a + b)` differ when `a + b` overflows the offset
type. rucc's `ptr_add` takes a target-width offset and the normalisation is only performed when the
constants combine without overflow at that width. Checked, not assumed.

## 19.7 How this is wrong

**Reassociating something that is not associative.** Signed overflow makes `+` associative only
under wrapping semantics, and rucc's `nsw` flag marks operations where overflow is undefined.
Reassociating a tree of `nsw` adds is legal precisely because overflow is undefined, so the compiler
may assume it does not happen, but the *result* nodes must then carry `nsw` consistently, or a later
pass draws a conclusion from a flag the reassociated form does not justify. The rule: reassociation
either preserves the flags on every node of the tree or drops them from every node. Mixed is a bug.

**Floating point sneaks in.** The type check is the whole defence and it should be an assertion, not
a branch.

**Linearization duplicates a shared subexpression.** The single-use restriction. Without it, a value
used twice gets its tree flattened into both users and the work doubles.

**The rank function is not deterministic.** Rank depends on block numbering, which depends on the
CFG traversal order, which per document 04 must be deterministic. If rank ever depends on a hash
iteration order, the compiler produces different output on different runs, which spec 00's
determinism requirement forbids and which is miserable to diagnose.

**Cancellation removes a trap.** `a + neg(a)` folds to zero; if `neg(a)` would have trapped on
`INT_MIN` under `-ftrapv`, it no longer does. rucc does not implement `-ftrapv` in M4, and the
absence should be recorded rather than discovered.

**The bit-test range optimization shifts by more than the width.** Covered in 19.4 and repeated here
because it is the one wrong-code bug this document is most likely to produce.

**Reassociation fights the e-graph.** The pass rewrites the IR into a shape the e-graph's rules may
immediately rewrite back. `a + neg(b)` versus `a - b` is the obvious pair. The resolution is that the
rule set does not contain both directions, that `a - b` is the canonical form at rest, and that
reassociation's subtraction breaking is internal to the pass and undone before it finishes. If the
e-graph is rebuilt after reassociation, as document 14.5 requires it to be after SCCP, this is
mechanical rather than delicate.

## 19.8 What it costs

Linearization is one walk per associative root, and roots do not overlap because interior nodes are
single-use. Sorting is `n log n` in operand count, and the header comment's observation that most
trees have three or fewer operands means `n` is small in the overwhelming majority of cases.

Cancellation is quadratic in operand count in the naive implementation. With trees of size three
that does not matter; with a generated expression of two hundred terms it does, so the operand list
is hashed by value number, which document 12's hash-consing already provides for free.

Range test optimization is linear in the operand list plus a merge of intervals, which document 10's
representation supports directly.

The measurement in document 42: the fraction of `-O2` time spent in reassociation, and separately,
the effect of the loop-carried phi bias on the loop-heavy part of the corpus. That second number is
the pass's justification. If it is under 1%, most of this document is 400 lines that did not earn
their slot, and the range-test half should be extracted into its own pass and the rest deleted.
