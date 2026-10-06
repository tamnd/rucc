# 40. Cost models

Every optimization in documents 12 through 39 that is not unconditionally profitable ends in a
comparison of two numbers. This document is where those numbers come from, and it is a collection
point: seventeen earlier documents deferred a constant, a formula or a threshold here, and the
purpose of gathering them in one place is that a heuristic constant sitting alone in the pass that
uses it is a constant nobody will ever revisit.

The organising claim is that **rucc has one cost model, not one per pass**, with a small number of
target-supplied inputs and a small number of shared derived quantities, and that every pass's
threshold is expressed in those terms rather than in instruction counts it invented.

## 40.1 The unit problem

`gcc/rtl.h:2074`:

```c
#define COSTS_N_INSNS(N) ((N) * 4)
```

Cost is measured in quarter-instructions. The factor of four exists so that a target can say an
instruction is one and a quarter of another without fractions, and it is the entire theory of units
in GCC's RTL cost model. `BRANCH_COST` is separately documented at `gcc/doc/tm.texi:7133` as "A value
of 1 is the default; other values are interpreted relative to that", which is a *different* unit.
The vectorizer has a third, the inliner a fourth counting in "estimated instructions", and
`gcc/tree-ssa-loop-ivopts.cc` a fifth.

**Five incommensurable units in one compiler is the failure this document exists to avoid.** rucc
should have exactly two, and they should be named types, not integers.

**`Cycles`**, a fixed-point estimate of execution time, in units of one simple ALU instruction's
reciprocal throughput on the target. This is what a cost model returns when asked how expensive
something is to run.

**`Bytes`**, the size in bytes of emitted code, which is exact and which the encoder can answer
precisely rather than estimating.

Everything else is derived. Size-versus-speed at `-Os` is a policy that combines them, not a third
unit; `-Oz` is the same policy with a different weight. A threshold expressed in "instructions" is
either `Cycles` or `Bytes` and the pass that cannot say which does not know what it is measuring.

## 40.2 The cost type: cost and complexity

`gcc/tree-ssa-loop-ivopts.cc:206` defines `comp_cost`, and it is worth copying:

```c
int64_t cost;         /* The runtime cost.  */
unsigned complexity;  /* The estimate of the complexity of the code for
                         the computation (in no concrete units --
                         complexity field should be larger for more
                         complex expressions and addressing modes).  */
int64_t scratch;      /* Scratch used during cost computation.  */
```

with comparison at :363 being lexicographic: equal costs are broken by lower complexity. And two
named constants at :260, `no_cost` and `infinite_cost (INFTY, 0, INFTY)`.

Three things to take.

**A cost is a pair, compared lexicographically.** The second component is a tiebreak that expresses a
preference the first cannot: among two equally fast options, prefer the simpler one. rucc's cost type
should be `(Cycles, Complexity)` for exactly this reason, and the ivopts case it was invented for,
preferring a simple addressing mode to a complicated one of the same measured cost, generalises.

**There is an explicit infinity**, so "this is not possible" and "this is very expensive" are the
same type and the comparison operators work on both. Every pass needs this and inventing
`i64::MAX / 2` in each of them is how overflow bugs happen.

**Costs are added, subtracted, multiplied and divided by scalars**, with operators. A cost model whose
values can only be compared forces every pass to unpack them.

## 40.3 What a target must be asked

`gcc/config/i386/i386.h:122` defines `struct processor_costs`, and reading it is the fastest way to
learn what a real cost model needs. It has roughly 107 fields in two groups.

**The register allocator's group**, a nested `hard_register` struct, whose comment at
`gcc/config/i386/i386.h:114` makes an important distinction: these costs "are used by
TARGET_REGISTER_MOVE_COST and TARGET_MEMORY_MOVE_COST to compute hard register move costs by register
allocator", and "relative costs of pseudo register load and store versus pseudo register moves in RTL
expressions for TARGET_RTX_COSTS can be different". Two cost tables for the same operations, because
the allocator's question and the expression evaluator's question are different questions.

Its contents: integer load and store costs indexed by width, floating-point move, load and store costs
indexed by mode, vector move costs per register width, cross-file move costs (`sse_to_integer`,
`integer_to_sse`, `mask_to_integer`, `integer_to_mask`), and mask register load and store costs.

