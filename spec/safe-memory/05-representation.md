# The metadata plane: representation, layout and ABI

Document 04 says what facts the monitor needs. This document says where they live. It is the document that decides the memory overhead, most of the cache behavior, and whether instrumented code can be linked against anything else, which is to say it decides most of the cost.

## 5.1 The decision: narrow pointers, side metadata

Three representations are available and the literature has tried all three.

**Fat pointers** put bounds inline: a pointer becomes 128 or 256 bits. This is the easiest to implement and the worst to live with. It changes `sizeof(void*)`, so it changes every struct layout, so every structure written to a file, sent over a socket or shared with another process changes shape. It breaks every ABI. Fil-C started here, [its first pointers were 256 bits](https://github.com/pizlonator/fil-c/blob/deluge/invisicaps_by_example.md), not thread-safe and without use-after-free protection, and moved off. Apple's `-fbounds-safety` uses implicit wide pointers for *locals only*, precisely because locals have no ABI, and reconciles at boundaries with `__counted_by`. That is the right use of fat pointers and it is a check-elimination technique, not a representation. Document 07 treats it as such.

**Tagged pointers** put a small tag in unused address bits and match it against a tag stored per granule: ARM MTE, HWASan, KASAN's SW_TAGS. Cheap and probabilistic. Four bits of tag is a 1-in-16 miss rate, which disqualifies it from Tier D by definition, and the 16-byte granule makes intra-granule overflow invisible. Excellent as an accelerator, unusable as the mechanism.

**Narrow pointers with side metadata**: the pointer is its natural size and its capability lives elsewhere, found by address. This is Fil-C's InvisiCaps after three iterations, it is SoftBound's design, and it is what we take.

The cost is stated up front because it is the honest headline: **every pointer stored in memory needs its capability stored somewhere, which is a second cache line touched by a data structure that fits in one.** Fil-C's accounting says structures containing pointers use roughly double the memory. This is not an instruction-count problem and reasoning about it in instructions will mislead. Document 13 measures cache misses and memory traffic, not instruction counts.

## 5.2 Layout

Four structures. The design goal throughout is that the common path (a bounds-and-lifetime check on a pointer already in a register) touches **one** metadata cache line, and that line is the one the pointer's own aux slot lives in.

### 5.2.1 The capability, in registers

A capability in flight is a 4-tuple in registers or spilled to the stack, never to a fixed format the program can see:

```
lo   : u64      base
ext  : u64      hi - lo, so that bounds arithmetic is one subtract and one compare
ver  : u64      lifetime version
meta : u64      { class:4, perm:3, state:2, tag_bits:4, flags:8, instance_id:43 }
```

`ext` rather than `hi` because the hot check is `(addr - lo) <u ext - n`, one unsigned compare that catches both underflow and overflow, which is the standard trick and is why the classic implementations store a base and a length. The three-form check Fil-C documents is then unnecessary in the common case; document 06 section 6.3 gives the cases where it is not.

`instance_id` at 43 bits gives 8.8 × 10¹² storage instances before wraparound, which at a million allocations per second is 100 days. `ver` is a full 64 bits and is the value that must not repeat, so the instance id is a debugging aid rather than a safety-critical field.

Register pressure is the obvious objection: four registers per live pointer is not affordable on x86-64. It does not arise, because a capability is only materialized where a check needs it, and document 07's whole purpose is that most checks do not survive. Where several checks share a capability, the parent's `regalloc2`-class allocator with live-range splitting is exactly the right tool. Document 13 measures spill counts as a first-class metric.

### 5.2.2 The aux plane: capabilities for pointers in memory

Every pointer-shaped, pointer-aligned word of program memory has an aux slot. Following Fil-C, aux storage is allocated alongside the object rather than in a global shadow map, because the aux line is then adjacent in the physical page and prefetched with the data.

For an allocated instance, `rucc-safe-rt`'s allocator over-allocates:

```
   [ header : 32 bytes ][ aux : ceil(n/8) * 16 bytes ][ payload : n bytes ]
   ^header                                             ^returned pointer
```

The header holds `lo`, `ext`, `ver`, `meta` and the allocator identity. The aux array holds 16 bytes per 8 payload bytes: `ver` and a packed `(lo, ext, meta)` for the capability of the pointer stored at that slot, with `lo` and `ext` compressed to 26 bits of exponent-and-mantissa in the manner of CHERI's capability compression, which bounds the representable-region error and is well studied. **[The compression scheme is a design decision not yet made; document 17 question 5. The straw man is CHERI-128's, adapted.]**

