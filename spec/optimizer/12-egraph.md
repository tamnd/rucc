# 12. The acyclic e-graph, and the experiment that decides it

Spec 00 calls this "the riskiest technical assumption in the project" and spec 19's open question
one asks whether it carries over from a JIT to an ahead-of-time C compiler. Since those were
written, the only production ægraph implementation published its numbers, and they change the
question. This document restates the design, states what the numbers say, and specifies an
experiment that can actually be run and lost.

## 12.1 What the design is

The ægraph, following Cranelift, is four decisions taken together.

**Hash-consing.** Building a node that structurally equals an existing node returns the existing
one. Two occurrences of `a + b` are one value, everywhere, always. This is global value numbering
happening continuously rather than as a pass.

**Rewriting at construction.** When a node is built, the rewrite rules that match it fire
immediately, and their results are added to the same equivalence class. Rules are applied *once*,
at creation, not to a fixpoint. Because operands were themselves built and rewritten first, a rule
matching on an operand's shape sees the rewritten shape. This is the "cascades" discipline and it
is what makes a single pass sufficient where a conventional peephole pass needs several.

**No control flow in the graph.** The CFG is a fixed skeleton. Only values live in the e-graph.
Side-effecting operations are pinned to the block they were in.

**Extraction and placement.** After rewriting, each needed value's best representative is chosen by
a cost model, and the chosen nodes are placed into blocks by Cliff Click's Global Code Motion
(PLDI 1995): as late as possible while dominating all uses, and as shallow in the loop nest as
possible. LICM, rematerialization and partial dead code motion fall out of this rather than being
passes.

The attraction is that four hard problems (pass ordering between folding and CSE, phase-order loss
in peepholes, LICM as a separate pass, redundancy elimination) collapse into one mechanism. That
attraction is real and it is not what the numbers question.

## 12.2 What the numbers say

Fallin's April 2026 write-up of Cranelift's production ægraph, summarised in document 05.1:

| Measurement | Value |
|---|---|
| Compile time versus a classical pipeline | +7 to 8% |
| Execution time of generated code | ~2% faster |
| Average e-class size after rewriting | **1.13 e-nodes** |
| Win attributable to multi-value representation | ~0.1%, at 0.005% compile time |
| Suboptimal extractions across ~4M value nodes | 2 |

The third row is the one that matters. An average e-class holding 1.13 e-nodes means that for
roughly seven values out of eight, exactly one form was ever created. There was no equality to
exploit, nothing for extraction to choose between, and the union-find had one member.

The fourth row confirms it directly: keeping multiple representations, which is the entire reason
an e-graph is not just a hash table, is worth 0.1% of execution time out of a 2% total win.

So Cranelift's 2% comes from the *other three* decisions. Hash-consing gives GVN. Rewriting at
construction gives order-insensitive peepholes. GCM gives LICM and remat. None of those requires
e-classes.

## 12.3 The three-way experiment

Spec 17's M4 exit criterion asks for the ægraph experiment to be run and written up. Document 05.1
argues the two-arm framing is now the wrong one. The three arms:

**Arm A, classical.** A conventional pipeline: fold, then instcombine-style peepholes to a bounded
fixpoint, then GVN, then LICM as a loop pass. This is what every C compiler does and it is the
baseline the other two must beat.

**Arm B, hash-consed with GCM, no e-classes.** Hash-consing, canonicalizing operand order,
rewriting once at construction with the same rule set as arm C, one representative per value,
and GCM for placement. Values are *replaced* on rewrite rather than unioned, so a rewrite is
committed at the moment it fires and there is no extraction step at all.

**Arm C, the full ægraph.** Arm B plus e-classes, plus a cost model, plus extraction.

The delta B minus A measures what the structure is worth. The delta C minus B measures what
equality is worth, which is the number Cranelift reports as approximately 0.1%.

**Why arm B is worth building even though nobody has published on it.** Because it is strictly
smaller than arm C, it has no NP-hard subproblem, its output is deterministic without a tiebreak
rule, and it removes the single largest source of complexity in arm C, which is that a rewrite may
be committed and then not selected. If arm B captures most of C's win, rucc ships a middle end
that is materially simpler than the parent spec assumed and no worse.

**What the arms share, so the comparison is fair.** The same rule set from document 13. The same
cost model from document 40. The same alias analysis, same ranges, same everything else in the
pipeline. Arms B and C share the GCM implementation. This is not a small amount of shared
infrastructure and it is why the experiment is affordable at all: the incremental cost of running
all three is the union-find, the extraction, and arm A's peephole driver.

**What is measured.** Compile time on the corpus in document 42, execution time on the same, and
one number the published work does not report: **lines of code in the middle end under each arm**,
because "within 10% of `gcc -O2`" from spec 02 is one axis and maintainability is the reason this
project exists.

**How it is decided.** If C beats B by less than 0.5% execution time, arm B ships. If B beats A by
less than 1%, arm A ships and the ægraph bet is lost, cleanly and in public. Spec 17's requirement
to write it up either way stands, and document 05.8 notes that this comparison has not been
published, so the write-up is worth doing well.

## 12.4 What is wrong with the design, independent of the numbers

Two things, both structural.

**Extraction is NP-hard**, because a node shared between two chosen expressions should be costed
once and a dynamic program over the DAG costs it twice. Cranelift's approximation ignores sharing
and, across four million nodes, chose badly twice. That is a strong argument for the simplest
possible extraction and against any investment in exact solving, and document 05.6's decision 3
records it. If rucc runs arm C, its extractor is the naive bottom-up dynamic program and nothing
more.

