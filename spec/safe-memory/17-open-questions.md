# Open questions

Ranked by how much of the specification depends on the answer. The parent's document 19 does the same job and this list extends it rather than replacing it: Q1-Q5 there stand, and these are seven more plus three deferrals.

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

## Question 6, Type-plane granule homogeneity

**The problem.** Document 05.2.3: the type plane at byte granularity is 4:1, and TySan pays 8x for the type plane alone. Tier D's 2x memory budget requires compressing it to roughly 1.25:1 by storing per-16-byte-granule a homogeneity flag plus a `TypeId`, with a per-byte side table only for heterogeneous granules.

**The claim that is unmeasured:** that real structures are overwhelmingly homogeneous per 16-byte granule. It is plausible (alignment rules cluster same-typed fields, arrays are homogeneous by definition, and pointer fields are 8-byte aligned) and it has never been measured.

**The cheap experiment.** Walk DWARF struct layouts over the corpus and count heterogeneous granules. This needs none of the type plane to exist and is a week of work, which is why document 16 puts it at S3 rather than at S5 where the type plane is built.

**The contingency, per document 09.8:** if compression does not work, the type plane moves behind its own flag and byte-granular type checking becomes a Tier D-strict option rather than the Tier D default. Written down now so it is a planned degradation rather than a crisis.

## Question 7, Is `-fsafety-subobject` ever enableable by default?

**The problem.** Document 09.4 catches intra-object overflow through the type plane rather than by narrowing capabilities, which is what lets `container_of` survive. But the check still distinguishes members only when their types differ: `struct { int a; int b; }` with an overflow from `a` into `b` is invisible, and `-fsafety-subobject=strict` catches it at the cost of the heterogeneous side table being used far more often.

**The question:** what fraction of real intra-object overflows cross a type boundary? If most do, the default form is nearly as good as strict for a fraction of the cost, and S4 could plausibly move into Tier D's default set. If most do not (and same-typed adjacent fields are extremely common) then the default form catches much less than document 03's S4 row implies and the row should say so.

**Answerable from the CVE corpus** once it exists: classify each intra-object case by whether the overflow crosses a type boundary. Document 16's S6.

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
