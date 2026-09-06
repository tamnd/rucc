# Milestones

Eight, S0 through S7, interleaved with the parent's M0-M11 rather than appended to them. Each has an exit criterion that is a *number* or a *demonstration*, never "the code is written," because a milestone whose exit criterion cannot be checked is a wish.

The ordering principle: **every measurement that can invalidate a number in document 02 happens before the work that depends on that number.** Document 13.5 lists five such measurements; each is placed here at the earliest milestone where it is possible.

## The dependency on the parent

| S | Needs from the parent | Earliest |
|---|---|---|
| S0 | the IR and its verifier exist | after M2 |
| S1 | a runtime links and programs run | after M3 |
| S2 | none | with S1 |
| S3 | SQLite builds and passes | after M5 |
| S4 | the optimizer and its rule DSL | after M4, real after M6 |
| S5 | LTO, sanitizer runtimes, debug info | after M8 |
| S6 | breadth; a large corpus builds | after M9 |
| S7 | the kernel builds and boots at all | after M11 |

Nothing here can start before the parent's M2 and the substantial work cannot start before M4, which is document 00's stated constraint and is the reason this is a sub-specification rather than a competing plan.

## S0: The IR extension (1 to 2 months, after M2)

The `cap` value type, the 19 instructions of document 06.2.2, the four facts, the plane metadata node kind. Verifier rules for every one. Printer and parser round-trip. `rucc-safety` exists as a crate at rank 9 and does nothing but insert checks on a hand-written test IR.

**Exit:** every safety instruction round-trips through the textual IR; the verifier rejects each of a written list of malformed forms; `cargo xtask layers` passes with the two new packages.

**Why first:** it is the cheapest thing that can be wrong in an expensive way. An IR design mistake discovered at S4 costs a rewrite of everything above it.

## S1: Bounds and lifetime, end to end, unoptimized (2 to 3 months, after M3)

The narrow path from source to a trapping program. Insertion for `load`, `store` and address derivation. `rucc-safe-rt` with the lifetime plane, the header/aux/payload allocator, and `__rucc_safety_fail` with `.rucc_safety_desc`. No elimination beyond frontend constant folding. No type plane, no init plane, no epoch plane, no boundary wrappers beyond `malloc`/`free`.

