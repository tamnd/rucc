# 34. Interprocedural analysis and optimization

Everything the compiler can learn by looking at more than one function at a time, other than
inlining. `gcc/ipa-*.cc` totals 67,117 lines across twenty-four files, which is comparable to the
entire tree middle end, and it is the part of GCC that has grown most in the last decade.

The organising fact is that all of it exists in three phases, because all of it must work under LTO.
A pass **generates a summary** per function during compilation, **propagates** across the callgraph
when the whole program is visible, and **transforms** afterwards. That structure is imposed by
document 35's requirements, and it costs something even in a non-LTO build. This document takes it as
given and document 35 says whether rucc pays for it.

## 34.1 The callgraph, and what a pass gets to see

Before any of this: the callgraph node, `cgraph_node`, with edges for calls, an indication of whether
each edge is direct or indirect, and a `symtab_node` base carrying visibility, aliases and
comdat groups. `gcc/ipa-visibility.cc` (1,032 lines) computes which symbols can be seen or replaced
from outside the unit, and it gates every other IPA pass, because a function that might be
interposed at link time cannot have its body trusted.

**That gating is the first thing rucc needs and it is not optional.** A `static` function is private
to the translation unit; an `extern` function in a shared library may be replaced by the dynamic
linker unless `-fno-semantic-interposition`; a `weak` symbol may be overridden. Every fact derived
from a function body is conditional on the body being the one that runs. `gcc/opts.cc:727` sets
`-fno-semantic-interposition` at `-Ofast`, which is the compiler admitting the assumption is not
free.

The traversal order is the callgraph's condensation in topological order, and every pass in this
document is either callee-to-caller or caller-to-callee over it, with strongly connected components
iterated to a fixpoint. Getting that utility right once, in `rucc-ipa`, is a precondition for
everything below.

## 34.2 Purity: `ipa-pure-const`

`gcc/ipa-pure-const.cc`, 2,415 lines, and the smallest high-value pass in the area. Its header at
`gcc/ipa-pure-const.cc:20` is precise about what it computes and when it may run:

> This file marks functions as being either const (TREE_READONLY) or pure (DECL_PURE_P). It can also
> set a variant of these that are allowed to loop indefinitely (DECL_LOOPING_CONST_PURE_P).
>
> This must be run after inlining decisions have been made since otherwise, the local sets will not
> contain information that is consistent with post inlined state. The global sets are not prone to
> this problem since they are by definition transitive.

Three things worth extracting.

**The three-valued answer.** Not pure or impure, but `const`, `pure`, and `looping` variants of both.
A function that computes only from its arguments but may not terminate is `const` for the purpose of
CSE, since two calls give the same answer, but not for the purpose of deletion, since deleting it
changes whether the program terminates. C's infinite-loop rules make this less pressing than in C++,
but the distinction is real and cheap to carry, and a compiler that conflates them will eventually
delete a spin loop.

**The ordering constraint.** Purity must be computed after inlining, because before inlining a
function's local behaviour includes calls whose effects are unknown.

**It is on at `-O1`.** `gcc/opts.cc:598` enables `-fipa-pure-const` at `OPT_LEVELS_1_PLUS`, alongside
`-fipa-profile` (597), `-fipa-reference` (599) and `-fipa-reference-addressable` (600). These are the
four IPA passes GCC considers cheap enough for `-O1`, and that is a strong signal about which
interprocedural analysis is worth its compile time.

`gcc/ipa-reference.cc` (1,341 lines) computes, for each function, the set of static variables it
reads and writes. In C, where file-scope `static` variables are the usual way to hold module state,
this is directly valuable: a call to a function that provably does not touch `some_static` does not
kill a load of it.

## 34.3 Mod/ref: `ipa-modref`

`gcc/ipa-modref.cc`, 5,682 lines plus `gcc/ipa-modref-tree.cc` at 1,121. Document 8.4 already
identified this as the answer to "what does a call do to memory" and deferred it here.

The header at `gcc/ipa-modref.cc:20` describes what it produces:

