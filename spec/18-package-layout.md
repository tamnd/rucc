# Package layout

A Cargo workspace of small crates with a strictly layered dependency graph. The layering is the mechanism behind document 00's modularity claim: "easy to extend" is not a property of good intentions, it is a property of a dependency graph in which the extension point does not depend on the thing you would otherwise have to change.

## 18.1 The tree

```
rucc/
├── Cargo.toml                  workspace
├── xtask/                      build automation, layer checking, blessing, benchmarks
├── crates/
│   ├── rucc-base/           0  arenas, interning, index newtypes, SoA helpers, sorted maps, the rule matcher
│   ├── rucc-diag/           1  diagnostics, spans, source maps, rendering, JSON output
│   ├── rucc-target/         1  TargetInfo, register files, machine models, ABI descriptions
│   ├── rucc-types/          2  the C type system, interned; layout computation
│   ├── rucc-session/        3  Session, options, the per-compilation context
│   ├── rucc-gnu/            4  features.toml, attributes, builtins, pragmas
│   ├── rucc-lex/            4  phases 1 to 3, pp-tokens, the fast scanner
│   ├── rucc-pp/             5  macro expansion, conditionals, includes, the header cache
│   ├── rucc-ast/            6  the AST, arena-allocated, and its printer
│   ├── rucc-parse/          7  recursive descent + Pratt, declarators, error recovery
│   ├── rucc-sema/           7  type checking, conversions, initialization, const eval, TAST
│   ├── rucc-ir/             8  the IR, its printer, parser and verifier
│   │   └── rules/              what the IR's terms mean, which both rule sets are read against
│   ├── rucc-object/         8  ELF, Mach-O, COFF writers
│   ├── rucc-lower/          9  the TAST to IR walk, SSA construction, ABI-directed lowering
│   ├── rucc-opt/            9  pass manager, ægraph, rules, analyses, the pipelines
│   ├── rucc-mir/            9  MIR, its printer and parser
│   ├── rucc-asm/            9  encoders, the assembler, inline asm, relaxation
│   ├── rucc-debug/          9  DWARF generation
│   ├── rucc-lto/           10  module merging, summaries, Thin LTO
│   ├── rucc-regalloc/      10  both allocators and the allocation checker
│   ├── rucc-codegen/       11  selection, scheduling, layout, frames, prologue/epilogue
│   │   └── rules/              the lowering rule sets, one file per target, and their models
│   │                           each of which includes the IR model above
│   ├── rucc-driver/        12  CLI, phase graph, job scheduling, linker invocation
│   └── rucc/               13  the binary; also the library entry point
├── build-tools/
│   ├── rucc-rules/             the rule DSL compiler; a build dependency, never a runtime one
│   └── rucc-verify/            SMT verification of the rule set; CI only
├── runtime/
│   └── rucc-builtins/          #![no_std], compiled *for the target*, not for the host
└── tests/
```

The rule sets sit under `rucc-codegen` rather than at the root because a published crate has to build from its own source archive, and a build script that reads a file outside the package it belongs to cannot. `rucc-verify` reads them from there as well, so there is one copy of every rule and one gate over it.

Twenty-three library crates, two build tools, one runtime library, one binary. `rucc-arena` and `rucc-intern` are reserved on crates.io but are modules inside `rucc-base`. The split is not worth two crates, and holding the names costs nothing while preventing a confusing squat. All names in this tree were confirmed unclaimed on crates.io on 2026-08-31.

`rucc-lower` is the crate that keeps the rest of the layering honest. It owns the walk from the typed AST to the IR, which means it is the only crate that sees both the C type system and the IR at once. Without it that walk would have to live in `rucc-ir`, and `rucc-opt` would then transitively depend on the AST and the type system, which is exactly what section 18.2 promises cannot happen.

## 18.2 The layer rule

**A crate may depend only on crates of a strictly lower rank.** The number in the tree above is the rank. It is a total order over the workspace, recorded in `xtask/layers.toml`, and it is stronger than the coarse conceptual stack in document 03: two crates that share a rank cannot depend on each other in either direction, so a rank is a set of crates that could be built in parallel and reviewed independently.

Ranks are assigned rather than derived, which is the point. Adding a dependency that the current ranks forbid is a design change, and it shows up in review as an edit to `layers.toml` rather than as one more line in a `Cargo.toml` that nobody reads.

This is enforced by `cargo xtask layers`, which is a required CI job, because a layering convention that is not mechanically checked is a layering convention that held for six months.

The rule has consequences that are worth stating so they are not discovered as annoyances:

- `rucc-opt` cannot see the AST or the type system. It outranks `rucc-ir` but not `rucc-ast` or `rucc-types`, and everything the optimizer needs must therefore be in the IR. This is what document 08 means by "everything C-specific has been resolved by document 07 and must not be re-derived here", and the layer rule is what makes that enforceable rather than aspirational.
- `rucc-codegen` cannot see `rucc-opt`. The backend consumes IR, not optimizer state.
- Nothing below `rucc-driver` knows about the file system or the command line. Everything is threaded through `Session`, which is what makes the compiler usable as a library and testable without a driver.
- A build tool may be a build dependency of a crate in the stack, and may be nothing else to it. `rucc-codegen` compiles its rule files with `rucc-rules` while it is being built, and nothing `rucc-rules` defines is linked into the compiler. `cargo xtask layers` allows that one direction and refuses a build tool as an ordinary or a development dependency, either of which would put the tool in the compiler.
- **No target-specific code outside `rucc-target` and the rule sets.** Target facts are data, consumed generically. `xtask` additionally greps the middle-end crates for target names and fails on a match, which is a crude check that catches the realistic mistake.

