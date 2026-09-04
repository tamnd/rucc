# The backend

From optimized SSA IR to machine instructions with physical registers. Two paths through this document: a fast path used at `-O0`, and an optimizing path used everywhere else. They share the instruction selector's rule set and nothing else.

## 10.1 MIR

The machine IR is a second representation: still a CFG of blocks, still in SSA form until register allocation destroys it, but the instructions are the target's instructions and the operands are virtual registers drawn from the target's register classes.

Keeping MIR in SSA until allocation is what lets the register allocator do live-range splitting, and it is the design regalloc2 assumes. After allocation, MIR is no longer SSA: block parameters have become physical registers, and the moves implementing the parameter passing have been materialized on the edges.

An MIR instruction is: an opcode from the target's opcode enum, a small operand vector where each operand carries its register class and a use/def/early-def role and any fixed-register constraint, an optional immediate, an optional memory addressing mode, and a source location. Twenty-four bytes, arena allocated, per document 03.

`--emit=mir` and `--emit=mir-final` print before and after allocation, both round-tripping.

The passes below run in one order and only one, and that order is a single entry point in `rucc-codegen` rather than something a caller assembles for itself: selection, then splitting the critical edges, then allocation, then the frame, then the prologue and the epilogue and the moves the allocator asked for, and last the block layout. The layout is last because everything before it finds the blocks a function returns from by looking for the ones that go nowhere, and it is the pass that reorders where a block goes. A caller says which machine it is compiling for and hands over a function. `--emit=mir-final` is that entry point called once per definition in the module, which makes it the first command that runs the whole compiler over a file, and a function with something in it no rule reaches is reported by name along with what stopped it rather than silently left out.

## 10.2 Instruction selection

Lowering is a term-rewriting rule set in the same DSL document 09 uses for the middle end, compiled by `rucc-rules` into a matcher at build time. **No lowering is written as hand-rolled `match` arms.** This is the settled decision from document 00 and the reasoning is in document 01: hand-written lowering is the largest single source of miscompilation in a from-scratch backend, and Crocus demonstrated that SMT verification of rule-based lowering finds real bugs including a 9.9-severity one.

```
;; x86-64: (add x (mul y 4))  =>  lea, using the addressing mode
(rule (lower (add.i64 (value x) (mul.i64 (value y) (iconst 4))))
      (x64.lea (amode_base_index_scale x y 4))
      (spec (= (bvadd x (bvmul y 4)) (result))))

;; aarch64: (add x (shl y k))  =>  add with shifted register, k < 64
(rule (lower (add.i64 (value x) (shl.i64 (value y) (iconst k))))
      (if (and (>= k 0) (< k 64)))
      (a64.add_shifted x y (lsl k))
      (spec (= (bvadd x (bvshl y k)) (result))))

;; riscv64: 32-bit ops sign-extend into 64-bit registers
(rule (lower (add.i32 (value x) (value y)))
      (rv.addw x y)
      (spec (= (sign_extend 32 64 (bvadd (extract 31 0 x) (extract 31 0 y)))
               (result))))
```

What a reader makes of that text is fixed by three rules, each of which closes an ambiguity the examples above leave open. Everything is a term, and a term is a name, a number, or a head applied to arguments; a bare name is a variable and a parenthesised one is an application, which is why a constructor that takes nothing is still written `(result)`. A variable is bound by its occurrence in the pattern and may occur there only once, because two occurrences would be asking the matcher for an equality test it does not have. And the `spec` clause is part of the grammar rather than a convention, so a rule written without one is a syntax error rather than an unverified rule, which is what gives the obligation below somewhere to stand.

**Naming a term.** A rule matches a head, and what an IR instruction's head is called comes from its opcode and its type together, which is why `add.i64` and `add.i32` are two rules rather than one rule with a width in it. That only works for a type with a name, and two kinds of type do not have an obvious one. An address has no width in the IR at all, because how wide an address is belongs to the target rather than to the program, so it is named at the target's address width and then every rule written about an integer of that width reaches it; adding to a pointer is an add at that width for the same reason, since the offset is already in bytes by the time the IR holds it and the front end is what did the multiplying. A vector is not the width of its lane, and naming it that would hand a rule meaning one integer an instruction meaning several, so it has no name here and is refused rather than lowered. A float is named apart from the integer of the same size, because the two are different arithmetic in different registers and a rule that confused them would be checked against the wrong operation, and the eighty bit one is named at neither width: it lives on a stack rather than in a register file, so it is refused in the same breath as a vector. Which types have names is one decision in one place, shared with the ABI lowering in section 10.7, because an argument brought in at a width no rule can name is a register nothing downstream is able to read, and a width the rules do cover that the ABI refuses is a function turned away for no reason.

