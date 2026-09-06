# Testing and validation

Axis 1 in document 02 is "zero known miscompilations under active search," and the operative words are *active search*. A compiler with no known bugs because nobody looked is not the same artifact as one with no known bugs after a year of continuous fuzzing, and this document is the difference between them.

## 15.1 The layers

Seven, from cheapest to strongest. Each catches a class the one below it cannot, and the cost per bug found rises steeply as you go down the list, which is the argument for having all of them rather than only the impressive ones.

**Unit tests** inside each crate, testing the things with a clean contract: constraint solving, integer formatting, the correctly-rounded literal conversion, the parallel-move sequencer, the classification algorithm. Fast, run on every build.

**Golden-file tests** over the textual forms from document 03. A `.c` file, an expected `--emit=tast`, `--emit=ir` and `--emit=mir-final`. Cheap to write, brutal at pinning behavior, and their diffs are readable, which matters because a test whose failure is unreadable does not get fixed, it gets deleted. Regenerable with `cargo xtask bless`, which is only acceptable because reviewing a blessed diff is easy. Every case is compiled for one target and under one dialect, so that the expectation is a fact about the compiler and not about the machine CI happened to run on. A case about a rule that changed between dialects names its own with a `// std:` line, because `int f();` means a function taking anything before C23 and a function taking nothing from C23 on, and a case about the first of those cannot be written in the default dialect at all.

**Round-trip and injection properties**, also from document 03: print, re-parse, print again, compare; and take a program's IR, print it, re-parse it, compile from there, and check the output matches compiling directly. The second catches printer/parser asymmetries that the first does not.

**The IR verifier** after every pass, per document 08.7, plus the register allocation checker per document 10.4. These are not tests, they are assertions that run inside every debug and CI compilation of every test, which makes them effectively free.