A slot whose payload word is not a pointer has `ver = 0`, which is the encoding of `⊥`. That single fact gives class Y1 for free: reading a non-pointer word as a pointer yields `⊥` and the first access through it fails.

For automatic instances the aux for the frame is a contiguous region in the frame itself, sized at compile time, so a stack pointer's capability lookup is a fixed offset from the frame pointer. For static instances it is a section, `.rucc_aux`, laid out by the object writer in the parent's document 11 with a relocation per initialized pointer so that statically initialized pointers have correct capabilities before `main`.

### 5.2.3 The range planes: lifetime, type, init, epoch

The four per-byte or per-range planes cannot live alongside objects, because the whole point of the lifetime plane is that it must be readable *after* the object is gone. They live in a direct-mapped shadow, in the manner of ASan and KASAN, at a target-specific scale and offset:

| Plane | Granularity | Shadow scale | Bytes per 4 KiB page |
|---|---|---|---|
| lifetime `ver` | 16 bytes | 8:1 (8 bytes of version per 16 bytes) | 2048 |
| type | 1 byte | 4:1 (a 32-bit `TypeId`) | 16384 |
| init | 1 byte | 1:8 (one bit) | 512 |
| epoch | 8 bytes | 8:1 (a 64-bit `(thread, clock)`) | 4096 |

That is the Tier D configuration and its memory overhead is dominated by the type plane, at 4x. TySan pays 8x for the type plane alone. Tier D's stated 2x memory budget in document 02 is therefore **only reachable with the type plane compressed**, and the compression is the standard one: the plane is stored per-16-byte-granule as a *homogeneity flag plus a `TypeId`*, falling back to a per-byte side table only for granules that are heterogeneous. Real structures are overwhelmingly homogeneous per granule. That takes the type plane from 4:1 to about 1.25:1 with a slow path. **[The homogeneity hit rate is unmeasured; document 17 question 6.]**

Tier E turns off the type and init planes entirely except for pointer slots, which are already in the aux, and its shadow is the lifetime plane alone at 8:1, a 12.5% memory overhead, which is where the 1.4x memory budget comes from once aux is included.

The lifetime plane at 16-byte granularity is why allocations round to 16 bytes and 16-byte alignment, which is Fil-C's minimum too and is the natural malloc alignment on both our 64-bit targets anyway.

### 5.2.4 Mapping the shadow

Direct-mapped with a shift and an add, per ASan and KASAN: `shadow = (addr >> k) + offset`. Three concerns, each with a decided answer.

**Address-space layout.** The parent targets Linux, macOS and Windows on x86-64 and AArch64. `MAP_NORESERVE` reservations of the shadow ranges at startup, with the offsets chosen per target in `rucc-target` as data, following the same discipline the parent's document 18 imposes: no target-specific code outside `rucc-target`.

**AArch64 with 52-bit VA and 64K pages** breaks fixed offsets that assume 48-bit. The offset is computed at runtime from `/proc/self/maps` or the target's equivalent on first use, and is a load rather than a constant in that configuration. One extra load on the shadow path, on one configuration.

**The kernel has no `mmap`.** Tier K's shadow is allocated the way KASAN's is, from the physical allocator during early boot, with the kernel's own shadow mapping helpers, and with the `KASAN_SHADOW_OFFSET` machinery reused wholesale. Document 11.

## 5.3 The ABI

