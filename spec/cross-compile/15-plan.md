# The plan: where this fits, and what issue #8 becomes

Parent document 17 has twelve milestones summing to 28 to 46 engineer-months of milestone work. This specification is not a thirteenth milestone. Most of it is work that the existing milestones already have to do, done in a way that scales, plus one genuinely new body of work, the sysroot and libc machinery, that has no home in the current list.

This document says which is which, so that the cost of adopting this specification is a delta rather than a new number.

## 15.1 The three categories

**(A) Already in the plan, done differently.** M6's AArch64, M9's RISC-V and Windows, M10's fourth target. These happen regardless. What this specification changes is the *shape*: the target model of document 03 instead of the three-field triple, the ABI description of document 06.7 instead of hand-written classification, the target table as data. The delta is small and front-loaded, and it is negative later, document 02 claim 3 is exactly the claim that doing it this way makes targets 4 through 12 cheaper than doing it the other way.

**(B) New work, genuinely.** Documents 08, 09 and 13: the header tree, the stub generator, the abilist blob, the cache, the provenance machinery, the distribution. Nothing in parent document 17 covers this, because the parent assumes a sysroot exists. This is where the added engineer-months are.

**(C) New work that pays for itself immediately.** Document 14's static layers and the differential ABI harness. These are testing infrastructure the parent needs anyway, parent 12.10 already asks for ABI validation, and building them before the fourth ABI rather than after is a scheduling change, not a cost increase.

## 15.2 The delta, per milestone

| milestone | what this specification adds | delta |
|---|---|---|
| **M0** | the target model of document 03, instead of the current `Triple`. Done at M0 or done as a workspace-wide edit later. | +0.25 |
| M1 to M5 | nothing. The x86-64 Linux path is unchanged, and document 02 claim 2 is the promise that it stays unchanged. | 0 |
| **M6** | AArch64 as planned, plus: the ABI description mechanism (document 06.7) proven on its second ABI, and **musl cross compilation end to end** (document 02.6 step 2), the whole sysroot pipeline with the libc problem at its easiest. | +0.75 |
| **M6.5 (new)** | the differential ABI harness (document 14.3) and the `_Static_assert` corpus (document 14.2), before the ABI count grows. | +0.75 |
| M7 | unchanged in content; §15.4 changes what the milestone is *called*. | 0 |
| M8 | nothing; debug info and LTO are per-target work already counted. | 0 |
| **M9** | RISC-V and Windows as planned, plus mingw-w64 as the default Windows environment (document 09.4) rather than assuming an installed toolchain. | +0.5 |
| **M9.5 (new)** | **glibc: the abilist blob, the stub generator, the merged header tree, start files, the version-node logic.** Document 09.2 and document 08.3. The single largest new item. | +3 to 4 |
| **M10** | the fourth target as planned, and it is now i686 with the model of document 03 behind it, so the published effort number is the number document 02 claim 3 is measured on. Plus targets five and six (armv7, ppc64le or s390x) at the marginal cost the claim predicts. | +1 |
| **M10.5 (new)** | distribution: the cache, provenance, size budget, the fetch mechanisms, the two licence walls. Document 13. | +1 |
| M11 | nothing. The kernel is a rung, not a target. | 0 |
| **continuous** | the header tree regeneration when glibc releases, the qemu divergence lists, the parity script. | ongoing |

**Total delta: roughly 7 to 8.5 engineer-months**, concentrated after M9. That is a real number and it is roughly a quarter of the parent's milestone total, for going from three targets to thirty. Whether it is worth paying is a decision the parent project makes at its second stopping point, which is exactly where it lands.

## 15.3 Why it lands after M9, and why that is right

Parent document 17 calls M9 "the second sane stopping point, and the strongest one": three hosts, three targets, two large databases, verified rules, continuous fuzzing. Everything in category (B) sits after that line, and that is deliberate rather than convenient.

Three reasons:

1. **Nothing in category (B) improves the compiler.** The stub generator does not make code faster or more correct; it makes an already-correct compiler usable without a distribution. Doing it before the compiler is good would be building packaging for something not yet worth packaging.
2. **The information is better later.** By M9 the target model has survived three architectures and two object formats, so document 02 claim 3 has actual evidence rather than a prediction, and the decision to spend four months on glibc machinery is made knowing whether targets are cheap.
3. **It is separable.** A user who has a cross toolchain gets value from rucc at M6 with `--sysroot`. The self-contained work makes it work for users who do not have one. That is a widening, not a prerequisite.

