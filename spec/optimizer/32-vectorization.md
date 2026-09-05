# 32. Vectorization

Do the loop's work several elements at a time. It is the largest single performance transformation a
compiler can perform, it is the most expensive one to implement correctly, and on the code rucc says
it is for, it is worth less than almost anything else in this directory.

The GCC 16 vectorizer is 65,198 lines across ten files plus a 2,999-line header:
`gcc/tree-vect-stmts.cc` (14,866), `gcc/tree-vect-slp.cc` (12,576), `gcc/tree-vect-loop.cc` (11,910),
`gcc/tree-vect-patterns.cc` (7,819), `gcc/tree-vect-data-refs.cc` (6,927),
`gcc/tree-vect-loop-manip.cc` (4,697), `gcc/tree-vect-generic.cc` (2,521),
`gcc/tree-vectorizer.cc` (2,100), `gcc/tree-vect-slp-patterns.cc` (1,733), and
`gcc/tree-vector-builder.cc` (49). Add `gcc/tree-if-conv.cc` (4,468), which exists only to feed it,
and `gcc/tree-data-ref.cc` (6,494) from document 31, which it cannot run without, and the area is
just over 76,000 lines. That is larger than the entire tree middle end this directory has covered so
far.

None of it is in M4. This document says what it would be, what the shape of a first version should
be, and why GCC's own `-O2` behaviour makes the decision to defer much less costly than it looks.

## 32.1 The three vectorizers

`gcc/tree-vectorizer.cc:21` names them:

> This file contains drivers for the three vectorizers:
> (1) loop vectorizer (inter-iteration parallelism),
> (2) loop-aware SLP (intra-iteration parallelism) (invoked by the loop vectorizer),
> (3) BB vectorizer (out-of-loops), aka SLP

The distinction is the one everything else follows from.

**Loop vectorization** takes parallelism *across* iterations: `for (i) a[i] = b[i] + c[i]` becomes a
loop doing four elements per iteration. It needs a countable loop, a proof that iterations `i` and
`i+3` do not interfere, which is document 31's, and machinery for the leftover iterations.

**SLP**, superword-level parallelism, takes parallelism *within* a block: four adjacent independent
statements that do the same thing to adjacent memory become one vector operation. `x[0] = y[0]+1;
x[1] = y[1]+1; x[2] = y[2]+1; x[3] = y[3]+1;` is one vector add and it needs no loop analysis at all.
It is much cheaper to justify and it applies to straight-line code, which is most C.

**Loop-aware SLP** is SLP applied to the body of an unrolled-by-the-vectorizer loop, which is how
interleaved accesses like `a[2*i]` and `a[2*i+1]` get handled.

## 32.2 GCC 16's architecture: everything is SLP now

This is the most important thing in the current source and it is recent enough that most written
material about GCC's vectorizer describes the previous design.

GCC has been converting the loop vectorizer to work exclusively through the SLP representation.
Classic loop vectorization, where each scalar statement becomes one vector statement, is now
expressed as **single-lane SLP**: an SLP node with one lane, unrolled by the vectorization factor.
There is no longer a separate non-SLP code path being maintained in parallel.

The evidence is in the fallback logic. `gcc/tree-vect-loop.cc:2624` is labelled "Try again with
single-lane SLP", and at 2673 the analysis rolls back its state, sets `force_single_lane = true` and
re-runs, reporting "re-trying with single-lane SLP" at 2677. So multi-lane discovery is attempted
first, and when it fails, because a grouped store or load cannot be expressed with the target's
permute or lane instructions, the compiler retries with the degenerate representation rather than
switching to different code. `gcc/tree-vect-slp.cc:5678` describes what happens then: "In the
degenerate case of having only single-lane SLP instances this should result in a series of permute
nodes emulating an interleaving scheme." The remaining acknowledgement of the old split is a comment
at `gcc/tree-vect-stmts.cc:13063` looking forward to dropping it.

**The design lesson, and it is the reason this document is worth writing before the code:** build the
SLP graph representation first and express loop vectorization on top of it. The alternative order,
which is the order GCC took historically and has spent several releases undoing, is to build a loop
vectorizer, then add SLP as a second mode, then discover that every statement-level function needs
two implementations. A compiler starting today has the benefit of knowing how that ends.

