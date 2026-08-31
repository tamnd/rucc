# rucc

An optimizing C compiler written in Rust.

The goal is a compiler that builds real software without patches, generates code that stands up next to `gcc -O2`, and runs on every platform its users do. The target ladder goes SQLite, then a mid tier of well known C projects, then PostgreSQL, then the Linux kernel. Nothing on that ladder is allowed a source patch. If a project needs one, the compiler is wrong.

This is early. Nothing compiles C yet. What exists is the workspace, the layer rule that keeps it modular, the driver skeleton, and CI that is green on Linux, macOS and Windows from the first commit. The full technical design is written down in [`spec/`](spec/) before it is built, and the milestones that build it are tracked as issues.

## Why another C compiler

There are good C compilers already, and the honest answer is that this one is trying to occupy a spot none of them do.

GCC and Clang are excellent and enormous. Their code quality is the bar, and their compile throughput is not: most compilations in the world are `-O0` or `-O1`, and that is the case both of them optimize least. The small compilers, chibicc and cproc and ccc among them, are readable and fast to build and are not trying to compete on generated code. TCC is fast and does not optimize.

rucc is aiming at four things at once, stated as falsifiable claims rather than aspirations:

1. **Correctness.** Zero known miscompilations under active search. The operative words are active search: continuous Csmith and YARPGen differential runs, a rewrite rule set every member of which is discharged by an SMT solver before it is allowed in, an IR verifier that runs after every pass, and a register allocation checker.
2. **Compile throughput.** Twice `clang -O0`, and 1.5 times `clang -O2`, measured as median of ten runs with the interquartile range published next to it.
3. **Code quality.** Within 10% of `gcc -O2` on scalar integer and pointer code. Explicitly not at parity on vectorized floating point, and saying so wherever numbers appear.
4. **Portability.** Three hosts and three targets, all in CI, all from early on rather than as a porting project later.

Every one of those is a number that can be measured and can come out wrong. [`spec/02-the-goal.md`](spec/02-the-goal.md) states them precisely, and [`spec/16-performance.md`](spec/16-performance.md) fixes the methodology and the reporting rules before there is anything to report, which is the only order in which those rules mean anything.

## Design in one page

**A rule set that is verified, not tested.** Middle-end rewrites and instruction selection patterns are written in one DSL. The same rule text is compiled into the matcher the compiler runs and handed to an SMT solver in CI. A rule that the solver cannot discharge does not enter the rule set. This follows Crocus (ASPLOS 2024), and it closes the largest historical source of compiler miscompilation by construction rather than by fuzzing after the fact.

**An IR with no poison and no undef.** Every rewrite has to be locally justifiable. `nsw` licenses specific proven rewrites instead of propagating a taint. The cost is some speculation we cannot do, and [`spec/19-open-questions.md`](spec/19-open-questions.md) treats that cost as an open question with a measurement attached rather than as a settled trade. The benefit is that SMT verification of the rules stays tractable, and that Alive2-style translation validation reduces to equality on defined behavior.

**Acyclic e-graphs in the middle end.** Rewriting, constant folding and global code motion as one fixpoint over an ægraph, in the shape Cranelift proved out. Whether that carries from a JIT with a microsecond budget to an AOT C compiler at `-O2` is the project's riskiest assumption, so M4 builds both an ægraph rewriter and a conventional pass pipeline over the same rule set, measures them, and takes the winner.

**A layer rule that is checked.** The workspace is 23 library crates with an assigned rank, and a crate may depend only on strictly lower ranks. `cargo xtask layers` is a required CI job. This is what makes "the optimizer cannot see the AST" a fact about the build rather than a claim in a document.

**No external assembler or linker in the compile path.** We encode instructions to bytes, we write ELF, Mach-O and COFF ourselves, and we emit DWARF ourselves. `-S` produces text from the same instruction description the encoder uses, so the two cannot disagree. Linking still shells out to a real linker before 1.0, and whether that stays true is an open question with two measurements attached to it.

## Status

M0 and M1 are done, tagged v0.1.0 and v0.2.0. M0 was the workspace, the layer rule, the driver's argument parsing and phase plan, the job scheduler, and CI on Linux, macOS and Windows. M1 is the preprocessor, and it is finished: all five translation phases, hide set macro expansion, the full directive set including `_Pragma` and `#embed`, include resolution with `#include_next`, `#pragma once` and the multiple include optimization, the `__has_*` family, and predefined macros generated from the target description. A diagnostic names every macro it came out of, in the order a reader wants them.

`rucc -E a.c` is a real preprocessor. Its output is diffed against the reference compiler over the glibc and musl header sets on every commit. `rucc a.c` still prints the phase plan and then tells you the phases after preprocessing are not implemented, because the parser is M2. That is the honest summary.

