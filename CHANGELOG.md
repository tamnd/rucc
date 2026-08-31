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
- `#include` and `#include_next`, against a search path that follows GCC's order: `-I` in command line order, then `-iquote` for the quoted form only, then `-isystem`, then the configured system directories, with `-idirafter` last. A quoted include looks next to the file that wrote it first, and `#include_next` continues from the directory after the one the current file came from, which is what a wrapper header around a system header of the same name needs. The computed form, `#include MACRO`, is expanded and then read as a header name. A header that is not there reports every directory that was searched, in the order they were searched.
- A file system abstraction in `rucc-session`, so the preprocessor reads headers through a trait rather than through `std::fs`. `MemoryFileSystem` is the in-memory implementation, which is what the preprocessor tests run against and what an embedder gets to plug into.
- `#pragma once` and the multiple include optimization. A header wrapped in the ordinary `#ifndef NAME` guard is recognised as wrapped, and once `NAME` is defined the file is not opened again rather than being read and thrown away. Both spellings of the guard are recognised, `#ifndef NAME` and `#if !defined NAME`, and a token outside the guard is enough to disqualify a file, because such a file really does produce something on a second read.
- The GNU compatibility matrix in `rucc-gnu`. `features.toml` next to the crate is the source of truth for what the compiler claims to support, a build script turns it into the table the compiler reads, and a row marked implemented with no test named against it fails the build. Only an implemented row answers yes, because a header that gets a yes and then fails to compile is far harder to diagnose than one that takes its fallback path.
- The `__has_*` family: `__has_include`, `__has_include_next`, `__has_attribute`, `__has_c_attribute`, `__has_builtin`, `__has_feature` and `__has_extension`. The two include operators ask the search path exactly what the directive on the same line would ask it, and their operand is resolved before macro expansion, so `__has_include(<linux/version.h>)` is not affected by `linux` being a predefined macro. The rest are resolved after expansion, which is what GCC does, and answer out of the matrix. `defined(__has_include)` is true, which is the shape every header that uses them is written in.

- The predefined macro set, generated from `TargetInfo` rather than hardcoded: the `__SIZEOF_*` family, `__CHAR_BIT__`, the limits, `__BYTE_ORDER__`, the exact width and fast integer families, `__SIZE_TYPE__` and its relatives, the `__FLT_*`, `__DBL_*` and `__LDBL_*` characteristics, the architecture and operating system macros, `__LP64__`, `__OPTIMIZE__` and `__NO_INLINE__`, and `__STDC_VERSION__` per the dialect. It arrives as two synthetic files, `<built-in>` and `<command-line>`, which are the names GCC uses and the names a diagnostic about a predefined macro now points at. `-D` and `-U` go into the second one, in that order, because `-U` beats `-D` whichever side of it the `-D` was written on.
- `__GNUC__` is defined, and the version claimed is a knob rather than a constant. It starts at 4.2.1, which is the version every real header set is known to cope with from a compiler that is not GCC, and it goes up as the matrix in `rucc-gnu` fills in. That is the order `spec/04-driver-and-cli.md` section 4.5 asks for: claiming too high a version means headers reach for extensions that are not there.
- `__DATE__` and `__TIME__`, fixed for the whole translation unit as the standard requires, and honouring `SOURCE_DATE_EPOCH` so that a build that asked to be reproducible is.

### Known limits

`__FILE__`, `__LINE__`, `__COUNTER__` and `__INCLUDE_LEVEL__` are not defined yet. They are the predefined macros whose value depends on where they appear rather than on the target, so they need the expander to ask rather than to substitute.

Almost every row of the matrix says unimplemented, so `__has_attribute` and `__has_builtin` answer no for almost everything. That is not a gap in the operators, it is the state of the compiler stated honestly. `__has_embed` waits for `#embed`.

`#embed` is recognised and refused with a diagnostic rather than silently ignored, because it needs the parser. `#line` is recorded and not applied. A file is identified by the path it was found at rather than by device and inode, so two names for one file are two files here. Those, the header cache and the predefined macro set are the next piece of M1.

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
