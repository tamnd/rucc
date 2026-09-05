# 36. Lowering and instruction selection

The boundary. On one side, a target-independent SSA IR over abstract operations; on the other,
something that has a one-to-one relationship with machine instructions. Everything in documents 12
through 35 happens above this line and everything in documents 37 through 39 happens below it.

GCC crosses it once, at `pass_expand`, and the thing on the far side is RTL. rucc crosses it once, at
selection, and the thing on the far side is MIR. The two designs are not as different as the
vocabulary suggests, and where they differ the differences are deliberate and are worth having
written down before the code exists.

The sizes, target-independent side first: `gcc/expr.cc` 14,772, `gcc/optabs.cc` 8,529,
`gcc/cfgexpand.cc` 7,519, `gcc/emit-rtl.cc` 7,103, `gcc/expmed.cc` 6,450, `gcc/internal-fn.cc`
5,940, `gcc/genrecog.cc` 5,543, `gcc/calls.cc` 5,425, `gcc/recog.cc` 4,766, `gcc/explow.cc` 2,407,
`gcc/tree-outof-ssa.cc` 1,522, `gcc/gimple-isel.cc` 1,408, `gcc/dojump.cc` 1,336, `gcc/optabs-tree.cc`
671. Call it 73,000 lines before a single target exists.

## 36.1 What is lowered before the boundary, and why that is the important list

The instructive thing about GCC's expansion is not `pass_expand` itself, it is the sequence of passes
immediately before it. From `gcc/passes.def:440` onward, in order: `pass_lower_vaarg`,
`pass_lower_vector`, `pass_lower_complex_O0`, `pass_lower_bitint_O0`, `pass_sancov_O0`,
`pass_lower_switch_O0`, `pass_asan_O0`, `pass_tsan_O0`, `pass_musttail`, `pass_sanopt`,
`pass_cleanup_eh`, `pass_lower_resx`, `pass_nrv`, `pass_gimple_isel`,
`pass_harden_conditional_branches`, `pass_harden_compares`, `pass_warn_access`,
`pass_cleanup_cfg_post_optimizing`, `pass_warn_function_noreturn`, and only then `pass_expand`.

Nine of those are lowerings. They exist because **each of them turns one IR construct into a shape of
control flow or a shape of arithmetic that the expander would otherwise have to invent**, and the
expander is the wrong place to invent control flow because by the time it runs the CFG is being
consumed rather than edited.

`va_arg` becomes explicit loads and a branch. A vector operation the target lacks becomes scalar
operations, which requires splitting a statement into several and, for some operations, into several
blocks. A complex arithmetic operation becomes a pair of real ones. A `_BitInt` becomes a loop or an
unrolled sequence over limbs, which is why `gcc/gimple-lower-bitint.cc` is 8,501 lines. A `switch`
becomes whatever shape the target wants, per document 24. A `resx` becomes the landing pad dispatch.

**This validates spec 10.2's rule directly and it is worth saying so in exactly those terms.** The
rucc spec says a lowering rule replaces a term with a term, has nowhere to put a block, and so any
construct whose lowering is a new shape of control flow is rewritten in the IR before selection runs.
GCC arrived at the same architecture from the other direction, by discovering over three decades that
doing these things inside the expander was unmaintainable, and moving them out one at a time. The
list above is that migration's current state.

So the actionable content of this section is: **rucc's pre-selection lowering group is not one pass
for `switch`, it is a group, and the group's membership is knowable now.** `switch`, per spec 10.2
and document 24. `va_arg`, per spec 10.7's split, where `va_arg` is rewritten before selection and
`va_start` is finished from the frame. Float constants, negations and conversions, per spec 10.2's
paragraph on arithmetic the rule language cannot do. `memcpy` and `memset` expansion, likewise.
Unsigned integer-float conversions, which spec 10.2 already routes to "a pass that rewrites it into
the signed ones". Any operation the target lacks, which is the general case that vector lowering is a
special case of.

