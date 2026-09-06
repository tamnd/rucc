# The goal: what "catch all memory bugs" can honestly mean

This is the document that decides whether the project is honest, and it is the one to read first. The parent's document 02 does the same job for the compiler.

The stated goal is to catch **all** memory bugs in **all** popular libraries, with the Linux kernel as the end of the ladder, and to be more ambitious than Fil-C. Two of those three words are doing more work than they can bear, and this document replaces them with claims that can be falsified.

## 2.1 Three limits, stated before the claims

**The coverage limit.** A dynamic monitor observes executed operations. It cannot report a bug on a path no input reaches. No amount of engineering changes this, and any project that says "we catch all bugs" without saying this is either confused or selling something. Therefore the soundness claim is about *executed operations*, and coverage is a separate problem solved by a separate apparatus, the fuzzing loop, the corpus's own test suites, and syzkaller, in document 12.

**The model limit.** The monitor catches violations of the model in document 04, not violations of the programmer's intent. Reading `s->b` when you meant `s->a`, where both are `int` and both are initialized and both are in bounds, is a bug in the program and is not a memory bug, and nothing here will find it. Similarly: integer overflow that computes a wrong size but stays in bounds after the allocation succeeds, a `memcpy` of the right length from the wrong buffer, a lock held for the wrong duration around correct accesses. The model is closed and document 03 enumerates it.

**The boundary limit.** Memory the monitor does not mediate is memory the monitor cannot check. Hand-written assembly, DMA buffers while a device owns them, uninstrumented shared objects, `ioremap`ed device registers, the page table walker. Every such region is a hole. Document 10's contribution is that the holes are *declared, counted and reported*, so the claim is "safe modulo 41 declared exemptions in this build" and 41 is a number in CI.

With those stated, here is what is actually claimed.

## 2.2 The three tiers

The single largest mistake available here is to build one mode. Fil-C builds one mode because Fil-C is selling a memory-safe userland, and that is a coherent product. Our end goal is a kernel, and no kernel will run a 4x monitor in production, so a design with one mode either cannot catch enough or cannot ship. Three tiers, with different budgets and different bug sets.

**Tier D, Detect.** The sound monitor. Every executed memory operation that violates the model in document 04 is reported at the operation that violates it. No sampling, no probabilistic tags, no quarantine that can be exhausted, no bug class silently omitted. Where a check cannot be performed, the operation is refused or the region is declared as an exemption and counted; it is never silently passed. Overhead budget: 4x geomean, 8x worst case, 2x memory. Tier D is what runs over the corpus, under Csmith and YARPGen, under the fuzzers in document 12, and under syzkaller. **This is the tier the word "all" refers to.**

**Tier E, Enforce.** A production-deployable subset that stops exploitation rather than finding bugs. Spatial safety complete, temporal safety complete, type and initialization planes off, sub-object bounds off, race detection off. Budget: 1.3x geomean and 1.6x worst case on the parent's document 16 scalar benchmark set, 1.4x memory. Tier E is the direct comparison against Fil-C's 1.5x/4x, and the reason we think a better number is available is in document 07: we insert checks as IR before the optimizer and discharge them with verified rewrite rules, where Fil-C instruments an optimizer that does not know what a capability is.

**Tier K, Kernel.** Tier D minus what a kernel makes impossible, plus what only a kernel needs. It is not a weaker Tier D; it is a different set. It loses nothing in the bounds and lifetime planes, gains DMA ownership checking, MMIO region typing, RCU-scoped lifetimes and `__init` lifetime tracking, and loses the ability to instrument roughly a hundred declared regions of assembly and early boot. Budget: 3x, against generic KASAN's ~3x for a much narrower bug set. Document 11.

A fourth, **Tier V, Verified**, is not a tier the user selects. It is the set of checks that were discharged statically and therefore cost nothing, and it exists as a named thing because document 07 requires every discharge to be attributable: `--emit=safety-summary` prints, per memory operation, which checks were required, which were discharged, and by which verified rule. No other tool in this space can answer "why is there no bounds check here", and being able to answer it is what makes a safety claim auditable rather than asserted.

## 2.3 The four axes

Each has a number and a measurement procedure in document 13 or document 14.

### Axis 1: soundness

**Zero false negatives at Tier D on the classified suite.** The suite has three parts:

1. The **Juliet** C test cases for the memory-safety CWEs: 121, 122, 124, 126, 127 (spatial), 401, 415, 416, 590, 761 (temporal), 457, 908 (uninitialized), 843 (type confusion). The SEI's POM work evaluated against all 4,604 cases for the five temporal CWEs and PoisonCap against 2,776 cases across three classes; those are the comparison points.
2. A **CVE reproduction corpus**: real historical memory-safety CVEs in the libraries of document 12, each reduced to a program plus an input that triggers it, each with a recorded pre-fix and post-fix build. Target at S6: 200 cases. This is the part that matters, because Juliet is synthetic and its false-negative profile is not the profile of real code.
3. The **ACSAC 2025 kernel CVE dataset**, 439 labelled Linux and FreeBSD vulnerabilities with the CHERI and Rust mitigation outcome already assigned. We evaluate Tier K against it and report the same number in the same form. CHERI blocks 35-61%; Rust blocks 84%. Our number goes in that table and is published whether or not it is flattering.

The claim is not "100%." It is **"every case in the suite is either detected at the violating operation, or is a written-down gap with a stated reason and a document-17 entry."** A specification that promises 100% has no way to be wrong and therefore says nothing.

### Axis 2: precision

**Zero false positives at the Tier D default over the corpus's own test suites.** A report that is not a violation of the written model is a bug in `rucc` at release-blocking severity, in exactly the way a miscompilation is in the parent's document 02.