This is where Fil-C stops and the reason it stops there is instructive: [its objects do not link against objects from other compilers](https://lwn.net/Articles/1042938/), and a non-Fil-C compiler is still needed for its own runtime, for glibc and for the kernel. Any design that requires whole-world instrumentation cannot reach a kernel, because a kernel links firmware blobs and hand-written assembly.

**The rule: an instrumented function's calling convention is unchanged.** Pointer arguments are passed in the same registers, in the same order, with the same sizes. Capabilities are passed **out of band**, in a per-thread side channel: a small fixed-size array in thread-local storage, written by the caller and read by the callee, indexed by argument position.

```
struct rucc_call_frame {
    u32 magic;              // identifies an instrumented caller
    u16 argc;               // number of pointer arguments described
    u16 flags;
    struct cap args[8];     // first 8 pointer arguments
    struct cap ret;
    struct rucc_call_frame *outer;
};
```

The consequences are exactly the ones we want:

**An instrumented function called by uninstrumented code** sees a stale or absent `magic` and treats every pointer argument as having a capability recovered from the shadow planes, which is possible, because the lifetime and type planes are keyed by address and the aux plane can be consulted if the pointer came from instrumented memory. If no capability can be recovered, the argument gets a **boundary capability**: bounds recovered from the containing mapping, `ver` matching the plane, and a flag marking it as recovered. Recovered capabilities are counted in the safety summary because each one is a weakening.

**An uninstrumented function called by instrumented code** is a J7 transfer for any pointer it receives, per document 10. The caller declares what it hands over.

**Structure layout, `sizeof`, `offsetof` and `alignof` are unchanged**, so an instrumented `struct stat` is the `struct stat` the kernel writes.

**Varargs** are handled the way Fil-C handles them and for the same reason: the variadic tail is materialized into a heap instance with its own capability, so `va_arg` past the end is a bounds failure and extracting an integer as a pointer yields `⊥`. This gives document 03's Y1 at the variadic boundary, which is the place it most often matters (`printf("%s", 42)`).

The cost of the out-of-band convention is one TLS access and up to eight capability stores per instrumented call. Both are eliminable: document 07's interprocedural summaries let a call between two functions in the same module, where the callee's checks are all discharged, drop the frame entirely. Whether that elimination fires often enough to matter is document 17 question 4.

**Alternative considered and rejected: a shadow argument register set.** Passing capabilities in otherwise-unused registers is faster and is what a from-scratch ABI would do, but it is not expressible without changing the psABI, it interacts badly with the parent's document 12 varargs and struct-passing rules, and it makes `setjmp`/`longjmp` and unwinding much harder. The TLS frame is slower and correct.

## 5.4 Alternate lowerings on capable hardware

The plane is an interface, not an implementation. Three alternate lowerings, selected by `-fsafety-hw=`:

**ARM MTE** (`-fsafety-hw=mte`, Tier E and Tier K3 only). The lifetime plane's version is truncated to the 4-bit tag and the check becomes free, the hardware does it in the memory pipeline at [1-2% in ASYNC mode](https://newsroom.arm.com/blog/memory-safety-arm-memory-tagging-extension). This is a *downgrade*: 4 bits is a 1-in-16 miss rate and 16-byte granularity, so it is inadmissible at Tier D. The NanoTag tripwire technique (reserve one tag value to mean "software must check this granule") recovers byte granularity for granules that need it, and is the right way to combine the two. Do not make any tier *depend* on MTE; document 01 records that the Pixel 11 may have dropped it.

**ARM pointer authentication** (`-fsafety-hw=pac`) can sign the aux slot's version field, making a forged aux entry detectable. Cheap and narrow; a hardening measure rather than a mechanism.

**CHERI** (`-fsafety-hw=cheri`, post-1.0). If the target has capability hardware then the bounds and permission conjuncts of J1 are enforced by the ISA for free and the aux plane disappears entirely for capability-typed slots, leaving us to supply the type, init and epoch planes, which is exactly what CHERI does not have and what PoisonCap is currently adding in hardware. This is the configuration where this design is at its best, and keeping the plane abstract so it stays reachable is worth the small amount of indirection it costs everywhere else.

## 5.5 What this costs, stated before it is measured

The predictions, written down now so that document 13's measurements can contradict them.

**Memory.** Aux is 2 bytes per byte of pointer-dense structure and near zero for arrays of scalars, so the corpus geomean should land near 1.35x from aux alone. The lifetime plane adds a flat 12.5%. Tier D's type and init planes add a further 30-40% if the granule-homogeneity compression works as expected and 400% if it does not. The 2x budget in document 02 rests entirely on that compression, which is why it is document 17 question 6.

**Time.** The instruction cost of a full J1 check is roughly: one aux load if the pointer came from memory, one shadow load for the lifetime version, one compare-and-branch for bounds, one for version, and (at Tier D) one shadow load and compare each for type and init. Six to eight instructions and two to four loads for an unoptimized check. Document 07 exists to make most of them go away; if it fails, this is a 4x tool and Tier E does not exist.

**The thing that will actually hurt** is neither of those. It is that a pointer chase through a linked structure now touches two lines per node instead of one, which doubles the miss rate of exactly the workload (pointer-heavy scalar code) that the parent's document 02 picked as its performance axis. Cpp2Rust's 6x on Brunsli, a pointer-arithmetic-heavy program, against 2% on WOFF2 in the same evaluation, is the shape of the result to expect.
