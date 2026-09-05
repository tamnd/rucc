# 03. The optimization levels

A level is a promise to the user about a tradeoff, and it is the only part of the optimizer most
people ever interact with. GCC has eight of them and the mapping from a level to behaviour is
three separate mechanisms layered on each other, which is worth understanding in detail because
rucc has to reproduce the *observable* part of it exactly while being free to differ everywhere
else.

## 3.1 How GCC actually decides what a level does

Three mechanisms, in order.

**The level is turned into four integers.** `gcc/opts.cc:750` scans the decoded command line for
`-O`, `-Os`, `-Oz`, `-Og` and `-Ofast` and sets `optimize`, `optimize_size`, `optimize_fast` and
`optimize_debug`. The conversions matter. `-Os` sets `optimize_size = 1` and then, at
`gcc/opts.cc:785`, *forces* `optimize` to 2, with the comment "Optimizing for size forces optimize
to be 2". `-Oz` does the same and additionally sets `optimize_size` to 2. `-Og` sets `optimize` to
1 and `optimize_debug` to 1. `-Ofast` sets `optimize` to 3 and `optimize_fast`. A bare `-O` is
`-O1`. `-O` with a number above 255 is clamped to 255, which is a nice piece of defensive coding
and also means `-O99` is legal and means `-O255`.

The consequence a program can see: `__OPTIMIZE__` is defined whenever `optimize` is nonzero, and
`__OPTIMIZE_SIZE__` whenever `optimize_size` is. So `-Os` defines both, and code that tests
`__OPTIMIZE__` to decide whether `__builtin_constant_p` will fold, which glibc's headers do
extensively, takes the optimizing path under `-Os`. rucc must match this and the test is in
document 42's macro suite.

**A table maps level bands to flag defaults.** `default_options_table` at `gcc/opts.cc:587` is
about 140 rows, each naming a band and an option. The bands are the interesting part, because they
are not simply "at this level and above":

| Band | Means |
|---|---|
| `OPT_LEVELS_1_PLUS` | `-O1` and up, including `-Og`, `-Os`, `-Oz` |
| `OPT_LEVELS_1_PLUS_NOT_DEBUG` | `-O1` and up but not `-Og` |
| `OPT_LEVELS_2_PLUS` | `-O2` and up, including `-Os` and `-Oz` |
| `OPT_LEVELS_2_PLUS_SPEED_ONLY` | `-O2` and up but not `-Os`, `-Oz` or `-Og` |
| `OPT_LEVELS_3_PLUS` | `-O3` and `-Ofast` |
| `OPT_LEVELS_FAST` | `-Ofast` only |

`OPT_LEVELS_2_PLUS_SPEED_ONLY` is the band that carries the whole size/speed split: function and
loop alignment, `-foptimize-strlen`, the software-trace-cache block reordering algorithm, both
vectorizers, and the pre-register-allocation scheduler. Everything else at `-O2` is also on at
`-Os`, which is the fact people find surprising and which explains why `-Os` in GCC is much closer
to `-O2` than to `-O1`.

**Each pass has a gate.** The table sets flags; a pass then decides for itself whether to run,
usually by testing its own `-f` flag but often by testing `optimize` or `optimize_size` directly.
This third layer is why reading the table is not sufficient to know what runs, and it is also why
GCC's `-fdump-passes` exists.

## 3.2 The `-O3` parameter bump, which is the whole difference

`-O3` turns on thirteen flags. That is the part everybody knows. The part that matters more is
five rows at `gcc/opts.cc:718` that do not turn anything on, they just make the inliner braver:

| Parameter | Default | At `-O3` |
|---|---:|---:|
| `max-inline-insns-auto` | 15 | 30 |
| `early-inlining-insns` | 6 | 14 |
| `inline-heuristics-hint-percent` | 200 | 600 |
| `inline-min-speedup` | 30 | 15 |
| `max-inline-insns-single` | 70 | 200 |

Doubling the automatic inline size, nearly tripling the early inline budget, halving the speedup a
call site must promise, and almost tripling the single-function cap. Document 33 argues that this,
and not vectorization, is where most of `-O3`'s win over `-O2` comes from on the kind of code spec
02 says rucc is optimizing for, and it is a much cheaper thing to implement.

## 3.3 The levels rucc defines

Spec 9.1 names six pipelines plus `-Og`. The code has six, in `rucc_session::OptLevel` at
`crates/rucc-session/src/lib.rs:43`: `O0`, `O1`, `O2`, `O3`, `Os`, `Oz`.

**There is no `-Og`, and spec 9.1 says there should be.** This is the first of the departures
document 43 tracks. `-Og` is not decoration: it is the level a debugger user actually wants,
Linux distributions build debug packages with it, and it is the level whose contract is easiest to
state (nothing that moves code across a statement boundary) and therefore easiest to keep. It
should be added in M4 alongside the first real pass set, because retrofitting a level after twenty
passes exist means auditing twenty passes for debug safety instead of deciding once per pass as
it lands. The work is a variant on the enum, a row in the level table, and a `debug_safe()`
method on `Pass` that `-Og` filters by.

**There is no `-Ofast` and there should not be.** `-Ofast` is `-O3` plus `-ffast-math` plus
`-fallow-store-data-races` plus `-fno-semantic-interposition`, all of which are flags rather than
levels, and GCC itself now discourages it. rucc should accept `-Ofast` for compatibility and
expand it to that flag set rather than model it as a seventh pipeline. That is a driver change,
not an optimizer one, and it belongs in `spec/04-driver-and-cli.md`.

