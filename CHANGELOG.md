# Changelog

All notable changes are recorded here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses [semantic versioning](https://semver.org/spec/v2.0.0.html) with the caveat in `spec/18-package-layout.md` section 18.6: pre-1.0 versions carry no compatibility promise at all.

## Unreleased

### Added

- Macro expansion in `rucc-pp`: object-like and function-like macros, `#` and `##`, variadic macros in both the standard `__VA_ARGS__` spelling and the GNU named form, `__VA_OPT__`, and the GNU comma swallowing extension. It is Prosser's hide set algorithm rather than an expansion depth counter, so mutually recursive macros come out right. Both of the standard's own examples from 6.10.4.5 are tests.
- Hide sets are interned, so a token carries a four byte index rather than a set, and the same set produced by the same nest of headers is stored once.

### Known limits

There are no directives yet. `#define` lines are parsed but nothing reads them off a file, and there is no `#if`, no `#include` and no header cache. Those are the next piece of M1.

## 0.1.0

M0, the skeleton. The compiler does not compile anything: `rucc a.c` prints the phase plan and then says the phases are not implemented. What this release is for is the shape everything else gets built inside, and the checks that keep that shape honest.

### Added

- The workspace: 23 library crates, the `rucc` binary, the rule DSL and its verifier under `build-tools/`, and the target-side runtime under `runtime/`.
- The layer rule, ranked in `xtask/layers.toml` and enforced by `cargo xtask layers`.
- `rucc --print-config`, `--version` and `--help`, which is the M0 exit criterion in `spec/17-milestones.md`.
- Target triple parsing and the target data model for x86-64, AArch64 and RISC-V 64 across Linux, Apple platforms and Windows.
- Diagnostics, spans and the per-compilation `Session`.
- CI on Linux, macOS and Windows, with formatting, lints, tests, the layer check, the prose check, a supply chain audit and a minimum supported Rust version job.
- The twenty document specification under `spec/`.
- The phase graph: `Plan` is pure data, so `-###` can print the plan without touching the file system, `-v` prints it while running, and `-x` forces an input language.
- Job scheduling across translation units, with `-j`. Results merge in input order rather than completion order, which is what keeps output byte identical between `-j1` and `-j16`.
- Translation phases 1 to 3: the byte order mark, line ending normalisation, trigraphs behind `-trigraphs`, line splicing, comments, and preprocessing token formation, with identifiers interned during the scan.

### Known limits

Nothing compiles C yet. The preprocessor and the parser land in M1 and M2.

`-j` changes the worker count that `-v` reports and nothing else, because the work it schedules is still a placeholder.

The lexer reads a file into memory rather than mapping it, and skips whitespace and comment bodies a byte at a time. Both are M1 performance items with a benchmark attached.
