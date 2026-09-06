# The bug model: the closed enumeration, and the coverage matrix

The word "all" in document 02 refers to this list. It is closed. A bug class not on this list is not claimed, and adding one is a specification change, not an implementation detail. This is the same discipline the parent's document 07 applies to undefined behavior: a closed written list is the difference between a claim and a slogan.

Each class states what it is in terms of the model in document 04, which plane of the metadata answers it, which tier carries it, and what it costs.

## 3.1 Spatial

| # | Class | CWE | Model violation | Plane | D | E | K |
|---|---|---|---|---|---|---|---|
| S1 | Heap out-of-bounds read/write | 122, 125, 787 | address outside `[lo, hi)` of its provenance | bounds | ✓ | ✓ | ✓ |
| S2 | Stack out-of-bounds | 121 | same, for an automatic storage instance | bounds | ✓ | ✓ | ✓ |
| S3 | Global/static out-of-bounds | 126 | same, for a static storage instance | bounds | ✓ | ✓ | ✓ |
| S4 | Intra-object (sub-object) overflow | 787 | address inside the allocation, outside the member's extent | type | ✓ opt | ✗ | ✓ opt |
| S5 | Pointer arithmetic escaping its object | none | result outside `[lo, hi]` of its provenance, one-past-end permitted | bounds | ✓ | ✓ | ✓ |
| S6 | Null dereference | 476 | access through provenance ⊥ | bounds | ✓ | ✓ | ✓ |
| S7 | Misaligned access | none | address not a multiple of the access alignment | bounds | ✓ | ✗ | ✓ |
| S8 | Out-of-bounds via library call (`memcpy`, `read`, `snprintf`) | 121-127 | interposed at the boundary; document 10 | bounds | ✓ | ✓ | ✓ |
| S9 | Out-of-bounds via syscall buffer | none | kernel writes past a user buffer's extent | bounds | ✓ | ✓ | n/a |

S5 is worth separating from S1 because the *computation* of an out-of-range pointer is undefined in C even when it is never dereferenced, and catching it there rather than at the eventual dereference is what makes the report point at the bug. Fil-C's three-form bounds check exists for the overflow case where `p += UINT_MAX` wraps; document 06 adopts it directly.

S4 is the class that Fil-C, CHERI-by-default and ARM MTE all miss, for the same reason in each case: their metadata is per-allocation and a member is not an allocation. We can catch it because the type plane is byte-granular. It is also the class most likely to fire on correct code, so it ships behind `-fsafety-subobject`. Document 09 section 9.4.

S7 is not memory-unsafety on x86 but is on several targets and is a reliable predictor of type-punning bugs, so it is in Tier D and out of Tier E.

## 3.2 Temporal

| # | Class | CWE | Model violation | Plane | D | E | K |
|---|---|---|---|---|---|---|---|
| T1 | Heap use-after-free | 416 | provenance version ≠ recorded version | lifetime | ✓ | ✓ | ✓ |
| T2 | Double free | 415 | free of provenance already in ended state | lifetime | ✓ | ✓ | ✓ |
| T3 | Invalid free | 590, 761 | free of a provenance that is not an allocation base | lifetime | ✓ | ✓ | ✓ |
| T4 | Stack use-after-return / after-scope | 562 | automatic instance's lifetime ended | lifetime | ✓ | ✓ | ✓ |
| T5 | Use of a dangling `realloc` result | 416 | old provenance ended at `realloc` | lifetime | ✓ | ✓ | ✓ |
| T6 | Use after `munmap` / `vfree` / `iounmap` | 416 | mapping-backed instance ended | lifetime | ✓ | ✓ | ✓ |
| T7 | Use after `free_initmem` (`__init` data) | 416 | bulk lifetime end of the `__init` section | lifetime | n/a | n/a | ✓ |
| T8 | Use of a `dma_unmap`ped buffer / CPU access while device-owned | none | ownership plane says the device holds it | ownership | n/a | n/a | ✓ |
| T9 | Memory leak (unreachable, never freed) | 401 | reachability, not an access violation | lifetime | ✓ report | ✗ | ✓ report |

