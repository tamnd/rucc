# Contributing

Thanks for looking. This document is short because most of what a contributor needs to know is in [`spec/`](spec/), and this is only the part about how changes get made.

## Before you start

The project is at M0. There is a lot of design written down and very little code, which means the highest value contribution right now is reading a specification document and telling us where it is wrong. Open an issue with `kind/open-question` if something in there does not hold up.

If you want to write code, take a milestone issue or a piece of one, and say so on the issue first. The milestones are ordered for a reason and work on M4 before M2 exists is work that gets thrown away.

## Running the checks

```
cargo xtask ci
```

That runs everything the per-commit CI job runs, in the same order, so a green run locally means a green run in CI. The order is cheapest first, so a formatting mistake costs you seconds rather than a full test run.

The individual pieces:

```
cargo xtask layers      # the dependency graph against xtask/layers.toml
cargo xtask style       # prose against the house rules
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

## What a change has to come with

**A change to behavior comes with a test that fails without it.** Not a test that exercises the code, a test that fails. If you cannot write one, say so in the pull request and explain why, and we will work out what the right test is together.

**A new rewrite rule or lowering rule comes with its SMT specification.** `spec/15-testing.md` section 15.5 makes rule verification non-optional. A rule the solver cannot discharge is allowed in with a bounded proof over restricted widths and a written justification, and the count of those is a reported metric, so it needs to be a considered decision rather than a shrug.

**A performance claim comes with the command that reproduces it.** `spec/16-performance.md` section 16.1 has six reporting rules and they apply to pull request descriptions as much as to the README. Median of ten runs with the interquartile range, the full table including the losses, and the machine described.

**A new dependency comes with a row in the table in `spec/18-package-layout.md` section 18.3.** Adding one should be a reviewable decision, which is what the table is for.

**A new `#[ignore]`, a new corpus exclusion or a new unverified rule comes with an issue number.** No test is deleted to make CI green. It is marked, given an issue, and counted in a report that is visible.

## Style

**Rust.** `cargo fmt` decides layout, so there is nothing to argue about there. Beyond that: comments explain why, not what, and a comment that restates the line above it is worse than no comment. Public items get documentation. Anything with a precondition gets a `# Panics` section, and `clippy::undocumented_unsafe_blocks` means every `unsafe` block gets a safety comment.

Where a decision in the code follows from something in the specification, cite the document. `// per spec/12-abi-and-runtime.md section 12.3` costs one line and saves the next person an afternoon.

**Prose.** README, specification documents, commit messages, issue and pull request text. Plain English, written the way you would explain it to a colleague. No em dashes and no en dashes: a comma, a colon, a period, parentheses or the word "to" all work and one of them is always right. No horizontal rules; use a heading. Do not hard-wrap sentences across lines, because a one-line-per-paragraph file produces readable diffs and a wrapped one does not. `cargo xtask style` checks the first two of those mechanically.

Publish the losses next to the wins. A benchmark table with the regressions removed is not a benchmark table.

## Commits and pull requests

One logical change per commit. The subject line is imperative and under about seventy characters, the body says why rather than what, because what is in the diff.

Pull requests describe the problem, then the change, then how it was verified. If it touches performance, the numbers go in the description. If it touches the layer graph, say which rank changed and why.

Rebase rather than merge. The history is meant to be bisectable, and `spec/14-target-ladder.md` says that a commit which breaks a climbed rung gets reverted rather than fixed forward, which only works if reverting is easy.

## Reporting a miscompilation

This is the most valuable kind of bug report the project can get, so it gets its own issue template and its own handling.

Reduce it if you can. `cvise` with an interestingness test is what we would do, and a nine-line reproducer is a bug report where a four-thousand-line one is a project. If you cannot reduce it, file it anyway with the original source and we will reduce it.

Include the exact command, the target, the optimization level, and what the program should have done. If GCC or Clang agree with each other and disagree with us, say so, because that settles which compiler is wrong before anyone opens a debugger.

## Security

See [SECURITY.md](SECURITY.md). A crash on malformed input is a bug and possibly worse, because people run compilers on code they did not write.
