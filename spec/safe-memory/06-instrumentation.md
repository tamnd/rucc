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

Nineteen of them. The names are written with underscores because the textual IR of the parent's document 08 keeps the dot for the type suffix and the flags, so `cap.of` would read back as the opcode `cap` with a type suffix `of` and there is no such type. Every other multi-word opcode in that set is spelled the same way, `global_addr` and `indirect_br` and `va_start` among them.

**Capability production and movement.**

```
%c = cap_of %p                          capability of a pointer value, from the pointer's provenance
%c = cap_load %p                        capability from the aux slot for the pointer stored at %p
       cap_store %p, %c                 write a capability into the aux slot for %p
%c = cap_null                           ⊥
%c = cap_narrow %c0, %off, %len         sub-object narrowing; -fsafety-subobject only
%c = cap_recover %p                     boundary recovery from the shadow planes; document 05 section 5.3
```

**Checks.** Each corresponds to one conjunct of J1 in document 04. They are separate instructions rather than one fused check so that they can be discharged independently, the common case is that bounds survives and everything else is proved.

```
check_bounds %c, %p, size n, align a    J1 bounds + alignment + permission
check_live   %c, %p                     J1 lifetime version
check_type   %c, %p, size n, tbaa !k    J1 type-plane compatibility
check_init   %c, %p, size n             J1 initialization
check_deriv  %c, %p, %newp              J2, at the point of derivation
check_race   %c, %p                     C1/C3, metadata epoch
```

The size and the alignment are written after the operands the way the parent's document 08 writes them on a `load`, because they are what the front end knew about the access rather than anything the program computed.

**Plane maintenance.** These are the writes that keep the planes true and they are *not* removable by the optimizer except by the rules in document 07 section 7.6, because removing one makes a later check wrong rather than merely slower.

```
meta_begin %p, %size, class c           J4, with c one of section 4.1's eight storage classes
meta_end   %p, %size                    J5
meta_type  %p, %size, tbaa !k           set the type plane
meta_init  %p, %size                    set the init plane
meta_transfer %p, %size, to o           J7, with o one of device, uninstrumented, kernel
```

The length is an operand and not a payload, because a variable length array has one the front end cannot know. The class and the owner are names out of a closed set rather than metadata nodes, for the same reason an atomic ordering is: there are eight of one and three of the other, the reader can turn down a ninth, and a metadata node would let anything through and find out later.

**Regions.**

```
safe_region_begin "hand written assembly, checked by review"
safe_region_end
```

Delimits a declared exemption. The reason is a string and not a metadata node, because document 10 section 10.2 counts these per build and prints them, so it is something a reviewer reads rather than something a pass keys on.

Neither takes an operand. What they say is about the code between them, not about any value, and that is also why they are ordered with respect to memory the way the accesses they bracket are: a region that could be moved would not be delimiting anything.

### 6.2.3 A metadata node kind

The parent's document 08 has one kind of metadata node, the type based aliasing node, written `!0 = tbaa "int", parent !1, offset 0`. The type plane needs a second, because its vocabulary is the types plus the three distinguished values of document 09 section 9.1 and there is no aliasing node for "nobody has stored here yet":

```
!1 = plane !0                   the type that aliasing node is
!2 = plane no_type              nothing has stored here, or it came from an untyped source
!3 = plane character            stored through a character type
!4 = plane pointer_slot 3       byte 3 of a pointer shaped word
```

A plane entry that names a type names the aliasing node the front end already interned, which is what document 15 section 15.1 means by type-plane facts travelling as opaque ids: the plane's vocabulary is exactly the compiler's, `rucc-opt` compares entries for equality without asking `rucc-types` anything, and a report can name a type in the spelling the source used.

The two kinds share one table and one numbering, because both are the same interned type universe seen from a different side and a reader chasing a `!3` should not have to know which table it came out of. `check_type` and `meta_type` name a plane entry and every other node reference names an aliasing node, which the verifier checks, since an aliasing query over `character` would mean nothing and a walk up a plane entry would find no parent.

