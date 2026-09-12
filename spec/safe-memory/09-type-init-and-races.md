# Types, initialization, sub-objects and pointer races

Documents 07 and 08 handle the two planes everybody agrees are necessary. This document handles the three that are contested: the type plane, the init plane, and the epoch plane. Each is a place where an existing tool exists and is not deployed, and in each case the reason it is not deployed is the same (the cost is high and the false-positive rate is worse) so each section here has to say what we do differently.

## 9.1 The type plane

**What it is for.** Four of document 03's eight type classes (Y1, Y2, Y3, Y5) and one spatial class (S4). Y1 in particular (reading a non-pointer word as a pointer) is the single class with the largest exploitability consequence, because a forged pointer is the step that turns a heap overflow into arbitrary code execution, and it is what InvisiCaps buys Fil-C.

**The good news is that Y1 is free.** Document 05 section 5.2.2: an aux slot whose payload is not a pointer has `ver = 0`, which is `⊥`, and the first access through the loaded pointer fails. No type plane is consulted, no extra load happens, and this is the Tier E configuration, Tier E has "pointer-slot only" in document 04's plane table for exactly this reason. Y1, Y4, Y5 and Y7 come from the aux plane; only Y2, Y3, Y6 and S4 need the type plane proper.

**The representation.** Per document 05, one `(homogeneous flag, TypeId)` per 8-byte granule with a per-byte side table for heterogeneous granules. The granule is 8 and not 16 because document 05.2.5 measured it, and 16 costs more than twice the budget on SQLite. `TypeId` is the parent's interned type universe from document 07, so the plane's vocabulary is exactly the compiler's, and a report can name the types in their source spelling.

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

**The mechanism.** Two stamps, both of them in the epoch plane of document 05 and neither of them in the aux slot. An earlier draft of this section put the epoch in the slot beside the `ver` and the compressed bounds, and the slot's format was decided after that was written and is full to the bit, so the epoch a capability was written at lives at the slot's own address in the plane and the epoch the pointer word beside it was written at lives at the word's address. What makes that free is that the plane is reserved and biased over a whole watched region rather than over the payloads in it, and a block is the aux, then the header, then the payload, all three inside the region. So the aux already has a stamp of its own, it is not the payload word's stamp, and nothing new has to be mapped. `rucc_safe_rt::alloc` has a test on that geometry, because a later change to the reservation could take it away without anybody noticing until C1 tried to use it. On a `cap_store` the storing thread writes its own `(tid, clock++)` at both addresses. On a `cap_load` the loading thread reads both back and:

- **C1, torn store:** the pointer word and the aux slot are read; if the aux's `ver` does not match the lifetime plane's version for the pointer's target, or the two stamps the plane holds for them are not the same stamp, the pair is inconsistent and a torn store is reported. Not the same rather than one being newer than the other, because a store writes both stamps from one thread with one clock value, so any difference at all means the two halves came from different stores and there is nothing for an ordering between them to say.
- **C3, metadata race:** two unordered writes to the same aux slot from different threads, detected by the same comparison from the other side, a store that finds an epoch from another thread newer than its own last synchronization point.
- **C4, race-induced use-after-free:** T1 with an epoch witness, so the report names the freeing thread and the accessing thread. The witness comes from the plane rather than from a check of its own: ending an instance stamps its granules with the freeing thread and the step it stood at, and the lifetime refusal reads that stamp back and compares it against the accessing thread the same way C2 and C3 compare a write. So C4 costs nothing at an access that is not already being refused, and the class is a line on a report rather than an extra load. The pointer-word check steps aside for storage nobody owns, or a use after free would be reported twice under two judgements.
- **C2, general pointer-word race:** the same detection, reported rather than only used internally.

**Where the two writes are made.** Beside the `cap_store` rather than folded into it, and the reading side beside the `cap_load` in the same way. They are `rucc_safe_rt::cap::pair` and `rucc_safe_rt::cap::torn`, and the reason they are separate calls is that the aux plane and race detection are two flags. The aux plane is what makes a capability survive a trip through memory and `-fsafety-races` is what asks about threads, and a program that wanted the first should not pay a plane write per pointer store for the second. A store that takes the pair takes it in place of the plane's ordinary epoch write and not as well as it, since a second stamp would move the word's half away from the slot's and read as a tear on the store that wrote both.

