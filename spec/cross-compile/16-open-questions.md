# Open questions

Six things this specification does not decide. Each has a measurement attached, a point in the plan where the measurement is available, and a stated default for what happens if nobody gets round to answering it. A question without those three is a worry, not an open question, and worries do not belong in a specification.

Parent document 19 does the same thing for the compiler; this is the cross-compilation continuation of it.

## Q1: Does the generality cost more than 2%?

**The question.** Document 02 claim 2 says the target model, the ABI descriptions and the data-driven predefined macros must not cost more than 2% of compile throughput or code quality on x86-64 Linux. Every mechanism in documents 03, 06 and 12 is a plausible way to lose a percent: a `const fn` becoming a field load, a monomorphic classifier becoming a table walk, a baked-in macro table becoming a generated one.

**Why it is not decidable now.** rucc is at M4/M5 and the model has not been built. Estimating this is exactly the kind of prediction that compiler projects get wrong in both directions.

**The measurement.** Parent document 16's benchmark, median of ten with IQR published, on the commit before and after document 03's migration stage 3, then again after document 06.7's classifier lands, then again at each of document 15.5's three decision points.

**The default if unanswered.** The work stops. Document 02.7 is written as an abandonment path precisely so that an unmeasured claim 2 blocks rather than passes by silence.

**This is the question that outranks the other five.** If Q1 fails, Q2 through Q6 are moot.

## Q2: How much of the rule-specification burden can be generated?

**The question.** Instruction selection is a rule set that `rucc-verify` discharges with an SMT solver, and writing the per-target specifications by hand is linear in targets. Document 05.10 proposes adopting Arrival's technique, generating ISLE-style specifications from vendor ASL and Sail semantics, for AArch64, RISC-V and x86-64, and labelling the other five targets as differentially validated rather than solver-verified.

**What is genuinely unknown.** Whether the generation technique transfers to rucc's rule language at all, and how much of a real rule set it covers versus how much still needs hand specification for the composite rules that are the interesting ones.

**The measurement.** At target four (M10), publish: engineer-days spent on rule specifications for each of targets 2, 3 and 4; the fraction of each target's rule set discharged by the solver; and the fraction whose specification was generated rather than written. Three numbers, per target, on a trend.

**The default if unanswered.** Targets beyond the third are labelled "differentially validated" in `--print-config` and in the support table, and the verification claim is narrowed rather than quietly weakened. Document 05.10's insistence is that the labelling is not optional, either the specs exist or the target says it lacks them.

## Q3: Who maintains the merged glibc header tree?

**The question.** Document 08.3's multi-version header tree is the single largest ongoing maintenance liability in this specification. It scales with glibc's release cadence, not with our target count, and it is the piece that has no upstream owner: glibc ships per-version trees, and the merge is ours.

**Why it is open.** It is an organizational question wearing a technical one. The technique is understood and prior art exists (`ziglang/universal-headers`); what is unknown is whether a project this size can absorb a recurring obligation to re-derive and re-validate an artifact when an external project releases.

**The measurement.** After two glibc releases have passed under this scheme: the wall-clock lag between the glibc release and our tree supporting it, and the engineer-hours spent per release. If the lag exceeds one release cycle or the cost exceeds a few days, the scheme is not sustainable as constructed.

**The default if unanswered.** Ship fewer glibc versions. Supporting 2.28 through 2.39 with a fixed floor and a fixed ceiling, refreshed annually, is a narrower but honest claim, and it degrades the reach claim without making it false.

## Q4: Do we ever link ELF ourselves?

**The question.** Document 11.2 bundles or fetches `lld` and is uncomfortable about it, because the size argument against LLVM applies to us the moment we ship LLVM-derived code. Parent document 19 already has an open question about an internal linker; this is its cross-compilation form, and the cross form is harder because an internal ELF linker for eight architectures needs relaxation, TLS relaxation, `--gc-sections` and LTO integration.

**What would change the answer.** Three things, any of which: `mold` or `wild` becoming a complete cross-architecture ELF linker that we can bundle at a smaller size; the linker turning out to dominate wall time in the incremental case rucc exists for, which parent document 11.6's measurement at M11 will show; or the fetch-a-linker mechanism proving to be the most common source of first-contact failures.

**The measurement.** Parent document 11.6's linker measurement at M11, plus the fraction of a `-O0` incremental rebuild spent in the link, plus document 13's size budget with and without a bundled linker.

**The default if unanswered.** Shell out, forever. It is the correct default and doing it badly is worse than not doing it.

## Q5: How fast does Darwin move?

**The question.** Document 07.3 argues Mach-O is the one format where "we wrote a correct object file" has a shelf life: chained fixups replaced relocation-and-rebase, `__init_offsets` replaced `__mod_init_func`, and both changes were driven by the linker and dyld shipping with an OS release rather than by a standards process.

**What is unknown** is the *rate*. Two structural changes in five years is tolerable. Two per year is a target that consumes an engineer.

**The measurement.** A tracked count, per rucc release: how many Darwin-specific fixes were required by a new Xcode or macOS release that were not required by anything else. Alongside it, the lag between an Xcode release and our tier-1 Darwin suite passing against it.

**The default if unanswered.** Darwin is tier 2 rather than tier 1, and its support statement names the SDK versions it has actually been tested against rather than claiming a range.

## Q6: What is the vector ABI, and when does it change?

**The question.** Document 03.7 takes the conservative option on vector arguments and document 05.1 records that the x86-64 psABI's treatment of 512-bit vectors is unresolved while APX and AVX10.2 land. AArch64's SVE brings sizeless types that C cannot express a `sizeof` for, and RISC-V's V extension has the same shape.

**Why deferring is right rather than lazy.** The conservative choice, vectors beyond a fixed width passed indirectly, no ABI-visible dependence on a feature flag, is compatible with every resolution of the psABI argument, and changing to a register convention later is a widening that old objects tolerate. Choosing early, and choosing wrong, is not.

**The measurement.** The psABI committees' resolutions, and the point at which GCC and Clang agree on a treatment for the same target. Document 06.9's policy then applies mechanically: when there is an incumbent and it is consistent, the incumbent is the ABI.

**The default if unanswered.** The conservative treatment stays, and the intrinsic headers work at full width because the vectors do not cross function boundaries in registers. That costs performance in vectorized code that passes vectors across non-inlined calls, which is a narrow case, and it never costs correctness.

## Decisions that are made, with the trigger that would reopen them

Not open questions, but recorded so that revisiting them is a decision rather than a drift:

| decision | where | what would reopen it |
|---|---|---|
| we ship no unwinder | 10.4 | a target with no system unwinder and a real `_Unwind_*` need |
| ARM64EC is out of scope | 06.6 | a linker we already bundle gaining full support |
| wasm's effort is published separately from claim 3 | 05.9, 07.5 | nothing; it is a different backend shape and that will not change |
| DWARF on Windows, no PDB | 07.4 | a user population that needs Visual Studio debugging, which is not our population |
| sanitizer runtimes are tier 1 only | 10.7 | the runtime cost per target falling, which it will not |
| no third-party sysroot content | 02.5, 08.7 | nothing. This is the boundary that keeps us from becoming a distribution |

## The shape of this list

Five of the six questions are answered by a number that does not exist yet and will exist at a specific milestone. That is the intended shape. A specification whose open questions are answered by discussion rather than by measurement has not finished thinking, and this one has three months of measurement scheduled into document 15's plan for exactly that reason.

Q1 is the exception in that its answer can end the work. It is first for that reason, and it is measured earliest, at document 03's migration stage 3, before any of the expensive work in documents 08, 09 and 13 begins.
