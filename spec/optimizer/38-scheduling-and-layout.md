# 38. Scheduling and block layout

Two decisions about order. Scheduling picks the order of instructions within a region so that the
machine's pipelines stay busy. Layout picks the order of blocks within a function so that the
branches the program actually takes are fall-throughs and the code it actually runs is contiguous.

They are in one document because they are the two places where the compiler reasons about the
machine's *front end* rather than its arithmetic, and because their relative importance has inverted
over thirty years. Scheduling mattered enormously on in-order machines and matters little on
out-of-order ones. Layout mattered little when programs fit in cache and matters a great deal now.

Sizes: `gcc/haifa-sched.cc` 9,294, `gcc/sched-deps.cc` 5,025, `gcc/sched-rgn.cc` 3,981,
`gcc/modulo-sched.cc` 3,386, `gcc/bb-reorder.cc` 3,085, plus `gcc/sel-sched*` for selective
scheduling and the per-target pipeline descriptions counted in document 36.6 at 17,382 lines for
x86 alone.

## 38.1 The list scheduler, and its tie-break list

`gcc/haifa-sched.cc:22` describes the algorithm, and the part worth having verbatim is the
tie-breaking order at :55, because it is a compressed statement of everything a scheduler cares about:

> The following list shows the order in which we want to break ties among insns in the ready list:
> 1. choose insn with the longest path to end of bb, ties broken by
> 2. choose insn with least contribution to register pressure, ties broken by
> 3. prefer in-block upon interblock motion, ties broken by
> 4. prefer useful upon speculative motion, ties broken by
> 5. choose insn with largest control flow probability, ties broken by
> 6. choose insn with the least dependences upon the previously scheduled insn, or finally
> 7 choose the insn which has the most insns dependent on it.
> 8. choose insn with lowest UID.

Spec 10.5 says "the scheduler's priority is critical-path length with a tiebreak on register
pressure, because a scheduler that ignores pressure creates spills that cost more than the latency it
hid." That is criteria one and two exactly. Criteria three, four and five are about interblock and
speculative motion, which rucc does not do. Criterion six is a locality heuristic, seven is a
fan-out heuristic, and eight is the determinism tiebreak, which rucc needs and which must be a stable
identifier rather than an allocation address.

**So rucc's scheduler is criteria one, two, six, seven, eight**, and that is the whole design. Five
comparison keys and a ready list.

The pass also notes, at `gcc/haifa-sched.cc:87`, something rucc must handle and which is easy to get
wrong:

> Having optimized the critical path, we may have also unduly extended the lifetimes of some
> registers. If an operation requires that constants be loaded into registers, it is certainly
> desirable to load those constants as early as necessary, but no earlier.

Hoisting a constant materialisation to the top of a block because it has no dependences and a long
path is the classic scheduler-induced spill.

The dependence graph is `gcc/sched-deps.cc`'s, 5,025 lines, and it distinguishes four kinds:
`REG_DEP_TRUE`, `REG_DEP_OUTPUT`, `REG_DEP_ANTI` and `REG_DEP_CONTROL`. **In rucc's MIR, which is in
SSA before allocation, output and anti dependences on registers do not exist.** They exist on memory,
on flags, and on any fixed physical register an instruction is pinned to, and they reappear in full
after allocation. That is the argument for scheduling before allocation; the argument against is that
before allocation the scheduler is guessing at pressure rather than measuring it, which is what
criterion two is for and what GCC's `sched-pressure-algorithm` parameter (`gcc/params.opt:1040`,
`Init(1)`, range 1 to 2) offers two answers to.

`gcc/params.opt:77` adds a parameter worth noting for what it admits: `cycle-accurate-model`,
`Init(1)`, "Whether the scheduling description is mostly a cycle-accurate model of the target
processor and is likely to spill aggressively to fill any pipeline bubbles." A target whose pipeline
model is approximate should say so, and the scheduler then trusts it less. Spec 10.5's plan to "ship
approximate models and refine them with measurements" needs exactly this flag, and it should exist
from the first model rather than being added when the first model turns out to be wrong.

## 38.2 What GCC does that rucc will not

**Interblock and speculative scheduling.** `gcc/sched-rgn.cc:22`: the pre-allocation run "performs
interblock scheduling, moving insns between different blocks in the same region", including
"speculative motions, including speculative loads", and "motions requiring code duplication are not
supported". Regions are bounded by `max-sched-region-blocks` `Init(10)` and `max-sched-region-insns`
`Init(100)` (`gcc/params.opt:765`, :769), with `sched-spec-prob-cutoff` `Init(40)`
(`gcc/params.opt:1044`) gating speculation on branch probability.

