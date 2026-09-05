# 13. The rewrite rules

Spec 9.3 says the rules are data, in a DSL, compiled into a matcher at build time, each carrying an
SMT `spec` clause discharged by `rucc-verify`. That is the right design, GCC arrived at the first
two thirds of it independently, and the last third is the part nobody has and the part that
matters most. This document says what the DSL should contain, what to take from `match.pd`, what
to refuse from it, and how many rules is the right number.

## 13.1 GCC's `match.pd`, which is the best prior art there is

`gcc/match.pd` is 12,293 lines containing 698 `simplify` rules and 121 `match` patterns, compiled
by `gcc/genmatch.cc` (6,475 lines) into a decision-tree matcher shared by GENERIC and GIMPLE. It is
the single most successful piece of compiler-as-data in any production compiler and rucc should
copy its surface almost exactly.

The features, from the source.

**Patterns with captures.** `@0`, `@1` name operands; a capture can be attached to a subexpression,
as in `(rshift@2 @0 INTEGER_CST@1)` at `gcc/match.pd:226`, which names both the shift and its
constant operand.

**Commutativity as an annotation.** `:c` on an operator, as in `(bit_xor:c ...)`, generates all
commutative permutations of the pattern. This is not a small convenience: without it, every
commutative rule has to be written twice, and the second copy is what gets forgotten.

**Predicates.** `define_predicates` at `gcc/match.pd:31` lists the tests a pattern may apply to a
capture: `integer_zerop`, `integer_pow2p`, `tree_expr_nonnegative_p`, `HONOR_NANS` and so on.

**Operator families.** `define_operator_list` at `gcc/match.pd:46` names sets like the six simple
comparisons and their swapped and inverted counterparts, and `for` iterates a rule over a family.
One rule text covers twelve comparison opcodes, and the twelve stay consistent because there is one
text.

**Guards.** `(if cond ...)` wraps a replacement in a condition, and `(with { ... } ...)` binds a
temporary. See `gcc/match.pd:214` for a rule using both.

The output is so large that GCC's build system shards it: `NUM_MATCH_SPLITS` at
`gcc/Makefile.in:227` splits the generated matcher into a configurable number of translation units
because a single one is too big for a C++ compiler to handle comfortably. That is a useful early
warning about what 700 rules costs in generated code.

## 13.2 The one thing to refuse

Look again at `gcc/match.pd:220`:

```
(with { tree utype = unsigned_type_for (TREE_TYPE (@0)); }
 (convert (absu:utype @0)))
```

That is arbitrary C++ inside a rewrite rule, and `match.pd` is full of it. The guard on the same
rule calls `target_supports_op_p`. Rules read target hooks, allocate trees, and call into the rest
of the compiler.

**This is the reason `match.pd` cannot be verified**, and it is the design decision rucc must not
copy. A rule containing an arbitrary host-language escape is a rule whose meaning is not expressible
as a logical formula, and therefore a rule that `rucc-verify` cannot discharge. Once the escape
hatch exists it will be used, because it is always the shortest path, and the verification
obligation quietly becomes optional.

**The rule for rucc's DSL: no host-language escape, at all.** Everything a rule needs is in the
DSL: a fixed set of predicates over captures, a fixed set of constant-arithmetic functions for
computing replacement constants, and access to the analyses in documents 08 and 10 through named
predicates rather than through code. If a transformation cannot be expressed in that language, it
is not a rewrite rule; it is a pass, and it goes in one of documents 14 through 25 where it can be
tested conventionally.

This will be inconvenient perhaps forty times. Each time, the right response is to add a predicate
or a constant function to the DSL, with its own semantics written down and its own verification,
rather than to open the hatch.

## 13.3 The verification obligation

Following Crocus (VanHattum et al., ASPLOS 2024), and per document 05.6's decision 5, every rule
carries a specification and no rule merges without it being discharged.

The obligation for a rule `lhs => rhs` under guard `g` is:

> For all assignments to the captures, at every bitvector width the rule applies to, if `g` holds
> then `rhs` refines `lhs`.

Three words in that sentence are doing work.

