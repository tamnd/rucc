# 24. Switch lowering

A `switch` is a single IR construct with an arbitrary number of targets, and there are five
fundamentally different machine-level shapes it can take. Choosing among them is a cost problem, and
the difference between the best and worst choice on a large switch is a factor of ten.

This is also the one place in the compiler where a single IR construct expands into an entirely
different program shape, which is why it gets its own document rather than living in document 36's
lowering.

GCC spends 4,065 lines in `gcc/tree-switch-conversion.cc` and its header, plus the expansion code in
`gcc/stmt.cc`.

## 24.1 The five shapes

**A sequence of comparisons.** `if (x == 3) goto L3; if (x == 7) goto L7; ...`. Correct always,
optimal for two or three cases, linear in the number of cases at run time.

**A balanced decision tree.** Binary search over the case values. `O(log n)` comparisons.
`balance_case_nodes` at `gcc/tree-switch-conversion.h:626` builds it. This is the fallback for a
sparse switch with many cases and it is what a switch on scattered constants becomes.

**A jump table.** Subtract the minimum, compare against the range, index a table of addresses, jump
indirectly. Constant time, one indirect branch, and a table of `n` pointers where `n` is the *range*
of case values, not the count. A switch on `1, 2, 1000000` has a range of a million and a jump table
is out of the question.

**A bit test.** The comment at `gcc/tree-switch-conversion.h:379` states it exactly:

> Expand a switch statement by a short sequence of bit-wise comparisons. "switch(x)" is effectively
> converted into "if ((1 << (x-MINVAL)) & CST)" where CST and MINVAL are integer constants.

This handles the case where many case values share a single target, which is extremely common:
`case 'a': case 'e': case 'i': case 'o': case 'u': return VOWEL;`. Each distinct target becomes one
mask constant and one test, so a switch with sixty cases and three targets becomes three ands and
three branches. There must be at most as many distinct targets as the target's word size permits.

**Switch conversion**, which is the transformation the GCC file is actually named after and which is
the most interesting of the five. If every case does nothing except assign a constant to the same
variable, the entire switch becomes an array lookup:

```c
switch (x) { case 0: y = 5; break; case 1: y = 9; break; case 2: y = 2; break; }
```

becomes `y = table[x]` with a range check. No branches at all. `switch_conversion::collect` at
`gcc/tree-switch-conversion.cc:181` gathers the shape and `build_constructors` at 741 builds the
arrays, plural because several variables can be assigned in each arm and each gets its own array.
`switch-conversion-max-branch-ratio` `Init(8)` (`gcc/params.opt:1128`) bounds the array size relative
to the branch count.

GCC 16 also has `exp_index_transform` (`gcc/tree-switch-conversion.cc:399`), which recognises a
switch on powers of two and replaces the index with its logarithm, making a sparse switch dense. That
is a nice trick and it is narrow.

## 24.2 The architecture worth copying: clusters

`gcc/tree-switch-conversion.h:36` describes the design:

> cluster
> |-simple_cluster (SIMPLE_CASE)
> `-group_cluster
>   |-jump_table_cluster (JUMP_TABLE)
>   `-bit_test_cluster (BIT_TEST)

The case list starts as a vector of `simple_cluster`, one per case. Then `find_bit_tests` groups
consecutive ones into bit tests where profitable, and `find_jump_tables` groups the result into jump
tables where profitable. What survives as `simple_cluster` becomes a decision tree.

**This is the right structure and rucc should adopt it directly.** A switch is not one shape, it is a
partition of the case list into contiguous runs, each of which gets a shape, joined by a decision
tree over the runs. A real switch statement in a parser has a dense run of ASCII values best served
by a jump table, a few scattered large constants best served by comparisons, and a set of aliased
cases best served by a bit test, all in the same statement. A design that picks one shape for the
whole switch cannot express that.