The compatibility relation the checks consult is not here. It is data attached to the module, per document 15 section 15.1, and it is written down in milestone S5 along with the pass that reads it.

### 6.2.4 Attributes and facts

The parent's document 08 section 8.4 already carries `noalias` and `provenance` on pointer values. Four more, and they are *facts* rather than instructions, which is what makes elimination a dataflow problem rather than a special pass:

```
!bounds(lo, ext)        this pointer is known to lie within these bounds
!live                   this pointer's provenance is known live at this program point
!init(n)                the n bytes at this pointer are known initialized
!aligned(a)             known alignment
```

Facts are produced by checks (a `check_bounds` that survives establishes `!bounds` on its pointer for its dominated region) and consumed by the elimination rules. This is deliberately the same shape as `nsw`/`nuw`: a fact the optimizer may assume, established somewhere, exploited elsewhere.

They are written at the end of a function body, one line per value, because a fact is about a value everywhere it is live rather than at the point it was made, and because a block parameter and an instruction result would otherwise need two spellings of the same thing:

```
facts:
    %0 = !bounds(%0, %1), !live, !init(4), !aligned(8)
    %8 = !aligned(4)
```

The two halves of a range are values and not numbers, since the range of a heap allocation is not known until it is made. They have to reach the value the fact is about, everywhere that value does, which is the rule that a range named by something computed later would break.

The facts live in a side table on the function rather than in the value, so a function nobody has said anything about carries no facts and prints exactly as it did before facts existed. That is section 6.2's constraint that safety off costs nothing, applied to the one part of this that is not an instruction.

### 6.2.5 Effects, and the ægraph problem

Checks trap. That makes them **control-dependent side effects** and it is the single most awkward interaction in this specification.

The parent's document 09 pins control flow in a CFG skeleton and forbids the ægraph from rewriting it. A trapping instruction cannot be sunk past a point where the program might otherwise exit, cannot be hoisted above a branch that guards it, and cannot be duplicated without duplicating the trap. Two checks on the same pointer are *not* interchangeable if a store between them could change the answer.

The specification:

**Checks live in the CFG skeleton, not in the e-graph value soup.** Their *operands* (the capability, the pointer, the computed address) are e-graph values and are optimized normally. The check instruction itself is pinned to its block and ordered with respect to other effectful operations, exactly as `load` and `store` are.

**A check is `may_trap` and `readonly`.** It reads the planes and nothing else. It may therefore be eliminated (it has no effect if it does not trap) but not reordered across anything that could change a plane, and not hoisted above a branch unless the branch is proved not to be the thing that made the check safe. Document 07's rules are written against exactly this.

**Redundant-check elimination is a dominator-tree walk, not an e-graph rewrite.** This is the honest consequence of the CFG-skeleton restriction and it means the parent's document 19 question one (does the ægraph carry from a Wasm JIT to an AOT C compiler) has a corollary here: even if the answer is yes, checks are outside it. Document 17 question 2.

What the e-graph *does* buy us is the arithmetic: `(addr - lo) <u ext - n` is an expression, its subexpressions are shared with the address computation the program was doing anyway, and canonicalizing them is exactly what an e-graph is good at. Document 07 section 7.4 shows the case where a loop's induction variable and its bounds check collapse into one comparison.

## 6.3 Where checks are placed

Insertion runs in `rucc-safety` on the IR produced by `rucc-lower`, before `rucc-opt`. It is a single walk.

**Every `load` and `store`** gets `check_bounds`, and at the enabled tier's plane set, `check_live`, `check_type`, `check_init`. The capability comes from `cap_of` on the pointer operand, which the fact propagation resolves to a concrete capability where the pointer's provenance is statically known, which for stack and global accesses is almost always.

**Every pointer-typed `store`** additionally gets a `cap_store` writing the aux slot, and every pointer-typed `load` a `cap_load`. This is the aux traffic that document 05 warns is the real cost.

