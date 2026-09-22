# Check elimination, and the right to perform it

This is where the cost budget is won or lost, and it is the document that distinguishes this design from an instrumentation pass. Document 06 inserts a check for every conjunct of every judgement at every memory operation. That program is roughly 4x. Everything between 4x and Tier E's 1.3x happens here.

The governing constraint, stated first: **an unsound elimination is a silent loss of safety.** An unsound ordinary optimization produces a wrong answer, which a differential test finds. An unsound check elimination produces a *correct* answer on every test and an undetected vulnerability in production. Nothing observes it. That asymmetry is why every rule in this document is data in the parent's rewrite DSL and is SMT-verified before it may ship.

## 7.1 What the literature says is achievable

The numbers to calibrate against, from document 01:

- **CCured** (POPL 2002) infers that most or all pointers in many C programs are statically type-safe, and instruments only the rest. This is the shape of the whole enterprise.
- **PICO** discharges in-bounds checks with Presburger formulas and replaces many checks with one placed at a cold point: **36% average execution-time reduction and 24% code-size reduction over SoftBound on SPEC**.
- **CHOP** uses profile data to build sufficient conditions for redundancy: **about 80% of dynamic bounds-check instructions avoided**, up to 95.8% improvement over SoftBound.
- **Baggy bounds** lands at ~30% overhead by making the *representation* cheap rather than by eliminating checks, which is the complementary lever and is document 05's job.

Tier E's 1.3x budget assumes PICO-class static elimination and CHOP-class profile-driven elimination **compose**, which nobody has demonstrated. Document 17 question 3 records it as an assumption and document 16 puts the measurement in S4, before anything depends on it. It has now been measured and the assumption is false. The two do not compose on the clock: section 7.4's loop work takes 78 percent of the Tier E overhead off, section 7.3's redundancy walk takes 7 percent off on its own, and the pair take the same 78 percent as the first one alone. They do compose in what they prove, which is the reason it took two numbers to see, and question 3 has both.

## 7.2 The four sources of a discharge

A check is discharged when a fact implies its conjunct. Facts come from four places, in increasing order of cost and decreasing order of yield.

**From the frontend.** The overwhelming majority of accesses in real C are to a local, a global, or a field of an object whose type is known, at a constant offset. `s.f` where `s` is an `alloca` of a known type has statically known bounds, statically known liveness within the scope, statically known effective type, and known alignment. Every conjunct is discharged at insertion time and no check instruction is ever created. This is not an optimization; it is the frontend not being stupid, and it is where most of the win is. Document 06 section 6.3 folds constant-offset derivations for the same reason.

**From dominance.** A check that has already been performed on the same capability and a range containing this one, with nothing in between that could change the answer, is redundant. This is the dominator-tree walk in section 7.3 and it is the classical redundant-check elimination.

**From induction-variable range analysis.** `for (i = 0; i < n; i++) a[i] = 0;` with `a`'s extent known to be at least `n` needs one check, before the loop, not `n` checks in it. This is the PICO result and section 7.4.

**From annotations and summaries.** `__counted_by(n)` on a parameter tells the callee its bound without a dynamic recovery. Interprocedural summaries carry the same information for un-annotated code. Section 7.5.

**What each one is worth, measured.** `-fdischarge-objects`, `-fdischarge-dominance` and `-fdischarge-summaries` run `crate::discharge` with one of these turned on and the rest off, and `-fdischarge-narrow` runs it with all three and no range widening. On the SQLite amalgamation at `-O2 -fsafety=detect` with the loop passes off, out of 102858 checks the frontend's facts take 11.9 percent, dominance takes 16.5 percent, summaries take 2.8 percent and the three together take 26.0 percent against 31.2 percent added up. So they overlap by about a sixth and the list above is a list of four different things, which is what it claims to be. The third one is the surprise. Asked after the other three it takes 0.06 percent more, because it is not a fourth kind of fact at all: it is a way of asking the other three about a subscript instead of about an address the program wrote out, and section 7.4's passes do that widening on the scale that matters. Document 17 question 3 has the rest of it, including the part where none of this shows up on the clock.