The one thing that must happen early is the target model (M0, +0.25). Everything else can wait; that cannot, because changing it later is a workspace-wide edit and every crate that reads a target will have been written against the wrong shape.

## 15.4 What issue #8 becomes

[tamnd/rucc#8](https://github.com/tamnd/rucc/issues/8) is "M7: Breadth", scoped to GNU-extension breadth and rung 2. That scope is correct and this specification does not change it.

What this specification argues is that **"breadth" in the milestone list names one of two axes and the other one was never made a deliverable.** M7's breadth is *how much C we accept*, attributes, builtins, statement expressions, intrinsic headers, the `features.toml` matrix. The second axis is *how many machines we emit for*, and parent document 17 addresses it only incidentally, as a side effect of M6, M9 and M10 each happening to add a target.

So the proposal for the issue tracker:

- **#8 stays exactly as it is.** Language breadth, M7, unchanged. Renaming it would lose the scoping work already in it.
- **A sibling issue, "M7b: target breadth"**, is opened as the tracking issue for this specification, linked from #8 as the other half of the same word. Its body is document 02's five claims and its checklist is document 04's target table.
- **Per-target issues** hang off it, one per row of document 04's table, each closed by document 02 claim 4's criterion, rungs 0 and 1 executing, with the hardware-or-emulation column filled in. Not by "the backend is written".
- **Four infrastructure issues** for the category-(B) work: the target model, the stub generator, the header tree, the distribution. These are the ones with dependencies and they are the ones worth scheduling explicitly.

The reason for a sibling rather than an edit: #8's exit criterion is checkable today and this specification's is not, and merging them would produce an issue that cannot be closed. Two issues that each close is better than one that does not.

## 15.5 The decision points

Three places where this specification can be stopped with the work up to that point still worth having:

**After M6 + M6.5.** The target model is in, musl cross compilation works, the ABI harness exists. rucc can cross compile for two architectures with a self-contained musl sysroot and its ABI is tested against GCC. This is already more than most compilers of its size offer, and it costs about 1.75 months over the parent plan.

**After M9.5.** glibc works. This is the point at which document 02 claim 1 becomes reachable, because glibc is what the Linux targets in zig's list actually need. It is also the point of maximum risk, per §15.6.

**After M10.5.** The whole specification. Claim 1 is measurable and the parity number is published per release.

At each point, document 02 claim 2's benchmark is run and the number published. A failure of claim 2 stops the work at whichever point it is discovered, per document 02.7, and the target model is rolled back.

## 15.6 The risks, ranked

1. **Claim 2 fails.** The generality costs more than 2%. Mitigated by measuring at document 03's migration stage 3, before the expensive work; the abandonment path is written down in document 02.7 and it is cheap because everything after M9 is separable.
2. **The glibc header tree is a maintenance sink.** Document 08.3 says so plainly, and it is the item with the worst effort-uncertainty in §15.2's table. The mitigation is that the tooling to derive it exists as prior art (`ziglang/universal-headers`), and the fallback is shipping fewer glibc versions, a narrower claim rather than a failed one.
3. **Darwin breaks under us.** Document 07.3: the format moves and the linker ships with an OS. Mitigated by testing against the current Xcode per release, and by document 16 tracking how often it actually happens.
4. **Testing thirty targets exceeds the CI budget.** Mitigated by document 14.8's explicit failure mode: reduce target count, never per-target depth.
5. **Effort concentration.** §15.2's largest item is one four-month block with no intermediate deliverable. Mitigated by musl at M6 proving the entire pipeline first, so that M9.5 is the glibc-specific parts only rather than the whole thing at once.

## 15.7 What success looks like, stated once

At the end of this specification, `rucc --target=aarch64-linux-musl main.c -o main` on a Windows laptop with no toolchain installed, no sysroot flag and no second download produces a static binary that runs on a Raspberry Pi, and `rucc --print-targets` lists thirty rows of which every non-tier-3 one has executed SQLite's test suite.

That is one sentence, it is falsifiable, and no other optimizing compiler under 25 MB does it.
