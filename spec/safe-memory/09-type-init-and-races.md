# Types, initialization, sub-objects and pointer races

Documents 07 and 08 handle the two planes everybody agrees are necessary. This document handles the three that are contested: the type plane, the init plane, and the epoch plane. Each is a place where an existing tool exists and is not deployed, and in each case the reason it is not deployed is the same (the cost is high and the false-positive rate is worse) so each section here has to say what we do differently.

## 9.1 The type plane

**What it is for.** Four of document 03's eight type classes (Y1, Y2, Y3, Y5) and one spatial class (S4). Y1 in particular (reading a non-pointer word as a pointer) is the single class with the largest exploitability consequence, because a forged pointer is the step that turns a heap overflow into arbitrary code execution, and it is what InvisiCaps buys Fil-C.

**The good news is that Y1 is free.** Document 05 section 5.2.2: an aux slot whose payload is not a pointer has `ver = 0`, which is `⊥`, and the first access through the loaded pointer fails. No type plane is consulted, no extra load happens, and this is the Tier E configuration, Tier E has "pointer-slot only" in document 04's plane table for exactly this reason. Y1, Y4, Y5 and Y7 come from the aux plane; only Y2, Y3, Y6 and S4 need the type plane proper.

**The representation.** Per document 05, a per-16-byte-granule `(homogeneous flag, TypeId)` with a per-byte side table for heterogeneous granules. `TypeId` is the parent's interned type universe from document 07, so the plane's vocabulary is exactly the compiler's, and a report can name the types in their source spelling.

Three distinguished values:

```
no-type          never stored, or stored from an untyped source
character        stored through a character type; compatible with everything
pointer-slot(k)  byte k of a pointer-shaped word
```

`character` is what makes the byte-wise-copy idiom work. C 6.5 says an object's effective type is set by a store through a non-character lvalue and that a character-type access is always permitted; the plane implements exactly that, so `char *p = (char*)&s; p[3] = 0;` sets byte 3 to `character` rather than to `char`, and a subsequent read of `s.f` still sees the field's type over the other bytes and `character` over one. `compatible()` treats `character` as compatible with any access, so the read passes and no false positive occurs. This is 6.5's rule, not an exception to it, which is document 03's line and it matters: **every entry in the false-positive table is either in the model or explicitly out of the checked set, never a special case bolted on.**

**The `memcpy` rule.** C says a copy through `memcpy` or through a character array carries the source's effective type. `meta_type` on a `memcpy` copies the source's plane range to the destination's, which makes the punning idiom work and simultaneously makes it *checked*: copying a `struct A` over a `struct B` and then reading it as a `struct B` is caught, because the plane says the bytes are `A`.

**When the plane is wrong.** Uninstrumented code writes memory without updating the plane. The result is a byte whose plane says `no-type` and whose contents are meaningful. `compatible(ty, no-type)` is **true** (an untyped byte takes the type of the first access) which is the only choice that does not produce false positives at every boundary, and which is also what C says, since storage with no declared type takes its effective type from the store. The consequence is that the type plane's coverage degrades gracefully at boundaries rather than failing loudly, which is right for a detection tool and is one of the places document 02's boundary limit bites.

**Y2 and the kernel.** Linux builds with `-fno-strict-aliasing` and always has. Under that flag Y2 and Y3 are *off*, because the program has declared that it does not obey the effective-type rules. Tier K therefore carries Y1, Y4, Y5, S4 and the init plane from this document, and not Y2/Y3. This is not a limitation to apologize for: checking a rule the program has explicitly opted out of would produce nothing but false positives. It does mean the "✓" in document 03's Y2 K-column is conditional on `-fstrict-aliasing`, and document 03 should be read with that in mind.

## 9.2 The init plane

MSan's problem, and MSan's deployment story is the cautionary tale: it requires *every* linked library to be instrumented, including libc++, because an uninstrumented write leaves the plane saying uninitialized and the next read is a false positive. That requirement is why MSan is used on fuzzers and not on production builds.