## 7.3 Redundancy over the dominator tree

The core algorithm, run in `rucc-opt` as a pass over the CFG skeleton, since document 06 section 6.2.5 puts checks outside the e-graph.

State is a map from capability to a set of *established facts* (proved-in-bounds ranges, liveness, initialized ranges, established types) propagated forward over the dominator tree with a kill set.

```
check_bounds %c, %p, n   is redundant if   ∃ established (%c, lo', ext')
                                            with  [%p, %p+n) ⊆ [lo', lo'+ext')
check_live   %c, %p      is redundant if   %c is established live and no
                                            meta_end, free, or scope exit
                                            reaching this point could have ended it
check_init   %c, %p, n   is redundant if   a meta_init or a store covering
                                            [%p, %p+n) dominates
check_type   %c, %p, ..  is redundant if   a meta_type establishing a compatible
                                            type over the range dominates
```

The kill sets are where the correctness lives and they are the part a hand-written pass gets wrong:

- `check_live` facts are killed by **any call that might free**, which without interprocedural information is any call at all. This is the reason temporal checks are far harder to eliminate than spatial ones and is the honest explanation for why Tier E cannot get near CHERI's 2%: bounds are a property of the pointer and liveness is a property of the world.
- `check_init` facts are killed by any store that could be to the same range, which is an alias-analysis query, and the parent's document 09 alias analysis, being founded on the same PNVI-ae-udi model, answers it in the same terms.
- `check_type` facts are killed by anything that changes the effective type, which is a `memcpy`, a union member store, or an untyped store.
- **Nothing kills a bounds fact except a redefinition of the capability**, which is why bounds elimination is the one that works well.


The `check_init` line of that table is in, and its kill set is not the one the bullet above it names. A store makes bytes initialized and cannot make them uninitialized again, so a store to the same range does not kill an initialization fact and neither does any other write of a value. What kills one is storage becoming fresh, which in the IR is `meta_begin`, and a copy carrying an uninitialized source over the range, which is `meta_init_copy`, and anything that could end the instance and hand the address back out, which is a call without `nofree`, `meta_end`, `meta_transfer` and inline assembly. The bullet's alias query is the right shape for the opposite question, which is a fact about a range the pass wants to keep narrow, and is not needed for this one.

The facts a passed `check_init` establishes are kept apart from the bounds facts rather than merged with them, and they are dropped at a call rather than carried across it the way bounds facts are. There is a version of the bounds argument that would carry them, since storage handed back out arrives with a capability whose version no longer matches and the lifetime check beside the access is what notices, but that rests on the lifetime check still being there, which is true only because the pass drops the lifetime facts at the same point. Resting one rule on another rule's conservatism wants a measurement first. What it costs is counted: 2382 of the SQLite amalgamation's initialization checks are ones a dominating check answered with only a call in the way.

The `check_type` line is in beside it and its kill set is the one the bullet names, with one thing the bullet does not say. A `meta_type` naming an entry puts that entry over the bytes it covered and leaves every other byte where it was, so a range recorded as agreeing with that same entry agrees with it still, wherever the write landed. That matters because a store through a type is the commonest instruction in the kill set and the read beside it usually asks at the type it was stored through. So the pass keeps one set of ranges per plane entry, a `meta_type` gives up every set but its own, and a `meta_type_copy`, a `meta_begin` and anything opaque give up all of them. The union member store and the untyped store the bullet names arrive as the same instruction with a different entry on it, and they are covered by giving up every other set.

What a passed `check_type` establishes is not quite what the check is named after. The plane over those bytes is compatible with the entry the check asked with, which is the plane holding that entry or the plane holding the untyped one, and the pass has no way to tell which. That is enough to answer a later check asking with the same entry and it is not enough to answer one asking with any other, so the entry is the key of the map rather than a field of the fact, and a lookup that misses is the honest answer for every other type.

