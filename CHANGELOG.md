# Changelog

All notable changes are recorded here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses [semantic versioning](https://semver.org/spec/v2.0.0.html) with the caveat in `spec/18-package-layout.md` section 18.6: pre-1.0 versions carry no compatibility promise at all.

## 0.1.1

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
- `-E` runs. The driver reads the file, runs phase 4 over it and writes the result to `-o` or to standard output, which is the first phase this compiler actually performs. The flags that phase reads came with it: `-D` and `-U` in both the joined and the separated spelling, `-I`, `-iquote`, `-isystem`, `-idirafter`, `-P`, `-std=` with every alias GCC takes, `-ansi` and `-ffreestanding`. A diagnostic prints as `file:line:column: severity: message [code]` with the chain of includes that reached it above, and under `-Werror` a warning says error rather than leaving the reader to work out why the build stopped.
- `OsFileSystem`, the implementation of the file system trait that talks to the disk. It lives in the driver, which is the only crate allowed to know the process exists, so a test below the driver still cannot read the machine it runs on by accident.
- The `-E` printer in `rucc-pp`: the expanded token stream written back out as text. Line markers are GCC's, including the `1`, `2` and `3` flags, a gap of up to eight lines is printed as blank lines and anything larger as a marker, and the indentation of the first token on a line is rebuilt from its column. A space goes in wherever two neighbouring tokens would otherwise read back as one token, so `+ +` does not become `++` and a `/` next to a `*` does not open a comment. `-P` turns the markers and the padding off. Output that diffs cleanly against GCC's is the point of all of it, because that diff is the fastest way to find a preprocessor bug.
- The predefined macros whose value is a question rather than a body: `__FILE__`, `__FILE_NAME__`, `__BASE_FILE__`, `__LINE__`, `__INCLUDE_LEVEL__` and `__COUNTER__`. They are answered by the expander out of the source map at the place they are used, which is the place the outermost macro invocation was written rather than the header the macro body lives in. A logging macro defined in a header and used in `main.c` says `main.c` and the line the user wrote, which is the only answer that is any use. `__COUNTER__` counts once per translation unit and once per expansion, so an argument that is used twice still carries one number.

- `#embed`, with `limit`, `offset`, `prefix`, `suffix` and `if_empty`, and the `__STDC_EMBED_*` answers. The resource is looked for on the include path the same way a header is, the parameters are macro expanded, and `limit` is a full constant expression rather than a number because the standard says so and because the `#if` evaluator was already there to do it. `__has_embed` reads the parameters too, so a guard answers the same question the directive would. The 256 possible byte spellings are interned once per directive rather than once per byte, which is what makes a one megabyte resource preprocess in about twenty milliseconds instead of pausing.
- `-dM`, which prints the macro table instead of the preprocessed text. It is the fastest way to diff a predefined set against GCC's, and it found a bug in `__UINT32_C` on its first run.
- `-fgnuc-version=`, so the GCC version claimed is a command line knob and not a rebuild. The differential harness reads the reference compiler's claim and passes it back in, which is how two compilers stop disagreeing merely about which compiler they are.
- `__USER_LABEL_PREFIX__`, which is empty on ELF and an underscore on Mach-O.
- The atomic memory orders and the `__GCC_ATOMIC_*_LOCK_FREE` answers, generated from the target rather than assumed.
- Apple's spelling of the architecture macros, and `wint_t` per target rather than per host.
- `__building_module`, which is always no.
- `cargo xtask bench`, the throughput floor benchmark. Three system headers and an empty main, timed against a reference compiler in the same loop, reported as the median of ten runs with the interquartile range next to it and never as a single number. It says so when the two interquartile ranges overlap, because ten runs that did not separate two distributions have not separated them whatever the medians say, and it compares how much output each compiler produced, because it is easy to be fast at preprocessing by not doing some of it. `--csv` writes the row layout a nightly wants.
- The differential against GCC over glibc and musl headers runs on every commit, and the harness and the corpora live in `tamnd/rucc-compat` so that this repository holds the compiler and nothing else.

### Fixed

- A macro that expands to no tokens now leaves its leading whitespace behind. `#define E` used as `int a E;` preprocesses to `int a ;` in both GCC and clang, and rucc was printing `int a;`. glibc hangs `__THROW` and its relatives off the end of several hundred prototypes per header, so this was most of the difference between the two on the standard header set. The rule is narrower than it sounds: only a vanished macro that had a space in front of it leaves one, and the space is handed to whatever gets rescanned next, so three vanishing macros in a row owe one space between them and not three.
- musl gets its own answer for `int_fast16_t` and `int_fast32_t`, which it defines as `int32_t` where glibc on the same processor uses `long`. GCC ships a `stdatomic.h` written directly out of `__INT_FAST16_TYPE__`, so the old answer made every atomic fast type twice as wide as the reference compiler's. `Target::host()` also stopped describing a compiler built on Alpine as a glibc compiler.
- `__UINT32_C` produced a `UU` suffix, found by diffing `-dM` against GCC.

### Changed

- The differential against glibc and musl headers is a required check rather than an advisory one. It was advisory while the standard header set was still coming out unequal, on the grounds that a gate which is red for a known reason teaches everyone to ignore it. Both libcs now come out at 29 of 29 on that set.

### Known limits

Most rows of the GNU compatibility matrix still say unimplemented, so `__has_attribute` and `__has_builtin` answer no for most things. That is not a gap in the operators, it is the state of the compiler stated honestly.

The `__has_*` operators only answer inside `#if`, and are left alone in ordinary text where GCC and clang both expand them. That is issue #40.

`#line` is recorded and not applied, so `__LINE__` and `__FILE__` say where the text really is rather than where a `#line` asked them to say it is. Applying one means the source map presenting a name and a line other than the real ones, and the line markers the `-E` printer writes would then have to say that name, so the two land together. A file is identified by the path it was found at rather than by device and inode, so two names for one file are two files here.

The lexer still reads a copy of the file rather than a mapping of it, dispatches on characters rather than through a table, and skips whitespace and comment bodies one byte at a time. Those are the rest of M1.

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
