# 18. Scalar replacement of aggregates

Take a local struct that lives in memory only because it is a struct, split it into one variable per
field, and promote each to a register. In a language with no aggregates this pass would not exist.
In C, where a `struct` is the primary means of organising data and where taking the address of one
is idiomatic, it is one of the most valuable passes there is, because everything downstream of it
works on scalars and nothing downstream works on memory.

Document 03.7 already argued for its position: SROA runs *before* the e-graph, because every scalar
it exposes is a value the e-graph can then reason about, and GCC agrees in the sense that
`pass_sra_early` (`gcc/tree-sra.cc:5255`) is the 44th of the 386 pass instances while the heavy
value-level work is a hundred and fifty entries later.

`gcc/tree-sra.cc` is 5,336 lines.

## 18.1 GCC's four stages

The header comment at `gcc/tree-sra.cc:23` lays out the algorithm and it is a good one.

**One: find candidates.** Declarations whose properties permit scalarization. A local, not
`volatile`, not variable-sized, address not taken in a way that defeats the analysis.

**Two: scan and record accesses.** Every read or write of any part of a candidate produces an
`access` structure recording base, offset, size and type (`gcc/tree-sra.cc:133`). Uses that defeat
scalarization remove the candidate. Assignments between two candidate aggregates additionally
produce an `assign_link`, which is what lets information flow between the two aggregates' access
trees.

**Three: analyse.** Sort accesses by offset and size. **Partially overlapping accesses disqualify
the whole aggregate** (`gcc/tree-sra.cc:71`): if one access covers bytes 0 to 3 and another covers
2 to 5, there is no consistent set of scalars. Non-overlapping or fully-nested accesses are fine,
and the nested ones form a tree where a child's extent lies within its parent's. Then propagate
accesses across the assign links, so that a field accessed in one struct implies the corresponding
field is worth splitting in a struct assigned from it.

**Four: rewrite.** Replace the accesses with scalars, insert the copies that struct assignment
implies, and leave the aggregate behind if some access could not be scalarized.

Two parameters bound it: `sra-max-scalarization-size-Ospeed` and `-Osize`
(`gcc/params.opt:1088`), the largest aggregate considered, different for size and speed; and
`sra-max-propagations` at `Init(32)` (`gcc/params.opt:1096`), how many artificial accesses are kept
per variable for the propagation in stage three.

GCC runs it twice, early and late, with one difference documented at `gcc/tree-sra.cc:49`: early
SRA does not scalarize unions returned from the function, because combined with inlining it
produces "weird type conversions". That is the kind of detail that only appears after a bug report.

## 18.2 What rucc's version looks like

The same four stages, and the same partial-overlap disqualification, which is not a heuristic but a
correctness requirement.

**The candidate set.** An `alloca` (`crates/rucc-ir/src/opcode.rs`) whose address does not escape,
is not `volatile`, and has a statically known size below the threshold. Document 08.4's escape
analysis is the prerequisite and it is the same whitelist-based predicate; a missed escape here
does not miscompile, it just wastes the pass's time, because a scalarized aggregate whose address
escaped would fail the access scan. But that is fragile reasoning and the escape analysis should be
correct rather than relied on to be conservative.

**The access scan.** Every `load`, `store`, `memcpy`, `memset` and `ptr_add` chain rooted at the
candidate. A use that is not one of these, an address passed to a call, an address stored
somewhere, a `ptrtoint`, disqualifies.

**The interesting case: `memcpy` and `memset`.** In C, `a = b` between structs lowers to `memcpy`,
and `struct S s = {0}` lowers to `memset`. If SROA does not understand these two, it disqualifies
almost every struct in real code. So they are handled specially: a `memcpy` between two candidates
is the assign link from stage two, and a `memset` to zero over a candidate becomes a zero store to
every scalar. Getting these two right is most of the value of the pass on C.

**Bitfields.** A bitfield access is a load, a shift and a mask over a storage unit. Two bitfields in
the same unit produce accesses at the same offset and size, which is the group case rather than the
overlap case, and they scalarize to one variable holding the unit. Two bitfields straddling a unit
boundary produce partial overlap and disqualify. That is correct and it is why bitfield-heavy
structs optimize poorly in every compiler.

**Unions.** A union accessed through two members at the same offset with different types is exactly
the partial-overlap case if the sizes differ, and the same-offset-same-size group case if they do
not. Type punning through a union, which document 08.1 established rucc must support, therefore
mostly survives SROA by disqualifying the union, which is the conservative and correct outcome.

## 18.3 The relationship to mem2reg

