# Verification: establishing that the monitor is faithful

Document 04 section 4.5 states the soundness claim in two directions. This document is the apparatus that establishes them, and it exists because of one asymmetry, already stated in document 07 and worth stating once more as the premise of everything here:

> A wrong optimization produces a wrong answer, which a differential test finds. A wrong check *elimination* produces a correct answer on every test and a silent vulnerability in production. Nothing observes it.

Every mechanism in this document is a way of making that failure observable.

## 14.1 What is being established, and by what means

| Obligation | Means | Section |
|---|---|---|
| Each check-eliminating rewrite rule is sound | SMT, per rule, in `rucc-verify` | 14.2 |
| The analyses feeding the rules are not wrong in practice | differential check accounting | 14.3 |
| Elimination does not lose reports on synthetic programs | randomized elimination fuzzing | 14.4 |
| Detections do not regress | the escape suite | 14.5 |
| The bug model's rows are covered | Juliet, per row | 14.6 |
| Kernel coverage is what we say it is | the ACSAC 439-CVE replay | 14.7 |
| The monitor itself is correct | conventional testing, stated as the trust set | 14.8 |
| No false positives | document 12's corpus; empirical, no proof available | 12.6 |

Nothing here proves the compiler correct and nothing here claims to. The parent's document 15 makes the same disposition: verify the narrow, mechanical, high-leverage things (the rewrite rules) and test everything else hard.

## 14.2 Rule verification

Every rule that removes a check, weakens a check, or elides a plane write is data in the `safety/` namespace of `rucc-codegen`'s rule tree (document 07 section 7.7), and `rucc-verify` discharges an obligation for each.

**The obligation.** For a rule of the form "check *C* may be removed in context Γ":

> for all machine states satisfying Γ, C does not trap.

