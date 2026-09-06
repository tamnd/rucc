# Bounds and refinements

Static spatial safety. The larger half of the obligation count, the tractable half, and the one where the literature has actually converged.

[Document 04](04-the-discharge-ladder.md) places the layers; this document specifies what they must handle, organized by the *shapes C actually takes* rather than by analysis technique, because the failure of static bounds checking has never been a shortage of domains, it has been a mismatch between the domains and the shapes.

## 6.1 The extent question

`O.hi` is `addr + n ≤ cap(p).hi`. Everything reduces to: **what is `hi`, symbolically, at this point?**

C gives five answers and they have wildly different difficulty.

| Source of extent | Difficulty | Share **[unverified]** |
|---|---|---|
| A declared array or object type | trivial, layer 0 | ~35% |
| An allocation site in view: `malloc(k)`, `malloc(n * sizeof(T))` | easy, layer 1/2 | ~20% |
| A parameter, with an annotation | easy given the annotation, layer 3/4 | ~15% |
| A parameter, without one | **hard**: layer 4/5, or never | ~20% |
| A struct field whose length is another field | **hard without an annotation**: layer 4 | ~10% |

The two hard rows are half the problem, they are both about *crossing a boundary where the extent was not written down*, and they are the reason [document 08](08-annotations.md) exists. **The single most valuable thing a programmer can do for this specification is annotate a header**, and the single most valuable thing [document 09](09-inference-and-llm.md) can do is annotate one for them.

## 6.2 The shapes

Enumerated because coverage of *these* is the specification's real spatial requirement, and each names the mechanism responsible.

**S1, Constant index into a declared object.** `a[3]`, `s.f`, `&a[i]` with `i` a constant. Layer 0. Free, and it is the plurality of static obligations.

**S2, The counted loop.** `for (i = 0; i < n; i++) a[i] = v;` with `a`'s extent related to `n`. Layer 2's induction recognizer. **The most important dynamic shape in C**, and if this one does not discharge reliably the specification fails on `R` regardless of what else works.

**S3, The pointer walk.** `for (p = buf; p < end; p++)`. A relation between two pointers, invisible to intervals, natural to octagons: `end - p ≤ extent`. Layer 2, and it needs `end` itself to have a proved relation to the object, which usually comes from `end = buf + n` in the same function.

**S4, The length-carrying struct.** `struct s { size_t len; char *buf; }` and `s->buf[i]` with `i < s->len`. A relation *through memory* between two fields. No numerical domain represents it. **Layer 4 with a field refinement, or an annotation, or nothing.** §6.5.

**S5, The NUL-terminated string.** `while (*p) p++;` and every `str*` function. The extent is not a number anywhere in the program; it is a property of the *contents*. §6.6.

**S6, Flexible array members.** `struct h { size_t n; T items[]; }` allocated as `malloc(sizeof(struct h) + n * sizeof(T))`. Ubiquitous in kernel and protocol code. The extent is a function of a field, established at an allocation site possibly in another translation unit. Layer 4 with `__counted_by`, which is exactly what the kernel has been annotating for.

**S7, Multidimensional and strided.** `m[i * stride + j]`. Needs a nonlinear relation (`i * stride`) that linear-arithmetic domains cannot express directly. §6.7.

**S8, The bulk operation.** `memcpy(dst, src, n)`. Two obligations of extent `n`, both at once. Discharges when both extents are known, and per [`../safe-memory/10.3`](../safe-memory/10-boundaries.md) the effects table already gives the shape (`writes(dst, n) reads(src, n)`), so the obligation generator gets these for free.

**S9, The cast-through-integer access.** Hash tables of pointers, tagged pointers, `uintptr_t` arithmetic. Provenance is `⊥` or ambiguous per J3, and there is nothing to prove against. Checked, always, and counted as an exposure.

## 6.3 Layer 2 in detail: making S2 and S3 work

Since S2 is the shape that decides `R`, its handling deserves precision.

