# 11. Profiles, branch prediction and block frequency

Almost every cost decision in this directory takes the form "is this code hot", and the answer
comes from here. Inlining (33), unrolling (29), block layout (38), register allocation spill
placement (39), if-conversion (22) and the size/speed choice within `-O2` all consume block
frequency, and if block frequency is wrong they are all wrong together in a way that is very hard
to attribute.

GCC spends about 16,400 lines: `gcc/predict.cc` at 4,847, `gcc/auto-profile.cc` at 4,905,
`gcc/tree-profile.cc` at 2,126, `gcc/value-prof.cc` at 1,962, `gcc/profile.cc` at 1,924 and
`gcc/profile-count.cc` at 597. Only the first is needed in M4; the rest is document 35's.

## 11.1 The best idea in GCC's profile machinery

`enum profile_quality` in `gcc/profile-count.h:30` attaches a provenance to every count and
probability. The values, in increasing order of trust: `UNINITIALIZED_PROFILE`, `GUESSED_LOCAL`,
`GUESSED_GLOBAL0_AFDO`, `GUESSED_GLOBAL0_ADJUSTED`, `GUESSED_GLOBAL0`, `GUESSED`, `AFDO`,
`ADJUSTED`, `PRECISE`.

So a `profile_count` is not a number. It is a number plus how much you should believe it, the
quality is stored in three bits packed alongside the value (`gcc/profile-count.h:162`), and
arithmetic on counts *degrades* the quality: combining a precise count with a guessed one gives a
guessed result (`gcc/profile-count.h:196` and following).

This is the design decision to copy and it is worth stating why. A compiler with real profile data
for half a program and heuristics for the other half will constantly compute with both. Without
quality tracking, one guessed count laundered through three arithmetic operations becomes
indistinguishable from measured data, and the inliner then makes an aggressive decision on a
fabricated number. With it, the consumer can ask, and consumers that should behave differently on
guesses than on measurements can do so. rucc's `Frequency` type carries a `Quality` and there is
no constructor that takes a bare number without one.

The corollary that GCC also gets right: a pass that transforms the CFG must *maintain* the counts,
and a pass that cannot maintain them honestly should downgrade the quality rather than invent a
number. Loop unrolling divides the header's count; jump threading splits a count across two paths;
if-conversion merges two. Each of those is arithmetic that either preserves the sum or does not,
and `-fdump-ir=` should print the count and quality on every block so a reviewer can check.

## 11.2 Static prediction, in the absence of a profile

`gcc/predict.def` defines 55 predictors through the `DEF_PREDICTOR (ENUM, NAME, HITRATE, FLAGS)`
macro. Each names a syntactic situation and a measured hit rate. A representative selection, with
GCC's numbers:

| Predictor | Hit rate |
|---|---:|
| `__builtin_expect` | very likely |
| `noreturn` call not taken | very likely |
| loop exit not taken | 89% |
| `fp_opcode`, a float comparison predicted a particular way | 90% |
| negative return value | 98% |
| null return value | 71% |
| loop guard taken | 73% |
| pointer comparison against null predicted non-null | 70% |
| block containing a call not taken | 67% |
| `continue` taken | 67% |
| `goto` taken | 66% |
| early return not taken | 66% |
| opcode values non-equal | 66% |
| const return | 65% |
| opcode values positive | 59% |
| indirect call | even |

Those numbers come from Ball and Larus's and Wu and Larus's measurements from the mid-1990s and
they have held up remarkably well, because they are facts about how people write programs rather
than about hardware.

Two combining mechanisms exist. **First match**: the predictors are ordered in `predict.def` and
the earliest applicable one wins, which is why the file's header comment
(`gcc/predict.def:28`) says the order is significant. **Dempster-Shafer**: `PRED_DS_THEORY`
combines multiple independent predictions into one probability rather than picking. GCC computes
both and uses first-match by default.

**What rucc builds.** Ten predictors, not 55, dropping every Fortran-specific one (there are ten)
and every one below a 65% hit rate, on the grounds that a 59% predictor moves a probability by
nine points and no downstream decision changes.

The ten: `__builtin_expect` and `__builtin_expect_with_probability`; `noreturn` and `cold`
attributes; loop exit not taken; loop guard taken; the pointer-null heuristic; negative return;
null return; call not taken; and `continue` taken. First match, ordered exactly as above, with
`__builtin_expect` first because a user who wrote it means it.

