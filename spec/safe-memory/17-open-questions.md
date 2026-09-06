# Open questions

Ranked by how much of the specification depends on the answer. The parent's document 19 does the same job and this list extends it rather than replacing it: Q1-Q5 there stand, and these are eleven more plus three deferrals.

The ranking covers questions 1 to 7, which were written together. Questions 8 and up were added later, as reading real code turned them up, and they are in the order they were found rather than in rank order, because renumbering the list would break every reference to it.

The discipline is the parent's: a question here is one where **the specification is genuinely undecided**, not one where the answer is known and the code is unwritten. The second kind belongs in document 16.

## Question 1, Aliased kernel mappings

**The problem.** The direct map, `vmalloc` space, `kmap`, per-CPU aliases and userspace mappings can all name one physical page. A shadow keyed by virtual address gives that page several independent plane entries which can disagree. An object freed through its direct-map address leaves the `vmalloc` alias's lifetime plane saying live, and a stale access through the alias is missed.

**Why it is ranked first.** It is the only genuinely *unsolved* problem in the set, everything else is a choice between known options or an unmeasured number. Document 02.6's third failure condition is this question going badly: if aliased mappings have no clean provenance answer, Tier K tops out below "the kernel" and the honest claim shrinks to "kernel subsystems that do not manipulate physical addresses."

**The three candidates,** with what would decide between them:

*Physical keying* is correct by construction and costs a virtual-to-physical translation per check, a subtraction for the direct map, a page-table walk for `vmalloc`. Decidable by measuring the frequency of `vmalloc`-space accesses on hot paths, which nobody has measured for this purpose.

*Canonicalization at alias creation* is cheap on the check path and costs `meta_begin`/`meta_end` in proportion to the alias count. Its risk is completeness: one alias-creating path not interposed is a silent hole.

*Restricting the claim* scopes Tier K's soundness to direct-map accesses and counts the rest as trust-set entries. Honest, immediately implementable, and strictly weaker than KASAN, which does handle `vmalloc` shadow.

**The plan** is restrict first, measure how often it bites, then canonicalize the paths that matter, holding physical keying in reserve. Document 16 puts the measurement at S7 and makes it the milestone's first item.

**What would change the answer:** a measurement showing `vmalloc` accesses are under a fraction of a percent of hot-path accesses would make physical keying viable and settle this cleanly.

## Question 2, Checks and the ægraph

**The problem.** The parent's document 19 question one asks whether the ægraph, which came from a Wasm JIT, carries to an AOT C compiler. This is its corollary: checks trap, so they are control-dependent side effects, so document 06.2.5 puts them in the CFG skeleton and outside the e-graph, and redundant-check elimination becomes a dominator-tree walk rather than an e-graph rewrite.

**The question.** Is that the right split, or is there a formulation in which a check *is* an e-graph node, one where the e-class carries the trap condition as part of its cost, so that a rewrite that makes a check redundant is the same kind of object as a rewrite that makes an add redundant?

**Why it matters.** If checks were e-graph nodes, elimination would compose with every other rewrite automatically, which is exactly the property that makes an e-graph worth having. As specified, we get the arithmetic sharing and none of the elimination composition, and the elimination pass is an ordinary dataflow pass with all the ordinary risks.

**The honest state:** the CFG-skeleton answer is known to work and is what document 06 specifies. The e-graph answer would be better if it exists, and nobody in the ægraph literature has done it for trapping instructions. This is a research question with a safe fallback, which is the best kind to have.

## Question 3, Do PICO-class and CHOP-class elimination compose?

**The problem.** Document 02's Tier E budget of 1.3x assumes static range-based elimination (PICO: 36% execution-time reduction over SoftBound) and profile-driven redundancy elimination (CHOP: ~80% of dynamic bounds checks avoided) *compose*. Neither paper evaluates against the other and the sources may overlap almost entirely, if CHOP's profile-identified redundant checks are largely the ones PICO already proves statically, the combined rate is close to the larger of the two rather than to their combination.

**Why it matters.** Document 13.3's decomposition shows the Tier E budget is at the optimistic end of its own prediction. If the elimination sources overlap heavily, Tier E is 1.5x-2x and is a draw with Fil-C rather than a win, which is document 02.6's first failure condition.

**How it gets answered.** Document 16's S4: discharge rate with each source enabled independently and together, over the corpus. This is a measurement, not a research question, and it is scheduled before anything depends on it.

## Question 4, Does call-frame elision fire often enough to matter?