**The induction pattern.** Recognize in the loop's SSA form: a block parameter `i` with entry value `c₀` and back-edge value `i + c₁` (`c₁` constant, sign known), and a loop guard `i < B` (or `≤`, `!=` with a monotonicity argument). Then over the body, `c₀ ≤ i < B` given `c₀ < B`, and `i < B` on every back edge.

**The extent match.** Discharge `O.hi` for `a[i]` if `B ≤ extent(a)` is provable from: a constant, a dominating check, a `__counted_by` refinement, an allocation `malloc(B * sizeof(*a))` reaching this point, or an octagon fact.

**The three failures to handle explicitly, because they are common:**

*The guard is `i != n`.* Sound only with a proof that `i` reaches `n` exactly, which needs `c₁ = 1` and `c₀ ≤ n`. Cheap to check and worth doing; `!=` guards are frequent in idiomatic C.

*The index is modified in the body.* `i` is not an induction variable and the pattern fails. Fall through to the octagon domain, which may still bound it.

*The bound is loaded from memory in the loop.* `for (i = 0; i < s->len; i++)` re-reads `s->len` each iteration and any call, or any store through a possibly-aliasing pointer, may change it. This is not a limitation of the analysis; **it is a real fact about C**, and the check must survive unless `s->len` is provably invariant. The parent's TBAA and layer 3's `nowrite` summaries are what make it provable, and the honest expectation is that this fires often in code that calls into other modules inside its loops.

**Splitting rather than failing.** Where the bound holds for all but a boundary iteration, peel. Where the bound holds under a condition testable once outside the loop (`if (n <= extent(a))`), **emit the check outside the loop and discharge inside both arms**: a loop-invariant hoist expressed as a discharge. This is [`../safe-memory/07`](../safe-memory/07-check-elimination.md)'s loop splitting, stated as a proof step, and it is the single most valuable rewrite in the layer because it converts a per-iteration cost into a per-loop cost.

## 6.4 Refinement types for C: the shape of layer 4

