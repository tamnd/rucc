# Integration into rucc

The concrete part. Everything before this is design; this is the diff. Crates, layer ranks, IR changes, pass placement, flags, build-system wiring, and what in the existing specification has to change.

Written against `rucc` workspace version 0.4.1, edition 2024, MSRV 1.85.0, 23 library crates, 2 build tools, 1 runtime library.

## 15.1 New crates

Two, plus additions to two existing ones. The layer ranks are from the parent's [document 18](../18-package-layout.md) section 18.2 and are the binding constraint on the whole design.

```
crates/
  rucc-safety/          9   check insertion, the plane model, the boundary tables
build-tools/
  (rucc-verify)             extended: the safety/ rule namespace
runtime/
  rucc-safe-rt/             #![no_std], compiled for the target: planes, allocator,
                            boundary wrappers, the reporter
```

**`rucc-safety` at rank 9**, alongside `rucc-lower`, `rucc-opt`, `rucc-mir`, `rucc-asm` and `rucc-debug`. It may depend on `rucc-ir` (8), `rucc-types` (2), `rucc-target` (1), `rucc-session` (3) and `rucc-diag` (1). It may **not** depend on `rucc-lower` or `rucc-opt`, which share its rank.

That constraint is not an inconvenience; it forces the right architecture. `rucc-safety` consumes IR and produces IR. It never sees the AST. The driver sequences it between `rucc-lower` and `rucc-opt` (which is exactly what document 06 section 6.3 specifies) and the sequencing lives in `rucc-driver` (12), which outranks all three.

**The consequence that matters: `rucc-opt` cannot see `rucc-types`.** Rank 9 to rank 2 is a legal edge for `rucc-safety`, but `rucc-opt` is also rank 9 and *also* cannot depend on `rucc-types`, per the parent's explicit statement that "everything C-specific has been resolved and must not be re-derived here." So the check-elimination rules in document 07, which run in `rucc-opt`, cannot ask the C type system anything.

The resolution is that **type-plane facts travel in IR metadata as opaque interned ids**, exactly as the parent's document 08 already carries TBAA metadata on memory operations. `rucc-safety` resolves `TypeId`s into `!tbaa`-shaped metadata nodes; `rucc-opt` compares them for equality and consults a compatibility relation that is *data attached to the module*, not a call into `rucc-types`. This is the same discipline the parent's alias analysis already lives under, so it costs nothing new, and it is the kind of thing that would have been discovered painfully during implementation if the layer rule did not force it out now.

**`rucc-safe-rt` in `runtime/`**, alongside `rucc-builtins`. `#![no_std]`, compiled *for the target* rather than for the host, per the parent's document 18. Contents: the shadow plane mapping and accessors, the default allocator with the header/aux/payload layout of document 05.2.2, the interposition API of document 10.4, the generated libc wrappers of document 10.3, the TLS call frame of document 05.3, and `__rucc_safety_fail` with its `.rucc_safety_desc` reader.

It is deliberately small (a few thousand lines) because it is in document 14.8's trust set and because everything the compiler can do at compile time it does. A kernel build uses a subset with the allocator and the libc wrappers removed, per document 11.

**`rucc-verify` extended, not replaced.** The `safety/` rule namespace lives under `rucc-codegen/rules/` with the lowering rules, because the parent's document 18 requires the rule tree to be inside the package that builds it. `rucc-verify` already reads that tree; it gains the plane theory of document 14.2 and a new obligation form.

## 15.2 Changes to existing crates

| Crate | Rank | Change |
|---|---|---|
| `rucc-ir` | 8 | the `cap` value type, 18 instructions, 4 facts, the plane metadata node kind, verifier rules for all of them, printer/parser round-trip |
| `rucc-types` | 2 | expose the effective-type compatibility relation as extractable data; no new types |
| `rucc-lower` | 9 | emit `!bounds` and `!aligned` facts it already knows; mark allocator calls; nothing else |
| `rucc-opt` | 9 | the elimination passes of document 07; the CFG-skeleton pinning of document 06.2.4 |
| `rucc-lto` | 10 | summary fields: `nofree`, dereferenced ranges, freed and escaping parameters |
| `rucc-codegen` | 11 | `rules/safety/*`; lowering of the check instructions; `.rucc_safety_desc` emission |
| `rucc-object` | 8 | the `.rucc_aux` and `.rucc_safety_desc` sections, and the aux relocations of document 05.2.2 |
| `rucc-driver` | 12 | flags, pass sequencing, `--emit=safety-summary`, linking `rucc-safe-rt` |
| `rucc-diag` | 1 | the safety report schema, sharing the existing JSON envelope |
| `rucc-debug` | 9 | nothing required; the descriptors reuse existing DWARF |
| `xtask` | none | `cargo xtask corpus --tier=D`, `cargo xtask verify-rules --safety`, `cargo xtask diff-checks` |