Concretely, rucc's vectorizer, whenever it is built, has one internal representation: a DAG of nodes,
each with a lane count and a vector type, whose leaves are loads or invariants and whose root is a
store or a reduction. Loop vectorization constructs that DAG from a loop body with lane count one and
an unroll factor; BB SLP constructs it from a group of isomorphic statements with lane count `n`. One
costing function, one code generator, one permute lowering.

## 32.3 What has to be true before a loop can be vectorized

`gcc/tree-vect-loop.cc:1427` lists the entry conditions:

> Verify that certain CFG restrictions hold, including:
> - the loop has a pre-header
> - the loop has a single entry
> - nested loops can have only a single exit.
> - the loop exit condition is simple enough
> - the number of iterations can be analyzed, i.e, a countable loop. The niter could be analyzed
>   under some assumptions.

Every one of those is document 26's canonicalization output or document 07.5's trip count, which is
the concrete reason those documents come first. A vectorizer built on an uncanonicalized CFG spends
its first thousand lines rebuilding document 26.

Two GCC 16 relaxations are worth recording because they are new ground.

**Uncounted loops.** At `gcc/tree-vect-loop.cc:1621`, when the iteration count is undetermined, the
loop is now "being analyzed as uncounted" rather than rejected, with outer-loop vectorization of such
loops still refused at 1625. The catch is profitability: `gcc/tree-vect-loop.cc:1960` explains that
"As we cannot use a runtime check to gate profitability for uncounted loops require either an
estimate or if none, at least a profitable vectorization within the first vector iteration (that
condition will practically never be true due to the required epilog and likely alignment prologue)."
So the feature exists and is admitted to almost never fire without profile data. That is a useful
calibration on how much a strictly-more-capable analysis is worth.

**Early breaks.** `LOOP_VINFO_EARLY_BREAKS` (`gcc/tree-vectorizer.h:1322`) supports loops with a
conditional exit in the body, the `while (*p) p++` shape, which requires the vector iteration to be
restartable and any stores before the break to be moved after it
(`gcc/tree-vect-loop.cc:11051`). This is the feature that would let a vectorizer touch `strlen`-shaped
C, and it is the one GCC 16 development most visible in the source.

## 32.4 The cost model, and what `-O2` actually does

This section is the one that changes rucc's plan, so it is worth getting exactly right.

`gcc/flag-types.h:279` defines four models, ordered:

```
VECT_COST_MODEL_VERY_CHEAP = -3,
VECT_COST_MODEL_CHEAP      = -2,
VECT_COST_MODEL_DYNAMIC    = -1,
VECT_COST_MODEL_UNLIMITED  =  0,
```

`gcc/doc/invoke.texi:15294` defines them:

> With the `unlimited` model the vectorized code-path is assumed to be profitable while with the
> `dynamic` model a runtime check guards the vectorized code-path to enable it only for iteration
> counts that will likely execute faster than when executing the original scalar loop. The `cheap`
> model disables vectorization of loops where doing so would be cost prohibitive for example due to
> required runtime checks for data dependence or alignment but otherwise is equal to the `dynamic`
> model. The `very-cheap` model disables vectorization of loops when any runtime check for data
> dependence or alignment is required, it also disables vectorization of epilogue loops but otherwise
> is equal to the `cheap` model.

And the levels, from `gcc/opts.cc`: `OPT_LEVELS_2_PLUS` sets `-fvect-cost-model=very-cheap` at 676,
`OPT_LEVELS_2_PLUS_SPEED_ONLY` enables `-ftree-loop-vectorize` and `-ftree-slp-vectorize` at 691 and
692, and `OPT_LEVELS_3_PLUS` upgrades the model to `dynamic` at 714.

So: **`gcc -O2` vectorizes, but only when vectorization is free.** No runtime alias check, no
alignment versioning, no alignment peeling, no epilogue vectorization. The two enforcement points are
`gcc/tree-vect-loop.cc:1846`, which rejects the loop under `very-cheap` if
`LOOP_VINFO_PEELING_FOR_ALIGNMENT` or `LOOP_VINFO_PEELING_FOR_GAPS` is set, with the comment "reject
cases in which we'd keep a copy of the scalar code (even if we might be able to vectorize it)"; and
`gcc/tree-vect-loop.cc:1918`, which rejects the loop if `min_profitable_estimate` exceeds the
vectorization factor, on the reasoning that "If the vector loop needs multiple iterations to be
beneficial then things are probably too close to call, and the conservative thing would be to stick
with the scalar code." `gcc/tree-vect-data-refs.cc:4445` and 4454 apply the corresponding restrictions
on the alignment side.

