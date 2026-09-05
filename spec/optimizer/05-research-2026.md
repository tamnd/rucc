# 05. The research layer, September 2026

`spec/01-research-2026.md` surveyed the landscape as of 31 August 2026 and it is the document this
one extends rather than replaces. What follows is the part of the literature that bears
specifically on the optimizer, with the numbers, and with an explicit statement of what each
result forces. A paper that does not change a decision is not in this document.

The rule from document 01.4 applies: a number, a venue, a year, or it does not appear.

## 5.1 The ægraph result, which is the most important number here

Chris Fallin published `The acyclic e-graph: Cranelift's mid-end optimizer` on 9 April 2026, a
write-up of the data behind the design that `spec/09-optimizer.md` bets on, presented at the
January 2026 Dagstuhl seminar on e-graphs. It is the first published quantitative evaluation of
ægraphs in production and it contains four numbers that matter to us more than anything else in
this document.

| Measurement | Value |
|---|---|
| Compile time, ægraph pipeline versus classical | +7 to 8% |
| Execution time of generated code | ~2% faster |
| Average e-class size after rewriting | 1.13 e-nodes |
| Execution win attributable to multi-value representation | ~0.1%, at 0.005% compile time |
| Extraction choosing badly, over ~4 million value nodes | twice, both in `spidermonkey.wasm` |

Read those together and they say something specific and slightly uncomfortable. The ægraph is
worth about 2% of runtime for about 7.5% of compile time, and *the e-graph part of it contributes
0.1% of that 2%*. An average e-class holding 1.13 e-nodes means that in the overwhelming majority
of cases exactly one form of a value is ever created, so there is nothing for extraction to choose
between and no equality being exploited. The extraction problem is NP-hard and Cranelift solves it
with a dynamic program that ignores shared substructure, and across four million nodes that
approximation cost them two decisions.

The honest conclusion is that Cranelift's win comes from the *structure* the ægraph imposes and
not from equality saturation at all. The structure is: hash-cons every node, canonicalize operands
before building, apply rewrite rules once at construction in the cascades style, and then place
values by global code motion. That combination gives GVN, LICM and rematerialization for free and
removes the pass-ordering problem between constant folding, peepholes, reassociation and GVN.
Every one of those benefits survives if you delete the union-find and keep one representative per
value.

**What this forces.** Spec 9.2 and spec 19's open question one framed the M4 experiment as
"ægraph versus a conventional instcombine-plus-GVN pipeline". That is now the wrong experiment,
because we know roughly what it will say. The right experiment, in document 12, is three-way, and
the middle arm is the one nobody has published on: a hash-consed, canonicalizing, rewrite-at-
construction IR with GCM placement and *no e-classes*. If the middle arm gets 1.9% of the 2% at
lower compile time and considerably less code, it wins, and rucc ships a simpler middle end than
the parent spec assumed. Document 12.6 says how to tell.

This also reframes the risk. Spec 00 calls the ægraph "the riskiest technical assumption" on the
grounds that the CFG skeleton's prohibition on control flow rewrites might cost too much. Fallin
confirms the limitation in the same post, listing redundant block parameter removal and
path-sensitive optimization as things the framework cannot express. But the cost of that
limitation is bounded by the fact that we keep conventional CFG passes anyway. The larger risk was
always that the e-graph machinery would not pay for itself, and that risk now has a measured size.

## 5.2 E-graphs elsewhere, and why they are not the counter-argument

`E-Graphs as a Persistent Compiler Abstraction` (arXiv 2602.16707, Merckx et al., 2026) reports
1.18x on a software case study and up to 11% circuit delay reduction on hardware, from keeping the
e-graph alive across abstraction levels and interleaving it with other analyses rather than
saturating once and discarding. This is a real result and it is not a rebuttal of 5.1, because its
win comes from *persistence across levels*, and rucc has one level in the middle end. The
technique becomes interesting if rucc ever wants the e-graph to span the IR-to-MIR boundary, which
is a post-1.0 question and is recorded as such in document 43.

`e-boost` (arXiv 2508.13020) attacks extraction with adaptive heuristics plus exact solving. Given
5.1's finding that extraction chose badly twice in four million nodes, this is solving a problem
rucc will not have. Noted and declined.

`LLM-Guided Strategy Synthesis for Scalable Equality Saturation` (arXiv 2604.17364) and the
guided-saturation line generally are aimed at the case where saturation is expensive and needs
steering. rucc's e-graph, if it has one, applies rules once at construction and never saturates.
Not applicable.

EGRAPHS 2026 ran at PLDI in June. Its accepted work does not enter the ACM DL, so nothing from it
is citable under document 01.4's rule; it is a place to watch, not a place to cite.

## 5.3 Verification, which is where the field actually moved

Three results, and together they set rucc's correctness strategy for the whole optimizer.