That group needs a name, one entry point, and one dump, and it should be built as a group from the
start rather than accumulating as a set of unrelated passes that happen to run adjacently. The name
in GCC is "the lowering passes"; the honest description is "everything the selector cannot express".

## 36.2 GIMPLE-level instruction selection

`gcc/gimple-isel.cc:1` calls itself "Schedule GIMPLE vector statements", and :43 documents its
transformation: an `ARRAY_REF` on a `VIEW_CONVERT_EXPR` of a vector, which is how GIMPLE spells
"index into a vector by a variable", becomes a call to the internal function `.VEC_SET` or
`.VEC_EXTRACT`, chosen by what the target has.

That is instruction selection, done on GIMPLE, before expansion. It exists because the choice
depends on the target and the consequences of the choice are best expressed in the IR the optimizer
understands rather than in RTL.

**The direction of travel matters more than this one pass.** GCC has been steadily moving decisions
that used to live in the expander up into GIMPLE, using internal functions as the vocabulary. It is
the same movement as the vectorizer's, which produces internal function calls rather than RTL, and
the same as the atomics and the overflow builtins. The RTL expander is slowly becoming a translator
rather than a decider.

rucc starts where GCC is heading: the decisions are in the rule set, the rule set is data, and the
matcher is generated. There is no expander with judgement in it.

## 36.3 Out of SSA, and the problem rucc does not have

`pass_expand::execute` begins, at `gcc/cfgexpand.cc:7037`, with `rewrite_out_of_ssa`. GCC leaves SSA
form at the boundary, because RTL is not in SSA form. `gcc/tree-outof-ssa.cc` is 1,522 lines and its
own opening comment, at `gcc/tree-outof-ssa.cc:53`, is a FIXME complaining that half of it is really
expansion code in the wrong file.

Leaving SSA is not a syntactic operation. Phi nodes become copies on edges; the copies must be
coalesced or the code is full of moves; coalescing requires an interference analysis; and the copies
on an edge are a *parallel* copy, so cycles among them need a temporary. This is the lost-copy and
swap problem and it is a well-known source of bugs.

On top of that GCC does **temporary expression replacement**, `flag_tree_ter` at
`gcc/common.opt:3374`, invoked from `gcc/tree-outof-ssa.cc:1519` as `remove_ssa_form (flag_tree_ter,
sa)` and documented at :1165. TER re-forms trees out of single-use SSA definitions so the expander
sees `(a + b) * c` as one tree rather than three statements, because RTL expansion is tree-based and
generates better code from a bigger tree. It also actively harms debug information, which is why
`avoid_deep_ter_for_debug` exists at `gcc/cfgexpand.cc:7047`.

**rucc has none of this**, and the reason is a design decision made in two places that compound.
Spec 10.1 keeps MIR in SSA until register allocation, so the boundary is not an SSA exit. And
document 09.3's block parameters mean there are no phi nodes to destruct even at the eventual exit:
the parallel copy on an edge is the block call's argument list, which is already explicit, already
ordered, and already the allocator's problem rather than the expander's.

**This is the sixth structural payoff of block parameters** and it should be recorded with the other
five: memory phis need no side table (09.3), copy propagation does not exist (15.1), jump threading
needs no phi bookkeeping (23.1), loop-closed SSA is nearly free (26.4), inlining a multi-return callee
needs no phi surgery (33.7), and now, out-of-SSA is not a pass. GCC spends 1,522 lines plus a
correctness-critical parallel-copy sequencer on a problem that in rucc is one function in the
register allocator's rewrite step, which spec 10.4 already assigns to it.

The TER half has no rucc analogue either, and it should not acquire one. TER exists because GCC's
expander is a tree walker that produces better code from larger trees. rucc's matcher matches over
the IR's DAG directly, with maximal munch across single-use edges, which is TER's benefit obtained
structurally rather than by a pre-pass that reconstitutes trees and damages debug info doing it.

## 36.4 Optabs: the target capability query

`gcc/optabs.def` has 489 entries. An optab is a mapping from an abstract operation and a mode to a
machine description pattern name, and the query "does this target have an instruction for signed
division of `SImode`" is `optab_handler (sdiv_optab, SImode) != CODE_FOR_nothing`.