**The consequence for rucc's stated target.** Spec 00's code quality axis is being within 10% of
`gcc -O2` on scalar integer and pointer code. The vectorizer `gcc -O2` runs is the `very-cheap` one,
and the set of loops it accepts is small and sharply characterised: countable, unit-stride,
provably independent without a runtime test, no leftover scalar iterations kept around, profitable
inside one vector iteration. That is a much smaller target than "a vectorizer", and it is a target
whose *boundary is written down in the source* rather than emerging from a cost model.

Two things follow. First, the `-O2` gap from having no vectorizer is bounded and measurable, and
document 42 should measure it before anyone writes a line of vectorizer. Second, when rucc does build
one, **the first version should implement the `very-cheap` model and nothing else**, because that is
simultaneously the smallest useful vectorizer and the exact thing that closes the stated gap.

The other parameters, for when they are needed: `vect-max-version-for-alias-checks` `Init(15)`
(`gcc/params.opt:1278`), `vect-max-version-for-alignment-checks` `Init(6)` (1282),
`vect-max-peeling-for-alignment` `Init(-1)` meaning target-chosen (1274), `vect-epilogues-nomask`
`Init(1)` (1266), `vect-partial-vector-usage` `Init(2)` (1286),
`vect-inner-loop-cost-factor` `Init(50)` (1290), `vect-max-layout-candidates` `Init(32)` (1270), and
`min-vect-loop-bound` (881) with no initialiser, so zero.

## 32.5 If-conversion, which is the vectorizer's front half

Document 22.3 assigned loop if-conversion here. `gcc/tree-if-conv.cc:20` states the purpose plainly:

> This pass implements a tree level if-conversion of loops. Its initial goal is to help the
> vectorizer to vectorize loops with conditions.

The algorithm, from the same comment: decide if the loop is if-convertible; walk the blocks in
breadth-first order removing conditional branches and propagating the condition into each successor's
predicate list; replace each assignment with a predicated assignment; merge every block into the
header, turning phis into conditional expressions. The comment includes a worked before-and-after,
which is the clearest specification of the transformation available anywhere.

**The correctness problem is stores, and it is the same problem as document 27.3's.** A store under a
condition, if-converted naively, becomes an unconditional store. `ifcvt_memrefs_wont_trap` at
`gcc/tree-if-conv.cc:903` is the guard, and its contract, stated in the comment above it at 891, is
that the memory reference must be "read or written unconditionally atleast once and the base memory
reference is written unconditionally once", so that making the write unconditional cannot introduce a
fault. When that cannot be shown, `ifcvt_can_use_mask_load_store` at 957 asks whether the target has
a masked store instead, and `ifcvt_can_predicate` at 997 asks the same for arithmetic.

When even that fails, `version_loop_for_if_conversion` at `gcc/tree-if-conv.cc:3526` emits both
versions under a guard, which is the same versioning machinery documents 30 and 31 need. The pattern
is now unmistakable: **versioning is a shared primitive underlying unswitching, splitting, alias
checks, alignment checks and if-conversion**, and a compiler that builds any one of them should build
it once as a utility. Document 26's block-splitting and edge-redirection code is where it belongs.

Note also that if-conversion is destructive: it makes the scalar loop worse in order to make the
vector loop possible, which is why GCC runs it on a copy and discards the copy when vectorization
fails. rucc's version must do the same, and that is an argument for building the versioning utility
before the vectorizer rather than alongside it.

## 32.6 Patterns

`gcc/tree-vect-patterns.cc:7438` is a table of recognisers, over forty of them, and the note at 7434
is a design constraint worth copying: "ordering matters - the first pattern matching on a stmt is
taken which means usually the more complex one needs to preceed the less comples onex (widen_sum only
after dot_prod or sad for example)."

The entries fall into three groups. **Widening and narrowing**: `over_widening`, `widen_mult`,
`widen_shift`, `widen_plus`, `widen_minus`, `widen_sum`, `mulhs`, `average`, `sat_trunc`. **Composite
idioms with a single instruction on real targets**: `dot_prod`, `sad` (sum of absolute differences),
`abd`, `popcount_clz_ctz_ffs`, `rotate`, `sat_add`, `sat_sub`. **Representation fixups**: `bool`,
`mask_conversion`, `bitfield_ref`, `bit_insert`, `gather_scatter`, `cond_store`, `divmod`.