**The general group**, which is everything else: `add`, `lea`, `shift_var`, `shift_const`,
`mult_init[5]` indexed by width, `mult_bit`, `divide[5]`, `movsx`, `movzx`, `large_insn`,
`move_ratio`, `clear_ratio`, aligned and unaligned vector load and store costs, gather and scatter
costs as `static + per_elt * nelts`, cache sizes and prefetch parameters, `branch_cost`, a dozen
floating-point instruction costs, `reassoc_int` / `reassoc_fp` / `reassoc_vec_int` / `reassoc_vec_fp`,
reduction latency-times-throughput thresholds, a vectorizer unroll limit, `memcpy` and `memset`
strategy tables, taken and not-taken branch costs for the vectorizer, four alignment strings, small
loop unrolling limits, and `br_mispredict_scale`, "Branch mispredict scale for ifcvt threshold".

And at `gcc/config/i386/i386.h:269`:

```c
#define ix86_cur_cost() \
  (optimize_insn_for_size_p () ? &ix86_size_cost : ix86_cost)
```

**Size optimization is a different cost table, not a different policy applied to the same table.**
That is a cleaner design than a weighting factor and rucc should adopt it: each target supplies a
speed table and a size table, and `-Os` selects the second. It makes `-Os` behaviour inspectable as
data and it stops every pass from having to remember to ask `optimize_for_size`.

## 40.4 Tuning is booleans, not only numbers

`gcc/config/i386/x86-tune.def` is 810 lines containing 123 `DEF_TUNE` entries, each a boolean
predicate over microarchitectures. `X86_TUNE_SCHEDULE` is the first, listing every processor family
for which scheduling is enabled.

The file's header, at `gcc/config/i386/x86-tune.def:21`, describes what tuning a new CPU involves:
adding it to the processor table, introducing a cost structure, building a stringop table from a
measurement script, designing a scheduler model, and setting the tuning flags, "split into sections
and each section is very roughly ordered by importance".

**Two lessons.** First, a target's cost model is a hundred-odd numbers *and* a hundred-odd booleans,
and the booleans are decisions like "does this microarchitecture prefer a `lea` to an `add`" that no
scalar cost can express. rucc will need them and should have a named place for them from the first
target rather than scattering `if target.is_x86()` through passes.

Second, "Stringop generation table can be built based on test_stringop script". **Some of these
numbers are measured, not chosen**, and the measurement harness ships with the compiler: it is
`contrib/bench-stringop`. That is the standard document 42 should be held to for rucc's block-copy
thresholds, and the precedent that a tuning constant's benchmark belongs in the repository next to the
constant.

## 40.5 Branches, predictability, and if-conversion

This discharges the obligations from documents 22, 24 and 37.

`gcc/config/i386/i386.h:2023`:

```c
#define BRANCH_COST(speed_p, predictable_p) \
  (!(speed_p) ? 2 : (predictable_p) ? 0 : ix86_branch_cost)
```

Read it carefully. **Optimizing for size, a branch costs 2. Optimizing for speed, a predictable
branch costs 0 and an unpredictable one costs the tuning value.** A correctly predicted branch is
free on an out-of-order machine, and any transformation that removes a predictable branch at the cost
of extra work is a pessimization. That single line is the reason document 37.7 warned about
if-converting a well-predicted branch.

Predictability is defined by `predictable-branch-outcome` (`gcc/params.opt:949`), `Init(2)`,
"Maximal estimated outcome of branch considered predictable". A branch whose probability is at most
2% or at least 98% is predictable.

And the if-conversion budget, from `gcc/params.opt:741` onward:

| Parameter | Init | Meaning |
|---|---:|---|
| `max-rtl-if-conversion-insns` | 10 | Block size limit for RTL if-conversion |
| `max-rtl-if-conversion-predictable-cost` | 20 | Cost budget for a predictable branch |
| `max-rtl-if-conversion-unpredictable-cost` | 40 | Cost budget for an unpredictable branch |

**A two-to-one ratio between the unpredictable and predictable budgets**, plus the target's
`br_mispredict_scale`. This is the complete answer to "when should a branch become a select", and
rucc should adopt these numbers as its starting point rather than deriving them, because they are
tuned against a corpus rucc does not have yet.

So the rucc rule, usable by document 22's phiopt and document 37's machine-level if-converter, which
must agree:

```
branch_cost(speed, predictable) = if !speed { 2 }
                                  else if predictable { 0 }
                                  else { target.branch_cost }
if_convert_budget = if predictable { 20 } else { 40 }
```

with predictability from document 11's profile if there is one, and from document 11.2's static
heuristics if there is not, and with the profile-quality field of document 11.1 deciding how much to
trust it. **A branch with no profile and a heuristic-derived probability should be treated as
unpredictable**, because the 2% threshold is one that static heuristics cannot honestly reach.

## 40.6 The register pressure model

This discharges document 12.5's GCM obligation and document 27.2's LICM obligation, which are the
same obligation, and connects to document 39.5's finding that the same model serves the allocator and
the scheduler.

**One function, computed once per function, consumed by four passes.** For each program point and
each register class, the number of values live there. In SSA this is exactly the maximum-live count
that document 39.5's chordality result makes meaningful: it is the number of registers the program
needs at that point, not an approximation of it.

The consumers and the question each asks:

- **LICM and GCM**: before hoisting a value out of a loop, is the maximum pressure inside the loop
  already at or above the allocatable count minus a margin. If so, hoist only genuinely expensive
  operations, meaning division and calls, per document 27.2.
- **The scheduler**, document 38.1's criterion two: among instructions of equal critical-path length,
  prefer the one that reduces live values.
- **The spill phase**, document 39.7: reduce maximum pressure to the register count. This is the
  primary consumer and the one that defines the quantity.
- **If-conversion**, document 37.6: converting a branch merges two arms' live ranges into one block,
  raising pressure there.

The margin for the first consumer is GCC's `ira-loop-reserved-regs` (`gcc/params.opt:336`), `Init(2)`,
"The number of registers in each class kept unused by loop invariant motion". **Two registers per
class.** That is the constant document 27.2 asked for and it comes from the allocator's own
parameters, which is the right place for it to come from.

## 40.7 Block copies and fills

This discharges document 21's trimming obligation and the `memcpy` inline-expansion threshold that
spec 10.2 left as "a count of moves rather than a count of bytes".

GCC's answer has two parts. `move_ratio` and `clear_ratio` in `processor_costs` are the thresholds in
scalar move instructions, and `struct stringop_algs` (`gcc/config/i386/i386.h:80`) is a table of
strategies with size ranges: for each size range, which algorithm to use, and whether alignment may be
assumed. There is a separate table for unknown sizes.

**Spec 10.2 already got the important part right**, that the threshold is a count of moves and not a
count of bytes, because the number of moves depends on the known alignment. The remaining constants:

- **The move-count threshold.** GCC's `move_ratio` is typically 6 for size and larger for speed. rucc
  should start at 8 for speed and 4 for size, and measure.
- **The move width** is the block's known alignment and no wider, per spec 10.2, and this is a
  correctness rule on targets that fault on unaligned access, not a cost rule.
- **The trimming rule of document 21.** A partially dead store may be narrowed to the live part only
  when the narrowed store is a single natural-width access at a natural alignment. Narrowing an
  8-byte store whose low 3 bytes are dead into a 5-byte store is not one instruction; narrowing one
  whose low 4 bytes are dead into a 4-byte store at offset 4 is. **The rule is: trim only to a power
  of two width, at an offset that is a multiple of that width, and only if the result is at least as
  wide as the target's cheapest store.** That last clause stops an 8-byte store becoming a 1-byte
  store when only one byte is live, which is legal but is usually a store-forwarding stall, per
  document 37.4's `avoid-store-forwarding`.

## 40.8 Reassociation width

This discharges document 19's obligation.

`processor_costs` has four fields, `reassoc_int`, `reassoc_fp`, `reassoc_vec_int`, `reassoc_vec_fp`,
with the comment "Specify reassociation width for integer, fp, vector integer and vector fp
operations. Generally should correspond to number of instructions executed in parallel."

**The width is the machine's issue width for that operation class**, so a chain of eight adds becomes
a tree of depth three on a machine that issues three integer operations per cycle, and stays a chain
on a machine that issues one. It is not a tuning constant in the usual sense; it is a hardware fact
that happens to live in the cost table.

rucc's version: one field per operation class in the target's speed table, defaulting to 1, meaning no
reassociation, so that a new target is correct before it is tuned. And document 19.2's loop-carried
bias, which prefers keeping a loop-carried operand at the root of the tree, is orthogonal to width and
is switched off wherever a vectorizer runs afterwards, per document 32.7.

