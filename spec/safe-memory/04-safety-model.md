# The safety model

Document 03 lists what is caught. This document says what "caught" means, precisely enough that the soundness claim in document 02 has content and that document 14 can verify it.

The model is an extension of the one the parent's [document 07](../07-types-and-semantics.md) already commits to. Nothing here contradicts it; everything here makes it observable at run time.

## 4.1 The definition of memory safety we are using

Listing prohibited events is how memory safety is usually defined and it is why every such list has been incomplete. We take the positive definition from [Hansen, Larsen and Askarov, PLDI 2026](https://arxiv.org/abs/2507.11282):

> **Gradual allocator independence.** A program is memory-safe when the allocator does not influence its observable behavior, except through two sanctioned downgrading events: exhaustion of memory, and pointer-to-integer casts.

This is noninterference with the allocator as the secret. It is the right definition for three reasons. It explains *why* the list in document 03 is the list it is: every entry is a way for allocator state (which address a `malloc` returned, whether an address was recycled, what was in memory before) to leak into the program's behavior. It names the two exceptions honestly, and both are exceptions we have to implement anyway: OOM is observable and pointer-to-integer casts are precisely PNVI-ae-udi's exposed-address rule. And it gives a criterion for whether a *new* check belongs in the model, which a list does not.

The monitor in this specification is an approximation of that property. It is sound in the direction that matters: **every report is a genuine violation**, and the set of violations it can miss is exactly the set enumerated in documents 02 and 03.

## 4.2 Storage instances, provenance and capabilities

From N3005 and the parent's document 07, restated in the form the monitor uses.

A **storage instance** is created when an object begins its lifetime or an allocation succeeds. Each carries an identifier `I` unique for the whole execution. Addresses are reused; identifiers never are. A storage instance has:

```
lo      : address       lower bound, inclusive
hi      : address       upper bound, exclusive
ver     : u64           lifetime version, unique per instance
class   : StorageClass  { static, automatic, allocated, mapped, mmio, device, function, literal }
perm    : Perm          { R, W, X } bitset
state   : { live, ended, quarantined, device_owned }
```

A **capability** is the run-time reification of provenance: the tuple `(I, lo, hi, ver, class, perm, state)`, or the distinguished value `⊥` meaning "no provenance." Every pointer value the program can produce has a capability, possibly `⊥`.

An **access** is a tuple `(p, addr, n, align, ty, kind)`: a pointer value `p` with its capability, the address, the width in bytes, the required alignment, the access's effective type, and read/write/execute.

## 4.3 The five planes

The model is one map from address ranges to facts, but it is convenient to name the five projections, because each answers a different question in document 03's matrix and each has a different cost.

**The bounds plane** answers "does this access lie inside its provenance." It is the capability itself and is per-pointer, not per-byte.

**The lifetime plane** answers "is this provenance still the one that owns this address." It is a per-address-range current version, and the check is `cap.ver == plane.ver(addr)`. This is the lock-and-key mechanism and it is document 08.

**The type plane** answers "is the effective type of these bytes compatible with this access." It is per-byte, mapping each byte to a type identifier from the parent's interned `TypeId` universe plus the distinguished values `no-type` (never stored, or stored by `memcpy` from an untyped source) and `pointer-slot(k)` (byte `k` of a pointer). Document 09.

**The init plane** answers "has this byte ever been stored." One bit per byte in the general case; one bit per pointer-shaped word in the Tier E subset. Document 09.

**The epoch plane** answers "who wrote this word last, and were they ordered with respect to me." Per pointer-shaped word: a `(thread, clock)` pair, updated on metadata store. Document 09 section 9.5.

The reason to name them is that a tier is a *choice of planes*, not a choice of checks:

| Tier | bounds | lifetime | type | init | epoch |
|---|---|---|---|---|---|
| D | ✓ | ✓ | ✓ byte | ✓ byte | ✓ |
| E | ✓ | ✓ | pointer-slot only | pointer-slot only | ✓ (metadata only) |
| K | ✓ | ✓ | ✓ byte | ✓ byte | ✓ |

## 4.4 The judgements

The monitor is defined by seven judgements. Each is a predicate the implementation must decide, and each corresponds to one or more rows of document 03. Where an implementation may not be able to decide a judgement (because the memory is outside its view) the operation is refused or the region is declared, per document 10; it is never silently permitted.

**J1, Access.** `access(p, addr, n, align, ty, kind)` is permitted iff

```
cap(p) ≠ ⊥
∧ cap(p).state = live
∧ cap(p).lo ≤ addr  ∧  addr + n ≤ cap(p).hi
∧ cap(p).ver = lifetime_plane(addr)
∧ kind ∈ cap(p).perm
∧ addr mod align = 0
∧ compatible(ty, type_plane(addr .. addr+n))                    [type plane]
∧ (kind = read ⇒ initialized(addr .. addr+n))                    [init plane]
```

The bounds conjunct is written `addr + n ≤ hi` in the model and lowered by document 06 into one of the three overflow-safe forms Fil-C documents, because `addr + n` can wrap.

**J2, Derive.** `p2 = p1 + k` produces a pointer whose capability is `cap(p1)`, and is permitted iff the result lies in `[lo, hi]` inclusive of the upper bound, which is C's one-past-the-end rule. A derivation outside that range is a violation (S5) at the point of derivation, and the resulting pointer's capability is `⊥` so that a suppressed derivation cannot be laundered into a permitted access.

**J3, Expose and synthesize.** A cast from pointer to integer marks `cap(p).I` **exposed**. A cast from integer `x` to pointer yields the capability of the unique exposed live instance whose `[lo, hi]` contains `x`; if there is none, `⊥`; if there is more than one, the `-udi` disambiguation applies and the result is `⊥` unless a subsequent access disambiguates. This is N3005's rule verbatim and it is the reason document 03's hash-table and tagged-pointer idioms work here and do not work under Fil-C's compiler-visibility heuristic.

This judgement is the sanctioned downgrade in the definition of section 4.1, and the monitor **counts** exposures: `--emit=safety-summary` reports how many storage instances a translation unit exposes, because that number is the size of the hole in the provenance argument and a program with zero exposures has a stronger guarantee than one with ten thousand.

**J4, Begin lifetime.** Creating a storage instance allocates a fresh `I` and a fresh `ver`, sets `state = live`, writes `ver` into the lifetime plane over `[lo, hi)`, and sets the type plane to `no-type` and the init plane to zero over the same range. For an automatic instance this is the `alloca`; for an allocated instance it is the allocator's report through document 10's API.

**J5, End lifetime.** Ending sets `state = ended` and bumps the lifetime plane's version over `[lo, hi)` to a value no live capability holds. Every existing capability for the instance therefore fails J1's version conjunct forever, including after the address is recycled. This is what makes the check a use-after-*free* check rather than a use-after-*reallocation* check, which is the distinction PoisonCap draws against Cornucopia Reloaded and which document 03 says is worth 26 points of kernel coverage.

**J6, Free.** `free(p)` is permitted iff `cap(p) ≠ ⊥`, `cap(p).state = live`, `cap(p).class = allocated`, `addr = cap(p).lo`, and the deallocator matches the allocator that created the instance. It then performs J5. T2 and T3 fall out.

**J7, Transfer.** `transfer(p, n, to)` moves a range out of the monitor's authority and back: `to ∈ {device, uninstrumented, kernel}`. While transferred, `state = device_owned` and J1 refuses every access, which is document 03's T8. On return the type and init planes over the range are set conservatively (`no-type`, initialized) unless the caller supplies better. This is the one judgement that has no analogue in any existing tool and it is the one that makes the Linux DMA API's ownership contract checkable.

## 4.5 The soundness statement

What document 14 has to establish, stated so it can be attacked.

> **Claim.** For every execution of a Tier D-instrumented program, and every memory operation executed by instrumented code and not within a declared exemption region, if the operation violates J1 through J7 then the monitor reports it at that operation, and if the monitor reports it then the operation violates J1 through J7.

Two directions, and they are established differently.

**No false negatives** (the ⇒ direction) is established by construction plus verification: document 06 places a check for every conjunct of every judgement at every operation, and document 07's elimination rules (the only thing that removes a check) are individually SMT-verified to remove only checks whose conjunct is implied by facts already established. That is a *local* argument about each rule rather than a global argument about the program, which is what makes it tractable and is the same reason Crocus's methodology works for instruction selection. Document 14 section 14.2.

**No false positives** (the ⇐ direction) is established empirically, over the corpus, and it is the axis document 02 says outranks the other in practice. There is no proof available: the model is an approximation of real C's rules and the question is whether the approximation is right, which is a question about the world.

**The three explicit escape hatches**, each of which appears in the claim's qualifiers:

*Not executed*: the coverage limit. *Uninstrumented code*: the boundary limit, counted per document 10. *Declared exemption region*: an explicit, source-annotated or list-declared region where the monitor is off, also counted.

The claim is deliberately not "the program is memory safe." It is "the monitor is faithful." Those are different statements and conflating them is the standard dishonesty in this field.

## 4.6 What the model deliberately does not decide

**Object lifetime within a storage instance.** C++ placement `new`, and C's rule that an object's lifetime within allocated storage begins at first store, are not modelled. A storage instance's lifetime is the model's unit. This costs us nothing in C and would cost a great deal in C++, which the parent's document 00 puts out of scope anyway.

**The `restrict` contract**, which is checked (Y8) but is not part of J1. It is a separate judgement over pairs of accesses within a scope and it is specified in document 09 section 9.6, because unlike J1 it is not decidable per-access.

**Alignment beyond the access's requirement.** Over-alignment is not tracked.

**Whether two capabilities for the same instance are "the same pointer."** They are, and provenance is per-instance, not per-derivation. A design where each derivation narrows bounds (CHERI's sub-object mode, `-fbounds-safety`'s `__bidi_indexable`) is strictly stronger and strictly more breaking; document 09 section 9.4 offers it as the `-fsafety-subobject` tier, implemented in the type plane rather than by narrowing capabilities, precisely so the default model stays as written here.

## 4.7 The relationship to the parent's UB table

The parent's document 07 section 7.7 gives a closed list of undefined behaviors `rucc` exploits, each with a disabling flag and a detecting sanitizer. Three of its rows are this document:

| Parent's row | Detected by | Here |
|---|---|---|
| An object's lifetime is respected | `-fsanitize=address` | J5, J6; document 08 |
| Objects are accessed through compatible effective types | `-fsanitize=alias` (ours) | J1's type conjunct; document 09 |
| Pointer arithmetic stays within an object | `-fsanitize=pointer-overflow` | J2 |
| `restrict` pointers do not alias | `-fsanitize=restrict` (ours) | Y8; document 09 section 9.6 |
| Uninitialized reads produce a value, not poison | `-fsanitize=memory` | J1's init conjunct; document 09 |

The parent chose, in row nine of that table, to treat an uninitialized read as producing an unspecified but *stable* value rather than LLVM's `poison`, on the grounds that the aggressive model produces baffling miscompilations. That decision is load-bearing here and should not be revisited without reading this document: with a stable-value model, an uninitialized read is a *detectable event with a defined consequence*, and the init plane's report is well-defined. Under a poison model the program's behavior after an uninitialized read is not defined, so a monitor cannot say what it observed. The parent's document 19 question five asks what the no-poison model costs; this document is a large part of the answer to why it is worth paying.