The interface is: `Cluster` is an enum with three variants carrying a case range and a lowering
method, the pass runs two grouping phases over a sorted vector, and the emitter walks the result.
Perhaps 700 lines including the emitters, which is the largest single pass in M4's control-flow group
and is worth it.

## 24.3 The profitability tests

`jump_table_cluster::can_be_handled` at `gcc/tree-switch-conversion.cc:1743` is the density test and
it is worth reading for how it is expressed:

```
return 100 * range <= max_ratio * comparison_count;
```

with `max_ratio` being `jump-table-max-growth-ratio-for-size` `Init(300)` (`gcc/params.opt:368`) or
`jump-table-max-growth-ratio-for-speed` `Init(800)` (`gcc/params.opt:372`). So at `-O2` a jump table
is allowed when the range is at most eight times the number of cases, and at `-Os` at most three
times. Plus overflow guards, which matter because the range of a 64-bit switch does not fit.

`is_beneficial` at `gcc/tree-switch-conversion.cc:1785` adds the count test: at least
`case-values-threshold` cases, a target-dependent number defaulting per machine
(`gcc/params.opt:129`). A jump table for four cases costs an indirect branch, which is mispredicted
much more often than a well-predicted conditional branch, so small switches stay as comparisons even
when dense.

**The indirect branch cost is the thing to get right and it is the thing everyone gets wrong.** A
jump table looks free in an instruction count model: one bounds check, one load, one jump. On a
modern out-of-order machine an indirect branch with many targets is mispredicted a large fraction of
the time and costs fifteen to twenty cycles. A binary search of four comparisons on well-predicted
branches can be faster. Document 40's cost model must price the indirect branch as a mispredict, not
as a jump, and the number is target-specific.

**Bit test grouping** has two algorithms, per `gcc/tree-switch-conversion.h`: `find_bit_tests_fast`,
greedy and possibly suboptimal, and `find_bit_tests_slow`, quadratic and optimal, selected by a
`max_c` threshold. rucc builds the greedy one and records that the optimal one exists, because the
quadratic one is only justified for large switches and large switches are where compile time matters.

## 24.4 What rucc builds and where

**The IR keeps `switch` as a terminator with a list of (value, block) pairs and a default block,
through the entire middle end.** This is important and it is a decision worth stating explicitly. A
switch lowered early to comparisons is a mess of branches that document 10's range analysis must
re-derive facts from, that document 23's jump threading must re-thread, and that document 21 must
clean up. A switch kept whole is a single node from which the range on each outgoing edge is
immediate: on the edge to case 5, the operand is exactly 5; on the default edge, it is outside the
case set.

That last fact is worth more than it sounds. Document 10's ranges over a switch operand are exact
and multi-interval, which is exactly the representation 10.2 chose, and this is the construct that
most benefits from it. GCC gets this too, and it is the reason `switch` survives to
`pass_lower_switch` late in the tree pipeline.

**Lowering happens once, late, at the boundary into the machine level**, which is document 36. This
document owns the decision procedure; document 36 owns the emission.

**And there is one middle-end transformation on switches**: removing cases that document 10's ranges
prove impossible. If the operand's range is `[0, 3]`, cases 7 and 12 are dead and the switch shrinks,
which can turn a sparse switch into a dense one and change the lowering decision entirely. This is
cheap, it uses machinery that exists, and it belongs in the same pass as the range-based branch
simplification of document 21.

**Switch conversion to a lookup table** is a middle-end transformation, not a lowering one, because
its output is ordinary loads and arithmetic that later passes optimize. It runs at `-O2` and above,
after inlining and constant propagation have made the arms' constancy visible. It requires emitting a
read-only global array, which the object writer already supports, and it must be careful with the
default case: the array only covers the case range and the default needs its own path.

## 24.5 What is deliberately not built

**The optimal bit-test partition.** Greedy only, per 24.3.

**The power-of-two index transform.** Narrow. Recorded.