The distinction PoisonCap draws (use-after-*reallocation* versus use-after-*free*) is the reason document 08 chooses per-allocation versions over quarantine. A quarantine catches a stale access only until the memory is recycled; a version compare catches it forever. The ACSAC 2025 kernel data says the difference between "temporal safety on" and "off" is 26 percentage points of kernel vulnerability coverage, which is the largest single number in this specification.

T9 is different in kind: a leak is not an illegal operation, it is the absence of one, and it is detected by a reachability sweep at exit or on demand rather than by a check. It is listed because it is CWE-401 and because the corpus expects it, and it is marked "report" rather than "✓" to keep the distinction visible. Fil-C gets leak-freedom as a side effect of its collector; we get leak *detection* as a periodic sweep over the metadata plane, which is strictly weaker and is honest about it.

T7 and T8 are kernel-only and are the two places where Tier K catches a class no userspace tool has an analogue for. T8 in particular checks the Linux DMA API's ownership contract, which is currently enforced by documentation.

## 3.3 Type and initialization

| # | Class | CWE | Model violation | Plane | D | E | K |
|---|---|---|---|---|---|---|---|
| Y1 | Pointer/non-pointer confusion | 843 | reading a non-pointer word as a pointer | type | ✓ | ✓ | ✓ |
| Y2 | Effective-type violation (strict aliasing) | 843 | access type incompatible with the byte's effective type | type | ✓ | ✗ | ✓ |
| Y3 | Union member confusion | 843 | read of a member other than the one last stored | type | ✓ | ✗ | ✓ |
| Y4 | Function pointer called with the wrong type | 843 | callee's signature ≠ call site's | type | ✓ | ✓ | ✓ |
| Y5 | Data called as a function, function read as data | 843 | provenance class mismatch | type | ✓ | ✓ | ✓ |
| Y6 | Uninitialized read | 457, 908 | byte within provenance with no prior store | init | ✓ | ✗ | ✓ |
| Y7 | Uninitialized *pointer* read | 457 | as Y6, restricted to pointer-shaped slots | init | ✓ | ✓ | ✓ |
| Y8 | `restrict` contract violation | none | two `restrict` pointers alias within their scope | provenance | ✓ | ✗ | ✓ |

Y1 and Y5 are the classes Fil-C gets for free from InvisiCaps and they are the load-bearing ones for exploitability: a forged pointer is how a heap overflow becomes code execution. Y2, Y3 and Y8 are the classes the parent's document 07 already promised as `-fsanitize=alias` and `-fsanitize=restrict`; this is where they are specified properly.

Y6 is expensive, it is MSan's problem, and MSan's requirement that *all* linked code be instrumented is what makes it hard. Y7 is the cheap and high-value subset: an uninitialized pointer is a bug with a security consequence, an uninitialized `int` usually is not, and the pointer-shaped slots already carry metadata for other reasons. Tier E carries Y7 and not Y6 for exactly this reason. Note the ACSAC study's observation that CHERI mitigates uninitialized-memory flaws only when they manifest as invalid pointer accesses, which is precisely the Y7-not-Y6 line.

## 3.4 Concurrency

| # | Class | CWE | Model violation | Plane | D | E | K |
|---|---|---|---|---|---|---|---|
| C1 | Torn pointer/metadata store | 362 | pointer value and its capability from different stores | epoch | ✓ | ✓ | ✓ |
| C2 | Data race on a pointer-shaped word | 362 | two unordered accesses, at least one a store | epoch | ✓ | ✗ | ✓ |
| C3 | Data race on metadata itself | 362 | as C2, on the plane | epoch | ✓ | ✓ | ✓ |
| C4 | Race-induced use-after-free (TOCTOU on a freed object) | 416 | T1 with an ordering witness | lifetime+epoch | ✓ | ✓ | ✓ |
| C5 | General data race on scalar data | 362 | none | none | ✗ | ✗ | ✗ |