Spec 10.10 already excludes global scheduling. The measurement in 38.7 is what would justify
revisiting that, and the prior is that it will not.

**Selective scheduling**, `selsched-max-lookahead` `Init(50)`, which is a different scheduler
entirely, used by Itanium and effectively nothing else.

**Modulo scheduling**, 3,386 lines, excluded by spec 10.10.

**Two scheduling passes.** GCC runs `pass_sched` before allocation and `pass_sched2` after. rucc runs
one, and the choice of which is 38.5's.

## 38.3 What the levels say, and the x86 fact

| Flag | Enabled at | `gcc/opts.cc` |
|---|---|---:|
| `-freorder-blocks` | `-O1` and above | 604 |
| `-freorder-blocks-algorithm=stc` | `-O2` and above, speed only | 689 |
| `-freorder-functions` | `-O2` and above | 664 |
| `-falign-functions`, `-falign-jumps`, `-falign-labels`, `-falign-loops` | `-O2` and above, speed only | 684 to 687 |
| `-fschedule-insns` (before allocation) | `-O2` and above, speed only | 697 |
| `-fschedule-insns2` (after allocation) | `-O2` and above, including `-Os` | 668 |

Two things fall out of that table.

**Block reordering is on at `-O1`.** It is one of the earliest optimizations GCC enables, before
almost anything in documents 14 through 24. It is nearly free and it always helps. rucc's spec 10.6
puts layout in the optimizing path; on this evidence **it should be in the `-O0` path too**, at least
in the reverse-postorder form spec 10.3 already describes, and the two are different points on one
mechanism rather than two mechanisms.

**Post-allocation scheduling is on even at `-Os`**, while pre-allocation scheduling is speed-only.
That asymmetry is because post-allocation scheduling cannot change code size and pre-allocation
scheduling can, by causing spills.

And the fact that decides the priority of this whole area:
`gcc/config/i386/i386-options.cc:2831` sets both scheduling flags to zero when `!TARGET_SCHEDULE`,
with the comment "When scheduling description is not available, disable scheduler pass so it won't
slow down the compilation and make x87 code slower."

**On modern x86-64 tunings GCC does schedule**, but the existence of that switch, and the fact that
`gcc/config/i386` ships nineteen separate pipeline models to make scheduling worthwhile, says how
much machinery the payoff requires. Spec 10.5's judgement, that block-level scheduling on
out-of-order application cores wins little and that RISC-V in-order cores are the real customer,
survives contact with the source.

## 38.4 Block layout: the software trace cache algorithm

`gcc/bb-reorder.cc:20` names the three things in the file: reorder blocks, partition blocks into hot
and cold, and duplicate computed gotos. Two algorithms for reordering, "simple", which minimises
executed unconditional branches, and "software trace cache", which "also copies code, and in general
tries a lot harder to have long linear pieces of machine code executed". STC is what `-O2` selects.

The algorithm, from `gcc/bb-reorder.cc:32`:

> This (greedy) algorithm constructs traces in several rounds. The construction starts from "seeds".
> The seed for the first round is the entry point of the function... Then the algorithm repeatedly
> adds the most probable successor to the end of a trace. Finally it connects the traces.
>
> There are two parameters: Branch Threshold and Exec Threshold. If the probability of an edge to a
> successor of the current basic block is lower than Branch Threshold or its count is lower than Exec
> Threshold, then the successor will be the seed in one of the next rounds. Each round has these
> parameters lower than the previous one. The last round has to have these parameters set to zero so
> that the remaining blocks are picked up.

Then the loop handling, at `gcc/bb-reorder.cc:53`:

> If the successor has been visited in this trace, a loop has been found. If the loop has many
> iterations, the loop is rotated so that the source block of the most probable edge going out of the
> loop is the last block of the trace. If the loop has few iterations and there is no edge from the
> last block of the loop going out of the loop, the loop header is duplicated.

Four things to take, and they are the whole design of rucc's layout pass.

**Multiple rounds with descending thresholds** is better than one greedy pass, and it is cheap. A
first round that only follows edges above 90% builds the trunk; later rounds pick up the rest. Spec
10.6 says "build chains greedily from the highest-weight edges, then order the chains", which is one
round; the rounds structure is a refinement worth taking because it costs almost nothing.

**Loop rotation belongs to layout, not just to the middle end.** Document 26's canonicalization
rotates loops into do-while form in the IR; this is the machine-level counterpart, arranging the trace
so the loop's exit is at the bottom and the back edge is a backward branch, which is what every branch
predictor's static prediction expects.