**`rucc-object`'s static aux relocations** are the least obvious item and the one most likely to be underestimated. A statically initialized pointer in `.data` needs a correct capability in `.rucc_aux` before `main` runs, which means a relocation per initialized pointer, emitted by the object writer, resolved by the linker. This works with the system linker because it is an ordinary relocation into an ordinary section; it is nonetheless the place where a mistake produces a program that fails in the dynamic loader with no useful message, and it deserves its own tests early.

**`rucc-ir`'s verifier** must reject malformed safety IR as aggressively as it rejects malformed SSA, a `check.bounds` whose capability operand is not dominated by its definition, a `meta.end` without a matching `meta.begin` on some path, a `cap.load` of a non-pointer-typed slot. The parent's document 08 already has a verifier and this is more rules for it, which is much cheaper than discovering the same errors as miscompilations.

## 15.3 Pass placement

The pipeline, with the additions marked:

```
  rucc-lower          TAST → IR, SSA construction, ABI lowering
+ rucc-safety         insertion: checks, plane maintenance, aux traffic   [document 06.3]
  rucc-opt  mem2reg   promotion; removes most locals and their checks     [document 06.4]
  rucc-opt  inline    brings caller facts to callee checks
+ rucc-opt  safety-dce  redundancy over the dominator tree                [document 07.3]
  rucc-opt  ægraph    the ordinary middle end; check operands participate
+ rucc-opt  safety-loop loop hoisting and splitting                       [document 07.4]
+ rucc-opt  safety-plane plane-write coalescing, aux elision              [document 07.6]
  rucc-lto            summaries; cross-module facts                       [document 07.5]
+ rucc-safety  lower  checks → compares and branches                      [document 06.3.1]
  rucc-codegen        selection, scheduling, frames
```

Four commitments in that ordering, each with a reason already argued:

**Insertion is before `mem2reg`.** This looks wasteful (we insert checks on locals that are about to be promoted away) and it is correct, because insertion is a syntactic walk that needs the addresses to still exist. `mem2reg` then deletes the `alloca`, its aux, its `meta.begin`/`meta.end` and its checks together, which is where the largest single share of Tier E's savings comes from and is why Tier E at `-O0` is not absurd.

**Insertion is before inlining**, so that a caller's established facts and a callee's checks meet in one function and the dominator walk discharges them. This is what makes check elimination interprocedural without an interprocedural analysis.

**Check lowering is after LTO and before instruction selection.** Checks stay as `check.*` instructions through the whole middle end so that every pass can reason about them; they become compares and branches only when nothing further will look at them.

**`safety-dce` runs before the ægraph and `safety-loop` after.** The redundancy walk wants the CFG the frontend produced; the loop transformations want the ægraph's canonicalized induction variables. Document 06.2.4's split (checks pinned to the CFG skeleton, operands in the e-graph) is what allows both.

## 15.4 Flags

The parent's document 12 already owns `-fsanitize=`. These extend it rather than opening a second namespace.

**Tier selection.** One flag, because the tiers of document 02 are the product:

```
-fsafety=off | detect | enforce | kernel      default: off
```

`-fsafety=detect` is Tier D, `=enforce` Tier E, `=kernel` Tier K. Everything below is a modifier and none of them need to be set for a tier to be meaningful.

**Plane and class modifiers.**

```
-fsafety-subobject[=strict]        S4; document 09.4              default off
-fsafety-init=none|pointer|byte|padding                           tier-dependent
-fsafety-types=off|pointer|full    Y1..Y5; off under -fno-strict-aliasing
-fsafety-races=off|metadata|pointer                               tier-dependent
-fsafety-leaks                     T9 sweep; document 08.7        default off
-fsafety-pointer-tag-bits=N        document 03.5's tagged pointers
```

**Behavior and reporting.**

```
-fsafety-on-error=abort|continue|log     default: abort at E and K3, continue at D
-fsafety-report=text|json
--emit=safety-summary                    document 07.8
-fsafety-suggest-annotations             document 07.5
```

**Boundary control.**

```
-fsafety-asm=permit|strict         document 10.6                  default permit
-fsafety-exempt=<file>             declared regions, with reasons; counted
-fsafety-hw=none|mte|pac|cheri     document 05.4                  default none
```

