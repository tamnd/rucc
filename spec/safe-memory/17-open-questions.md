# Open questions

Ranked by how much of the specification depends on the answer. The parent's document 19 does the same job and this list extends it rather than replacing it: Q1-Q5 there stand, and these are twelve more plus three deferrals.

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

## Question 3, Do PICO-class and CHOP-class elimination compose? Answered, they do not

**The problem.** Document 02's Tier E budget of 1.3x assumes static range-based elimination (PICO: 36% execution-time reduction over SoftBound) and profile-driven redundancy elimination (CHOP: ~80% of dynamic bounds checks avoided) *compose*. Neither paper evaluates against the other and the sources may overlap almost entirely, if CHOP's profile-identified redundant checks are largely the ones PICO already proves statically, the combined rate is close to the larger of the two rather than to their combination.

**Why it matters.** Document 13.3's decomposition shows the Tier E budget is at the optimistic end of its own prediction. If the elimination sources overlap heavily, Tier E is 1.5x-2x and is a draw with Fil-C rather than a win, which is document 02.6's first failure condition.

**How it gets answered.** Document 16's S4: discharge rate with each source enabled independently and together, over the corpus. This is a measurement, not a research question, and it is scheduled before anything depends on it.

**How it was asked.** A number for one source cannot be read off a full run's remarks. The rules are asked in an order and whichever one answers first is the one the remark names, so the second source to be asked about a check that two of them could answer looks like it answered nothing. So `crate::discharge` has a run per source, reached by `-fdischarge-objects`, `-fdischarge-dominance`, `-fdischarge-summaries` and `-fdischarge-narrow`, and a run that asks everything under a name of its own so that the parts and the whole are measured in the same place in the pipeline. They were asked with `-fno-hoist -fno-split -fno-unroll`, since a pass that copies a loop body copies the checks in it and a static count of what is left would then be a count of the copying. `cargo xtask cost` takes the same flags, so the clock was asked the same question the counts were.

**The static answer, and it is the encouraging half.** On the SQLite amalgamation at `-O2 -fsafety=detect` there are 102858 checks standing with nothing discharged. What an object says about itself takes 12201 of them, 11.9 percent. What a dominating check already established takes 16970, 16.5 percent. What every caller guarantees takes 2903, 2.8 percent. The three together take 26751, 26.0 percent, against 32074 for the three added up, so they overlap by 5323 checks and the combination keeps 83 percent of the sum. The 137 programs of `tests/safety` say the same thing at a different mix: 123, 58 and 2 checks of 689 alone, 144 together against 183 added up, 79 percent of the sum kept. So the sources are close to independent in what they prove, and the fear in the paragraph above, that one source's checks are largely the other's, is wrong as a statement about which checks get discharged.

**The dynamic answer, and it is the discouraging half.** The eight Tier E programs at `-O2` with neither kind of elimination run at 4.33x geomean. Adding the whole discharge pass and nothing else takes that to 4.09x, which is 7 percent of the overhead. Adding `hoist` and `split` and no discharge at all takes it to 1.73x, which is 78 percent of the overhead. Adding both takes it to 1.80x, and three more passes put the pair at 1.74x and 1.75x against 1.72x and 1.76x, so the two are the same number and the combination is worth nothing over the larger of them. That is exactly the failure this question was written about, and the direction is the opposite of the one it feared: the range-based side is the large one and the redundancy side is the one that vanishes into it.

**Why both halves are true at once.** The sources prove different checks and the checks they prove are worth wildly different amounts. A quarter of SQLite's checks is a real number and almost all of it is a check that runs once, in a function that sets something up, on a local this function declared. The clock is spent in loops, and in a loop `hoist` and `split` take the check out of the fast path whether or not anything could have proved it, so by the time a dominance rule could have been asked the check it would have answered is already gone. The one source that is range-based inside `discharge` shows this in miniature: asked after the other three it takes 57 more checks on SQLite, 0.06 percent, and 18 more on the corpus. It is not a fourth kind of fact. It is a way of asking the other three about a subscript instead of about an address the program wrote out, and `hoist` and `split` do the same widening on a scale that matters.