> 1) load/store access tree described in ipa-modref-tree.h. This is used by tree-ssa-alias to
>    disambiguate load/stores
> 2) EAF flags used by points-to analysis (in tree-ssa-structalias). and defined in tree-core.h.

And it notes the structural point that matters for design: "This file contains a tree pass and an IPA
pass. Both performs the same analysis however tree pass is executed during early and late
optimization passes to propagate info downwards in the compilation order. IPA pass propagates across
the callgraph."

**So the same analysis exists twice, at two scopes, sharing the summary representation.** That is the
right shape and it is what rucc should copy: one summary type, one analysis producing it from a
function body, and two drivers, a local one that uses whatever summaries exist for callees already
compiled, and a whole-unit one that propagates to a fixpoint.

The summary content is per parameter: which parts of the pointed-to object are read, which written,
at which offsets and sizes, whether the parameter escapes, and whether it escapes to a call. The
escape summaries "hold escape points for given call edge. That is a vector recording what function
parameters may escape to a function call (and with what parameter index)", which is what lets escape
information cross the call boundary rather than stopping at it.

The bound: `ipa-max-aa-steps` `Init(25000)` (`gcc/params.opt:300`), "Maximum number of statements that
are visited by IPA formal parameter analysis based on alias analysis in any given function". A budget,
per function, of the same kind as document 09.2's memory walk.

`-fipa-modref` is enabled at `OPT_LEVELS_1_PLUS_NOT_DEBUG` (`gcc/opts.cc:633`), so it is considered
cheap enough for `-O1` but is disabled when `-g` semantics matter, which is the `-Og` boundary.

## 34.4 Interprocedural constant propagation: `ipa-cp`

`gcc/ipa-cp.cc`, 6,933 lines, plus `gcc/ipa-prop.cc` at 6,959 for the representation. The largest
pass in the area and the one with the most transferable ideas.

The header at `gcc/ipa-cp.cc:23` states two goals: discover functions always invoked with the same
constant argument and specialize them, and "partial specialization - create specialized versions of
functions transformed in this way if some parameters are known constants only in certain contexts but
the estimated tradeoff between speedup and cost size is deemed good". It cites Callahan, Cooper,
Kennedy and Torczon (Comp86) and Cooper, Hall and Kennedy on procedure cloning.

**Jump functions** are the representation and they are the idea worth stealing. For each argument at
each call site, record how it relates to the caller's own parameters, in three forms
(`gcc/ipa-cp.cc:60`):

> Pass through - the caller's formal parameter is passed as an actual argument, plus an operation on
> it can be performed.
> Constant - a constant is passed as an actual argument.
> Unknown - neither of the above.

The pass-through case with an operation is what makes the propagation transitive: if `f` passes
`x + 1` to `g`, and `f` is always called with `x == 3`, then `g` is always called with 4. Without it,
propagation stops at one level and finds almost nothing in layered code.

**Three stages**, per the header: intraprocedural analysis building jump functions; interprocedural
propagation over the condensation in topological order with SCCs iterated, which "also record what
known values depend on other known values and estimate local effects" and then "propagate cumulative
information about these effects from dependent values to those on which they depend"; and
materialization of clones with call redirection.

That middle stage's second half is the interesting part. It is not enough to know that a parameter is
sometimes constant; the decision to clone needs to know *what that constant is worth*, which is
document 33.3's predicated summaries evaluated with the candidate value. So IPA-CP and the inliner
share the cost machinery, and neither works properly without it.

The parameters are unusually numerous and unusually informative:

| Parameter | Line | Value | What it encodes |
|---|---:|---:|---|
| `ipa-cp-eval-threshold` | 252 | 500 | when a clone is worth it |
| `ipa-cp-loop-hint-bonus` | 256 | 64 | bonus for making a loop bound or stride known |
| `ipa-cp-unit-growth` | 284 | 10% | total size budget for cloning |
| `ipa-cp-large-unit-insns` | 288 | 16000 | when a unit is large enough to be careful |
| `ipa-cp-value-list-size` | 292 | 8 | values tracked per parameter |
| `ipa-cp-max-recursive-depth` | 260 | 8 | recursive specialization depth |
| `ipa-cp-min-recursive-probability` | 264 | 2 | |
| `ipa-cp-recursive-freq-factor` | 268 | 6 | |
| `ipa-cp-recursion-penalty` | 272 | 40% | |
| `ipa-cp-single-call-penalty` | 276 | 15% | |
| `ipa-cp-sweeps` | 280 | 3 | how many times the whole decision pass repeats |
| `ipa-max-agg-items` | 304 | 16 | aggregate fields tracked per parameter |
| `ipa-max-param-expr-ops` | 308 | 10 | complexity of a pass-through operation |
| `ipa-jump-function-lookups` | 296 | 8 | statements visited finding an offset |
| `ipa-max-loop-predicates` | 312 | 16 | |
| `ipa-max-switch-predicate-bounds` | 316 | 5 | |

`ipa-cp-loop-hint-bonus` is the same idea as document 33.5's `loop_iterations` hint: the value of
knowing a constant is mostly the value of what it unlocks downstream. `ipa-cp-single-call-penalty`
exists because a function that just calls another function is a wrapper, and specializing the wrapper
without specializing the callee accomplishes nothing.

`-fipa-cp` is `-O2` (`gcc/opts.cc:654`), along with `-fipa-bit-cp` (653) which propagates known bits
rather than whole values and `-fipa-vrp` (658) which propagates ranges, both of which are document
10's lattices lifted to the callgraph. **Cloning is `-O3` only**: `-fipa-cp-clone` at
`gcc/opts.cc:704`. So at `-O2` GCC propagates but only transforms functions whose parameter is
constant in *every* context; specialization for some contexts waits for `-O3`.

## 34.5 The rest, briefly and with a verdict each

**`ipa-sra`** (4,753 lines) removes unused parameters and return values and splits aggregate
parameters passed by reference into scalars. Its header at `gcc/ipa-sra.cc:20` explains the two
sweeps: callees to callers for parameter removal, callers to callees for return value removal, with
SCCs iterated. The transitive case it highlights is worth quoting the shape of: if two parameters of
one function are used only in a sum passed to another function that does not use it, all three
parameters disappear. Parameters: `ipa-sra-max-replacements` `Init(8)` (324),
`ipa-sra-ptr-growth-factor` `Init(2)` (328), `ipa-sra-deref-prob-threshold` `Init(50)` (320). `-O2`
(`gcc/opts.cc:657`). **Verdict for rucc: the unused-parameter and unused-return half is worth
building and is perhaps 300 lines; the aggregate-splitting half is not, in M4.** The cheap half is
where most of the benefit is on C, because it is what deletes the dead argument setup at every call
site of a function whose parameter stopped being used after inlining.

**`ipa-icf`** (3,728 lines plus `gcc/ipa-icf-gimple.cc` at 1,094), identical code folding. The
algorithm at `gcc/ipa-icf.cc:24` is seven steps: build a semantic item per function and read-only
variable, hash them, form congruence classes by hash, deep-compare within a class, then run value
numbering over the classes "published by Alpert, Zadeck in 1992", then merge by alias or thunk.
`-O2` (`gcc/opts.cc:655`). **Verdict: valuable for size, nearly worthless for speed, and the hash
comparison is subtle** because two functions are identical only if every referenced symbol, type
size, and alias set matches. Post-M4, and when it comes it belongs behind `-Os` and `-Oz` first.

**`ipa-split`** (2,082 lines), partial inlining. `gcc/ipa-split.cc:21` gives the shape: a function
that is a cheap test plus a rare expensive body is split into a small header calling a `.part`
function, so the header becomes inlinable. This is the pass whose interaction produced document
33.4's wrapper penalty, and the two must be understood together or the penalty looks arbitrary.
`-fpartial-inlining` is `-O2` (`gcc/opts.cc:662`), governed by
`partial-inlining-entry-probability` (`gcc/params.opt:941`). **Verdict: post-M4, and it is a
surprisingly good idea for C, where the error-handling-in-a-cold-branch pattern is everywhere.**