The [Flux](https://dl.acm.org/doi/10.1145/3591283) design, transposed. What C changes, and what it costs.

**The type language.** A refined pointer type is `ptr<T>{ν : φ(ν)}` where the refinement variable ranges over the extent, and `φ` is quantifier-free linear integer arithmetic over program variables in scope, struct fields, and refinement parameters. `char * __counted_by(n)` desugars to `ptr<char>{ν : ν = n}`; `__sized_by(n)` to a byte-extent form; `__ended_by(e)` to `ν = e - ν₀`.

**Verification conditions** are generated syntax-directed, one per obligation, in QF-LIA, and discharged in batches by the SMT solver the parent already links for [Crocus-style rule verification](../15-testing.md). No new dependency.

**Invariant inference by liquid typing.** Candidate qualifiers are drawn from the obligations in scope (for each `O.hi(a, i)` in a loop, the candidates `i < extent(a)`, `i ≤ extent(a)`, `0 ≤ i`) and the strongest conjunction of candidates that is inductive is found by the standard fixpoint over the candidate lattice. This is what gives Flux its zero-annotation result on loops, and it costs one solver call per candidate per iteration, which is why layer 4 is `deep`.

**What C takes away.** Flux gets strong updates from Rust's ownership: after `x = e`, the refinement of `x` is exactly `e`'s, because nothing else can hold a mutable alias. In C, a store through any pointer that may alias `x`'s location invalidates its refinement. Three mitigations, in order of yield:

1. **Non-escaping locals** get strong updates unconditionally, §5.3's escape analysis pays twice.
2. **TBAA** rules out stores through incompatible types, under `-fstrict-aliasing`. Not available on the kernel, which builds `-fno-strict-aliasing`; that is a real cost and it should be stated in the kernel's row rather than hidden.
3. **`restrict`** gives non-aliasing directly, and the monitor's `-fsanitize=restrict` ([`../safe-memory/09.6`](../safe-memory/09-type-init-and-races.md)) means we can *rely* on it while also checking it, a combination no existing compiler has, and a genuinely novel position: the annotation is an assumption for the prover and an obligation for the monitor simultaneously.

Where none apply, refinements are *weakly* updated (join with the old) and most of the layer's power is lost. **Expect layer 4 in C to be materially weaker than Flux in Rust**, and expect the gap to be concentrated in exactly the S4 shape it was introduced to solve.

## 6.5 The S4 shape, in full, because it is the crux

`struct s { size_t len; char *buf; }` with the invariant `extent(buf) = len`.

**With an annotation** (`char *buf __counted_by(len)`) the refinement is on the field, the invariant holds at every load of `s->buf`, and `s->buf[i]` for `i < s->len` discharges directly. This is exactly what `-fbounds-safety` was designed around and what the kernel has been annotating.

**The obligation that annotation creates**, per [`03.7.1`](03-obligations.md): every store to `s->len` or `s->buf` must re-establish the invariant. That is the assume-check duality doing real work, the annotation is not free, it relocates the check from every *read* to every *write*, and since reads outnumber writes in this shape by a large factor, the relocation is the win.

**The multi-field update problem.** `s->len = n; s->buf = malloc(n);` violates the invariant between the two statements. C has no atomic struct update. Three options, and the third is the answer:

- Forbid the intermediate state, breaks all existing code.
- Check at every use, defeats the purpose.
- **Delay: the invariant is required to hold at every point where the struct is *read through a refinement-consuming operation*, not at every program point.** Between the two stores, in straight-line code with no intervening read of `s->buf`, no obligation is generated. `-fbounds-safety` takes essentially this position and it is what makes the annotation adoptable.

**Without an annotation,** layer 4 may still infer the invariant when both fields are written together at every reaching store, which is the common case in constructor-shaped code. This is an inference and it must be *checked* like any other, per [`03.7`](03-obligations.md): the inferred field refinement generates obligations at every writer, and if some writer cannot discharge, the inference is discarded rather than assumed. [Document 09](09-inference-and-llm.md) covers proposing these at scale.

## 6.6 Strings, S5, and the honest answer

NUL-terminated strings defeat every extent-based analysis, because the extent is a property of the *contents of the buffer at run time*.

**What is available:**

- `__null_terminated` (`-fbounds-safety`'s attribute) states the property; the extent is then "up to the first NUL, which is within the allocation", and `strlen`, `strcpy` and friends get effects-table entries that are *sound given the annotation* and *checked at the boundary* by the monitor's wrapper.
- String literals have known extents and known contents. Layer 0.
- The `strn*`/`snprintf`/`strlcpy` family carry an explicit bound and are ordinary S8 bulk operations.

**What is not available:** proving `strcpy(dst, src)` safe. It requires proving `strlen(src) < extent(dst)`, which requires reasoning about the contents of `src`, which is layer 6 with a full specification, and even there, the property depends on data that came from outside.

**The position:** string obligations are checked, the monitor's interposed wrappers do the checking ([`../safe-memory/10.3`](../safe-memory/10-boundaries.md)), and this specification claims **no static discharge for the unbounded string functions at all**. That is a large residue and it is concentrated in exactly the code where CVEs live, which is a good argument for the monitor's existence and a bad one for anyone who thinks static analysis makes it unnecessary.

## 6.7 Nonlinearity, S7

`m[i * stride + j]` needs `i * stride + j < extent(m)`, and `i * stride` is nonlinear when `stride` is a variable.

**Three affordable techniques, in order:**

*Constant stride.* `stride` known at the site, very common after inlining and constant propagation. Linear again. This alone covers most 2-D array code with fixed dimensions.

*Bound multiplication.* Given `0 ≤ i < I` and `0 ≤ stride ≤ S` with `S` constant, `i * stride ≤ (I-1) * S` is a sound linearization, discharging when the product bound fits. Cheap, sound, and loses precision only when it fails to discharge.

*Solver-side nonlinearity.* Hand `i * stride + j < extent` to the SMT solver at layer 4 and accept nonlinear arithmetic's incompleteness and unpredictable time. Behind the layer's timeout, where a timeout is a check.

**The overflow interaction, which is easy to get wrong.** `i * stride` can overflow. If the index arithmetic is on `size_t`, wrapping is defined and the obligation must be proved against wrapped semantics, meaning the "obvious" linear bound is unsound. If it is signed, the parent's UB table says signed overflow does not wrap and the prover may assume it does not, *but* the monitor still needs `-fsanitize=signed-integer-overflow` to be honest about the assumption. Layer 2 and 4 both must work over machine arithmetic, not idealized integers, and the VC generator emits the bit-vector form when wrapping is possible. This is the most likely source of an actual unsoundness bug in this document and it is called out for that reason.

## 6.8 Sub-object bounds

[`../safe-memory/09.4`](../safe-memory/09-type-init-and-races.md) puts intra-object overflow detection in the type plane rather than in narrowed capabilities, so `container_of` survives. Statically, the same decomposition applies: a member access `s->f` generates `O.hi` against the *allocation's* extent, and the member-level property is an `O.type` obligation.

The consequence for this document is that layer 0 discharges member accesses trivially and correctly, and that `-fsafety-subobject`'s extra precision is a type-plane matter handled in §6.9 rather than a bounds matter. This is a place where the monitor's design choice made the prover's job *easier*, which is worth noting because the alternative (CHERI-style narrowing) would have generated a hard obligation at every `container_of`.

## 6.9 Init and type obligations

Together 27% of the count per [`03.2.1`](03-obligations.md), with no other home in this document set, and both are more tractable than they look.

**`O.init`.** A load's initializedness is a classic dataflow property: definite-assignment analysis over the dominator tree, per byte range. Layer 1 discharges the straight-line and single-block cases; layer 2 discharges loop-filled buffers where the fill's induction range covers the read's index, the same induction machinery as S2, reused. `calloc`, `memset` and zero-initialized statics discharge whole objects at layer 0.

The residue is the interesting part and it is small: partially-initialized structs (padding aside, per [`../safe-memory/09.3`](../safe-memory/09-type-init-and-races.md)), buffers filled by an uninstrumented callee, and `read(2)`-shaped fills where the initialized extent is the return value. The last of these is a *refinement on the return value* (`ssize_t read(...)` returning `r` initializes `[buf, buf+r)`) and belongs in the effects table.

**Init-plane elision is unusually achievable**, because the init plane is written on *every store* and read on *every load*, so a fully-discharged object drops a plane write per store. Objects that are zeroed at allocation and never partially written (the majority) should elide entirely. **This may be the largest single contributor to `R`**, and it is worth measuring before anything harder is attempted.

**`O.type`.** Under `-fstrict-aliasing`, discharge is nearly free at layer 0: an access through a pointer whose static type has not been laundered is type-compatible by construction, and the parent's TBAA already computes exactly this. The residue is casts, unions, `memcpy`-based type punning, and everything the kernel does, which builds `-fno-strict-aliasing` and therefore, per [`../safe-memory/09.1`](../safe-memory/09-type-init-and-races.md), runs with Y2/Y3 off anyway.

So: **`O.type` discharges well in userspace and is largely moot in the kernel**, which is a comfortable place to be. What remains is `-fsafety-subobject`'s member-distinguishing form, where the obligation is that an access to member `f` stays within `f`, and that *is* statically dischargeable at layer 0 for direct member access, which is most member accesses. The hard residue is pointer arithmetic within a struct, which is rare outside deliberate punning.

## 6.10 Claims

**B1.** Layers 0-2 discharge **≥ 70% of static `O.lo` + `O.hi`** on the tier-1 corpus. (This is [`02.6`](02-the-goal.md)'s C1, restated where it is earned.)

**B2.** Layer 2 discharges **≥ 80% of `O.hi` in loops matching S2 with a locally-visible extent.** If this fails, `R` fails.

**B3.** Layer 4 with `__counted_by` annotations present discharges **≥ 60% of S4 and S6 obligations**, measured on the kernel where the annotations already exist.

**B4.** Init-plane elision covers **≥ 50% of allocated bytes** at `default`. The cheapest large contribution to `R` in the specification, and the first one to measure.

**B5.** No static discharge is claimed for S5's unbounded string functions or S9's synthesized pointers, at any level. Stated as a claim so that a future version claiming otherwise has to argue for it.