## 40.9 Addressing modes and induction variables

This discharges document 28's obligation.

Two constants and one principle.

**Addressing mode costs** are a per-target table indexed by the mode's shape: base, base plus
displacement, base plus index, base plus index scaled, base plus index scaled plus displacement, and
the target's pre- and post-increment forms if it has them. The cost is `Cycles` and the tiebreak is
`Complexity`, per 40.2, which is exactly what `comp_cost` was invented for. At
`gcc/tree-ssa-loop-ivopts.cc:4797` onward the complexity is incremented once per structural feature
of the address: a symbol, a scaled index, a base-plus-index pair, a non-zero offset. Note the comment
at :4799, "Don't increase the complexity of adding a scaled index if it's the only kind of index that
the target allows": **complexity is measured relative to what the target can do**, not absolutely,
which is a refinement worth copying because otherwise every address on a target with only one
addressing mode looks complex.

**The bias toward original variables.** An induction variable that appears in the source should be
preferred to a synthesised one of equal cost, because it keeps the debug information meaningful and
because it usually keeps the loop's exit test cheap. This is a `Complexity` tiebreak, not a `Cycles`
adjustment, and expressing it that way is why the cost type is a pair.

**The principle:** ivopts is a set-selection problem, choose a set of candidates covering all uses at
minimum total cost, and the cost of a candidate is not independent of the others because they share
registers. GCC handles this with the pressure model of 40.6 folded into the objective. rucc's version,
per document 28, is much smaller, and the constants it needs are the addressing-mode table plus the
register-pressure margin, both of which already exist for other reasons.

## 40.10 Switches and the indirect branch

This discharges document 24.6's obligation.

The decision is between a compare chain, a binary search tree of compares, and a jump table. The costs:

- **Compare chain**: `n/2` well-predicted branches on average for `n` cases, if the distribution is
  uniform, and far fewer if it is not.
- **Binary search**: `log2(n)` branches, each of which is poorly predicted because the comparison is
  data-dependent.
- **Jump table**: one bounds check, one load, one indirect branch. The load may miss. **The indirect
  branch is the cost that matters and it must be priced as a mispredict**, not as a branch.

Document 24 recorded the finding that on modern out-of-order machines an indirect branch with many
targets is mispredicted a large fraction of the time, so a chain of well-predicted direct branches can
beat a table. The constant rucc needs is the mispredict penalty in `Cycles`, which is a target number
in the range of fifteen to twenty on current cores, times the mispredict probability, which depends on
the number of distinct targets and the predictor's capacity and which rucc will approximate as: a
table with at most a handful of hot targets is predictable, a table with many is not.

**Concretely**: price the jump table's indirect branch at `branch_cost(speed, predictable=false)` plus
`target.mispredict_penalty` when the number of distinct table targets exceeds a threshold, and at the
ordinary unpredictable branch cost otherwise. The threshold is a measurement document 42 owes, and the
benchmark that exposes it is an interpreter dispatch loop, which document 42 already lists.

## 40.11 The inliner's badness, in profile-guess form

This discharges document 33.4's obligation.

GCC's formula, from the comment at `gcc/ipa-inline.cc:1350`:

```
                 time_saved * caller_count
goodness =  -------------------------------------------------
            growth_of_caller * overall_growth * combined_size

badness = -goodness
```

and the nonlinearity at `gcc/ipa-inline.cc:1436`:

```c
	  /* Strongly prefer functions with few callers that can be inlined
	     fully.  The square root here leads to smaller binaries at average.
	     Watch however for extreme cases and return to linear function
	     when growth is large.  */
	  if (overall_growth < 256)
	    overall_growth *= overall_growth;
	  else
	    overall_growth += 256 * 256 - 256;
```

**Quadratic below 256, linear above, and the two pieces meet exactly**: at 256 the first gives 65,536
and the second gives 256 + 65,536 - 256, the same number. That is a deliberately continuous piecewise
function, which is the detail to notice, because a discontinuity in a heuristic that ranks a priority
queue produces decisions that flip on an off-by-one change in an unrelated size estimate. The comment
calls it a square root because the term is in the denominator.