**What the number does not account for.** It is eight programs written to be pathologies, and a program whose hot loop the analysis cannot follow gets nothing from either side. It is also one arrangement of the pipeline: `discharge` runs after `split` and was not asked what it would do in front of it. Running it in both places is a cheap experiment and it is not this measurement. And the static counts are counts of checks and not of executions, which is the whole reason the two halves disagree, so neither one alone is the answer and the pair of them is.

**What that means for the design.** Tier E is 1.74x to 1.80x, not 1.3x, and document 02.6's first failure condition is the live one. The response is not another source of facts. Three of them together move the clock by 7 percent and a fourth would move it by less. It is the loop rows `split` still refuses, which `cargo xtask` reports and tamnd/rucc#810 leads: 3364 checks at 332 sites where what the address does round the loop is not something the analysis follows, 662 where a call in the loop might free what it reads, 598 in a loop inside the one being split. Those are checks in hot code that nothing discharges, and they are where the remaining 0.5x is.

## Question 4, Does call-frame elision fire often enough to matter? Answered at 22 percent

**The problem.** Document 05.3's out-of-band capability passing costs one TLS access and up to eight capability stores per instrumented call. Document 07.5 says a call between two functions in the same module, where the callee's checks are all discharged, can drop the frame entirely.

**The question is empirical:** how often does that fire on real code? Inlining removes many calls entirely, which helps. But cross-module calls without LTO, calls through function pointers, and calls to large functions all pay it every time, and document 13.7 names deep call chains of small functions as an expected pathology.

**If it does not fire often,** the alternative is the shadow argument register set that document 05.3 rejected, faster, and not expressible without changing the psABI, which is the thing that must not change if a kernel is the goal. There is no third option, so a bad answer here is a real cost rather than a redesign.

**The measured answer.** `--emit=safety-summary` classifies every call site, and on the SQLite amalgamation at `-O2 -fsafety=detect` there are 13724 calls that hand a pointer over and 3077 of them, 22 percent, go to a function in the same unit with no checks left. The rest split 9046 where the callee still checks something, 1350 where the callee is in another translation unit, and 251 through a pointer. That last pair is the surprise: cross-module calls and function pointers together are 12 percent, and document 13.7 expected them to be the whole story. They are not. Two thirds of the calls that keep their frame keep it because the callee still has checks in it.

**What the number does not account for.** rucc has no inliner, so this is the rate over every call the source wrote. Inlining takes calls out of both halves of the fraction and the direction it moves the rate is not obvious: the small leaf functions it would take first are the ones most likely to have no checks left, which is the numerator. So this is the rate on the calls that would survive an inliner and not a lower bound on the rate an inlining compiler would report.

**What that means for the design.** The elision rate is a function of the discharge rate and not of the module structure, so LTO is worth much less here than it looked and check elimination is worth more. It also means the rate is all or nothing per function: at `-O0` the rate is already 17 percent, because a function that never touches memory has nothing to check, and discharging a third of SQLite's bounds checks at `-O2` moves it only five points, since a function keeps its frame for its last surviving check exactly as much as for its first. The shadow register set is not called for at these numbers, but a per-call frame that carries only the capabilities the callee actually looks at would be, and that is a cheaper change than a psABI. It is not scheduled here because nothing publishes a frame yet.

## Question 5, The capability compression scheme. Answered, do not compress

**The problem.** Document 05.2.2 stores, per 8 bytes of payload, a 16-byte aux slot holding a version and a compressed `(lo, ext, meta)`. The compression is marked as a design decision not yet made, with CHERI-128's exponent-and-mantissa scheme as the straw man.