**Relationship to the existing sanitizers.** `-fsanitize=address`, `=memory`, `=alias` and `=restrict` from the parent's document 07 table remain, and where `-fsafety=` covers the same ground it *supersedes* them: `-fsafety=detect` implies and subsumes `-fsanitize=alias` and `=restrict`, and combining them is an error rather than a silent double-instrumentation. `-fsanitize=undefined`'s non-memory checks are orthogonal and compose freely, which is the correct relationship, document 03.6 says integer overflow and shift-amount checking are a different feature that shares a prefix.

## 15.5 The build-system surface

**Userspace.** `rucc -fsafety=enforce foo.c -o foo` links `rucc-safe-rt` automatically, as `-fsanitize=address` links its runtime. `LD_PRELOAD`-able form per document 10.7. Nothing else changes: no new linker, no changed ABI, no rebuild of libc required.

**The kernel.** `CONFIG_RUCC_SAFETY=n|K3|K2|K1`, with the existing `KASAN_SANITIZE`/`KMSAN_SANITIZE` Makefile variables consumed as-is and `RUCC_SAFETY_SANITIZE` added only where the exclusion set differs. `arch/*/include/asm/rucc_safety.h` per architecture for the shadow offsets, mirroring `KASAN_SHADOW_OFFSET`. Document 11.3.

**`xtask` additions.** `cargo xtask corpus --tier=D --project=sqlite` runs one project and emits a document 12.5 scoreboard fragment; `cargo xtask diff-checks` runs the differential accounting of document 14.3; `cargo xtask verify-rules --safety` gates the `safety/` namespace. All three are CI jobs.

## 15.6 What in the parent specification has to change

Listed as a diff, because a sub-specification that quietly contradicts its parent is worse than one that does not exist.

| Parent doc | Change | Why |
|---|---|---|
| 07 §7.7 | the no-poison uninitialized-read decision is promoted from a preference to a **requirement**, with a cross-reference here | document 09.2.1: a monitor cannot report on a program whose post-violation behavior is undefined |
| 07 §7.8 | PNVI-ae-udi's exposed-address rule gains an *observable* consequence: exposures are counted | document 04.3 |
| 08 §8.2 | the `cap` value type; opaque, like `ptr` | document 06.2.1 |
| 08 §8.4 | four new facts alongside `noalias` and `provenance` | document 06.2.3 |
| 09 | the rewrite DSL gains a `safety/` namespace and the notion of a `may_trap`, `readonly` instruction pinned to the CFG skeleton | document 06.2.4 |
| 09 §9.8 | summaries gain `nofree` and parameter dereference ranges | document 07.5 |
| 12 §12.9 | the sanitizer runtime section gains `rucc-safe-rt` and the shadow scheme is unified with the one already sketched for `-fsanitize=alias` | document 05.2.3 |
| 14 | the target ladder gains the Tier K rungs | document 11.2 |
| 17 | milestones S0-S7 interleave with M0-M11 | document 16 |
| 18 | two new packages and their ranks | §15.1 |
| 19 | questions Q1-Q5 gain the seven of document 17 | document 17 |

**Nothing here reverses a parent decision.** The strongest statement is the first row, and it strengthens an existing choice rather than changing it. That is the expected shape: document 02.5's argument is that the parent already specified everything this needs for other reasons, and if this document had required reversing the parent's design, that argument would have been wrong.

## 15.7 What this costs to build

Document 00 says 12-20 engineer-months on top of the parent's 40-70, and cannot start before M4. The breakdown:

| Piece | Months | Depends on |
|---|---|---|
| IR extension and verifier | 1-2 | parent M2 (IR exists) |
| `rucc-safety` insertion | 2-3 | the above |
| `rucc-safe-rt` planes and allocator | 2-3 | parent M4 (a runtime links) |
| Boundary tables and wrappers | 1-2 | the above; mostly data entry |
| Check elimination | 3-5 | parent M6 (the optimizer and its DSL) |
| Rule verification | 1-2 | parent M8 (`rucc-verify` exists) |
| Corpus, scoreboard, triage | 2-3 | continuous, not a phase |
| Kernel profile | 3-5 | parent M11 (the kernel builds at all) |

The ranges overlap because the pieces do. The two that dominate the schedule risk are check elimination (where the entire Tier E budget lives and where document 17 question 3 says the composition assumption is unvalidated) and the kernel profile, which cannot begin until the parent compiles a kernel and which contains document 17 question 1, the only genuinely unsolved problem in the specification.

The honest summary: **the first half of this is a well-understood engineering job with known techniques, and the second half contains one open research question and one unvalidated performance assumption.** Document 16 sequences them so both are answered before anything depends on them.
