# The rewrite rules

The middle end's rewrites are written as rules rather than as code. This directory is where they live, one file per tier of `spec/optimizer/13-rewrite-rules.md` section 13.4, in the language `build-tools/rucc-rules` reads. They sit inside `rucc-opt` because that is the crate whose build compiles them into the table the simplifier matches with, and because a published crate has to build from its own source archive.

A rule here says that one IR term and another IR term compute the same thing. That is the difference from `crates/rucc-codegen/rules`, where a rule says that an IR term and a machine term do, and it is the whole of the difference: the language is the same language, the matcher is the same matcher, and the proof obligation is the same obligation.

Every rule file has a model file beside it with the same name and a `.model` extension. `simplify.model` is one line, an include of `crates/rucc-ir/rules/ir.model`, and it has nothing of its own because a rewrite from the IR to the IR has no terms in it beyond the ones the IR already has. A rule set that reached for a target's terms would need more, and it would also be a rule set in the wrong crate.

Nothing enters the rule set unverified. `cargo run -p rucc-verify -- crates/rucc-opt/rules` reads every file here, asks a solver about every rule in it, and refuses the file if anything comes back as less than a proof. That is a required CI job, and it is the same job the lowering rules go through.

`simplify.rules` is tier one, the identities: the rewrites that hold at every value of every operand, need nothing known about the operands to fire, and leave a term strictly smaller than the one they replaced. Adding nothing, multiplying by one, and'ing a value with itself. Each is written out at every width it holds at, which is four lines where a reader might want one and is what lets each of them be read and proved on its own.

A replacement is one of two shapes and the pass relies on that. `(value.iN x)` means the result is a value the function already has, and `(iconst.iN k)` means it is a constant. `crates/rucc-opt/src/simplify.rs` has a test that walks the table and fails on any rule leaving something else, because such a rule would be matched, found to be neither, and skipped, and nothing at run time would say the rewrite had stopped happening.