**What a rule cannot do.** A rule replaces a term with a term, and a term is instructions. It has nowhere to put a block, so a construct whose lowering is a new shape of control flow is not something a rule can express, and any of them are rewritten in the IR before selection runs rather than during it. There is one today and it is `switch`, which leaves a block with as many successors as the program had cases where every other terminator leaves it with one or two. What it becomes is a target decision and not a language one, which is why it survives the front end intact: a chain of compares is right for three cases and wrong for two hundred, where the answer is a jump table, and wrong again for twenty spread over a million, where it is a binary search on the value. The chain is what goes in first, because a slow `switch` is a working `switch` and no `switch` is not, and because a jump table wants a read only section and a relocation to reach it.

Rules are matched over the e-graph-canonicalized IR with a maximal-munch strategy: the matcher tries rules in specificity order and takes the first that fires, with a cost-based tiebreak where multiple rules of equal specificity match. Because the rules are data, the matcher is a generated automaton rather than a chain of conditionals, which is both faster and easier to reason about.

**The verification obligation.** Every rule carries a `spec` clause relating the IR semantics to the machine semantics, discharged by `rucc-verify` against an SMT solver in CI. Every *term* used in a rule needs a specification, which is Crocus's stated tax and which we pay from the first rule rather than retrofitting. The machine semantics come from a per-target model in `rucc-target`, hand-written initially; the follow-up work cited in document 01 verifies against authoritative ISA semantics instead, and document 19 records adopting that as a post-1.0 improvement.

Not every term is a bitvector, and the two that are not are worth naming because both are places where a solver could be asked a question nobody meant. A rule with an effect relates one memory to another, so memory is a map from an address to a byte and nothing wider than a byte is built in: a load of four bytes is written as four reads put together in the model file, which is what makes the byte order something a reviewer reads rather than something the tooling decides. A rule about a float relates two floats, in the arithmetic the floating point standard defines rather than in two's complement, and a float is not the bitvector of its own width however much it looks like one. Keeping the two apart is what makes a rule that lowers float arithmetic to an integer instruction an error rather than a proof about the wrong operation. The rounding is nearest with ties to even and it is supplied once rather than written on each rule, so no two rules can be proved under different roundings, and a `long double` has no float sort here at all: eighty bits on the x87 stack is not one of the interchange formats and the ABI lowering refuses it by name for the same reason. The one place a float and the bitvector of the same size are the same thing is a load and a store, since neither instruction looks at the bits it moves, and that reading is a head of its own so that a rule which means it has to say so and every other way of putting the two together stays an error. Going between the two on purpose is a different matter with heads of its own, because a conversion keeps the value as closely as the format allows and keeps no bit where a reinterpretation keeps every bit and no value, and the rounding is not the same in both directions: a number becoming a float rounds to nearest like the arithmetic does, and a float becoming an integer keeps the part before the point and discards the rest whatever the mode is set to, which is what C says and is why the instruction selected for it is the one whose mnemonic has two `t`s in it. A float too big for the integer it is asked for has no answer and none is claimed, since the model leaves that case unspecified, the language leaves it undefined and the machine writes a value of its own, so such a rule is proved for every float the conversion is defined for and says nothing about the rest. The unsigned conversions are not rules at all, because the machine has no instruction for either at a width the rules name, so each is several instructions and belongs in a pass that rewrites it into the signed ones.

Comparing two floats is where keeping the two sorts apart pays a second time. C has fourteen predicates that are not constants, the machine has one instruction that sets three flags, and what relates them is a table rather than a rule of thumb: the zero, parity and carry flags together tell four outcomes apart, which are greater, less, equal and neither because one side was a not a number, and six of the fourteen predicates are one condition code read off that. Four more are one of those six with the operands the other way round, which is sound because the instruction treats a not a number the same in either order and is not something a reader should have to take on trust, so it is the one thing the comment above those rules says. The last two are the equality that is false when either side is a not a number and the inequality that is its negation, neither of which the machine has a condition for, so each is two condition codes and a boolean operation between them, written as one instruction with a second byte it writes and discards rather than as two rules that would compare the same pair twice. What such an instruction needs from the description is only that the spare byte is a written operand, since two definitions of one instruction are both live where it ends and the allocator will not put two overlapping ranges in one register. The claim each of these rules proves is written with the floating point standard's own comparisons and not with the solver's equality, because that equality says a not a number equals itself and says a positive zero is not a negative zero, and a C program sees both of those differently.

The question a rule is asked is the negation of two claims at once. What the pattern means and what the replacement means have to agree, both read out of the machine model, and the `spec` clause has to hold as well. The second is not redundant: the clause is written by hand, and a rule whose stated claim is not what its pattern means would otherwise be checked against its own mistake. A guard is an assumption rather than part of the claim, which is what makes a rule that only holds for some constants provable. Nothing but `unsat` is a pass, and a solver that gives up is recorded as having given up rather than folded into either answer.