**Where the stamps are not, and why.** Two other homes for the second stamp were considered, and both of them cost more than a write does. Halving `ver` to 32 bits frees the room in the slot and gives up the temporal check, because a 32-bit version repeats after four billion allocations, which is an hour of a busy allocator, and a repeat is a use after free that the checker calls live. Truncating the stamp and paying for it out of the displacement and the extent lowers the threshold for exact bounds from 2 MiB to 64 KiB at 16 bits each, and makes C1 probabilistic on top of that. What settles it is not the byte count but who pays: both of those make the default posture worse for every program there is, and the second plane write is paid only by the programs that asked for race detection.

**What this is and is not.** It is a **happens-before-free, no-false-positive, incomplete** detector. It has no vector clocks; it does not reconstruct the happens-before relation; it cannot tell you that two accesses *could* have raced on a different schedule. It reports the races it actually observes in the interleaving that actually happened. That is strictly weaker than ThreadSanitizer and it costs nearly nothing, where TSan costs 5-15x and 5-10x memory, which is why TSan is a testing tool and this can be on in production.

The synchronization edges needed to know "unordered" come from the atomics and the lock primitives, interposed at the boundary per document 10. A thread's clock advances on every metadata store; acquiring a lock takes the max with the lock's released clock. That is a Lamport clock, not a vector clock: it gives us "this was concurrent with something" soundly enough to avoid false positives, and misses orderings a vector clock would establish, which only costs recall.

**Cost.** Two 8-byte plane reads and a compare on the `cap_load` path, and two 8-byte plane writes on the `cap_store` path where there was one. The two addresses are rarely in one line, because the plane is one to one with the heap at 8:1 and the aux for the first word of a 32-byte node sits 96 bytes below the payload, which the plane keeps 96 bytes apart in turn. The geometry gives bounds rather than a number: a pointer store that touched 32 bytes touches 40, a quarter more, and a program that chases pointers rather than sweeping them goes from three lines to four. Where it falls between those is the access pattern, and document 13.5's aux plane simulation is the shape that measurement would take. It is a sizing exercise and not a gate in front of C1, because C1 exists only under `-fsafety-races` and so the bytes are paid by programs that asked for race detection and by nobody else. Predicted under 3% at the load, which document 13 measures. Memory is unchanged: the plane is 8:1 over the region either way, it is in document 05's table, and it is only present when it is enabled.

`-fsafety-races=off|pointer|metadata` selects. Tier E carries `metadata` (C1, C3, C4) and not `pointer` (C2).

## 9.6 `restrict`

Document 03's Y8, the parent's promised `-fsanitize=restrict`, and the one judgement document 04 section 4.6 explicitly refuses to put in J1, because it is not a property of a single access.

**The contract.** C 6.7.3.1: if an object accessible through a `restrict`-qualified pointer `P` in a block is modified by any means, then all accesses to it in that block must be through `P`. It is a promise about a *scope*, checkable only by comparing accesses within the scope.

**The mechanism.** For each block that declares `restrict` pointers, the monitor maintains, for the block's dynamic extent, one entry per pointer holding the range of addresses reached through it and whether any of those accesses wrote. An access whose range overlaps another entry's, where at least one of the two wrote, violates the contract. It is J8 of document 04 section 4.4.

Ranges of addresses rather than a map from storage instance, which is what an earlier draft of this section said. The addresses answer the question directly, they answer it for automatic and static storage as well as for allocated storage, where there is no instance to look up, and a check then touches the block's own stack slot and nothing else.

The entries are per pointer, so the scan is over four of them in a stack slot, not a hash table. A block rarely has more than a handful of `restrict` parameters, and `memcpy`, `strcpy` and the numeric kernels that use `restrict` have two.

**What the claim is.** The range kept per pointer is the union of everything reached through it, so two pointers that stride through one array without ever landing on the same byte are reported, and the standard read strictly does not make that a violation because the elements are different objects. That is the intended answer rather than an imprecision to apologise for. This check exists because a violated `restrict` is a miscompilation, and what the optimizer acts on, through document 08 section 8.2 layer 5, is that the ranges are disjoint. A program the union rule reports is a program the optimizer is entitled to break.

