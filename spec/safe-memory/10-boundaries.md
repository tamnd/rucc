# Boundaries: libc, syscalls, assembly, and the counted trust set

Document 02 names three limits on the safety claim and calls the third the boundary limit. This document is that limit, made concrete: every place instrumented code touches memory the monitor does not control, what happens there, and how the resulting weakening is counted rather than hidden.

This is the document that decides whether the project can reach a kernel. Fil-C's design requires whole-world instrumentation ([its objects do not link against objects from other compilers](https://lwn.net/Articles/1042938/)) and a kernel links firmware blobs, hand-written assembly, and a bootloader. Everything here exists so that "instrumented code linked against uninstrumented code" is a supported configuration with stated properties rather than a failure.

## 10.1 The principle: no silent permission

There are exactly three things the monitor may do at a boundary, and it must do one of them explicitly:

1. **Model it.** The boundary is interposed by a wrapper that performs the judgements the uninstrumented code would have performed, and updates the planes as the uninstrumented code would have. `memcpy` is the archetype.
2. **Transfer it.** J7: the range leaves the monitor's authority, accesses to it are refused while it is away, and the planes are set conservatively on return. `dma_map_single` is the archetype.
3. **Declare it.** A `safe_region_begin` and `safe_region_end` pair with a written reason, counted in the summary. Hand-written assembly is the archetype.

What the monitor may never do is *assume*. A pointer handed to unknown code and later observed to have been written is not silently accepted as still-typed and still-initialized; it is one of the three above, and if the build system has not said which, the default is the most conservative one that does not produce false positives, which for reads is "treat as initialized, no-type" and for the capability is a recovered boundary capability, both of which are weakenings, and both of which are counted.

## 10.2 The trust set, and counting it

**The trust set** is the enumerated collection of things whose correctness the safety claim depends on but which the monitor does not check. Every serious safety argument has one; the contribution here is that ours is *counted per build*, so that "this binary's guarantee rests on 3 assembly regions, 41 boundary-recovered capabilities and 2 exposed instances" is a number a reviewer can read.

The categories:

| Category | What is trusted | Counted as |
|---|---|---|
| The monitor itself | `rucc-safety`, `rucc-safe-rt`, the shadow mapping | not per-build; audited once |
| The verified rules | the SMT encoding is faithful to the IR semantics | not per-build; document 14 |
| Interposed libc | each wrapper implements the same effects as the real function | wrapper count, and the unwrapped-symbol list |
| Declared regions | the reason given is true | region count, by reason |
| Inline assembly | the asm does what its constraints say | asm site count |
| Recovered capabilities | the recovered bounds are not wider than the real ones | recovery count |
| Exposed instances | J3, per document 04 section 4.3 | exposure count |
| Transferred ranges | the device or callee respects the extent it was given | transfer count |
| Uninstrumented objects | linked code respects the extents it is passed | object list |

`--emit=safety-summary` (document 07 section 7.8) reports every row. A build whose counts are all zero except the first two has the strongest guarantee this design can produce; a build with ten thousand recoveries has a guarantee that is mostly aspiration, and the point is that the difference is visible without reading code.

This is a modest idea and it is absent from every tool in document 01. ASan does not tell you how much of your program it did not instrument.

## 10.3 libc and the interposed surface

**The rule: an interposed function is one whose memory effects are written down as judgements.** Not "one we replaced", the wrapper may call straight through to the real implementation, as long as it performs the judgements first and updates the planes after.

Four groups.

**Memory movement.** `memcpy`, `memmove`, `memset`, `memcmp`, `bcopy`, `strcpy`, `strncpy`, `strcat`, `strncat`, `strlen`, `strnlen`, `strcmp`, `strchr`, `strstr`, `snprintf`, `sprintf`, `vsnprintf`. Bounds on both operands, type-plane propagation per document 09 section 9.1's `memcpy` rule, init-plane set on the destination. The string functions are the interesting ones because their extent is *discovered*: `strcpy`'s write length is not known until the source's NUL is found, so the check is performed incrementally against the destination's extent and fails at the byte that overflows, which gives a better report than a length check would. This is document 03's S8 and it is the highest-yield group by a wide margin, because it is where the classic overflow lives.