**What is at stake.** The scheme decides the representable-region error, a compressed bound is *wider* than the true bound by a bounded amount, which means a small out-of-bounds access can be missed. CHERI's scheme is well studied and its error bounds are known; adopting it wholesale is the low-risk path and costs a known, small amount of precision at large object sizes.

**The alternative** is an uncompressed 32-byte aux slot, which is 4 bytes of aux per byte of pointer-dense structure instead of 2, and is a straight trade of memory for precision. Document 05.5 predicts aux at 1.35x geomean; doubling it is probably not affordable.

**Why it was ranked here rather than higher:** it is a bounded engineering choice with a known-good default, and the cost of getting it wrong initially is a change to one structure in `rucc-safe-rt`. That ranking was right and the default turned out to be the wrong one anyway.

**How it was asked.** Representability is a property of two numbers rather than of a program. A range is exactly representable at a mantissa of `m` bits when there is an exponent `e` such that both ends land on a multiple of `2^e` and the length fits in `m` bits at that scale, and nothing in that condition mentions the code. So `cargo xtask compress` sweeps the ranges that can arise instead of building anything: whole heap allocations as the allocator places them today and again from an allocator that over-aligns every block to its own size, narrowed members up to 4 KiB, narrowed ranges over large arrays, and the mappings the boundary code recovers. Section 5.2.7 of document 05 holds the tables.

**The answer is that the straw man rounds in the wrong place.** A small member is exactly representable from twelve bits of mantissa upward, because the offsets inside a structure are pointer-aligned and the members are short. That is the reassuring half, since intra-object overflow is the class this design claims over Fil-C and compression is not where the claim would be lost. A whole heap object and a large array member are the other half, and at CHERI-128's fourteen bits only 77 percent of heap objects are exact. The slack is worst exactly where an overflow has the most room to run.

**No allocator fixes that, which is the part I had wrong when I started.** The obvious response is to put each block on whatever alignment its own size needs, which is what CHERI's allocators do, and the first version of this measurement assumed it made the error go away. It does not. Aligning the base settles one end, and the top still has to round up, because the length is the program's and a length that is not a multiple of the granule has nowhere else to go. What is left is a slack of about one part in `2^m` however the allocator behaves: at fourteen bits, up to 64 bytes past a 1 MB buffer and 4 KB past a 64 MB one. Those are bytes no bounds check can refuse, and a redzone allocator refuses them today, so adopting the straw man would be a monitor that is weaker than ASan on the largest objects in the program.

**What is adopted instead is exact or recover.** A full 64-bit `ver` and the 21 bits of `meta` that a check actually reads leave 43 bits, which is a 21-bit displacement, a 21-bit extent and one flag, and 21 is not a coincidence: it is the largest pair that fits, and it fits exactly. Both numbers are relative to the pointer stored in the word the slot sits beside, so `lo` costs nothing. Every object up to 2 MiB is then described with no error at all. Above that the flag is set and the capability is recovered from the address the way a pointer from uninstrumented code already is, which is a block lookup and a header load, on allocations large enough that one load is nothing. The 32-byte slot the question named as the alternative is not needed, so document 05.5's aux prediction stands.

**What the answer does not account for.** It is a bound on the hole rather than a prediction of what gets through it, since whether a program ever reaches the slack is a question about programs. It is unweighted, and the 2 MiB threshold is the number an allocation profile would move first, being chosen to fill the slot rather than from any count of how many live allocations are above it. The recover path's cost is asserted and not measured. And a hardware capability machine has whatever format it has, so on the lowerings of document 05.4 this decision does not arise.

**What it leaves behind.** Both 16-byte layouts are full, with the straw man's nine spare bits going to the flag and the displacement under this one. Judgement C1 wants the epoch a pointer word was written at held beside the capability's own version, and there is no room for it in either, so where that second stamp lives is a decision of its own rather than a corner of this one. It was tamnd/rucc#1069 and it is answered: both stamps live in the epoch plane, each at the address it is about, which costs no bits of the slot and no new mapping either, because the plane already covers the aux for the same reason it covers the header. Document 09.5 has the mechanism and what the second write costs.

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