What the dominance source alone removes, at `-O2 -fsafety=detect`: 5096 of the amalgamation's 26990 type checks, 921 of zlib's 2718, and 1103 of Lua's 5754. Fewer than the initialization half in every case, and the amalgamation has fewer type checks than initialization checks to start with because a read whose payload names no type asks nothing of the plane.

What the dominance source alone removes, at `-O2 -fsafety=detect`: 7951 of the amalgamation's 27574 initialization checks, 1626 of zlib's 2718, and 1528 of Lua's 5754. The rest divide into the ones nothing in front of them says anything about, which is the majority, the ones a call stands in the way of, and the ones whose pointer is a base and a computed index rather than a base and a constant, which is the shape section 7.4 is about and is where `a-matrix-multiply` lives.

**The hoisting rule.** A check may be hoisted to a dominating block only if every path from the hoisted position to the original reaches the original (that is, only to a block that the original post-dominates) because otherwise the program traps on a path where it would not have. Loop-invariant checks hoisted out of a loop with a possibly-zero trip count are the standard error here and the rule above forbids it. The correct transformation for that case is the guard in section 7.4.

**One call for two questions.** What the discharge leaves behind on a read that needs both plane checks is two calls where one would do. A `check_type` and a `check_init` in front of the same read take the same address and the same width, they carry descriptor rows that say the same thing because both of them are judgement J1, and the runtime answers them with the same function up to which plane it ends at. Both have to find the region the address is in before they can ask anything, and finding the region is the expensive half: it walks the published regions and builds all four planes over the one it finds, where each plane query after that is a handful of shifts and a load. So the pair is fused at lowering into one call that asks the type plane and then the init plane and reports the first refusal, which is the refusal the two separate calls reported, because the type call was the one that ran first. Fusing at lowering rather than in the IR is what keeps the two fact sets of the paragraphs above independent: a read whose type check or whose init check the discharge has taken out still lowers the survivor on its own.

What the fusion has to check rather than assume is that the two are still one read's. Same address, same width, same block, and nothing in between that writes memory or ends the block, and a second check of either kind in between ends the search rather than being walked past. The discharge takes one of the two out without the other often enough that a type check fused with an init check belonging to some later read is a real risk, and that would be an init question asked earlier than the program asks it, which is a refusal of a correct program. Over the SQLite amalgamation at `-O2 -fsafety=detect` the pair is recognised 19159 times, against 2735 type checks and 464 init checks that lower on their own, so nine in ten of the plane checks that survive the discharge are half of a pair. On the clock it takes between 3 and 22 percent of the whole program off every one of the eight Tier E rows, the most of it on `a-string-scan`, which goes from 8.92x to 6.93x, and `a-matrix-multiply`, which goes from 22.42x to 17.60x.

## 7.4 Loops: one check instead of n

The transformation that matters most, because array loops are where the checks are.

```c
for (i = 0; i < n; i++) sum += a[i];
```

Insertion produces a `check_bounds` on `&a[i]` inside the loop. Range analysis over the induction variable establishes `0 ≤ i < n`, so the accessed range is `[a, a+n*sizeof)`. The transformation replaces `n` dynamic checks with one:

```
%ok = check_bounds %c_a, %a, n * sizeof(T)     ; hoisted, guards the whole loop
loop: ... no check ...
```

Legal only when the loop is *counted*: trip count known before entry, no early exit that could be taken before an out-of-bounds access would occur, no store in the loop that could change `%c_a`. When there is an early exit, hoisting the check makes a program that would have exited before the bad access trap instead, which is a false positive, which document 02 says is a release-blocking bug. The correct form in that case is PICO's: keep a check but make it cheaper, or split the loop into a checked prologue and an unchecked body over the provably safe range.

