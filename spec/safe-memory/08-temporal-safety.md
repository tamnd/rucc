# Temporal safety: use-after-free without a collector

The hard document. Spatial safety is well understood, cheap and solved in the literature; every serious project in this space that shipped shipped spatial safety and deferred temporal safety, and Chrome's 36%-use-after-free figure and the ACSAC 2025 kernel study's 26-point gap between temporal-on and temporal-off both say the deferred half is the valuable half.

## 8.1 The mechanisms, and why we pick the one we pick

Five mechanisms exist. Each is characterized by what happens between `free(p)` and the next access through a stale `p`.

**Quarantine.** Delay reuse; a stale access hits memory nobody else owns and a redzone check catches it. ASan and KASAN do this. It is cheap and it is *bounded*: the quarantine has a size, and once an address is evicted and recycled, the stale access hits a live object and is undetected. This is use-after-*reallocation* detection, and it is a probabilistic tool masquerading as a checker. **Rejected for Tier D on soundness grounds.**

**Sweeping revocation.** On free, find and invalidate every pointer to the object. CHERI's Cornucopia scans memory for capabilities to revoke; [Cornucopia Reloaded](https://dl.acm.org/doi/abs/10.1145/3620665.3640416) uses load barriers and capability load generations to make it concurrent. It is sound, it needs hardware to be affordable, and (as [PoisonCap](https://arxiv.org/abs/2605.13210) points out sharply) the deployed version still provides use-after-reallocation rather than use-after-free safety, because revocation is batched behind an epoch. **Rejected: requires hardware we do not have.**

**Garbage collection.** Do not free until nothing points at the object; make `free` a state change rather than a deallocation. This is Fil-C's FUGC and it is the strongest available answer in a hosted process: `free()` deterministically and atomically disables every pointer to the memory, so use-after-free, double free and invalid free are all guaranteed panics, and reclamation is safe because unreachable stale pointers are repointed at a global free singleton first. It also solves leaks. **Rejected, and this is the central design decision of the specification, for reasons in section 8.2.**

