# 31. Dependence analysis

Given two array references in a loop nest, do they ever touch the same element, and if so, how many
iterations apart. Every transformation in documents 30 and 32 rests on this question, and none of
them can be built without it.

It is a different question from alias analysis, document 08's, and the difference is worth stating
because the two are constantly confused. Alias analysis asks whether two pointers may designate
overlapping storage. Dependence analysis assumes they designate the *same* array and asks whether the
subscripts collide at particular iteration pairs. Alias analysis is a prerequisite: if the two
references might be to different objects entirely, they are independent for free; if they might be to
the same object, dependence analysis takes over.

`gcc/tree-data-ref.cc` is 6,494 lines. Nothing in M4 uses it. This document exists because documents
30 and 32 both defer to it and because the design decisions are better made now than under pressure.

## 31.1 The question, stated exactly

`gcc/tree-data-ref.cc:25` gives the formulation:

> given two access functions chrec1 and chrec2 to a same array, and x and y two vectors from the
> iteration domain, the same element of the array is accessed twice at iterations x and y if and only
> if: chrec1 (x) == chrec2 (y).

So the problem is solving an equation over the integers, subject to the iteration domain's bounds.
When the access functions are affine, that is a linear Diophantine system, and the whole field is
techniques for deciding such systems cheaply and conservatively.

The output is not a boolean. GCC's list, at `gcc/tree-data-ref.cc:31`:

- **Independence**, qualified as `chrec_known`, meaning proven never to collide.
- **Distance vectors**: the difference in iteration counts at which the collision occurs, per loop
  level. A distance of `(0, 1)` means the same outer iteration, one inner iteration apart.
- **Direction vectors**: the sign of each distance, `<`, `=`, `>`, used when the exact distance is
  not known.
- **Loop-carried level**: the outermost loop level at which the dependence exists.

The distinction between distance and direction matters for consumers: interchange needs directions,
predictive commoning needs exact distances, vectorization needs to know the distance exceeds the
vector length.

## 31.2 The classical tests, and their names

`gcc/tree-data-ref.cc:3609` marks "the classic Banerjee tests", and the statistics structure at
`gcc/tree-data-ref.cc:110` enumerates the taxonomy, which is the clearest summary of the field
available in a hundred lines of code:

**ZIV**, zero index variables: both subscripts are loop-invariant. `a[3]` and `a[5]`. Compare the
constants. Trivial and it resolves a surprising fraction of real subscript pairs.

**SIV**, single index variable: both subscripts involve the same one induction variable. `a[i]` and
`a[i+1]`, or `a[2*i]` and `a[2*i+1]`. This is where most real dependences live, and there are exact
tests for the common shapes: strong SIV where both have the same coefficient, weak-zero SIV where one
coefficient is zero, weak-crossing SIV where the coefficients are negatives.

**MIV**, multiple index variables: subscripts involving several induction variables. `a[i+j]` and
`a[i-j]`. Exact tests are expensive; the practical answer is a conservative test.

The conservative tests, in increasing order of power and cost:

- **The GCD test.** `a[c1*i + c0]` and `a[c2*i + d0]` collide only if `gcd(c1, c2)` divides
  `d0 - c0`. Two lines of arithmetic, proves independence for a useful fraction of pairs, and proves
  nothing about distance when it fails.
- **Banerjee's test.** Bound the range of `chrec1(x) - chrec2(y)` over the iteration domain; if zero
  is outside the range, independent. Handles the loop bounds, which the GCD test ignores.
- **The Omega test.** Exact integer programming over the constraints, from Pugh. Exponential in the
  worst case, bounded in practice, and GCC has it.

GCC's statistics counters track `independent`, `dependent` and `unimplemented` separately for each of
ZIV, SIV and MIV, which is itself a design lesson: the pass instruments how often each test class
gives up, so the decision about where to add power is driven by counts rather than intuition.

## 31.3 What rucc would build, and in what order

The instrumentation-first approach that GCC's counters imply, applied deliberately.

**Stage one: the data reference structure.** For each memory access in a loop nest, decompose it into
a base object, a list of subscripts, and an access function per subscript expressed as a chrec from
document 07.4. This is most of the work and it is shared by every consumer. It requires the base to
be recognisable, which is document 08's job, and the subscripts to be affine, which is document
07.4's.

Note that C makes this harder than Fortran does, and the reason is worth stating. `a[i][j]` on a
true two-dimensional array decomposes into two subscripts. The same access through `int *p` with
manual index arithmetic, `p[i*n + j]`, is one subscript in a linearised space, and recovering the two
dimensions requires knowing `n` and proving `j < n`. GCC has delinearization for this and it is
imperfect. C code that uses pointers rather than arrays, which is most C code, gives dependence
analysis much less to work with than the equivalent Fortran, and that is a large part of why the
transformations in document 30 pay off less on C.

**Stage two: ZIV and the GCD test.** Perhaps 150 lines. Instrument how many subscript pairs each
resolves.

**Stage three: SIV, all three variants.** Perhaps 400 lines. Exact for the shapes it covers.

**Stage four: Banerjee's bounds test** for what remains.

**Never: Omega.** The exact test's incremental yield over Banerjee on C code has not been shown to be
worth its cost, and rucc's dependence-analysis consumers are all optional transformations. When the
test cannot decide, the answer is "dependent" and the transformation does not happen.