**What running it showed.** The Tier D run confirmed the hole with a measurement nobody had to construct. The one refusal in `veryquick.test` that took a day to explain was `fillInCell` walking its source pointer forward through a record whose tail is a `zeroblob`. The buffer it walked off is a lookaside slot of about a kilobyte, and the refusal did not fire until the pointer had left the whole forty eight kilobyte lookaside block, thousands of bytes and several loop iterations later. Everything in between is inside one instance as far as the monitor is concerned, so an overflow of a lookaside slot into its neighbour is not merely undetected, it is undetectable, and the one report that did come out named an object the program does not believe it has. That is the false negative in the paragraph above, seen from the outside, and it also makes every report near a lookaside slot harder to read than it should be.

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

## Question 12, Should S5 report at the derivation or hand back a pointer that cannot be dereferenced?

**The problem.** S5 refuses when a program computes a pointer that leaves the object it was derived from, whether or not anything is ever read or written through it. Document 03.1 gives the reason under its table: the computation is undefined by 6.5.6 on its own, and refusing where the pointer is made rather than where it is used is what makes the report name the line with the bug in it instead of a line thousands of instructions later. Nothing about that argument has changed. What has changed is that there is now a run against a real program to hold it up against.

**What running it showed.** The first full pass over SQLite's `veryquick.test` at Tier D leaves five refusals and four of them are S5 on a pointer that is never read through. Two are loop guards in `ext/misc/zipfile.c` that form `p + k` in order to test whether `p + k` is in range, one is `test_prepare_v3` in SQLite's own test harness passing arithmetic on a null `zTail`, and one is `fillInCell` advancing its source pointer on every iteration after the payload has become a `zeroblob` and the copy has become a `memset`. Every one of them is undefined C and the rule is right about all four. Not one of them touches a byte outside any object, and the overshoots are six bytes, thirty bytes, a whole address space and several kilobytes, so the one-past-the-end window in document 03.1 does not reach any of them and cannot be widened to without giving the class away. So on the first corpus member the check reports four times and prevents nothing, and a program built this way stops on the first of them.

**The candidate answer**, which is CHERI's and is written down here so it can be argued with: permit the derivation, hand back a pointer that carries no capability, and let J1 report when something is read or written through it. On hardware with tagged capabilities this is what the machine already does, the out-of-range value is representable and simply untagged, and the fault arrives at the load. In software the equivalent is to let S5 produce a pointer whose capability is cleared rather than to stop, which costs nothing at the derivation and moves the report to the access that in these four cases never comes.

**What it gives up**, which is the reason this is a question and not a decision. It is exactly document 03.1's sentence: a report at the eventual dereference points at the line that used the pointer, and the line that computed it may be in a different function and a different file. Every intra-object overflow that S5 catches early would then be caught by J1 at the write, which is later and reads worse, and the ones that never dereference would not be caught at all. It also changes what J2 means in document 04. Today J2 is a judgement against a derivation; under the candidate answer a derivation cannot be judged and J2 becomes either a warning or nothing, and document 04's list of seven judgements is part of the report format that document 11 fixes.

**What would settle it:** classify the S5 refusals across more than one corpus member. If the shape stays what it is on SQLite, that is four false alarms to zero catches, and no amount of report quality pays for a check that only ever stops correct programs. If other members produce S5 refusals on pointers that are then written through, the early report is doing the work document 03.1 says it does and the answer is a narrower exemption, not this one. Either way a middle answer exists and should be measured alongside: keep S5 as a refusal, add a mode that demotes it to a logged warning carrying both the derivation site and, if an access ever happens, the access site, and see whether the demoted report is still good enough to find a bug with.

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