The reason this file exists separately from document 20's idiom recognition is that these rewrites
are only valid *because* the result is going to be vectorized. Turning a scalar loop's
`sum += abs(a[i]-b[i])` into a `sad` node is not a scalar improvement; it is a scalar
pessimisation that pays off exactly when the target has a `psadbw`. So the patterns run inside the
vectorizer's analysis, on a shadow representation, and are discarded if vectorization does not
happen.

**For rucc this is the strongest argument in this document for document 12's arm C.** A pattern set
that must be applied speculatively, evaluated for cost, and rolled back if the transformation does
not happen, is precisely what an e-graph does natively: add the rewrite as an alternative in the
e-class, and let extraction decide. GCC needs `vect_recog_*` to be a separate 7,819-line subsystem
with its own shadow statements because GIMPLE is destructive. If rucc's mid-end is an ægraph, the
widening and idiom patterns are ordinary rules in document 13's DSL with a target-conditional guard,
and the shadow representation does not exist. That claim should go into document 12.3's experiment as
a fourth item to instrument, alongside 20.6's reversibility and 22.4's branch-versus-branchless
extraction.

## 32.7 Reductions

A loop that accumulates, `for (i) sum += a[i]`, cannot be vectorized by splitting the iterations
alone: the accumulator is loop-carried. The transformation keeps a vector accumulator and reduces it
to a scalar after the loop.

`gcc/tree-vectorizer.h:92` enumerates six kinds, and the list is a good map of the difficulty:

- `TREE_CODE_REDUCTION`, the ordinary `+`, `*`, `min`, `max`, `and`, `or`, `xor` case.
- `COND_REDUCTION`, where the accumulation is conditional.
- `INTEGER_INDUC_COND_REDUCTION` and `CONST_COND_REDUCTION`, specialisations of it where the value
  accumulated is an induction variable or a constant, which is the `index of first match` idiom.
- `EXTRACT_LAST_REDUCTION`, for `res = cond[i] ? val[i] : res`, implemented with a
  `FOLD_EXTRACT_LAST` inside the loop.
- `FOLD_LEFT_REDUCTION`, "Use a folding reduction within the loop... (with no reassocation)".

That last one is the floating-point answer and it is worth understanding. Vectorizing `sum += a[i]`
by keeping four partial sums reassociates the additions, which changes the result in floating point
and is therefore illegal without `-ffast-math`. A fold-left reduction instead uses a target
instruction that adds a whole vector into a scalar accumulator *in order*, so the result is bit-
identical to the scalar loop. Targets that have one, notably SVE's `FADDA`, can vectorize strict FP
reductions; targets that do not, cannot, and GCC leaves the loop alone.

**The interaction with document 19.** Section 19.2 recorded that GCC turns off the reassociation
pass's loop-carried phi rank bias before vectorization because it interferes with reduction chains.
This is why: reassociation reorders the accumulator's operand chain to shorten the dependence, and
the vectorizer needs to recognise the chain in its original shape to classify it as a reduction. The
ordering constraint is therefore concrete and one-directional: **reassociation's loop-carried bias
must be off in any pipeline where a vectorizer runs afterwards.** rucc has no vectorizer in M4, so
the bias is on; if one is ever built, this is the thing that will be forgotten, and it is recorded
here for that reason.

## 32.8 Loop manipulation, which is half the code

Given an analysed loop, the code that has to be emitted is not just the vector body.

`vect_do_peeling` (`gcc/tree-vect-loop-manip.cc:3238`) creates the prologue, which peels iterations
until the accesses are aligned, and the epilogue, which handles `n mod VF` leftover iterations.
`vect_loop_versioning` at 4300 emits the guard: its comment at 4282 says that when references "may or
may not be aligned or/and has data reference relations whose independence was not proven then two
versions of the loop need to be generated", with the alignment test and the alias tests of document
31.4 combined, and the profitability threshold folded into the same condition.
`vect_gen_vector_loop_niters` at 2895 computes the vector loop's trip count and
`vect_update_ivs_after_vectorizer` at 2422 fixes up every induction variable's value at the exit so
the epilogue starts where the vector loop stopped.