**A loop that ends on a test it may step over.** A count that is settled only if the counter arrives at the limit is not a trip count known before entry, and `while (p != end)` is that shape. If the limit is behind the counter the loop runs until the counter wraps, which is very many iterations rather than the small number the subtraction gives, and a check written on the strength of the small number covers a fraction of what the loop reads. So a count resting on the counter approaching its limit is refused here rather than clamped, because there is no number of iterations to clamp: the clamp at zero that pays for a loop possibly running no times says nothing about a loop running a great many. Refusing costs nothing measured. No loop in the SQLite amalgamation and none in the corpus reaches the refusal, because a counter that does not also promise not to wrap is already refused a step earlier.

**The address that does not move.** The degenerate case of the same transformation, and worth naming because it looks like it belongs to LICM and does not. A check inside the loop whose address is the same every time round is one check's worth of bytes, so what goes in front of the loop is the check itself with nothing computed. LICM will not do it, because moving something that can trap in front of a loop needs the loop to be known to run and LICM answers that with post-dominance over the loop it was handed; the conditions in this section are a stronger answer to the same question and the pass doing this already has them. Redundancy elimination will not do it either, because there is only ever the one instruction and taking out a redundant second check is what that pass is for.

**Bounding the count.** The hoisted check covers the count times the step, so the transformation is only sound if that multiplication cannot wrap, and establishing that means bounding the count before the loop starts. For a count read out of a type narrower than the arithmetic the width of the type is the bound, which is where `for (int i = 0; i < n; i++)` gets its answer. For a count as wide as the arithmetic the width says nothing, and the bound has to come from range analysis at the preheader: `for (size_t i = 0; i < n; i++)` under a guard on `n` is bounded by the guard, and the preheader is on the far side of it. That covers most of what real code writes, since a length is usually checked before it is walked. A length nothing anywhere bounds, a `strlen` result walked to the end being the case that shows up in practice, keeps its check, and getting that one means bounding what a call returned, which is section 7.5's territory rather than this one's.

**Loop splitting** is the general form and is what gets the last of it: compute `m = min(n, extent/sizeof)`, run `[0, m)` with no checks at all, and run `[m, n)` (usually empty) with checks. The unchecked body is then eligible for the vectorizer and for everything else the parent's document 09 does, which matters because a bounds check in a loop body does not merely cost its own instructions, it *blocks* every transformation that needs the body to be side-effect-free.

The extent is the half of that a compiler cannot work out on its own, so it is a question asked at run time. `cap_extent %c, %p, %want` answers with how many bytes from `%p` on the capability covers, never more than `%want`, and it lowers to `__rucc_extent`. The limit is an operand rather than something the runtime picks because answering means walking the lifetime plane while a capability does not yet carry its own bounds, and a walk that stops at the number of bytes the loop was going to read is bounded by an eighth of the work the loop is already doing. An answer smaller than the truth costs iterations in the checked half and is never unsound, which is what makes stopping early allowed; an answer larger than the truth would put an access past the end of an object in the half that has no checks in it, which is the one way this transformation can turn a caught bug into an uncaught one. Once a capability carries its bounds the query is a subtraction and the limit is one `min`.

**In bytes, not in iterations.** The `m` above is written as an iteration number because that is how the transformation reads, but a compiler that carries the iteration number has to get from it to an address, and that means a multiply by the step going in and a divide by the step coming out. Both are symbolic, both are at the machine's full width, and the claim that has to hold for the unchecked half to be allowed to drop its checks is then that `i * step + reach <= extent` for every `i` under `(extent - reach) / step + 1`. That claim is true and z3 does not finish on it: not in two minutes signed, not in two minutes with a divisibility hypothesis, not in two and a half minutes unsigned with four-gigabyte bounds on every term. So a pass written that way sits outside section 7.7, and it sits there for a solver reason rather than a design one.

Carry the byte offset instead. It starts at zero, it goes up by the step every time round exactly as the address does, the window is `extent - reach`, and the claim is that an offset at or below the window names an access inside the object. No multiply and no divide, and it is the claim `swept.sym.i64` already makes, so the pass asks the table rather than deciding. The per-iteration cost is the same instruction for instruction, an add and a compare either way, and the preheader loses a divide per moving access. Two accesses that walk by the same amount are at the same offset on every iteration, so they share the offset and the smaller of their two windows. tamnd/rucc#817.