This one indirection is what lets 73,000 lines of target-independent expansion code exist at all.
`gcc/optabs-tree.cc` (671 lines) maps tree codes to optabs; `gcc/optabs.cc` (8,529) is the expansion
machinery, and the interesting part of it is the fallback ladder. When the direct optab is missing,
`expand_binop` tries: a widened mode, a related mode, the operation open-coded from simpler
operations, and finally a libcall. Signed division on a target without a divide instruction descends
that ladder to `__divsi3`.

**rucc's equivalent exists and should be named as such.** Spec 10.2's coverage test asks what names
an instruction can be called by and what names a rule is written at. That is an optab table computed
at build time rather than a hook consulted at run time, which is better: the fallback ladder in GCC
is a run-time search whose result depends on the target and is therefore hard to enumerate, whereas
rucc's is a static table that a build-time test reads.

But the fallback *ladder* is a separate thing from the coverage test, and rucc needs it too. An
operation with no rule on a target is not automatically an error; sometimes the right answer is a
libcall to the compiler runtime, which is exactly what spec 10.2 already does for an oversized
`memcpy`, refusing it by name because there may be no `memcpy` to link against. The generalisation:
**the coverage test's exception list, the libcall list, and the pre-selection lowering group of 36.1
are three answers to the same question**, which is what to do about an operation the target lacks,
and they should be one table with three columns rather than three mechanisms.

`gcc/internal-fn.def` is the modern face of the same idea, with 255 entries. Its header at
`gcc/internal-fn.def:21` explains the motivation: internal functions "have no linkage and cannot be
called directly by the user", they "represent operations that are only synthesised by GCC itself",
and they are used "instead of tree codes if the operation and its operands are more naturally
represented as a GIMPLE_CALL than a GIMPLE_ASSIGN". `DEF_INTERNAL_OPTAB_FN` binds one directly to an
optab, so the capability query and the IR opcode are the same declaration.

The design lesson, and it applies to rucc's IR opcode set: **adding an opcode should not be a change
to a fixed enum that every pass switches on.** GCC's tree codes are that, and internal functions
exist to escape it. rucc's opcode enum is fine while the opcode set is small and target-independent;
the moment target-specific or optional operations start arriving, the pressure GCC felt will arrive
too, and the answer is the same one: a call-shaped opcode with a table-driven definition, not two
hundred more enum variants.

## 36.5 The machine description, and what generates the matcher

`gcc/genrecog.cc:21` describes what a machine description is compiled into, and the algorithm is
worth having in full because it is exactly the problem rucc's `rucc-rules` solves:

> 1. Build up a decision tree for each routine... First determine the "shape" of the rtx, based on
> GET_CODE, XVECLEN and XINT. This phase examines SET_SRCs before SET_DESTs since SET_SRCs tend to be
> more distinctive... 2. Try to optimize the tree by removing redundant tests, CSEing tests, folding
> tests together, etc. 3. Look for common subtrees and split them out into "pattern" routines... 4.
> Split the matching trees into functions, trying to limit the size of each function to a sensible
> amount. 5. Write out C++ code for each function.

Five observations that transfer directly.

**Test ordering is a heuristic and it is stated.** Sources before destinations because sources are
more distinctive. rucc's matcher generator needs the same kind of ordering decision and it should be
made explicitly and documented, not fall out of field order in a struct.

**Redundant test elimination and test CSE are the point.** A naive matcher over 4,397 patterns
re-tests `GET_CODE (x) == PLUS` thousands of times. The decision tree tests it once. This is the
difference between a matcher that is fast and one that is a chain of conditionals, which is the
distinction spec 10.2 already draws.

**Subtree sharing across patterns that differ only in mode or code** is a compression that a rule
compiler must do or the generated matcher will not fit in cache. GCC's example is `(plus:SI reg reg)`
and `(minus:DI reg reg)` sharing one routine parameterised by code and mode. rucc's mode iterators
are the widths in an opcode name, per spec 10.2, so the same sharing is available and the generator
must take it.

