# 39. Register allocation

The last hard problem in the backend and the one whose output quality most directly determines
whether spec 00's within-10%-of-`gcc -O2` target is met on register-pressure-bound code. It is also
the place where a bug is hardest to diagnose, because the symptom is a value that is wrong long after
the instruction that lost it.

GCC's is 59,616 lines across two allocators and the corpse of a third: `gcc/reload1.cc` 9,099 and
`gcc/reload.cc` 7,381 for the legacy reload pass, `gcc/lra-constraints.cc` 8,096, `gcc/ira.cc` 6,286,
`gcc/ira-color.cc` 5,392, `gcc/ira-build.cc` 3,577, `gcc/ira-costs.cc` 2,774, `gcc/lra.cc` 2,709,
`gcc/lra-assigns.cc` 1,893, `gcc/ira-lives.cc` 1,850, `gcc/lra-lives.cc` 1,575,
`gcc/lra-eliminations.cc` 1,564, `gcc/lra-remat.cc` 1,353, `gcc/ira-emit.cc` 1,328,
`gcc/ira-conflicts.cc` 909, `gcc/lra-spills.cc` 879, `gcc/lra-coalesce.cc` 360, plus 2,349 lines of
headers.

Spec 10.4 already fixes rucc's design: two allocators behind one interface, single-pass linear scan
for `-O0`, backtracking with live-range splitting following regalloc2 and IonMonkey for `-O1` and
above, coalescing per George and Appel, a checker in debug and CI builds, and an open question about
whether to use `regalloc2` as a dependency. This document tests that design against GCC and against
document 05.4's research, and answers the question document 05.4 explicitly deferred here.

## 39.1 GCC has two allocators, and so should everyone

The structure is worth stating plainly because it is not what the names suggest. `pass_ira` assigns
hard registers to pseudos. `pass_reload`, which today runs LRA, then makes every instruction
*satisfy its constraints*, generating reload instructions, spilling, rematerializing and substituting
until a fixpoint is reached.

These are two different problems and GCC separates them completely. The first is an optimization: any
assignment is correct, some are better. The second is a legalization: the code is not valid until it
is done, and it must terminate.

**That separation is the single most important structural lesson in this document**, and it is not
the one spec 10.4 currently encodes. Spec 10.4's two allocators are fast-and-poor versus slow-and-good,
which is the throughput axis. GCC's two are assign-well versus make-legal, which is a different axis
entirely, and rucc needs both splits: two *assignment* strategies behind one interface, and one
legalization step that runs after either of them.

The legalization step is where the operand constraints of spec 10.1 are enforced: an operand that
must be in a particular register class, a two-address instruction whose destination must be one of
its sources, an addressing mode whose displacement must fit. rucc's position is better than GCC's
here because rucc's instruction selection already produced instructions that satisfy their
constraints by construction, the rules having been written that way, so the legalization step has
only the constraints that allocation itself can violate: an operand that ended up in memory when the
instruction needs a register. But it is still a step, it still needs to terminate, and it should be
named rather than smeared into the allocator's rewrite loop.

## 39.2 IRA: regional graph colouring

`gcc/ira.cc:21`:

> The integrated register allocator (IRA) is a regional register allocator performing graph coloring
> on a top-down traversal of nested regions. Graph coloring in a region is based on Chaitin-Briggs
> algorithm. It is called integrated because register coalescing, register live range splitting, and
> choosing a better hard register are done on-the-fly during coloring.

The vocabulary, from the same comment, and each term has a rucc counterpart worth naming:

**Region.** "The entire function for the root region and natural loops for the other regions." So
allocation is done for the whole function, then improved for each loop, then each subloop, top down.
`gcc/ira.cc:131` explains the accumulation discipline that makes this sound: allocnos in an outer
region accumulate the costs and conflicts of the same pseudo in inner regions, so "attributes for
allocnos in a region have the same values as if the region had no subregions".

