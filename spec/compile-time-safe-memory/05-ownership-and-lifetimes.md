# Ownership and lifetimes

Static temporal safety. The hard half, for a reason that is structural rather than a matter of effort, and the document that has the most to say about what *cannot* be done.

## 5.1 The obligation, and the two ways to discharge it

`O.live` from [`03.2`](03-obligations.md):

```
cap(p).state = live  ∧  cap(p).ver = lifetime_plane(addr)
```

There are exactly two ways to prove it, and conflating them is why this area is confused in the literature.

**Flow discharge.** Prove that on every path from the point where `cap(p)` was created to this access, **no end-lifetime event occurred for that storage instance**. This is a control-flow property of the analyzed region. It needs no notion of ownership, no regions, no type system.

**Ownership discharge.** Prove that `p` is a borrow, valid for a region, from an owner whose lifetime encloses the region. This is the Rust argument, and it is what layer 5 attempts.

The finding this document is built on: **flow discharge is cheap, reliable, and where nearly all of the realizable yield is. Ownership discharge is expensive, fragile on real C, and should be attempted last.** That inverts the intuition (Rust made ownership the headline idea, so ownership feels like the answer) and it follows directly from [`01.4`](01-research-2026.md)'s evidence that full C does not statically own.

## 5.2 Flow discharge: the no-free interval

The analysis is small enough to state completely.

Define, over a function's CFG, the **free-free intervals**: maximal regions in which no operation can end a storage instance's lifetime. An operation can end a lifetime if it is:

- a call to `free`, `realloc`, or any deallocator in the interposition table ([`../safe-memory/10.3`](../safe-memory/10-boundaries.md));
- a call to a function without a `nofree` summary ([`04.6`](04-the-discharge-ladder.md));
- the end of an enclosing scope, for automatic instances;
- a `transfer` (J7) of a range containing the address;
- **any point at which another thread may run, for an instance reachable by another thread**;
- **any point at which a signal handler may run, for an instance the handler can reach.**

Within an interval, if `O.live` holds at the top, it holds throughout. So:

> Discharge `O.live` at access *b* if some dominating point *a* establishes it (by a `Checked` obligation, by a successful allocation, or by an `alloca`) and *a* and *b* lie in the same free-free interval for that instance.

That is the whole rule and it composes with everything: it is the same dominator-tree walk layer 1 already performs, extended with one extra invalidation condition.

### 5.2.1 The two entries in the invalidation list that are easy to forget

**Thread interference.** A pointer to an object reachable from a global, in a program that creates threads, cannot use a free-free interval across any point where another thread may run, which, absent a memory-ordering analysis, is *every point*. In practice this means the analysis is sound only for instances proved thread-local, which is what layer 3's `noescape` gives for stack and freshly-allocated objects and gives for nothing else. Programs with heavy shared mutable state get little from this section, and that is correct rather than pessimistic.

**Signal handlers.** Same argument, weaker in practice because handlers that call `free` are already undefined behavior in C. Treat a program as signal-clean by default and provide `-fsafety-assume-signal-frees` for the paranoid; record the assumption in the summary's trust counts, since [`../safe-memory/10.2`](../safe-memory/10-boundaries.md)'s discipline is that assumptions are counted.

### 5.2.2 Yield

This should be the largest single source of temporal discharge, for the reason [`../safe-memory/08.8`](../safe-memory/08-temporal-safety.md) already names: temporal checking costs 5% with `nofree` summaries and 40% without, which is a statement that most accesses are in a free-free interval whose top is nearby. **Predicted 40-60% of static `O.live`, higher dynamically because tight loops are free-free by construction. [unverified: V2]**

## 5.3 Stack locals and escape

The most reliable win in the document, and the one most likely to be dismissed as trivial.

An automatic storage instance whose address does not escape its frame has a lifetime that is *syntactically* the frame's, and every access to it through a pointer derived in that frame discharges `O.live` at layer 0. No analysis, no fixpoint.

Two things make this bigger than it sounds. First, `mem2reg` runs *after* insertion in [`../safe-memory/15.3`](../safe-memory/15-integration.md)'s pipeline and promotes most non-escaping locals to SSA values, at which point their obligations vanish entirely, but the ones that survive promotion (address-taken arrays, structs passed by pointer to `nofree` callees) are exactly the ones this rule catches. Second, [`../safe-memory/08.4`](../safe-memory/08-temporal-safety.md) already says most frames need no lifetime plane at all; this is the static justification for that claim, and it should be *proved* per frame rather than assumed.

