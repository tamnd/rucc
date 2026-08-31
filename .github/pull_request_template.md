## What this changes

<!-- The problem first, then the change. What is in the diff does not need restating. -->

## Why

<!-- If it closes an issue, say `Closes #N`. If it implements part of a milestone, say which. -->

## How it was verified

<!-- Which test fails without this change. If none does, say so and say why. -->

## Checklist

- [ ] `cargo xtask ci` passes locally
- [ ] A change to behavior comes with a test that fails without it
- [ ] A new rewrite or lowering rule comes with its SMT specification
- [ ] A performance claim comes with the command that reproduces it, median of ten runs with the interquartile range
- [ ] A new dependency comes with a row in the table in `spec/18-package-layout.md` section 18.3
- [ ] A new `#[ignore]`, corpus exclusion or unverified rule comes with an issue number
- [ ] Prose follows the house rules: plain English, no em dashes, no horizontal rules, no hard-wrapped sentences