**Loop header duplication for short loops** is the layout-level version of peeling, and it is
notable that it is conditioned on there being no exit from the last block, which is the shape where
duplication is free.

**STC copies code.** That is the difference between "simple" and "stc" and it is why "stc" is
speed-only. rucc's first layout pass should be the non-copying version, with duplication as a later
refinement measured on its own.

## 38.5 Hot and cold, and alignment

**Partitioning.** `-freorder-blocks-and-partition` splits a function's blocks into two sections, with
the cold half emitted into `.text.unlikely` (`gcc/varasm.cc:638`). Spec 10.6 already commits to this,
identifying cold blocks as those reachable only through a `__builtin_expect(x, 0)` branch, marked cold
by profile, or ending in a `noreturn` call, and claims "several percent from instruction cache
behavior alone".

`gcc/opts.cc:1303` records a dependency worth copying: turning on partitioning turns on
`-freorder-functions`. Splitting a function into hot and cold halves is pointless if the halves are
not then gathered with other hot and cold code, which is what function-level ordering does through
`.text.hot` and `.text.unlikely` section names and the linker's section ordering.

**Function reordering** is `-O2`. In GCC it is a matter of emitting each function into a named
section chosen by its profile, and letting the linker group them. It is nearly free at the compiler's
end, and it is where document 35.3's `ipa-locality-cloning` work is heading at greater expense.

**Alignment.** Four flags, all `-O2` speed-only, plus two parameters:
`align-loop-iterations` `Init(4)` (`gcc/params.opt:25`), "Loops iterating at least selected number of
iterations get loop alignment", and `align-threshold` `Init(100)` (:29), "Select fraction of the
maximal frequency of executions of basic block in function given basic block get alignment".

The second is the interesting one: alignment is applied to a block whose execution frequency is at
least a hundredth of the function's hottest block. Padding is size, so it is spent where it pays.
This is a small, self-contained, profile-driven decision, it costs perhaps eighty lines, and it is
worth having because instruction fetch on x86 is line-granular and a loop straddling a line boundary
pays on every iteration.

**Branch shortening.** `pass_shorten_branches` and `pass_compute_alignments`, near the end of
`passes.def`, are where instruction lengths are finalised. This is not an optimization, it is a
fixpoint: alignment padding changes branch distances, branch distances change which branch encoding is
legal, and encoding changes lengths, which changes padding. rucc's assembler owns this, per spec 11,
and the only thing this document contributes is the observation that **alignment and branch shortening
must be solved together or the result does not converge**, and that the standard answer is to iterate
to a fixpoint from an over-approximation, shrinking only, so that termination is guaranteed.

## 38.6 What rucc builds

**Layout, at every optimization level.** At `-O0`, spec 10.3's reverse postorder with successors
walked in reverse. At `-O1` and above, the trace construction of 38.4 with descending thresholds,
loop rotation, and no code duplication. Roughly 600 lines, and it is the higher-value half of this
document by a wide margin.

**Hot and cold partitioning at `-O2`**, per spec 10.6, with `.text.unlikely` and, with
`-ffunction-sections`, `.text.hot` for the ordering that makes it worth doing.

**Alignment at `-O2`, speed only**, driven by frequency with a threshold, per 38.5.

**Scheduling at `-O2` and above, per subtarget, one pass, after register allocation.** This is a
change from a natural reading of spec 10.5, which does not say where the scheduler runs, and the
reasoning is worth recording.

Before allocation, the scheduler sees no register anti-dependences, so it has more freedom, and it
must guess at pressure. After allocation, it sees exactly the code that will execute, including
spills and reloads, and its pressure question is already answered, but physical registers reintroduce
every anti-dependence and the freedom mostly evaporates. GCC runs both and gates the first on speed
only.

For rucc the deciding factors are that a pre-allocation scheduler that gets pressure wrong causes
spills, which is a code-quality regression that is hard to attribute; that rucc's allocator does
live-range splitting, which is disturbed by a scheduler that has moved definitions away from uses;
and that the customer for scheduling is in-order RISC-V cores, where the post-allocation code is what
stalls. **One scheduler, after allocation, before the layout freeze.** If measurement later shows a
pre-allocation pass is worth it on some subtarget, the criteria of 38.1 are the same and the pass is
the same code with a different dependence graph builder.

Roughly 800 lines for the scheduler plus the per-target model format.

**And the `cycle-accurate-model` flag** of 38.1, per target model, from the first model.