The point of regions is that a value can live in a register inside a hot loop and in memory outside
it, which is live-range splitting driven by loop structure rather than by conflict. rucc's
backtracking allocator gets splitting from conflict resolution instead, which is a different and
generally better mechanism, but the *loop-structure* motivation is worth keeping as a splitting
heuristic: a range that crosses a loop boundary and is not used inside the loop is a candidate for a
split at that boundary regardless of whether a conflict forced one.

**Allocno class and pressure class.** The register class an allocno may be assigned from, and the
classes over which pressure is measured. Two distinct notions, and GCC needs them separate because
its class lattice is a partial order with overlaps. The comment is candid about the cost of that:
Chaitin-Briggs requires that the sets of assignable hard registers "form a forest", and where they do
not, "we use some approximation to form the tree".

**rucc should not have overlapping register classes**, and this is the concrete recommendation from
this section. x86-64's are nearly a forest already, the awkward cases being the byte-addressable
subset and the fixed-register requirements of `div`, `shl` by a variable, and the string
instructions. Spec 10.1's per-operand fixed-register constraint handles the second class of case
without a register class at all, which is the right design and which avoids the approximation GCC
apologises for.

**Hard-register costs.** A vector per allocno, one entry per candidate register. "The cost of a
callee-clobbered hard-register for an allocno is increased by the cost of save/restore code around
the calls through the given allocno's life. If the allocno is a move instruction operand and another
operand is a hard-register of the allocno class, the cost of the hard-register is decreased by the
move cost."

That is the entire mechanism by which GCC does coalescing and callee-saved selection: **not as
separate passes, but as biases on a per-register cost vector.** rucc's allocator, following regalloc2,
uses hints rather than costs, which is the same idea with less resolution. The cost vector is
strictly more expressive and is worth considering, because it lets the allocator trade "this register
saves a move" against "that register avoids a callee-save" numerically rather than by precedence.

**Threads**, at `gcc/ira.cc:189`: "Thread is a set of non-conflicting colorable allocnos connected by
copies. Pushing thread allocnos one after another onto the stack increases chances of removing copies
when the allocnos get the same hard reg." A cheap, effective coalescing heuristic that costs an
ordering rather than a pass, and one rucc's priority ordering can adopt directly.

And **Briggs optimistic colouring**, which is the improvement over Chaitin that means a node is not
spilled when it is pushed, only when it cannot be coloured on the way back up. The equivalent in a
backtracking allocator is that eviction is preferred to spilling and spilling is the last resort,
which regalloc2 already does.

## 39.3 LRA: legalization as a fixpoint

`gcc/lra.cc:22` states the design goals, and they read as a repudiation of `reload`:

> The major LRA design solutions are:
> o division small manageable, separated sub-tasks
> o reflection of all transformations and decisions in RTL as more as possible
> o insn constraints as a primary source of the info (minimizing number of target-depended
> macros/hooks)

The second is the one to steal. `reload` kept its decisions in side tables and applied them at the
end, which made it undebuggable; LRA rewrites the instructions as it goes, so a dump between
iterations is a valid program. **rucc's allocator rewrite must have the same property**, and spec
10.1's `--emit=mir-final` is the hook for it, but the useful version dumps each iteration rather than
only the last.

The block diagram at `gcc/lra.cc:45` names the loop: remove scratches, update virtual register
displacements, transform to satisfy constraints, do inheritance and splitting transformations in EBB
scope, assign new and old pseudos, undo inheritance for spilled pseudos, coalesce memory-to-memory
moves, and iterate; then rematerialization, then spilled-pseudo-to-memory substitution, then hard
register substitution and devirtualization.

Three of those are worth naming for rucc.

**Iteration until no change.** Allocating a stack slot changes an address displacement, which can
make an instruction illegal, which needs a reload, which needs a register, which may need a spill,
which allocates a stack slot. This is a genuine fixpoint and it is why the legalization step of 39.1
must be separate: it has a termination argument and the assignment step does not need one. GCC's
termination comes from monotonicity, each iteration either changes nothing or moves something to
memory, and rucc's must too, with an iteration cap that is an internal error rather than a silent
give-up.