The one hypothesis this owes the rule is that the offset cannot wrap, since an offset that wrapped comes back small and the guard would wave it through. The window is held short of `i64::MAX - step`, which is one comparison and one select in the preheader and is so far past any object a program allocates that it never fires. It is there because the failure it stops is silent.

**A walk from high to low.** A loop whose address goes down each time round is the same transformation looked at from the other end, and writing it so that it is the same code is worth a paragraph because the obvious way to write it is unsound. The offset carried round the loop counts bytes moved rather than bytes added, so it goes up here exactly as it does going up and the guard, the block parameter, the clamp and the test are unchanged. What turns over is which end of the object the runtime is asked about, and `cap_extent_back %c, %p, %want` is that question: how many of the `%want` bytes ending at `%p` belong to whatever owns the byte below it, lowering to `__rucc_extent_back`. It is asked at the end of the first access rather than at its start, so the window is again the answer less the reach and the access on iteration `delta` is the `reach` bytes ending at `first + reach - delta`. That is `swept.down.sym.i64`, the mirror of the rule above in every part, and the pass asks it rather than asking the ascending rule and subtracting somewhere of its own.

Anchoring the query at the lowest address the loop reaches is the unsound way, and it is unsound for a reason worth recording. Where that anchor sits depends on how far the loop goes, so it needs a real trip count, and this transformation takes loops nobody counted and gives them the same guess of ten that everything else guessing about a loop uses. A guess costs nothing going up, where asking for too little only moves iterations into the checked half. Going down it decides where the verified range starts, and a guess that is too small puts the range above where the loop actually reads. So the query goes the other way instead of the anchor. tamnd/rucc#680.

**One loop, both kinds of access.** A loop that walks an array and reads a header field on every iteration has an address that moves and an address that does not, and the two want different arithmetic. The moving one has a window and an offset that walks; the still one is at offset zero on every iteration, so what there is to work out is whether its one access fits, and how far the runtime is asked to look for it is just the bytes it reads. Every access has to fit for the unchecked half to be the one that runs, and a still one that does not fit sends the whole loop down the checked half rather than putting a bound on it, because an access that would fail fails on the first iteration as much as on the last. Deciding this once for the plan rather than once per access is what tamnd/rucc#818 was: a still access has a step of zero, and asking it how many iterations it allows divides by that step.

That last point is worth stating plainly: **the largest cost of a check in a hot loop is not the check, it is the optimizations it prevents.** Any measurement that counts check instructions understates the cost, and document 13's methodology accounts for it.

## 7.5 Interprocedural facts

Three mechanisms, in increasing order of ambition.

**Annotations as hints.** `__counted_by(n)`, `__sized_by(n)`, `__counted_by_or_null(n)` and `__ended_by(p)` from Apple's `-fbounds-safety` are accepted verbatim, because the kernel already writes them and because their semantics are documented and deployed on millions of lines of production C. In this design they are **not** required for safety (an un-annotated parameter still gets a recovered capability and still gets checked) they are facts that let the checker discharge without a dynamic recovery. That reframing is important: it means adoption is monotone. Annotating a header makes the program faster and never changes whether it is safe.

`-fsafety-suggest-annotations` emits, per un-annotated pointer parameter, the bound the profile observed, in the form of a patch. That is the tooling that makes annotation adoption tractable and it is nearly free given the machinery.

**Summaries.** For each function, `rucc-lto` records what the parent's document 09 section 9.8 already carries, plus: which pointer parameters are dereferenced and over what range, which are freed, which escape, and whether the function can free memory at all. The last is the one that unlocks temporal elimination, a call to a function summarized as `nofree` does not kill liveness facts, and `nofree` is true of a very large fraction of leaf functions.