Two of those deserve comment. `noreturn` is the one that makes error paths cold, which is what
makes the whole hot/cold split work on real C, because C's error handling is `if (x) { report();
abort(); }`. And the `cold` and `hot` function attributes are the user's explicit statement and
must be honoured absolutely, not blended.

`max-predicted-iterations` at `Init(100)` (`gcc/params.opt:733`) caps how many iterations a loop
is statically predicted to run. Without a cap, a nested loop's header frequency grows
multiplicatively and overflows, which is the second most common way a frequency implementation
breaks.

## 11.3 From probabilities to frequencies

Edge probabilities are local. Block frequency is global and is what consumers actually want: how
often does this block execute relative to the function entry.

The computation is a linear system: the frequency of a block is the sum over incoming edges of the
source frequency times the edge probability, with the entry pinned at 1. On an acyclic CFG this
solves in one reverse-postorder pass. On a loop it does not, because the header's frequency depends
on the latch's, which depends on the header's.

The standard solution, and the one rucc should use, is Wu and Larus's: process the loop forest from
the innermost outward, and for each loop compute the *cyclic probability*, the probability of
going around again, then multiply the header's frequency by `1 / (1 - p)` to account for all
iterations. Clamp `p` away from 1, or a loop with an unpredicted exit produces infinity, and this
is the most common way a frequency implementation breaks.

Two details that turn out to matter more than the algorithm.

**Represent frequency as a fixed-point integer, not a float.** Spec 03's determinism rule makes
floating point in the compiler suspect, because the result depends on evaluation order and on the
host's excess precision. Frequencies feed cost comparisons and cost comparisons decide code
generation, so a frequency that differs in the last bit between two hosts is a reproducibility
failure. GCC uses a scaled integer for exactly this reason.

**Irreducible regions have no well-defined frequency.** Document 06.4 declines to transform them
and this is where the consequence lands: the blocks in an irreducible region get a frequency
computed by the acyclic method as if the back edges were absent, which is wrong but bounded, and
they are marked so a consumer can decline. GCC does the same.

## 11.4 Hot and cold

`hot-bb-frequency-fraction` at `Init(1000)` (`gcc/params.opt:232`): a block is hot if its frequency
is at least 1/1000 of the entry block's. `unlikely-bb-count-fraction` at `Init(20)`
(`gcc/params.opt:1246`) sets the unlikely threshold in profiled mode.
`hot-bb-count-ws-permille` at `Init(990)` (`gcc/params.opt:228`) is the LTO-mode definition: hot
blocks are those in the top 99% of the whole program's execution.

Note that those three definitions are not consistent with one another and cannot be. "Hot relative
to this function" and "hot relative to the program" are different questions and both are needed:
the register allocator wants the first, the section-placement decision in document 38 wants the
second, and without a whole-program profile the second is unanswerable.

**rucc's rule.** Two predicates, named differently so they cannot be confused: `is_hot_in_function`
and `is_hot_in_program`. The second returns "unknown" without profile data, and every caller
handles "unknown" explicitly. A boolean that silently means "hot, or we have no idea" is how cold
code ends up in the hot section.

## 11.5 What is not in M4

Instrumentation-based PGO (`gcc/tree-profile.cc`, `gcc/profile.cc`), sample-based AutoFDO
(`gcc/auto-profile.cc`) and value profiling (`gcc/value-prof.cc`) are all document 35's and all
post-M4. Spec 9.9 already scopes them.

What M4 owes them is the *shape*: the `Frequency` type with its quality field, the counts on blocks
and edges, the maintenance discipline in every transforming pass, and the dump format. Retrofitting
count maintenance into thirty passes after PGO arrives is the failure mode, and it is a real one:
GCC's profile maintenance bugs are mostly in passes written before profile quality tracking
existed.

The single M4 deliverable that guards this: a verifier check that, after every pass, the sum of
incoming edge counts equals the block count for every block, within a tolerance, and that edge
probabilities out of a block sum to one. Cheap, runs with the IR verifier, and it catches the pass
that split a block and forgot.

## 11.6 How this is wrong

**Frequencies overflow.** Nested loops multiply. The cap from 11.2 and saturating arithmetic in
the `Frequency` type are the defences, and saturation must be in the type rather than at the call
sites.

**Quality is laundered.** A guessed count is used in an arithmetic operation whose result is
labelled precise. The defence is that the arithmetic is on the type and degrades quality
automatically, as GCC's does, and that there is no way to construct a `Frequency` with a chosen
quality outside the module.

**A pass drops counts.** Blocks created by a transformation get a default frequency of zero or one,
and suddenly the hot loop body is cold. The verifier check in 11.5 catches the sum violation; it
does not catch a proportionally-wrong-but-consistent assignment, and nothing does except review.
This is the argument for printing counts in every dump.

**`__builtin_expect` is ignored or inverted.** Users notice this and it is a compatibility issue,
not just a performance one. The test is a corpus of `__builtin_expect` uses where the predicted
path is checked to be the fall-through in the emitted code.

**Static prediction is applied on top of a real profile.** If measured data says a branch is taken
30% of the time, no heuristic may override it. GCC's quality ordering encodes this and rucc's must
too: a predictor may only write a probability whose quality is `Guessed`, and only where the
existing quality is lower.

## 11.7 What it costs

Static prediction is one walk of the CFG applying ten pattern matches per branch. Frequency
propagation is one walk of the loop forest plus a reverse-postorder pass per loop. Both are
linear and neither will appear in a time report.

The cost is entirely in the maintenance burden on every other pass, which is not measurable by a
timer and is measurable by the verifier check in 11.5: how often it fires during development is
the real cost of this analysis, and it is worth paying.