**The problem.** Document 05.3's out-of-band capability passing costs one TLS access and up to eight capability stores per instrumented call. Document 07.5 says a call between two functions in the same module, where the callee's checks are all discharged, can drop the frame entirely.

**The question is empirical:** how often does that fire on real code? Inlining removes many calls entirely, which helps. But cross-module calls without LTO, calls through function pointers, and calls to large functions all pay it every time, and document 13.7 names deep call chains of small functions as an expected pathology.

**If it does not fire often,** the alternative is the shadow argument register set that document 05.3 rejected, faster, and not expressible without changing the psABI, which is the thing that must not change if a kernel is the goal. There is no third option, so a bad answer here is a real cost rather than a redesign.

## Question 5, The capability compression scheme

**The problem.** Document 05.2.2 stores, per 8 bytes of payload, a 16-byte aux slot holding a version and a compressed `(lo, ext, meta)`. The compression is marked as a design decision not yet made, with CHERI-128's exponent-and-mantissa scheme as the straw man.

**What is at stake.** The scheme decides the representable-region error, a compressed bound is *wider* than the true bound by a bounded amount, which means a small out-of-bounds access can be missed. CHERI's scheme is well studied and its error bounds are known; adopting it wholesale is the low-risk path and costs a known, small amount of precision at large object sizes.

**The alternative** is an uncompressed 32-byte aux slot, which is 4 bytes of aux per byte of pointer-dense structure instead of 2, and is a straight trade of memory for precision. Document 05.5 predicts aux at 1.35x geomean; doubling it is probably not affordable.

**Why it is ranked here rather than higher:** it is a bounded engineering choice with a known-good default, and the cost of getting it wrong initially is a change to one structure in `rucc-safe-rt`.

## Question 6, Type-plane granule homogeneity, answered at 8 bytes

**The problem.** Document 05.2.3: the type plane at byte granularity is 4:1, and TySan pays 8x for the type plane alone. Tier D's 2x memory budget requires compressing it to roughly 1.25:1 by storing one entry per granule and falling back to a per-byte side table only for heterogeneous granules.

**What was unmeasured:** that real structures are overwhelmingly homogeneous per granule. Alignment rules cluster same-typed fields, arrays are homogeneous by definition, and pointer fields are 8-byte aligned, so it was plausible, and the document picked 16 bytes without evidence.

**The measurement, now done.** `--emit=type-granules` walks every record the front end has typed and paints its bytes, and document 05.2.5 has the method and the curve. The answer is that the compression works and 16 bytes is the wrong granule. SQLite is 64.8% heterogeneous at 16 bytes and costs 2.84 bytes of plane per byte of program against a budget of 1.25, and 12.6% heterogeneous at 8 bytes and costs 1.00. The reason is not subtle once you see it: on a 64-bit target the unit of a distinct type is 8 bytes, so a 16-byte granule holds two of them and `struct { char *p; int a; int b; }` alone is enough to make it disagree. Eight bytes is the minimum of the curve on both inputs measured. The specification now says 8.

**Two things fell out of it.** At 8 bytes the two keyings give the same numbers, so distinguishing one pointer target type from another is free and the plane can afford to be precise about it. And a union is a choice and not a coexistence, which sounds obvious and was got wrong in the first version of the measurement; counting a union's members as sharing bytes inflated the 16-byte figure from 65% to 71% and would have made the case against 16 for the wrong reason.

**What is still open.** The measurement is static and weighted by declared size, so it is a pessimistic bound on what a run-time heap pays: a program whose heap is mostly uniform buffers pays less than its declarations suggest, and every program's heap is more uniform than its type table. It also covers records only, and it says nothing about how often a heterogeneous granule is actually touched. The run-time number needs the plane to exist, which is S5.

**The contingency, per document 09.8, is not needed.** It stays written down because a later corpus member could still move the curve, and because the fallback is a flag rather than a redesign.

## Question 7, Is `-fsafety-subobject` ever enableable by default?

**The problem.** Document 09.4 catches intra-object overflow through the type plane rather than by narrowing capabilities, which is what lets `container_of` survive. But the check still distinguishes members only when their types differ: `struct { int a; int b; }` with an overflow from `a` into `b` is invisible, and `-fsafety-subobject=strict` catches it at the cost of the heterogeneous side table being used far more often.

**The question:** what fraction of real intra-object overflows cross a type boundary? If most do, the default form is nearly as good as strict for a fraction of the cost, and S4 could plausibly move into Tier D's default set. If most do not (and same-typed adjacent fields are extremely common) then the default form catches much less than document 03's S4 row implies and the row should say so.