## 38.7 How this is wrong

**The scheduler lengthens live ranges and causes a spill.** Criterion two exists for this and it is
still the dominant failure mode. Post-allocation scheduling avoids it entirely, which is the third
argument for 38.6's placement.

**The scheduler moves a load above a store it aliases.** Memory dependences are the scheduler's
weakest input, and `gcc/haifa-sched.cc:71` is candid: "Only if we can be certain that memory
references are not part of the data dependency graph... can we move operations past memory references.
To first approximation, reads can be done independently, while writes introduce dependencies." rucc's
MIR carries the `mem` operand from the IR, per document 09, so the dependence is explicit rather than
recomputed, which is a real advantage and should be used rather than rebuilding an aliasing query at
machine level.

**The scheduler moves an instruction across a flags definition.** Flags are a single resource with
many definitions and short live ranges. In rucc they are an operand, so this is an ordinary
dependence, and the failure is a target description that omitted a clobber, which is the same root
cause as 37.7's.

**The pipeline model is wrong.** Spec 10.5's position, that "an incorrect model produces slow code
rather than wrong code, which is the right failure mode", is correct and is the reason this area is
safe to approximate. The `cycle-accurate-model` flag is how the approximation declares itself.

**Layout puts a hot block in the cold section.** With static heuristics this is a real risk, and
`-fprofile-partial-training` (document 35.5) exists because GCC hit it. The rucc rule should be that
a block is cold only on positive evidence, `__builtin_expect`, a `noreturn` successor, or a profile,
and never merely because a heuristic guessed low.

**Layout leaves a block with two jumps.** Spec 10.6 already handles this: a block with two arms ends
in a conditional jump to the first and falls into the second, and where neither arm can be laid out
next, an empty block is created for the second edge. That invariant needs an assertion after layout,
not a comment.

**Alignment padding pushes a branch out of range.** 38.5's fixpoint. Iterating from an
over-approximation and only shrinking is what guarantees termination, and doing it the other way
around is a hang, not a miscompile, which is at least easy to notice.

**Cold-section placement breaks unwinding.** A function split across two sections has two ranges, and
the CFI and the exception tables must describe both. This is a real GCC bug class and it is why
`gcc/opts.cc:1562` through :1604 disable `-freorder-blocks-and-partition` under several conditions and
warn when the user asked for it explicitly. rucc must do the same and must have a test that unwinds
through a split function.

## 38.8 What it costs, and what to measure

Layout is one pass over the CFG with a priority queue, linear in edges. Alignment is a walk.
Scheduling is per block, quadratic in the worst case in block size, which is why GCC has
`max-sched-ready-insns` `Init(100)` (`gcc/params.opt:761`); rucc needs the same bound.

Document 42 owes six numbers, and the first three are the ones that decide whether the scheduler is
built at all.

- **Scheduling on and off**, on x86-64, AArch64 and RISC-V separately. The expectation from spec 10.5
  is near zero on the first two and material on the third. If that holds, the scheduler is
  RISC-V-only and its priority drops accordingly.
- **The pipeline model's sensitivity**: the same corpus with a deliberately crude model, all latencies
  one, against the tuned model. This bounds how much accuracy is worth buying.
- **Post-allocation against pre-allocation scheduling** on the one target where scheduling pays,
  which settles 38.6's placement decision with evidence rather than argument.
- **Layout on and off**, and STC-style traces against plain reverse postorder. This is the number
  spec 10.6's design rests on and it is cheap to take.
- **Hot and cold partitioning on and off**, against spec 10.6's claim of several percent, measured on
  a large program where instruction cache behaviour is the binding constraint rather than on a
  microbenchmark where it is not.
- **Alignment on and off**, and code size cost, since alignment is the one item here that trades size
  for speed and the trade should be quantified before `-O2` takes it.

## 38.9 The decision

Layout first and layout everywhere: it is on at `-O1` in GCC, it is nearly free, it helps every
target, and it is the half of this document that pays. Traces with descending thresholds, loop
rotation, no duplication in the first version, hot and cold partitioning at `-O2`, frequency-driven
alignment at `-O2` speed only.

Scheduling second, conditional, and one pass after register allocation rather than GCC's two. Its
justification is in-order cores, its measurement is 38.8's first number, and its cost model is a data
file that declares its own accuracy.

The finding worth carrying forward: **the tie-break list at `gcc/haifa-sched.cc:55` is the complete
specification of a list scheduler in eight lines**, and five of the eight are all rucc needs. A
scheduler is a small pass with a large data file behind it, and the file is where the work is.
