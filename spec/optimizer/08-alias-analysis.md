# 08. Alias analysis

This is the analysis with the highest ratio of downstream value to lines of code, and also the one
where being wrong is least survivable. Every memory optimization in documents 16, 17 and 27 is
gated on it. A points-to answer that is too conservative costs performance quietly and forever. A
points-to answer that is too aggressive miscompiles, and it miscompiles in the way that produces
a bug report three years later from someone whose program worked on every other compiler.

GCC spends roughly 23,800 lines on this: `gcc/tree-ssa-alias.cc` at 4,509,
`gcc/alias.cc` at 3,588, `gcc/tree-ssa-structalias.cc` at 2,152 plus the two files GCC 16 split
out of it, `gcc/pta-andersen.cc` at 2,565 and `gcc/gimple-ssa-pta-constraints.cc` at 4,189, and
`gcc/ipa-modref.cc` at 5,682 with `gcc/ipa-modref-tree.cc` at 1,121. That split of the points-to
analysis into a constraint generator and a solver is new and is worth copying as an organising
principle regardless of which solver goes behind it.

## 8.1 The shape of the question

There is one primitive and everything else is built on it: given two memory references, can they
touch the same byte. GCC's answer is `refs_may_alias_p_1` at `gcc/tree-ssa-alias.cc:2574`, and its
worker `refs_may_alias_p_2` at `gcc/tree-ssa-alias.cc:2399` is worth reading in full because its
structure is the specification for rucc's.

It decomposes both references into a base plus an offset plus a maximum size, and then dispatches
on what kind of base each has: two declarations goes to `decl_refs_may_alias_p`
(`gcc/tree-ssa-alias.cc:2047`), a declaration and an indirect reference goes to
`indirect_ref_may_alias_decl_p` (`gcc/tree-ssa-alias.cc:2102`), and two indirect references go to
`indirect_refs_may_alias_p` (`gcc/tree-ssa-alias.cc:2266`). Three cases, and each is a different
argument.

Three details in that function are not obvious and all three matter.

**Two volatile accesses always conflict**, checked before anything else. Not "may conflict":
they are treated as conflicting so that neither can be reordered around the other, which is what
`volatile` is for.

**Offset-based disambiguation runs before TBAA when both bases are declarations**, and the comment
at `gcc/tree-ssa-alias.cc:2461` says exactly why: "to handle must-alias cases in conformance with
the GCC extension of allowing type-punning through unions". This is a compatibility fact rucc must
reproduce. Writing through one union member and reading another is undefined in ISO C and is
defined by GCC, an enormous amount of real C relies on it, and getting the *layer ordering* right
is how you support it without disabling TBAA.

**The `restrict` implementation is two integers.** `gcc/tree-ssa-alias.cc:2503`:

```c
/* If the accesses are in the same restrict clique... */
&& MR_DEPENDENCE_CLIQUE (rbase1) == MR_DEPENDENCE_CLIQUE (rbase2)
/* But based on different pointers they do not alias.  */
&& MR_DEPENDENCE_BASE (rbase1) != MR_DEPENDENCE_BASE (rbase2))
  return false;
```

A memory reference carries a clique number and a base number. Clique zero means no information.
Same clique plus different base means no alias. That is the whole mechanism, it is exactly the
"scope tree, not a blanket assumption" that spec 9.4 asks for, and it is far cheaper than the
phrase "implemented properly with the scope tree" suggests. rucc should take this design directly:
two `u16` fields on every memory operation, assigned when a `restrict` scope is entered during
lowering.

The one trap GCC has already hit is at `gcc/tree-ssa-alias.cc:469`, PR71062: restrict information
may not be used to optimize *pointer comparisons*, only accesses. Two restrict pointers that do
not alias can still compare equal, because `restrict` constrains what is accessed through them and
not what their values are. A rewrite rule that folds `p == q` to false because they do not alias
is wrong, and it is the kind of wrong that looks obviously right. Document 13's rule set must not
contain it and there must be a test.

## 8.2 The six layers, revisited

Spec 9.4 lists six layers and the list is good. What follows is each layer with what it costs and
what changed.

**1. Trivially distinct storage.** Two distinct `alloca`s never alias; an `alloca` whose address
never escapes cannot alias anything indirect; a global and a local never alias. Free, and it
answers a startling fraction of queries in real code. This is the layer to build first and the
only one `-O1` needs.

