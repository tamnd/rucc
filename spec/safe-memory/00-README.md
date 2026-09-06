# Spec 2131/safe-memory: catching every memory bug rucc can see

A sub-specification of [spec 2131](../00-README.md). The parent specifies an optimizing C compiler in Rust with its own frontend, ægraph middle end, backend, assembler, object writer and debug info, targeting x86-64, AArch64 and RISC-V 64, with a compile ladder that ends at a booting Linux kernel. This sub-specification is about the thing the parent explicitly declared out of scope in its "What this is not" section, and which is now the most valuable thing the parent's architecture makes possible.

Written 6 September 2026. Everything factual was checked during the week of 6 September 2026 and the links are inline.

## The claim, in one paragraph

`rucc` already commits, in [document 07](../07-types-and-semantics.md), to implementing **PNVI-ae-udi** from [WG14 N3005](https://www.open-std.org/jtc1/sc22/wg14/www/docs/n3005.pdf) as the reference semantics for what its alias analysis is permitted to assume: every storage instance carries an ID unique for the whole execution, addresses are reused but IDs never are, and a pointer's provenance is the ID of the instance it points into or one past. That is not merely an aliasing model. **It is a complete specification of what a memory-safety monitor has to check.** Out-of-bounds is an address outside the bounds of its provenance. Use-after-free is provenance whose lifetime has ended. Type confusion is an access whose type is incompatible with the effective type recorded against its provenance. An uninitialized read is a byte inside its provenance that has never been stored. An invalid free is provenance that is not an allocation base. A pointer race is two accesses to the same provenance-carrying word with no happens-before edge. Six of the seven bug classes that dominate the CVE record are the *same* violation of the *same* model, queried six different ways.

A compiler that has written down what it assumes can check what it assumed. That is the entire thesis, and it is available to us and not to a compiler that never wrote the model down.

## What we are trying to beat

[Fil-C](https://fil-c.org/), by Filip Pizlo, is the state of the art and the thing to measure against. It is a fork of Clang 20.1.8 with a runtime that gives genuinely complete memory safety for C17 and C++20 on stock hardware, using invisible capabilities plus a concurrent garbage collector. It builds a whole memory-safe Linux userland, [Pizlix](https://fil-c.org/pizlix), Linux From Scratch 12.2 with a memory-safe OpenSSH, CPython, OpenSSL, SQLite and a working GUI. [LWN's October 2025 assessment](https://lwn.net/Articles/1042938/) puts the average slowdown at about 4x; the author's own figures are roughly 1.5x in good cases and 4x in bad ones, with a stated goal of 1.5x worst case. Document 01 covers it in detail. It is excellent work and this document set does not pretend otherwise.

It also cannot ever compile a kernel, and the reasons are structural rather than incidental. It requires a garbage collector, so it requires safepoints, root scanning and ownership of allocation, none of which exist in a kernel. It requires a hosted environment: its own libc, a dynamic linker, `mmap`. Its objects [do not link against objects from other compilers](https://lwn.net/Articles/1042938/), and a kernel links against hand-written assembly, firmware blobs and its own linker scripts. It [disallows inline assembly](https://fil-c.org/invisicaps_by_example) except the compiler-fence idiom, and Linux contains thousands of inline assembly statements it cannot remove. And it checks everything, always, at run time, because it is a Clang fork whose instrumentation pass fights an optimizer that knows nothing about capabilities.

Being more ambitious than Fil-C does not mean being safer than Fil-C in userspace. It means reaching the place Fil-C's design cannot reach.

## The four decisions that make that possible

**Separate detection from enforcement, and be explicit about which one you are selling.** Fil-C sells one product: a program that cannot violate memory safety, at 1.5x to 4x. We specify three tiers. **Tier D** is a sound monitor: every executed memory operation that violates the model is reported at the operation that violates it, no exceptions and no sampling, and the overhead budget is whatever it takes. Tier D is what runs over the corpus, under the fuzzer, and under syzkaller. **Tier E** is a production-deployable enforcement subset with a hard 1.3x budget. **Tier K** is the kernel profile, which is Tier D minus the things a kernel makes impossible and plus the things only a kernel needs. Document 02 makes each tier falsifiable.

**One metadata plane, many queries.** ASan, MSan, TSan, TySan and UBSan each carry their own shadow memory, each pay their own instrumentation, and several of them [cannot be enabled together at all](https://clang.llvm.org/docs/AddressSanitizer.html). That is not a limitation of the idea; it is a consequence of five teams independently retrofitting five shadow memories onto a compiler that had no memory model. We have a memory model. There is one plane, keyed by storage instance, recording bounds, lifetime state, effective type, initialization and last-writer epoch, and every check is a query against it. Document 05.

**Checks are IR, inserted early, eliminated by verified rewrite rules.** This is the structural advantage and it is the one that follows from the parent specification rather than being bolted onto it. AddressSanitizer instruments late precisely so that the optimizer cannot delete its checks, and consequently its checks are never optimized. We insert checks as first-class IR before the middle end, let the ægraph discharge the ones that are provably redundant, and **verify every check-eliminating rule with an SMT solver**, using exactly the apparatus [document 09](../09-optimizer.md) and [document 15](../15-testing.md) already build for the ordinary rewrite rules. An unsound optimization normally produces a wrong answer; an unsound *check elimination* produces a silent loss of safety, which is worse, and the defence is the one we were already paying for. Documents 06 and 07.

**The safety claim is stated modulo an enumerated, counted trust set.** Every region the monitor cannot see (hand-written assembly, DMA buffers while the device owns them, uninstrumented objects, `ioremap`ed MMIO) is declared, counted, and printed in the build's safety summary. "Memory safe" is not a boolean we assert. It is a number that has to go down, and it is visible in CI. Document 10.

## The goal, stated so it can be falsified

Four axes, matching the parent's document 02 in form and measured by the methodology in document 13.

**Soundness.** Zero false negatives at Tier D on a classified suite: the [Juliet](https://samate.nist.gov/SARD/test-suites) test cases for the memory-safety CWEs, plus a reproduction corpus of real historical CVEs drawn from the libraries in document 12. Every case is either detected at the violating operation or is written down as a known gap with a reason. This is the axis the word "all" in the project's goal cashes out to, and document 02 spends most of its length on what "all" can honestly mean.

**Precision.** Zero false positives at the Tier D default over the corpus's own test suites. Every report is a real violation of the written model. A tool that cries wolf on `container_of` gets turned off in a week and then catches nothing, which is why this axis outranks coverage in practice.

**Cost.** Tier D within 4x geomean on the corpus, which is the interesting number because it is one mode that catches what ASan, MSan, TySan and TSan catch between them and which cannot be run together. Tier E within 1.3x geomean and 1.6x worst case on the parent's scalar benchmark set, against Fil-C's 1.5x/4x. Tier K within 3x, against generic KASAN's [roughly 3x](https://docs.kernel.org/dev-tools/kasan.html) for a much narrower bug set.

**Reach.** The corpus in document 12 builds at Tier D and passes its own test suites. The Linux kernel boots at Tier K and survives a syzkaller run. The falsifiable form of the kernel claim is not "it boots" but "syzbot found bugs with a rucc-built kernel that a KASAN-built kernel did not find."

## Settled decisions

**Lock-and-key, not garbage collection, is the temporal-safety mechanism.** A GC is the better answer in a hosted process and Fil-C is right to use one. It is unavailable in a freestanding kernel and the ultimate goal is a kernel, so we pay the version-compare cost everywhere rather than maintaining two temporal models. Document 08 argues this at length and records the cost honestly.

**Narrow pointers with side metadata, not fat pointers, at every ABI boundary.** Fat pointers change struct layout, break every `write(2)` of a structure to disk, and make interop with uninstrumented code impossible. Fil-C reached the same conclusion after starting with 256-bit pointers. Document 05.

**Sub-object bounds are a tier, not a default.** Intra-object overflow is the largest class that Fil-C, CHERI-by-default and ARM MTE all miss, and we can catch it because the type plane is byte-granular. It also breaks `container_of`, flexible array members and every union-based type pun in existence, so it ships off by default behind `-fsafety-subobject` with a declared exemption mechanism. Document 09.

**We consume the kernel's existing sanitizer annotations rather than inventing our own.** Linux has been annotated for KASAN, KMSAN and KCSAN for a decade: `kasan_kmalloc`, `kasan_slab_free`, `__no_sanitize`, `SLAB_TYPESAFE_BY_RCU`, the DMA API's ownership transfer points. That is thousands of hours of upstream work describing exactly where a monitor's hooks belong. Tier K1 implements the KASAN ABI so the existing annotations work unmodified, and Tier K2 extends from there. Document 11.

**Data race detection is scoped to safety-relevant accesses.** Full ThreadSanitizer is [5x to 15x](https://clang.llvm.org/docs/ThreadSanitizer.html) and is a different product. Races on the metadata plane itself (two threads storing pointers to the same word without ordering) are the races that turn into use-after-free, and the plane already has a last-writer epoch for other reasons, so detecting them is nearly free. Document 09.

## The documents

| | | |
|---|---|---|
| 00 | this file | the thesis, the settled decisions, what to read first |
| 01 | `01-research-2026.md` | the landscape as of September 2026, with numbers and what each one forces |
| 02 | `02-the-goal.md` | what "catch all memory bugs" can honestly mean; the tiers; the four axes |
| 03 | `03-bug-model.md` | the closed enumeration of bug classes and the coverage matrix |
| 04 | `04-safety-model.md` | the formal model, the monitor semantics, and the soundness statement |
| 05 | `05-representation.md` | the metadata plane, its layout, its ABI, and why not fat pointers |
| 06 | `06-instrumentation.md` | checks as IR, where they are inserted, and how they are lowered |
| 07 | `07-check-elimination.md` | discharging checks statically, and verifying that we may |
| 08 | `08-temporal-safety.md` | use-after-free: quarantine, epochs, versions, and why not a GC |
| 09 | `09-type-init-and-races.md` | effective types, uninitialized reads, sub-object bounds, pointer races |
| 10 | `10-boundaries.md` | libc, syscalls, inline asm, uninstrumented code, and the counted trust set |
| 11 | `11-kernel.md` | the kernel profile: what breaks, the K-tiers, DMA, MMIO, RCU, aliased maps |
| 12 | `12-corpus-and-evidence.md` | "all popular libraries": the corpus, the scoreboard, the triage process |
| 13 | `13-performance.md` | the overhead budget per tier, and the rules for reporting it |
| 14 | `14-verification.md` | proving the monitor sound: SMT, differential, Juliet, the CVE escape suite |
| 15 | `15-integration.md` | crates, flags, IR changes, pass placement, the rucc integration spec |
| 16 | `16-milestones.md` | S0 to S7, mapped onto the parent's M0 to M11, with exit criteria |
| 17 | `17-open-questions.md` | the ranked list, what would settle each, and by when |
| 18 | `18-sqlite-idioms.md` | document 03 section 3.5 checked against a real program, row by row |

Read 02 first, then 03, then 15. Document 02 decides whether the goal is honest, document 03 decides whether "all" is a defensible word, and document 15 is what actually gets built.

## What this is not

**Not a static analyzer.** The parent's document 00 says `rucc` implements `-fanalyzer`-class checks nowhere, and that stands. Everything here is either a dynamic check or a *discharge* of a dynamic check by a proof that the compiler can state and an SMT solver can confirm. We never report a bug we did not observe.

**Not a safe dialect of C.** There is no `_Checked` region, no ownership annotation you must write, no borrow checker. [Checked C](https://github.com/checkedc/checkedc) and the SEI's [Pointer Ownership Model](https://www.sei.cmu.edu/library/a-pointer-ownership-model-for-c-inspired-by-rust/) are good work and document 01 covers them, but their adoption cost is the annotation burden and this project's premise is that unmodified upstream source must build. Optional annotations that *reduce overhead* by letting the compiler discharge checks are in scope; annotations required for correctness are not.

**Not a replacement for Rust.** Rust blocks [about 84% of kernel vulnerabilities](https://mars-research.github.io/doc/2025-cheri-acsac25.pdf) in the ACSAC 2025 classification against CHERI's 35-61%, and it blocks them at compile time for zero run-time cost. Nothing in this document set changes that. This is for the code that is not going to be rewritten, which is most of the code.

**Not a hardware proposal.** CHERI is a better mechanism than anything implementable in software and document 01 says so. We target stock x86-64, AArch64 and RISC-V 64 because that is what the corpus runs on, and we use ARM MTE and pointer authentication as *accelerators* where present rather than as requirements.

## Honesty about scope

Twelve to twenty engineer-months on top of the parent specification's forty to seventy, and it cannot start before the parent's M4, because it needs an IR, a verified rule DSL and a working optimizer to exist first. Document 16 sequences it and puts the first tier at M5, alongside SQLite.

The parts most likely to kill it, in order:

**False positives on real code.** Every previous project in this space has died here. `container_of`, type punning through unions, `memcpy` between incompatible types, pointer-to-integer round trips through hash tables, the `MAP_FIXED` re-mapping tricks in allocators, and the twelve different ways real code computes an address that is briefly out of bounds and then comes back. Document 03 enumerates the ones we know about and document 12's corpus exists to find the ones we do not. The mitigation is that a false positive at Tier D is a **release-blocking bug in us**, not a bug in the corpus, and is treated that way.

**The overhead of the metadata plane is memory traffic, not instructions.** Every pointer stored to memory needs its capability stored somewhere, which is an extra cache line touched on a data structure that was tuned to fit in one. Fil-C's own accounting says structures containing pointers use double the memory. Instruction-count reasoning will mislead us badly here and document 13 measures the right thing.

**The kernel's aliased mappings may not fit the model.** A physical page in Linux is reachable through the linear map, through `vmalloc`, through a user mapping and through `kmap`, and PNVI-ae-udi has one storage instance per object with one set of addresses. Document 11 proposes frame-keyed provenance for the linear map and document 17 makes it the first open question, because if it has no clean answer then Tier K2 is bounded at "most of the kernel" rather than "the kernel."

The riskiest technical assumption is that check elimination in an ægraph works. Checks trap, trapping instructions cannot be freely reordered, and the parent's document 09 already notes that the ægraph pins control flow in a CFG skeleton. If checks have to live entirely outside the e-graph then they are optimized by a conventional pass and Tier E's 1.3x budget is probably not reachable. That is open question two, the experiment is scheduled in S2, and document 07 is written so that both answers leave most of it standing.