**Execution test suites.** The [c-testsuite](https://github.com/c-testsuite/c-testsuite), GCC's `gcc.c-torture` and `gcc.dg`, and Clang's relevant tests. These encode decades of accumulated knowledge about what breaks compilers, and running them is the single highest-value day of work available at M3. Document 20 is how one of them is actually run: what counts as an oracle, which path from C to an executable is being tested, what bounds a run, and what it means for the rule set to be fully exercised.

**Differential testing against real code**: the corpus, section 15.3.

**Randomized differential testing**: section 15.4.

**Translation validation**: section 15.5.

## 15.2 Testing each stage in isolation

The pipeline in document 03 is separable, and each seam is a place to inject a test.

The **preprocessor** is tested by diffing `-E` output against GCC over the corpus, per document 05. Not equality (comment and whitespace conventions differ legitimately) but token-stream equality after normalization, which is the property that actually matters.

The **parser** is tested by golden ASTs, by a round-trip through a pretty-printer, and by an error-recovery suite that asserts a specific diagnostic count and set for malformed input, so that recovery quality is a tested property rather than an accident.

The **type checker** is tested by golden typed ASTs and by a suite of accept/reject programs at each `-std=` level, including the C23 changes that make previously valid programs invalid. The cases live in `tests/accept` and each one names the dialects it has to compile under and the dialects it must not, so a dialect nobody mentioned is a dialect nobody thought about. A rejected case also carries the sentence its rejection has to contain, and that sentence is measured against the reference compiler rather than written from what rucc happens to say, which is what keeps the wording something a build system can grep for.

The **optimizer** is tested by IR-in/IR-out golden files per pass, and, more importantly, by generating IR directly with a fuzzer, which reaches optimizer states the frontend cannot produce and does so without spending time in the frontend.

The **backend** is tested by golden MIR, by the encoder differential in document 11.1, and by execution, which document 20 specifies.

## 15.3 The corpus

Every project on document 14's ladder, built continuously, with its own test suite run, at every optimization level. This is the most valuable test infrastructure in the project and it is also the most expensive to operate, so it is worth being precise about what it is for.

It is not for finding *classes* of bug. A fuzzer does that better. It is for finding the bugs that only occur in code nobody would write on purpose: the 4000-line function, the 90-deep macro expansion, the header that includes itself, the struct with 200 bitfields, the switch with 8000 cases. Real code contains all of these and no generator produces them.

Corpus results are stored, so "this test started failing" is answerable by bisection over both our history and the project's.

There is a second corpus and the two are not the same thing. [tamnd/rucc-corpus](https://github.com/tamnd/rucc-corpus) is written rather than found: every program in it exists for one named transformation, and the answer it should print was computed in Rust by the generator rather than taken from another compiler, so it can say whether an optimization was correct without asking GCC anything. `cargo xtask corpus` runs it against the compiler this tree builds, with GCC 16 alongside as the reference for sizes and times. The commit of it that counts is pinned in `xtask/corpus.toml`, per [`optimizer/42-measurement.md`](optimizer/42-measurement.md) section 42.3, because a corpus that drifts makes historical numbers meaningless. Moving the pin is a commit of its own, which is where the discontinuity in the numbers is written down.

## 15.4 Random program generation

**[Csmith](https://github.com/csmith-project/csmith)** generates C programs free of undefined behavior, which is what makes differential comparison valid: any difference between `rucc -O2` and `gcc -O2` on a Csmith program is a bug in one of them. It found hundreds of bugs in GCC and LLVM and it will find ours. Its weakness is a narrow feature distribution. It produces a recognizable style of program, and after some time it stops finding new things.

**[YARPGen](https://github.com/intel/yarpgen)** takes the complementary approach of generating programs specifically shaped to trigger optimizations, which finds a different and largely disjoint set of bugs, particularly in loop transformations and vectorization.

**Our own generator** covers what neither does and what our design specifically requires: the GNU extension surface from document 13, `_BitInt` and the C23 features, bit-fields with randomized layouts (feeding document 12.6), `_Atomic` and the memory-order space, VLAs, and inline assembly with randomized valid constraint sets. This is the generator that tests the parts of our compiler nobody else's tests reach.

**Differential comparison** runs each generated program under `rucc` at five optimization levels, under `gcc -O0` and `-O2`, and under `clang` where available, and compares a checksum of the final program state. Any disagreement is triaged: first determine which compiler is wrong by inspecting the reduced case, then file it in the right place. Being the compiler that files GCC bugs is a good sign, not a bad one.

**Self-differential testing** catches what cross-compiler comparison cannot, namely a bug in an area where GCC has the same behavior we do for the wrong reason. Compare our own optimization levels against each other; compare `-flto` against non-LTO; compare a native run against QEMU for the same target. And compare against an **IR interpreter**, a straightforward reference evaluator over document 08's IR, slow and obviously correct, which gives an oracle for the optimizer independent of any other compiler.

**Reduction with [cvise](https://github.com/marxin/cvise)** is what makes all of this usable. A 4000-line Csmith failure is not a bug report; the same failure reduced to nine lines is. The reduction is automated, runs immediately on any failure, and its interestingness test is generated from the failure mode. Combined with document 09's `-fpass-fuel`, which bisects to the individual transformation site, a random miscompilation goes from discovery to "this rewrite rule, this line" without a human in the loop for the mechanical part.

## 15.5 Translation validation and proof

Three uses of formal methods, in decreasing order of how settled they are.

**Rule verification is the primary one and it is not optional.** Every rewrite rule in document 09 and every lowering and peephole rule in document 10 carries an SMT specification, discharged by `rucc-verify` in CI. An unverified rule does not enter the rule set. This is the design decision that most distinguishes the project, it follows Crocus (ASPLOS 2024) directly, and its value is that the largest historical source of compiler miscompilation is closed by construction rather than by testing.

Some rules will not be discharged automatically. Solver timeouts on wide bitvector multiplications are the usual reason. Those get a bounded verification over restricted widths plus an explicit, reviewed, checked-in justification. The count of such rules is a reported metric, and it going up is a signal.

**Function-level translation validation** in the style of [Alive2](https://github.com/AliveToolkit/alive2): given the IR before and after a pass, ask a solver whether the transformation is refinement-preserving. This works on small functions and times out on large ones, so it runs as a fuzzing mode (generate small IR, run one pass, validate) rather than as a gate. The absence of poison from document 08.4 makes this substantially more tractable than it is for LLVM, since the refinement relation is simple equality on defined behavior, which is a benefit worth naming when document 19 weighs the cost of that decision.

**Not attempted:** a verified compiler in the CompCert sense. The proof burden is a different project with a different scope, and CompCert's own code quality demonstrates the cost. We take the parts of the technique that pay for themselves.

## 15.6 Sanitizers on ourselves

The compiler is a large Rust program that manipulates arena indices and does its own bit-packing, and `unsafe` exists in the arenas and the memory-mapped source handling. CI runs the test suite under Miri where it is fast enough, under ASan and TSan on the native builds, and with `RUSTFLAGS=-Zsanitizer` on nightly. Every `unsafe` block carries a safety comment justifying it, enforced by `clippy::undocumented_unsafe_blocks`.

Compiling untrusted input is a security boundary. People run compilers on code they did not write, and a crash on malformed input is a bug and potentially worse. So the compiler is itself a fuzz target: `cargo-fuzz` runs against the driver with random bytes, random mutations of corpus files, and structurally valid but semantically hostile programs (10000-deep nesting, gigabyte string literals, cyclic typedefs). The requirement is a clean diagnostic or a controlled ICE, never a panic in `unsafe` code and never a hang.

## 15.7 The CI matrix

**Per commit**, and required to merge:

| Host | Target | Configuration |
|---|---|---|
| Linux x86-64 | x86-64 | debug, all tests, verifier on |
| Linux x86-64 | x86-64 | release, all tests |
| Linux x86-64 | aarch64 | cross, execution under QEMU |
| Linux x86-64 | riscv64 | cross, execution under QEMU |
| macOS arm64 | arm64 | native, all tests |
| Windows x86-64 | x86-64 | native, all tests |

Plus: rung 1 of the ladder in full; a one-hour Csmith run; `rucc-verify` over the whole rule set; the determinism check (build twice, compare bytes); a clippy and format gate; and the compile-throughput benchmark from document 16 with a regression threshold.

**Nightly:** rungs 2 and 3 in full at all optimization levels; an eight-hour randomized differential run; the full torture suite; the DWARF differential from document 11.4; the ABI differential from document 12.10; sanitizer builds of the compiler; and the code-quality benchmark suite with results published to a tracking dashboard.

**Weekly:** kernel levels B and C; a translation-validation campaign; a bootstrap-equivalence run over the whole corpus comparing every optimization level pairwise.

**The rules that make this a gate rather than decoration:** a red main branch blocks all merges. A performance regression over threshold blocks the merge unless the PR states the tradeoff. A new `#[ignore]`, a new corpus exclusion, or a new unverified rule requires an issue number. And no test is deleted to make CI green. It is marked with an issue and counted in a report that is visible.

## 15.8 What we measure about our own testing

Line and branch coverage over the compiler, per crate, with the frontend held to a higher bar than the optimizer because frontend gaps are behavior gaps. Mutation testing on the type checker and the constant evaluator, where a surviving mutant is a genuine missing test rather than noise. The count of IR opcodes with no lowering rule, which should be zero and is not yet: `rucc_codegen::coverage` keeps it, with the reason and the issue beside every entry, and prints it where CI can be read. The count of GNU features in document 13's matrix by status. The number of days since the last miscompilation found by each mechanism, which is the single most informative number in the project: when the fuzzers stop finding bugs, either the compiler is good or the fuzzers are exhausted, and knowing which is the difference between shipping and pretending.