Cyclic dependencies are impossible by construction, which matters more than it sounds: a cycle in a compiler's module graph is how "the parser needs the type checker needs the parser" situations get resolved badly, and the typedef problem in document 06 is exactly such a situation, resolved there by parser-side scope tracking specifically so that this rule holds.

## 18.3 External dependencies

ccc's dependency-free design is admirable and we are not copying it. Writing our own DWARF emitter and ELF writer would consume months to produce something worse than what exists, and the portability argument that justifies not shelling out to `as` and `ld` does not apply to a Rust library that compiles everywhere we do.

But the dependency list is short, deliberate, and each entry is justified in writing:

| Crate | Why | Where |
|---|---|---|
| `object` | ELF/Mach-O/COFF reading and writing; mature, widely used | `rucc-object` |
| `gimli` | DWARF writing; the same, and by the same authors | `rucc-debug` |
| `memmap2` | memory-mapped source files, per document 05 | `rucc-driver` only |
| `rustc-hash` | FxHash; fast, non-cryptographic, deterministic | `rucc-base` |
| `hashbrown` | raw entry API for hash-during-scan interning | `rucc-base` |
| `rayon` | work-stealing pool for the two levels of parallelism | `rucc-driver` only |
| `libc` / `windows-sys` | process spawning, file operations the std lacks | `rucc-driver` only |

`memmap2` is listed against the driver rather than against `rucc-base` because the driver is the only crate allowed to know that a file system exists. Everything below it reads through the file system trait, which is what keeps a test below the driver from reading the machine it runs on.

**Rules.** No dependency that pulls a proc-macro toolchain into the compiler's build: `syn` and its dependents are permitted in `xtask` and `rucc-rules`, which are build tooling, and nowhere else. No dependency with `unsafe` we have not read. No dependency that is one person's unmaintained crate. Versions are pinned in a committed lockfile and `cargo-deny` runs in CI for licenses and advisories. Adding a dependency requires an entry in this table, which makes it a reviewable decision rather than an incidental one.

The total dependency tree should stay small enough to audit, under fifty crates including transitives, and the number is reported by CI so that drift is visible. CI reports it twice, once for what the compiler binary pulls in and once for the whole workspace, because the second number includes the build tooling of section 18.3's rule about proc macros and a reader comparing the two should not have to work out which one the budget is about. It is about the compiler.

## 18.4 Features

Cargo features are used sparingly, because a feature is a configuration that must be tested and an untested configuration is a broken one.

`target-x86_64`, `target-aarch64`, `target-riscv64` gate the target crates, defaulting to all. `llvm-compat` gates nothing yet and is reserved. `serde` on the IR and diagnostic crates for tooling. That is the whole list, and CI builds the powerset, which is only affordable because the list is short.

There is no feature that changes compiler *behavior*. A flag does that, at runtime, where it can be tested.

## 18.5 Stability tiers

The crates are published, and publishing implies a promise, so the promise is graded explicitly and stated in each crate's README and top-level documentation.

**Tier 1: stable at 1.0.** `rucc` (the binary's command-line interface), and the observable behavior of the compiler: which programs compile, what they do, what the diagnostics mean. This is the actual product and it is what semantic versioning applies to.

**Tier 2: stable within a major version after 1.0.** `rucc-ir`'s textual form, per document 08.10. `rucc-diag`'s JSON output, because editors will consume it. The `--emit=` formats, loosely: they round-trip, and their syntax is stable, but new instructions may appear.

**Tier 3: explicitly unstable, forever unless promoted.** Every other crate's Rust API. These are published so the workspace can be published and so people can read them, not because we intend to keep them fixed. Each carries a prominent warning to that effect, and each is versioned in lockstep with the workspace so there is no illusion of independent stability.

Being honest about tier 3 up front is the difference between a modular compiler and an accidental API surface that becomes impossible to change. The alternative, publishing internals without a warning and then breaking them, is how projects acquire a reputation for churn.

## 18.6 Versioning and release

One version number for the whole workspace, bumped together. Rust edition 2021 with a stated MSRV, tested in CI, and raised deliberately rather than by accident. A compiler that requires a nightly Rust or last week's stable is a compiler people cannot use on the systems where they need it.

Pre-1.0 versions carry no compatibility promise at all and say so. The 1.0 release is gated on document 17's M9 at minimum, because publishing a 1.0 that has not met the axes in document 02 would make the axes decorative.

## 18.7 Building the compiler

`cargo build` works with no configuration and no external tools, on all three hosts, which is a requirement and is checked. `cargo xtask` provides the rest: `layers`, `bless`, `bench`, `verify-rules`, `corpus`, `matrix`, and `fuzz`. There is no `configure`, no CMake, no Python in the build, and no code generation step that is not a `build.rs` or an `xtask`, which is what makes the "clone and build" experience match the claim, and which is worth protecting, because build system complexity accretes silently.