**Refines, not equals.** If `lhs` is poison or undefined for some input, `rhs` may be anything. If
`lhs` is defined, `rhs` must equal it. This is Alive2's formulation and it is the only one that is
sound in the presence of `nsw`, `nuw`, and undefined behaviour generally. Stating it as equality
rejects correct rules and, worse, invites people to weaken the checker.

**At every width.** A rule about `x & (x - 1)` is a different formula at 8 bits and at 64. Checking
one width proves nothing about the others. In practice: check exhaustively at 4 bits, check by SMT
at 8, 16, 32 and 64, and treat a rule that is true at 4 and false at 32 as evidence that the
exhaustive check is worth running first because it is fast and produces a concrete counterexample.

**Guards are part of the formula.** A rule guarded on "operand is non-zero" is verified under that
hypothesis, which means the guard's predicate needs a formal meaning too. This is the second
argument against the escape hatch: a predicate implemented as C++ has no formal meaning, so the
guard cannot enter the formula, so the rule cannot be verified.

Rules that reach beyond bitvector arithmetic are the residue: anything about memory, about calls,
about floating point rounding. Floating point is checkable with the SMT theory of floats and it is
slow; memory is not checkable this way at all. Those rules are marked `unverified` in the source,
the mark is visible in `--print-rules`, and the count of them is a number the project reports. A
growing count is a signal.

## 13.4 How many rules

`match.pd` has 698 and LLVM's InstCombine is comparable. Both are the product of twenty years of
"somebody's benchmark got 0.3% faster". Spec 00's framing, that rucc does 30 transformations
properly rather than 90 badly, applies here more than anywhere.

**The M4 target is 250 rules**, and they are chosen by the following order.

*Tier 1, the identities, roughly 80 rules.* `x + 0`, `x * 1`, `x * 0`, `x & x`, `x | x`, `x ^ x`,
`x - x`, `x & 0`, `x | -1`, double negation, double `not`, `x / 1`, shifts by zero, `min(x,x)`,
comparisons of a value with itself. These fire constantly on lowered C because the lowering produces
them, and they are individually trivial and collectively load-bearing. Every one is verifiable in
milliseconds.

*Tier 2, the strength reductions, roughly 40.* Multiply by a power of two to a shift. Unsigned
divide and modulo by a power of two to a shift and a mask. Signed divide by a power of two, which
needs the bias correction and is the classic rule everybody gets wrong on negative inputs. Multiply
by a constant to shift-add sequences up to a bounded cost. Division by an arbitrary constant to a
multiply-high, which document 19 owns because the constant computation is involved.

*Tier 3, canonicalization, roughly 30.* Constants to the right on commutative operators. `x - c`
to `x + (-c)`. Comparisons normalised so the constant is on the right and the predicate is one of
a preferred set. `!(a < b)` to `a >= b` for integers. These do not improve code directly; they
make every other rule need half as many variants, and they are what makes hash-consing in document
12.1 collapse structurally-different-but-equal expressions.

*Tier 4, the width rules, roughly 50.* Truncation and extension algebra: `trunc(zext(x))`,
`zext(trunc(x))` under a mask, extension pushed through arithmetic when it provably does not
change the result, comparisons narrowed to the operand width. This tier is the one that pays on
real C, because C's integer promotions generate extensions on nearly every expression and most of
them are removable. `crates/rucc-opt/src/fold.rs` already gestures at this in its module comment,
where `long y; y + 7` producing a 32-bit constant plus a `sext` is described as costing two
instructions and a register on every wide-integer operation.

*Tier 5, the comparison rules, roughly 30.* Folding comparisons against constants using ranges
from document 10, `(x & c) != 0` patterns, comparison chains, `x < 0` on an unsigned type.

*Tier 6, select and control, roughly 20.* `select(c, x, x)`, `select(true, ...)`, `select(c, 1, 0)`
to a zero-extended condition, min and max recognition, absolute value recognition.

That is 250. Note what is not there: nothing about memory, nothing floating point beyond the
identities that hold without fast-math (which is almost none: `x + 0.0` is not `x` because of
negative zero, and `x * 1.0` is not `x` because of signalling NaN in some modes), nothing target
specific, nothing about calls.