**Coverage.** Every IR opcode must have at least one lowering rule per target, checked by a build-time completeness test. A missing rule is a compile error in the rule compiler, not a runtime "unsupported instruction" panic discovered by a user.

The rule set is where the per-target work actually is. Expect roughly 600 to 900 rules per target for good coverage, of which perhaps 150 are needed to compile anything at all, which is the ordering that makes a new target cheap to bring up and expensive to finish.

## 10.3 The fast path

At `-O0` the selector runs in single-pass mode: no cost-based tiebreak, take the first matching rule, no cross-block matching, no e-graph. Then a single-pass linear-scan register allocator, then emit. No scheduling, no peepholes, and a block order that comes from the shape of the control flow rather than from how often each block runs: reverse postorder with each block's successors walked in reverse, which puts the first arm of a branch next and so gets the fall-through right on an `if` and on a loop without knowing anything about either.

This is where the 2x-over-`clang -O0` claim in document 02 is won, and it is why the two paths are separate code with a shared rule set rather than one path with quality knobs.

## 10.4 Register allocation

Two allocators behind one interface.

**Single-pass, for `-O0`.** Linear scan over a linearized block order, with live intervals computed in one backward pass. Spill on demand, no splitting, no coalescing. Produces mediocre code quickly, which is exactly what `-O0` wants.

**Backtracking with live-range splitting, for `-O1` and above.** The design follows regalloc2 and, through it, IonMonkey: assign live ranges to registers in priority order, and when a conflict arises either evict a lower-priority range or split the current one at a point that resolves the conflict. Splitting is what buys the 10 to 20% on register-pressure-bound code that document 01 records. Coalescing follows George and Appel's iterated register coalescing so that the moves introduced by block parameters mostly disappear.

Whether the second allocator is ours or `regalloc2` as a dependency is open question three in document 19. The interface is the deciding factor: a `run(env, program) -> allocations + inserted_moves` signature, which is regalloc2's own API shape, so the answer can change without touching anything else. The argument for using regalloc2 is that it is mature and ships with a *checker*, a verifier that independently confirms the allocation preserves the program's dataflow, which is worth a great deal. The argument against is a dependency on a crate whose priorities are Wasmtime's.

Either way, **we run an allocation checker in debug and CI builds**. An independent check that every use reads the value its SSA definition produced catches the entire class of register allocation bugs, which are otherwise among the hardest in a compiler to diagnose because the symptom appears far from the cause.

**Before allocation**: critical edges are split, per document 08's invariant. **After allocation**: parallel moves on edges are sequenced correctly, including cycles, which need a scratch register or an exchange, a small algorithm that is wrong in a startling number of compilers.

## 10.5 Scheduling

A list scheduler over the dependence DAG within each basic block, at `-O2` and above only.

The target supplies a machine model: per-opcode latency, functional unit occupancy, and issue width. The scheduler's priority is critical-path length with a tiebreak on register pressure, because a scheduler that ignores pressure creates spills that cost more than the latency it hid.

This matters much less than it used to. Modern out-of-order cores reorder aggressively and the win from block-level scheduling on x86-64 and AArch64 application cores is small. It matters for in-order targets, and RISC-V implementations in the wild include in-order cores, so the machinery exists and is enabled per subtarget rather than per architecture.

Machine models are data files, not code, and an incorrect model produces slow code rather than wrong code, which is the right failure mode and which means we can ship approximate models and refine them with measurements.

## 10.6 Block layout

Ordering blocks so that hot edges become fall-through. The algorithm is chain construction over the CFG weighted by block frequency from document 09's profile data or from static heuristics when no profile exists: build chains greedily from the highest-weight edges, then order the chains.

Choosing the order is only half of it. The other half is that a machine has no such thing as an arm of a branch, so once the order exists every edge the order did not put next to each other has to become a jump. That is the same pass, it runs after the prologue and the epilogue are in, and it is where a conditional branch stops being one instruction that says where both arms go and becomes a comparison and a jump that takes one of them.

Where a jump goes stays on the block rather than moving into the instruction, because an instruction is twenty four bytes by the decision in document 03 and a block reference does not fit in one. So after the layout has run a block's arms mean something they did not mean before: a block with no arms returns, a block with one arm falls into it when it is the block laid out next and jumps to it when it is not, and a block with two arms ends in a conditional jump to the first arm and falls into the second, which the layout guarantees is the block laid out next. That guarantee is what stops a block ever ending in two jumps, and the case where neither arm can be laid out next is handled by making an empty block for the second edge to jump from, which is the critical edge splitting above done for a different reason and at the same cost the second jump would have been.