**The problem for rucc is `caller_count`.** With a profile it is the call site's execution count.
Without one it is a guess, and the guess is the product of the enclosing loops' assumed trip counts
and the branch probabilities on the path, which is document 11's frequency. That number has a much
wider dynamic range than a real count and it is systematically wrong in a particular way: a call
inside a loop that the heuristics assumed runs a hundred times gets a hundredfold weight even when the
loop is cold.

So the rucc form, and it is a deliberate deviation:

```
goodness = (time_saved * clamp(frequency, 1, F_max))
           / (growth_of_caller * overall_growth' * combined_size)
```

with `overall_growth'` GCC's piecewise function exactly, and `F_max` set from the profile quality:
unbounded with a real profile, and bounded at something like 100 with static heuristics, so that a
nest of three loops the heuristics guessed at cannot produce a thousandfold weight on a call site that
might never run. **The clamp is the only change.** The rest of the formula is GCC's unchanged, because
every term in it earns its place and the nonlinearity is documented, measured and continuous.

The clamp's value is document 42's experiment 33: compare the inlining decisions made with a real
profile against those made with static frequencies, and pick the `F_max` at which the two sets most
nearly agree.

## 40.12 rucc's cost model, as an interface

```
Cycles(fixed point)         // one simple ALU op = 1.0
Bytes(u32)                  // exact, from the encoder
Cost = (Cycles, Complexity) // lexicographic, with an explicit infinity

trait TargetCosts {
    fn table(&self, for_size: bool) -> &CostTable;   // two tables, per 40.3
    fn tune(&self, flag: TuneFlag) -> bool;          // the booleans, per 40.4
}
```

`CostTable` is a struct of named fields, one file per target, checked by a test that every field is
set. `TuneFlag` is an enum with one variant per boolean decision, defaulting to a documented value so
that a new target is correct before it is tuned.

Derived quantities, computed once per function and shared:

- **Register pressure** per point per class, per 40.6.
- **Block frequency** with its quality field, per document 11.
- **Branch predictability**, per 40.5, which is frequency plus the 2% threshold plus the quality
  field.

**And the heuristic-constant table**, which is the deliverable this document exists to produce: one
file, `crates/rucc-opt/src/costs.rs`, containing every threshold any pass consults, each with the
document that justifies it, the GCC parameter it corresponds to where one exists, and whether it was
chosen or measured. A pass may not contain a bare numeric threshold; the coding standard test greps
for one.

The initial table, from the documents that deferred here:

| Constant | Value | From |
|---|---:|---|
| Branch cost, size | 2 | 40.5 |
| Branch cost, speed, predictable | 0 | 40.5 |
| Branch cost, speed, unpredictable | target | 40.5 |
| Predictable branch threshold | 2% | 40.5 |
| If-conversion budget, predictable | 20 | 40.5 |
| If-conversion budget, unpredictable | 40 | 40.5 |
| If-conversion block size limit | 10 | 40.5 |
| LICM pressure margin | 2 per class | 40.6 |
| Block copy move threshold, speed | 8 | 40.7 |
| Block copy move threshold, size | 4 | 40.7 |
| Trim minimum width | target's cheapest store | 40.7 |
| Reassociation width | target, default 1 | 40.8 |
| Jump table mispredict threshold | measured | 40.10 |
| Inliner frequency clamp, no profile | 100 | 40.11 |
| Inliner `overall_growth` squaring bound | 256 | 40.11 |
| Allocator degradation threshold | measured | 39.4 |
| Alignment frequency threshold | 1/100 of hottest block | 38.5 |
| Loop alignment minimum iterations | 4 | 38.5 |
| Scheduler ready-list bound | 100 | 38.8 |

Every entry is either a GCC value adopted deliberately or a measurement document 42 owes, and there
is nothing in the table that was invented here.

## 40.13 How this is wrong

**A cost is compared against a threshold in a different unit.** The failure 40.1 exists to prevent,
and the defence is that `Cycles` and `Bytes` are distinct types that do not convert implicitly.

**A cost overflows.** Costs are multiplied in 40.11's formula and a deep loop nest with a large
frequency can overflow. GCC's `comp_cost` uses `int64_t` and an explicit `INFTY`; rucc should
saturate rather than wrap, and the saturating case should be a counter rather than silent.

**A target's table is incomplete and a field is zero.** A zero cost for an operation makes it free and
every heuristic that consults it goes wrong in the same direction. The completeness test is the
defence and it must check every field, not merely that the struct was constructed.