```
$ rucc --print-config
version: 0.2.0
target: x86_64-unknown-linux-gnu
arch: x86_64
os: linux
env: gnu
object-format: elf
pointer-width: 64
long-width: 64
long-double-width: 128
endian: little
char-signed: true
opt-level: -O0
emit: exe
debug-info: false
```

The twelve milestones are in [`spec/17-milestones.md`](spec/17-milestones.md) and are tracked as issues. Three of them are sane stopping points: M5 is a correct, fast, optimizing compiler that builds SQLite; M9 adds PostgreSQL on three hosts and three targets; M11 is the kernel.

## Installing

Every tagged release publishes prebuilt binaries for Linux, macOS and Windows on [the releases page](https://github.com/tamnd/rucc/releases), each with a SHA-256 file and a build provenance attestation you can check with `gh attestation verify`.

From the registry, if you already have a Rust toolchain:

```
cargo install rucc
```

The whole workspace is published, so a crate can be depended on individually. `rucc-lex` is a C preprocessing token lexer, `rucc-pp` is a macro expander, and neither drags the rest of the compiler in with it. The layer rule in `xtask/layers.toml` is what makes that true: a crate depends only on crates of strictly lower rank.

## Building

```
git clone https://github.com/tamnd/rucc
cd rucc
cargo build --release
```

That is the whole of it, on Linux, macOS and Windows. No configure, no CMake, no Python in the build, no code generation step that is not a `build.rs` or an `xtask`. Everything else is a task:

```
cargo xtask layers    # check the dependency graph against xtask/layers.toml
cargo xtask style     # check the prose against the house rules
cargo xtask ci        # run what CI runs, in the order CI runs it
```

## Repository layout

```
crates/         the compiler, 23 library crates plus the binary
build-tools/    the rule DSL compiler and its SMT verifier, never linked into the compiler
runtime/        rucc-builtins, the only crate compiled for the target rather than the host
spec/           the technical design, twenty documents, written before the code
xtask/          build automation, including the layer rule and the prose check
tests/          the corpus and the golden files
```

[`spec/18-package-layout.md`](spec/18-package-layout.md) explains the rank of each crate and why it is where it is.

## The specification

Twenty documents, roughly 40,000 words, written before the implementation. They are in the repository rather than a wiki because they are reviewed and revised in pull requests like everything else, and because a design document that drifts from the code is worse than no design document.

| | |
|---|---|
| [00](spec/00-README.md) | What this is and how to read it |
| [01](spec/01-research-2026.md) | The research the design draws on, with citations |
| [02](spec/02-the-goal.md) | The four axes, stated as falsifiable claims |
| [03](spec/03-architecture.md) | The pipeline, the crate stack, the textual forms |
| [04](spec/04-driver-and-cli.md) | The driver, the flag surface, the phase graph |
| [05](spec/05-preprocessor.md) | The five translation phases and the header cache |
| [06](spec/06-lexer-and-parser.md) | The scanner, the parser, the typedef problem |
| [07](spec/07-types-and-semantics.md) | The C23 type system and the constant evaluator |
| [08](spec/08-ir.md) | The SSA IR, block parameters, and no poison |
| [09](spec/09-optimizer.md) | The ægraph, the rules, the analyses, the pipelines |
| [10](spec/10-backend.md) | Selection, allocation, scheduling, and the target ladder |
| [11](spec/11-asm-objects-debug.md) | The assembler, object writers, and DWARF |
| [12](spec/12-abi-and-runtime.md) | Four ABIs, struct layout, TLS, and the runtime |
| [13](spec/13-gnu-compat.md) | What `__GNUC__` obliges us to, as a machine-readable table |
| [14](spec/14-target-ladder.md) | The five rungs, and what climbing one means |
| [15](spec/15-testing.md) | Seven layers of testing and the CI matrix |
| [16](spec/16-performance.md) | Methodology, and the rules for what we may claim |
| [17](spec/17-milestones.md) | M0 through M11, with exit criteria |
| [18](spec/18-package-layout.md) | The crates, the layer rule, the dependency policy |
| [19](spec/19-open-questions.md) | What is not decided, and what would settle it |

## Contributing

Read [CONTRIBUTING.md](CONTRIBUTING.md). The short version is that a change to behavior comes with a test that would fail without it, a new rewrite rule comes with its SMT specification, and a performance claim comes with the command that reproduces it.

## Not in scope

C++, permanently. MSVC dialect extensions, `__declspec` and SEH included. A JIT. MSan. A verified compiler in the CompCert sense. Each of those is discussed where it belongs in the specification, and each is out of scope or post-1.0 rather than quietly unmentioned.

## License

MIT or Apache-2.0, at your option.
