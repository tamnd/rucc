# 37. Machine-level optimization

Everything between instruction selection and the assembler. In GCC that is the RTL pipeline, and it
is larger than most people expect: counting from `pass_expand` to the end of `passes.def` there are
98 `NEXT_PASS` entries, of which roughly eighty are real passes and the rest are containers.

The sizes: `gcc/combine.cc` 15,297, `gcc/haifa-sched.cc` 9,294, `gcc/cse.cc` 7,838, `gcc/ifcvt.cc`
6,539, `gcc/sched-deps.cc` 5,025, `gcc/gcse.cc` 4,438, `gcc/sched-rgn.cc` 3,981, `gcc/modulo-sched.cc`
3,386, `gcc/cfgcleanup.cc` 3,339, `gcc/pair-fusion.cc` 3,145, `gcc/bb-reorder.cc` 3,085,
`gcc/early-remat.cc` 2,638, `gcc/postreload.cc` 2,477, `gcc/loop-invariant.cc` 2,331,
`gcc/regrename.cc` 2,054, `gcc/cprop.cc` 2,003, `gcc/lower-subreg.cc` 1,903, `gcc/auto-inc-dec.cc`
1,751, `gcc/regcprop.cc` 1,538, `gcc/postreload-gcse.cc` 1,470, `gcc/ree.cc` 1,433,
`gcc/ext-dce.cc` 1,401, `gcc/mode-switching.cc` 1,335, `gcc/dce.cc` 1,304, `gcc/fwprop.cc` 1,080,
`gcc/compare-elim.cc` 981, `gcc/late-combine.cc` 949, `gcc/fold-mem-offsets.cc` 934,
`gcc/avoid-store-forwarding.cc` 719. Plus `gcc/rtl-ssa/`, 5,775 lines of implementation and 3,832 of
headers.

Scheduling and layout are document 38's and register allocation is document 39's. This document is
the rest, and its central question is which of these eighty passes rucc needs, given that the answer
is emphatically not all of them.

## 37.1 The repetition, and what it means

Read the pass list and the first thing that stands out is how much of it is the GIMPLE pipeline
again. Between `pass_expand` and `pass_ira`:

| Optimization | RTL instances |
|---|---|
| Common subexpression elimination | `pass_cse`, `pass_cse_after_global_opts`, `pass_cse2`, `pass_postreload_cse` |
| Copy propagation | `pass_rtl_cprop` three times, plus `pass_cprop_hardreg` after reload |
| Partial redundancy elimination | `pass_rtl_pre`, `pass_gcse2` |
| Code hoisting | `pass_rtl_hoist` |
| Store motion, dead store elimination | `pass_rtl_store_motion`, `pass_rtl_dse1`, `pass_rtl_dse2` |
| Dead code elimination | `pass_ud_rtl_dce`, `pass_ext_dce`, `pass_fast_rtl_dce` |
| Loop invariant motion | `pass_rtl_move_loop_invariants` |
| Unrolling | `pass_rtl_unroll_loops` |
| If-conversion | `pass_rtl_ifcvt`, `pass_if_after_combine`, `pass_if_after_reload` |
| Jump optimization | `pass_jump`, `pass_jump_after_combine`, `pass_jump2` |
| Combining | `pass_combine`, `pass_late_combine` twice |

Every one of these has a GIMPLE counterpart that already ran. **The repetition is not redundancy, and
understanding why is the most useful thing in this document**, because it determines what rucc must
have below selection and what it can leave to the middle end.

Three reasons, and they are different in kind.

**Expansion creates new expressions the middle end never saw.** An array reference becomes a shift, an
add, and a load. Two references to `a[i]` and `a[i+1]` share the address computation, and that sharing
is not visible in GIMPLE where both are single `MEM_REF`s. Post-expansion CSE is therefore finding
real redundancy in real code, and it is redundancy the tree level could not have found.

**RTL exposes target detail that changes the answer.** A 64-bit constant on AArch64 is up to four
instructions; whether hoisting it out of a loop is profitable depends on that. `subreg`s,
sign-extension patterns, condition-code registers and addressing-mode legality are all invisible
above selection.

**Register allocation creates a third wave.** Spills and reloads are loads and stores that nothing
above generated. `pass_postreload_cse`, `pass_gcse2`, `pass_rtl_dse2` and `pass_cprop_hardreg` exist
to clean up after the allocator, and `pass_ree`, redundant extension elimination, is largely about
extensions introduced by reload.