**The size table and the speed table disagree about what is possible.** If the speed table says a
target has a scaled-index addressing mode and the size table does not, selection and costing
disagree. The two tables must differ only in numbers, never in capability, and that is checkable.

**A heuristic is tuned on a corpus that does not represent the user's code.** The universal problem,
and the honest response is document 42's corpus definition plus the discipline that a constant's
provenance is recorded. A constant marked "chosen" is a constant nobody should defend.

**Static frequencies are trusted as if they were counts.** 40.11's clamp, document 11.1's quality
field, and 40.5's rule that a heuristic-derived probability never counts as predictable. This is the
same error in three places and it is worth the repetition.

**A cost model is consulted after the transformation it was meant to gate.** Document 29.2's finding
and document 33.3's independently: cost must be estimated after the folding the transformation
enables, or every enabling transformation looks unprofitable. This is a structural requirement on how
passes call the cost model, not on the model itself.

## 40.14 What to measure

Document 42 owes, from this document specifically:

- **The jump table mispredict threshold**, on an interpreter dispatch loop, per 40.10.
- **The block-copy move threshold**, both directions, on a corpus with realistic structure sizes, with
  a measurement harness in the repository per 40.4's `test_stringop` precedent.
- **The if-conversion budgets**, since GCC's 20 and 40 are tuned for GCC's cost units and rucc's
  differ.
- **The LICM pressure margin**, which GCC sets at 2 and which is the constant most likely to be wrong
  for a different allocator.
- **The inliner frequency clamp**, by comparing inlining decisions with and without a profile and
  finding the clamp at which they most nearly agree.
- **Sensitivity in general**: for each constant in 40.12's table, the corpus run time at half and
  double the value. A constant whose sensitivity is flat is a constant that does not need tuning, and
  knowing which ones those are is worth more than tuning the rest.

### What the sweep found

The sensitivity sweep was run on 6 October 2026 as experiment 60 of section 42.5, tamnd/rucc#2970, over the 3500 cases of rucc-corpus at `-O2` against gcc 16. Each row of `rucc --print-params` was moved to half and to double its value through `--param`, one at a time, and only the cases whose assembly changed were built and run. It counts instructions retired rather than run time, since they barely move from run to run and most of the changes here are smaller than a timer's noise. The corpus page, [experiments/60-constant-sweep](https://github.com/tamnd/rucc-corpus/blob/main/experiments/60-constant-sweep/README.md), has every move with its compile time, the facets that moved most and rucc against gcc 16, and `results.json` next to it has the same for a tool. `cargo xtask sweep` runs it again.

A row is flat when neither end moves instructions retired or text by more than 0.1% of what the cases it changed had before. It is wrong when an end saves on one of the two and does not spend on the other, so a value the sweep tried beats the default. Every other row that moves something is worth tuning, which means a different value trades one of the two for the other. Of the 88 rows, 63 are flat, 16 are worth tuning and 9 are wrong, and no move made any case print the wrong answer. Each wrong row has an issue, and its value only changes in a PR of its own that answers it with SQLite as well as the corpus.

Three of the items above have a first answer here. The if-conversion budgets are flat, since no case moves at half or at double either of them, so the units they are counted in do not matter yet on this corpus. The LICM pressure margin, `loop-reserved-regs`, is wrong at both ends by a little, mostly in `crc32c`. The jump table threshold, `jump-table-min-targets`, is wrong at half: at 5 the cases it changed retire 14.88% fewer instructions, which goes against the timing of tamnd/rucc#1759 that set it at 11, and tamnd/rucc#3164 has to settle which of the two rucc follows. Each change is over the cases that move changed, not over the whole corpus.

