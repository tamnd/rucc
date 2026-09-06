# Integration into rucc

The concrete diff. Written against the workspace as it stands: version 0.4.1, edition 2024, MSRV 1.85.0, resolver 3, 23 library crates, 2 build tools, 1 runtime library, 1 binary, with the total-order layer rule of [`spec/18-package-layout.md`](../18-package-layout.md) §18.2 enforced by `xtask/layers.toml`.

It assumes the companion's [`../safe-memory/15`](../safe-memory/15-integration.md) has landed: `crates/rucc-safety/` at rank 9 and `runtime/rucc-safe-rt/`.

## 12.1 The layering problem, and how it is solved

The naive design is one crate, `rucc-proof`, holding the whole ladder. **The layer rule forbids it**, and working out why produces the architecture.

The ladder needs dominator trees, loop structure, SSA, alias facts and the pass manager. All of those live in `rucc-opt`, **rank 9**. It also needs to consume the obligations that `rucc-safety` generates, `rucc-safety` is also **rank 9**. A crate at rank 9 may depend on neither, and a crate at rank 10 may depend on both but then cannot be invoked from inside `rucc-opt`'s pipeline, which is where [`11.5`](11-residual-and-composition.md)'s ordering puts most of the work.

Three consequences, and they are the design:

**1. The obligation model lives in `rucc-ir` (rank 8), not in a new crate.** Obligations are IR-attached metadata that passes must maintain, exactly like source locations, and the no-`Open` invariant of [`03.3`](03-obligations.md) is a *verifier* rule, and `rucc-ir` is "the IR, its printer, parser and verifier." This is the right home on the merits, and it has the pleasant side effect that the trust-set core of [`10.4`](10-soundness-and-trust.md) sits at the bottom of the graph where everything can see it and it can see almost nothing.

**2. `rucc-safety` and `rucc-opt` communicate through the IR, not through a dependency.** `rucc-safety` generates obligations into IR metadata; `rucc-opt`'s discharge passes read them from there. Neither crate names the other. This is the same resolution the companion found for type-plane facts, which travel as opaque interned ids exactly as TBAA already does, and it is the second time the layer rule has forced the right answer rather than an awkward one.

**3. Layers 0-3 are passes inside `rucc-opt`; layers 4-6 are a new crate at rank 10.** The split falls where the dependencies do: layers 0-3 need only what `rucc-opt` already has, and layers 4-6 need an SMT solver and an external prover bridge, which §12.4 shows must not be in the compiler's default graph anyway.

## 12.2 New and changed crates

```
crates/
  rucc-ir/          8   + obligation model, certificates, the no-Open verifier rule
  rucc-safety/      9   + obligation generation (companion crate, extended)
  rucc-opt/         9   + discharge layers 0-3, plane-liveness, L0-L3 certificate checking
  rucc-lto/        10   + cross-module summary discharge (layer 3's interprocedural half)
  rucc-proof/      10   NEW: layers 4-6, VC generation, solver bridge, Vc certificate checking
  rucc-driver/     12   + the three-phase pipeline of 12.3; the -fsafety-proof flags
build-tools/
  rucc-verify/          + the `proof/` rule namespace
tools/
  rucc-annotate/        NEW: offline generation (document 09); NOT in the compiler graph
```

Per-crate detail on the ones that change materially:

| Crate | Change | Risk |
|---|---|---|
| `rucc-ir` | `Obligation`, `ObligationId`, `Certificate`, `Evidence`; textual round-trip for all of them; the no-`Open` verifier rule; the parentage rule of [`03.6`](03-obligations.md) | low, but it is a public IR surface change and must be right before anything else is built |
| `rucc-safety` | generation pass; lowering reads `Narrowed` residual predicates | low |
| `rucc-opt` | four new passes; **every existing pass must maintain obligation metadata** | **the most underestimated item in this table**: see §12.6 |
| `rucc-lto` | summaries gain `nofree`, `noescape`, `nowrite`, extent relations, each with a certificate | medium; reuses the companion's summary plumbing |
| `rucc-proof` | the whole of layers 4-6 | high, and entirely optional |
| `rucc-driver` | phase graph gains a proof phase between two optimizer invocations | medium; §12.3 |
| `rucc-verify` | verifies the layer-0 rule set as data, exactly as it verifies lowering rules | low; the machinery exists |