Which arm is which is therefore no longer which way the condition went. A block that falls into the arm the condition is true for needs the jump taken when it is false, so a target names both conditional jumps and the layout picks one and puts the arms in the order that matches. What the condition meant survives in the opcode.

Cold blocks, those reachable only through a `__builtin_expect(x, 0)` branch, or marked cold by profile, or ending in a `noreturn` call, are moved to the end of the function and, with `-ffunction-sections`, into a `.text.unlikely` section. On a large program this is worth several percent from instruction cache behavior alone, and it is nearly free.

## 10.7 ABI lowering and frames

Argument and return value lowering happens at the IR-to-MIR boundary, driven by the psABI descriptions in document 12. By the time MIR exists, every call has its arguments in the right registers or stack slots and every return value is where the ABI says.

Frame layout is computed after register allocation, when the spill slots are known: incoming arguments, saved callee-saved registers, spill slots, local `alloca`s sorted by alignment, and the outgoing argument area. Prologue and epilogue are generated from the frame description, including the frame pointer when `-fno-omit-frame-pointer` or the target's ABI requires it, stack probing when `-fstack-clash-protection` is on or the frame exceeds a page, the stack protector canary when `-fstack-protector` is on, and the CET or PAC/BTI instructions when `-fcf-protection` or the AArch64 branch protection flags require them.

A fixed-size `alloca` is built from the frame rather than matched by a rule, for the same shape of reason a call is built from the calling convention: what a rule may replace a term with is instructions, and what a local needs first is bytes, which the rule language has no way to ask for. So selection records the size and alignment each one asked for and writes the single instruction that computes its address, and how far into the frame that memory sits is written into that instruction after the frame is laid out, which is the earliest moment anybody knows. Nothing folds an `alloca` into anything along the way, because there is no rule and no term name for one, and what it hands the rest of the function is an ordinary register holding an address that every addressing mode rule then applies to as usual.

Dynamic stack allocation for VLAs and `alloca` adjusts the stack pointer at runtime and requires a frame pointer, and the interaction between a dynamic frame, the outgoing argument area and stack realignment for over-aligned locals is a place where compilers historically get things subtly wrong. Each combination gets a test.

**Unwind information** is emitted alongside the prologue: DWARF CFI on ELF and Mach-O, and the SEH unwind tables on Windows. The kernel additionally wants ORC-compatible output, which is generated by `objtool` from our objects rather than by us, and which imposes constraints on what the prologue may look like. Document 14 covers this.

## 10.8 Target descriptions

A target is a `TargetInfo` from document 04, a register file description, a machine model, a lowering rule set, an ABI description from document 12, an instruction encoder from document 11, and a relocation set. Nothing else. No target-specific code in any pipeline crate; `xtask` enforces it.

**x86-64** is the first target because it is the most-tested platform, the ABI is well documented, and the corpus is largest. Its difficulties are the instruction encoding, which is genuinely complicated, the two-address instruction forms that constrain the allocator, and x87 for `long double`.

**AArch64** is second and is in many ways easier: fixed-width encoding, three-address instructions, a regular register file. Its difficulties are the addressing mode variety, the immediate encoding rules for logical operations, and Apple's ABI divergences from AAPCS64.

**RISC-V 64** is third and is the simplest to encode and the most demanding on the optimizer, because the base ISA has no addressing mode beyond register-plus-immediate and no condition codes, so the quality of the generated code depends more heavily on the middle end. That makes it a useful canary: a regression that shows up first on RISC-V is usually a middle-end regression.

Adding a fourth target should be a rule set and four data files. Whether that is actually true is testable, and the M10 exit criterion in document 17 is bringing up a fourth target (the candidate is 32-bit ARM or i686) with a measured effort number, which either validates the abstraction or reveals what leaked.

## 10.9 Peepholes after selection

A final pass over MIR applying target-specific peepholes: redundant move elimination, combining a compare and a branch, folding an address computation into a memory operand's addressing mode, replacing a load-modify-store triple with a read-modify-write instruction on x86, and choosing shorter encodings at `-Os`.

These are rules in the same DSL with the same verification obligation, because a peephole over machine instructions is exactly as capable of miscompiling as a lowering rule and exactly as amenable to being proved.

## 10.10 What the backend does not do

No global scheduling across basic blocks, no software pipelining, no trace scheduling. No register allocation across function boundaries. No machine outliner before 1.0, except in `-Oz` where it is the single largest size win and is therefore reconsidered in document 19.

No JIT. The IR and the encoders would support one and it is an obvious future direction, but a JIT has an entirely different set of requirements around memory protection, patching and unwinding, and adding it before 1.0 would compromise the ahead-of-time design.