**Alive2** (Lopes, Lee, Hur, Liu, Regehr, PLDI 2021) remains the reference point: bounded
translation validation, fully automatic, no false alarms, no compiler changes. Its reported
harvest is 47 new bugs from LLVM's own unit test suite, 28 fixed, and eight patches to the LLVM
Language Reference, which is the more interesting number: running a verifier against a compiler
found eight places where the *specification* was wrong. The companion CAV 2021 paper covers all of
LLVM's intraprocedural memory optimizations. The cost is real and should be planned for: about
2.5 hours to run LLVM's unit suite on an eight-core machine.

**Crocus** (VanHattum et al., ASPLOS 2024) is already spec 00's justification for SMT-verified
instruction selection rules, and its authors report the technique carries to middle-end rewrites.
Document 13 is built on this.

**Minotaur** (Liu, Mada, Regehr, OOPSLA 2024) synthesises peepholes, verifies each with Alive2,
and reports 7.3% average on GMP with a 13% maximum and 1.5% average on SPEC CPU 2017 with 4.5% on
`638.imagick`. Several of its discoveries were upstreamed into LLVM. Spec 9.11 already says the
right use is offline rule generation feeding the rule DSL rather than a solver in the pipeline,
and nothing since changes that; what changes is confidence, because 7.3% on real numeric code is
larger than most individual passes are worth.

The 2026 CGO cluster on verification-guided optimization (`LLM-VeriOpt`,
`Compiler-Runtime Co-operative Chain of Verification for LLM-Based Code Optimization`) is worth
knowing about mainly for what it concedes: the papers exist because generating a plausible
optimization is now easy and establishing that it is correct is the whole problem. That is the
same conclusion spec 00 reached from a different direction, and it is the reason document 13
refuses to merge a rule without an SMT obligation.

**What this forces.** Document 41 adopts bounded translation validation as a *third* correctness
layer alongside the IR verifier and differential execution, scoped to single functions at
`-O1`, run in CI rather than in the compiler, and budgeted at hours rather than minutes. It is not
an M4 deliverable. The M4 deliverable is that the IR's semantics are written down precisely enough
that translation validation is possible later, which is a constraint on `spec/08-ir.md` and not on
the optimizer.

## 5.4 Register allocation

`Faster Chaitin-like Register Allocation via Grammatical Decompositions of Control-Flow Graphs`
(Cai, Goharshady, Hitarth, Lam, ASPLOS 2025, pp. 463 to 477) speeds up classical graph colouring
by exploiting structural properties of real control-flow graphs. The underlying observation, that
CFGs from structured source have bounded treewidth, is the same one that makes the Cooper-Harvey-
Kennedy dominator algorithm fast in practice and is worth internalising generally.

The SSA chordality result (Hack, Grund, Goos, CC 2006, plus two independent contemporaneous
discoveries) is old and remains the most useful single fact about register allocation: the
interference graph of an SSA program is chordal, chordal graphs colour optimally in
O(omega(G) times |V|) along a perfect elimination order, and the dominance relation *is* such an
order, so the interference graph need never be built. Spec 10 commits to a backtracking allocator;
document 39 argues that the SSA-based decoupling of spilling from colouring from coalescing is the
part to take, independent of which colouring strategy is chosen.

`TPDE: A Fast Adaptable Compiler Back-End Framework` (arXiv 2505.22610, 2025) is the useful
counterweight on the throughput axis: it compiles 4.27x faster than Cranelift, and 2.68x faster
than Cranelift using its single-pass allocator, but its output runs 1.64x slower than Cranelift
with backtracking allocation. That last ratio is the price of a fast allocator and it is large.
It is direct evidence for spec 10's decision to have two allocators rather than one, and for
document 03.4's `-O0` pipeline being about the allocator more than about the passes.

## 5.5 Vectorization

The state of the art here is older than the rest of this document and that is itself informative.

`All you need is Superword-Level Parallelism: Systematic Control-Flow Vectorization with SLP`
(Chen, Mendis, Amarasinghe, PLDI 2022) generalises SLP across basic blocks and loop nests and
reports 1.36x geometric mean on Polybench and 1.47x on serial graphics benchmarks, with 3.28x on a
volume renderer. Its framing is the one to adopt: SLP's original promise was to replace loop
vectorization entirely and it failed because SLP could not reason about control flow.

`Look-ahead SLP` (Porpodas, Rocha, Góes, CGO 2018) is the fix for commutative operand ordering and
is cheap enough to be worth having from the start. `SuperGraph-SLP` makes the point that matters
most for a cost model: treating SLP graphs in isolation *overestimates* cost when consecutive
graphs share data, so a naive per-graph cost model systematically declines profitable
vectorization.

The standing critique of vectorizer cost models, from NeuroVectorizer (CGO 2020) among others, is
that they are lookup tables of per-instruction latency and throughput, and that cost is an
abstract number which does not translate into performance. This is true and it is not a reason to
build something cleverer; it is a reason to keep the cost model small, keep it in one place, and
measure it, which is document 40's entire argument.