**`rucc-mir`, `rucc-codegen`, `rucc-regalloc`, `rucc-asm`, `rucc-object` and `rucc-debug` are untouched.** Discharge happens entirely above MIR, and the residual is ordinary checks the companion already lowers. That is a real property of the design and it is worth stating: **this specification adds nothing to the backend.**

## 12.3 The pipeline, and the phase split

[`11.5`](11-residual-and-composition.md)'s ordering, arranged so that every arrow respects the layer rule. The key move is that `rucc-opt`'s pipeline is invoked **twice**, with `rucc-proof` between, orchestrated from `rucc-driver` at rank 12 which can see all of them.

```
rucc-driver:
    rucc-lower                 TAST → IR
    rucc-safety::insert        checks, plane maintenance, aux traffic
    rucc-safety::obligations   generate; dumb and complete
    rucc-opt::pipeline_early   L0 · mem2reg · L1 · inline · L1' · L2
    rucc-lto                   summaries, then L3
    rucc-proof                 L4 · L5 · L6          [feature = "proof-smt"]
    rucc-opt::pipeline_late    plane-liveness · ægraph · certificate check
    rucc-ir::verify            the no-Open invariant
    rucc-safety::lower         checks → compares and branches
    rucc-codegen               unchanged
```

At `-fsafety-proof=default`, `rucc-proof` is not invoked at all and the two `rucc-opt` invocations run back to back, which is one reason the default path is cheap and the other reason §12.4 works.

**Splitting `rucc-opt`'s pipeline into `early` and `late` is the one structural change this specification asks of the parent's optimizer**, and it is small: the pass manager already takes a pipeline description, so this is two named pipelines instead of one.

## 12.4 The dependency question, which is the sharpest constraint

The parent's [`spec/18-package-layout.md`](../18-package-layout.md) §18.3 blesses exactly two external dependencies for the compiler (`memmap2` and `object`) and keeps them in the workspace manifest specifically so that adding a third has to be argued for in review.

**An SMT solver is not a third dependency we can add.** It is large, it is a C++ build, and it would sit in the compiled path of every user.

The resolution is designed into the ladder rather than bolted on:

- **Layers 0-3 use no solver.** Intervals, octagons, induction recognition, dominance and summaries are all decided by hand-written analyses over the IR. This is not an accident of the design; it is the reason `default` is layers 0-3 and it should be treated as a hard invariant. **If a proposed layer-0-through-3 technique needs a solver, it is not a layer-0-through-3 technique.**
- **Layers 4-6 use a solver out of process**, behind `--features proof-smt`, off by default, invoking a solver binary found on `PATH` and failing gracefully (as a check, per [`10.7`](10-soundness-and-trust.md)) when it is absent.
- **`rucc-verify` already links a solver** and is CI-only, so the layer-0 rule verification of §12.2 costs nothing new.
- **`rucc-annotate` is a separate binary outside the compiler's graph** ([`09.5`](09-inference-and-llm.md)), so whatever it depends on is irrelevant to the compiler's dependency budget.

The result is that a stock `cargo install rucc` has exactly the dependencies it has today and supports `-fsafety-proof=default`. That property is worth more than layers 4-6.

## 12.5 The diff against the companion specification

Stated per [`../safe-memory/15.6`](../safe-memory/15-integration.md)'s discipline. **Nothing here reverses a companion decision**; three things are re-homed.

| Companion item | Change | Why |
|---|---|---|
| `safety-dce` pass | becomes discharge layers 0-1, emitting certificates | [`11.4`](11-residual-and-composition.md); one eliminator, not two |
| `safety-loop` pass | becomes part of layer 2 (splitting and hoisting) | same |
| `safety-plane` pass | becomes `plane-liveness`, driven by discharge state | [`03.5`](03-obligations.md); it now has a principled trigger |
| the `safety/` rule namespace | unchanged; rules become layer-0 certificate rules | still verified by `rucc-verify` identically |
| `--emit=safety-summary` | gains the obligation, layer and plane blocks of [`03.8`](03-obligations.md) | the accounting was already there in outline |
| the LTO summary set | gains certificates on each summary | [`10.2`](10-soundness-and-trust.md)'s `Summary` evidence |
| `-fsafety-suggest-annotations` | gains ranking and confidence ([`08.7`](08-annotations.md)) | it is now driven by a real obligation set |

