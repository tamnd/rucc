# 10. Value ranges

Knowing that a value lies in `[0, 63]` is what removes a bounds check, narrows a 64-bit multiply to
32 bits, proves a shift count in range, folds a comparison, and tells the switch lowering in
document 24 that four of the eleven cases are unreachable. It is the analysis that most often
turns out to be the reason a transformation fires, and it is also the analysis whose GCC
implementation changed most in the last decade, so the historical folklore about it is unusually
misleading.

GCC 16 spends about 18,400 lines here: `gcc/range-op.cc` at 5,293, `gcc/value-range.cc` at 3,696,
`gcc/gimple-range-cache.cc` at 1,893, `gcc/value-relation.cc` at 1,883,
`gcc/gimple-range-gori.cc` at 1,737, `gcc/gimple-range-fold.cc` at 1,693, `gcc/gimple-range.cc` at
957, `gcc/gimple-range-path.cc` at 769 and `gcc/gimple-range-infer.cc` at 523.

## 10.1 The architectural fact worth stealing: it is on demand

The old design, and the one every textbook describes, is a forward dataflow propagation: initialise
every value to the empty range, iterate to a fixed point over the CFG, store a range per SSA name.
That is what GCC's VRP was until roughly GCC 11 and it has two problems. It computes ranges for
everything when the consumer wanted three of them, and the range it stores is the range at the
*definition*, whereas the useful question is almost always the range at a *use*, which can be much
narrower because of the branches in between.

Ranger inverts it. You ask `range_of_expr (r, name, stmt)`: what is the range of this name at this
statement. The machinery walks *backwards* from there, through the def chain and through the
branch conditions that dominate the statement, computing only what the question needs.

The piece that makes it work is **GORI**, Generates Outgoing Range Info (`gcc/gimple-range-gori.h`
and `gcc/gimple-range-gori.cc`). Given the edge out of `if (x + 3 < 10)`, GORI works out not just
that `x + 3` is in `[INT_MIN, 9]` on the true edge but that `x` is in `[INT_MIN, 6]`, by inverting
the operations along the chain from the condition back to `x`. The `range_def_chain` class at
`gcc/gimple-range-gori.h:29` records, per name, which names it depends on, so the inversion knows
where it can go.

This is bounded by two parameters, and their existence is the tell that the walk would otherwise
be unbounded: `ranger-logical-depth` at `Init(6)` (`gcc/params.opt:998`) caps how deep into logical
expressions the edge calculation looks, and `ranger-recompute-depth` at `Init(5)`
(`gcc/params.opt:1003`) caps chain recomputation.

**rucc takes the on-demand design.** It fits document 04.3's analysis manager exactly: a range
query is a method on the analysis, the analysis caches per name per block, and the cache is
dropped when the CFG or any arithmetic changes. Building a forward propagation instead would mean
computing ranges for the 90% of values nobody asks about, and rucc has fewer consumers than GCC
does, so the ratio is worse.

## 10.2 The representation, which has three parts and not one

This is where GCC's design is better than the obvious one and where rucc should follow closely.

**Intervals, plural.** `irange` (`gcc/value-range.h:288`) is a *union of disjoint sub-ranges*, not
a single `[min, max]`. `num_pairs()` says how many, and `int_range<N>` at `gcc/value-range.h:385`
stores `N` pairs inline. This matters more than it seems: `x != 0` gives `[INT_MIN, -1] union
[1, INT_MAX]`, which a single interval cannot express and which therefore degrades to "everything"
in a single-interval implementation. Non-zero is the single most useful range fact in a C compiler,
because it is what a null check produces and what division needs.

rucc should carry a small fixed number of pairs, two or three, with anything more collapsing to
the hull. Three covers `x != 0`, `x != 0 && x != 1`, and switch case exclusions, and unbounded
pair counts are how a range implementation becomes a memory problem.

**Known bits, alongside.** `irange_bitmask` at `gcc/value-range.h:149` carries a value and a mask,
where a set mask bit means "unknown" and a clear one means the corresponding value bit is known.
GCC keeps this *on the same object as the interval*, and the two refine each other: knowing the
low three bits are zero tells you the value is a multiple of 8, which narrows an interval; knowing
the interval is `[0, 15]` tells you the top bits are zero.

Keeping them together is the right call and it is the thing a from-scratch implementation gets
wrong, by building a range lattice and a separate known-bits lattice and never letting them talk.
`range_from_mask` (`gcc/value-range.h:169`) is the bridge in one direction and the constructor
taking a min and max is the bridge in the other.

**Pointers are separate.** `prange` at `gcc/value-range.h:402` is a distinct class from `irange`,
which is a GCC 14-era change. A pointer range is mostly about null and non-null and about
provenance, and forcing it through integer interval arithmetic produces nonsense like a pointer in
`[0x1000, 0x2000]` that no target guarantees. rucc should have the same split, and its pointer
range should carry the provenance identifier from document 08.2, which makes the range analysis a
second source of alias facts for free.

Floats get `frange` at `gcc/value-range.h:547`. rucc should skip float ranges in M4 entirely: the
interesting facts about floats are NaN-ness and sign, the consumers are few, and the correctness
traps around signed zero and NaN comparison are numerous. Document 19 says what little rucc does
with float facts.

## 10.3 The relational oracle

`gcc/value-relation.cc`, 1,883 lines, tracks that `a < b` without knowing the range of either. This
is a genuinely different kind of fact and it is not derivable from intervals: if `a` is
`[0, 100]` and `b` is `[0, 100]`, intervals say nothing about `a < b`, but if control flow proved
it, it is true.