**Every `getelementptr`-equivalent address computation** gets `check_deriv`, except where the offset is a constant that provably keeps the result in `[lo, hi]`, which is the majority and is folded at insertion time rather than left for the optimizer.

**Every `alloca`** gets `meta_begin` at its definition and `meta_end` at every scope exit including every unwind edge. Address-taken locals whose address escapes are additionally promoted per document 08 section 8.5.

**Every `call`** to an uninstrumented target gets `meta_transfer` for the pointer arguments it hands over, per document 10.

**`memcpy`, `memmove`, `memset` and the string builtins** get range checks on both operands and a type-plane operation: `memcpy` sets the destination's effective type to the source's, per C 6.5's `memcpy` rule, which is what makes document 03's `memcpy`-punning idiom work rather than fire.

### 6.3.1 The three bounds-check forms

Document 05 chose base-and-extent so that the hot check is one unsigned compare:

```
check_bounds %c, %p, n     ⟶     %d = sub %p, %c.lo
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
- **The allocation site** of the storage instance, and for a temporal violation, **the deallocation site**: both recorded in the instance header at `meta_begin` and `meta_end`, which is the single most useful field in a use-after-free report and is why the header is 32 bytes rather than 24.
- For a type violation, both types by their sugar spelling, per the parent's document 07 section 7.1.
- For a race, the other thread and its last-write location.

Output is human-readable by default and JSON under `-fsafety-report=json`, sharing the parent's `rucc-diag` schema so the same tooling consumes both.

Behavior on violation is `-fsafety-on-error=abort|continue|log`. `abort` is the Fil-C posture and the correct default for Tier E. `continue` is what a corpus run wants, so that one bug does not hide a hundred, and it requires the monitor to define a recovery: the access is performed as written, the planes are updated as if it were legal, and the report is deduplicated by descriptor id. That is unsound as an enforcement posture and is exactly right as a detection posture, which is the tier distinction doing its job.

## 6.6 The malformed forms that are rejected

The soundness argument in document 14 assumes the IR cannot express a check that means nothing. That assumption is only worth something if the forms it rules out are written down, because a rule that lives only in the verifier's source is a rule nobody can check the verifier against. This is that list. Every row has a test, the test is named after the form, and a row without a test is a bug rather than a plan.

Two of these matter more than the rest and are worth saying out loud. A check handed the same value as both its capability and its pointer would pass, always, because a pointer is inside its own bounds however wrong those bounds are, and it would look exactly like a check that worked. A `check_type` handed an aliasing node instead of a plane entry would ask a question about the wrong table and get an answer. Neither is a crash; both are silent, and silent is the failure mode the whole design is trying not to have.

**Capabilities.**

| The form | Why it is not allowed | Test |
| --- | --- | --- |
| A pointer where a capability belongs | The two are separate operands so that neither can stand in for the other | `a_pointer_where_a_capability_belongs_is_reported` |
| A capability where a pointer belongs | The same rule from the other side | `a_capability_where_a_pointer_belongs_is_reported` |
| A capability instruction with the wrong operand count | An operand nobody reads is an operand somebody meant | `a_capability_instruction_with_the_wrong_number_of_operands_is_reported` |
| `cap_null` given an operand | It describes nothing, so a pointer beside it is a pointer somebody believed it was about | `a_null_capability_handed_an_operand_is_reported` |
| `cap_narrow` given an offset and a length of different widths | One of the two would be extended, and which one changes the answer | `narrowing_by_an_offset_and_a_length_of_different_widths_is_reported` |
| Something that is not a capability instruction producing a `cap` | A capability has to come from somewhere nameable or the provenance argument has a hole in it | `an_instruction_that_is_not_a_cap_instruction_producing_one_is_reported` |
| A `cap` in a function's parameters | Section 6.2.1: capabilities do not cross a call, they are recovered on the other side | `a_capability_parameter_is_reported` |
| A `cap` in a function's results | The same | `a_capability_result_is_reported` |

**Checks.**

| The form | Why it is not allowed | Test |
| --- | --- | --- |
| A check given its pointer and its capability the other way round | It would compare the two fields of the wrong thing | `a_check_given_its_pointer_and_its_capability_the_other_way_round_is_reported` |
| A check on the same value twice | It passes and means nothing, which is worse than failing | `a_check_on_the_same_value_twice_is_reported` |
| `check_deriv` without the pointer it is about | The judgement is about where the derived pointer landed, so there is nothing to decide without it | `a_derivation_check_without_the_pointer_it_is_about_is_reported` |
| `check_type` naming an aliasing node | The two node kinds share one numbering, so the kind is the only thing that says a reference is to the right table | `a_type_check_handed_an_aliasing_node_is_reported` |

**Plane writes and regions.**

| The form | Why it is not allowed | Test |
| --- | --- | --- |
| A plane write over a length that is not a number | The length is an operand because a variable length array has one the front end cannot know, and an operand can be the wrong type | `a_plane_write_over_a_length_that_is_not_a_number_is_reported` |
| A `load` or a `store` naming a plane entry | The mirror of the `check_type` rule, in the direction that would make an aliasing query out of `character` | `a_load_handed_a_plane_entry_is_reported` |
| A region marker given an operand | Section 6.2.2: the markers are about the code between them and not about any value | `a_region_marker_handed_a_value_is_reported` |

**Facts.**

| The form | Why it is not allowed | Test |
| --- | --- | --- |
| A fact about something that is not a pointer | Every one of the four says something about an address | `a_fact_about_something_that_is_not_a_pointer_is_reported` |
| An alignment that is not a power of two | The lowered check is a mask, and a mask of six is not a test for divisibility by six | `an_alignment_that_is_not_a_power_of_two_is_reported` |
| A range that starts somewhere that is not a pointer | Where a range starts is an address, and a number there answers a different question | `a_range_that_starts_somewhere_that_is_not_a_pointer_is_reported` |
| A range whose extent is not a number | How long it is is a count of bytes | `a_range_whose_extent_is_not_a_number_is_reported` |
| A range named by something computed after the pointer it is about | The fact is about the value everywhere it is live, so half its uses could not name the range | `a_range_computed_after_the_pointer_it_is_about_is_reported` |

**Metadata nodes.**

| The form | Why it is not allowed | Test |
| --- | --- | --- |
| A plane entry for a byte no pointer on the target has | It says which byte of a pointer it is, so byte eight of an eight byte pointer is a plane nothing could write | `a_plane_entry_for_a_byte_no_pointer_has_is_reported` |
| A plane entry naming another plane entry | Section 6.2.3: an entry names a type, and a type is an aliasing node | `a_plane_entry_naming_another_plane_entry_is_reported` |
| A metadata node naming one that does not come before it | The one rule that makes the table a forest and not a graph | `a_metadata_node_that_is_its_own_parent_is_reported` |

**Rejected before the verifier sees them.** The textual IR is a format as well as a debugging aid, so the parser turns down what it can rather than building something for the verifier to complain about. A fact name nobody has heard of (`a_fact_nobody_has_heard_of_is_turned_down`), a value given facts twice (`a_value_said_twice_is_turned_down`), facts about a value that does not exist (`a_fact_about_a_value_that_does_not_exist_is_turned_down`), a plane entry of an unknown kind (`a_plane_entry_nobody_has_heard_of_is_turned_down`), a metadata node of an unknown kind (`a_metadata_node_of_no_known_kind_is_turned_down`), a plane entry naming a node written later (`a_plane_entry_naming_a_node_that_comes_later_is_turned_down`), and a reference to a node the module never defines (`metadata_nobody_defines_is_reported`).

The verifier repeats the last of those rather than trusting the parser, because IR built by a pass never went through the parser and a rule that only the parser enforces is a rule that holds for text and not for the compiler.
