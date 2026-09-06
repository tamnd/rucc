# The kernel profile

The target that makes this specification worth writing, and the one Fil-C structurally cannot reach. Everything in documents 04 through 10 was decided with this document in view: versioning rather than collection, narrow pointers rather than fat ones, an unchanged calling convention, a mixed link as a supported configuration, and an interposition API rather than a required allocator. This document says what those decisions buy and what remains hard.

## 11.0 The state of the ground, September 2026

The kernel is not standing still and this project must be honest about what it is competing with.

Rust's experimental phase in the kernel formally ended: maintainers concluded at the December 2025 Maintainers Summit that it had succeeded, and Linux 7.0, released 12 April 2026, made Rust a permanent part of kernel development. Linux 7.1's merge window adds `CONFIG_RUST_INLINE_HELPERS`, inlining C helpers into Rust code for a measured ~2% gain on the Rust null block driver, against a Rust 1.85 baseline and bindgen 0.71.1. Production Rust in-kernel is real: Android 16's Rust ashmem allocator ships on millions of devices via Linux 6.12, and NVIDIA's Nova driver has reached production-quality Hopper and Blackwell enablement. **[These are search-summary sourced and should be confirmed against the 7.0 and 7.1 release notes before they are quoted anywhere load-bearing.]**

What has *not* changed is the 40 million lines of C. The consensus position, stated by the Rust-for-Linux effort itself, is that the kernel will not be rewritten; new code will be safer. That leaves the existing C (where the CVEs are) with exactly the tooling it had: KASAN, KMSAN, KCSAN and KFENCE, all of which are debug instruments.

**That gap is this project's entire reason for existing in the kernel.** Rust makes new drivers safe. This makes the existing forty million lines checkable, and (at Tier K's cost budget) potentially enforceable. The two are complements, not competitors, and any framing that treats them as competitors is wrong.

## 11.1 What a kernel breaks

Enumerated first, because each one kills at least one design that works in userspace.

1. **No collector.** Section 8.2. Kills Fil-C's mechanism outright.
2. **No `mmap` for the shadow.** The planes must be allocated from the physical allocator during early boot, before the allocator being instrumented is fully up. KASAN solves this and we reuse its solution.
3. **Many allocators.** Buddy, SLUB, `vmalloc`, per-CPU, mempools, page cache, DMA pools, bootmem, early `memblock`. Document 10.4's interposition API is the only viable answer; a monitor that owns allocation is not merging.
4. **Multiple virtual addresses for one physical page.** The direct map, `vmalloc` space, `kmap`, per-CPU aliases, and userspace mappings all name the same bytes. A direct-mapped shadow keyed by virtual address gives one page *several* independent plane entries. **This is the hardest unsolved problem in this specification.** Section 11.5 and document 17 question 1.
5. **Memory the CPU does not own.** DMA buffers while the device holds them, MMIO registers with side effects on read. Document 03's T8, and J7.
6. **Deliberate use-after-free.** `SLAB_TYPESAFE_BY_RCU`. Section 11.6.
7. **Bulk lifetime ends.** `free_initmem` frees the entire `__init` section at once. Document 03's T7.
8. **Contexts where nothing may fault, sleep, or take a lock.** NMI, interrupt, `preempt_disable`, holding a raw spinlock, the page fault handler itself, and the early boot path before the planes exist.
9. **Assembly and firmware.** Entry/exit paths, the EFI stub, the decompressor, KVM's guest entry, microcode and firmware blobs. Document 10.6.
10. **`-fno-strict-aliasing`.** The kernel opts out of effective types, which turns off Y2 and Y3 (document 09 section 9.1).

## 11.2 The three kernel sub-tiers

Document 02's Tier K is not one configuration. The kernel's requirements differ too much between "find bugs in a syzkaller VM" and "ship this on a phone."

**K1, the debug kernel.** Everything: all five planes, byte-granular type and init, sub-object bounds, pointer races. Cost budget 5-8x, which is worse than generic KASAN's usual figure and buys sound temporal safety, uninitialized-read detection and type checking simultaneously, where the kernel today needs three mutually exclusive builds (KASAN, KMSAN, KCSAN) to get less. **The single clearest early win is that K1 is one kernel where the tree needs three,** because a bug found by KMSAN in a KASAN build is a bug nobody found.