**2. Provenance.** Per the PNVI-ae-udi model in `spec/07-semantics.md`. Two pointers with
different provenance do not alias. This is the layer that makes stack and heap disambiguation
work and it is what GCC gets, less principledly, out of tracking base declarations. rucc's version
is more principled and should be cheaper, since a provenance identifier is a small integer
attached at the point the object came into existence, not a tree walk.

Worth being explicit: provenance is *not* the same as points-to. Provenance says which object a
pointer was derived from, which the IR knows locally. Points-to says which objects a pointer might
hold at runtime, which needs a whole-module fixed point. Layer 2 is cheap because it is the first
question.

**3. TBAA.** GCC's is `same_type_for_tbaa` at `gcc/alias.cc` plus the component-reference machinery
at `gcc/tree-ssa-alias.cc:1254` and `gcc/tree-ssa-alias.cc:1922`, and it is complicated because
GIMPLE types are complicated. rucc attaches an alias-set identifier to each memory operation
during lowering, derived from the effective type, and the query is an integer comparison against a
small alias-set lattice. `-fno-strict-aliasing` sets every alias set to the universal one, which
makes the flag a one-line change rather than a condition threaded through the analysis.

Note the comment at `gcc/tree-ssa-alias.cc:4329`, "Alias sets are not stable across LTO
streaming". rucc's alias sets must be defined by a canonical serialisation of the type, not by
allocation order, or document 35's LTO will silently lose or, worse, gain disambiguations.

**4. Offset-based.** Same base, constant offsets, non-overlapping ranges. Cheap and it must run
before TBAA on same-base accesses, per 8.1.

**5. `restrict`.** The clique-and-base scheme from 8.1.

**6. Points-to.** Here is the one open question in this document and it is worth taking seriously.

## 8.3 Steensgaard or Andersen

Spec 9.4 commits to Steensgaard: "near-linear time, less precise, and the precision difference on
C code is smaller than the compile-time difference", with document 19 recording that it may need
revisiting. Document 05.8 flags this as one of three places where the literature does not have the
number we need.

The honest statement of the tradeoff. Steensgaard is unification-based: `p = q` merges the
points-to sets of `p` and `q` into one equivalence class, and the analysis is a union-find over
the whole program, near-linear. Andersen is inclusion-based: `p = q` adds an edge, and the
analysis is a dynamic transitive closure, cubic in the worst case and much better in practice with
cycle elimination. GCC uses Andersen, citing Pearce, Kelly and Hankin's cycle elimination work at
`gcc/tree-ssa-structalias.cc:68`.

The reason Steensgaard is attractive for rucc is not primarily speed. It is that a unification
solver is perhaps 400 lines and an Andersen solver with the cycle elimination that makes it
tractable is 2,565 lines in GCC 16, and the difference is not just typing: the Andersen solver has
worst cases, needs tuning, and is a place bugs live.

The reason to doubt it is the standard criticism, which is that unification is *symmetric* and
assignment is not. `p = q` in Steensgaard makes `q` point to everything `p` does, which is not
implied by the program, and the imprecision compounds through every assignment in the module. On C
code with a lot of pointer copying, which is all C code, the merged classes can grow to cover
most of the heap.

**The resolution.** Layer 6 is the *last* layer consulted. Layers 1 through 5 answer most queries
and they answer the ones that matter for local optimization. Layer 6 exists for the queries the
others cannot touch, which are mostly about globals and about pointers passed between functions.
Its imprecision therefore costs less than the literature's benchmarks suggest, because those
benchmarks measure points-to precision in isolation and rucc's is one voice in six.

Build Steensgaard. Instrument it: record, per compilation, how many queries reached layer 6 and
how many of those it answered "no alias". If layer 6 answers no-alias on fewer than 5% of the
queries that reach it, it is not earning its slot and the answer is to delete it rather than to
upgrade it. If it answers 20% and the merged classes are large, that is the evidence for Andersen
and document 05.8's missing number now exists.

The structural insurance is GCC 16's own split. Generate constraints in one module, solve in
another, behind a trait. Swapping the solver is then a contained change rather than a rewrite,
and it is the reason to imitate that refactor on day one rather than after it hurts.

## 8.4 Calls, and the thing that is actually expensive