This axis outranks axis 1 operationally, because a tool with a 1% false-positive rate over a million-line codebase produces ten thousand reports, gets suppressed wholesale, and thereafter catches nothing. Every serious project in this space has died here. The specific hazards are in document 03 section 3.5 and they are all real code patterns, not pathologies: `container_of`, type punning through unions and through `char*`, pointer-to-integer round trips through hash tables and tagged pointers, allocators that `MAP_FIXED` over their own mappings, the one-past-the-end idiom, `offsetof`-based address arithmetic, and the `struct sockaddr` family's deliberate type confusion.

The measurable form: the false-positive count over the corpus is a number in CI, it starts high, and no milestone in document 16 exits with it above zero.

### Axis 3: cost

Three numbers, reported separately and never averaged into one.

| Tier | Geomean | Worst case | Memory | Compared against |
|---|---|---|---|---|
| D | 4x | 8x | 2x | ASan+MSan+TySan+TSan, which cannot be combined |
| E | 1.3x | 1.6x | 1.4x | Fil-C at ~1.5x/4x |
| K | 3x | 6x | 1/8 of RAM | KASAN generic at ~3x for a narrower set |

Reported per benchmark, never geomean alone. Cpp2Rust's result (2% on one program and 6x on another in the same 13k-line evaluation) is the reason: in this domain the geomean hides the case that decides whether anyone can use the tool. Document 13 sets the reporting rules.

### Axis 4: reach

**The corpus builds at Tier D and passes its own test suite.** Document 12 defines the corpus and the scoreboard. The honest form of the claim is the parent's: not "compiles C safely" but "these named projects, at this tier, with this many declared exemptions, finding this many upstream-confirmed bugs."

**The kernel boots at Tier K and survives syzkaller.** The falsifiable form is not the boot. It is: **syzbot, running against a rucc-built Tier K2 kernel, reports bugs that a KASAN-built kernel does not report.** One such bug, confirmed and fixed upstream, is worth more than any benchmark in this document, and document 16 makes it the exit criterion for S7.

## 2.4 Against whom, and where we lose

Stating the losses explicitly, because a comparison table that only shows wins is marketing.

**Against Rust.** We lose. Rust blocks 84% of the ACSAC kernel vulnerability set at compile time for zero run-time cost, and it eliminates the classes rather than detecting them. Nothing here is a reason to write new C. This is for the code that already exists.

**Against CHERI.** We lose on cost and we win on availability and on completeness. Arm's estimate for production Morello is ~2% against our 1.3x target; that is not a close comparison. We win on stock hardware, on stack temporal safety, which Cornucopia still does not have, and on the type and initialization planes, which CHERI does not have and which PoisonCap is currently adding at the hardware level. Where CHERI hardware exists, document 05 keeps the plane abstract so we can lower onto it rather than compete with it.

**Against Fil-C, in userspace.** Roughly a draw on bug coverage, with two differences in each direction. Fil-C catches everything we catch in the spatial and temporal planes and has a five-year head start on compatibility across a real distribution. We catch intra-object overflow, uninitialized reads, effective-type violations and safety-relevant races, which Fil-C does not, and we report torn pointer-plus-capability races that Fil-C considers safe. Fil-C has a working memory-safe Linux userland today and we have a specification.

**Against Fil-C, below libc.** We win, and this is the whole point. Fil-C requires a collector, a host, a libc, a dynamic linker, and near-total absence of inline assembly. Document 11 is a plan for a kernel and there is no version of Fil-C's design that has one.

**Against ASan.** We win on completeness, ASan's quarantine is finite, so use-after-free after quarantine eviction is a miss, and ASan cannot see intra-object overflow, uninitialized reads or type confusion, and we lose on maturity and on the fact that ASan works today with every compiler.

## 2.5 Why a compiler is the right place for this

Three reasons, and the third is the one that is specific to this compiler.

Instrumentation needs the type system. Bounds come from types, effective types come from the frontend's 6.5 analysis, initialization state comes from knowing where objects begin. A binary rewriter cannot recover any of it, which is why binary-level tools are probabilistic.

Instrumentation needs the optimizer, and needs it to be cooperative rather than adversarial. AddressSanitizer runs late specifically so the optimizer cannot remove its checks, which means its checks are never optimized; TySan disables TBAA for alias analysis while it is active, giving up the optimization to keep the accesses observable. Both are the same concession, made because the instrumentation and the optimizer belong to different worlds. Ours do not.

And this compiler in particular has already written the model down. The parent's document 07 commits to PNVI-ae-udi, document 08 carries `provenance` as an attribute on pointer values in the IR, document 09 has a rewrite-rule DSL whose rules are data, and documents 10 and 15 have SMT verification of those rules with the Crocus methodology. Every one of those was specified for a different reason (auditability of the alias analysis, verifiability of instruction selection) and every one of them is exactly what a safety monitor needs. This sub-specification is mostly a matter of pointing the existing machinery at a second problem.

## 2.6 What would make this project not worth doing

Three outcomes, each of which should stop it.

If Tier E cannot be brought under 2x on the corpus, then Fil-C is better on every axis that matters for userspace and the only remaining justification is the kernel, which is a much narrower project. Document 13's S4 measurement decides this.

If the false-positive count over the corpus does not converge to zero (specifically, if there is a category of legitimate C idiom the model cannot accommodate without a per-project exemption list) then the tool is a research artifact. Document 03 section 3.5 is the list of candidates and document 16's S3 exit criterion is that the list is closed.

If aliased kernel mappings turn out to have no clean provenance answer, Tier K2 tops out somewhere below "the kernel" and the honest claim shrinks to "kernel subsystems that do not manipulate physical addresses." That is still useful and it is not what this document promised. Document 17 question one.