**How the set grows after M4.** By evidence, per spec 9.10's "a pass must earn its slot" rule
applied at rule granularity. Instrument the matcher: count firings per rule per compilation of the
corpus. A rule that never fires on any of it is deleted. Minotaur's approach from document 05.3
gives the other direction: synthesize candidate rules offline against the corpus, verify them, and
add the ones that fire. Its reported 7.3% on GMP suggests this is worth doing properly, once, after
1.0.

## 13.5 The compiler from rules to matcher

Spec 9.3 says the DSL is compiled at build time into a matcher. `rucc-rules` is that compiler and
it produces three things.

**The matcher itself**, a decision tree over opcode and then over operand shape, so that a node is
matched against all 250 rules in time proportional to the depth of the tree rather than to the
number of rules. This is what `genmatch` produces and it is the whole reason the DSL is compiled
rather than interpreted.

**The verification obligations**, one file per rule in SMT-LIB, so that `rucc-verify` is a separate
program run in CI rather than a build dependency. Making verification a build step would mean every
contributor needs a solver installed and every build pays for it; making it a CI step with a
lockfile of discharged obligations, keyed by a hash of the rule text, means an unchanged rule is
not rechecked and a changed one blocks the merge. That lockfile is the same pattern
`gcc-internals`' `citations.lock.json` uses and document 01.1 already relies on.

**A cycle check.** A rule set where `a` rewrites to `b` and `b` rewrites to `a` does not terminate,
and per document 12.7 this is a real hazard under cascades. The check is a graph over rule
left-hand-side and right-hand-side shapes with an ordering: every rule must strictly decrease a
cost measure, or be marked as a canonicalization with an explicit direction. Rules that cannot be
proven decreasing are reported at build time and must be justified in the source.

The output size warning from 13.1 applies: 250 rules generating a decision tree in Rust will be a
large generated file. Sharding it, as GCC does, is a problem for the day the compile time of the
generated crate becomes noticeable, and the DSL compiler should emit multiple files from the start
so that day is a configuration change.

## 13.6 How this is wrong

**A rule is right and its guard is wrong.** The rule for signed division by a power of two is
correct only with the bias; the rule for `x >> c` narrowing is correct only when `c` is less than
the width. Guards are where the bugs are, they are the part the verifier checks least naturally
because they must be encoded rather than derived, and each guard predicate needs its own test that
it means what the SMT encoding says it means.

**The DSL's semantics and the IR's semantics disagree.** The verifier proves a rule against a model
of the IR; the compiler executes it against the real IR. If the model says `shl` by an
over-wide amount is poison and the implementation wraps, every rule about shifts is verified
against the wrong thing. There is exactly one defence and it is that the model is generated from,
or checked against, `crates/rucc-ir`'s own documentation of each opcode's semantics, and that the
divergence is a test.

**Commutative permutation explodes.** `:c` on three commutative operators in one pattern is eight
patterns. GCC has rules that do this. The generated matcher grows and the build slows. Cap the
permutation count per rule and report a rule that exceeds it.

**A rule fires on a poison value and produces a defined one.** This is a *refinement in the wrong
direction* and it is legal (a compiler may make undefined behaviour defined) but it hides bugs from
sanitizers, which is exactly the objection `fold.rs` raises about folding operations that overflow
under `nsw`. The rule set should be consistent about it: rules do not manufacture definedness, and
a rule that would is marked and justified.

**Rules interact.** Two individually-correct rules can cycle, or can fight, one canonicalising in
the direction the other undoes. The cycle check in 13.5 catches the first. The second is caught by
a test that runs the matcher to a fixpoint on a corpus of expressions and reports any that do not
converge within a small bound.

## 13.7 What it costs

Matching is one decision-tree walk per node constructed. With hash-consing in document 12, nodes
are constructed once, so the total cost is proportional to the number of distinct values, which is
the right order.

The costs that are easy to underestimate are the build-time ones: generating the matcher, compiling
the generated crate, and discharging 250 SMT obligations. The first two are paid by every
contributor on every clean build and should be measured; the third is paid by CI and, per document
05.3's note that Alive2 takes about 2.5 hours on LLVM's unit suite, should be budgeted in hours and
run against the lockfile so that an ordinary change checks nothing.