**Now the rucc reading.** Reason one is genuine and rucc is exposed to it exactly as GCC is: the
addressing-mode arithmetic that selection materialises is redundant across instructions and nothing
above saw it. Reason two is genuine. Reason three is genuine.

But the *magnitude* is not the same, and the reason is document 36.3's. GCC re-runs the full
optimizer at RTL partly because RTL is not in SSA form, so each pass rebuilds its own dataflow, and
partly because expansion is a tree walker that generates locally and leaves obvious redundancy for
somebody else. rucc's selector matches over a DAG that has already been through the e-graph, its
addressing modes are folded by rules rather than by a later pass, and its MIR is in SSA. **So rucc
does not need a second copy of the middle end below selection. It needs a small number of genuinely
machine-level passes, and this document's job is to name them.**

## 37.2 RTL SSA, which is GCC agreeing

`gcc/doc/rtl.texi:4382` introduces it:

> The patterns of an individual RTL instruction describe which registers are inputs to that
> instruction and which registers are outputs from that instruction. However, it is often useful to
> know where the definition of a register input comes from and where the result of a register output
> is used. One way of obtaining this information is to use the RTL SSA form, which provides a Static
> Single Assignment representation of the RTL instructions.

It is an on-the-side SSA form: 5,775 lines of implementation plus 3,832 of headers, with its own phi
nodes, access lists, and a change-application framework. The passes that have been converted to it,
found by searching for `using namespace rtl_ssa`, are `gcc/fwprop.cc`, `gcc/late-combine.cc`,
`gcc/pair-fusion.cc`, `gcc/config/aarch64/aarch64-early-ra.cc`,
`gcc/config/aarch64/aarch64-narrow-gp-writes.cc`, `gcc/config/riscv/riscv-vsetvl.cc` and
`gcc/config/riscv/riscv-avlprop.cc`.

Note what that list is: **every recently written RTL pass uses it, and no old one does.** GCC is
retrofitting SSA onto its machine-level IR, pass by pass, over years, because writing machine-level
optimizations without def-use chains is the thing that made `gcc/combine.cc` 15,297 lines long.

Spec 10.1's decision to keep MIR in SSA until register allocation is therefore not a novelty, it is
where GCC is going at considerable expense. Recording that is worth more than the decision itself,
because it converts "we chose this" into "the incumbent is paying 9,607 lines to get here".

The second thing to take from `rtl-ssa` is its shape. It has a change-application framework,
`gcc/rtl-ssa/changes.cc` (1,343 lines), whose job is to let a pass propose a set of instruction
changes, ask whether they are all valid together, and commit or abandon them atomically. That is the
right interface for a machine-level rewrite, because a combine-style transformation is speculative by
nature: substitute, ask the matcher whether the result is a legal instruction, and back out if not.
**rucc needs the same thing and should build it as a named component rather than letting each pass
invent its own undo.**

## 37.3 Combine

`gcc/combine.cc:20` describes itself, and the description is the clearest statement of what a
machine-level optimizer is for:

> The LOG_LINKS of each insn identify the most recent assignment to each REG used in the insn... We
> try to combine each pair of insns joined by a logical link. We also try to combine triplets of insns
> A, B and C when C has a link back to B and B has a link back to A. Likewise for a small number of
> quadruplets... Combination is done by mathematically substituting the previous insn(s) values for
> the regs they set into the expressions in the later insns that refer to these regs. If the result is
> a valid insn for our target machine, according to the machine description, we install it, delete the
> earlier insns, and update the data flow information.

Three points.

**The mechanism is substitute-then-ask-the-matcher.** Combine does not know what instructions the
target has; it produces a candidate RTL expression and asks `recog`. This is why the machine
description is the single source of truth and why adding a pattern to the `.md` file makes combine
smarter without anyone editing combine. It is the strongest argument in GCC's design for keeping
target knowledge as data.

**The window is bounded and the bound is a parameter.** `max-combine-insns` at `gcc/params.opt:537`
is `Init(4)` with `IntegerRange(2, 4)`, and `max-combine-search-insns` at :541 is `Init(3000)`. Four
instructions is the entire depth of GCC's most important machine-level optimization. That is
reassuring for anyone sizing rucc's peephole set: the useful window is small.

**LOG_LINKS never cross basic blocks.** Combine is block-local. Every one of those 15,297 lines
operates within a single block.

`gcc/late-combine.cc:20` states the newer, narrower pass's two purposes:

> - to substitute definitions into all uses, so that the definition can be removed.
> - to try to parallelise sets of condition-code registers with a related instruction...
>
> The pass can run before or after register allocation. When running before register allocation, it
> tries to avoid cases that are likely to increase register pressure. For the same reason, it avoids
> moving instructions around... These limitations are removed when running after register allocation.

That last paragraph is the general rule for this whole document and it should be quoted in rucc's
own pass documentation: **before allocation, a machine-level rewrite must respect register pressure
and must not move instructions; after allocation, it may do both, and can do neither of the things
that need virtual registers.** Every pass placement decision in 37.5 follows from it.

`late-combine` does in 949 lines with RTL SSA a useful fraction of what `combine` does in 15,297
without it. That ratio is the single most quotable number in this document.

## 37.4 The passes that are genuinely machine-level

The short list, meaning the transformations that cannot be done above selection because they depend
on the target's instruction set, its condition codes, its addressing modes, or on the register
allocator's output.

**Combining and peepholing.** Spec 10.9's pass, in the DSL with the SMT obligation. The evidence from
`combine` says the window is two to four instructions and block-local, so rucc's version is a
DAG-matching pass over MIR in SSA with a bounded window, which is `late-combine` in shape rather than
`combine`. The rule set is per-target and grows over time. Not sized here because spec 10.9 owns it,
but on the evidence, low hundreds of rules and low thousands of lines of framework.

**If-conversion to conditional moves.** `gcc/ifcvt.cc` is 6,539 lines and runs three times: before
combine, after combine, and after reload. It is here rather than in document 22 because whether a
branch should become a `cmov` or a `csel` depends on the target having one, on the cost of the
instruction, and on the branch's predictability. Document 22's phiopt does the shape recognition
above selection; the *decision* belongs here. The three-instances-at-three-points structure exists
because each earlier pass exposes more opportunities, and rucc should not copy it; one instance,
after combining, before allocation.

**Redundant extension elimination.** `gcc/ree.cc`, 1,433 lines. On x86-64 a 32-bit operation
zero-extends into the 64-bit register, so an explicit zero-extension after it is dead. On RISC-V a
`W`-suffixed operation sign-extends, per spec 10.2's own example rule. These facts are target
semantics and the redundancy they create is invisible above selection. This is one of the few passes
on this list that rucc needs on day one for x86-64, because without it the generated code is full of
redundant `movl`.

**Bit-group liveness.** `gcc/ext-dce.cc:48` describes a refinement of the same idea: liveness is
tracked not per register but per bit group, "bit 0..7, bit 8..15, bit 16..31, bit 32..BITS_PER_WORD-1",
so an instruction that writes bits the program never reads is dead even though the register is live.
Four groups, chosen because that covers byte, half, word and doubleword. This is a small, elegant,
self-contained analysis, roughly 1,400 lines in GCC, and it subsumes a good deal of what `ree` does
plus narrowing opportunities. **If rucc builds one of these two, build this one**, because it is more
general and because its analysis is a straightforward extension of the liveness the register
allocator computes anyway.

**Addressing-mode folding.** `gcc/fold-mem-offsets.cc:40` gives the worked example, an add whose
constant can be moved into the displacement of every memory instruction that uses it, and its
justification is worth having verbatim because it is the argument for a late cleanup pass in general:

> Although the previous passes try to emit efficient offset calculations this pass is still beneficial
> because: - The mechanisms that optimize memory offsets usually work with specific patterns or have
> limitations. This pass is designed to fold offsets through complex calculations that affect multiple
> memory operations and have partially overlapping calculations. - There are cases where add
> instructions are introduced in late rtl passes...

934 lines. rucc's addressing-mode rules fold a base-plus-index-plus-displacement at selection time,
per spec 10.2's `lea` example, but they fold it *locally*, one instruction at a time, and the case
this pass exists for is one add feeding several memory operations with different offsets. Worth
building, worth building late, and small.

**Compare elimination.** `gcc/compare-elim.cc`, 981 lines. Most arithmetic instructions on most
targets set flags; an explicit compare against zero after one of them is redundant. On x86-64 and
AArch64 this is a real and frequent win, and it is not expressible above selection because flags are
not in the IR. rucc's operand model has flags as a fixed-register operand, per spec 10.1, so this
becomes a straightforward SSA query: is the flags definition here the same as the one an earlier
instruction already produced.