`mem2reg` promotes an `alloca` that is only loaded and stored as a whole into an SSA value. SROA is
the generalisation: an `alloca` accessed in disjoint pieces becomes several values.

They should be one pass, not two. `mem2reg` is the case where the access tree has a single node
covering the whole object. Writing them separately means two implementations of the same
escape check, the same access scan and the same rewrite, and it means a struct that `mem2reg`
cannot handle falls through to a pass that runs later in the pipeline.

But note that document 03.4 puts `mem2reg` in the `-O0` pipeline as its entire content, and SROA is
not appropriate at `-O0`: it is not free, it changes what a debugger sees, and `-O0`'s job is to
compile fast. So the pass takes a mode. At `-O0` it promotes only whole-object accesses; at `-O1`
and above it does the full analysis. One implementation, one flag.

## 18.4 What it unlocks, which is the real argument

SROA's own benefit is modest: some loads and stores become register moves. Its value is almost
entirely in what it exposes to other passes.

A struct field in memory is invisible to constant propagation (document 14), to value numbering
(16), to range analysis (10) and to the e-graph (12), all of which work on SSA values. Promote it
and all five apply. A pointer field promoted to a value can be tracked by alias analysis; before
promotion it is a memory location that every call might write.

This is why the ordering in document 03.7 matters, and it is also the strongest argument for SROA
being in `-O1`, which document 03.4's `-O1` list already has. A `-O1` without SROA has a
value-level pipeline that cannot see inside any struct, and C code is mostly structs.

## 18.5 What is deliberately not built

**Interprocedural SRA.** GCC's `SRA_MODE_EARLY_IPA` (`gcc/tree-sra.cc:105`) splits aggregate
parameters across the call boundary, so a function taking a struct by value takes its fields
instead. This is document 34's, it depends on the call graph, and it is post-M4. It is worth
noting that it is one of the higher-value IPA transformations on C++ and a much lower-value one on
C, where passing structs by value is less common.

**Scalarization of arrays with variable indices.** An array accessed at a constant index
scalarizes; one accessed at `a[i]` does not, and no amount of cleverness changes that without
turning the array into a switch. GCC does not do it either.

**Splitting aggregates in global storage.** A static struct is not a candidate, because its address
is observable across the module and because nothing else in M4 reasons about globals.

## 18.6 How this is wrong

**Partial overlap is missed and two scalars alias.** The stage-three sort and overlap check is the
correctness core of the pass. Its test suite is a set of structs with hand-chosen overlapping
accesses, including through unions, through bitfields, and through casts of the address.

**An access is scanned as smaller than it is.** A `memcpy` whose size is a variable, an access
through a pointer with an unknown offset, a `ptr_add` by a value the analysis did not constant
fold. Every one of these must disqualify rather than being assumed to cover nothing. The default
for an unrecognised access is disqualification, and there is no case where the pass proceeds on an
access it did not fully understand.

**Volatile is scalarized.** A `volatile` field means the whole aggregate is out.

**Padding is scalarized and something reads it.** C says the padding bytes of a struct have
unspecified values, but `memcpy` of the struct copies them, and a program that compares two structs
with `memcmp` observes them. SROA that drops padding and then rebuilds the struct for a `memcpy`
produces different padding. This is legal by the standard and it breaks real programs. The safe
rule: an aggregate that is ever copied *as a whole* to something outside the candidate set keeps
its memory form, and only its scalarizable accesses are additionally promoted.

**Debug information is lost.** After SROA the variable `s` does not exist; `s.a` and `s.b` do.
`-Og` does not run SROA per document 03.4, which sidesteps this, and at `-O2` the DWARF must
describe `s` as a composite location made of its pieces. That is `spec/12-debug-info.md`'s problem
and this document owes it the information: SROA records, per split, which scalar corresponds to
which offset and size of the original, and hands it over.

## 18.7 What it costs

Stage two is one walk of the function per candidate, so linear in uses. Stage three sorts the
accesses per candidate, so `n log n` in accesses. Stage four rewrites.

The parameter that bounds it is the aggregate size threshold, and its purpose is not compile time
but code quality: splitting a 400-byte struct into 100 scalars creates 100 live values and the
register allocator spills all of them, producing worse code than leaving it in memory. GCC has
separate thresholds for size and speed for this reason.

rucc's thresholds start at GCC's defaults, and document 42 measures the alternative directly:
compile the corpus at several thresholds and read the numbers. This is one of the few parameters in
the whole optimizer where the right value is likely to be genuinely different for rucc than for
GCC, because it interacts with the register allocator in document 39 and the allocators differ.
