# 04. The pass manager and the analysis manager

Spec 9.10 says the pass manager is deliberately boring, and it is right, and boring is not the
same as small. The manager decides three things that every subsequent document depends on: what a
pass is allowed to be, in what order functions and passes are traversed, and how an analysis
computed by one pass reaches the next one without being recomputed or being stale. rucc has
answered the first, answered the second in a way that should be revisited, and not yet answered
the third at all.

## 4.1 What exists

`crates/rucc-opt/src/pass.rs:21` defines the whole of it:

```rust
pub trait Pass: Sync {
    fn name(&self) -> &'static str;
    fn describe(&self) -> &'static str;
    fn run(&self, func: &mut Func, fuel: &mut Fuel) -> bool;
}
```

Three good decisions are already baked in here and should not be relitigated. A pass is a value
rather than a function, so its name travels with it and `PASSES` at `pass.rs:38` is a registry
rather than a convenience, which is what makes "a pass that is written and not in here is a pass
nobody can turn on" enforceable by a test. A pass returns whether it changed anything, which is
what lets the verifier run only when there is something new to verify. And fuel is threaded
through the signature rather than read from a global, so a pass physically cannot transform
without being offered the chance to be stopped.

`run` at `pipeline.rs:207` iterates passes outermost and functions innermost: every pass sees the
whole module before the next pass starts. Fuel is per pass per compilation and therefore shared
across functions, which is exactly right for bisection, since the site being searched for is one
site in one file and numbering it per function needs the function identified first.

## 4.2 The traversal order, which should change

GCC does the opposite. `gcc/cgraphunit.cc:1874` runs `execute_pass_list (cfun, all_passes)` for
one function, inside a loop over the call graph in `expand_all_functions` at
`gcc/cgraphunit.cc:1990`. One function goes through all 276 entries of `all_passes` and is
expanded to RTL and emitted before the next function is looked at.

This is not an accident and there are three reasons for it, all of which apply to rucc.

**Memory.** Pass-outermost means every function's IR, and every analysis anybody computed for it,
is live simultaneously. Function-outermost means one function's working set is live at a time.
On the SQLite amalgamation this is the difference between holding a quarter-million lines of IR
and holding one function's worth, and on the kernel at `allmodconfig` it is the difference
between working and not. Spec 9.8 already concedes this point for LTO, where monolithic mode is
described as needing more memory than the machine has; the same argument applies one level down.

**Parallelism.** Spec 00 promises `rayon` gives function-level parallelism for free. It gives it
for free only under function-outermost traversal, where each function is an independent unit of
work. Under pass-outermost the parallel unit is a pass over all functions, which needs a barrier
between every pass and gets far worse scaling.

**Cache.** The IR for one function, walked forty times, stays in L2. The IR for the whole module,
walked forty times, does not. This is a compile-throughput argument and compile throughput is one
of the four axes.

The cost of switching is the dumps. Pass-outermost gives a dump that is the state of the whole
program between two passes, which is exactly what a human reading a dump wants, and
`pipeline.rs:213` says so in a comment. Function-outermost gives forty dumps per function
interleaved. The fix is that the dump writer buffers by pass name and concatenates at the end,
which restores the readable form at the cost of holding the dumps, and dumps are only held when
somebody asked for them.

**Recommendation.** Switch to function-outermost in M4, before there are enough passes and enough
analyses for the memory behaviour to be load-bearing, and buffer the dumps. This is departure two
in document 43.

## 4.3 The analysis manager, which does not exist

This is the largest gap in `rucc-opt` and `crates/rucc-opt/src/lib.rs:12` is honest about it:
"There are no analyses to declare, so that machinery lands with the dominator tree rather than
being guessed at now." That was the right call in the commit that added the pass manager. It
stops being the right call the moment the second pass wants a dominator tree, which is the third
pass anybody writes.

**What it has to do.** Four things, and no more than four.

*Compute on demand and cache.* A pass asks for the dominator tree; if a valid one exists for this
function it is handed over, otherwise it is computed and cached. Nothing is computed because a
level said so; things are computed because somebody asked.

*Invalidate by declaration.* Each pass declares what it preserves. Everything not preserved is
dropped after it runs. GCC does this with a bitmask of `TODO_` flags at `gcc/tree-pass.h:240`,
which is a workable design that has one flaw: the flags say what to *do* (`TODO_cleanup_cfg`,
`TODO_update_ssa`, `TODO_rebuild_alias`) rather than what is *invalid*, so the pass author has to
know which analyses a CFG change hurts. Declaring invalidity and letting the manager work out the
consequences is the better direction and it is what LLVM converged on after its own detour.

*Catch a pass that lies.* Spec 9.10 asks for this explicitly and it is the single highest-value
piece of the whole manager. In a debug build, after a pass that claimed to preserve analysis A,
recompute A and compare. A pass that quietly invalidates the dominator tree and says otherwise
produces a miscompilation three passes later that is essentially unfindable by reading code, and
this check turns it into an assertion failure at the exact pass. It costs nothing in release
builds and it will pay for itself the first week it exists.