**Whole-program inference.** CCured's type inference, the SEI's [Pointer Ownership Model](https://doi.org/10.1145/3814943.3816182), and the LLM-assisted completion in CMU/SEI-2025-TR-008 are all in this category, and all of them are out of scope before 1.0. Document 17 deferral 3. The reason is not that they do not work; it is that they need whole-program visibility that LTO gives us only within a link unit, and the corpus's libraries are shared objects.

## 7.6 Eliminating plane maintenance

The writes are as expensive as the checks and are less studied. Three rules.

**Dead metadata elimination.** A `meta_type` or `meta_init` whose range is entirely overwritten by a later one on all paths, with no intervening check that reads it, is dead. This is dead-store elimination over the planes and it uses the same machinery.

**Plane-write coalescing.** A loop that stores a scalar array element by element performs `n` `meta_init` bit-sets. Coalesced into one range operation before or after the loop, by the same counted-loop analysis as section 7.4. The same applies to `meta_type` over a `memset`.

**Aux elision by escape analysis.** A `cap_store` is only needed if some other code can `cap_load` the slot. If a structure never escapes the function and every pointer field's capability is available in a register at every use, the aux traffic disappears entirely. This is ordinary escape analysis and it is where the most memory-traffic savings are, because per document 05 the aux traffic is the real cost. `mem2reg` gets the easy cases before the optimizer starts.

That last sentence turned out to be the whole story, and the rule as written above fires on nothing. It was measured at the end of the pass pipeline, which is where the aux writes that survive everything else are, on four libraries built at `-O2 -fsafety=detect`. Every `cap_store` was classified by where its destination pointer comes from, and the ones the rule is about are the ones writing into a local the function can name, that does not escape, and whose capability slots nothing reads back.

| | SQLite | libwebp | zlib | Lua |
| --- | --- | --- | --- | --- |
| `cap_store` reaching the end of the pipeline | 2585 | 954 | 186 | 750 |
| destination is a parameter of the function | 773 | 378 | 93 | 300 |
| destination is a pointer read out of memory | 525 | 33 | 26 | 164 |
| destination is the result of a call | 416 | 37 | 25 | 90 |
| destination is a block parameter | 210 | 18 | 0 | 122 |
| destination is a global | 40 | 421 | 0 | 8 |
| destination is a local that escapes | 574 | 59 | 42 | 66 |
| destination is a local whose slots are read back | 47 | 8 | 0 | 0 |
| **the rule fires** | **0** | **0** | **0** | **0** |

The zero is not an accident of these four libraries, it is what the rule asks for. A pointer written into a local that nothing else can see and that nothing reads back afterwards is a dead store in the ordinary sense, so plain dead store elimination has already taken it away, and it takes the `cap_store` with it because the two travel together. By the time anything could ask the escape question there is nothing left in the category. Every local-destination `cap_store` still standing is in one of the two rows above the zero, 574 and 47 on SQLite and 59 and 8 on libwebp, and in both of those rows the aux write is genuinely needed.

Counting local destinations with no reader but ignoring escape gives the ceiling for the whole direction: 261 of SQLite's 2585, 23 of libwebp's, 10 of zlib's and 21 of Lua's. All of those are blocked on the escape question and none on the reader test, so a sharper escape analysis is the only thing that could ever move the number, and ten percent of the aux writes is the most it could pay back. Widening the rule from stack slots to allocation results that do not escape, which is the cheap version of the same idea, reaches 8 `cap_store` on SQLite and 3 on Lua.

Where the aux traffic actually is, is the other half of the measurement. Three quarters of SQLite's `cap_store` and half of libwebp's write through a pointer the function cannot name at all, meaning a parameter, a pointer that was itself read out of memory, the result of a call, or a block parameter. Those need a caller to say something about the object, which is section 7.5 and document 11's summaries, or they need the whole program. libwebp's 421 writes into a global are the one group a link unit could answer on its own, and they are 44 percent of its aux traffic. An escape analysis, however good, reaches none of this.

**What may never be eliminated:** `meta_begin` and `meta_end` for a storage instance whose address escapes, and `meta_transfer`. Ending a lifetime is the event that makes future checks correct; skipping it is not an optimization, it is a bug that manifests as a missed use-after-free.

## 7.7 The rules are data, and they are verified

Every transformation in sections 7.3 through 7.6 is expressed in the parent's document 09 rewrite DSL, in a `safety/` rule namespace under `rucc-codegen`'s rule tree alongside the middle-end and lowering rules, per the parent's document 18 packaging constraint. `rucc-verify` covers them. The one exception is aux elision, which has no data form because it has no instances: the census above is the reason, and a rule that fires on nothing is not worth a row in the table.

**What is verified.** For each rule of the form "check *C* may be removed in context *Γ*", the obligation is

> for all machine states satisfying Γ, C does not trap.

Encoded as an SMT query over bitvectors in exactly the manner of [Crocus](https://cs.wellesley.edu/~avh/veri-isle-preprint.pdf), which the parent's document 10 already commits to. This is far cheaper than verifying a program, because a rule is small and the context is an explicit hypothesis rather than something to be inferred. It is the same reason Alive2 verifies transformations rather than compilers.

**What is not verified, and is therefore the weak point.** The *analysis* that establishes Γ (the range analysis, the alias query, the dominance walk) is ordinary code and is not verified. A rule that correctly says "removable if the range is provably within the bounds" is useless if the range analysis says a range is within bounds when it is not. Two mitigations:

**Differential check accounting.** Build the corpus twice, once with elimination and once without, run both over the same inputs, and assert that every violation reported by the unoptimized build is also reported by the optimized one. A missed report is an unsound elimination, found automatically, on real code. This is the check-elimination analogue of the parent's document 15 differential execution and it is the highest-value test in this specification. Document 14 section 14.3.

**Randomized elimination fuzzing.** Csmith and YARPGen programs are already generated free of the undefined behavior we would be detecting; inject a memory error into a generated program at a known point, compile at both settings, and assert both report it. Document 14 section 14.4.

## 7.8 Auditability: `--emit=safety-summary`

Every discharge is attributable, and this is a feature no existing tool has.

For each memory operation in the module, the summary records: which conjuncts of J1 were required, which were discharged, and by which rule at which source location established the fact. For each translation unit: the number of checks emitted, discharged and remaining, per class; the number of declared exemption regions and their reasons; the number of storage instances exposed by pointer-to-integer casts, per document 04 section 4.3; and the number of boundary-recovered capabilities, per document 05 section 5.3.

It also records what document 05 section 5.3's call frame would cost, per call site: how many calls hand a pointer over at all, and of those how many go to a function this unit defines that has no checks left, which is the condition on dropping the frame. The other three are the three reasons the rule refuses, and they are separate numbers because they have separate fixes. A callee that still checks something is a check elimination problem, a callee in another translation unit is an LTO problem, and a call through a pointer is neither. This is a static count of where the rule would fire and not a count of frames that were dropped, because nothing publishes a frame yet.

The purpose is that "why is there no bounds check on line 412" has an answer, and that a reviewer auditing a security-critical file can read the summary rather than the disassembly. It is also the input to document 13's cost model and document 12's scoreboard, so it is not an optional debugging feature; it is the artifact the rest of the specification consumes.

The output is JSON with a stable schema, in the parent's tier-2 stability class.

## 7.9 What we are not doing

**Sound whole-program abstract interpretation.** Frama-C's Eva and Verasco are the right technology and building one is a decade. The narrow-verified-rule approach is chosen because it is affordable and because its failure mode (a check that survives when it could have been discharged) is a performance bug rather than a safety bug.

**Machine-learned elimination policies.** The parent's document 01 rules MLGO out of scope for the optimizer on the grounds that the gains are 1% and the plumbing is a year, and the same reasoning applies here with the additional consideration that a learned policy is not verifiable.

**Speculative elimination with deoptimization.** JIT-style "assume it is in bounds, trap and recompile if not" is available in a managed runtime and not in an AOT compiler producing an object file.