The query "does this call clobber this reference" is answered by `call_may_clobber_ref_p_1` at
`gcc/tree-ssa-alias.cc:3091` and "does this call read it" by `ref_maybe_used_by_call_p_1` at
`gcc/tree-ssa-alias.cc:2838`, and without interprocedural information the answer to both is yes
for anything the call could reach, which is anything whose address escaped. In a program with a
call in every loop, that is the end of memory optimization.

GCC's answer is `ipa-modref`, 5,682 lines of interprocedural mod/ref summaries recording which
parameters a function reads, which it writes, and at what offsets. It is one of the highest-value
things in modern GCC and it is also, at that size, out of scope for M4.

**What rucc does in M4** is the cheap 80%, in three parts.

*Attributes.* `const` and `pure` on a declaration mean no writes, and no writes plus no reads of
mutable memory, respectively. GCC has these, C code uses them, glibc's headers are covered in
them, and honouring them is a table lookup.

*Known libcalls.* `memcpy` writes its destination and reads its source and touches nothing else.
`strlen` reads and writes nothing. There are perhaps forty of these and they are the calls that
appear in hot loops. Document 20 owns the list; this document owns the fact that the list is also
an alias fact.

*Escape analysis.* A local whose address never escapes the function cannot be touched by any call.
This is a bit per `alloca`, computed by walking uses, and it is the single most valuable
interprocedural-flavoured fact available without interprocedural analysis, because it covers every
local struct that a C programmer takes the address of only to pass a field.

The full mod/ref summary is document 34's, at `-O2` with LTO, post-M4.

## 8.5 Attribution, which is not optional

Spec 9.4 requires `-fdump-alias` to say *which rule* concluded no-alias, and this is the single
best decision in that section. The query returns not a boolean but a small enum: `MayAlias`, or
`NoAlias(Reason)` where `Reason` names the layer. It costs one byte in a return value that is
already going in a register.

Three things it buys. A miscompilation from an alias bug is localised to one layer in one dump
rather than bisected across the whole analysis, which is the difference spec 9.4 describes as a
week versus an hour. The layer statistics in 8.3 come for free. And a user asking "why did you not
optimize this" gets a real answer, which per document 05.6's remark about machine-facing
diagnostics is worth more now than it was.

## 8.6 How this is wrong

**The union type-punning ordering gets missed** and a program that has worked on GCC for twenty
years breaks. The test is a union with an `int` and a `float`, written as one and read as the
other, at `-O2`, checked for the value GCC produces.

**Alias sets are assigned by allocation order** and two compilations of the same translation unit
disagree, or LTO merges two modules whose set numbers mean different things. The defence is that
alias sets are derived from a canonical type encoding and there is a test that compiles the same
file twice and compares.

**`restrict` is applied to a pointer comparison**, per PR71062. One test.

**Escape analysis misses an escape.** A pointer stored into a struct that is later passed by
address, a pointer passed to an unknown call, a pointer cast to an integer and back, a pointer
whose address is taken by inline assembly. Each of these is an escape and missing any one of them
is a miscompilation. The rule for the implementation is that escape analysis is written as a
whitelist: a use is non-escaping only if it is one of a small enumerated set of opcodes, and every
other opcode escapes, including opcodes added later. Writing it as a blacklist means the next
person to add an opcode introduces a miscompilation without touching this file.

**Provenance is confused with points-to** and layer 2 is asked a question only layer 6 can answer.
The type system should keep them apart: a `Provenance` is not a `PointsTo` and there is no
conversion.

## 8.7 What it costs and how that is measured

Layers 1 through 5 are constant time per query and the analysis they need is built during
lowering, so their cost is a bit per memory operation in the IR and nothing at optimization time.
This is the argument for doing the work in the front end rather than recovering it later, which is
what GCC's 4,509-line `tree-ssa-alias.cc` largely is: recovery of information the front end knew
and discarded.

Layer 6 is a module-wide fixed point run once at `-O2`. Its cost is reported separately in
`-ftime-report`. The thresholds: if it exceeds 5% of `-O2` compile time, it is too expensive for
what 8.3's instrumentation says it delivers; if the instrumentation says it answers well and the
cost is under 2%, the Andersen question is worth reopening.

Document 42 also owns the precision measurement, which is not the same as either: compile a corpus
with layer 6 forced off and count how many loads GVN eliminates. That number, and not any
published points-to benchmark, is what the layer is worth to rucc.