**Switch conversion producing multiple arrays**, one per assigned variable. M4 does the
single-variable case, which is the overwhelmingly common one, and gives up on arms that assign more
than one thing. The generalisation is mechanical and can wait.

**Profile-driven case reordering.** With profile data, the hottest cases should be tested first in a
decision tree, and the tree should be weighted rather than balanced. GCC's cluster structure carries
`profile_probability` and `subtree_prob` for exactly this (`gcc/tree-switch-conversion.h:51`). rucc's
`Cluster` should carry document 11's `Frequency` from the start even though M4 does not use it,
because retrofitting it means touching every construction site.

## 24.6 How this is wrong

**The jump table is indexed out of range.** The bounds check is `(unsigned)(x - min) <= range`, one
unsigned comparison covering both ends, and it must be present even when the operand's range is
believed to cover only the table. If document 10's range analysis is wrong, the check saves the
program from jumping to an arbitrary address; without it, a range bug becomes arbitrary code
execution. **The bounds check is not an optimization decision.** It may only be omitted when the
switch has no default and the operand's range provably lies within the table, and even then the
value of omitting it is one comparison. M4 always emits it.

**The bit test shifts by more than the word width.** `1 << (x - MINVAL)` is undefined when
`x - MINVAL` exceeds the word size, so the bit test is only correct after a range check confirms `x`
lies in `[MINVAL, MAXVAL]`. Same class of bug as document 19.4's, same defence.

**Case values overflow during the range computation.** `get_range(low, high)` on a 64-bit switch with
`low = INT64_MIN` and `high = INT64_MAX` overflows. GCC guards this explicitly at
`gcc/tree-switch-conversion.cc:1767` with a check that `range` is nonzero and below
`HOST_WIDE_INT_M1U / 100`, the second because the profitability test multiplies by 100. rucc's
arithmetic here is in a wider type or is checked; assuming it fits is how the bug happens.

**Duplicate or overlapping case values.** The front end rejects these, so the middle end may assume
uniqueness, and an assertion should record that assumption rather than leaving it implicit. A pass
that merges cases, such as the dead-case removal in 24.4, must maintain it.

**The default edge is lost.** A switch whose cases cover the operand's entire range still has a
default edge in the CFG until something proves it unreachable, and deleting it prematurely, or
failing to delete it when the range analysis proves it dead, are respectively a missed optimization
and a wrong CFG. The second is worse: an unreachable default block that is still an edge target keeps
its contents alive, which is merely wasteful; an edge deleted while reachable is a miscompilation.

**Switch conversion changes the value on the default path.** The lookup table covers `[min, max]`.
An operand outside that range must reach the default, and an operand *inside* the range that has no
case must also reach the default. The second is the trap: a switch on `0, 1, 3` converted to a table
of size four needs a hole at index 2. GCC's `gather_default_values`
(`gcc/tree-switch-conversion.cc:710`) fills the holes with the default's value, which is correct only
if the default arm merely assigns a constant. If the default does something else, the switch is not
convertible.

**The lookup table is emitted in a writable section.** It must be read-only, and on targets that
care, it must be in a section the linker can place near the code.

## 24.7 What it costs

Sorting the case list is `n log n`. Bit test grouping is linear in the greedy form. Jump table
grouping is a linear scan with a profitability test per candidate run. The decision tree balance is
`n log n`. Everything here is cheap relative to the size of the construct.

The cost that matters is code size: a jump table for a 256-entry switch is 2 KB of pointers on a
64-bit target, or 1 KB with 32-bit offsets, which is worth doing and is a document 36 concern. At
`-Oz`, jump tables are often the wrong choice for exactly this reason and the growth ratio parameter
handles it.

The measurement in document 42: on the corpus, compare the shape chosen per switch against `gcc -O2`
and `gcc -Os`, and separately measure the run-time effect of forcing each shape on a benchmark with a
hot switch. An interpreter loop is the right benchmark and it is the one where the indirect branch
cost from 24.3 dominates everything else.