**Exit:** a hand-written suite of 100 programs (one per row of document 03's S and T tables where applicable, plus the false-positive idioms of document 03.5) each producing exactly the expected report at the expected line, or exactly no report. The overhead is expected to be terrible and is measured anyway, as the baseline everything else improves on.

## S2: The boundary (1 to 2 months, with S1)

Document 10, before anything large is attempted, because a corpus project that cannot link is not a data point. The interposition table and its wrapper generator, the memory-movement and string wrappers, the syscall wrappers, the allocator interposition API, the TLS call frame, boundary capability recovery, and `--emit=safety-summary` with the trust-set counts.

**Exit:** a program built at Tier D links against an *uninstrumented* shared library, runs, and reports a genuine bug in its own code; the summary counts the recovered capabilities and names the uninstrumented object. Document 10.7's incremental-adoption property demonstrated rather than asserted.

**Why this early:** document 10.7 says nothing else in the specification is worth much if the mixed link does not work. Finding out at S6 would be a catastrophe.

## S3: SQLite at Tier D, and the two cheap experiments (2 to 3 months, after M5)

The first real code. The parent's M5 is "SQLite builds and passes its test suite"; S3 is the same sentence with "at Tier D" and "with zero reports" appended.

Two measurements from document 13.5 land here because they are cheap and because two numbers in document 02 depend on them:

**Type-plane granule homogeneity** (document 17 question 6), answerable by walking DWARF struct layouts over the corpus without building any of the type plane. A week of work that decides whether Tier D's 2x memory budget survives.

**Aux plane locality**: adjacent-aux versus shadow-mapped-aux, compared on miss counts. Decides whether document 10.4's adopted third-party allocators are acceptable.

**Exit:** SQLite's full test suite passes at Tier D with zero reports; every idiom in document 03.5 that SQLite exercises is either handled or is a document 17 entry with a written reason; both measurements are recorded with their consequences for document 02 stated.

**This is stopping point 1 for the sub-specification.** If SQLite cannot reach zero reports, document 02.6's second failure condition has fired and the honest response is to stop and fix the model.

## S4: Check elimination, and the budget (3 to 5 months, after M6)

Where Tier E is won or lost. The dominator-tree redundancy walk, loop hoisting and splitting, plane-write coalescing, aux elision by escape analysis, and the interprocedural summaries, with `nofree` first, because document 08.8 says it is the difference between temporal checks costing 5% and 40%.

Every rule is data in the `safety/` namespace from the day it is written, not retrofitted.

Three more of document 13.5's measurements land here, all of which can invalidate document 02's Tier E row:

- **PICO+CHOP composition** (document 17 question 3): discharge rate with each source alone and together.
- **Call-frame elision rate** (document 17 question 4).
- **Register pressure**: spill/fill delta on the pointer-heavy benchmarks.

**Exit:** Tier E under 2x geomean on the parent's document 16 benchmark set plus document 12.4's pointer-heavy additions, reported per benchmark with worst case and discharge rate; the three measurements recorded; and the differential check accounting of document 14.3 running nightly with zero divergences.

**This is the sub-specification's most important gate.** Document 02.6 says a Tier E above 2x means Fil-C is better on every userspace axis and the project narrows to the kernel. That decision is made here, on measurements, not later on hope.

## S5: The full plane set, and the OSS-Fuzz replay (2 to 3 months, after M8)

The type plane with its granule compression, the byte-granular init plane with the padding rules of document 09.3, the epoch plane, `restrict` checking, and `-fsafety-subobject`. Rule verification in `rucc-verify` for the whole `safety/` namespace. Randomized elimination fuzzing per document 14.4.

And the activity document 12.7 calls the highest expected-value thing in the project relative to its cost: **replay every OSS-Fuzz corpus for every tier-1 project against a Tier D build.** The inputs exist, the harnesses exist, and the classes we add over ASan (post-quarantine use-after-free, intra-object overflow, uninitialized reads, type confusion) are exactly the ones those corpora have been exercising for years without a checker that could see them.

**Exit:** the Juliet per-row numbers of document 14.6 published for every row and every tier, with missed cases enumerated by test id; the OSS-Fuzz replay run over at least six tier-1 projects; and, the criterion that matters, **at least one upstream-confirmed bug** found by a class no existing tool covers.

## S6: The corpus, and the CVE suite (3 to 4 months, after M9)

Breadth. Every tier-1 project in document 12.2 building and passing at Tier D. The 200-case CVE reproduction corpus of document 12.3 assembled, both directions tested. The scoreboard running nightly and published. The triage process of document 12.6 in routine operation, with the five buckets counted.

musl instrumented in full, which removes the largest single boundary in any userspace build and is the milestone's hardest single item.

**Exit:** every tier-1 project at `reports_total = 0`; the CVE corpus at 200 cases with the pre-fix builds all reporting at the right class and line and the post-fix builds all silent; the trust-set counts published per project; and document 03.5's false-positive table closed, every entry either handled or a document 17 question with a stated reason.

**This is stopping point 2.** A project that reaches here has a real tool, whether or not it ever reaches a kernel.

## S7: The kernel (4 to 8 months, after M11, widest uncertainty)

Document 11, and the widest range in the plan for the same reason the parent's M11 is.

Ordered by risk rather than by dependency:

1. **Aliased mappings** (document 11.5, document 17 question 1), the only genuinely unsolved problem here. Implement the restricted claim first, *measure how often it bites*, then canonicalization for the paths that matter. The measurement is the deliverable; the mechanism follows from it.
2. Plane bring-up on the KASAN shadow machinery; exclusion lists consumed; the NMI-safe reporter.
3. Slab and page allocator interposition; `free_initmem` as `__rucc_alloc_purge`.
4. `SLAB_TYPESAFE_BY_RCU` version scoping (document 11.6), the check no other tool has.
5. DMA ownership and MMIO typing (document 11.4), likewise.
6. `copy_to_user` init checking, which is the highest-yield single check in the kernel.
7. K3, then K2, then K1, in that order, because each is a subset of the next and a K3 kernel that boots is evidence a K1 kernel eventually will.

**Exit, in three parts:**

- **A Tier K2 kernel boots and passes the kernel selftests.** The floor.
- **The ACSAC replay** of document 14.7: predicted and observed coverage over the 439-CVE set, both published, and the gap between them explained.
- **The falsifiable claim of document 11.8: syzkaller against a Tier K1 kernel finds memory-safety bugs that the same corpus against a KASAN + KMSAN + KCSAN kernel does not.** One such bug, confirmed and fixed upstream, is worth more than every benchmark in this specification.

**This is stopping point 3, and it is the one the whole document set was written for.**

## Summary

| S | What | Months | After |
|---|---|---|---|
| S0 | IR extension and verifier | 1-2 | M2 |
| S1 | Bounds and lifetime end to end | 2-3 | M3 |
| S2 | The boundary; the mixed link works | 1-2 | with S1 |
| S3 | **SQLite at Tier D, stopping point 1** | 2-3 | M5 |
| S4 | **Check elimination; the Tier E budget decided** | 3-5 | M6 |
| S5 | Full plane set; the OSS-Fuzz replay | 2-3 | M8 |
| S6 | **The corpus and the CVE suite, stopping point 2** | 3-4 | M9 |
| S7 | **The kernel, stopping point 3** | 4-8 | M11 |
| | | **18-30** | |

The total exceeds document 00's stated 12-20 engineer-months because these are calendar ranges for the work as scheduled and much of S2 overlaps S1, S5 overlaps S4's tail, and the corpus work of S6 is continuous rather than a phase. The two figures should be reconciled by measurement rather than by argument, and until they are, **18-30 is the number to plan against.**

## What is not on this list

**Whole-program type inference** (document 07.5, document 17 deferral 3). Post-1.0.

**CHERI lowering** (document 05.4). Post-1.0, and it becomes attractive the moment capability hardware is available to test on, because it is the configuration where this design is at its best.

**C++.** The parent's document 00 puts it out of scope and document 04.6 depends on that.

**A Fil-C-style memory-safe distribution.** A different product with a different set of problems, and Fil-C is five years ahead on it.