The one thing the companion must do differently from day one: **its elimination passes must not be written as free-standing rewrites.** If they are, re-homing them later is a rewrite. [Document 14](14-milestones.md) puts V0 before the companion's S4 for exactly this reason.

## 12.6 The unglamorous risk

**Every pass in `rucc-opt` must maintain obligation metadata**, and there are a lot of passes.

This is the same class of problem as debug-info location maintenance, it has the same failure mode (a pass silently drops metadata and nobody notices for a year) and the parent already has the machinery and the discipline for it. What is different here is the consequence: dropping a source location makes a debugger worse, while dropping an obligation removes a safety check.

Three mitigations, and all three are cheap:

1. **The no-`Open` verifier rule runs at every pass boundary in debug builds**, not only at the end. A pass that drops an obligation trips it immediately, with the pass named.
2. **Obligation counts are invariant across passes except at explicitly-accounted transitions**: promotion by `mem2reg`, duplication by inlining and unrolling, hoisting by layer 2. A pass manager assertion checks that the delta is attributed.
3. **The differential accounting of [`10.6`](10-soundness-and-trust.md)** catches whatever the first two miss, on any executed path.

## 12.7 Flags

Added to the companion's set in [`../safe-memory/15.4`](../safe-memory/15-integration.md).

| Flag | Meaning |
|---|---|
| `-fsafety-proof=off\|fast\|default\|deep\|verify` | the ladder level; [`02.4`](02-the-goal.md) |
| `-fsafety-proof-explain[=<file>]` | render certificates for the named sites |
| `-fsafety-proof-test` | compile specification clauses to run-time assertions; [`07.4`](07-separation-logic.md) |
| `-fsafety-proof-require` | a failed `verify`-level proof is an error |
| `-fsafety-proof-time-budget=<ms>` | wall-clock cap; **warns that the build is no longer reproducible**; [`11.3`](11-residual-and-composition.md) |
| `-fsafety-proof-steps=<n>` | the deterministic step budget multiplier |
| `-fsafety-assume-refcounts` | §[5.5](05-ownership-and-lifetimes.md); counted in the trust set |
| `-fsafety-assume-signal-frees` | §[5.2.1](05-ownership-and-lifetimes.md); counted |
| `-fsafety-assume-nofree=<list>` | declared-safe indirect-call targets; counted |
| `--emit=proof-certificates` | dump the certificate set, for diffing across builds |

`__proof(level)` per [`08.5`](08-annotations.md) overrides the level per function.

**Defaults:** `off` at `-O0`, `fast` at `-O1`, `default` at `-O2` and above, and `default` under `-fsafety=kernel`. Never `deep` implicitly, the compile-time cost must be asked for.

## 12.8 Effort

| Item | Engineer-months |
|---|---|
| Obligation model in `rucc-ir`, verifier rule, round-trip | 0.5 |
| Generation in `rucc-safety` | 0.5 |
| Layer 0 + certificate checking | 1.0 |
| Layer 1 (intervals, dominance) | 1.0 |
| Layer 2 (relational, induction, splitting) | **2.5** |
| Layer 3 (summaries, `nofree` first) | 1.0 |
| plane-liveness | 0.5 |
| Pass-maintenance retrofit across `rucc-opt` | 1.0 |
| Certificate checker hardening, differential accounting | 1.0 |
| **Subtotal, layers 0-3, the shippable system** | **9.0** |
| Layer 4 (refinements, liquid inference, VC encoder) | 3.0 |
| Layer 5 (ownership) | 2.0 |
| Layer 6 (separation logic, path (a)) | 3.5 |
| `rucc-annotate` | 1.5 |
| **Total** | **19.0** |

[`00`](00-README.md) estimates 10-18 engineer-months with layers 0-3 at about 6. This table says 9 for layers 0-3 and 19 total, and the difference is the pass-maintenance retrofit and the certificate checker, both of which are infrastructure that document 00 did not price.

**19 is the number to plan against**, and the discrepancy is recorded rather than reconciled by argument, per the companion's practice in [`../safe-memory/16`](../safe-memory/16-milestones.md).
