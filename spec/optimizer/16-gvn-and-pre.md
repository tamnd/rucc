# 16. Value numbering, redundancy elimination and hoisting

This is where a compiler removes work rather than making work cheaper, and on real C it is one of
the two or three largest wins available. It is also, at 14,013 lines in GCC
(`gcc/tree-ssa-sccvn.cc` 9,283 and `gcc/tree-ssa-pre.cc` 4,730), one of the largest pieces of the
middle end, and one where the ambitious version and the useful version differ by an order of
magnitude in effort.

## 16.1 Value numbering, and the part the e-graph already does

Two expressions are equivalent if they compute the same value. Hash-based value numbering assigns
each expression a number, equal expressions share a number, and a second occurrence is replaced by
the first.

In straight-line code this is a hash table walked in reverse postorder. **Document 12's
hash-consing is exactly this, happening continuously**, so under arms B or C of the experiment
there is no straight-line GVN pass to write: two occurrences of `a + b` were never two values.

The hard case is cycles in the SSA graph, which is to say loops. Consider a value defined by a
block parameter whose argument is defined in terms of the parameter. Is it equal to another such
value? Answering optimistically, assuming they are equal until contradicted, gets strictly better
results than answering pessimistically, for the same reason document 14.1 gives about SCCP.

GCC's answer is Cooper and Simpson's SCC-based value numbering (`gcc/tree-ssa-sccvn.cc:1`). Do a
DFS of the SSA graph, find strongly connected components, and iterate optimistically *within each
component only*, using a separate optimistic hash table. The source lists the two properties that
make it better than iterating the whole function
(`gcc/tree-ssa-sccvn.cc:23`): when an SCC is popped, everything outside it is already numbered, so
operands need no special handling; and the SCC walk is a DFS, so combining and simplifying can
happen in the same traversal.

**What rucc builds.** Hash-consing gives the acyclic case. The cyclic case needs the SCC walk, and
it is worth roughly 200 lines: Tarjan's SCC algorithm over the SSA graph, an optimistic table per
component, iterate the component to a fixpoint. The payoff is recognising that two induction
variables advancing in lockstep are one value, which is common in code that indexes two arrays
together, and which nothing else in the pipeline finds.

This is a *supplement* to the e-graph rather than a replacement for it, and the ordering matters:
run it after the e-graph is built, and have it union values in the e-graph rather than rewriting
the IR, so the extraction in document 12 sees the result.

## 16.2 Redundant load elimination, which is the real prize

Value numbering over pure arithmetic is worth less than people expect on C, because C compilers
mostly do not see the same arithmetic twice; the front end does not generate it and the programmer
does not write it.

Loads are different. `p->x` appearing three times in a function is three loads, and if nothing
writes through an aliasing pointer in between, two of them are redundant. On C code with pointer
chasing, structs, and no aliasing information from the language, this is the single largest source
of removable work.

The mechanism is document 09's: for each load, walk memory SSA back to its clobbering definition. If
two loads of the same address reach the same clobbering definition, they are the same value. If a
load reaches a *store* of the same address, the load is the stored value, which is store-to-load
forwarding and is worth more still.

Three refinements, in decreasing order of value.

*Partial overlap*, from document 09.5: a load reaching a store that covers it exactly is the stored
value; one covering it partially needs an extract; one covering it not at all continues the walk.
Getting the three-way distinction right is a correctness requirement, not a refinement.

*Through `memcpy`.* A load from a destination that a `memcpy` wrote is a load from the
corresponding offset of the source. This is GCC's `translate` callback (document 09.2) and it is
what makes struct assignment transparent. Worth building in M4 because struct assignment is
everywhere in C.

*Across a call.* A load before and after a `const` or `pure` call, or a call whose modref summary
says it does not write this object. Depends on document 08.4 and is mostly post-M4.

**Where it runs.** Redundant load elimination is a pass, not a rewrite rule, because it needs memory
SSA and a backwards walk. It is in the `-O2` list per document 03.4 and it should also be in the
`-O1` list in a restricted form: same block only, no phi translation, no memcpy. That version needs
no memory SSA, is perhaps 80 lines, and catches the repeated `p->x` in one basic block, which is
the majority of the opportunities. This is a refinement to document 03.4.

## 16.3 Partial redundancy elimination

An expression is *fully* redundant if it is computed on every path reaching it. It is *partially*
redundant if it is computed on some. PRE makes the second into the first by inserting the
computation on the paths that lack it, and then eliminating.

GCC implements GVN-PRE and its description at `gcc/tree-ssa-pre.cc:96` is the clearest statement of
the algorithm anywhere, so it is worth restating in its terms.

**AVAIL** is a forward dataflow problem: which values are computed on all paths to here. The source
notes at `gcc/tree-ssa-pre.cc:100` that in SSA there is no kill set, because values are never
killed, so AVAIL needs no fixpoint iteration. One pass.

**ANTIC** is backwards: which values *could* be computed here, meaning that if you inserted the
computation here it would be legal and useful. This one does need a fixpoint, because values are
not live over the whole function and must leave the set once you pass their definition. It also
needs **phi translation**: going backwards through a join, an expression must be rewritten in terms
of the values on each incoming edge. Phi translation is the hard part of PRE and it is where the
complexity lives.

**Insertion.** An expression is inserted where it is AVAIL in some but not all predecessors and
ANTIC in all of them (`gcc/tree-ssa-pre.cc:126`).

Two variations worth noting. At `-Os`, GCC only eliminates a partial redundancy when insertion is
needed in exactly one predecessor (`gcc/tree-ssa-pre.cc:133`), which it says "avoids almost
completely the code size increase that PRE usually causes". And *partial anticipation*, where a
value is anticipated on some path only, is handled separately and more conservatively.

