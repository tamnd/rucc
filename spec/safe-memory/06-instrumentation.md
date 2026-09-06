# Instrumentation: checks as IR

The single most consequential decision in this specification is *when* checks are inserted relative to the optimizer, and this document makes it. Everything about the cost budget in document 13 follows from it.

## 6.1 The decision, and why it is different from every existing tool

AddressSanitizer instruments late in the pipeline, after the optimizer has run, specifically so that the optimizer cannot delete its checks. TySan goes further: [it disables TBAA for alias analysis while it is active](https://clang.llvm.org/docs/TypeSanitizer.html), giving up the optimization so that the accesses it needs to observe are not removed. Both are the same concession, and both are forced by the same structural fact, the instrumentation and the optimizer belong to different worlds, and the optimizer does not know what a check is.

The consequence is that a sanitizer's checks are *never optimized*. Every one is emitted, every one executes, and the entire cost of the tool is the naive cost. ASan pays 2x for spatial safety that a good compiler could largely prove.

**We insert checks as first-class IR before the middle end, and we let the optimizer discharge them.** The optimizer knows what a check is because the check is an instruction with defined semantics in the parent's [document 08](../08-ir.md) IR, and because the facts that discharge it (bounds, provenance, liveness, initialization) are attributes the IR already carries or is extended here to carry.

The obvious hazard is equally structural: an optimizer that can delete a check can delete a check it should not have, and the failure mode is not a wrong answer but a *silent loss of safety*, which is strictly worse because nothing observes it. The defence is that check elimination happens only through the rewrite-rule DSL of the parent's document 09, whose rules are data, and every check-eliminating rule is SMT-verified with the same `rucc-verify` machinery the parent's document 10 already builds for instruction selection following [Crocus](https://cs.wellesley.edu/~avh/veri-isle-preprint.pdf). Document 07 specifies the rules and document 14 specifies the verification.

That is the whole argument for doing this in `rucc` rather than as an LLVM pass, and it is why this sub-specification could not have been written against a compiler that did not already have a verified rule DSL.

## 6.2 The IR extension

Additions to the parent's document 08. The design constraint is that an IR module with no safety instructions must be bit-identical to today's, so that everything downstream is unaffected when safety is off.

### 6.2.1 A value type

```
cap             an opaque capability value; 4 machine words when materialized
```

`cap` is opaque in the same sense and for the same reasons `ptr` is opaque in the parent's document 08 section 8.2: its representation is document 05's business and nothing in the optimizer may depend on it. It has no load or store; capabilities move between registers and the aux plane only through the instructions below.

### 6.2.2 Instructions

**Capability production and movement.**

```
%c = cap.of %p                          capability of a pointer value, from the pointer's provenance
%c = cap.load %p                        capability from the aux slot for the pointer stored at %p
       cap.store %p, %c                 write a capability into the aux slot for %p
%c = cap.null                           ⊥
%c = cap.narrow %c0, %off, %len         sub-object narrowing; -fsafety-subobject only
%c = cap.recover %p                     boundary recovery from the shadow planes; document 05 section 5.3
```

**Checks.** Each corresponds to one conjunct of J1 in document 04. They are separate instructions rather than one fused check so that they can be discharged independently, the common case is that bounds survives and everything else is proved.

```
check.bounds %c, %p, size, align        J1 bounds + alignment + permission
check.live   %c, %p                     J1 lifetime version
check.type   %c, %p, size, !tbaa        J1 type-plane compatibility
check.init   %c, %p, size               J1 initialization
check.deriv  %c, %p, %newp              J2, at the point of derivation
check.race   %c, %p                     C1/C3, metadata epoch
```

**Plane maintenance.** These are the writes that keep the planes true and they are *not* removable by the optimizer except by the rules in document 07 section 7.6, because removing one makes a later check wrong rather than merely slower.

```
meta.begin %p, %size, !class            J4
meta.end   %p, %size                    J5
meta.type  %p, %size, !tbaa             set the type plane
meta.init  %p, %size                    set the init plane
meta.transfer %p, %size, !to            J7
```

**Regions.**

```
safe.region.begin !reason
safe.region.end
```

Delimits a declared exemption. Document 10 counts them.

### 6.2.3 Attributes and facts

The parent's document 08 section 8.4 already carries `noalias` and `provenance` on pointer values. Four more, and they are *facts* rather than instructions, which is what makes elimination a dataflow problem rather than a special pass:

```
!bounds(lo, ext)        this pointer is known to lie within these bounds
!live                   this pointer's provenance is known live at this program point
!init(n)                the n bytes at this pointer are known initialized
!aligned(a)             known alignment
```

Facts are produced by checks (a `check.bounds` that survives establishes `!bounds` on its pointer for its dominated region) and consumed by the elimination rules. This is deliberately the same shape as `nsw`/`nuw`: a fact the optimizer may assume, established somewhere, exploited elsewhere.

### 6.2.4 Effects, and the ægraph problem

Checks trap. That makes them **control-dependent side effects** and it is the single most awkward interaction in this specification.

The parent's document 09 pins control flow in a CFG skeleton and forbids the ægraph from rewriting it. A trapping instruction cannot be sunk past a point where the program might otherwise exit, cannot be hoisted above a branch that guards it, and cannot be duplicated without duplicating the trap. Two checks on the same pointer are *not* interchangeable if a store between them could change the answer.

The specification:

**Checks live in the CFG skeleton, not in the e-graph value soup.** Their *operands* (the capability, the pointer, the computed address) are e-graph values and are optimized normally. The check instruction itself is pinned to its block and ordered with respect to other effectful operations, exactly as `load` and `store` are.

**A check is `may_trap` and `readonly`.** It reads the planes and nothing else. It may therefore be eliminated (it has no effect if it does not trap) but not reordered across anything that could change a plane, and not hoisted above a branch unless the branch is proved not to be the thing that made the check safe. Document 07's rules are written against exactly this.

**Redundant-check elimination is a dominator-tree walk, not an e-graph rewrite.** This is the honest consequence of the CFG-skeleton restriction and it means the parent's document 19 question one (does the ægraph carry from a Wasm JIT to an AOT C compiler) has a corollary here: even if the answer is yes, checks are outside it. Document 17 question 2.

What the e-graph *does* buy us is the arithmetic: `(addr - lo) <u ext - n` is an expression, its subexpressions are shared with the address computation the program was doing anyway, and canonicalizing them is exactly what an e-graph is good at. Document 07 section 7.4 shows the case where a loop's induction variable and its bounds check collapse into one comparison.

## 6.3 Where checks are placed

Insertion runs in `rucc-safety` on the IR produced by `rucc-lower`, before `rucc-opt`. It is a single walk.

**Every `load` and `store`** gets `check.bounds`, and at the enabled tier's plane set, `check.live`, `check.type`, `check.init`. The capability comes from `cap.of` on the pointer operand, which the fact propagation resolves to a concrete capability where the pointer's provenance is statically known, which for stack and global accesses is almost always.

**Every pointer-typed `store`** additionally gets a `cap.store` writing the aux slot, and every pointer-typed `load` a `cap.load`. This is the aux traffic that document 05 warns is the real cost.

**Every `getelementptr`-equivalent address computation** gets `check.deriv`, except where the offset is a constant that provably keeps the result in `[lo, hi]`, which is the majority and is folded at insertion time rather than left for the optimizer.

**Every `alloca`** gets `meta.begin` at its definition and `meta.end` at every scope exit including every unwind edge. Address-taken locals whose address escapes are additionally promoted per document 08 section 8.5.

**Every `call`** to an uninstrumented target gets `meta.transfer` for the pointer arguments it hands over, per document 10.

**`memcpy`, `memmove`, `memset` and the string builtins** get range checks on both operands and a type-plane operation: `memcpy` sets the destination's effective type to the source's, per C 6.5's `memcpy` rule, which is what makes document 03's `memcpy`-punning idiom work rather than fire.

### 6.3.1 The three bounds-check forms

Document 05 chose base-and-extent so that the hot check is one unsigned compare:

```
check.bounds %c, %p, n     ⟶     %d = sub %p, %c.lo
                                  %l = sub %c.ext, n
                                  %ok = icmp ule %d, %l
                                  br %ok, cont, trap
```

This is correct including for wraparound: if `%p < %c.lo` then `%d` is huge and the unsigned compare fails, and `%c.ext - n` underflows only when `n > ext`, which the frontend rejects for a fixed-size access and which the dynamic case guards with a separate `n <= ext` test. Two additional forms exist for the cases this misses, following Fil-C's analysis: when `n` is a runtime value that can itself overflow, and when the access is alignment-checked and the alignment guarantees the last-byte test. `rucc-safety` selects among them and the selection is one of the SMT-verified rules.

## 6.4 Interaction with the rest of the pipeline

**mem2reg runs after insertion.** A local promoted to a register has no address, no aux slot and no checks, and this is where the majority of Tier E's savings come from before the optimizer proper does anything. The parent's `-O0` pipeline already runs mem2reg and nothing else, which conveniently means Tier E at `-O0` is not absurd.

**Inlining runs after insertion**, which is what makes interprocedural check elimination work at all: the caller's established facts meet the callee's checks in the same function and document 07's dominator walk discharges them. This is the same reason `-fbounds-safety`'s implicit wide pointers for locals work, the bound is visible where the access is.

**LTO** carries the facts across modules through the summaries in the parent's document 09 section 9.8. A `__counted_by`-annotated parameter is a fact in a summary.

**The backend** sees ordinary comparisons and branches. The trap target is a call to `__rucc_safety_fail` with a static descriptor id; there is no per-check code beyond the compare and the branch, and the descriptors live in a `.rucc_safety_desc` section that the reporter in `rucc-safe-rt` reads. This keeps the hot path two instructions and the cold path arbitrarily detailed, which is what makes good diagnostics affordable.

**Register allocation** is where the capability's four words hurt. The parent's document 10 already specifies a backtracking allocator with live-range splitting for `-O1` and above, which is the correct tool, and document 13 measures spills as a first-class metric because if capability materialization causes spilling in hot loops then no amount of check elimination saves us.

## 6.5 Diagnostics

A memory-safety report that does not say what the program did is worth very little, and this is the part of ASan that made it succeed. `__rucc_safety_fail` produces:

- The judgement violated, in document 04's numbering, and the document 03 class.
- The faulting address, the capability's bounds and version, and the plane's version.
- The source location of the access, from the parent's document 11 DWARF.
- **The allocation site** of the storage instance, and for a temporal violation, **the deallocation site**: both recorded in the instance header at `meta.begin` and `meta.end`, which is the single most useful field in a use-after-free report and is why the header is 32 bytes rather than 24.
- For a type violation, both types by their sugar spelling, per the parent's document 07 section 7.1.
- For a race, the other thread and its last-write location.

Output is human-readable by default and JSON under `-fsafety-report=json`, sharing the parent's `rucc-diag` schema so the same tooling consumes both.

Behavior on violation is `-fsafety-on-error=abort|continue|log`. `abort` is the Fil-C posture and the correct default for Tier E. `continue` is what a corpus run wants, so that one bug does not hide a hundred, and it requires the monitor to define a recovery: the access is performed as written, the planes are updated as if it were legal, and the report is deduplicated by descriptor id. That is unsound as an enforcement posture and is exactly right as a detection posture, which is the tier distinction doing its job.