*Be transitive.* The loop forest depends on dominators. Memory SSA depends on the alias analysis
and the dominator tree. Invalidating dominators must invalidate the loop forest. The dependency
edges are declared by the analysis, not by the pass, so a pass author who invalidates dominators
does not need to know what else that breaks.

**The shape.** An `Analysis` trait with an associated `Result`, a `Manager` holding
`HashMap<(FuncId, TypeId), Box<dyn Any>>`, and a `Pass` trait grown two methods:

```rust
fn preserves(&self) -> Preserved;   // All, None, or a named set
fn run(&self, func: &mut Func, an: &mut Manager, fuel: &mut Fuel) -> bool;
```

`Preserved::All` is the honest answer for an analysis-only pass and for anything that only
rewrites values without touching the CFG, which is most of the value-level pipeline and, notably,
the entire e-graph rewrite phase: the CFG skeleton is pinned, so dominators and the loop forest
survive it. That is a real and underappreciated benefit of spec 9.2's pinning constraint and
document 12 makes more of it.

## 4.4 The seven analyses, and who invalidates them

The complete set for M4. Every one has a document.

| Analysis | Document | Depends on | Invalidated by |
|---|---|---|---|
| CFG and edges | 06 | nothing | any pass that changes control flow |
| Dominator tree and frontiers | 06 | CFG | any CFG change |
| Post-dominators | 06 | CFG | any CFG change |
| Loop forest | 07 | dominators | any CFG change |
| Scalar evolution | 07 | loop forest | loop changes, IV rewrites |
| Alias analysis | 08 | module-level, per function query | new memory operations |
| Memory SSA | 09 | alias, dominators | any memory operation added or removed |
| Value ranges | 10 | CFG, dominators | any branch condition or arithmetic change |
| Block frequency | 11 | CFG, profile | any CFG change |

Nine rows, not seven; the count in spec 9.4 was written before value ranges and block frequency
were separated out from the passes that consume them, and separating them is right because three
passes each want ranges and computing them three times is how compilers get slow.

The important column is the last one. Notice that almost everything is invalidated by "any CFG
change", which is the single fact that determines pipeline shape: cluster the CFG-changing passes
together and run the value-level work between clusters, rather than alternating. Document 03.4's
`-O2` list is arranged that way on purpose.

## 4.5 Fuel

`crates/rucc-opt/src/fuel.rs` is complete and correct and there is nothing to add to the
mechanism. What is missing is the enforcement that spec 9.10 promises: "a test that runs each pass
at fuel 0 and confirms the output equals the input". That test is four lines, it belongs in
`pipeline.rs`'s test module, and without it the whole fuel apparatus is a convention rather than a
guarantee. A pass that transforms before asking passes every other test in the suite and silently
breaks bisection, which is the one thing you need working on the day you have a miscompilation.

The second missing piece is the bisection script itself. `-fpass-fuel=<pass>=<n>` plus a script
that binary-searches *n* over a failing test is the whole feature, and the script is perhaps thirty
lines of shell. It should exist in `xtask` before the first optimizing pass lands, because the
first thing an optimizing pass does is miscompile something.

The third is fuel over the pass *list*: `-fpass-fuel-global=<n>` stopping the entire pipeline after
*n* transformations across all passes, which localizes to a pass first and then to a site within
it. Two binary searches of twenty compilations each beats one search over a space you have to
guess the shape of. GCC's `-fdbg-cnt=` is this and it is the tool GCC developers actually reach
for.

## 4.6 Dumps

`Dumps` at `pipeline.rs:69` handles `all`, `before-<pass>` and `after-<pass>`, validates the pass
name against the registry so a typo is an error rather than silence, and numbers the output so a
directory listing sorts in execution order. That is the right feature set and it is done.

`-fdump-ir-diff` from spec 9.10 is not done and it is the one people will use. A dump of a
40,000-line module before and after a pass that changed two instructions is not a debugging aid.
The diff form is: print both, diff them, and print the changed instructions with three lines of
context and the enclosing block header. The subtlety is that value numbering shifts every
subsequent `%n`, so a naive textual diff of IR is all noise. The fix is to print with stable names
derived from a hash of the defining instruction rather than from allocation order, under the dump
flag only. That is a `rucc-ir` printer option and it should be built with the diff.

## 4.7 What the manager must never do

No adaptive ordering. No running a pass twice because it reported a change. No pass scheduling
heuristics, no cost-driven pass selection, no "run until fixpoint". Spec 03's determinism rule
forbids the first three and the fourth is forbidden for a subtler reason: a fixpoint loop makes
compile time a function of program shape in a way nobody can predict, and it makes the
`--print-pipeline` output a lie. If a pass needs to run twice, it is written twice in the level
table, where a human can see it and a benchmark can price it. That is exactly what GCC does, and
running `pass_ccp` seven times in an explicit list is more honest than running it until it stops
changing things.