GCC 16's own vectorizer changes are the most recent practical work: vectorization of uncounted
loops, more efficient early-break handling by eliminating redundant vector induction computation,
peeling for alignment on vector-length-agnostic loops using masking, and mutual peeling for
alignment. None of these is in scope for M4. All of them are in scope for whoever writes document
32's implementation, and they are the reason that document says loop vectorization is a milestone
of its own rather than a pass.

## 5.6 Machine learning, and the decision not to

MLGO (Trofin et al.) is in production in LLVM for inlining-for-size and register allocation, with
up to 7% size reduction over `-Oz` on the inlining model. Spec 9.11 declines to use ML in the
heuristics on the grounds that the numbers are smaller than what implementing the ordinary passes
properly gets, and that position is unchanged and correct for rucc's stage.

Two operational details from MLGO are worth stealing without the ML. First, they avoid training
online because it would affect determinism, which is the same constraint spec 03 imposes for
different reasons and is a useful independent confirmation that determinism and adaptivity are in
tension. Second, the argument for ML is explicitly that hand-written heuristics limit the *number
of features* a decision can consider. That is a criticism of heuristic design, not an argument for
neural networks, and the response available to us is document 40's: put every heuristic's inputs
and constants in one place where they can be seen, tuned and swapped, so that adding a feature is
editing a table rather than rewriting a pass.

The LLM-for-optimization line (`LLM-Vectorizer`, CGO 2025, reporting 1.1x to 9.4x; `AutoPass`;
`AutoVecCoder`; `Leveraging Large Language Models for Generalizing Peephole Optimizations`,
arXiv 2603.18477) is not a compiler technique, it is a technique for *producing* compiler
techniques offline. In that framing it composes with document 13 exactly the way Minotaur does:
generate candidate rewrite rules, discharge the SMT obligation, ship the verified ones, and never
let a model near the compilation. `LLM-Vectorizer`'s headline range should be read with its
methodology note in mind, that baseline compilers were given `restrict` on every pointer and the
LLM output was not.

`AI Coding Agents Need Better Compiler Remarks` (arXiv 2604.13927) is the one to take seriously
for a different reason: it argues compiler diagnostics are becoming a machine-facing interface.
rucc already commits to `-fdump-alias` saying which rule concluded no-alias, and spec 9.10's dump
apparatus is more thorough than GCC's. Making optimization remarks structured and machine-readable
is nearly free given that, and it is worth doing for humans regardless.

## 5.7 The eleven decisions that rest on a number

Per document 01.4, every place a result in this document decides something, so that a revised
number can be traced to what it changes.

| # | Decision | Rests on |
|---|---|---|
| 1 | The M4 e-graph experiment becomes three-way, with a no-e-class middle arm | 5.1, e-class size 1.13 |
| 2 | The e-graph is not expected to pay for more than ~2% | 5.1 |
| 3 | Extraction uses a simple cost model with no exact solving | 5.1, two bad choices in 4M nodes |
| 4 | E-graph persistence across IR levels is post-1.0 | 5.2, 1.18x needs multiple levels |
| 5 | No rule ships without an SMT obligation | 5.3, Crocus and the CGO 2026 cluster |
| 6 | Translation validation is a CI layer, not a compiler feature, and not in M4 | 5.3, 2.5 hours per suite run |
| 7 | Offline rule synthesis is a post-1.0 project with a real expected win | 5.3, Minotaur 7.3% on GMP |
| 8 | Two register allocators rather than one | 5.4, TPDE 1.64x runtime penalty |
| 9 | Spilling, colouring and coalescing are decoupled | 5.4, SSA chordality |
| 10 | SLP cost is computed over connected graphs, not per graph | 5.5, SuperGraph-SLP overestimation |
| 11 | No ML in any heuristic; heuristic constants centralised instead | 5.6, MLGO 0.3 to 7% versus ordinary passes |

## 5.8 What nobody has published that we would like

Three gaps, recorded because noticing them is cheap and because if any of them is filled between
now and M4 it changes something.

Nobody has published a controlled comparison of an e-graph mid-end against a hash-consing
mid-end with the same rule set and the same placement algorithm. That is exactly document 12's
experiment and its result would be publishable, which is a reason to run it carefully and write
it up whichever way it goes, as spec 17's M4 exit criterion already requires.

Nobody has published good numbers on what a *modern* alias analysis is worth in a C compiler in
isolation. The Steensgaard-versus-Andersen tradeoff in spec 9.4 is argued from asymptotics and
from folklore, and document 08 flags this as the measurement most worth taking early because it
determines a large amount of downstream work.

Nobody has published on translation validation of the *middle end* of a non-LLVM compiler where
the IR semantics were designed for it. Alive2 works despite LLVM's IR rather than because of it.
rucc is in the unusual position of being able to design the IR knowing this, and document 41 says
what that costs.
