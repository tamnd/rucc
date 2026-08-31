# Changelog

All notable changes are recorded here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses [semantic versioning](https://semver.org/spec/v2.0.0.html) with the caveat in `spec/18-package-layout.md` section 18.6: pre-1.0 versions carry no compatibility promise at all.

## Unreleased

### Added

- Macro expansion in `rucc-pp`: object-like and function-like macros, `#` and `##`, variadic macros in both the standard `__VA_ARGS__` spelling and the GNU named form, `__VA_OPT__`, and the GNU comma swallowing extension. It is Prosser's hide set algorithm rather than an expansion depth counter, so mutually recursive macros come out right. Both of the standard's own examples from 6.10.4.5 are tests.
- Hide sets are interned, so a token carries a four byte index rather than a set, and the same set produced by the same nest of headers is stored once.
- The release workflow publishes the whole workspace to crates.io after the binaries have built on every host, so a tag produces both the archives and the registry upload. It does not run for a manual dry run, because an upload to crates.io cannot be taken back.
- Release notes now come from the changelog section for the tag rather than from a list of commit subjects, with GitHub's generated list of merged pull requests appended after it.
- Every crate carries the README, so the crates.io page for `rucc-lex` says what `rucc-lex` is instead of being blank.
- Translation phase 4 in `rucc-pp`: `Preprocessor::run` walks a file, recognises directives, and returns the expanded token stream. `#define` and `#undef`, the full conditional family including `#elifdef` and `#elifndef`, `#error` and `#warning`, `#line`, `#pragma` and `_Pragma`.
- The source map in `rucc-diag`: every file of a translation unit gets a range of one flat coordinate space, so a span stays two integers however deep the header nest goes, and a byte offset resolves back to a file, a line and a column. The line table for a file is built the first time something asks about that file, because most files in a build are never the subject of a diagnostic. It records what included what, so the "in file included from" block of a diagnostic is available long after preprocessing has finished.
- The `#if` expression evaluator: integer and character constants, every operator C allows there, `defined` in both spellings, and the rule that a surviving identifier is zero. Short circuiting is real rather than an optimisation, so `#if defined(X) && 1/X` and `#if 1 ? 2 : 1/0` are both legal, and a skipped region is read for nesting only, so a header may guard prose or a broken directive behind `#if 0`.

### Known limits

There is no `#include` yet. `#include`, `#include_next` and `#embed` are recognised and refused with a diagnostic rather than silently ignored, because including a file needs a source map and a file system abstraction that `rucc-session` does not have. `#line` is recorded and not applied for the same reason. Those, the header cache and the predefined macro set are the next piece of M1.

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