**Answerable from the CVE corpus** once it exists: classify each intra-object case by whether the overflow crosses a type boundary. Document 16's S6.

## Question 8, Who annotates a sub-allocator that lives inside the program?

**The problem.** Document 03's carving row and document 10's `__rucc_alloc_split`, `__rucc_alloc_merge` and `__rucc_alloc_adopt` were written with jemalloc, tcmalloc, mimalloc and the kernel slab in mind, which are four allocators, all of them known, all of them worth patching by hand once. Document 18 found that SQLite ships its own, the lookaside allocator, on by default, carving one 120 kilobyte `malloc` per connection into fixed slots and threading a free list through the freed slots themselves.

**Why that is different.** The four named allocators are the allocator, so annotating them is a one time cost that every program inherits. A per program sub-allocator is not inherited by anything, there is one in most large C programs, and nobody outside the project is going to write the annotations. Unannotated, the monitor sees one instance where the program sees thousands, so every overflow between slots and every use of a freed slot is invisible. **This is a false negative, not a false positive**, which means it does not trip document 03's release-blocking rule and it does not show up in any test that only checks for spurious reports. A silent hole is worse than a noisy one.

**The question, stated so it can be answered:** is the interposition API enough, or does Tier D need to *detect* carving rather than be told about it? The detectable shape is narrow and might be recognizable: a single allocation, walked by a constant stride, with each stride start stored into a list. If it is recognizable then this is a pass, and the pass has to be sound in the direction that matters, which is that failing to recognize a carve is the safe outcome.

**What would settle it:** count sub-allocators across the corpus in document 12, and for the ones found, measure what fraction of the program's small objects come from them. If SQLite's number holds up, most objects in most large C programs come from a sub-allocator and the API alone is not enough.

**Related, from the same audit:** SQLite's default `sqlite3MemMalloc` puts an eight byte size header in front of every allocation and returns an interior pointer. Nothing is out of bounds, but the recorded instance is eight bytes wider at the front than the object, so an underflow of eight bytes or fewer is undetectable everywhere in the program. Same fix, much smaller stake.

## Question 9, Bulk writes that begin at one member and cross several

**The problem.** Document 03's `container_of` row is about deriving the enclosing object from a member pointer. Document 18 found the other direction in SQLite: `PARSE_HDR` and `PARSE_TAIL` produce `char*` pointers into the middle of a `Parse` structure and then `memset` or `memcpy` a run of bytes that spans many members, and `MEMCELLSIZE` copies a prefix of a `Mem` sized by an `offsetof`. Both are ordinary and neither is `container_of`.

**Why it is not already answered.** Y2 and Y3 are quiet because the access is through a character type and through `memcpy`, both of which are in the model. S1 is quiet because the whole run is inside the object. The only thing that objects is S4, which narrows a capability to the member the pointer was derived from, and would reject the run at the first boundary it crosses. So this is entirely an S4 question, and S4 is off by default, which is why it is a question rather than a blocker.

**The candidate answer**, written down so it can be argued with: a pointer derived from a member and immediately converted to a character type is not narrowed, because C's character type rules already say that such a pointer addresses the object representation of the whole object. That would make S4's narrowing apply to typed member access and not to byte access, which is both simpler and closer to what 6.5 says, and it may give away too much, since byte access is exactly how intra-object overflow is written in the bugs S4 exists to catch.

**What would settle it:** the S6 classification in question 7 already has to look at every intra-object case in the CVE corpus. Add a column for whether the overflowing access was through a character type. If most real intra-object overflows are typed, the exemption is cheap; if most are byte writes, it guts the check.

## Question 10, What pervasive address exposure costs

**The problem.** Under PNVI-ae-udi a cast from a pointer to an integer *exposes* that storage instance, and an exposed instance is one about which the compiler may assume much less, because a later integer-to-pointer cast may recover it. Document 07 discharges checks by proving things about provenance, and exposure is precisely the thing that stops those proofs.

**What the audit found.** SQLite converts pointers to integers on a scale nobody anticipated when document 07 was written. `SQLITE_WITHIN` and the open coded comparisons in the free path mean every single call to `sqlite3DbFree` casts the pointer being freed to `uptr` and compares it against arena bounds. No pointer is ever recovered from an integer anywhere in the program, so nothing is a violation and nothing is a false positive. But under a literal reading of the exposed address rule, every object SQLite frees is exposed.