Where `rucc` lowers a call to one of these to a builtin or an inline expansion (which the parent's document 09 does) the expansion carries the checks rather than the wrapper, and the two paths share the same rule.

**Allocation.** Section 10.4.

**I/O and the syscall surface.** Section 10.5.

**Everything else in libc.** Not interposed, listed. `qsort`, `bsearch`, `strtol`, the math functions, the locale functions: they receive pointers, so they receive J7 transfers or, where the effect is known and narrow, a declared effect annotation. The unwrapped-symbol list is in the summary, so the gap is visible.

**The pragmatic route to a large wrapper set.** Writing several hundred wrappers by hand is a large and boring job with a high error rate. `rucc-safety` generates them from a declarative table, for each function, the C signature plus an effects clause naming which arguments are read, written, and over what extent, in the vocabulary of the `__counted_by` family:

```
memcpy(void * __sized_by(n) dst, const void * __sized_by(n) src, size_t n)
    writes(dst, n) reads(src, n) types(dst := src)
```

The table is data, it lives in `rucc-safe-rt`, and generating wrappers from it means an error is a data fix rather than a code fix. The same table drives document 07's interprocedural summaries, so annotating a libc function makes both the check and the elimination better, which is the monotonicity property document 07 section 7.5 wants.

## 10.4 Allocator interposition

Document 03's false-positive table promises this API. Every production allocator (jemalloc, tcmalloc, mimalloc, the kernel's slab allocators) obtains a large region from the OS and carves it into many objects, which under the model is one storage instance becoming several. A monitor that does not know this either reports every allocation as out of bounds of the arena or, worse, treats the whole arena as one instance and catches no heap overflow at all.

The API, in `rucc-safe-rt`, callable from C and stable at the parent's tier-1 ABI stability:

```c
/* The allocator obtained a region from the OS and will manage it. */
void __rucc_alloc_adopt(void *base, size_t size, unsigned class);

/* Carve [base, base+size) out of an adopted region as a fresh instance. */
void __rucc_alloc_split(void *base, size_t size, unsigned flags);

/* End the instance at base; the storage returns to the adopted region. */
void __rucc_alloc_merge(void *base);

/* Bulk end: every instance within [base, base+size). */
void __rucc_alloc_purge(void *base, size_t size);

/* Associate the instance at base with a deallocator identity, for J6. */
void __rucc_alloc_tag(void *base, unsigned deallocator_id);
```

`__rucc_alloc_split` performs J4 and `__rucc_alloc_merge` performs J5, so a carved object gets a fresh version and every stale pointer to the previous occupant of those bytes fails forever. That is the whole of temporal safety for the heap, and it is why the API is five functions rather than a framework: the allocator's only job is to say when an instance begins and ends, and the monitor does the rest.

