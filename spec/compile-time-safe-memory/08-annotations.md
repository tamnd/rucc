# Annotations

The surface, and why every part of it is optional.

## 8.1 The three rules

**Rule 1, An annotation changes speed, never safety.** Carried over verbatim from [`../safe-memory/07.5`](../safe-memory/07-check-elimination.md). Annotating a header makes the program faster by discharging obligations statically. Removing every annotation from a program changes its performance and not the set of memory errors it reports. This is what makes adoption *monotone*: there is no point at which a partially annotated codebase is in a worse position than an unannotated one.

**Rule 2, Annotated source compiles on other toolchains.** `-fbounds-safety`'s adoptability property, and the reason it is on millions of production lines while Checked C and TrapC are not. Every annotation is either an attribute that expands to nothing under a header guard, or a comment. A file annotated for `rucc` builds under GCC and Clang, unchanged, with the same semantics.

```c
#ifndef __has_feature
#  define __counted_by(N)
#  define __sized_by(N)
#  define __ended_by(E)
#endif
```

**Rule 3, A wrong annotation is loud.** Per [`03.7.1`](03-obligations.md)'s assume-check duality: an annotation is an assumption inside a function and an *obligation* at every call site. A `__counted_by(n)` on a pointer that is really `n/2` long produces an obligation at the caller which either discharges (it was true) or becomes a check (which traps in testing). **A wrong annotation causes a false positive, never a missed error.**

Rule 3 is what licenses everything in [document 09](09-inference-and-llm.md). Machine-generated annotations are safe to accept because the worst case is a spurious trap during testing, and it is the single most important structural property in this document set.

## 8.2 What we accept: the `-fbounds-safety` set

Adopted verbatim, because the kernel and a large body of userspace already write them and [`../safe-memory/11.3`](../safe-memory/11-kernel.md)'s principle is that we invent no annotation that already exists.

| Annotation | Meaning | Refinement ([`06.4`](06-bounds-and-refinements.md)) |
|---|---|---|
| `__counted_by(n)` | extent is `n` elements | `ptr<T>{ν : ν = n}` |
| `__counted_by_or_null(n)` | as above, or null | `ptr<T>{ν : p = 0 ∨ ν = n}` |
| `__sized_by(n)` | extent is `n` **bytes** | byte-extent form |
| `__sized_by_or_null(n)` | as above, or null | none |
| `__ended_by(e)` | extent runs to pointer `e` | `ν = e - p` |
| `__terminated_by(t)` | sentinel-terminated | contents property; [`06.6`](06-bounds-and-refinements.md) |
| `__null_terminated` | `__terminated_by(0)` | none |
| `__single` | points to exactly one object | `ν = 1` |
| `__indexable` / `__bidi_indexable` | implicit wide pointer | see §8.3 |
| `__unsafe_indexable` | no bounds; do not check | a **declared exemption**, counted |

`n` and `e` may be another parameter, a struct field in the same struct, a global, or an expression over them, the same scoping rules as `-fbounds-safety`, so annotated headers port directly.

### 8.2.1 The one that is not like the others

`__unsafe_indexable` is not a hint. It is a declaration that the monitor should not check, which makes it one of [`../safe-memory/04.5`](../safe-memory/04-safety-model.md)'s three escape hatches (the *declared exemption*) and it therefore must be **counted in the trust set** and reported per [`../safe-memory/10.2`](../safe-memory/10-boundaries.md).

`--emit=safety-summary` prints the count and the list. A codebase whose annotation effort consisted of adding `__unsafe_indexable` until the warnings stopped is a codebase whose summary says so.

## 8.3 What we accept but treat differently: wide pointers

`-fbounds-safety`'s central mechanism is **implicit wide pointers on locals**: locals get bounds attached without annotation, so only ABI boundaries need annotating. That is why its adoption burden is low.

We cannot adopt the representation: [`../safe-memory/05.1`](../safe-memory/05-representation.md) chose narrow pointers with side metadata, because a fat pointer changes the ABI and the kernel is the goal. But we adopt the *idea* in its static form:

> **Every local pointer carries a refinement, inferred, with no annotation.** Layers 1, 2 and 4 already do this. `__indexable` and `__bidi_indexable` on a local are therefore no-ops for us (the information is inferred) and on a parameter or field they are read as "an extent exists, infer it", which is a weaker `__counted_by` and is accepted as such.

This means source annotated for `-fbounds-safety` compiles here with the same meaning, and the difference is invisible except in performance.

## 8.4 What we accept from the kernel

Already present, already checked by Sparse, and each one discharges something.

| Kernel annotation | What it gives us |
|---|---|
| `__counted_by(n)` | as above; already spreading through the tree |
| `__rcu` | §[5.6](05-ownership-and-lifetimes.md)'s free-free intervals, the highest-yield temporal fact in the kernel |
| `__user` | J7 / boundary; the pointer is not ours, every access must go through `copy_*_user` |
| `__iomem` | storage class `mmio`; [`../safe-memory/11.4`](../safe-memory/11-kernel.md) |
| `__percpu` | thread-locality, which §[5.2.1](05-ownership-and-lifetimes.md) needs to use free-free intervals at all |
| `__acquires` / `__releases` / `__must_hold` | lock-held facts; §[5.5](05-ownership-and-lifetimes.md)'s refcount and lock reasoning |
| `__force`, `__bitwise` | a declared cast; counted |
| `__must_check`, `__nonstring` | minor; `__nonstring` matters for [`06.6`](06-bounds-and-refinements.md) |