**Inheritance and splitting in extended basic block scope**, gated by
`lra-inheritance-ebb-probability-cutoff` `Init(40)` (`gcc/params.opt:464`). Inheritance is reusing a
value already reloaded into a register at a nearby point rather than reloading it again. It is
redundancy elimination on reload instructions and it is why document 37's post-allocation cleanup
list is short in GCC: LRA already did the local part.

**Rematerialization**, `gcc/lra-remat.cc`, 1,353 lines, enabled at `-O2` by `-flra-remat`
(`gcc/opts.cc:660`). Recomputing a value is cheaper than spilling and reloading it when the value is a
constant or a simple function of still-live values. Document 37.5 declined `gcc/early-remat.cc` on the
grounds that rematerialization belongs inside the allocator; this is the confirmation, and it is where
rucc should put it too. It is not day-one work but it should be a planned extension point rather than
a retrofit, which means the spill decision must be a function that can return "recompute" as well as
"store", from the first version.

## 39.4 The degradation path

`gcc/ira.cc:5756` is a small piece of code with a large lesson:

```c
lra_simple_p
  = (ira_use_lra_p
     && (num_used_regs >= (1U << 26) / last_basic_block_for_fn (cfun)
         || ((uint64_t) get_max_uid ()
             > (uint64_t) param_ira_simple_lra_insn_threshold * 1000)));
if (lra_simple_p)
  {
    flag_caller_saves = false;
    flag_ira_region = IRA_REGION_ONE;
    ira_conflicts_p = false;
  }
```

`ira-simple-lra-insn-threshold` is `Init(1000)` (`gcc/params.opt:344`) and is in units of 1,000, so a
function of more than about a million instructions, or one whose pseudo count times block count
exceeds 2^26, gets a simplified allocation: no live-range splitting, no regional allocation, no
conflict graph.

**Every allocator needs this and most implementations discover it after a bug report.** Register
allocation is the pass whose worst case is worst, its inputs come from machine-generated code and
from unrolled loops, and the honest response is not to make the good algorithm fast enough but to
have a second algorithm and a threshold at which to use it.

rucc has the second algorithm already, since spec 10.4's `-O0` linear scan exists. **The
recommendation is that the threshold exist too**: at any optimization level, a function above a size
bound uses the single-pass allocator, with a note in the dump saying so. The bound is measured rather
than guessed and document 42 owes the measurement.

The related bounds: `ira-max-loops-num` `Init(100)` (`gcc/params.opt:348`) caps regional allocation,
`ira-max-conflict-table-size` `Init(1000)` caps the conflict table at a gigabyte,
`lra-max-considered-reload-pseudos` `Init(500)` (:468) caps the spill candidate search, and
`lra-max-pseudos-points-log2-considered-for-preferences` `Init(30)` (:472) caps preference
computation at 2^30 pseudo-point pairs. Four separate quadratic blowups, four separate caps.

## 39.5 The SSA result, and what it changes

Document 05.4 records the finding and defers the argument here.

The interference graph of a program in SSA form is chordal (Hack, Grund and Goos, CC 2006, plus two
independent contemporaneous discoveries). Chordal graphs are optimally colourable in
O(omega(G) times |V|) along a perfect elimination order, and a reverse dominance order is such an
order. Two consequences follow and the second is the important one.

**The interference graph need never be built.** Colouring is a single dominator-order walk assigning
each definition a free register, which is what a linear scan over a dominator-order linearisation
already approximates. This is a compile-time result more than a code-quality one.

**The number of registers needed is exactly the maximum number of values live at any program point,
and that number is known before colouring.** This is the decoupling result: in a non-SSA program you
cannot know whether a colouring exists without trying, so Chaitin's algorithm interleaves spilling and
colouring and iterates. In SSA you can compute the register pressure at every point first, spill until
pressure is everywhere at most the number of registers, and *then* colour, with a guarantee that
colouring succeeds.

**So the pipeline is: spill, then colour, then coalesce, three separate phases, each with a clean
specification.** Spilling is "reduce maximum pressure to k", a well-defined optimization problem with
good heuristics. Colouring is a walk that cannot fail. Coalescing is a cleanup that removes moves
without raising pressure above k.