C5 is explicitly out. Full race detection is ThreadSanitizer's problem, it costs 5-15x, and it is a different product. What is in scope is the subset of races that *cause* memory unsafety, which is the subset touching pointer-shaped words and the metadata plane. Those words already carry metadata, and adding a last-writer epoch to metadata we are already loading is close to free. Document 09 section 9.5 specifies the mechanism and is honest that it is a heuristic detector for C2 rather than a complete one: it finds races it observes, with no false positives, and it does not do vector-clock happens-before reconstruction.

C1 deserves emphasis. Fil-C's own documentation states that a non-atomic pointer store can pair one thread's pointer value with another thread's capability, and that this is memory-safe because the access is still bounded by a real capability. It is memory-safe. It is also a silent wrong answer produced by a race, and the check that would detect it is a metadata version compare that we are performing anyway. Converting Fil-C's accepted imprecision into a diagnostic is one of the concrete senses in which this design is more ambitious.

## 3.5 What will produce false positives, and what we do about each

This section is the one that decides whether the project survives contact with real code. Every entry is a legitimate, widespread C idiom that a naive implementation of the model in document 04 rejects. The mitigation column is the specification; anything not resolvable here becomes a document 17 entry.

| Idiom | Why the naive model rejects it | Resolution |
|---|---|---|
| `container_of(ptr, T, member)` | derives a pointer to the enclosing object from a pointer to a member; sub-object bounds forbid it | S4 is off by default; when on, `container_of`-shaped arithmetic (subtract a constant `offsetof`, land at an object base of matching type) is recognized as a widening and permitted. The recognition is a rewrite rule and is SMT-verified like the rest. |
| Type punning through a `union` | Y3 rejects reading a member other than the one stored | C's rules actually permit it since C99 TC3 for the common-initial-sequence and for reading any member of a union whose address has been taken; Y3 implements 6.5.2.3 rather than folklore. Remaining rejections are real UB and `-fno-strict-aliasing` disables Y2/Y3 entirely, which is what the kernel builds with. |
| Type punning through `char*` / `memcpy` | Y2 | the character-type exemption and the `memcpy` rule from 6.5 are in the model, not exceptions to it. Parent document 07 section 7.8. |
| Pointer-to-integer round trip through a hash table or a tagged pointer | provenance is lost at the integer, so the recovered pointer has provenance ⊥ | PNVI-ae-udi's exposed-address rule: the cast to integer *exposes* the storage instance, and a cast back to a pointer recovers the provenance of an exposed instance containing that address. `-udi` handles the ambiguous case. This is why we implement the WG14 model rather than Fil-C's compiler-visibility heuristic, which rejects exactly this pattern. |
| Low-bit tagged pointers | the tagged value is out of bounds until untagged | provenance survives arithmetic; only *access* is checked, and S5's one-past-end rule is widened to a target-configurable low-bit mask (`-fsafety-pointer-tag-bits=N`) since alignment guarantees the bits. |
| One-past-the-end pointers, `&a[n]` | address == `hi` | permitted by the model as it is by C. Only dereference is rejected. |
| `offsetof`-based arithmetic on a null pointer | provenance ⊥ arithmetic | `offsetof` is a compile-time construct and never produces a run-time access; the frontend folds it. Hand-rolled `((size_t)&((T*)0)->m)` is recognized and folded too. |
| Allocators that `mmap(MAP_FIXED)` over their own mappings, or carve one allocation into many | one storage instance becomes several | the allocator-interposition API in document 10: `__rucc_alloc_split`, `__rucc_alloc_merge`, `__rucc_alloc_adopt`. jemalloc, tcmalloc, mimalloc and the kernel's slab all need it and all get it. |
| `struct sockaddr` / `struct sockaddr_in` deliberate confusion | Y2 | the POSIX-sanctioned confusions are in the model's compatible-type relation. A short list, written down. |
| Flexible array members and the `T x[1]` idiom that predates them | the declared extent understates the real one | the bounds come from the *allocation*, not the declared type, so both work; only S4 sees the declared extent and S4 treats a trailing array as unbounded within the allocation. |
| Reading padding bytes (`memcmp` of structs, hashing a struct) | Y6, since padding is never stored | padding is initialized-by-fiat when the object is; a whole-object store initializes the whole object including padding. Document 09 section 9.3. |
| `setjmp`/`longjmp` and C++ exceptions unwinding past scopes | automatic instances end without a scope exit the compiler saw | the unwinder is an interposed boundary; `longjmp` bulk-ends every instance whose frame is above the target. Parent document 12 already requires the compiler to constrain the optimizer around `setjmp`. |
| Variable-length arrays and `alloca` | extent is dynamic | the extent is a run-time value and the plane stores run-time values; this is not special. |
| Placement `new` / manual object lifetime in C++ | one storage instance hosts a sequence of objects | out of scope: parent document 00 says this is not a C++ compiler. |

