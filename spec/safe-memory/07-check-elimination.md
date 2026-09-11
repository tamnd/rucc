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

**The hoisting rule.** A check may be hoisted to a dominating block only if every path from the hoisted position to the original reaches the original (that is, only to a block that the original post-dominates) because otherwise the program traps on a path where it would not have. Loop-invariant checks hoisted out of a loop with a possibly-zero trip count are the standard error here and the rule above forbids it. The correct transformation for that case is the guard in section 7.4.

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

**What may never be eliminated:** `meta_begin` and `meta_end` for a storage instance whose address escapes, and `meta_transfer`. Ending a lifetime is the event that makes future checks correct; skipping it is not an optimization, it is a bug that manifests as a missed use-after-free.

## 7.7 The rules are data, and they are verified

Every transformation in sections 7.3 through 7.6 is expressed in the parent's document 09 rewrite DSL, in a `safety/` rule namespace under `rucc-codegen`'s rule tree alongside the middle-end and lowering rules, per the parent's document 18 packaging constraint. `rucc-verify` covers them.

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