**The question:** does exposure for the purpose of comparison have to count as exposure? Comparison cannot recover a pointer, so an instance exposed only through comparisons is not actually ambiguous, and the analysis that proves an integer never reaches a cast back to a pointer is a small local one. If that analysis is sound, the cost is nothing. If it is not, document 07's discharge rate on real programs is much worse than its estimates, which were made on code that does not do this.

**What would settle it:** implement the discharge pass, measure the rate on SQLite with the comparison exemption and without it, and report both. That is a number document 13 should be printing anyway, so the marginal cost of answering this is one flag.

## Question 11, Should the aux plane be in the block at all?

**The problem.** Document 05.2.2 puts the aux array in the same allocation as the payload, following Fil-C, on the argument that the aux then arrives with the data. Document 13.5 asked for that to be measured before anything depended on it, and the thing it was supposed to decide was narrower: whether an adopted third-party allocator, which has to use a shadow, is acceptable.

**What the measurement said.** Document 05.2.6. Shadow-mapped aux was never worse than in-block aux on trips to memory across seven access patterns, was better on five of the seven on page walks, and used three to four times less heap. The narrow question is answered and document 10.4 is corrected: an adopted allocator is fine.

**What is now open, which is the wider question nobody asked.** If shadow is not worse, the case for the in-block layout is no longer performance. It is that the header, the aux and the payload are one allocation, so `free` is one call, `cap_of` is a subtract and a load, and there is no second address space to reserve, size, or map lazily. Those are real and they are the reasons to keep it. But they are engineering convenience rather than the reason document 05 gives, and a design that reserves a shadow anyway for the range planes of 05.2.3 is already paying most of the cost of a shadow.

**Why this is not decided here.** The measurement is a simulation with no prefetcher, no frees and no allocation-time zeroing, and the last of those is the one that favours the current layout. Switching the layout would rewrite the allocator, the `cap_of` lowering, and the boundary recovery of document 10, which is most of S1 and S2. That is not a change to make on a simulator.

**What would settle it:** at S5 there is a monitor and a corpus. Build one project both ways and report the same table against real hardware counters. If shadow still wins on real traces, the in-block layout stays only if its allocation-path and `free`-path savings pay for the loss, and that is then a measurement too rather than an argument.

## Deferrals

Not questions, decisions already made to not do something, recorded so that "why isn't this here" has an answer.

**Deferral 1, Whole-program type inference.** CCured's inference, the SEI's Pointer Ownership Model, and the LLM-assisted completion in CMU/SEI-2025-TR-008 would all raise the discharge rate substantially. They need whole-program visibility that LTO gives only within a link unit, and the corpus's libraries are shared objects. Post-1.0. Document 07.5.

**Deferral 2, Sound whole-program abstract interpretation.** Frama-C's Eva and Verasco are the right technology for proving checks away rather than eliminating them locally, and building one is a decade. The narrow-verified-rule approach is chosen because its failure mode is a surviving check (a performance bug) rather than a missing one. Document 07.9. *This is also the boundary with the compile-time-proof specification at `../compile-time-safe-memory/`, which takes the opposite position and should be read against this one.*

**Deferral 3, Machine-learned elimination policies.** The parent's document 01 rules MLGO out for the optimizer on the grounds that the gains are 1% and the plumbing is a year. The same applies here, with the additional consideration that a learned policy is not verifiable, which is the property document 14.2 is built on.

## The questions the parent already owns

Restated only as pointers, because they bear on this specification and should not be re-answered here:

- **Parent Q1, the ægraph's transfer from a JIT to an AOT compiler.** Question 2 above is its corollary.
- **Parent Q5, what the no-poison uninitialized-read model costs.** Document 09.2.1 argues the model is a *requirement* for a monitor, so this specification raises the price of answering that question the other way. Document 15.6's first row.

## What is not an open question

Stated because a list of open questions is read as a list of everything uncertain, and these are decided:

**Lock-and-key rather than garbage collection.** Document 08.2. The kernel decides it and the decision is not revisitable without abandoning Tier K.

**Narrow pointers rather than fat pointers.** Document 05.1. The ABI decides it.

**Checks inserted before the optimizer rather than after.** Document 06.1. It is the reason this design can beat a sanitizer's cost, and it is only safe because the parent already has a verified rule DSL.

**The soundness claim's three escape hatches.** Document 04.5. Coverage, boundary, declared exemption. Removing any of them would make the claim false rather than stronger.