That last one is where rucc's IR pays off, in the same way as document 26.4's. Under block
parameters with loop-closed SSA, the exit value of every loop-carried value is already an explicit
parameter on the exit block, so "update the IVs after the vectorizer" is "supply a different argument
on the exit edge". Under phi nodes with a side table for memory, it is a walk of every phi in the
exit block plus every memory phi, which is why the function is 470 lines.

The modern alternative to prologue and epilogue is **masking**: run the loop with a predicate that
disables the lanes past the end, so there are no leftover iterations at all. That is what
`vect-partial-vector-usage` `Init(2)` controls, meaning "for all loops", and it is how AVX-512, SVE
and RVV loops are meant to be written. It removes the epilogue entirely, which under the `very-cheap`
model is the difference between a loop being vectorizable and not.

## 32.9 Fixed length or vector-length agnostic, and the cost of the answer

GCC represents every vector size as a `poly_uint64`, a polynomial in the runtime vector length,
because SVE and RVV do not know their vector width at compile time. The type appears 216 times across
the vectorizer files, densest in `gcc/tree-vect-stmts.cc` (61) and `gcc/tree-vect-slp.cc` (45), and
it is not confined to the vectorizer: it is in the middle end's mode and type machinery throughout.

This is a decision that cannot be deferred, because retrofitting it means touching every arithmetic
computation on a vector size in the compiler. rucc's targets are x86-64, AArch64 and RISC-V64. The
first two have fixed-length baselines, SSE2 and NEON, that cover the overwhelming majority of
deployed hardware. The third's vector extension is length-agnostic by construction.

**Recommendation, recorded now so it is not decided by accident later: rucc's first vectorizer is
fixed-length only, sizes are `u32`, and RVV is out of scope for it.** The justification is that a
`very-cheap`-model vectorizer on SSE2 and NEON is a bounded piece of work, and a vector-length-
agnostic one is a different and much larger project that infects the whole compiler with polynomial
arithmetic for a target extension rucc will not be measured on. If RVV support is wanted later, it is
a rewrite of the vectorizer and not of the compiler, provided the vector size is a single type alias
from day one so the blast radius is known.

## 32.10 What rucc would build, in order

Not in M4. When it happens, four stages, and the order is chosen so each one is independently
shippable and independently measurable.

**Stage zero: the measurement.** Document 42 reports the run-time gap between rucc `-O2` and
`gcc -O2` on the corpus, and separately the same comparison with `gcc -O2 -fno-tree-vectorize`. The
difference between those two numbers is the entire value of this document. Everything below is
conditional on it.

**Stage one: BB SLP.** The SLP graph representation of 32.2, discovery over straight-line code, no
loop analysis, no dependence analysis, no versioning, no peeling. It needs only document 08's alias
analysis to order the loads and stores and document 40 for costs. Perhaps 2,500 lines. It is the
cheapest real vectorization available, it works on the struct-and-array code that appears constantly
in C, and building it first means the graph representation is exercised before the loop machinery
depends on it. Look-ahead operand reordering from CGO 2018, per document 05.5, goes in from the
start because it is cheap and because retrofitting commutative reordering into a discovery algorithm
is unpleasant.

**Stage two: the `very-cheap` loop vectorizer.** Countable loops with unit-stride contiguous
accesses whose independence follows from `restrict`, distinct objects, or document 31.5's minimum
viable answer; masked epilogue where the target supports it, otherwise no vectorization; reductions
restricted to integer `+`, `min`, `max`, `and`, `or`, `xor`; no if-conversion; no alignment peeling;
no runtime checks. Built on stage one's graph with lane count one. Perhaps 3,000 lines on top of
stage one, and it reproduces `gcc -O2`'s behaviour by construction rather than by tuning.

**Stage three: if-conversion and versioning.** The shared versioning utility, then if-conversion per
32.5, then runtime alias checks per document 31.4. This is the step that turns the `-O2` vectorizer
into an `-O3` one and it is also the step that introduces every correctness risk in 32.11.

**Stage four: patterns and multi-lane SLP inside loops.** Widening, `dot_prod`, `sad`, interleaved
access with permutes. Under document 12's arm C, most of this is rules rather than code, per 32.6.

