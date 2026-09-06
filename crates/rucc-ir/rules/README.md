# What the IR means

`ir.model` says what every term a rule can be written about computes, in bitvectors and in floats. It is the file a reviewer reads to find out what the compiler believes one of its own instructions does, and `build-tools/rucc-verify` reads it to turn a rule's `spec` clause into something a solver can answer.

It is here rather than beside a rule set because two rule sets are written about these terms. `rucc-codegen` lowers them to machine terms and `rucc-opt` rewrites them to more of themselves, and a rewrite and a lowering are the same claim about two terms. Two spellings of what `add.i32` means would be two vocabularies over one IR, and nothing would say so the day they disagreed. The names themselves come from `crates/rucc-ir/src/term.rs`, which is where an instruction is given the name a rule file writes, and that file is here for the same reason.

A model that wants these writes `(include crates/rucc-ir/rules/ir.model)` and adds its own heads underneath. The path is counted from the root of the repository rather than from the including file, so it reads the same as the path in the prose beside it. A head given a meaning in two of the files that are read together is refused, which is what makes the split load bearing rather than merely tidy.

Two IR heads are missing from it, and the line they are on the other side of is worth stating. A term whose meaning is the same wherever it is compiled belongs here. A load and a store do not have one: what four bytes at an address mean depends on which end of the value sits at the lowest of them, which is a fact about the target, so those entries live in the target's model. `crates/rucc-ir/tests/model.rs` writes that list out and fails if it grows, so the exception stays an exception.

That test also checks the two lists of names against each other. The heads here and the names `crates/rucc-ir/src/term.rs` gives an instruction are the same list written twice, and a name spelled one way in one and another way in the other is a rule that never fires, which is the quietest mistake a rule set can have.

There are no rules in this directory and there will not be. A rule says what to put in a term's place, which is a question for a rule set, and what is here is only what the terms mean.