**What rucc does.** PRE is the most expensive thing in this document and its value on C is
concentrated in one case: a load that is redundant on one path and not another, which is what a
loop with a conditional store produces, and what `if (p) x = p->a; ... y = p->a;` produces.

**M4 builds load PRE only, not full expression PRE.** AVAIL and ANTIC over memory values rather
than over all expressions, insertion of loads only, and only where insertion is needed on a single
predecessor edge, which is GCC's `-Os` restriction applied at every level. That restriction
eliminates the code growth, eliminates the need for a cost model, and keeps ANTIC's phi translation
to the load-address case.

Full expression PRE is post-1.0 and is gated on document 42 showing that the e-graph plus GCM plus
load PRE leave a measurable gap. GCM already performs a form of code motion that captures some of
what PRE would (document 12.5), and quantifying the overlap is a genuinely open question that
document 12's experiment should be extended to answer.

## 16.4 Code hoisting

Moving a computation *up* to a point that dominates several copies, making them all redundant.
Primarily a size optimization.

GCC piggy-backs it on PRE's ANTIC sets and lists five conditions at `gcc/tree-ssa-pre.cc:161`:

1. The value is in `ANTIC_IN(B)`, so it will be computed on all paths from B and can be computed
   in B.
2. It is not in `AVAIL_OUT(B)`, so it is not already there.
3. All successors of B are dominated by B, which the source admits is not strictly necessary but
   which "would complicate the hoisting pass a lot", and which holds for diamond-shaped regions,
   which is most candidates.
4. At least one successor has it in `AVAIL_OUT`, to stop it hoisting too far.
5. B has at least two successors, since hoisting in straight-line code is pointless.

Condition 4's justification is the useful part: "Experiments with SPEC and CSiBE have shown that
hoisting up too far results in more spilling, less benefits for code size, and worse benchmark
scores." That is the register-pressure tension from document 12.5 again, measured.

And the ordering note at `gcc/tree-ssa-pre.cc:193` is a genuinely useful pass-ordering fact of the
kind this project should be collecting: "code hoisting never exposes new PRE opportunities, but PRE
can create new code hoisting opportunities", so hoisting runs after each round of PRE and not
before.

**rucc's position:** hoisting is `-Os` and `-Oz` only, it runs after load PRE, and it hoists loads
and pure arithmetic under GCC's five conditions. At `-O2` it is off, because condition 4's
experimental result says the speed benefit is not there and the spilling cost is.

## 16.5 The order in the pipeline

Document 03.4's `-O2` list has `egraph`, `sccp`, `gvn`, `pre`, then jump threading. Refined by
this document:

1. `egraph` builds and rewrites; hash-consing handles acyclic value numbering.
2. `sccp` does the conditional constant fixpoint (document 14).
3. `gvn` is now two things: the SCC walk for cyclic equivalences (16.1), and redundant load
   elimination (16.2). They share the memory SSA and should be one pass.
4. `pre` is load PRE only (16.3).
5. At `-Os` and `-Oz`, `hoist` after `pre` (16.4).

Note what moved: full expression PRE is gone, and `gvn`'s arithmetic half is gone into the e-graph.
What remains is memory-centric, which is the right emphasis for a C compiler.

## 16.6 How this is wrong

**Two values are numbered equal that differ in poison.** `a + b` with `nsw` and `a + b` without are
not the same value; the first is poison on overflow. Document 12.7 already requires flags in the
hash key and this is the same bug reached from the other direction: the SCC walk's equality test
must use the same key.

**Optimistic value numbering is not run to a fixpoint.** As with SCCP in document 14.6, optimism is
only sound at convergence. Fuel gates the rewriting, not the analysis.

**A load is forwarded from a store of a different size.** The three-way distinction from 09.5. This
is the single most likely miscompilation in this document because the two-way version is what
somebody writes first and it is right most of the time.

**PRE inserts a load on a path where it faults.** Insertion is only legal where the load is known
safe: the address is dereferenced on that path anyway, or the object is known live and large
enough. ANTIC's definition ("could be computed here") must encode this, and "could" meaning "is
legal" rather than "is expressible" is the distinction. A safe-to-speculate predicate is needed by
this document, by 15.6 and by document 27, and it should be written once.

**Phi translation is wrong at a loop header.** Translating an expression backwards through a loop
header's parameters into the latch's arguments produces an expression in terms of the previous
iteration's values, which is correct and easy to get subtly wrong. This is where PRE bugs live and
it is the strongest reason M4's version is restricted to loads with simple addresses.

**Hoisting increases register pressure.** GCC measured this and made it condition 4. rucc's
version, being `-Os` only, cares less, but the same measurement should be repeated rather than
assumed.

## 16.7 What it costs

The SCC walk is one DFS of the SSA graph with per-component iteration, linear in practice.

Redundant load elimination is one memory SSA walk per load, budgeted per document 09.2. This is the
dominant cost and it is why the budget exists.

AVAIL is one pass. ANTIC is a backwards fixpoint with phi translation and it is the expensive part
of PRE; GCC's own PRE is one of the slower passes in the middle end. Restricting to loads bounds the
set size sharply, since the number of distinct load addresses in a function is much smaller than
the number of distinct expressions.

The measurement that decides whether full expression PRE ever gets built: on the corpus, with the
e-graph and GCM and load PRE in place, how many *arithmetic* computations does `gcc -O2` remove
that rucc does not. Document 42 owns it and it is one of the more informative numbers the project
can collect, because it directly prices the largest piece of GCC's middle end that rucc has chosen
not to build.