**Function splitting exists to keep the C++ compiler from choking**, which is a self-hosting concern
GCC has and rucc has in the same form: `rucc-rules` generates Rust that `rustc` must compile, and one
function with a hundred thousand branches in it compiles slowly.

**And `recog` returns an insn code or -1**, plus a count of missing clobbers. That last detail is
worth noting as a design smell to avoid: GCC's matcher will accept a pattern that matches except for
absent `CLOBBER` expressions, and the caller must then allocate a `PARALLEL` and call `add_clobbers`.
This is x86's flags register leaking into the matching interface. rucc's operand model, per spec
10.1, carries the role and the fixed-register constraint on each operand, so an instruction that
clobbers flags says so in its definition and the matcher never sees a partial match. That is the
right design and it is worth knowing what it avoids.

The other generated matchers from the same description: `split_insns` and `peephole2_insns`, per
`gcc/genrecog.cc:42` and :46. Splitting and peepholing are the same matching problem at a different
point in the pipeline, and generating all three from one description is why GCC can afford 176
peepholes on x86. rucc's spec 10.9 already puts peepholes in the same DSL with the same verification
obligation, which is the same conclusion.

## 36.6 The honest size of a target

This is the number the rest of the document exists to contextualise.

| Target | ISA description | Pipeline models | Target C++ | Total |
|---|---:|---:|---:|---:|
| i386 | 76,211 | 17,382 | 29,117 | 122,710 |
| aarch64 | 51,097 (all `.md`) | included | 34,139 | 85,236 |
| riscv | 42,283 (all `.md`) | included | 16,968 | 59,251 |

The i386 breakdown, since it is the one rucc must match first: `i386.md` 30,901, `sse.md` 33,668,
`mmx.md` 7,005, `predicates.md` 2,406, `sync.md` 1,285, `constraints.md` 475, `subst.md` 471, and
nineteen separate pipeline description files totalling 17,382 lines, one per microarchitecture from
`pentium.md` through `znver.md`.

Pattern counts across the i386 descriptions: 4,397 `define_insn`, 984 `define_expand`, 444
`define_insn_and_split`, 269 `define_mode_iterator`, 212 `define_split`, 178 `define_subst`, 176
`define_peephole2`, 29 `define_code_iterator`.

Spec 10.2 estimates 600 to 900 rules per target, of which 150 compile anything at all. Set that
against 4,397 `define_insn` and the gap looks alarming until it is decomposed, and the decomposition
is the useful part:

- **`sse.md` alone is 33,668 lines**, 44% of the i386 ISA description, and rucc has no vector types
  in M4 at all (document 32.9, spec 10.2's refusal to name a vector type). Nearly half the gap is
  scope rucc has explicitly declined.
- **`mmx.md` is 7,005 lines** for an instruction set that has been obsolete for twenty years and is
  kept for compatibility.
- **17,382 lines of pipeline models** correspond to spec 10.5's machine models, which are data files,
  and the decision there is to ship approximate models and refine them. One model per architecture,
  not nineteen per architecture.
- **`define_subst`, 178 uses**, generates masked and rounding variants of AVX-512 patterns
  mechanically. It is metaprogramming over patterns, and it means the 4,397 figure overcounts what a
  human wrote.
- **`define_expand`, 984 of them**, are not instruction patterns; they are the target's half of the
  optab interface, and many are multi-instruction sequences that in rucc live in the pre-selection
  lowering group of 36.1 or in the peephole set of spec 10.9.

What remains, the scalar integer and floating-point instruction patterns that a C compiler targeting
x86-64 actually needs, is on the order of a thousand to fifteen hundred, and spec 10.2's estimate is
therefore in the right range rather than optimistic by a factor of four. **That is a real finding and
it should be recorded, because the raw comparison of 800 against 122,710 would otherwise read as
evidence that the plan is unserious.**

The number that is not explained away is `i386.cc` at 29,117 lines. That is target hooks: cost
functions, ABI classification, addressing mode legality, constant legality, register class
preferences, builtin expansion, and the accumulated special cases of a thirty-year-old port. rucc's
`rucc-target` will grow toward some fraction of that and the fraction is unknown. Spec 10.8's claim
that a target is "a `TargetInfo`, a register file description, a machine model, a lowering rule set,
an ABI description, an encoder, and a relocation set, nothing else" is a hypothesis, and spec 10.8
already names the experiment that tests it, which is bringing up a fourth target in M10 and measuring
the effort.

## 36.7 Stack slots, which are decided at the boundary

`gcc/cfgexpand.cc` spends several hundred lines on something easy to overlook: assigning stack slots
to local variables, with sharing. `add_stack_var_conflict` at :500 and `stack_var_conflict_p` at :517
build a conflict graph over the local variables whose live ranges overlap, and the partitioning that
follows lets two locals in disjoint scopes share a slot. `-fstack-reuse` controls it.

This matters more than it sounds, because C programs declare large aggregates in nested scopes and a
compiler that gives each one its own slot produces frames several times larger than necessary, which
costs stack traffic and, with `-fstack-clash-protection` or deep recursion, correctness-adjacent
behaviour.

Also at `gcc/cfgexpand.cc:7052`, `discover_nonconstant_array_refs` marks arrays indexed by a
non-constant as address-taken, forcing them to memory. That is the expansion-time recognition that an
object which document 20's SROA declined to scalarise must be given real storage.

**rucc's frame layout is spec 10.7's and it happens after register allocation**, which is later than
GCC's and is the right place, because spill slots and local slots should be allocated by one
mechanism rather than two. The conflict-graph sharing above applies to both, and it is the same
interference computation the register allocator already did. That is a simplification worth taking:
**one slot allocator, running after allocation, sharing slots between locals and spills using the
allocator's own liveness.** GCC cannot do this because its stack layout is fixed before reload.

## 36.8 What rucc builds, and what changes from spec 10

Spec 10 is the design and it stands. What this document adds is four things.

**The pre-selection lowering group** of 36.1, named, with one entry point and one dump, with its
membership enumerated rather than discovered. Roughly: `switch`, `va_arg`, float constants,
float negation, float-integer conversions in both directions including the unsigned cases, `memcpy`
and `memset` expansion, and oversized-operation splitting. Perhaps 1,200 lines, and it is a
prerequisite for the first target rather than a refinement.

**The capability table** of 36.4, one table with three columns, saying for every opcode-and-width
name whether it is lowered by a rule, lowered by the pre-selection group, or emitted as a libcall,
with the exception list of spec 10.2's coverage test folded in as a fourth answer. This replaces
three separate mechanisms with one and makes the coverage test a query against it.

**The unified slot allocator** of 36.7, sharing frame slots between locals and spills using the
register allocator's liveness.

**And the explicit non-adoption of out-of-SSA and TER**, recorded here so that nobody later
implements either by analogy with GCC. There is no SSA destruction pass in rucc; the register
allocator's rewrite is where SSA ends, and the parallel copy sequencing that spec 10.4 assigns to it
is the entirety of the problem GCC spends 1,522 lines on.

The rest is spec 10 as written: rules in the DSL, no hand-rolled match arms, maximal munch with
specificity ordering and cost tiebreak, a generated automaton, and an SMT obligation on every rule.

## 36.9 How this is wrong

**A rule is proved and wrong anyway because the specification is wrong.** The `spec` clause is
written by hand. Spec 10.2 already handles this by proving pattern-meaning against replacement-meaning
*and* the clause separately, so a clause that does not match its own pattern is caught. That is the
right structure and it is worth restating: the solver is asked two questions, not one.

**A rule matches something the guard should have excluded.** A shifted-register add on AArch64 where
the shift amount is 64. Guards are assumptions in the proof, so a guard that is weaker than the
instruction requires produces a proof that is valid and an instruction that is not. The defence is
that the machine model's precondition must be part of the model, not part of the rule.

**Maximal munch takes a big match that is worse than two small ones.** The classic tiling failure:
`lea` with a scale folds an add and a multiply into one instruction, but if the multiply's result is
used again elsewhere the fold duplicates it. Any DAG-covering selector must check that the operands
it absorbs are single-use, and forgetting that check on one rule is a code-quality bug that no proof
catches because both codes compute the same value.

**A pre-selection lowering introduces a block after something assumed block structure.** The lowering
group of 36.1 edits the CFG. Anything computed before it and cached across it, dominators, loop
structure, frequencies, is stale. GCC's answer is that expansion frees dominance info explicitly at
`gcc/cfgexpand.cc:7061`. rucc's answer must be the same: the group invalidates everything, and it
runs before selection precisely so that selection can assume a fixed CFG.

**An operation reaches selection with no rule and no entry in the capability table.** The build-time
coverage test is what prevents this, and it prevents it only if the test's exception list is
maintained honestly rather than grown whenever it fails. Spec 10.2 requires a reason beside each
entry; the discipline is that adding an entry needs the reason, and "no rule yet" is a
tracked-issue reason and not an excuse.

**A frame slot is shared between two locals whose live ranges the analysis thought were disjoint and
were not.** The `-fstack-reuse` bug class. It is a wrong-code bug that manifests as one variable
corrupting another, and the historical instances in GCC involve address-taken locals whose addresses
outlive their scopes. The defence is conservatism: a local whose address escapes gets its own slot.

**The parallel copy on an edge is sequenced wrongly when it contains a cycle.** Spec 10.4 already
names this as "a small algorithm that is wrong in a startling number of compilers". It needs a
dedicated exhaustive test over small permutations, not incidental coverage.

**A `long double` or a 128-bit integer reaches a point that assumed it had been refused.** Spec 10.2
refuses both by name at the ABI boundary and at the type-naming boundary. Two refusal points that
must agree is a place where they can disagree, which is why spec 10.2 already insists the decision be
"one decision in one place". Worth a test that enumerates every type name and asserts the two agree.

## 36.10 What it costs, and what to measure

Selection is one pass over the IR with a generated automaton, so it is linear in instructions and
should be a small fraction of compile time. GCC's expansion is not, because it does out-of-SSA, TER,
stack layout and RTL generation in one pass, and `TV_OUT_OF_SSA` and `TV_VAR_EXPAND` are separate
timers precisely because they are large enough to want separating.

Document 42 owes five numbers.

- **Selection's share of `-O2` compile time.** Target under 5%. If the generated automaton is slower
  than that, the decision-tree optimisations of 36.5 are not being done.
- **The generated matcher's size**, in Rust source lines and in compiled object bytes, since
  `genrecog`'s steps 3 and 4 exist entirely to control that number and rucc will need them at the
  same point.
- **Rule count per target against instructions actually selected**, over the corpus, so that the
  600-to-900 estimate is checked against what firing rules there are. A rule that never fires on the
  whole corpus is either dead or covering a case the corpus lacks, and knowing which is worth the
  counter.
- **Frame size against `gcc -O2`**, which is the visible consequence of 36.7's slot sharing and is
  cheap to measure since it is a static property of the emitted prologue.
- **The single-use check's effect**: number of DAG covers where a maximal munch was rejected because
  an absorbed operand had another use, and the code size and run time with the check disabled, which
  will be wrong-in-quality rather than wrong-in-behaviour and is therefore measurable rather than
  merely testable.

## 36.11 The decision

Spec 10 stands unamended in its essentials. This document adds the pre-selection lowering group as a
named deliverable, folds the coverage exception list and the libcall list into one capability table,
moves local slot allocation into the register allocator's slot allocator, and records that out-of-SSA
and temporary expression replacement are deliberately absent rather than not yet written.

The finding that matters most for the plan is 36.6's: **GCC's x86 backend is 122,710 lines and the
part of it that a scalar C compiler needs is on the order of a tenth of that**, once vector ISAs,
obsolete ISAs, nineteen microarchitectural pipeline models, and mechanically generated pattern
variants are set aside. Spec 10.2's estimate survives contact with the source, which is not something
that could be assumed and is the reason to have counted.