**Post-allocation hard-register copy propagation.** `gcc/regcprop.cc`, 1,538 lines, run as
`pass_cprop_hardreg`. After allocation the code contains copies the allocator could not coalesce, and
some of them are propagable because the source register is still live and unmodified. This is
cheap and it is the main post-allocation cleanup that matters. Spec 10.4 already assigns coalescing
to the allocator; this is the residue.

**Instruction pair fusion.** `gcc/pair-fusion.cc`, 3,145 lines, built on RTL SSA. Merging two
adjacent loads into an AArch64 `ldp` or two stores into an `stp`. It is target-specific in its
profitability but the framework is generic, which is why it was factored out of the AArch64 backend.
For rucc this is post-1.0 and AArch64-only, but it should be noted that the framework being generic
was worth doing, and that the pass needs SSA to find the pairs.

**Store-forwarding avoidance.** `gcc/avoid-store-forwarding.cc:43`, 719 lines, and the example in its
header is exact: a byte store followed by a word load from an overlapping address stalls on many
cores, and the fix is to reorder the load before the store and repair the loaded value with a bitfield
insert. This is a microarchitectural workaround, not an optimization in the classical sense, and it is
the newest kind of thing in the RTL pipeline. Post-1.0 for rucc, but it belongs on the list because it
is the sort of pass that only exists below selection and that nobody anticipates when planning a
backend.

**Register renaming.** `gcc/regrename.cc`, 2,054 lines, breaks false dependences after allocation.
It matters on in-order cores and matters much less on out-of-order ones, which is the same
qualification spec 10.5 makes about scheduling. Enabled per subtarget, or not built.

## 37.5 What rucc does not build, and the argument for each

**No RTL-level CSE, PRE, hoisting, store motion or dead store elimination.** Documents 16 and 21 own
these above selection. The residual redundancy that expansion creates is addressing arithmetic, and
the answer to that is 37.4's addressing-mode folding pass plus the fact that rucc's selector matches
over a DAG where the shared address computation is already one value with several uses. GCC's
post-expansion CSE exists partly because its expander duplicates such computations; rucc's does not.

**No RTL-level loop invariant motion or unrolling.** Documents 27 and 29 own these. GCC's RTL
versions exist because the tree versions cannot see target-dependent costs and because
`pass_rtl_doloop` needs to run late to form the hardware loop instruction where targets have one.
rucc has no such target, so the reason evaporates.

**No `lower-subreg`.** `gcc/lower-subreg.cc`, 1,903 lines, splits multi-word operations into
word-sized ones. It runs three times. rucc refuses 128-bit integers and `long double` by name at the
type boundary, per spec 10.2, so there is nothing to lower. This is a concrete dividend of that
refusal and it is worth recording as such.

**No `mode-switching`.** 1,335 lines, for targets with modal state such as x87's rounding mode or
MIPS's ISA modes. rucc's x87 usage is confined to `long double`, which is refused.

**No modulo scheduling.** `gcc/modulo-sched.cc`, 3,386 lines. Spec 10.10 already excludes software
pipelining.

**No early rematerialization.** `gcc/early-remat.cc`, 2,638 lines. Rematerialization belongs inside
the allocator, and document 39 will say so.

**No delay slot filling.** `pass_delay_slots`. No target rucc supports has them.

That is roughly 30,000 lines of GCC that rucc does not write, and every line of it is declined for a
stated reason rather than by omission.

## 37.6 The ordering

Two groups, separated by register allocation, following `gcc/late-combine.cc:31`'s rule.

**Before allocation, on MIR in SSA:** combining and peepholing; if-conversion to conditional moves;
bit-group dead code elimination; compare elimination; addressing-mode folding. All of these may
create and delete virtual registers, none of them may move an instruction far enough to matter to
register pressure, and if-conversion is the one that must consult a pressure estimate because
converting a branch extends both arms' live ranges into one block.

**After allocation:** hard-register copy propagation; a second, narrower peephole run for
encoding-size selection at `-Os`, per spec 10.9; pair fusion where a target wants it; register
renaming where a subtarget wants it.

**And the constraint marker.** `gcc/passes.def:556` carries the comment "No target-independent code
motion is allowed beyond this point, excepting the legacy delayed-branch pass." That line exists
because everything after it, the alignment computation, the branch shortening, the CFI emission, and
`final`, depends on instruction addresses being stable. rucc needs the same marker in the same place
and should write it as an assertion rather than a comment: after block layout, the instruction
sequence is frozen, and a pass that edits it is a bug the encoder will report as a relocation that
does not fit.