Encoded as an SMT query over bitvectors, in the manner of [Crocus](https://cs.wellesley.edu/~avh/veri-isle-preprint.pdf), which the parent's document 10 already commits to for instruction selection. The encoding reuses the parent's semantics for the arithmetic and adds a small theory for the planes: `plane_ver(a)`, `plane_type(a)`, `plane_init(a)` as uninterpreted functions constrained by the effects of `meta.*` operations.

**Worked example.** The redundancy rule from document 07 section 7.3:

```
Γ ≡  established(%c, lo', ext')  ∧  %p ≥ lo'  ∧  %p + n ≤ lo' + ext'
     ∧  no redefinition of %c between the establishing point and here
C ≡  check_bounds %c, %p, n
```

The query is whether `(%p - %c.lo) <u (%c.ext - n)` can be false under Γ. It is not, provided `established` genuinely means what the analysis intends, which is the next section's problem, not this one's.

**Why this is tractable.** A rule is small, its context is an explicit hypothesis rather than something to be inferred, and the query has no loops. This is Alive2's insight applied one level down: verifying a *transformation* is enormously easier than verifying a *compiler*, and the transformations are where the subtle errors are.

**Why it is not sufficient.** Two gaps, both real:

- **The context is asserted, not verified.** `established(%c, lo', ext')` is produced by an unverified dataflow analysis. If the analysis says a fact holds where it does not, a correct rule removes a necessary check. Section 14.3.
- **The plane theory is a model of the runtime, not the runtime.** If `rucc-safe-rt`'s `meta_end` does not actually write the range the theory says it does, the verification is about a different program. Section 14.8, and it is why the runtime is small and is in the trust set explicitly.

**CI posture.** A rule without a discharged obligation does not ship. Timeouts are failures, not passes; a rule the solver cannot handle is rewritten until it can be, which is a constraint on rule complexity and is a healthy one.

## 14.3 Differential check accounting

**The highest-value test in this specification.** It closes the first gap in 14.2 empirically, on real code, automatically, with no annotation burden.

**The procedure.** Build each corpus project twice from the same source at the same tier:

- **Build A:** all checks inserted, *no elimination at all*. Slow, and by construction it performs every check document 06 says to perform.
- **Build B:** the normal build, with document 07's elimination.

Run both over the same inputs, the project's test suite, its OSS-Fuzz corpus, the CVE reproducers. Collect the set of reports from each, keyed by `(class, source location, dynamic occurrence index)`.

**The assertion: `reports(A) ⊆ reports(B)`.**

Any report in A and not in B is a check that elimination removed and that would have fired: an unsound elimination, caught on real code, attributed to a specific check at a specific line. That is the exact failure mode nothing else in this project can observe.

**Why the subset holds in that direction.** B may legitimately report *more* than A in one narrow case (`-fsafety-on-error=continue` means a suppressed-and-continued violation in A can change subsequent state) so the runs are made with `abort` semantics for this test and each divergence is investigated individually rather than assumed benign. In practice the sets should be equal and any difference in either direction is investigated.

**Cost.** Doubles the corpus run time, which is why it is nightly rather than per-commit. Its value justifies it: this is the mechanism that lets check elimination be aggressive, and without it the correct engineering posture would be to eliminate almost nothing.

**Extension: three-way accounting.** Adding a build C at a lower optimization level gives a second comparison and localizes whether a lost report came from safety elimination or from an ordinary optimizer bug. Cheap, and useful for triage.

## 14.4 Randomized elimination fuzzing

Differential accounting tests the paths the corpus executes. This tests paths nobody wrote.

**The procedure.** Csmith and YARPGen generate programs that are free of undefined behavior by construction, which is exactly the wrong thing for a memory-safety checker, they contain no bugs to find. So:

1. Generate a UB-free program.
2. **Inject one memory error at a known point**: shift an array index past the bound, free a pointer that is used later, read a local before writing it, cast a pointer through an incompatible type. The injection is mechanical and the ground truth (class, source location, expected dynamic occurrence) is known by construction.
3. Compile at every combination of tier and optimization level.
4. Assert every build reports the injected error, at the injected location, with the injected class.

This is the parent's document 15 differential-execution harness with a different oracle, so it reuses the generation and reduction infrastructure rather than building its own. The reducer is the valuable part: a failing case reduces to a minimal program, which is what makes the failures actionable.

**A second oracle, free.** A generated program with *no* injected error must produce *no* reports at any tier. That is a false-positive test over an unbounded supply of legal C, and Csmith's whole design goal is to exercise the corners of the language, which is precisely where a model that is subtly wrong about C will show it. This may find model bugs the corpus never does.

## 14.5 The escape suite

The regression mechanism, named because its job is to prevent detections from escaping.

**The rule: every real bug found by any means becomes a permanent test case.** Corpus run, fuzzer, syzkaller, a developer's laptop, an upstream report, reduced to a minimal program plus an input, with an `expect.toml` naming the class and location per document 12.3, and committed.

A regression that reintroduces a missed detection then fails CI the same way a miscompilation regression does, which is the parent's discipline applied to a different property. This is the mechanism that makes the coverage in document 03's matrix *monotone*: it can go up, and it cannot silently go down.

The suite grows without bound and that is fine; these are small programs and the run is embarrassingly parallel. What matters is that adding to it is a required step of the triage process in document 12.6, not an optional courtesy.

## 14.6 Juliet, per row

Document 03's matrix has a row per class and every row with a CWE column runs the corresponding Juliet cases at every tier, reporting detected / missed / false-positive.

**How the numbers are reported.** Per row, per tier, as raw counts, with the missed cases *enumerated by test id* rather than summarized as a percentage. A missed case is either a document 17 entry with a reason or a bug. The comparison points, from document 01: the SEI's Pointer Ownership Model work evaluated against all 4,604 cases for the five temporal CWEs, and PoisonCap against 2,776 cases across three classes.

**What Juliet is not.** Synthetic, uniform in shape, and its false-negative profile is not real code's. A tool can score 100% on Juliet and be useless. It is a floor and a regression detector, and document 12's CVE corpus is the number that means something. Reporting Juliet as the headline result is a well-established way to overstate a tool in this field and this project will not do it.

## 14.7 The ACSAC replay

The kernel coverage claim, and the one number that goes into an existing published table.

The [ACSAC 2025 study](https://dl.acm.org/doi/10.1145/3708821.3733916) classified 439 Linux and FreeBSD vulnerabilities and assigned each a CHERI outcome (35-61% mitigated, depending on configuration) and a Rust outcome (84%). Two numbers come out of the replay:

**Predicted coverage:** each of the 439 classified by hand against document 03's matrix at Tier K. Cheap, doable before any kernel work exists, and it is the sanity check on whether the kernel effort is worth starting.

**Observed coverage:** for the subset with working reproducers, actually run against a Tier K kernel.

**The gap between them is the most informative number this project produces**, because it measures what document 02's boundary limit costs in practice. A predicted 80% and an observed 45% would say the boundary holes are where the bugs are and that document 10 is the project rather than document 07. Both numbers are published regardless of which way they fall.

## 14.8 The trust set of the verification itself

Stated, because a verification effort that does not state its own assumptions is doing the thing document 10.2 exists to prevent.

**The SMT encoding is faithful to the IR semantics.** If the encoding of `check_bounds` differs from what `rucc-safety` emits, the proof is about a different instruction. Mitigated the way the parent's document 15 mitigates it for instruction selection (the encoding is generated from the same rule data the compiler uses) and not eliminated.

**The runtime is correct.** `rucc-safe-rt`'s plane operations must do what the plane theory says. This is ordinary code, tested ordinarily, and it is small *on purpose*: the whole runtime is a few thousand lines, because everything the compiler can do at compile time it does, and the runtime is left with allocation, the planes, the boundary wrappers and the reporter.

**The solver is correct.** Standard and universally assumed.

**The specification documents are consistent with each other.** No mechanism enforces this; it is prose. The cross-references are dense on purpose so that an inconsistency is more likely to be noticed, and the numbering commitments (document 03 referring to document 09 section 9.4, and so on) make a section that drifts visible.

**The model in document 04 is the right model of C.** The largest assumption in the whole specification, and the only evidence available for it is document 12's false-positive count over real code. Which is why axis 2 outranks axis 1, and why triage bucket 2 ("the model is wrong") is a release-blocking outcome rather than a curiosity.