| row | default | at half | at double | call |
|---|---:|---|---|---|
| `branch-cost-for-size` | 2 | no case | no case | flat |
| `branch-cost-predictable` | 0 | not run | no case | flat |
| `predictable-branch-percent` | 2 | no case | no case | flat |
| `if-conversion-budget-predictable` | 20 | no case | no case | flat |
| `if-conversion-budget-unpredictable` | 40 | no case | no case | flat |
| `if-conversion-block-limit` | 10 | no case | no case | flat |
| `phiopt-arm-instructions` | 2 | 116 cases, -6.34% instructions, +0.04% text | 45 cases, +24.07% instructions, +0.57% text | wrong, tamnd/rucc#3157 |
| `phiopt-factor-depth` | 4 | no case | no case | flat |
| `phiopt-arm-scan-instructions` | 16 | no case | no case | flat |
| `phiopt-unpredictable-margin-percent` | 3 | 2 cases, +0.28% instructions, -0.77% text | no case | worth tuning |
| `short-circuit-instructions` | 2 | 8 cases, -0.53% instructions, +0.19% text | 65 cases, +0.25% instructions, +0.67% text | worth tuning |
| `sra-max-bytes` | 128 | 11 cases, +0.00% instructions, +10.29% text | no case | worth tuning |
| `sra-max-pieces` | 8 | 4 cases, +0.01% instructions, +11.32% text | 26 cases, +3.16% instructions, +8.38% text | worth tuning |
| `sra-max-word-pieces` | 16 | 22 cases, +0.01% instructions, +5.72% text | no case | worth tuning |
| `block-copy-moves-for-speed` | 8 | no case | no case | flat |
| `block-copy-moves-for-size` | 4 | no case | no case | flat |
| `loop-header-insns-for-speed` | 20 | no case | 4 cases, -0.03% instructions, +1.59% text | worth tuning |
| `loop-header-insns-for-size` | 5 | no case | no case | flat |
| `jump-thread-duplication-insns` | 15 | no case | no case | flat |
| `jump-thread-paths` | 64 | no case | no case | flat |
| `jump-thread-path-insns` | 100 | no case | no case | flat |
| `jump-thread-back-edge-scale` | 2 | no case | no case | flat |
| `reassoc-width-untuned` | 1 | no case | no case | flat |
| `inline-frequency-clamp` | 100 | no case | no case | flat |
| `inline-growth-squaring-bound` | 256 | no case | no case | flat |
| `inline-insns-single` | 70 | no case | no case | flat |
| `inline-early-insns` | 6 | no case | no case | flat |
| `inline-insns-single-o3` | 200 | no case | no case | flat |
| `inline-insns-auto` | 15 | 169 cases, +1.26% instructions, -4.92% text | 83 cases, -0.14% instructions, +16.49% text | worth tuning |
| `inline-called-once-insns` | 4000 | no case | no case | flat |
| `inline-called-once-loop-depth` | 6 | no case | no case | flat |
| `inline-frame-growth` | 1000 | no case | 6 cases, +0.10% instructions, -6.72% text | worth tuning |
| `inline-large-frame` | 256 | 6 cases, -0.19% instructions, +13.00% text | 6 cases, +0.10% instructions, -6.72% text | worth tuning |
| `inline-frame-growth-conserve` | 40 | no case | no case | flat |
| `inline-large-frame-conserve` | 100 | no case | no case | flat |
| `inline-hint-percent` | 200 | 8 cases, -0.05% instructions, -18.11% text | 12 cases, -0.01% instructions, +24.23% text | wrong, tamnd/rucc#3158 |
| `inline-hint-percent-o3` | 600 | no case | no case | flat |
| `inline-min-speedup` | 30 | no case | no case | flat |
| `inline-min-speedup-o3` | 15 | no case | no case | flat |
| `inline-unit-growth` | 40 | no case | no case | flat |
| `large-unit-insns` | 10000 | no case | no case | flat |
| `large-function-insns` | 2700 | no case | no case | flat |
| `large-function-growth` | 100 | no case | no case | flat |
| `inline-call-time` | 10 | no case | no case | flat |
| `hot-block-fraction` | 1000 | no case | no case | flat |
| `predict-expect` | 90 | 8 cases, +11.04% instructions, -0.14% text | 17 cases, -0.24% instructions, +0.09% text | wrong, tamnd/rucc#3159 |
| `predict-never-returns` | 99 | 96 cases, -0.00% instructions, -0.02% text | no case | flat |
| `predict-cold-call` | 99 | no case | no case | flat |
| `predict-loop-exit-not-taken` | 89 | 448 cases, -0.06% instructions, -0.03% text | 482 cases, +5.83% instructions, +0.42% text | worth tuning |
| `predict-loop-guard-taken` | 73 | 335 cases, +0.12% instructions, +0.29% text | 87 cases, -0.08% instructions, +0.19% text | worth tuning |
| `predict-pointer-not-null` | 70 | 74 cases, +0.09% instructions, -0.48% text | 24 cases, +0.00% instructions, -0.02% text | wrong, tamnd/rucc#3160 |
| `predict-negative-return` | 98 | no case | no case | flat |
| `predict-null-return` | 71 | no case | no case | flat |
| `predict-call-not-taken` | 67 | 198 cases, -0.32% instructions, -0.03% text | 46 cases, -0.00% instructions, -0.07% text | wrong, tamnd/rucc#3161 |
| `predict-continue-taken` | 67 | no case | no case | flat |
| `max-predicted-iterations` | 100 | 4 cases, -0.00% instructions, +0.00% text | 6 cases, -0.00% instructions, +0.00% text | flat |
| `loop-reserved-regs` | 2 | 71 cases, -0.05% instructions, +0.07% text | 71 cases, -0.03% instructions, -0.26% text | wrong, tamnd/rucc#3162 |
| `licm-expensive` | 20 | no case | no case | flat |
| `unroll-max-times` | 16 | 164 cases, +9.92% instructions, -8.10% text | 76 cases, -1.34% instructions, +37.23% text | worth tuning |
| `unroll-max-insns` | 200 | 58 cases, +2.99% instructions, -7.49% text | 48 cases, -5.20% instructions, +33.02% text | worth tuning |
| `split-max-insns` | 200 | no case | no case | flat |
| `split-remade-insns` | 8 | no case | no case | flat |
| `unroll-max-depth` | 8 | no case | no case | flat |
| `profile-sum-tolerance-percent` | 1 | no case | no case | flat |
| `predict-return-blocks` | 8 | no case | no case | flat |
| `align-frequency-fraction` | 100 | no case | no case | flat |
| `loop-align-min-iterations` | 4 | no case | no case | flat |
| `iv-max-considered-uses` | 250 | no case | no case | flat |
| `iv-consider-all-candidates-bound` | 40 | no case | no case | flat |
| `iv-always-prune-cand-set-bound` | 10 | no case | no case | flat |
| `ivopts-new-variable-bias` | 3 | 484 cases, -0.38% instructions, +1.19% text | 203 cases, +5.70% instructions, +0.12% text | worth tuning |
| `ivopts-set-penalty` | 10 | no case | no case | flat |
| `scheduler-ready-list-bound` | 100 | no case | no case | flat |
| `switch-conversion-max-growth` | 8 | no case | 2 cases, -0.02% instructions, -15.64% text | wrong, tamnd/rucc#3163 |
| `jump-table-min-targets` | 11 | 29 cases, -14.88% instructions, -1.53% text | 1 case, +35.76% instructions, +35.31% text | wrong, tamnd/rucc#3164 |
| `jump-table-min-targets-for-size` | 6 | no case | no case | flat |
| `switch-peel-percent` | 66 | no case | 4 cases, +11.04% instructions, +0.00% text | worth tuning |
| `wasm-jump-table-min-targets` | 24 | no case | no case | flat |
| `wasm-jump-table-min-targets-for-size` | 4 | no case | no case | flat |
| `allocator-degradation-percent` | 10 | no case | no case | flat |
| `dse-walk-limit` | 256 | 34 cases, +0.00% instructions, +0.64% text | 21 cases, -0.00% instructions, -0.36% text | wrong, tamnd/rucc#3165 |
| `range-test-bit-intervals` | 3 | 63 cases, +0.01% instructions, +1.35% text | 1 case, +1.12% instructions, +0.78% text | worth tuning |
| `range-switch-cases` | 64 | no case | no case | flat |
| `constant-p-load-depth` | 8 | no case | no case | flat |
| `cons-cascade` | 8 | no case | no case | flat |
| `egraph-nodes` | 65536 | no case | no case | flat |
| `select-forms` | 2 | no case | no case | flat |
| `simplify-rounds` | 4 | no case | no case | flat |

## 40.15 The decision

One cost model, two units, one cost type with an explicit infinity and a lexicographic tiebreak, two
target tables differing only in numbers, one boolean tuning set, three shared derived quantities, and
one file containing every threshold with its provenance.

The finding that justifies the structure: **GCC has at least five incommensurable cost units and its
constants are spread across `params.opt`, `x86-tune.def`, `processor_costs` and the passes
themselves**, and the consequence is that nobody can answer "what would happen if this were larger"
without reading the pass. A compiler being written from scratch gets one chance to avoid that, and it
costs almost nothing to take.