`__rucc_alloc_purge` exists for arena and pool allocators that free thousands of objects by resetting a pointer, which is the pattern that would otherwise leave the plane claiming a region is live long after it is not. It is also what Tier K uses for `free_initmem` (document 03's T7) and for slab page reclamation (document 08 section 8.6).

**The default allocator.** `rucc-safe-rt` ships one, because the corpus needs something that works out of the box and because the layout in document 05 section 5.2.2 (header, aux, payload) wants an allocator that knows about aux. It is not required: an adopted third-party allocator works, and carries its aux in the shadow rather than in the block. This document used to say that it pays an extra miss for that. It does not. Document 05.2.6 measured the two layouts and shadow was never worse on trips to memory, better on five of seven access patterns on page walks, and three to four times smaller. An adopted allocator is acceptable, which was the question document 13.5 asked, and the reason to keep shipping one is that the corpus needs a default rather than that the default is faster.

**`mmap`, `brk`, `munmap`.** Interposed directly. A `mmap` is a `mapped`-class instance; `munmap` ends it, giving document 03's T6. `MAP_FIXED` over an existing mapping is a purge followed by an adopt.

## 10.5 Syscalls

The kernel writes user memory and does not consult our planes. Document 03's S9 and the kernel-side infoleak case.

**Reads into user buffers** (`read`, `recv`, `recvmsg`, `readv`, `getdents`, `ioctl` with a known direction): the buffer's extent is checked against the capability *before* the syscall, which catches the classic "size argument larger than the buffer" bug at the point where the bug is, and the written range's init plane is set and type plane set to `no-type` after. This is a real check that catches real bugs and it costs one bounds comparison per syscall, which is noise against a syscall.

**Writes from user buffers** (`write`, `send`, `sendmsg`, `writev`): bounds checked, and at Tier D the init plane is checked over the range, which is the userspace analogue of the kernel infoleak, writing uninitialized bytes to a socket or a file is CWE-200 and is worth reporting.

**`ioctl` with an unknown direction, and anything with a `void *` of unspecified extent:** J7 transfer, counted. There is no honest alternative; the extent is genuinely unknown.

**Scatter-gather and `struct iovec`:** the array of iovecs is itself checked, and each element's base and length are checked against that element's capability, which means the iovec array's *pointers* need capabilities, which they have because they were stored through instrumented code. An iovec array built by uninstrumented code gets recovered capabilities and is counted.

## 10.6 Inline assembly

The parent's document 12 specifies GCC-style extended asm with constraints. The constraints are a partial specification of the asm's memory effects and we use them for exactly what they say:

- **Operands with known extents** (a memory operand of a known type) are checked before the asm and their planes updated after, according to whether the constraint says read, write or read-write.
- **`"memory"` clobber** means the asm may touch anything. The conservative response is to invalidate every established fact (document 06 section 6.2.4) across it, which is what the optimizer already does, and to leave the planes alone, which means the asm's writes are unobserved and later reads of them are treated as initialized and `no-type` per document 09 section 9.2's inversion. Under-report, not over-report.
- **Everything else** is a declared region: the asm site is counted in the summary as a trust-set entry.

`-fsafety-asm=strict` refuses to compile a translation unit containing asm without an effects annotation, which is the posture a security-critical library would take. `__rucc_asm_effects(...)` supplies one in the same vocabulary as section 10.3's table.

There is no attempt to analyze the assembly. Doing so is a research project of its own and the payoff is small: assembly is a tiny fraction of even the kernel, it is heavily reviewed, and it is not where the CVEs are.

## 10.7 Uninstrumented objects, and the mixed link

The configuration that matters most in practice: an instrumented application against uninstrumented shared libraries, or an instrumented subsystem inside an uninstrumented kernel.

**What works.** Document 05 section 5.3's unchanged calling convention means the link succeeds and the program runs. Instrumented code checks its own accesses fully. Pointers received from uninstrumented code get boundary-recovered capabilities. Pointers handed to uninstrumented code are J7 transfers.

**What is lost, precisely.** Accesses performed *by* the uninstrumented code are unchecked, that is the whole of the boundary limit. Additionally, memory written by uninstrumented code has stale planes, so its type and init information is `no-type`/initialized, which weakens Y2, Y3 and Y6 over that memory. Lifetime and bounds are unaffected for instrumented accesses, because they come from the capability and the lifetime plane, both of which the uninstrumented code does not disturb, *unless* the uninstrumented code allocates, which is why the allocator is the one library that must be either instrumented or interposed.

**The `LD_PRELOAD` posture.** For a userspace corpus run, `rucc-safe-rt` can be preloaded so that the allocator and the interposed libc surface are ours even when the application is not rebuilt. That gives quarantine-grade heap checking of the *uninstrumented* application with none of the per-access checks, which is ASan-without-recompilation and is worth having for triage but is not a tier.

**Incremental adoption is the design goal.** A user instruments one library, links it into an unchanged program, and gets checked accesses within that library plus a counted list of what it trusts. Nothing else in this specification is worth much if that does not work, because nobody rebuilds the world to try a compiler flag.

## 10.8 The two boundaries with no good answer

Written down rather than glossed.

**`fork` and shared memory.** A `MAP_SHARED` mapping is written by another process whose planes are not ours. The specification is that shared mappings are `mapped`-class instances with the type and init planes disabled over their extent, and that pointers stored in shared memory get `⊥` capabilities on load, because a capability from another address space is meaningless. Programs that store pointers in shared memory and dereference them are doing something the model cannot check, and they get an honest `⊥` and a report rather than a silent pass.

**`dlopen` of an uninstrumented library that registers callbacks.** The callback is instrumented code entered from uninstrumented code with no call frame, so every pointer argument is recovered. This works and it is counted, and it is the reason the recovery count is a headline number in the summary rather than a footnote.