This table has been checked against one real program. [Document 18](18-sqlite-idioms.md) walks it row by row against SQLite 3.45.1: nine rows are exercised, five are not exercised by that program at all, one does not apply to C, and no row's resolution had to change. The audit did find three idioms with no row here, which are now questions 8, 9 and 10 in document 17, and they are questions rather than rows because none of them is a false positive under the model as written. Every later corpus member gets the same treatment, and a row that no corpus member exercises is a row whose resolution is untested however many members pass.

The rule that governs all of this: **a false positive at Tier D is a bug in `rucc` at release-blocking severity.** Not a bug in the corpus, not a thing the user annotates around. Document 12's triage process routes every report through a classification step where "the model is wrong" is one of the outcomes and is the outcome that gets the fix.

## 3.6 What is explicitly not in the model

Stated so that no one has to infer it.

**Undefined behavior that is not a memory error.** Signed overflow, shift-amount, division, `_Bool` range, `unreachable`. These are the parent's document 07 table and its UBSan checks, they are already specified, and they are a different feature that shares a command-line prefix.

**Integer overflow in size computations**, except where the resulting access is itself out of bounds, in which case S1 catches it at the access. Catching it at the multiplication is `-fsanitize=unsigned-integer-overflow`, which produces false positives on hashing code and is the parent's problem, not this one.

**Uninitialized non-pointer scalars at Tier E.** Y6 is Tier D and Tier K only.

**General data races.** C5, above.

**Control-flow integrity, stack protection, CFI, shadow stacks.** These are exploitation mitigations, they are already in the parent's document 12, and they are orthogonal: they make an unfixed memory bug harder to exploit, where this document makes it visible.

**Logic errors that stay within the model.** Document 02's model limit.

**Information disclosure through padding, uninitialized copies to userspace excepted.** The kernel case (CWE-200 via `copy_to_user` of a partially initialized struct) *is* in scope at Tier K, because it is Y6 evaluated at a boundary and the boundary is one we already interpose. It is one of the highest-yield checks in the kernel and KMSAN exists largely for it.

## 3.7 How this matrix gets evaluated

Every row has test cases and a number in CI. Document 14 specifies the harness. The three evaluations that matter:

**Per-row Juliet coverage.** Every row with a CWE column runs the corresponding Juliet cases and reports detected/missed/false-positive, per tier. Rows that miss cases get a document 17 entry naming the case.

**The ACSAC kernel replay.** The 439 labelled kernel CVEs, each classified against this matrix by hand, producing a predicted-coverage number, and then (for the subset with reproducers) an *observed* coverage number at Tier K. The gap between predicted and observed is the most informative number this project produces, because it measures how much the boundary limit in document 02 actually costs.

**The escape suite.** Document 14 section 14.5: for every real bug the corpus finds, a reduced test case is added permanently. A regression that reintroduces a missed detection is caught the same way a miscompilation regression is.