**We invert the failure mode.** MSan tracks uninitialized-ness and propagates it, so a gap in instrumentation produces a false positive. We track initialized-ness with the rule that **a byte the monitor did not observe being written is treated as initialized**, so a gap in instrumentation produces a false *negative*. That is the correct trade for a tool whose false-positive rate is a release-blocking property, and it is the concrete meaning of document 02's boundary limit for this class: we under-report near uninstrumented code rather than over-reporting.

**Granularity.** One bit per byte, 1:8 shadow, 512 bytes per 4 KiB page. Cheap in memory; the cost is the maintenance traffic, since every store sets bits. Document 07 section 7.6's plane-write coalescing is what makes it affordable, because a loop that fills an array sets one range rather than n bits.

**Setting.** `meta_init` on every store, over the store's width. A `calloc`, a `memset`, and an allocator that zeroes set the whole range in one operation. A struct assignment sets the whole struct.

### 9.2.1 The relationship to the parent's no-poison model

The parent's document 07 section 7.7 row nine decides that an uninitialized read produces an unspecified but *stable* value rather than LLVM's `poison`. Document 04 section 4.7 already says this is load-bearing; here is why concretely.

Under a poison model, `int x; if (x) f(); if (x) g();` may call `f` and not `g`, because each read of a poison value may independently produce anything and the optimizer exploits that. A monitor cannot report "the program read uninitialized memory at this point and then did *this*", because what it did is not defined by the language. Under the stable-value model the read yields a value, the program proceeds deterministically, and the report says what happened and what followed. `-fsafety-on-error=continue` in document 06 section 6.5 depends on this: continuing after an init violation is only meaningful if continuing is defined.

The parent's `-fstrict-init` opts into the aggressive model, and under it Y6 becomes an *enforcement* check rather than a diagnostic (the read traps rather than yielding a value) which is a coherent combination and is the one Tier E would use if Y6 were in Tier E, which it is not.

## 9.3 Padding

Document 03's false-positive table promises this section. Reading padding bytes is completely ordinary (`memcmp` of two structs, hashing a struct, writing a struct to a file) and a naive init plane reports every one of them, which would be a torrent of false positives on real code.

**The rule: a store that writes an object as a whole initializes the object as a whole, including its padding.** A struct assignment, a `memcpy` into a struct, a `calloc`, a `memset`, and an initializer with `= {0}` all set the plane over `[lo, lo+sizeof)`, not over the union of the members. A member-by-member fill does not, and the padding stays uninitialized.

That is the correct rule and it still leaves the common case (fill the members individually, then `memcmp` or `write()` the struct) reporting. Two things save it. First, that case *is a bug*: it is CWE-200 and it is exactly the kernel infoleak that KMSAN was built to find, so reporting it is the point. Second, at Tier E the general init plane is off and only pointer slots are tracked (Y7 not Y6), so production builds never see it.

For Tier D on userspace corpora, `-fsafety-init=padding|nopadding` selects whether padding participates, defaulting to `nopadding` for library code and `padding` for the kernel profile, where the infoleak is the thing being hunted. That default split is stated here so document 12's scoreboard reports the two configurations separately rather than mixing them.

## 9.4 Sub-object bounds: `-fsafety-subobject`

Document 03's S4, the class Fil-C, CHERI-by-default and MTE all miss. Overflowing one member into the next within the same allocation is invisible to any per-allocation metadata scheme, and it is a real and exploited class, the ACSAC study's out-of-bounds-write category includes it.

**Why it is a tier and not a default.** `container_of` is the reason. The Linux kernel is built on deriving a pointer to an enclosing structure from a pointer to a member, and so are intrusive lists in every C codebase that has them. Under sub-object bounds, `container_of` is out of bounds by construction. CHERI's sub-object mode has exactly this problem and it is why CHERI does not enable it by default.

**The mechanism, and why it is the type plane rather than narrowed capabilities.** Both CHERI's sub-object mode and `-fbounds-safety`'s `__bidi_indexable` narrow the *pointer*: the capability for `&s.f` has `s.f`'s bounds. That is stronger and it is what breaks `container_of` irreparably, because the wider bounds are gone and cannot be recovered.

