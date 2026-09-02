# The rule sets

Instruction selection and the middle end's rewrites are written as rules rather than as code. This directory is where those rules live, one file per target, in the language `build-tools/rucc-rules` reads and `spec/10-backend.md` section 10.2 describes.

Every rule file has a model file beside it with the same name and a `.model` extension. The rules say what to match and what to put in its place, and the model says what the terms in them mean in bitvectors. The two are separate files because a rule is about a rewrite and a model is about a target, and because the model is the thing a reviewer reads when they want to know what the compiler believes an instruction does.

Nothing enters the rule set unverified. `cargo run -p rucc-verify -- rules` reads every file here, asks a solver about every rule in it, and refuses the file if anything in it comes back as less than a proof. That is a required CI job. A rule the solver cannot settle at its own width may carry a `(bounded "...")` clause giving a reason to accept a proof at narrower widths instead, and the number of rules that needed one is printed on every run, because that number going up is the signal worth watching.

There are no rule files here yet. The x86-64 lowering set is the next piece of M3, and the gate is standing before the rules arrive rather than after.