That is what document 05.4 meant by "the SSA-based decoupling of spilling from colouring from
coalescing is the part to take, independent of which colouring strategy is chosen", and it is worth
being precise about why it does not settle the `regalloc2` question.

**The catch, and it is why nobody ships pure SSA allocation.** The guarantee holds for a machine with
one uniform register class and no fixed-register constraints. Real machines have neither. A
two-address instruction, an operand pinned to `%rcx`, a call's clobber set, and a value that must live
in a specific class all break either the chordality or the "any free register works" step. Handling
them requires either repairing after the fact, which is where the code quality goes, or a colouring
that backtracks, which is where the guarantee goes.

**The conclusion for rucc, and it is a change of emphasis rather than of plan.** Keep spec 10.4's
backtracking allocator, which handles constraints natively and is what regalloc2 is. But take the
decoupling as an *organising principle*: compute pressure explicitly, make spilling a separate
decision with its own cost function and its own dump, and treat colouring as the phase that should
rarely fail. An allocator whose spill decisions are made inside its eviction loop cannot be measured
or tuned; one whose spilling is a phase can be.

And take the pressure computation itself as a deliverable used elsewhere. Document 27's LICM needs
it, document 38.1's scheduler needs it, document 32's vectorizer would need it. GCC has
`-fira-loop-pressure` and `-fira-hoist-pressure` (`gcc/common.opt:2267`, :2272) precisely because LICM
and hoisting want the allocator's pressure model. **One pressure model, computed once, consumed by
four passes**, is the right structure and it is cheaper than four approximations.

## 39.6 The `regalloc2` question, answered

Spec 10.4 leaves it open as question three in spec 19. On the evidence gathered here the answer is
**write our own, and steal the checker's design outright.**

The arguments for the dependency were maturity and the checker. Maturity is real. But spec 00's
constraint is that rucc is dependency-free, and taking one dependency for the single most
correctness-critical component in the compiler is the worst place to take it, because a bug there is
one that cannot be fixed locally and whose diagnosis requires understanding somebody else's
representation. The checker is the actually valuable artefact and it is separable: an independent
verifier that every use reads the value its SSA definition produced is a few hundred lines and does
not require the allocator it checks.