We keep the capability at allocation granularity and put the sub-object check in the type plane instead. `check_type %c, %p, size, !tbaa` at an access to `s.f` asks whether the bytes at `p` have `f`'s type; an overflow from `f` into `g` reads bytes whose plane says `g`, and the access's TBAA node says `f`, and that is the violation. The capability is untouched, so `container_of` still has the whole object's bounds and still works.

This is a genuinely better decomposition than the capability-narrowing designs and it falls out of having a type plane for other reasons. It is also weaker in one specific way: two adjacent members of the *same* type are indistinguishable to it, so `struct { int a; int b; }` with an overflow from `a` into `b` is not caught. `-fsafety-subobject=strict` adds a per-member instance id to the plane's heterogeneous side table and catches it, at the cost of the side table being used far more often. The default is the type-compatibility form.

**`container_of` explicitly.** Per document 03's table, the widening (subtract a constant `offsetof`, land at a base whose plane type matches the named struct) is recognized as legitimate and permitted. It is a rewrite rule in the `safety/` namespace, SMT-verified like the rest, and the verification obligation is that the recognized pattern produces a capability no wider than the one the source pointer already carried, which is trivially true since the capability is unchanged. The check is that the *result* is used at a type the plane agrees with.

**Flexible array members** and the `T x[1]` idiom get the treatment document 03's table promises: a trailing array is unbounded within the allocation, so S4 does not fire on it, and S1 still does at the allocation edge.

## 9.5 Pointer races: the epoch plane

Document 03's C1 through C4, and the section where we do something no existing tool does, cheaply, because the metadata is already being loaded.

**What Fil-C accepts.** Fil-C's documentation states that a non-atomic store of a pointer can tear: one thread's pointer value can be paired with another thread's capability, and the result is memory-safe because the capability is a real capability with real bounds. That is true and it is a reasonable engineering decision. It is also a silent wrong answer produced by a race (the program follows a pointer to an object it never had a pointer to) and it is a bug the program's author would want to know about.

**The mechanism.** Every pointer-shaped aux slot carries, alongside its `ver` and compressed bounds, an epoch: a `(thread_id, clock)` pair, 64 bits, in the plane described in document 05. On a `cap_store`, the storing thread writes its own `(tid, clock++)`. On a `cap_load`, the loading thread reads the epoch along with the capability (same cache line, no extra miss) and:

- **C1, torn store:** the pointer word and the aux slot are read; if the aux's `ver` does not match the lifetime plane's version for the pointer's target, or the aux's epoch is *newer than the pointer word's own epoch*, the pair is inconsistent and a torn store is reported. This requires the pointer word to carry an epoch too, which it does: the aux slot is 16 bytes and holds both the capability's epoch and a copy of the sequence number at which the paired pointer word was written. A mismatch means the two halves came from different stores.
- **C3, metadata race:** two unordered writes to the same aux slot from different threads, detected by the same comparison from the other side, a store that finds an epoch from another thread newer than its own last synchronization point.
- **C4, race-induced use-after-free:** T1 with an epoch witness, so the report names the freeing thread and the accessing thread.
- **C2, general pointer-word race:** the same detection, reported rather than only used internally.

**What this is and is not.** It is a **happens-before-free, no-false-positive, incomplete** detector. It has no vector clocks; it does not reconstruct the happens-before relation; it cannot tell you that two accesses *could* have raced on a different schedule. It reports the races it actually observes in the interleaving that actually happened. That is strictly weaker than ThreadSanitizer and it costs nearly nothing, where TSan costs 5-15x and 5-10x memory, which is why TSan is a testing tool and this can be on in production.

The synchronization edges needed to know "unordered" come from the atomics and the lock primitives, interposed at the boundary per document 10. A thread's clock advances on every metadata store; acquiring a lock takes the max with the lock's released clock. That is a Lamport clock, not a vector clock: it gives us "this was concurrent with something" soundly enough to avoid false positives, and misses orderings a vector clock would establish, which only costs recall.

**Cost.** One extra 8-byte field read from a line already being read, and one extra compare on the `cap_load` path. Predicted under 3%; document 13 measures it. Memory is the epoch plane at 8:1, which is in document 05's table and is only present when the plane is enabled.