**`ipa-devirt`** (4,602 lines) plus `gcc/ipa-polymorphic-call.cc` (2,629). Type-based
devirtualization of C++ virtual calls. `-O2` (`gcc/opts.cc:645`). **Verdict: not applicable. rucc is
a C compiler.** What *is* applicable is the indirect-call half: a call through a function pointer
whose value IPA-CP has determined is a single known function can be turned into a direct call, and
that appears in C in dispatch tables and callback structures. That is a small piece of IPA-CP's
output, not a separate 4,600-line pass, and it is worth having because a direct call is inlinable and
an indirect one is not.

**`ipa-profile`** (1,094 lines) propagates profile-derived hotness across the callgraph and performs
indirect call speculation from value profiling. `-O1` (`gcc/opts.cc:597`). Document 11.5 already
defers profile data; this follows it.

**`ipa-locality-cloning`** (1,531 lines) is new and is a link-time layout transformation: place
frequently executed call chains in the same partition, cloning functions when a chain cannot be
placed with a function already assigned elsewhere. It is entirely an LTO partitioning concern and
belongs to document 35.

**`ipa-strub`** (3,634 lines) implements stack scrubbing for the `strub` attribute, a security
feature. Not an optimization; recorded so the file size is accounted for.

## 34.6 What rucc builds in M4

M4 compiles one translation unit at a time, which is exactly GCC's non-LTO mode, and that is the
scope here. Whole-program anything is document 35's.

**The infrastructure, roughly 500 lines.** A callgraph over the unit's functions, with direct edges
from calls and a flag for indirect ones; visibility computed per symbol per 34.1; the condensation
and its topological order; and an SCC-iterating driver that a pass supplies a transfer function to.
Every pass below is a use of it, and the driver is where determinism must be enforced, per spec 03:
the traversal order is by a stable function ordering, not by hash iteration.

**Purity analysis, roughly 200 lines,** at `-O1` and above, after inlining, with the four-valued
answer of 34.2. This is the highest value per line in the document. Its consumers already exist:
document 08.4's call handling, document 17's DCE deleting calls whose results are unused, document
16's GVN treating two calls as the same value, and document 27.1's speculation predicate, which
currently says a call is safe only if it is `const` and needs somebody to compute that.

**A local mod/ref summary, roughly 400 lines,** at `-O2`. Per function: for each pointer parameter,
does the function read through it, write through it, does the pointer escape. No offsets, no access
trees, no aggregate granularity. That is much less than GCC's and it answers the question that
matters in a loop, which is whether this call can have written the array I am about to reload. It
propagates callee-to-caller over the condensation, and where a callee is unavailable, the answer is
the conservative one.

**Interprocedural constant propagation without cloning, roughly 500 lines,** at `-O2`. Jump functions
in all three forms of 34.4, propagation over the condensation, and the transformation restricted to
the case GCC also allows at `-O2`: a parameter that is the same constant in every call site becomes a
constant in the body, and the argument is dropped. No specialization, no clones. Cloning is `-O3` in
GCC and it should be `-O3` in rucc, and it needs document 33.3's predicated summaries to decide, which
means it arrives with them or after them.

**Unused parameter and return value removal, roughly 300 lines,** at `-O2`, the cheap half of 34.5's
IPA-SRA. This one needs care about ABI: a function whose signature changes must have every call site
updated, and any function whose address is taken or which is externally visible cannot be changed at
all.

**Escape analysis, already in M4** per document 08.4, is upgraded by the mod/ref summary from "does
the address leave this function" to "does the address leave this function *given what the callees
do*", which is a materially better answer and costs nothing extra once the summary exists.

Not in M4: cloning, ICF, splitting, aggregate parameter splitting, devirtualization, bit and range
propagation across the callgraph. Each is recorded above with its verdict.

## 34.7 The one structural decision: do summaries exist before LTO needs them

The three-phase structure of 34.0 costs real complexity: a summary type that is separable from the
function body, an explicit propagation phase, and a transformation phase that can run long after the
analysis. A single-unit compiler does not need any of that, because it can analyse and transform in
one traversal.