The argument against the dependency that this document adds is 39.1's: rucc needs an assignment phase
and a legalization phase, and regalloc2's interface is a single `run(env, program) -> allocations +
inserted_moves`, which is an assignment interface. The legalization step, the fixpoint of 39.3, has
to be rucc's regardless, and it is the half that touches spec 10.1's operand constraints, spec 10.7's
frame, and document 36.7's slot allocator. Owning the assignment half too keeps one component instead
of two that must agree.

This is a recommendation, not a decision spec 19 has taken, and it should be recorded as such in
document 43.

## 39.7 What rucc builds

**One pressure model**, per register class, per program point, computed from liveness. Consumed by
the spill phase, the scheduler, LICM and hoisting. Perhaps 300 lines.

**A spill phase**, reducing maximum pressure to the register count, with a cost function that
consults block frequency and that can answer "rematerialize" as well as "spill". Perhaps 400 lines.

**A backtracking assignment phase** following regalloc2 and IonMonkey, with priority ordering,
eviction, splitting at conflict points, per-register cost vectors rather than bare hints per 39.2,
and thread-ordered pushing per `gcc/ira.cc:189`. This is the large piece, perhaps 2,500 lines.

**A legalization phase**, iterative, monotone, capped, rewriting instructions so that every operand
satisfies its constraint, and reporting an internal error rather than looping. Perhaps 600 lines.
Smaller than LRA's 8,096 because selection already produced constraint-satisfying instructions.

**A parallel-move sequencer** for block-parameter arguments on edges, handling cycles with a scratch
register or an exchange, per spec 10.4. Perhaps 150 lines and it needs an exhaustive test over small
permutations.

**The slot allocator** of document 36.7, sharing frame slots between locals and spills using this
pass's own liveness.

**The checker**, run in debug and CI builds unconditionally.

**A degradation threshold** per 39.4, above which the single-pass allocator runs regardless of
optimization level.

**The single-pass allocator** of spec 10.3 and 10.4, which is already specified there including its
two look-past-the-interval heuristics.

## 39.8 How this is wrong

**A value is assigned a register that a call clobbers.** The ABI's clobber set is data, and a target
description error here produces wrong code that appears only when the callee happens to use that
register. The defence is that the clobber set is checked against the ABI document and that the
checker treats a call as defining every caller-saved register.

**The parallel move on an edge is sequenced wrongly.** Spec 10.4 already names this. Exhaustive test.

**Two live ranges of the same value get different registers and no move connects them.** The
splitting bug. This is exactly what the checker catches and it is the reason the checker is not
optional.

**A fixed-register constraint is satisfied by evicting a value that the same instruction reads.**
Two-address instructions and instructions with early-clobber outputs. Spec 10.1's use/def/early-def
roles exist for this and the failure is an instruction whose operand roles are declared wrong in the
target description.

**Rematerialization recomputes a value whose inputs have since changed.** The remat candidate's
inputs must be live and unmodified at the point of recomputation. GCC checks this and it is easy to
get wrong for a value derived from a frame address, since the frame is not final until later.

**The legalization fixpoint does not terminate.** 39.3's monotonicity argument. The cap turns a hang
into a diagnosable internal error, which is the difference between an unusable compiler and a
reportable bug.

**Spilling makes the wrong choice because frequencies are wrong.** A range spilled inside a loop
because static heuristics guessed the loop was cold. Document 11's profile quality field is what lets
the spill cost function be less confident when the numbers are guesses.

**Stack slots are shared between values whose ranges overlap.** Document 36.7's slot allocator uses
this pass's liveness, so an error in liveness becomes a wrong-code bug rather than a
poor-allocation bug. The checker should verify slot disjointness as well as register assignment.

**Compile time explodes on a generated function.** 39.4, and it is a certainty rather than a risk.

## 39.9 What it costs, and what to measure

The allocator is the backend's dominant compile-time cost and should be expected to be. GCC caps four
separate quadratics in it. rucc will find its own.

Document 42 owes seven numbers.

- **The allocator's share of `-O2` compile time**, and separately of `-O0` compile time, since spec
  10.3's throughput claim rests on the single-pass allocator being cheap.
- **Spills and reloads per thousand instructions against `gcc -O2`**, on each target. This is the
  most direct measure of allocation quality available without running the code, and it is the number
  that says whether spec 00's 10% target is reachable on pressure-bound code.
- **Moves remaining after coalescing**, which measures the thread ordering and the cost-vector bias
  of 39.2 separately from spilling.
- **The single-pass allocator's penalty**, rucc `-O0` against rucc `-O1` with only the allocator
  changed. Document 05.4's TPDE number is 1.64x and that is the yardstick.
- **The degradation threshold**, found by measuring allocation time against function size on
  generated inputs until the knee appears, rather than by copying GCC's million.
- **Rematerialization on and off**, once it exists, since it is the cheapest quality win in the list
  and its size should be known before it is prioritised.
- **How often the legalization fixpoint iterates**, distribution over the corpus. If it is almost
  always one, the phase is simpler than feared; if it has a tail, the tail is where the bugs are.

## 39.10 The decision

Spec 10.4 stands, with four additions and one answered question.

The additions: **separate the legalization fixpoint from the assignment phase**, per 39.1 and 39.3,
because they have different correctness obligations and only one of them needs a termination
argument. **Make spilling a phase with its own cost function and dump**, per 39.5's decoupling, so
that it can be measured. **Build one register-pressure model** and let LICM, hoisting and the
scheduler consume it. **Add a degradation threshold**, per 39.4, before the first bug report needs it.

The answered question: **write the allocator rather than depending on `regalloc2`**, and implement the
checker first, since the checker is the part of regalloc2 that was worth having and it is independent
of the allocator it checks. Recorded as a recommendation to spec 19's open question three rather than
as a settled decision, and collected in document 43.

The finding that carries: **GCC ships a graph-colouring allocator, a constraint-satisfaction
legalizer, and a degraded fallback, and it needs all three.** A plan with only the first is a plan
that has not yet met a two-address instruction or a machine-generated function.
