# The landscape, September 2026

What exists, what it costs, what it cannot do, and what each of them means for this design. Sections end with "What this means for us."

The organizing observation: **static memory-safety proof has six distinct traditions that barely cite each other**, and the reason no compiler has integrated them is that each was built as a standalone tool with its own annotation language, its own notion of failure, and its own idea of an acceptable running time. The opportunity is integration, not invention.

## 1. Separation logic: the deep end

The tradition that can actually prove memory safety of real systems C, at the cost of human effort per function.

**VeriFast** (KU Leuven, imec-DistriNet) is a sound, modular, auto-active verifier for single- and multi-threaded C, Java and unsafe Rust. It reasons by symbolic execution; assertions over data values are discharged to Z3 against the current path condition. Specifications use inductive datatypes, primitive recursive pure functions, abstract predicates, points-to assertions and separating conjunction, and in practice need loop invariants, explicit `open`/`close` of predicates, and lemmas. It catches dangling-pointer dereference and double free directly, because those are what separation logic is *about*.

**RefinedC** ([Sammler et al.](https://pub.ista.ac.at/~msammler/paper/refinedc.pdf)) is the foundational version: proofs live in Coq/Rocq and are checked there, but the aim is full automation so the user need not enter Coq. Annotations for pre/postconditions and invariants drive a *refinement type system* translated into a subset of Iris, giving syntax-directed proof search; typing rules are lemmas about the separation-logic model of types, proved sound in Iris. The stated caveat is the honest one: automation may require an expert to extend the typing system for a new use case, and unresolved verification conditions still need manual work.

**CN** ([Pulte, Makwana, Sewell, Memarian, Krishnaswami et al., POPL 2023](https://dl.acm.org/doi/10.1145/3571194); [tool](https://github.com/rems-project/cn)) is the one closest to this project's needs, for three reasons. It is built on the **Cerberus C semantics**, which is the same body of work that produced PNVI-ae-udi, the memory model [`../safe-memory/04`](../safe-memory/04-safety-model.md) and the parent's document 07 already commit to. It reduces refinement typing to decidable propositional reasoning with Z3, uses *first-class resources* so that pointer aliasing and pointer arithmetic are expressible rather than excluded, has resource inference for iterated separating conjunction (which is what array reasoning needs), and restricts ghost variables syntactically so that their inference is guaranteed to succeed. And it was used to verify **the pKVM hypervisor's buddy allocator**: pre-existing code, written by the pKVM developers, not written for a prover.

Two 2026 developments matter. [**Fulminate**](https://dl.acm.org/doi/10.1145/3704879) compiles CN specifications into C assertions and *tests* them, so a specification can be debugged by running it before anyone tries to prove it. And a PLDI 2026 paper, *Code-Specify-Test-Debug-Prove: Flexibly Integrating Separation Logic Specification into Conventional Workflows* (Aamer, Banerjee, Katsura, Kaloper-Meršinjak, Economou, Memarian, Makwana, Krishnaswami, Pierce, Pulte, Sewell), makes the workflow argument explicitly.

**QCP** ([arXiv:2505.12878](https://arxiv.org/html/2505.12878v3)) computes strongest postconditions by symbolic execution and discharges verification conditions with an entailment solver combining rule-based abductive reasoning with a lightweight SMT solver, reporting times comparable to VeriFast in fully automatic mode.

**VST** is Coq-based and integrated with CompCert, so properties survive compilation, the strongest end-to-end story available, and the one requiring the most manual guidance.

**Foundational VeriFast** ([arXiv:2601.13727](https://arxiv.org/html/2601.13727), 2026) addresses the fact that VeriFast's ~30K lines of OCaml are themselves unverified, by emitting a Rocq proof script on success and replaying the symbolic execution in Rocq, "hinted mirroring." This is the certificate idea, done properly, and it is the direct ancestor of [document 10](10-soundness-and-trust.md)'s design.

**What this means for us.** CN is the right shape for the deep layer of [document 07](07-separation-logic.md), and the reason is not its automation but its *semantics*: it is built on Cerberus, and so are we. A specification language whose memory model is the same memory model the rest of the compiler uses is worth more than one with better proof automation and a different model, because the alternative is two definitions of what a pointer is. Fulminate's testing-before-proving is the ergonomic idea to copy, and Foundational VeriFast's mirroring is the trust story to copy. What we do *not* copy is the posture: separation logic is per-function, opt-in, for allocators and parsers, and it never runs by default.

## 2. Refinement and liquid types: the affordable middle

**Flux** ([Lehmann, Geller, Vazou, Jhala, PLDI 2023](https://dl.acm.org/doi/10.1145/3591283); [tool](https://github.com/flux-rs/flux)) is a refinement type checker for Rust, built on Liquid Types, implemented as a compiler plug-in. It indexes mutable locations with pure values in refinements, exploits Rust's ownership to abstract sub-structural reasoning, supports strong updates, and synthesizes loop invariants (including quantified invariants about container contents) by liquid inference. Soundness is proved against Stacked Borrows.

The number that decides the design is the comparison against Prusti: for lightweight but ubiquitous properties like bounds safety, liquid typing cut **specification lines by 2x, verification time by an order of magnitude, and annotation overhead from up to 24% of code size (average 9%) to nothing at all.** Zero annotation, because the invariants are inferred.

That is the whole argument for putting a refinement layer between the cheap domains and separation logic: it is the last layer that requires no human, and its target (array bounds) is exactly the obligation class that dominates the count.

**Checked C** is the C-side ancestor, inspired by Deputy and Cyclone, distinguished from them by letting checked and unchecked code coexist; `_Ptr<T>`, `_Array_ptr<T>`, `_Nt_Array_ptr<T>`, with checked regions guaranteeing spatial safety.

**What this means for us.** [Document 06](06-bounds-and-refinements.md) is a liquid-types layer for C obligations, with `__counted_by` and friends read as *refinements already written by the programmer* rather than as a separate feature. Flux's zero-annotation result is the target and its 9%-average-annotation comparison point is the number to beat, since our annotations are optional by construction.

## 3. Abstract interpretation: the tradition that scaled and then stopped

**Astrée** ([AbsInt](https://www.absint.com/astree/index.htm); [Cousot et al.](https://www.di.ens.fr/~rival/papers/erts10.pdf)) proves absence of run-time errors in safety-critical C, computing the set of values each variable can take over all executions. It is sound and incomplete, so it finds all run-time errors and may report spurious alarms, and its distinguishing achievement is **zero false alarms in practice** on its target program class, obtained by tuning abstractions to that class. It has proved absence of run-time errors on real industrial code of several hundred thousand lines in a few hours. Precision comes from small-array expansion, widening with thresholds, loop unrolling, trace partitioning, and relations between loop counters and other variables; efficiency from a clever representation of abstract environments, plus a parallel implementation. **Its scope excludes recursion, dynamic allocation and (in the base tool) concurrency.**

**Frama-C's Eva** ([manual](https://www.frama-c.com/download/frama-c-eva-manual.pdf)) is the open equivalent: context-sensitive abstract interpretation over several numerical domains, targeting invalid memory access, uninitialized reads, integer overflow, division by zero and dangling pointers, over the whole program, reporting all errors in its supported UB class. Frama-C's architecture is the interesting part for us: a small kernel holding the program representation and a property database in ACSL, with plug-ins that either *assert* a property or *ask* whether one holds, in the hope another plug-in validates it later. E-ACSL then monitors the alarms at run time and **does not instrument annotations already proved by a static analyzer.**

That last sentence is this entire specification, invented in another project a decade ago, and it is worth being explicit that we are not the first to think of it. What Frama-C has not done is put it inside a production compiler with no alarm list.

**The two limits.** *Scope*: Astrée's exclusions (no recursion, no dynamic allocation) are exactly the C that CVEs live in. *Time*: hours for hundreds of thousands of lines. The literature's own critique notes that static analysis tools are hardly integrated into CI/CD because they remain time- and memory-expensive to run after every patch, motivating work on reusing results across small edits, experimented with on Eva.

**What this means for us.** Layers 1 and 2 of [document 04](04-the-discharge-ladder.md) are interval and weakly-relational (octagon-class) domains, and the design constraints come straight from this section: they run per-function, not whole-program; they are *incremental* by construction because a compiler recompiles one translation unit; they have a hard time budget and give up rather than widening forever; and they produce no alarms, because an undischarged obligation becomes a check. Astrée's precision techniques (trace partitioning, loop unrolling, thresholds) are directly reusable and are the highest-yield tuning available.

## 4. Ownership inference: static temporal safety, and its ceiling

The line that tries to recover Rust's discipline from unannotated C.

**CCured** (POPL 2002) infers that most or all pointers in many C programs are statically type-safe and instruments only the rest. It is the shape of the entire enterprise and the companion specification already calibrates against it.

**Crown** (CAV 2023) does static ownership analysis for C-to-Rust translation, inferring ownership models of C pointers, and **scales to half a million lines in under 10 seconds**: which is a genuinely compiler-compatible running time and is the reason this layer is affordable at all.

**&inator** ([Chen, Coughlin, Bond, PLDI 2026](https://arxiv.org/abs/2604.17261); PACMPL 10:PLDI 580-603, 8 June 2026) is the state of the art. It infers Rust types for *interface* variables (struct fields, function parameters, return values, globals) by whole-program constraint-based type inference that incorporates borrow-checking rules. The correctness criterion is an existence property: an inferred interface is correct if there *exists* a safe Rust translation using it that preserves the C program's behavior, modulo dynamic borrowing conflicts and memory leaks; and for a C program without undefined behavior, &inator infers a correct interface. The authors note that CCured, Cyclone and Checked C all did interface inference but none inferred types for a language with *statically enforced ownership and borrowing*. Stated limitations: support for certain C features, and scaling to large programs, are left to future work.

**Scylla** ([arXiv:2412.15042](https://arxiv.org/pdf/2412.15042), OOPSLA1 2026, Article 121) translates an *applicative subset* of C to safe Rust, arguing that existing tools target unsafe Rust (permitting unchecked pointers and transmutations) which defeats the purpose. The word "applicative" is the finding.

**Cpp2Rust** (PLDI 2026, PACMPL 10:PLDI 480-504) takes the other road: rather than proving C++'s aliasing conforms to Rust's ownership model, it inserts **run-time** ownership and mutability checks. On 13k lines it costs 2% on WOFF2 and **6x on Brunsli**, the difference attributed to pointer-arithmetic density.

Also 2026: *Project-Level C-to-Rust Translation via Pointer Knowledge Graphs* (PACMSE 3:FSE 3675-3698), *Mostly Automatic Translation of Language Interpreters from C to Safe Rust* ([arXiv:2606.27122](https://arxiv.org/pdf/2606.27122)), *Validated Code Translation for Projects with External Libraries* ([arXiv:2602.18534](https://arxiv.org/abs/2602.18534)), and *Mitigating False Positives in Static Memory Safety Analysis of Rust Programs via Reinforcement Learning* ([arXiv:2605.04000](https://arxiv.org/pdf/2605.04000)).

**What this means for us, and it is the least comfortable finding in this document.** The ownership tradition's own 2026 results say the technique works on an *applicative subset*, that a full-C translation needs run-time checks, and that the run-time checks cost 6x on pointer-arithmetic-heavy code. That is a direct measurement of how much of real C is not statically ownable. So [document 05](05-ownership-and-lifetimes.md) predicts a **low static discharge rate for temporal obligations** and says so up front, which inverts the intuition that static analysis is the natural home of lifetime reasoning. Crown's 500k-lines-in-10-seconds is the encouraging counterweight: the analysis is cheap even where its yield is modest, so running it costs little and is worth doing for whatever it does discharge.

## 5. Safe dialects and bounds annotations: what actually shipped

**`-fbounds-safety`** ([Clang docs](https://clang.llvm.org/docs/BoundsSafety.html); [adoption guide](https://clang.llvm.org/docs/BoundsSafetyAdoptionGuide.html)) is the deployment success of the decade. Its core contribution is reducing annotation burden by reconciling bounds annotations at ABI boundaries with **implicit wide pointers on locals**, which need no annotation. It is adopted on millions of lines of production C in a consumer operating system. Its adoptability properties are the ones to copy verbatim: designed for incremental adoption because modifying a whole project and its dependencies at once is usually impossible; partially adoptable with real benefit; a *conforming C extension*, so annotated source still compiles on toolchains without support, via a header that macro-defines the annotations away.

Upstreaming is proceeding as a series of PRs behind `-fexperimental-bounds-safety`, with `__counted_by` attribute parsing and TableGen definitions already landed, and a [GSoC 2026 proposal](https://discourse.llvm.org/t/gsoc-2026-proposal-review-upstreaming-fbounds-safety-bringing-implicit-wide-pointers-and-runtime-traps-to-clang/90301) targeting the missing enforcement mechanisms.

**TrapC** ([N3423](https://www.open-std.org/jtc1/sc22/wg14/www/docs/n3423.pdf), presented to WG14 2025-03-02) removes `goto` and `union`, adds `trap` and `alias`, and makes pointers lifetime-managed. Reception was mixed and the union removal drew the most resistance; N3507's `#dialect` pitch explicitly asks what a trap mechanism unlike TrapC's would look like.

**What this means for us.** The lesson is not technical, it is about adoption, and it is the reason [document 08](08-annotations.md) makes annotations optional. `-fbounds-safety` succeeded because it demanded nothing and rewarded increments; Checked C and TrapC ask for a dialect and have not. We read `__counted_by`, `__sized_by`, `__counted_by_or_null` and `__ended_by` as refinements and add no keyword to C.

## 6. Machine-generated proofs: new, fast-moving, and dangerous in a specific way

The 2025-2026 explosion, all of it downstream of one observation: proof artifacts are text with a mechanical oracle, which is the ideal setting for a language model.

**AutoVerus** ([PACMPL, 10.1145/3763174](https://dl.acm.org/doi/10.1145/3763174)) generates correctness proofs for Rust code in Verus using a network of agents mimicking the three human phases, preliminary generation, refinement by generic tips, debugging by verification errors. On a benchmark of 150 non-trivial tasks it reports **correct proofs for over 90%, averaging under 30 seconds each.**

**ExVerus** ([arXiv:2603.25810](https://arxiv.org/pdf/2603.25810)) generates source-level counterexamples during repair, noting that AutoVerus encodes repair strategies as per-error prompts needing manual updates, and that **invariant inference is the most prevalent bottleneck.**

**VeriStruct** ([arXiv:2510.25015](https://arxiv.org/pdf/2510.25015)) targets data-structure modules and points at RL on the verifier's own signal, successful verification is a reward, failure is a diagnostic.

**VCoT-Bench** ([arXiv:2603.18334](https://arxiv.org/html/2603.18334)) is the critique: AlphaVerus, SAFE, AutoVerus, RagVerus and VeriStruct all treat verification as a black box and measure success solely by whether the program verifies.

**KaPilot** ([arXiv:2607.21957](https://arxiv.org/html/2607.21957v1)) extends this to bounded model checking of unsafe Rust with Kani, comparing against AutoSpec (LLM specification generation for Frama-C), and raises the open problem that matters: **whether a specification that passes verification truly captures the intended safety property.**

On the C side: LLM-generated specifications for VeriFast have been studied twice ([arXiv:2411.02318](https://arxiv.org/pdf/2411.02318); [arXiv:2606.26490](https://arxiv.org/pdf/2606.26490)), and *Enabling Memory Safety of C Programs using LLMs* ([arXiv:2404.01096](https://arxiv.org/pdf/2404.01096)) targets Checked C annotation.

**What this means for us, and it is the cleanest fit in the whole document.** VCoT-Bench's and KaPilot's critique ("does the specification capture the intended property?") is *devastating for functional verification and irrelevant for us*, because our specification is not written by anyone. The obligations come from J1-J7 and are fixed. A model is only ever asked to produce the *middle* of the proof: a loop invariant, a `__counted_by` annotation, a predicate, a ghost witness. Every one of those is checked by the layer below, and a wrong one produces a failed proof and a run-time check.

So [document 09](09-inference-and-llm.md)'s rule is short: **a model may propose an annotation or an invariant; it may never discharge an obligation, appear in a certificate, or enter the trust set.** And the build must be reproducible, so generated annotations are committed to the repository as source, not regenerated during compilation.

## 7. The kernel-scale reality check

**seL4** ([CACM](https://cacm.acm.org/research/sel4-formal-verification-of-an-operating-system-kernel/)) remains the reference: machine-checked functional correctness from abstract specification down to C, assuming correct compiler, assembly and hardware. Memory safety falls out as explicit obligations, all array accesses proved in bounds, all pointer accesses proved well-typed even through casts, non-null and alignment obligations at every access, no leaks, no use-after-free, and termination of every kernel call. Coverage has since extended to binary code, and to 64-bit RISC-V. The acknowledged soft spot is instructive: the C semantics assume a flat in-kernel memory view kept consistent by the VM subsystem, and that consistency argument is only informal.

**Why it does not transfer.** seL4 is ~10k lines, single-threaded by design, purpose-built for verification, at roughly 20 person-years. Linux is 40M lines. Linux drivers cannot be analyzed separately from the core because of interdependency, and the whole-kernel source is too large for existing model checkers, so driver analysis uses an environment model rather than the real core.

**What does transfer** is the shape of the successful Linux-adjacent work: *slices*, not the whole. CN's pKVM buddy allocator. Deductive verification of a 26-function benchmark of unmodified kernel library string and memory functions. Four years of automated verification of the [eBPF verifier's range analysis](https://sanjit-bhat.github.io/assets/pdf/ebpf-verifier-range-analysis22.pdf), which is a trusted-computing-base bottleneck worth the effort, and where the note that "the Linux verifier is well over 14k lines and supplying definitions for all kernel structures gets unwieldy fast" is a warning about the environment problem, not the proof problem.

**What this means for us.** [Document 07](07-separation-logic.md) targets slices, chosen by leverage: the slab allocators (because [`../safe-memory/10.4`](../safe-memory/10-boundaries.md)'s interposition API depends on them being right), `copy_to_user`/`copy_from_user` (the highest-yield boundary), and the string/memory library functions that already have a published deductive-verification benchmark. Nothing else. A specification that proposes proving Linux is not a specification.

## 8. Facts that did not survive checking

Per the parent's discipline, the things this document could not confirm.

**Most 2026 arXiv identifiers here were surfaced by search summaries and not individually fetched.** Specifically: ExVerus, VeriStruct, VCoT-Bench, KaPilot, Foundational VeriFast, the second VeriFast LLM study, *Mostly Automatic Translation of Language Interpreters*, and *Validated Code Translation*. The claims attributed to them are as reported by the search index. Before any of them is quoted in a paper or used to justify a design decision, fetch the paper. **[unverified]**

**&inator's evaluation numbers are not stated here because the abstract does not contain them.** The arXiv abstract gives the correctness criterion and the limitations but no lines-of-code, benchmark or success-rate figures, and the full paper was not read. The design in [document 05](05-ownership-and-lifetimes.md) therefore rests on the *criterion* (which is precisely stated and is what matters) and not on a claimed yield. **[unverified: yield]**

**AutoVerus's "over 90% of 150 tasks" is on a benchmark the authors constructed** from existing code-generation and verification benchmarks. It is not a claim about arbitrary systems C and should never be quoted as one.

**Astrée's "zero false alarms" is scoped to its target program class**: synchronous control code without recursion or dynamic allocation. Quoting it without the scope would be dishonest and would set an expectation nothing in this specification can meet.

**No source found gives a static discharge rate for memory-safety obligations on general C.** This is the single number this whole specification is about, and as far as this survey can tell nobody has published it, because nobody has built a system that generates a complete obligation set and reports what fraction was proved. [Document 13](13-evaluation.md) makes producing that number the primary deliverable, which is a good position to be in: the first honest measurement of a quantity is worth more than an incremental improvement on a measured one.