In the other direction the check is incomplete, and deliberately so. An access the front end could not trace back to a declaration carries no base, and no base means the names did not say where the pointer came from rather than that it came from nowhere. Such an access is passed. So J8 reports only programs that are broken and does not report every one of them, which is the direction document 02 says outranks the other.

**Where it is numbered.** The clique and the base an access carries are `rucc_ir::Restrict`, worked out by `crates/rucc-lower/src/restrict.rs` from the names the access was written with, and they are the same two numbers document 08 section 8.2 layer 5 reads. One clique is one scope and one base is one pointer of it, so the monitor's scope and the optimizer's disjointness query are reading the same fact, which is what makes the two agree by construction rather than by inspection.

**Why this is worth having.** `restrict` is a promise the optimizer *acts on*: the parent's document 09 alias analysis uses the `noalias` attribute from document 08 to reorder and vectorize. A violated `restrict` is therefore not merely UB in principle, it is a miscompilation in practice, and it is one of the hardest bugs in existence to diagnose because the symptom appears only at `-O2` and only after vectorization. A checker for it is a genuinely novel and useful tool; no shipping compiler has one. The parent already promised it and this is the specification.

**How it is emitted.** Four instructions, which document 06 section 6.2.2 spells out and `rucc_safety::promise` puts in. A block that declares `restrict` pointers reserves its scope with an `alloca` in the entry block and opens it with `restrict_enter`, every access through one of the block's pointers is preceded by `check_restrict_read` or `check_restrict_write` carrying the clique and the base, and `restrict_leave` closes the scope before every path that returns. Those four are the only instructions in the safety namespace that write memory, because a check here records the range it was asked about, which is what makes it the one check the optimizer may not hoist out of a loop or fold with an identical one.

**Cost and tiering.** A flag of its own, `-fsafety-restrict`, off by default at every tier. The per-access cost is a scan of the block's restrict map, which is only paid inside blocks that declare `restrict` pointers, so it is zero for the overwhelming majority of code and non-trivial inside exactly the hot numeric kernels where `restrict` appears. That is a bad distribution rather than a large cost, and together with the union rule above, which reports programs the standard permits, it makes this a thing you run over a test suite rather than a thing you ship. Tying it to a tier would make that the compiler's decision when it is the build's.

## 9.7 Effective types, function pointers and the small classes

**Y4, function pointer called with the wrong type.** The capability's `class` field is `function` and the aux slot carries the interned signature `TypeId`. An indirect call compares the call site's signature against it. This is Clang's `-fsanitize=function` and CFI's forward-edge check, obtained here for free from metadata that already exists, and it is the check that turns a corrupted vtable-equivalent into a report rather than a jump.

**Y5, data called as a function or a function read as data.** The `class` field again: `class = function` is not readable, `class ≠ function` is not callable, and the `perm` bits already carry it. This falls out of J1's permission conjunct with no extra machinery.

**Y3, union member confusion.** The type plane records the member last stored. A read of a different member is a violation *unless* it falls under 6.5.2.3's common-initial-sequence rule or the union's address has been taken, both of which the model encodes. Off under `-fno-strict-aliasing` with Y2.

## 9.8 What this document costs

Summarizing for document 13, since these are the planes whose cost is least certain:

| Plane | Time (predicted) | Memory | Tiers |
|---|---|---|---|
| Type (Y1, Y4, Y5 via aux) | ~0 | 0, aux already exists | D, E, K |
| Type (Y2, Y3, S4 via plane) | 15-25% | 100% at an 8-byte granule, 400% uncompressed | D, K (S4 opt) |
| Init, pointer-slot (Y7) | ~0 | 0, aux already exists | D, E, K |
| Init, byte-granular (Y6) | 10-20% | 12.5% | D, K |
| Epoch (C1, C3, C4) | <3% | 0 when folded into aux | D, E, K |
| Epoch (C2) | <5% | 8:1 plane | D, K |
| `restrict` (Y8) | 0 outside restrict blocks | 0 | D, K |

The type plane's memory was the dominant uncertainty and is now measured, in document 05.2.5, and the compression works at an 8-byte granule. What is left of question 6 is that the measurement is static and so is a pessimistic bound on a run-time heap. If a later corpus member moves the curve, the type plane moves behind its own flag and Tier D's byte-granular type checking becomes a Tier D-strict option rather than the default. That contingency stays written down so it is a planned degradation rather than a crisis.