**And the default answer is dependent.** This must be said in the interface, not the implementation:
the query returns `Independent`, `Dependent(DistanceVector)`, or `Unknown`, and `Unknown` is treated
identically to `Dependent` by every consumer. There is no path where a failure to analyse becomes
permission to transform. This is the same discipline as document 08.6's whitelist rule and it is
worth the same emphasis, because the failure mode is the same: silent wrong code.

## 31.4 Runtime alias checks, which change the economics

When the analysis cannot prove independence because two base pointers might be the same object, the
compiler can emit a runtime test and version the loop:

```c
if (p + n <= q || q + n <= p) { /* independent version */ } else { /* original */ }
```

`create_runtime_alias_checks` at `gcc/tree-data-ref.cc:2673` builds these, `runtime_alias_check_p` at
1635 decides whether one is worth emitting, and `prune_runtime_alias_test_list` at 1808 merges
overlapping checks so that `k` reference pairs do not produce `k` tests.
`vect-max-version-for-alias-checks` (`gcc/params.opt:1278`) bounds how many are emitted.

**This is what makes vectorization work on C at all.** Without `restrict`, almost no two pointer
parameters can be proven distinct, so almost no loop over two arrays can be proven independent. The
runtime check converts a static impossibility into a dynamic cost of two comparisons before the loop.

The consequence for rucc: **the runtime check machinery is not an optional refinement of dependence
analysis, it is a required part of it for C**, and any plan that builds the static analysis and defers
the runtime checks will find the static analysis proves almost nothing useful. If dependence analysis
is built, both halves are built together.

The pruning at `gcc/tree-data-ref.cc:1808` is not optional either: a loop touching four arrays has
six pairs and six checks, and merging the checks that share a base is what keeps the prologue from
costing more than the loop saves. `gcc/tree-data-ref.cc:2385` and 2630 handle the inclusive versus
exclusive endpoint arithmetic that the segment comparison needs, and the fact that GCC spends
paragraphs of comment on it is a warning about where the off-by-one bugs are.

## 31.5 The relationship to `restrict`

Document 08.1 established that GCC implements `restrict` as two integers, a clique and a base, on
each reference. In a loop, `restrict` on two pointer parameters answers the dependence question
immediately and for free: they designate disjoint objects, so no subscript analysis is needed.

That is worth more on C than any amount of subscript machinery, and it is already in M4's alias
analysis. So the sequence for any future dependence query is: ask alias analysis first, which handles
`restrict`, distinct objects, and `malloc`-derived pointers; only then decompose subscripts.

And it suggests a cheap intermediate step that is not a full dependence analysis: **for loops where
alias analysis already proves the accesses are to distinct objects, the loop is trivially free of
loop-carried memory dependences.** That fact is available in M4 today, needs no new analysis, and
would be enough for a future vectorizer to handle `restrict`-annotated code. It is worth recording as
the minimum viable dependence answer.

## 31.6 How this would be wrong

**A dependence is missed and the loop is transformed.** The whole risk. The distance vector says the
references are five iterations apart, vectorization by eight is performed, and the result is wrong
for one element in eight. It reproduces reliably, at least, which is more than can be said for the
alias bugs.

**The distance is computed with the wrong sign.** A dependence from iteration `i` to `i+1` is a
different constraint from `i+1` to `i`. Direction conventions are a classic source of confusion and
the defence is a test suite of loops with hand-computed distance vectors.

**Overflow in the subscript arithmetic.** `a[i*n + j]` where `i*n` overflows. The analysis reasons in
mathematical integers; the program computes in machine integers. Under C's undefined signed overflow
the assumption is legal, and where the index is unsigned it is not. This is the same class of issue
as document 28.7's.

**Delinearization guesses the dimensions wrong.** Recovering `a[i][j]` from `p[i*n+j]` requires
`0 <= j < n`, and if `j` can exceed `n` the accesses interleave differently. GCC's delinearization
produces conditions that must be checked, and skipping them is how this becomes wrong code.

**A runtime check is emitted that does not cover the whole access range.** The segment length must
account for the element size, the trip count, and the direction of iteration. The inclusive-exclusive
arithmetic at `gcc/tree-data-ref.cc:2385`.

**A runtime check is emitted for a pair whose base objects the check cannot compare.** Comparing
pointers into different objects is undefined in C, and the check `p + n <= q` on unrelated pointers
is exactly that. In practice compilers do it and it works on flat address spaces; the honest position
is to note that the generated check relies on a flat address space, which every target rucc supports
provides.

## 31.7 What it would cost

The data reference decomposition is one walk per loop nest, with a chrec query per subscript.

The subscript tests are per pair of references, so quadratic in the number of memory accesses in the
nest. `loop-max-datarefs-for-datadeps` (`gcc/params.opt:452`) bounds it in GCC and the bound exists
because the quadratic is real: a loop body with fifty memory accesses has 1,225 pairs.

The runtime check machinery costs code size in the prologue and a versioned loop body.

## 31.8 The decision

**Dependence analysis is not in M4, and it is the gateway to documents 30 and 32.** Building it is
the single largest prerequisite for moving rucc from a good scalar compiler to a vectorizing one, and
the order in which it would be built is stage one through four of 31.3 plus the runtime checks of
31.4, together perhaps 2,500 lines, which is a quarter of GCC's and would cover the C cases that
matter.

The number that decides whether to start is document 30.8's: the fraction of corpus run time in
perfectly nested affine loops. Collect it first. Everything in documents 30, 31 and 32 is downstream
of that one measurement, and it is cheap to take.