## 37.7 How this is wrong

**A combine substitutes across an instruction that modifies the source.** `combine` guards this with
`modified_between_p`. In SSA the guard is structural for registers and is *not* structural for
memory and for flags, both of which are single resources with many definitions. The flags register is
the classic case: substituting an arithmetic instruction across a compare changes what the compare
tested. rucc's operand model makes flags an explicit operand, which turns this into an ordinary
dataflow question, and the failure mode is a target description that forgot to declare that an
instruction clobbers flags.

**If-conversion converts a branch that guarded a fault.** Turning `if (p) x = *p;` into an
unconditional load and a select is wrong. This is document 27.1's safe-to-speculate predicate and it
applies here unchanged; the machine-level if-converter must consult the same predicate the middle end
does, which is an argument for computing it once on the IR and carrying it to MIR rather than
recomputing it on machine instructions.

**If-conversion converts a well-predicted branch.** A `cmov` costs the latency of both arms; a
correctly predicted branch costs nearly nothing. Converting a branch that predicts at 99% is a
pessimization, and it is one of the places where profile data pays best. Document 40 owes the cost
model and document 11's static heuristics are the fallback.

**Extension elimination gets the target's semantics wrong.** Whether a 32-bit operation zero-extends
or sign-extends or leaves the upper bits undefined is a per-target, per-instruction fact. Getting it
wrong produces garbage in the upper half that a later 64-bit use reads. This is exactly the class of
bug spec 10.2's SMT obligation exists for, and the extension eliminator should be expressed as rules
with specs for that reason rather than as a hand-written analysis.

**Bit-group liveness deletes a write whose bits are read through memory.** A partial write to a
register that is later stored in full. The analysis must treat a store as reading every bit of its
source, and an escape as reading everything.

**Addressing-mode folding produces an offset the target cannot encode.** Every target bounds the
displacement, and the bound differs per instruction and per width on AArch64 in particular. The fold
must ask the encoder, not a constant.

**Post-allocation copy propagation propagates a register that is clobbered by an intervening call.**
The call's clobber set is ABI data, and a caller-saved register is dead across a call. This is
`function-abi`'s job in GCC and it is a place where a target description error produces silent wrong
code.

**A pass runs after the layout freeze and invalidates branch distances.** 37.6's marker.

## 37.8 What it costs, and what to measure

The whole group is a small number of linear or near-linear passes over MIR. The two that are not
obviously linear are combining, which is bounded by `max-combine-search-insns`-equivalent limits and
should have one, and if-conversion, which examines diamond shapes and is bounded by the number of
branches.

The honest expectation, taken from where GCC's time goes and from the fact that rucc is not
re-running the middle end here: the entire machine-level group should be under 10% of `-O2` compile
time, with the register allocator dominating the backend.

Document 42 owes six numbers.

- **Redundant extensions per thousand instructions before and after the eliminator**, on x86-64 and
  RISC-V separately, since the two targets create them for opposite reasons.
- **Compare elimination hit rate**, which is a count and is nearly free to instrument.
- **If-conversion on and off**, run time and size, split by whether a profile was available. This is
  the pass on the list most likely to be a net loss with static heuristics, and the measurement
  should be allowed to say so.
- **Addressing-mode folding's instruction count reduction**, which is the number
  `gcc/fold-mem-offsets.cc` claims and which should reproduce.
- **The combine window's marginal value**: how many combines fire at depth two, three and four.
  GCC's `IntegerRange(2, 4)` says four is enough; the counts say whether three would do.
- **The machine-level group's share of `-O2` compile time**, against the 10% expectation.

## 37.9 The decision

Ten passes below selection, not eighty. Five before register allocation: combining and peepholing,
if-conversion, bit-group dead code elimination, compare elimination, addressing-mode folding. Four
after: hard-register copy propagation, size-directed peepholes, and, per target and post-1.0, pair
fusion and register renaming. Plus the change-application framework of 37.2, which is shared
infrastructure rather than a pass and which should exist before the first of them.

The two findings that carry beyond this document: **GCC is retrofitting SSA onto RTL and paying
9,607 lines for it**, which settles spec 10.1's design choice as the incumbent's own direction; and
**`late-combine` does a useful fraction of `combine`'s work in 949 lines against 15,297**, which is
the ratio to expect from having def-use chains and which is the quantitative case for the whole
approach.