## 3.4 The pipelines, written out

Today, from `crates/rucc-opt/src/pipeline.rs:28`, every level above `-O0` is the single-element
list `["fold"]` and `-O0` is empty. That is honest scaffolding and it is what M4 fills in. What
follows is the target: the list each level should hold at the end of M4, in order. Passes not yet
written are named anyway, because the list is the specification.

**`-O0`.** `mem2reg`. That is the entire pipeline and it is deliberate. SSA construction happens
during lowering per `spec/08-ir.md`, so the only thing left is promoting the allocas the front end
emitted for address-taken-looking locals that turned out not to be. No dominator tree, no loop
forest, no alias analysis, nothing computed on demand because nothing asks. This is the pipeline
the 2x-over-`clang -O0` claim in spec 02 rests on and document 42 gates it.

**`-Og`.** `mem2reg`, `fold`, `simplify-cfg` restricted to removing empty and unreachable blocks,
local CSE within a block, and DCE. Nothing that moves a computation between statements, so every
variable still has a home at every point a breakpoint can land.

**`-O1`.** `mem2reg`, `simplify-cfg`, `sroa`, `egraph` (one round), `gvn`, `dce`, loop
canonicalization, `licm`, `simplify-cfg`, `dce`. Inlining at a conservative threshold, before all
of it. Dominators, the loop forest and the cheap alias layers get computed; Memory SSA and the
points-to analysis do not.

**`-O2`.** Inline with the full cost model, then `sroa`, `egraph`, `sccp`, `gvn`, `pre`,
`jump-threading`, `dse`, `dce`, then loop canonicalization, `licm`, `indvars`, `unroll`, then
`egraph` again, `if-conversion`, `slp`, `dce`, `tail-calls`. Memory SSA and the whole alias stack
including the module-wide points-to analysis. The second e-graph round after the loop pipeline is
the point of spec 9.1's "two rounds around the loop pipeline": loop transformations create
address arithmetic that the first round could not have seen.

**`-O3`.** `-O2` with the parameter bump from 3.2, plus `loop-vectorize`, plus `interchange` and
`distribute` behind the dependence analysis, plus function specialization.

**`-Os`.** `-O2`'s list with `unroll` and both vectorizers removed and the cost model switched to
size. Not `-O1`'s list. This follows GCC and it follows for the same reason: PRE, GVN, SROA and
jump threading all usually shrink code, and a level that skips them to save compile time is
`-O1` wearing a different name.

**`-Oz`.** `-Os` plus the outliner, and instruction selection preferring the shorter encoding at
every choice, which is document 36's problem rather than this one's.

## 3.5 The flags that must be accepted and do nothing

GCC has roughly 250 `-f` optimization flags. rucc will have perhaps forty passes. The remaining
two hundred have to be handled and the handling has to be a decision rather than an oversight.

The rule is the one in document 01.3: accept, record, and report. `--print-pipeline` prints three
sections, the passes this level runs, the passes this level does not run, and the flags accepted
with no pass behind them. A user who passes `-fgcse-las` and reads the third section learns
something true in one command. A user who passes it and gets silence learns something false.

The exception is any flag that changes *semantics* rather than selecting a transformation.
`-fno-strict-aliasing`, `-fwrapv`, `-fno-delete-null-pointer-checks`, `-ftrapv`, `-ffast-math` and
its components, `-fno-semantic-interposition`, `-fexcess-precision`. These are not optional and
they are not pass selectors: they change what the IR is allowed to assume, they are recorded per
function and per instruction per spec 9.8, and accepting one without implementing it is a
miscompilation waiting for a bug report. Document 41 lists them and the test that each one
demonstrably changes output.

## 3.6 How a level is tested

Three tests, all cheap, all in CI from the first pass.

**The level table round-trips.** `rucc --print-pipeline -O2` parses back into the same list. This
catches a pass added to a level and not to the printer.

**Every pass is reachable and defeatable.** For each pass in the registry, some level runs it, and
`-fno-<name>` at that level removes it from the printed pipeline. `crates/rucc-opt/src/pass.rs:38`
already makes the registry the single source of truth for this; the test is the other half.

**Levels are monotone where they claim to be.** `-O2`'s pass list contains `-O1`'s, and `-O3`'s
contains `-O2`'s, as sets. `-Os` is exempt and so is `-Og`, and the exemption is written down in
the test rather than discovered by it. This catches the classic mistake of a pass that only runs
at `-O1` because somebody wrote the wrong constant.

## 3.7 Where this departs from spec 09

Spec 9.1 says `-O1` runs "the e-graph once" and lists SROA, GVN, DCE and LICM after it. The order
above puts SROA *before* the e-graph, and that is intentional: SROA turns aggregate loads and
stores into scalars, and every scalar it exposes is a value the e-graph can then reason about.
Running the e-graph first wastes the round. GCC agrees, in the sense that `pass_sra_early`
(`gcc/tree-sra.cc:5255`) is the 44th of the 386 instances, inside the early optimization group,
while the heavy value-level work is a hundred and fifty entries later.

Spec 9.1 gives `-O2` "two e-graph rounds around the loop pipeline" and does not say what runs
between them. 3.4 says. This is a refinement rather than a disagreement and spec 09 should absorb
it.