**The case that must not be lost:** a local whose address is stored somewhere outliving the frame. [`../safe-memory/08.4`](../safe-memory/08-temporal-safety.md) heap-promotes escaping locals; the escape analysis here is the same analysis, and it must be conservative in the direction of *not* discharging. An escape analysis used for optimization may be unsound in ways that cost only performance; used here it is in the trust set. [Document 10](10-soundness-and-trust.md) §10.4 says which analyses are, and this is one of them.

## 5.4 Regions and arenas

The one allocation discipline in C that genuinely does own, and it is common: obstacks, `apr_pool`, `talloc`, per-request arenas in servers, `devm_*` in the kernel, and every parser that allocates an AST and frees it in one call.

**The recognition rule.** An arena is a value `A` with a `create`, an `alloc(A, n)` returning pointers, and a `destroy(A)` ending all of them. Given that shape (recognized either from an annotation (`__arena`, [document 08](08-annotations.md)) or by a pattern over the interposition table) every pointer from `alloc(A, ·)` has a lifetime enclosing every point dominated by the `alloc` and dominating the `destroy`, and `O.live` discharges by dominance alone.

**Why it is worth a section.** Arena-allocated objects are frequently the hottest data in a program (ASTs, request state, parse buffers) and their access patterns are the pointer-chasing ones where [`../safe-memory/13.7`](../safe-memory/13-performance.md) predicts the worst cost. Discharging their `O.live` obligations *wholesale* is one of the few realistic routes to [`03.5`](03-obligations.md)'s plane elision on heap data, because it discharges every reader of the lifetime plane over a whole region at once.

**Requirement:** the arena's own implementation must not defeat this by recycling within the arena. `talloc`'s per-node free and `apr_pool`'s sub-pool clear do exactly that, so the rule needs the arena to be declared as *bulk-free-only* or the recognizer must see the individual free and give up. Give up loudly in the summary rather than quietly.

## 5.5 Reference counting

Not ownership and not flow, and it needs its own treatment because it is how a large fraction of real C manages lifetime: CPython's `Py_INCREF`, glib's `g_object_ref`, the kernel's `kref`/`refcount_t`, OpenSSL's `_up_ref`.

**The rule.** If a dominating operation increments a reference count on the instance, and no operation between it and the access decrements it on this path, then the instance is live, *provided* the reference-counting discipline is correct, which is precisely what we are not proving.

That proviso is fatal to treating this as a proof, and honest treatment is:

- **A held reference is an assumption, declared via `__acquires`-style annotation** (which the kernel, again, already has via Sparse's `__acquires`/`__releases` and `__must_hold`).
- The assumption is counted in the trust set, per instance, in the summary.
- It is **not** available at `default`; it requires `-fsafety-assume-refcounts`, off unless asked for.

The alternative (proving the refcount discipline itself) is a functional-correctness proof of the object's protocol, which is layer 6 on a per-type basis. That is a legitimate use of [document 07](07-separation-logic.md) and it is exactly the kind of high-leverage, small-surface target §7 selects for, but it is not something the ladder does automatically.

## 5.6 RCU, and the best fit in the specification

Read-copy-update is a lifetime protocol that is **statically annotated, syntactically scoped, and already present in the source tree we most want to analyze.**

Inside an RCU read-side critical section (between `rcu_read_lock()` and `rcu_read_unlock()`) an object obtained from an RCU-protected pointer cannot be freed, because the grace period cannot complete. That is exactly a free-free interval, delimited by two function calls the kernel already writes, over pointers the kernel already marks `__rcu` for Sparse.

So:

> **`rcu_read_lock()` opens a free-free interval for every instance reached through an `__rcu`-annotated pointer, closed by the matching `rcu_read_unlock()`.**

This discharges `O.live` over the interior of every RCU read-side critical section, which in networking and VFS fast paths is most of the hot code. It costs a recognizer for two function names and a reader of an annotation that is already in the tree, and it needs no inference at all.

It is also the clearest instance of [`../safe-memory/11.3`](../safe-memory/11-kernel.md)'s principle (*we invent no new annotation the kernel does not already have*) paying off on the proof side rather than the monitor side.

**The caveat, and it is the one from [`../safe-memory/08.6`](../safe-memory/08-temporal-safety.md):** `SLAB_TYPESAFE_BY_RCU` means an object *can* be recycled inside a read-side section, into another object of the same type. The version in that model is scoped to slab-page return rather than object free, so the lifetime plane's version does not change on recycle, which means the flow rule above is sound for the *plane check* precisely because the monitor's model already made it so. The two designs agree, and they agree because they were derived from the same reading of the kernel's rules. Nothing about the object's *contents* is discharged, and code that reads a recycled object's fields still needs its `O.type` and `O.init` obligations.

Nested locks, `srcu_read_lock`'s cookie-carrying form, and `rcu_read_lock_bh` all need the same treatment and are mechanical once the first works.

## 5.7 Ownership inference, layer 5, and its realistic scope

What [`01.4`](01-research-2026.md)'s tradition offers, and where it applies.

**The shapes that infer.** Tree-shaped data with a unique parent pointer. Functions with a clear `create`/`destroy` pair over an opaque type. Buffers allocated, filled and freed within one function or a two-function pair. Values moved rather than shared, the `p = f(); g(p); free(p);` chain.

**The shapes that do not, all of which are load-bearing in the code we care about:**

*`container_of`.* A pointer to an embedded member is upcast to the container. Ownership flows in the opposite direction to the type. This is the same construct that [`../safe-memory/09.4`](../safe-memory/09-type-init-and-races.md) had to preserve by putting sub-object bounds in the type plane rather than narrowing capabilities; here it defeats field-based ownership inference outright.

*Intrusive lists.* The list node is inside the object, the list does not own the object, and which pointer owns is a convention documented in a comment.

*Callbacks holding pointers.* Ownership crosses a function-pointer boundary and no whole-program analysis short of full devirtualization sees it.

*Globals.* Anything reachable from a global has program lifetime by default, which discharges trivially and uselessly, or is freed by some path the analysis must find.

**The decision rule.** Layer 5 is built, measured at V4, and cut if it discharges under 10% of the temporal residue at that point. It is written as a separable pass with no other consumers so that cutting it is deleting a file, not unpicking a design. Given [`01.4`](01-research-2026.md)'s evidence, Scylla's applicative subset, Cpp2Rust's retreat to run-time checks, &inator's stated scaling limitation, **the prior should be that it gets cut**, and it is in the plan because being wrong about that would be worth a lot and finding out costs one measurement.

## 5.8 What is handed to the monitor, without apology

The residue is large and this is the list, so that nobody is surprised by it later:

- Any access to a shared, mutable, non-thread-local object across a point another thread may run.
- Objects whose lifetime is a protocol: refcounts without the assume flag, state machines, "owned by the subsystem that last set this flag".
- Anything reached through `container_of` from a member whose container's lifetime is not evident.
- Everything downstream of an unresolved indirect call.
- Every use-after-free that is a *bug in the ownership discipline itself*, which is definitionally the interesting case and is exactly what static ownership reasoning would have to assume in order to proceed.

That last point deserves the emphasis: **a proof of temporal safety by ownership inference must assume the program's ownership discipline is correct in order to infer it.** The code that has a use-after-free is code whose discipline is wrong. This is not fatal (an inference that fails on buggy code and succeeds on correct code still removes checks from the correct code, which is the point) but it means static temporal reasoning is systematically weakest exactly where the bugs are, and the monitor is not optional.

## 5.9 The payoff: lifetime-plane elision

Per [`03.5`](03-obligations.md), discharging obligations only recovers cost when a plane dies. For the lifetime plane, the condition is:

> The lifetime plane is dead over a storage instance if every `O.live` obligation on every access to that instance, within the analyzed scope, is `Discharged`.

When it holds, J4's range write of the version at allocation and J5's bump at free both become dead stores over that range, and [`../safe-memory/08.3`](../safe-memory/08-temporal-safety.md)'s 8:1 plane traffic disappears for that object.

**Where it will actually fire:** non-escaping stack frames (§5.3, and this is [`../safe-memory/08.4`](../safe-memory/08-temporal-safety.md)'s claim made static), arenas (§5.4), and short-lived heap objects allocated and freed within one `nofree`-clean function. **Where it will not:** any object stored into a long-lived structure, which is most heap data in most programs.

This is the concrete form of [`02.2.1`](02-the-goal.md)'s warning. A program can discharge 60% of its `O.live` obligations and elide the lifetime plane for 5% of its bytes, and the second number is the one that shows up in a benchmark.

## 5.10 Claims

**T1.** Flow discharge (§5.2) plus escape (§5.3) discharges **≥ 40% of static `O.live`** on the tier-1 corpus at `-fsafety-proof=default`. **[the load-bearing temporal claim]**

**T2.** RCU-interval discharge (§5.6) discharges **≥ 60% of `O.live` inside kernel RCU read-side critical sections**, measured on the networking and VFS paths.

**T3.** Lifetime-plane elision (§5.9) covers **≥ 30% of allocated *bytes*** on the corpus. This is the one that predicts `R`, and it is the one most likely to fail.

**T4.** Layer 5 ownership inference discharges **≥ 10% of the residual temporal obligations after layers 0-3**, or it is cut at V4.