**The CFG skeleton cannot be rewritten.** Fallin lists this limitation explicitly: no redundant
block parameter removal, no path-sensitive optimization. So jump threading (23), if-conversion
(22), block merging (21), unswitching (30) and tail merging all remain conventional passes running
outside the e-graph.

That is not fatal and rucc's pipeline in document 03.4 already assumes it. But it does mean the
e-graph is not "the middle end"; it is one phase among fifteen, and the pass-ordering problem it
solves is only solved *within* it. The e-graph runs, then jump threading changes the CFG and
creates new opportunities, then the e-graph runs again. That is exactly the two-round structure in
spec 9.1, and it is an admission that the phase-ordering problem was reduced rather than removed.

There is one genuine benefit of the skeleton being pinned that is worth naming because it is easy
to miss: per document 04.3, the entire rewrite phase can honestly declare `Preserved::All` for the
dominator tree, the loop forest and the CFG analyses, because it provably cannot change them. In a
pipeline where almost every pass invalidates almost everything (document 04.4), a large phase that
invalidates nothing is worth real compile time.

## 12.5 Global code motion, which is worth having regardless

GCM is separable from the e-graph and is the piece most likely to survive whichever arm wins.

The algorithm, from Click 1995, is two passes. **Early schedule**: place each value in the
shallowest block that is dominated by all its operands' blocks, which is the earliest legal
position. **Late schedule**: place it in the block that is the least common ancestor, in the
dominator tree, of all its uses' blocks, then walk from there up towards the early position
choosing the block with the smallest loop depth. The result is as late as possible, as shallow as
possible.

Three consequences. A value used only inside a loop but computed outside stays outside, which is
LICM. A value used on one arm of a branch sinks into that arm, which is partial dead code
elimination. A value used twice far apart may be recomputed at each use if the cost model prefers
it to keeping it live, which is rematerialization.

Three traps.

*Side effects are pinned.* Loads, stores, calls, and anything trapping do not move under GCM. A
load can be moved only where the alias analysis and memory SSA say it is safe, which makes load
motion a separate decision and not a GCM one.

*Sinking increases live ranges as often as it shortens them.* Placing a value at its latest
position lengthens the live ranges of its operands. Click's algorithm is a heuristic, not an
optimum, and document 39's register allocator will occasionally want the opposite. The pressure
heuristic belongs in document 40 and it must exist, or GCM on a function with many long-lived
values makes the allocator's job worse.

*Loop depth is the wrong tiebreak without frequency.* "Shallowest loop depth" assumes an inner loop
runs more than an outer one, which the profile in document 11 may contradict. When frequency data
exists, use it; loop depth is the fallback.

## 12.6 The M4 sequence

The order matters because arm B is a prefix of arm C, and arm A shares the rule set.

1. The rule DSL and the rule set (document 13), independent of all three arms.
2. Hash-consing in `rucc-ir`, plus operand canonicalization. Small, and useful to arm A too.
3. GCM, standalone, testable against hand-written expectations.
4. Arm B: rewrite at construction, one representative, GCM placement.
5. Arm A: a conventional driver over the same rules, plus a separate GVN and LICM.
6. Measure B against A. If B does not win, stop, and report.
7. Arm C: union-find, cost model, extraction.
8. Measure C against B.

Step 6 is a real stopping point and the sequence is arranged to reach it before the most expensive
work. That is the whole reason to order it this way.

## 12.7 How this is wrong

**A rewrite rule is applied to a node whose operands have not been canonicalized**, so a rule that
should have matched does not, and the result depends on the order the builder happened to see the
operands. Commutative operands are ordered by a total order on values before hashing, always, in
the constructor, and there is a test that `a + b` and `b + a` are the same value.

**Hash-consing merges two values that are not interchangeable.** Two `add`s with the same operands
but different overflow flags are not the same value: one is poison on overflow and one wraps. The
hash key includes the flags, and this is the classic bug. The same applies to any attribute
carried on an instruction: if it affects semantics it is in the key, and the rule for adding a new
attribute is that it goes in the key unless there is a written argument why not.

**Extraction picks a node whose operands were not extracted**, producing a value referencing a
non-selected representative. Extraction is bottom-up and memoized and the verifier checks that
every operand of every emitted instruction is itself emitted.

**GCM moves a load above a store that aliases it.** GCM's legality check must consult memory SSA,
not just dominance. The cheap and correct M4 rule is that loads do not move under GCM at all;
load motion is LICM's job in document 27, where the analysis is already in hand.

**The e-graph grows without bound.** Rules that produce nodes which match other rules can cascade.
Rewriting once at construction bounds this in principle, but a rule set with a cycle (`a*2` to
`a+a` to `a*2`) does not terminate even under cascades. Document 13 requires the rule set to be
checked for cycles at build time, and there is a node-count budget per function that, when
exhausted, stops rewriting and proceeds with what exists.

## 12.8 What it costs

The published number is +7 to 8% compile time for arm C against arm A. That is the budget to
beat and it is a large one: spec 02's throughput axis asks for 1.5x `clang -O2`, and 8% of the
optimizer is not 8% of the compiler, so this is affordable if the 2% is real.

Arm B should cost less than arm C by the extraction pass and the union-find, which Cranelift
measures at 0.005% compile time for the multi-value part alone. That figure suggests B and C will
be close in compile time and the decision between them will be made on code quality and on lines
of code, which is an unusual and rather satisfying position to be in.