**K2, the hardened kernel.** Bounds, lifetime, pointer-slot type and init, metadata races. Cost budget 3x, which is document 02's stated Tier K number. This is the distribution-hardened-kernel and cloud-hypervisor-host configuration: too expensive for a phone, cheap enough for a machine whose threat model justifies it.

**K3, the shipped kernel.** Bounds and lifetime only, with `-fsafety-hw=mte` where the hardware has it, targeting under 20%. This is KASAN's HW_TAGS mode's niche and it is where the honest statement is that we are *not* better than HW_TAGS on tagged hardware, because the hardware does the work and everyone gets the same instruction. We are better on hardware without MTE, where HW_TAGS does not exist and the alternative is nothing.

The sub-tier is a Kconfig choice: `CONFIG_RUCC_SAFETY=n|K3|K2|K1`.

## 11.3 Bring-up: what has to work before anything else does

**The planes before the allocator.** The lifetime plane must exist before the first `kmalloc` that will be checked. KASAN's answer is a two-stage bring-up (a small statically allocated early shadow, all pages mapped to one zero page, replaced with real shadow once the page allocator works) and it is the right answer. `rucc-safety`'s Tier K reuses `KASAN_SHADOW_OFFSET`, the arch shadow-mapping helpers, and the early/late split wholesale rather than inventing a parallel mechanism, because that machinery is per-architecture, subtle, and already reviewed by the people who maintain the architectures.

**Instrumentation exclusion.** The kernel already has `KASAN_SANITIZE_file.o := n` and `__no_sanitize_address`, applied to the entry code, the early boot path, KASAN itself, and the arch code that runs before the shadow exists. We consume the same exclusion lists (`KASAN_SANITIZE`, `KCSAN_SANITIZE`, `KMSAN_SANITIZE` in the Makefiles, and the `__no_sanitize_*` attributes) and add `RUCC_SAFETY_SANITIZE` only where our exclusion set genuinely differs. Every excluded file is a document 10.2 trust-set entry and appears in the summary.

**Consuming the annotations that already exist.** This is the point that makes kernel adoption plausible at all. The tree already carries, in-tree and maintained:

- `__counted_by`, `__counted_by_le/be`, and the `-fbounds-safety` family on flexible array members, used by the hardening effort.
- `__user`, `__kernel`, `__iomem`, `__percpu`, `__rcu`, Sparse address-space annotations that name exactly the storage classes document 04's `StorageClass` needs.
- `__init`, `__initdata`, `__exit`, section annotations that name bulk lifetimes.
- `__must_hold`, `__acquires`, `__releases`, lock annotations that give document 09 section 9.5's Lamport clock its synchronization edges for free.
- KASAN's `kasan_disable_current`/`kasan_enable_current` and the poisoning hooks in the slab allocators, which are already exactly document 10.4's `__rucc_alloc_split`/`_merge` at different names.

**We invent no new annotation the kernel does not already have.** `__iomem` becomes `class = mmio`, `__user` becomes a class whose accesses are refused outside `copy_from_user`, `__rcu` participates in section 11.6, and the slab hooks are wired to the interposition API. A patch series that adds a new annotation vocabulary to the kernel does not get merged; one that reads the existing one might.

## 11.4 DMA and MMIO: the checks nobody has

Document 03's T8, and the place where Tier K catches a class with no userspace analogue.

**The DMA ownership contract.** The Linux DMA API specifies, in `Documentation/core-api/dma-api.rst`, that after `dma_map_single` the CPU must not touch the buffer until `dma_unmap_single` or a `dma_sync_*_for_cpu`, and that after `dma_sync_*_for_device` the CPU must not touch it again until synced back. Violating this produces data corruption on non-coherent architectures that is intermittent, hardware-dependent, and among the hardest bugs in the tree to diagnose. **The contract is currently enforced by documentation.** `CONFIG_DMA_API_DEBUG` checks mapping/unmapping bookkeeping but does not check CPU accesses.

J7 checks it directly. `dma_map_*` is a transfer to `device`; `state = device_owned`; J1 refuses every access; `dma_unmap_*` and `dma_sync_*_for_cpu` transfer back. The cost is a state field the plane already has and a comparison the check already performs, so this class is free given everything else, and it is a class that costs real engineering time in the tree today.