**The kernel needs to write nothing new to get most of what this specification offers.** That is a strong claim and it is defensible: `__rcu` and `__percpu` alone unlock the two facts (§5.6, §5.2.1) that temporal discharge in the kernel depends on, and both have been in the tree for two decades.

## 8.5 What we add

Kept as small as possible. Four, and each has a stated reason for not being derivable.

**`__nofree`** on a function declaration. The single bit of [`04.6`](04-the-discharge-ladder.md), stated where inference cannot reach it: across a shared-library boundary, or through a function pointer type. Given [`../safe-memory/08.8`](../safe-memory/08-temporal-safety.md)'s 5%-vs-40%, this is the highest-value addition in the list by a wide margin. Verified at the definition when the definition is visible; a trust-set entry when it is not.

**`__arena` / `__arena_alloc(A)` / `__arena_destroy(A)`** on a type and a function triple. §[5.4](05-ownership-and-lifetimes.md)'s recognizer, stated rather than pattern-matched, because arena APIs vary too much to recognize reliably and getting it wrong is a soundness bug rather than a missed proof.

**`__proof(level)`** on a function. Per-function override of `-fsafety-proof`, so a hot loop gets `deep` in a `default` build and a pathological function gets `off` rather than blowing the compile-time budget for the file. Also how `verify` is requested per [`07.4`](07-separation-logic.md).

**The CN-style comment specifications** of [`07.3`](07-separation-logic.md). Comments, so rule 2 holds trivially.

**Not added:** an ownership annotation. `__owned`/`__borrowed` is the obvious fifth entry, it is what Checked C and the [SEI Pointer Ownership Model](01-research-2026.md) reach for, and it is omitted because §[5.7](05-ownership-and-lifetimes.md) predicts the analysis that would consume it gets cut. If layer 5 survives V4's measurement, this is the first thing to add.

## 8.6 The effects table is an annotation surface

The declarative table of [`../safe-memory/10.3`](../safe-memory/10-boundaries.md), whose rows look like this:

```
memcpy(void * __sized_by(n) dst, const void * __sized_by(n) src, size_t n)
    writes(dst, n) reads(src, n) types(dst := src)
```

is read by the prover as well as by the wrapper generator. `reads(src, n)` gives `O.init` for the source; `writes(dst, n)` establishes initializedness of the destination for everything after; `types(dst := src)` discharges downstream `O.type`.

This means **the effects table is where third-party library annotation lives**, without touching the library's headers. A project can ship a `rucc-effects.toml` describing zlib's API and get discharge across the boundary, which is a far lower-friction adoption path than patching headers, and per [`../safe-memory/10.2`](../safe-memory/10-boundaries.md), every entry is a counted trust-set entry until the library is itself instrumented or proved.

## 8.7 Suggestion mode

`-fsafety-suggest-annotations` (already in [`../safe-memory/15.4`](../safe-memory/15-integration.md)) emits, for each undischarged obligation whose failure is attributable to a missing extent at a boundary, the annotation that would discharge it:

```
foo.c:88: note: 4102 checks in this file would be discharged by
          annotating: void parse(char * __counted_by(len) buf, size_t len)
          confidence: high (all 7 call sites in this build pass an extent
          equal to `len`)
```

Three properties make this the most useful diagnostic in the specification. It is **ranked by check count**, so the first suggestion is the one that pays most. It reports **confidence based on call-site evidence**, which is a real inference and not a guess. And it is **the input format for [document 09](09-inference-and-llm.md)**: a model asked to annotate a codebase should be given this list rather than the raw source.

It is also the only diagnostic mode that exists, and it is off by default, per the no-alarms rule.

## 8.8 Placement, inheritance, and the boring rules

Stated because ambiguity here becomes bug reports.

- Annotations attach to declarations. A definition and its declaration must agree; a mismatch is an error (this is a *type* error, not a proof failure, so it is reported).
- A parameter annotation is in scope for the whole function and for all callers.
- A field annotation is in scope wherever the struct type is complete.
- Annotations are **not** inherited through `typedef` of a pointer type, a typedef'd pointer is unannotated unless the typedef itself carries the attribute, which it may.
- Annotations on a function-pointer *type* apply at every indirect call through it, generating obligations at each. This is how callbacks get any static treatment at all.
- `__counted_by(n)` where `n` is not visible at the annotation's scope is an error at the declaration, not a silent no-op.

## 8.9 Reporting

`--emit=safety-summary` includes:

```toml
[annotations]
counted_by       = 412
sized_by         = 88
unsafe_indexable = 6      # trust-set entries; see 8.2.1
nofree_declared  = 23     # of which unverified (no definition visible): 9
effects_entries  = 141    # of which unverified: 141
obligations_discharged_via_annotation = 21044
```

The last line is the one that answers "was annotating this header worth it," and it is the reason the summary tracks discharge *by source* and not only by layer.

## 8.10 Why not a dialect

The alternative design is Checked C's or TrapC's: new pointer types, a checked region, a language mode. It gives stronger guarantees per annotated line and it is why both projects can claim spatial safety by construction.

It is rejected for one reason, and it is empirical rather than aesthetic. **`-fbounds-safety` is deployed on millions of lines of production C and Checked C and TrapC are not**, and the difference between them is not the quality of the design, it is that one asks for attributes that expand to nothing elsewhere and the others ask a project to change languages. A project cannot change languages incrementally, cannot ship a partially-converted library to consumers on other toolchains, and cannot justify the conversion before seeing the benefit.

The corollary, which is uncomfortable and correct: **this specification's guarantees on unannotated code must be good enough to be worth adopting with zero annotations**, because that is the state every codebase starts in and most will stay in. The monitor provides those guarantees; this document set only makes them cheaper. Annotations are the accelerant, never the mechanism.