It is what lets GCC remove the second comparison in `if (a < b) { ... if (a < b) ... }` after
intervening code, prove a loop terminates, and eliminate bounds checks where the bound is a
variable.

**rucc should have this and should keep it minimal.** A map from ordered pairs of values to a
relation in the six-element lattice `{<, <=, ==, !=, >=, >}`, populated from branch conditions,
queried by the fold rules. It is not transitive-closed, because closing it is where the cost goes;
it answers what was directly recorded, plus a single step of composition. Recording that `a < b`
on a branch is nearly free and the payoff on bounds-check-heavy code is large.

## 10.4 The per-opcode range operations

`gcc/range-op.cc` at 5,293 lines is the largest single file in this area and it is entirely
mechanical: for each opcode, given ranges for the operands, compute the range of the result, and
also, given the range of the result and one operand, compute the range of the other, which is what
GORI's inversion needs. Two directions per opcode.

There is no clever way to write this. It is a table, it must be right for every opcode including
the wrapping cases and the shift-by-more-than-the-width cases, and every entry is a small
correctness proof.

**This is where document 13's verification technique should be applied outside document 13.** A
range operation is a claim of the form "for all `x` in `R1` and `y` in `R2`, `op(x, y)` is in
`R3`", and that is an SMT query at bitvector width 8 or 16, exhaustively checkable at width 4. The
same `rucc-verify` machinery that discharges rewrite rules discharges range operations, and it
should, because a wrong range operation is a silent miscompilation and there are a hundred of them.

The M4 subset: add, subtract, multiply, the bitwise operations, shifts, the comparisons,
truncation, sign and zero extension, and negation. Not division, not remainder, not the overflow
builtins, not the intrinsics. That is perhaps twenty opcodes forward and twelve invertible, and it
covers everything the consumers in 10.5 ask about.

## 10.5 Consumers, and the honest count

| Consumer | Document | Fact wanted |
|---|---|---|
| Conditional constant propagation | 14 | is this condition constant here |
| Fold rules | 13 | is this operand non-zero, non-negative, in range for a narrower type |
| Switch lowering | 24 | which cases are reachable |
| Induction variables | 28 | does this IV wrap; can it be narrowed |
| Dead code elimination | 17 | is this branch never taken |
| Bounds and null checks from builtins | 20 | is this pointer non-null; is this size known |

Six, and the first two are most of the value. That is the argument for the M4 subset in 10.4: the
consumers that exist do not ask about division ranges.

## 10.6 The scaling problem GCC solved with parameters

`gcc/params.opt:1302` sets `vrp-block-limit` to 150,000: above that many basic blocks, VRP
switches to a lower-memory model. `vrp-sparse-threshold` at `Init(3000)` (`gcc/params.opt:1310`)
switches the cache to a sparse bitmap above 3,000 blocks. `vrp-cstload-limit` at `Init(32)` caps
inference from constant aggregates.

Those numbers are a record of real bug reports. The generalisable lesson, and the one rucc should
internalise before it has the bug reports, is that a per-name per-block cache is O(names x blocks)
and that product is quadratic in function size. A generated parser with 40,000 blocks and 100,000
names is not hypothetical; it is what `bison` output looks like, and it is in every C compiler's
test corpus for exactly this reason.

**rucc's version:** the cache is per name, holding a range at the definition plus a small map of
block-specific refinements, and the map is bounded. When it would exceed the bound, the query
falls back to the definition range, which is correct and less precise. One parameter, one
threshold, and a counter in `-ftime-report` saying how often the fallback fired.

## 10.7 How this is wrong

**Wrapping.** `[100, 200] + [100, 200]` in `unsigned char` is not `[200, 400]`. Every range
operation takes the type, and the correct answer when the sum can wrap is either a wrapped
multi-interval or the full range. Getting one opcode wrong here miscompiles everything downstream
of it, and this is the primary reason 10.4 argues for verifying the table.

**Signed overflow.** In signed types, wrapping is undefined, so `[100, 200] + [100, 200]` in
`signed char` may be assumed to be `[200, 400]` and therefore to not fit, which is a contradiction
the analysis can exploit. It may only assume this when `-fwrapv` is off. The flag must be an input
to the range operations, not a check somewhere upstream, because a range computed under one
assumption and cached across a change in the other is a miscompilation.

**Ranges from undefined behaviour, generally.** Like document 07.5's loop bounds, ranges inferred
from "this would be UB otherwise" are correct and surprising. They must be dumpable, they must
name the line, and `-fdump-ranges` should mark them distinctly from ranges derived from control
flow.

**The relational oracle outlives its branch.** `a < b` holds on one edge and not another, and a
relation recorded without its block is a wrong answer on the other path. Relations are keyed by
block, not by function.

**Precision loss is invisible.** Unlike a miscompilation, a range that degraded to "everything"
because of a missing opcode entry produces correct code that is slower, forever, with no signal.
The defence is a counter: how many range queries returned the full range, broken down by the
opcode that lost the information. That counter is how the table in 10.4 grows in the right order,
by evidence rather than by guesswork.

## 10.8 What it costs

The on-demand design means the cost is proportional to what is asked, which makes it hard to state
in the abstract and easy to measure. `-ftime-report` reports range analysis separately, and the
counters from 10.7 report queries, cache hits, fallbacks and precision losses.

The threshold that matters: at `-O2` on the SQLite amalgamation, range analysis should be under
4% of compile time. GCC's is higher, and GCC's Ranger answers questions rucc will not ask.