**Lock and key (versioning).** Each storage instance gets a version; each pointer's capability carries the version it was created with; each access compares the capability's version against the plane's current version for that address. Free bumps the plane's version. A stale pointer fails forever, including after the address is recycled, because the *versions* never repeat even though the addresses do. This is CETS (Nagarakatte et al., ISMM 2010), it is the mechanism Zhou, Criswell and Hicks used for [Checked C's temporal safety](https://arxiv.org/pdf/2208.12900), and it is what we take.

**Page-permission tricks.** One object per page, `mprotect` on free. Sound and completely impractical at scale; Oscar and its predecessors. **Rejected.**

## 8.2 Why not a garbage collector

Fil-C is right and we are doing something worse, in userspace, on purpose. The reason is that the goal is a kernel and the two designs cannot both exist.

**A collector needs to own allocation.** FUGC's `verse_heap` is Fil-C's heap. A kernel has the buddy allocator, three slab allocators, `vmalloc`, per-CPU allocators, mempools, the page cache, DMA pools and bootmem, and none of them are going to become a collector's arena.

**A collector needs safepoints and root scanning.** FUGC polls at loop back edges and runs signal handlers only at safepoints. A kernel executes in interrupt context, in NMI context, with preemption disabled, holding spinlocks, in the scheduler, in the page fault handler. There is no point at which every thread can be brought to a state where its roots are enumerable, and the places where a soft handshake would have to wait are exactly the places that must not wait.

**A collector's pauses are unbounded from the kernel's point of view.** Even a fully concurrent on-the-fly collector has handshake latency, and the kernel has hard real-time paths.

**Reachability is not the kernel's ownership model.** Kernel objects are freed by refcount, by RCU grace period, by explicit ownership transfer to a device, and by section teardown at `free_initmem`. An object being reachable does not mean it is live, and an object being unreachable does not mean it may be freed.

Maintaining two temporal mechanisms (a collector for hosted targets and versioning for freestanding) would mean two sets of rules, two verification obligations, two sets of bugs, and a Tier D that means different things on different targets. Document 02's tier structure only works if a tier is the same monitor everywhere. **We pay the version-compare cost in userspace so that the same monitor runs in the kernel.**

What this costs, stated honestly: we do not get leak-freedom, we do not get Fil-C's guarantee that a freed object stays materialized, and we pay a metadata load on accesses where Fil-C pays nothing extra beyond its bounds check. Document 13 measures the gap.

## 8.3 The mechanism, specified

**Versions.** A 64-bit counter, one per allocator arena, incremented on every `meta.begin` and every `meta.end`. Sixty-four bits at 10⁹ allocations per second is 584 years, so wraparound is not a case that needs handling, which matters, because a wrapping version is a soundness hole and the alternative designs with 10-16 bit key spaces (PTAuth, ViK, per document 01) have exactly that hole and buy their lower overhead with it.

**The plane.** Document 05: 8 bytes of version per 16 bytes of address space, 12.5% memory overhead, direct-mapped shadow. Every 16-byte granule of every live storage instance holds that instance's version.

**Begin.** `meta.begin` writes the new version over `[lo, hi)`. For a 4 KiB allocation this is 256 bytes of shadow write, a `memset`, vectorized, and coalesced with the allocator's own zeroing where the allocator zeroes. This is the cost that scales with allocation *size* rather than count and it is the reason large short-lived allocations are the worst case for this design.

**End.** `meta.end` writes a fresh version over `[lo, hi)`. Every capability held anywhere in the program now fails its version compare, forever. This is the property quarantine does not have and it is what makes this a use-after-free check rather than a use-after-reallocation check.

**Check.** `check.live %c, %p` is `cap.ver == plane[p >> 4]`: one shift, one load, one compare, one branch. Fused with the bounds check where both survive, since they share the branch.

**Free.** J6 in document 04: capability non-⊥, state live, class allocated, address equals base, deallocator matches allocator. Double free and invalid free fall out with no extra machinery, which is a real advantage over quarantine designs that need a separate freed-object registry.

## 8.4 Stack temporal safety

The class every hardware scheme punts on. Cornucopia is heap-only and the CheriBSD documentation says so; stack temporal safety is [an open research area at Cambridge](https://www.cheribsd.org/tutorial/23.11/temporal/). Fil-C handles it. So do we, and the mechanism is the same one with one addition.

Every `alloca` is a storage instance with a version. A frame's locals share one version, bumped on entry and on exit, so a pointer to a dead frame's local fails its check. Use-after-return and use-after-scope (document 03's T4) both fall out.

The complication is that stack frames are recycled constantly and writing the version plane over the frame on every function entry and exit is unaffordable, it is a `memset` proportional to frame size on every call. Three mitigations, in order of importance:

**Most frames need no plane at all.** A frame with no address-taken local has no instance and no version. `mem2reg` has already promoted everything else. Measured on real code this is the large majority of functions.

**Frames with address-taken locals that do not escape** need a version but the plane write can be *deferred*: the frame's version is written lazily on the first `cap.of` for a local, and the exit write is only needed for the granules actually covered. Since the compiler knows statically which locals are address-taken, the plane operations cover only those, not the frame.

**Locals whose address escapes are heap-promoted.** This is Fil-C's technique and it is the right one: if a local's address can outlive the frame, the local is not on the stack, it is a heap instance with the frame's lifetime, freed at every exit including unwind edges. Cost is an allocation per escaping local per call, which is why the escape analysis has to be good; it is the same escape analysis document 07 section 7.6 needs for aux elision, so it is paid for once.

Longjmp and unwinding bulk-end every frame between the throw and the catch, per document 03's mitigation table, which requires the unwinder to be an interposed boundary. The parent's document 12 already constrains the optimizer around `setjmp`.

## 8.5 Reclamation, and the one thing a collector does that we cannot

Under versioning, freed memory is reclaimed immediately and reused. A stale pointer to it fails the version check. Good.

But consider: an object's *aux slots* are freed with it, and a stale pointer to the object might be loaded from a slot that has been recycled into a different object's aux. Loading a capability from a recycled aux slot yields the *new* object's capability paired with the *old* pointer value, which is document 03's C1 in a new guise.

The resolution is that aux slots carry the same version as their object. `cap.load %p` reads the aux slot and checks the aux's version against the plane's version for `p` before believing it; a mismatch yields `⊥`. That is one extra compare on a path that is already loading two words from the same line, and it closes the hole.

What we still cannot do, and Fil-C can: guarantee that a *report* is produced. Under FUGC the freed object is kept materialized in a free state as long as anything reachable points at it, so the panic message can name the object. Under versioning the storage is gone and the report has only the version, the address and (because we record it in a small ring buffer of recently ended instances, which is a heuristic) probably the allocation and deallocation sites. Document 06 section 6.5's report quality is therefore best-effort for temporal violations in a way it is not for spatial ones. This is a real regression against Fil-C and it is written down rather than glossed.

## 8.6 The two hard cases

**Reallocation.** `realloc` ends one instance and begins another, possibly at the same address, possibly in place. In place is the awkward one: the address does not move and the contents survive, but every capability for the old instance must fail, because the extent changed. The specification is that `realloc` always ends and begins, even in place, so the version always changes and every pre-`realloc` pointer fails. That catches document 03's T5, which is a real and common bug, and it is stricter than C requires, C says the old pointer is indeterminate, and code that keeps using it is already wrong.

**RCU and type-safe-by-RCU.** The Linux kernel's `SLAB_TYPESAFE_BY_RCU` deliberately permits use-after-free within a type: an object may be freed and reallocated as another object of the same type while a reader holds a pointer, and the reader is expected to revalidate. Google Project Zero's analysis notes that these regions [cannot be protected by memory tagging](https://projectzero.google/2023/08/mte-as-implemented-part-3-kernel.html) for exactly this reason.

Versioning has an answer that tagging does not: **version at the granularity the type-safety contract specifies.** For a `SLAB_TYPESAFE_BY_RCU` cache, the version is bumped when the *slab page* is returned to the page allocator, not when an object within it is freed. A stale pointer to a recycled object of the same type passes, which is the contract; a stale pointer surviving past the slab's return to the page allocator fails, which is a real bug. That is a strictly more precise model than any existing tool applies to these caches, and it is available only because the mechanism is a version and versions can be scoped. Document 11 section 11.6.

Ordinary RCU is easier: the object is freed after a grace period by `kfree_rcu` or the callback, and that free is the `meta.end`. No special handling.

## 8.7 Leaks

Document 03's T9, and the one place we are strictly worse than Fil-C, which gets leak-freedom as a consequence of collection.

`-fsafety-leaks` runs a reachability sweep over the metadata plane at exit or on demand. The roots are globals, thread stacks and registers; the edges are aux slots, which are precise, so the sweep is a precise trace rather than Boehm-style conservative scanning. An allocated instance that is live, unreachable, and never freed is reported with its allocation site.

This is LeakSanitizer's job and LeakSanitizer already does it, with conservative scanning. Ours is more precise for the same reason FUGC's tracing is more precise than Boehm's: the aux plane tells us exactly which words are pointers. Cheap, given everything else, and not a differentiator.

## 8.8 Cost, predicted

Written down so document 13 can contradict it.

The per-access cost is one shadow load and one compare, sharing a branch with the bounds check. On a workload whose working set already misses, the shadow load is a second miss and the cost is large. On a workload that fits in cache, the shadow line for a 16-byte granule covers 8 bytes per 16, so a 64-byte cache line of shadow covers 128 bytes of data, and locality is good.

The per-allocation cost is a shadow `memset` proportional to size. For the corpus's allocation size distribution (dominated by small objects) this is a few stores. For a program that allocates and frees megabyte buffers in a loop, it is 1/16th of a `memset` of the buffer on each of allocation and free, which is 12.5% of a `memset` the program was not doing. That is the worst case and it is worth naming: **large, short-lived, untouched allocations are this design's pathological case.**

The elimination story is worse than for bounds, per document 07 section 7.3: liveness facts are killed by any call that might free, and without interprocedural summaries that is every call. `nofree` summaries are therefore not a nice-to-have; they are the difference between temporal checks costing 5% and costing 40%. Document 16 puts them in S4 rather than deferring them.
