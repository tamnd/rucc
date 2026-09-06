# The memory-safety landscape, checked September 2026

Everything here was checked against sources during the week of 6 September 2026 and the links are inline. Where a claim is a vendor estimate, a single blog post, or the author's own figure rather than an independent reproducible measurement, it says so. Where something could not be confirmed it is marked **[unverified]** and document 17 tracks it.

The purpose is the same as the parent's document 01: about twenty specific facts determine most of this design, and when one of them changes we need to know which decisions to revisit.

## 1. Fil-C, which is the state of the art

### 1.1 What it is

[Fil-C](https://fil-c.org/), by Filip Pizlo, is a "fanatically compatible" memory-safe implementation of C and C++. It is a fork of Clang 20.1.8 plus a runtime, Apache-2.0 with LLVM exceptions, supporting C17 and C++20 and almost all Clang 20 extensions including atomics and SIMD intrinsics. Version at the time of this check is **0.684**. It began in 2023; Pizlo has said he was not sure it was possible when he started.

Its coverage, from the [repository README](https://github.com/pizlonator/fil-c): out-of-bounds on the heap or the stack, use-after-free on the heap or the stack, type confusion between pointers and non-pointers, pointer races, `va_list` misuse, and validation of buffers passed to system calls. Every violation is a "Fil-C panic" that stops the process. There is **no escape hatch**: no `unsafe` block.

### 1.2 InvisiCaps, the pointer representation

This is the part worth reading in full, in [`invisicaps_by_example.md`](https://github.com/pizlonator/fil-c/blob/deluge/invisicaps_by_example.md), because it is the most careful published treatment of the software-capability design space.

Pointers are their native size. Each pointer carries an invisible capability holding lower bound, upper bound, and a status word covering permissions, function-pointer-ness, read-only, freed, and global. Because the capability is invisible, a pointer *stored in memory* cannot carry it inline; instead Fil-C creates an **aux allocation** alongside, storing capabilities for the pointer-shaped slots. Heap allocations carry two metadata words before the object. Structures containing pointers therefore use roughly double the memory.

Three consequences that matter to us:

**Integer-pointer laundering is scoped.** `(const char*)((uintptr_t)str ^ 1)` works when the cast pair is locally visible to the compiler, because the original capability can be recovered. Round-tripping through a global `uintptr_t` produces a null capability and panics on use. This is a pragmatic approximation of PNVI-ae-udi's exposed-address rule, done by compiler visibility rather than by a model.

**Bounds checks are written three ways to defeat pointer-arithmetic overflow.** `P <= upper - S` where the minimum allocation size exceeds `S`; `P < upper && P + S <= upper` otherwise; `P < upper` when the access is separately alignment-checked. All allocations round to 16 bytes with 16-byte alignment, which is what makes the first form safe.

**Non-atomic pointer stores can tear between value and capability.** A non-atomic pointer load decomposes into a monotonic access to the capability and a non-atomic access to the value, so a race can pair thread A's pointer value with thread B's capability. `_Atomic` pointers use 128-bit lock-free operations and do not tear. Fil-C's position is that the torn result is still memory-safe, because the access is still bounded by *some* real capability. It is safe, and it is also a wrong answer that no tool reports. Document 09 takes a different position.

**Variadics are heap-allocated and read-only**, so `va_arg` past the end is a bounds failure and extracting an integer as a pointer yields a null capability.

**Inline assembly is essentially banned.** Only the compiler-fence idiom `asm volatile("" : : : "memory")` is permitted.

### 1.3 FUGC, the collector

Fil's Unbelievable Garbage Collector is a parallel, concurrent, on-the-fly grey-stack Dijkstra collector with a soft-handshake fixpoint and a SIMD "turbosweep" over `verse_heap`. Objects are never moved, so raw pointers stay valid. Safepoints are at loop back edges, and signal handlers run only at safepoints, which is what makes `malloc` signal-safe. Tracing is precise, using the aux capability information, unlike Boehm-Demers-Weiser.

The collector is what makes `free()` sound. `free()` does not deallocate; it marks the capability free by setting `upper = lower`, so every subsequent access fails a bounds check. The object is kept alive in that state as long as any pointer to it is reachable. When the only references are from unreachable heap objects, FUGC repoints them at a global **free singleton** and reclaims the storage, so the panic survives reclamation. Use-after-free, double free and invalid free are therefore all deterministic panics rather than probabilistic ones. In stress tests the collector reportedly runs about 35% of the time.

This is an elegant design and it is the reason Fil-C's temporal safety is complete where CHERI's is not. It is also the reason Fil-C cannot go where we are going.

### 1.4 Numbers, and where they come from

[LWN, 28 October 2025](https://lwn.net/Articles/1042938/), Daroc Alden: programs run "about four times more slowly" on average, though the slowdown depends heavily on pointer-usage patterns; Bash 5.2.32 built with Fil-C was usable as a daily shell without noticeable impact. Pizlo's own figures in [repository discussions](https://github.com/pizlonator/fil-c/discussions/145) are roughly 1.5x in good cases and 4x in the worst, with a stated target of 1.5x worst case and 1.2x for many programs. Release notes track single-digit-percent gains against PizBench, his own suite: ~1% from moving the aux pointer component into the high 48 bits with flags in the low 16, ~1% from fixing double- and triple-zeroing of allocations, ~4% from better union codegen after fixing capability loss in `std::optional`-shaped unions. **There is no published per-benchmark table [unverified]**; do not quote a single headline multiplier as measured.

Notably, some optimizations were reverted because they broke constant-time cryptography idioms while showing no PizBench gain. That is a good instinct and document 13 adopts the same rule.

### 1.5 Reach, and the wall

[Pizlix](https://fil-c.org/pizlix) is Linux From Scratch 12.2 rebuilt with Fil-C: a memory-safe userland with a memory-safe `sshd`, a working GUI, `git`, `sudo`, `tmux`, on glibc 2.40. OpenSSL, CPython, SQLite, zlib, PCRE, ICU, musl, libc++ all build with zero or minimal changes. Platform support is Linux on x86-64 and ARM64 only; Darwin/ARM64 and FreeBSD were dropped to allow a more faithful libc. The ARM64 port is no longer beta and supports 4K, 16K and 64K pages, but its ABI is explicitly not stable; the x86-64 ABI is "plan of record" stable without enforcement.

The wall, from LWN and from the repository: objects compiled by Fil-C do not link against objects from other compilers. A non-Fil-C compiler is still required for Fil-C's own runtime, for glibc, and for the Linux kernel. There is no cross-language linking with Rust. There is no compile-time verification, and no protection against undefined behavior outside memory safety.

**What this means for us.** Fil-C settles four design questions and leaves one open. It settles that narrow pointers plus side metadata is the right representation, that non-moving collection lets `free` be sound, that "fanatically compatible" is achievable on a very large corpus, and that the honest overhead of complete software memory safety in a hosted process is single-digit multiples rather than the 20x folklore. What it leaves open (and what this specification is about) is everything below the libc boundary. Documents 08 and 11 are the two places we deliberately diverge, and both divergences are forced by the same fact: a kernel has no collector and no host.

## 2. Hardware capabilities: CHERI

[CHERI](https://www.cl.cam.ac.uk/research/security/ctsrd/) makes pointers unforgeable hardware capabilities carrying bounds and permissions with a tag bit. [Arm Morello](https://ieeexplore.ieee.org/document/10123148/) is the prototype SoC, CHERI on a Neoverse N1, which allows direct comparison against a non-CHERI baseline of the same microarchitecture.

**Porting cost.** Arm's [Hot Chips 2022 figures](https://hc34.hotchips.org/assets/program/conference/day1/Academia/HC2022.Arm.RichardGrisenthwaite.v1_0.pdf): roughly 6 MLoC adapted by one FTE in three months, 0.026% of lines changed, 73.8% assessed vulnerability mitigation. Ericsson's [Cloud-RAN evaluation](https://www.ericsson.com/en/blog/2024/9/memory-safety-in-telecommunications-with-cheri) needed changes to about 1% of benchmark source, mostly in dependencies. Microsoft's ISA security analysis concluded CHERI would have deterministically mitigated at least two thirds of 2019's critical memory-safety issues. Arm's estimate for a production-quality Morello is about 2% overhead in CHERI mode [vendor estimate].

**The kernel number, which is the one we care about.** [Li, Zhang, Tlatelpa-Agustin, Chen and Burtsev, ACSAC 2025](https://mars-research.github.io/doc/2025-cheri-acsac25.pdf), classified 439 Linux and FreeBSD kernel vulnerabilities. CHERI blocks **35% to 61%** depending on whether temporal safety is enabled, 61% with capability revocation, 35% without. Rust blocks 84% of the same set. Porting the FreeBSD kernel to pure-capability mode took seven months. CHERI mitigates some uninitialized-memory flaws, but only when they manifest as invalid pointer accesses. Revocation changed the exploitation outcome for exactly one vulnerability in the set. The artifact and the labelled CVE dataset are [public](https://github.com/mars-research/cheri-impact-artifact).

That study is the single most useful document in this landscape for us, because it is a labelled ground truth of *which kernel bugs which mechanism catches*, and document 03's coverage matrix is built to be evaluated against it directly.

**Temporal safety on CHERI is not free and not complete.** [Cornucopia](https://www.cl.cam.ac.uk/research/security/ctsrd/pdfs/2020oakland-cornucopia.pdf) (Oakland 2020) added sweep-based capability revocation with a kernel-resident service; [Cornucopia Reloaded](https://dl.acm.org/doi/abs/10.1145/3620665.3640416) (ASPLOS 2024) rebuilt it on load barriers using capability load generations, and ships in [CheriBSD 23.11](https://www.cheribsd.org/tutorial/23.11/temporal/) with quarantining allocators and a shared revocation epoch counter. Both are heap-only; **stack temporal safety remains open**.

**2026 work.** [PoisonCap](https://arxiv.org/abs/2605.13210) (Wang, Woodruff, Mazzinghi, Rugg, Joannou, Stark, Watson, Moore; arXiv 2605.13210, May 2026) makes the sharpest point in the literature for our purposes: Cornucopia Reloaded provides use-after-*reallocation* safety, not use-after-*free* safety, and cannot enforce initialization safety at all. PoisonCap introduces a poison capability format stored in the memory data itself, 128 bits per poison capability, no external tags beyond CHERI's tag bit, with poisoning privilege delegated through capability bounds so nested allocators can enforce safety on their consumers. Evaluated on CHERI RISC-V QEMU and the CHERI-Toooba FPGA softcore against 2,776 Juliet cases covering use-after-free, uninitialized access and double free, plus SPEC CPU2006 INT and SQLite; reported as no fundamental overhead relative to a Cornucopia baseline that zeros before reallocation. [PICASSO](https://arxiv.org/abs/2602.09131) takes the other route, colored capabilities with a hardware provenance-validity table allowing bulk retraction without quarantine, ~5% geomean SPEC overhead. [CHERI-SIMT](https://www.asplos-conference.org/asplos2026/program/index.html) (ASPLOS 2026) brings capabilities to GPUs.

**What this means for us.** Three things. The use-after-reallocation versus use-after-free distinction that PoisonCap draws is exactly the distinction document 08 has to make, and it is the reason we choose per-allocation *versions* rather than quarantine-and-sweep: a version compare is a use-after-free check, a quarantine is a use-after-reallocation check, and the ACSAC number says the difference is worth 26 percentage points of kernel vulnerability coverage. Second, the 61%-with-temporal figure is the ceiling for a bounds-and-lifetime mechanism on kernel code and tells us that documents 09's type and initialization planes are not optional extras if we want to beat it. Third, CHERI is a better mechanism than ours and where the hardware exists we should use it; document 05 keeps the metadata plane abstract so a CHERI backend is a representation choice rather than a rewrite.

## 3. Hardware tagging: ARM MTE

MTE, from ARMv8.5, stores a 4-bit tag per 16-byte granule in dedicated tag storage and matches it against a tag in the pointer's top byte, using Top Byte Ignore. The check is in the memory pipeline, so there are no extra instructions. Arm estimates roughly [1-2% overhead in ASYNC mode](https://newsroom.arm.com/blog/memory-safety-arm-memory-tagging-extension); SYNC mode aborts immediately with full fault information and costs more. Google added MTE to Scudo, Android's default allocator.

The limits are well documented. Four bits means sixteen tags and roughly a 1-in-16 chance of missing any given violation. `0xFF` is a match-all tag and accesses through such pointers are unchecked. The 16-byte granule means **intra-granule overflow is invisible**. Google Project Zero's [kernel analysis](https://projectzero.google/2023/08/mte-as-implemented-part-3-kernel.html) notes that `SLAB_TYPESAFE_BY_RCU` regions permit use-after-free by design and so cannot be protected by tagging, and that a single memory write may suffice to disable KASAN.

Deployment: Pixel 8 was the first production device, late 2023. As of late August 2026, reporting indicates the Pixel 11's Tensor G6 **omits the MTE hardware blocks**, blocking the GrapheneOS port and making it the first Google flagship in three years without silicon memory tagging [single secondary source, [Tech Times](https://www.techtimes.com/articles/325985/20260831/google-removed-pixel-11-memory-safety-hardware-blocking-grapheneos-port.htm), 31 August 2026, **unverified**, and worth re-checking before any decision rests on it].

[NanoTag](https://arxiv.org/html/2509.22027v1) (Li, Ye, Devietti, Jana, Khan; IEEE S&P 2026) is the most interesting recent MTE result: byte-granular overflow detection on *unmodified binaries* by setting a tripwire on granules that may need intra-granule checking, so hardware handles the common case and software handles the rest. Built on Scudo, it detects nearly as many bugs as ASan at overhead similar to Scudo in MTE SYNC mode. [Optimized tagging on AmpereOne](https://arxiv.org/abs/2511.17773v1) is the server-side counterpart.

**What this means for us.** MTE is an accelerator, not a mechanism. Its granularity and its 1-in-16 miss rate disqualify it as the basis of a *sound* monitor, which is what Tier D has to be. It is an excellent basis for Tier E and Tier K3 on arm64, where probabilistic is acceptable and 1-2% is not. Document 05 specifies MTE as an alternate lowering for the bounds query on hardware that has it, and NanoTag's tripwire technique is the right way to keep sub-object coverage while using it. The Pixel 11 report, if it holds, is a reason not to make any tier *depend* on MTE.

## 4. Language-level approaches

**`-fbounds-safety`**, Apple's Clang extension, is the deployment success story in this category: [adopted on millions of lines of production C in a consumer operating system](https://clang.llvm.org/docs/BoundsSafety.html). Its contribution is reconciling explicit bounds annotations at ABI boundaries (`__counted_by`, `__counted_by_or_null`, `__sized_by`) with *implicit* wide pointers for locals (`__bidi_indexable`), so most function bodies need no annotation while the ABI stays C-compatible. Adoption is per-file and incremental. Upstreaming is partial as of 2026, staged behind `-fexperimental-bounds-safety`, and is an [active GSoC 2026 project](https://discourse.llvm.org/t/gsoc-2026-participating-in-upstreaming-fbounds-safety/89649); the frontend pieces such as `__counted_by` parsing have landed and the enforcement pieces have not. The Linux kernel already uses `__counted_by` in its own headers.

**[Checked C](https://github.com/checkedc/checkedc)** splits the program into `_Checked` and `_Unchecked` regions, gives three flavours of checked pointer, forbids forging a checked pointer from a raw one, and provides bounds-safe interfaces for legacy boundaries. Temporal safety was added later by [Zhou, Criswell and Hicks](https://arxiv.org/pdf/2208.12900) (OOPSLA 2023) with lock-and-key fat pointers, which is the mechanism document 08 adopts. Their related-work numbers are the ones to beat: CETS at 48% overhead on selected SPEC CPU2006, PTAuth at 26%, ViK at 9% but with 10-16 bit key spaces and a fixed maximum object size.

**The SEI's Pointer Ownership Model.** [CMU/SEI-2025-TR-008](https://sei.cmu.edu/documents/6361/Design-of-Enhanced-Pointer-Ownership-Model-for-C.pdf) (September 2025) and ["A Pointer-Ownership Model for C Inspired by Rust"](https://doi.org/10.1145/3814943.3816182) (Svoboda, Klieber, Hoskinson, Flynn, Martins; LCTES 2026) build a temporal safety model over unmodified C by distinguishing responsible from irresponsible pointers, using a SAT solver to enforce the constraints and an LLM to complete the per-program model where inference is incomplete. Evaluated on all 4,604 Juliet C cases for CWE-401, 415, 416, 590 and 761. Code at [`cmu-sei/pom`](https://github.com/cmu-sei/pom).

**TrapC** ([WG14 N3423](https://www.open-std.org/jtc1/sc22/wg14/www/docs/n3423.pdf), Robin Rowe) removes `goto` and `union`, adds `trap` and `alias`, and makes pointers lifetime-managed. It met significant skepticism at WG14, particularly over removing `union`.

**Translation to Rust.** [Cpp2Rust](https://pldi26.sigplan.org/details/pldi-2026-papers/23/Cpp2Rust-Automatic-Translation-of-C-to-Safe-Rust) (PLDI 2026) is the first system translating C++ to functionally equivalent memory-safe Rust automatically, by inserting runtime-enforced ownership and mutability checks where C++'s unrestricted aliasing does not fit Rust's model. Evaluated on 13k lines across WOFF2 and Brunsli: full memory safety at a 2% penalty on WOFF2 and **6x slower on Brunsli**, which is heavy in pointer arithmetic. DARPA's TRACTOR programme is the funded version of the same idea.

**What this means for us.** Four things. `-fbounds-safety` proves the annotation model works at scale and its annotation vocabulary is the one to accept, because the kernel already writes it, document 07 treats `__counted_by` as a *check-elimination hint*, which is the right framing and the one that keeps annotations optional. Checked C's numbers set the bar for lock-and-key overhead and document 13 quotes them as the baseline to beat. The POM work is the model for optional whole-program inference that reduces overhead without being required for correctness, and its Juliet evaluation protocol is the one document 14 copies. Cpp2Rust's 6x-on-pointer-arithmetic result is the useful negative datum: it is the same workload shape that will be worst for us, and it says the honest reporting unit is per-benchmark rather than geomean.

## 5. Sanitizers, and their prices

The numbers below are the reason "one plane, many queries" is the right architecture: these tools are individually expensive, mutually exclusive, and collectively still incomplete.

| Tool | Slowdown | Memory | Catches | Source |
|---|---|---|---|---|
| ASan | ~2x | ~3x | heap/stack/global OOB, heap UAF (quarantine-bounded) | LLVM docs |
| MSan | ~3x, more with origins | large | uninitialized reads; requires *all* code instrumented | LLVM docs |
| TSan | 5-15x | 5-10x | data races | [LLVM docs](https://clang.llvm.org/docs/ThreadSanitizer.html) |
| TySan | 2-3x best case | ~8x (8 shadow bytes per byte) | TBAA / effective-type violations | [Clang docs](https://clang.llvm.org/docs/TypeSanitizer.html) |
| KASAN generic | ~3x | ~1/8 of RAM | kernel OOB, UAF | [kernel docs](https://docs.kernel.org/dev-tools/kasan.html) |
| KASAN SW_TAGS | lower | ~1/16 of RAM | same, arm64 only | kernel docs |
| KASAN HW_TAGS | low | low | same, probabilistic, needs MTE | kernel docs |
| KCSAN | 5.0x boot default, 2.8x fast path | a few MiB | kernel data races | [kernel docs](https://docs.kernel.org/dev-tools/kcsan.html) |
| KMSAN | very high | very high | kernel uninitialized reads | kernel docs, "not for production" |
| KFENCE | near zero | small | sampled OOB/UAF/invalid-free | kernel docs |

TySan is worth a second look because its problem is ours. It [merged into LLVM in December 2024](https://www.phoronix.com/news/LLVM-Merge-TySan-Type-Sanitizer) for LLVM 20, generates descriptor tables from the same TBAA metadata the optimizer uses, intercepts `memcpy` and `memset`, and re-runs the full TBAA algorithm at run time. Critically, **it disables TBAA for alias analysis while active**, so the optimizer will not delete the accesses it needs to see. That is the exact tension document 06 has to resolve, and TySan resolves it by giving up the optimization. We cannot, because Tier E has a 1.3x budget.

The parent specification already promised `-fsanitize=alias` and `-fsanitize=restrict` as novel contributions in [document 07](../07-types-and-semantics.md) and [document 12](../12-abi-and-runtime.md). This document set is where those grow into the type plane in document 09.

**What this means for us.** The KASAN annotation surface is the most valuable asset in this table and document 11 is built to consume it. The overhead figures set Tier D's budget: 4x for a superset of ASan + MSan + TySan + safety-relevant TSan is a real improvement over 2x + 3x + 2-3x + 5-15x for four modes you cannot combine. And KFENCE is the model for a Tier E fallback where a sound monitor is unaffordable: sampling with near-zero cost across a fleet finds bugs that no test workload reaches.

## 6. Formal footing

**[The Downgrading Semantics of Memory Safety](https://arxiv.org/abs/2507.11282)** (Hansen, Larsen, Askarov; PLDI 2026) is the paper document 04 is built on. It defines memory safety positively rather than as a list of prohibited events: **gradual allocator independence**, the property that the allocator must not influence program execution, framed as noninterference with two sanctioned downgrading events, out-of-memory, and pointer-to-integer casts. Null dereference, use-after-free, double free and heap overflow all fall out as consequences. This matters because "catch all memory bugs" is only falsifiable against a definition of memory safety that is not itself a list, and every previous list has been incomplete.

**Provenance.** [WG14 N3005](https://www.open-std.org/jtc1/sc22/wg14/www/docs/n3005.pdf), the PNVI-ae-udi model, already adopted by the parent's document 07, resting on Memarian et al.'s "Into the depths of C" (PLDI 2016) and "Exploring C semantics and pointer provenance" (POPL 2019). WG14's effective-type and provenance cleanup continues in the "Slay Some Earthly Demons" series (N3244, N3409, N3410).

**Sound whole-program analysis** is the technology that discharges checks. [Frama-C](https://frama-c.com/)'s Eva plugin is a sound abstract interpreter over most of C99 and its WP plugin discharges up to 98% of verification conditions on real case studies with Alt-Ergo, CVC5 or Z3. Verasco is the Coq-verified analyzer that composes with CompCert. An [OOPSLA 2023 result](https://dl.acm.org/doi/pdf/10.1145/3622855) on SMT encodings of memory models reports up to 40% verification-time reduction and was applied to Linux kernel code.

**Static check elimination**, which is where our overhead budget is won or lost:

- **CCured** (Necula, McPeak, Weimer, POPL 2002) infers that most or all pointers in many C programs are statically type-safe and instruments only the rest. This is the original and still the right shape.
- **PICO** ([Presburger In-bounds Check Optimization](https://dl.acm.org/doi/fullHtml/10.1145/3460434)) captures spatial safety exactly with Presburger formulas, then either discharges checks or replaces many checks with one placed at an infrequently executed point. Over SoftBound on SPEC: **36% average execution-time reduction, 24% code-size reduction**.
- **CHOP** ([Convex Hull Optimization](https://arxiv.org/pdf/1907.04241)) uses runtime data from past executions to build a knowledge base of sufficient conditions for redundancy: about **80% of dynamic bounds-check instructions avoided**, up to 95.8% improvement over SoftBound.
- **ShadowBound** (arXiv 2406.02023) does runtime-driven elimination by having the allocator hand the compiler facts static analysis cannot derive.
- **Baggy bounds** ([UCAM-CL-TR-798](https://www.cl.cam.ac.uk/techreports/UCAM-CL-TR-798.pdf)) reports ~30% overhead with 7.5% average peak memory, which the author calls short of the 10% ideal.

**What this means for us.** The downgrading-semantics definition goes straight into document 04 as the statement of what the monitor is a monitor *for*, and it is what lets document 02 use the word "all" without lying. PICO and CHOP together say the achievable reduction on a naive instrumentation is large but not unbounded (roughly a third statically, roughly four fifths with profile data) and document 13's Tier E budget is built on the assumption that PICO-class static elimination plus PGO-driven CHOP-class elimination compose, which is itself an assumption worth listing in document 17. The Frama-C and Verasco results say that a *sound* analyzer strong enough to discharge checks is a decade-scale project, which is why document 07 discharges checks with narrow, verified rewrite rules over the ægraph rather than with a general abstract interpreter.

## 7. Why anyone should care, in numbers

Matt Miller's 2019 MSRC analysis, still the most-cited figure: roughly **70% of CVEs Microsoft assigns each year are memory-safety issues**, quoted by [CISA](https://www.cisa.gov/news-events/news/urgent-need-memory-safety-software-products). Chromium's analysis of 900+ high and critical severity bugs since 2015 found [about 70% memory-safety](https://www.chromium.org/Home/chromium-security/memory-safety/), split 36% use-after-free and 32% other. Android reports 78% of confirmed in-the-wild exploited vulnerabilities are memory-safety violations, and that after adopting Rust for new code the memory-safety share of Android vulnerabilities fell from 76% to 35%.

Chromium's own security team now writes that the Rule of Two is [straining](https://chromium.googlesource.com/chromium/src/+/main/docs/security/rule-of-2.md): process isolation and sandboxing have stopped yielding enough, and it has become cheaper to fix bugs at the source. That is the market for this work.

On the Rust side: at the December 2025 Linux Kernel Maintainers Summit the consensus was that the Rust experiment had succeeded and Rust should lose experimental status, becoming a permanent core kernel language [reported widely; primary LWN coverage not re-read for this check, **unverified** on the exact wording]. The 2026 Rust project goals for Rust-for-Linux list coherence domains and niche optimizations for pointer-carrying enums.

**What this means for us.** The 36%-use-after-free figure in Chrome and the 61%-with-temporal figure in the ACSAC kernel study point the same direction: temporal safety is where the value is, it is the expensive half, and a design that ships spatial safety first and defers temporal safety has shipped the cheap half. Document 08 is therefore the hard document and document 16 does not let a milestone claim success without it. And the Rust trajectory sets the honest framing for this whole project: new kernel code is going to Rust and should, this is for the twenty-eight million lines that are not moving.

## 8. Facts that did not survive checking

Three things, recorded so nobody re-derives them.

There is **no published per-benchmark Fil-C performance table**. The circulating numbers are the author's good-case/bad-case bracket and LWN's "about four times" summary. Any comparison in document 13 must be measured by us on our corpus, not quoted.

The **Pixel 11 MTE removal** rests on a single secondary source dated 31 August 2026. If a tier's design comes to depend on MTE availability, verify this against Arm and Google primary sources first.

The exact current status of **Rust's experimental designation in the kernel** and the current `-std=` and minimum GCC version for the kernel tree are both carried over as unverified from the parent's document 01 section 7. Read `Documentation/process/changes.rst` in the tree you are targeting.