Ordering against the rest of the pipeline is already fixed by document 28.6: **the vectorizer runs
before ivopts**, because it needs to see `a[i]` and not an incremented pointer, and after LICM and
loop header copying, because it needs a preheader and a known-at-least-one-iteration loop.

## 32.11 How this is wrong

**A dependence is missed.** Document 31.6's list applies unchanged and this is the consumer that
makes it dangerous: vectorizing by four when the true dependence distance is two produces wrong
results for half the elements, deterministically, and only for inputs long enough to reach the vector
loop.

**A store is introduced by if-conversion.** 32.5. The `ifcvt_memrefs_wont_trap` contract. Getting
this wrong writes to memory the program never wrote to, which can fault, can race, and can corrupt
data the compiler was not asked to touch.

**A load is introduced past the end of an object.** Vectorizing `for (i = 0; i < n; i++)` by four
when `n` is 5 must not load elements 4 through 7 if only 5 exist. This is what peeling for gaps and
masking exist to prevent, and it is the classic vectorizer segfault.

**The epilogue disagrees with the vector loop about where it stopped.** `vect_update_ivs_after_
vectorizer`'s job. An off-by-one here processes one element twice or zero times.

**A floating-point reduction is reassociated without permission.** 32.7. Not wrong code by the
standard's lights if `-ffast-math` was given, and silently different results if it was not. rucc's
default is that FP reductions are not vectorized unless the target has a fold-left instruction or
the user asked for reassociation, and that this is one flag with one meaning, per document 41's
semantic-flag list.

**The cost model says yes and the loop gets slower.** The ordinary failure, and the reason GCC's
`-O2` model is as restrictive as it is. A vectorized loop with a prologue, an epilogue and a runtime
guard is a large fixed cost, and on a loop that runs three times it is pure loss. `very-cheap`'s rule
at `gcc/tree-vect-loop.cc:1918` (profitable within one vector iteration) is the cheap defence and
rucc should adopt it verbatim.

**SLP cost is computed per graph and consecutive graphs share data.** Document 05.5's SuperGraph-SLP
finding and decision 10 of document 05.7. A per-graph cost model systematically declines profitable
vectorization when two adjacent graphs would share loads. The cost must be computed over the
connected component.

**Vectorized code is generated for a target that does not have the instruction.** The legality query
must be a target capability table, not an assumption, and it must be consulted during costing rather
than during emission, or the compiler commits to a transformation it cannot finish.

## 32.12 What it costs, and the decision

Compile time: GCC's vectorizer is one of the most expensive things in `-ftime-report` on loop-heavy
code, and the reason is visible in the structure. Analysis is attempted per loop, per vector mode,
with a full rollback and retry on failure (32.2), and the SLP layout optimization at
`gcc/tree-vect-slp.cc:6552` onward is a graph partitioning problem with its own parameter,
`vect-max-layout-candidates` `Init(32)`. A vectorizer that respects spec 00's throughput axis has to
be off by default at `-O1` and bounded hard at `-O2`, which the `very-cheap` model does anyway by
rejecting early.

Implementation cost: stages one and two are perhaps 5,500 lines and are a milestone of their own.
Stages three and four with dependence analysis from document 31 are another 5,000 and are a second.
This is why document 05.5 says loop vectorization is a milestone rather than a pass.

**The decision: no vectorization in M4, BB SLP is the first thing built after it, and the loop
vectorizer is gated on a measurement.** The measurement is stage zero's, and it is cheap: two runs of
`gcc -O2` on the corpus, one with `-fno-tree-vectorize`. If the difference is under 3%, rucc's stated
code quality axis is reachable without any of this, and the effort belongs in documents 37 through 39
where the scalar code quality actually lives. If it is over 10%, the corpus is not the scalar C the
scope claims and either the scope or the corpus is wrong.

That measurement should be taken now, during M4, not when someone is ready to start writing a
vectorizer. It costs an afternoon and it determines whether a two-milestone project is worth
beginning.

**One bookkeeping note.** Document 03.4's `-O2` pass list includes `slp`, and document 03.5's `-O3`
list includes `loop-vectorize`. Those lists describe the levels as they should eventually stand, not
as M4 ships them. This document defers both, so the `-O2` and `-O3` lists in document 03 are ahead of
M4 by exactly one entry each, and document 43 should record that as a known gap rather than leaving
it to be discovered as a contradiction.