**The recommendation is to build the three phases anyway, and the reason is not LTO.** It is that a
summary which cannot be separated from the body also cannot be cached, cannot be dumped, cannot be
compared against a recomputation, and cannot be tested in isolation. Document 41's translation
validation and document 42's measurement both want to look at what an analysis concluded, and an
analysis that only exists as a side effect of a traversal offers nothing to look at. The LTO payoff
in document 35 is then free rather than a rewrite.

The concrete form: each IPA analysis produces a `Summary` value per function, serializable, stored in
a side table keyed by function id. Passes read summaries and write summaries. Only the transformation
phase touches IR.

## 34.8 How this is wrong

**A fact is derived from a body that is not the one that runs.** 34.1. Interposition, weak symbols,
comdat. This is the characteristic IPA miscompilation: it is correct in a static link, wrong in a
shared library, and it does not reproduce under the test suite because the test suite links
statically. rucc's rule: a function is *body-trusted* only if it is `static`, or it is defined in
this unit and `-fno-semantic-interposition` is in effect, and the flag defaults per GCC's rules.

**A summary is stale after a transformation.** Inlining changes what a function calls; IPA-SRA changes
its signature; cloning creates functions with no summary. GCC's `ipa_merge_fn_summary_after_inlining`
exists for this. rucc's rule follows document 04's invalidation discipline: a transformation that
changes a function's body or signature invalidates its summary, and the pass manager enforces it
rather than the pass remembering to.

**An SCC is not iterated to a fixpoint.** A recursive function's purity depends on itself. Starting
optimistic (`const`) and lowering on contradiction gives the right answer; starting pessimistic gives
"impure" for every recursive function, which is most of a functional-style C program's helpers. The
optimistic start is the same trick as document 14's SCCP and it must be paired with the same
discipline: the result is only sound after the fixpoint, so nothing may read the lattice mid-flight.

**A parameter is removed but a caller was missed.** Every call site, including calls through
pointers, must be updated together, which is why a function whose address escapes is excluded. An ABI
mismatch here is not a wrong answer, it is a crash.

**Varargs.** A variadic function's parameters cannot be removed or reordered, and jump functions for
the variadic part do not exist. Excluded outright.

**`setjmp`.** A function that may return twice invalidates the assumptions of any analysis that
reasons about a call returning once. GCC tracks `returns_twice` on the edge; rucc must too, and a
function containing a `returns_twice` call is excluded from purity and from parameter removal.

**Attribute lies.** A user declares a function `pure` and it is not. GCC believes the attribute. So
should rucc, and it should say so in the documentation, because the alternative is to not believe
attributes, which throws away most of the value available in C. Where the body is visible and
contradicts the attribute, a warning is the right response, not a silent override.

**The analysis is quadratic on a large unit.** The propagation is linear in edges per sweep, but
IPA-CP does `ipa-cp-sweeps` (3) full traversals, and jump function construction is bounded per
function by `ipa-max-aa-steps` (25,000). Both bounds must be adopted, and budget exhaustion must be
counted, per document 42's discipline.

## 34.9 What it costs, and what to measure

The infrastructure is one pass over every function to build the graph, plus a condensation, which is
Tarjan's algorithm and is linear. Each analysis is a bounded walk per function plus a fixpoint over
the condensation, so linear times a small constant on programs without large SCCs.

The memory is the summaries, and they are the reason IPA is a memory cost even when it is a small
time cost.

Document 42 owes four numbers.

- **What purity analysis is worth**, measured by run time with `ipa-pure-const` on and off. This is
  the cheapest pass here and the claim that it is the highest value per line should be checked.
- **What the local mod/ref summary is worth to redundant load elimination**, counted as loads
  eliminated across a call that would not have been. Document 08's measurement plan already wants the
  alias-layer breakdown and this is one more layer in it.
- **How many parameters are constant in every call site**, on the corpus, which prices the
  no-cloning IPA-CP directly and also says how much is left on the table by not cloning.
- **How often a function is excluded because it is not body-trusted**, split by reason. If most
  functions in a typical unit are excluded for interposition, the flag defaults matter more than the
  analysis does, and that is worth knowing before writing the analysis.