`-fsafety-races=off|pointer|metadata` selects. Tier E carries `metadata` (C1, C3, C4) and not `pointer` (C2).

## 9.6 `restrict`

Document 03's Y8, the parent's promised `-fsanitize=restrict`, and the one judgement document 04 section 4.6 explicitly refuses to put in J1, because it is not a property of a single access.

**The contract.** C 6.7.3.1: if an object accessible through a `restrict`-qualified pointer `P` in a block is modified by any means, then all accesses to it in that block must be through `P`. It is a promise about a *scope*, checkable only by comparing accesses within the scope.

**The mechanism.** For each block that declares `restrict` pointers, the monitor maintains, for the block's dynamic extent, a small map from storage instance to the `restrict` pointer through which it was accessed. An access through a different `restrict` pointer, or a modification reached other than through the recorded one, violates the contract.

The map is small (a block rarely has more than a handful of `restrict` parameters, and `memcpy`, `strcpy` and the numeric kernels that use `restrict` have two) so it is a linear scan over four entries in a stack slot, not a hash table.

**Why this is worth having.** `restrict` is a promise the optimizer *acts on*: the parent's document 09 alias analysis uses the `noalias` attribute from document 08 to reorder and vectorize. A violated `restrict` is therefore not merely UB in principle, it is a miscompilation in practice, and it is one of the hardest bugs in existence to diagnose because the symptom appears only at `-O2` and only after vectorization. A checker for it is a genuinely novel and useful tool; no shipping compiler has one. The parent already promised it and this is the specification.

**Cost and tiering.** Tier D and Tier K, off in Tier E. The per-access cost is a scan of the block's restrict map, which is only paid inside blocks that declare `restrict` pointers, so it is zero for the overwhelming majority of code and non-trivial inside exactly the hot numeric kernels where `restrict` appears. That is a bad distribution and it is the reason this is a Tier D check: `-fsanitize=restrict` is a thing you run over a test suite, not a thing you ship.

## 9.7 Effective types, function pointers and the small classes

**Y4, function pointer called with the wrong type.** The capability's `class` field is `function` and the aux slot carries the interned signature `TypeId`. An indirect call compares the call site's signature against it. This is Clang's `-fsanitize=function` and CFI's forward-edge check, obtained here for free from metadata that already exists, and it is the check that turns a corrupted vtable-equivalent into a report rather than a jump.

**Y5, data called as a function or a function read as data.** The `class` field again: `class = function` is not readable, `class ≠ function` is not callable, and the `perm` bits already carry it. This falls out of J1's permission conjunct with no extra machinery.

**Y3, union member confusion.** The type plane records the member last stored. A read of a different member is a violation *unless* it falls under 6.5.2.3's common-initial-sequence rule or the union's address has been taken, both of which the model encodes. Off under `-fno-strict-aliasing` with Y2.

## 9.8 What this document costs

Summarizing for document 13, since these are the planes whose cost is least certain:

| Plane | Time (predicted) | Memory | Tiers |
|---|---|---|---|
| Type (Y1, Y4, Y5 via aux) | ~0 | 0, aux already exists | D, E, K |
| Type (Y2, Y3, S4 via plane) | 15-25% | 25-30% with granule compression, 400% without | D, K (S4 opt) |
| Init, pointer-slot (Y7) | ~0 | 0, aux already exists | D, E, K |
| Init, byte-granular (Y6) | 10-20% | 12.5% | D, K |
| Epoch (C1, C3, C4) | <3% | 0 when folded into aux | D, E, K |
| Epoch (C2) | <5% | 8:1 plane | D, K |
| `restrict` (Y8) | 0 outside restrict blocks | 0 | D, K |

The dominant uncertainty is the type plane's memory, which is document 17 question 6 and which document 02's Tier D 2x budget rests on. If granule-homogeneity compression does not work, the type plane moves behind its own flag and Tier D's byte-granular type checking becomes a Tier D-strict option rather than the default. That contingency is written down now so it is a planned degradation rather than a crisis.