**MMIO.** `class = mmio`, from `__iomem`. Three properties: reads have side effects so they must not be speculated, duplicated or elided (the parent's document 09 must treat them as volatile, which it already does); accesses must go through `readl`/`writel` and friends, and a direct dereference of an `__iomem` pointer is a violation the plane catches rather than Sparse catching it statically; and the extent comes from `ioremap`, so a register access past the end of a mapped BAR is an ordinary bounds failure. `iounmap` ends the instance, giving T6 for device memory.

**`copy_to_user` and `copy_from_user`.** The highest-yield checks in the kernel. `copy_from_user` writes kernel memory from an unchecked source: bounds against the destination capability, and the destination's type plane set to `no-type` and init plane set. `copy_to_user` reads kernel memory: bounds, and **at K1 and K2 the init plane is checked over the whole source range including padding**, which is the kernel infoleak, CWE-200, and the class KMSAN was largely built for. Per document 09 section 9.3, the kernel profile defaults to `-fsafety-init=padding` precisely so that structure-padding infoleaks fire. This is one check on one function and it subsumes a large fraction of KMSAN's practical value.

## 11.5 Aliased mappings: the unsolved problem

The direct map, `vmalloc` space, `kmap`, and userspace mappings can all name the same physical page. A shadow keyed by virtual address gives that page several plane entries, and they can disagree: an object freed through its direct-map address leaves the `vmalloc` alias's lifetime plane saying live, so a stale access through the alias is missed (a false negative) and, worse, `meta_begin` through one alias does not initialize the other, so an access through the second alias sees an uninitialized plane, which under document 09 section 9.2's inversion is treated as initialized and is another false negative rather than a false positive. We fail safe on precision and unsafe on recall.

Three candidate answers, none yet chosen. This is **document 17 question 1** and it is the highest-ranked open question in the specification.

**Key the planes by physical address.** Correct by construction, one page, one plane entry, all aliases agree. Expensive: every check needs a virtual-to-physical translation, which for the direct map is a subtraction and for `vmalloc` is a page-table walk. Viable if `vmalloc`-space accesses are rare enough in hot paths, which is an empirical question nobody has measured for this purpose.

**Canonicalize at alias creation.** `vmalloc`, `kmap` and `ioremap` register the alias, and plane operations fan out to every registered alias of the range. Cheap on the check path, expensive on `meta_begin`/`meta_end` in proportion to the alias count, and it requires that every alias-creating path be interposed, the risk being one that is not, which is a silent hole.

**Restrict the claim.** Tier K's soundness statement is scoped to accesses through the direct map, and `vmalloc`/`kmap` aliases are counted trust-set entries per document 10.2. Honest, immediately implementable, and strictly weaker than KASAN, which handles `vmalloc` shadow. Acceptable only as a stepping stone.

The pragmatic sequence is: restrict first, measure how often the restriction bites, then implement canonicalization for the paths that matter, and hold physical keying in reserve. Document 16 puts the measurement in S6.

## 11.6 RCU and `SLAB_TYPESAFE_BY_RCU`

Document 08 section 8.6 states the mechanism; this is the kernel-specific detail.

**Ordinary RCU** is easy: the object is freed by `kfree_rcu` or a callback after the grace period, and that free is the `meta_end`. A reader holding a pointer past the grace period without an `rcu_read_lock` held is a genuine use-after-free and is caught. This is a class syzkaller finds constantly.

**`SLAB_TYPESAFE_BY_RCU`** deliberately permits an object to be freed and immediately reallocated *as another object of the same type* while a reader holds a pointer; the reader is expected to revalidate. Google Project Zero's analysis of MTE in the kernel notes that these regions [cannot be protected by memory tagging](https://projectzero.google/2023/08/mte-as-implemented-part-3-kernel.html), because a tag change would break the contract and no tag change means no protection.

Versioning does better, because a version can be *scoped to the contract*. For a `SLAB_TYPESAFE_BY_RCU` cache, `__rucc_alloc_split`/`_merge` on individual objects do not bump the version; the version is bumped by `__rucc_alloc_purge` when the slab page returns to the page allocator. A stale pointer to a recycled same-type object therefore passes, which is what the contract promises, and a stale pointer surviving past the page's return fails, which is a real bug and is the exploitable case. **No existing tool models these caches this precisely,** and it is available only because the mechanism is a version rather than a tag.

The residual risk is that the caller's revalidation logic is itself wrong (the reader checks a refcount or a generation counter and gets it wrong) and that is a logic error within the model, which document 02 says we do not catch.

**`rcu_dereference` and `__rcu`.** The `__rcu` annotation marks pointers that must be accessed through the RCU primitives. A direct dereference is caught the same way a direct `__iomem` dereference is, from the storage class. This is Sparse's `__rcu` check made dynamic and it is nearly free.

## 11.7 The contexts where the monitor must not run

NMI, interrupt context, `preempt_disable`, holding a raw spinlock, the page fault handler, the entry/exit paths, and early boot before the planes exist.

The specification is that the check path **never allocates, never sleeps, never takes a lock, and never faults**. Concretely:

- The planes are pre-mapped for all of kernel address space; a plane access cannot fault. This is KASAN's requirement too and is why the early shadow exists.
- The check path is a shift, a load, a compare and a branch, with no calls on the success path.
- The failure path calls a reporter that must be NMI-safe: it writes to a lockless per-CPU ring buffer, and the buffer is drained by a workqueue. It does not `printk` directly from NMI.
- `-fsafety-on-error` for the kernel is `log` by default at K1 and K2, a kernel that panics on the first report finds one bug per boot, and syzkaller wants hundreds. K3 defaults to `panic`, matching the kernel's existing `panic_on_warn` posture for hardened builds.
- Recursion into the monitor from the allocator is prevented the way KASAN prevents it: a per-CPU depth counter, and the interposition API's entry points check it.

## 11.8 The falsifiable claim, and how it gets tested

Document 02 states it: **syzkaller running against a Tier K1 kernel finds memory-safety bugs that the same syzkaller corpus running against a KASAN + KMSAN + KCSAN kernel does not.**

It is falsifiable, it is cheap to run once the kernel builds, and it is the only claim in this specification that would convince a kernel maintainer of anything.

The reasons to expect it to hold, each of which is also a way it could fail:

- **Sound temporal safety versus quarantine.** KASAN's quarantine is bounded; a version compare is not. Every use-after-free that survives long enough for the memory to be recycled is invisible to KASAN and visible to us. This is the largest expected source of new bugs and it is testable in isolation.
- **One kernel instead of three.** KASAN, KMSAN and KCSAN are mutually exclusive builds. A use-after-free that only manifests along a path reached because of an uninitialized value is found by neither build alone.
- **Classes nobody checks.** T8 (DMA ownership), Y4 (indirect call signatures), C1 (torn pointer stores) have no kernel checker today.
- **The failure mode:** syzkaller's reachability, not the checker's, is the binding constraint, and we find the same bugs slightly differently. That result would be worth publishing too, because it would say the kernel's residual memory bugs are a coverage problem rather than a detection problem, which is a useful and non-obvious fact.

The comparison must be run properly: same syzkaller version, same corpus, same wall-clock and same CPU-hours (not same iterations, a 6x slower kernel does fewer iterations per hour, and pretending otherwise flatters us), same set of enabled subsystems, and bug reports deduplicated by hand. Document 12 specifies the protocol and document 13 the accounting.

## 11.9 What Tier K does not attempt

**Hypervisor and firmware.** The EFI stub, the decompressor and the early assembly are excluded and counted.

**The scheduler and the memory allocators themselves**, at K1, are instrumented; at K3 they are excluded for cost. Which is which is a Kconfig-visible list, not a hidden default.

**Userspace memory.** `__user` pointers are never dereferenced by instrumented kernel code (that is already a kernel rule) and the `copy_*_user` functions are the boundary. We do not extend the planes into userspace address space.

**Speculative execution.** Spectre-class issues are architectural and orthogonal; the parent's document 12 covers the mitigations and this monitor neither helps nor hinders. Worth stating because "memory safety" is sometimes read to include them and it does not here.

**Replacing Rust-for-Linux.** Stated once more because it will otherwise be misread: the Rust effort makes new code safe, this makes old code checkable, and the kernel needs both.
