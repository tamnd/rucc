# The driver and the command line

The driver is the compatibility surface. Nobody types `rucc` directly at first; `configure`, `cmake`, `meson` and `Kbuild` type it, and they type it the way they type `gcc`. Every hour spent on flag compatibility saves a day of debugging someone else's build system.

## 4.1 The compatibility contract

**We are GCC, not Clang, where they differ.** `CC=rucc ./configure` must behave as if `CC=gcc`. Where Clang has added a better spelling for something, we accept both and document GCC's as canonical.

Three rules that sound minor and are not:

**Unknown `-W` flags warn, they do not fail.** Autoconf probes for warning flags by trying them. A compiler that errors on `-Wno-format-truncation` fails configure scripts written for GCC 8. Similarly, `-Wno-<anything>` is always accepted silently, matching GCC's rule that unknown *negative* warning flags are only diagnosed if some other error occurs.

**Unknown `-f` flags that we do not implement are accepted and ignored with a note under `-v`, not rejected**, when they are known-safe no-ops (`-fno-ident`, `-funit-at-a-time`). Flags that change semantics and that we do not implement are hard errors, because silently ignoring `-fno-strict-aliasing` is a miscompilation waiting to happen. Document 13 keeps the list of which is which; the default for an unrecognized `-f` flag is a hard error, and moving one to the ignore list is a deliberate act.

**Exit codes, output file naming, and stderr formatting match GCC.** `-o` semantics, the `.o` next to the source when `-c` without `-o`, `a.out` by default, one diagnostic per line with `file:line:col: severity: message`.

## 4.2 Phases

Each input file gets a phase sequence derived from its extension and the mode flags. The recognized extensions and their entry points:

| Extension | Meaning | Entry phase |
|---|---|---|
| `.c` | C source | preprocess |
| `.i` | preprocessed C | parse |
| `.ir` | this compiler's IR, in the text its printer writes | read the IR |
| `.h` | header, only with `-x c-header` or explicit | preprocess |
| `.s` | assembly | assemble |
| `.S` `.sx` | assembly needing preprocessing | preprocess then assemble |
| `.o` `.obj` | object | link |
| `.a` `.lib` | archive | link |
| `.so` `.dylib` `.dll` | shared library | link |

`.ir` is not a GCC input kind, because GCC has no textual IR. It is here because the IR's printer and its parser are a pair, and a pair is only known to agree if something reads back what was written, so `rucc --emit=ir a.c -o a.ir` followed by `rucc --emit=ir a.ir -o b.ir` is the round trip of document 08 stated as two files a byte comparison has an opinion about, over whatever code is at hand rather than over the modules a test happens to build. What comes in this way is verified the way what the walk builds is, since a module a person edited has not been through the verifier. An input file whose output would have the name it has itself is refused rather than written over.

`-x <lang>` overrides extension detection for subsequent inputs, with `-x none` reverting. `-E` stops after preprocessing, `-S` after code generation, `-c` after assembly, and no flag runs through linking.

`-###` prints the phase plan and the exact linker invocation without executing anything, and `-v` prints it while executing. Both are load-bearing for debugging other people's builds. The phase plan half of both landed in M0 with the phase graph, because a plan that is pure data is the thing that makes them possible and it costs nothing to print it; the linker invocation half arrives in M3 when there is one.

`@file` response files are expanded recursively, with the shell-like quoting rules GCC uses, before any other parsing. Windows builds need this; so do very large link lines.

## 4.3 Target selection

`--target=<triple>` selects the target. The triple is parsed into a `TargetInfo` in `rucc-target`, and *nothing downstream ever asks about the host*. Recognized triples at 1.0:

```
x86_64-unknown-linux-gnu      x86_64-unknown-linux-musl
aarch64-unknown-linux-gnu     aarch64-unknown-linux-musl
riscv64-unknown-linux-gnu     riscv64-unknown-linux-musl
x86_64-apple-darwin           aarch64-apple-darwin
x86_64-pc-windows-msvc        aarch64-pc-windows-msvc
x86_64-unknown-none           aarch64-unknown-none     riscv64-unknown-none
```

The `-none` triples are the freestanding ones and are what the kernel build uses. They imply `-ffreestanding`, no default libraries, and no assumptions about libc.

`TargetInfo` is a plain data structure, not a trait object, containing: pointer and integer widths, `char` signedness, `long double` representation, endianness, alignment rules, the object format, the ABI variant, the default `-fPIC` setting, the assembler dialect, the register file, and the predefined macro set. Adding a target means adding a `TargetInfo` and a lowering rule set, and nothing else. Document 10 and document 12 own the contents.

GCC-style flags are accepted as aliases where they are unambiguous: `-m32` and `-m64` adjust the triple, `-march=` and `-mtune=` and `-mcpu=` select the subtarget, `-mabi=` selects the ABI variant. Target-specific `-m` flags that gate instruction set extensions (`-msse4.2`, `-mavx2`, `-march=armv8.2-a+crypto`) set feature bits consulted by instruction selection and by the `__builtin_cpu_supports` and predefined-macro machinery.

## 4.4 Search paths

The include search order matches GCC exactly, because header shadowing bugs caused by a different order are miserable to diagnose: `-I` directories in command-line order, then `-iquote` for `"..."` includes only, then `-isystem`, then the configured system directories, with `-nostdinc` suppressing the last group and `--sysroot=` prefixing it. `-idirafter`, `-iprefix`, `-iwithprefix` and `-I-` are supported for compatibility. `-MD`, `-MMD`, `-MF`, `-MT`, `-MP` and `-MQ` produce make-format dependency files, and these are required by essentially every build system.

Between `-isystem` and the configured system directories sits one entry that is not a directory at all. A hosted C implementation is two halves: the library ships the headers that declare functions you link against, and the compiler ships the handful whose contents only it can know. `<stdarg.h>` is the target's calling convention, `<limits.h>` and `<float.h>` are the target's types, `<stddef.h>` is the ABI, and no library can write any of them. Those headers are compiled into the binary and appear on the search path under the name `<builtin>`, which is spelled with the angle brackets so that no directory a user can create can shadow them or be shadowed by them, and so that a diagnostic naming `<builtin>/stdarg.h` reads as what it is. Keeping them in the binary rather than on disk means the compiler does not have to find its own installation before it can preprocess a file, which is what lets a single static binary work wherever it is copied. `-nostdinc` drops this entry along with the configured system directories, because the two halves of a hosted implementation are a pair and half a pair is worse than none. The set is `float.h`, `iso646.h`, `limits.h`, `stdalign.h`, `stdarg.h`, `stdbool.h`, `stddef.h`, `stdint.h` and `stdnoreturn.h`. `limits.h` and `stdint.h` chain to the library's with `#include_next` when there is one, because on a hosted system the library's is what the rest of the library was written against.

The other half of the pair is the library's own headers, and "configured" is the wrong word for them here. GCC decides where they are when it is built, which it can do because a GCC is built for the machine it will run on, and this compiler is one binary that runs wherever it is copied, so it asks the machine instead. What that means per platform is one list of candidates each, of which the ones that exist are taken: on Linux, `/usr/local/include` and then the multiarch directory and then `/usr/include`, which is GCC's order and every distribution's layout; on an Apple platform the SDK and nothing else, since there has been no `/usr/include` on a Mac since the command line tools stopped installing one, found from `-isysroot` or `SDKROOT` or by asking `xcrun`; on Windows whatever `INCLUDE` names, because there is no fixed place there and the environment `vcvarsall.bat` sets is what every compiler on that platform reads. Cross compiling to another operating system offers nothing at all without a `--sysroot`, because this machine's `/usr/include` describes this machine's library and handing it to a program being built for somewhere else moves the failure from an `#include` that could not be resolved to a declaration that is quietly wrong.

Library search is `-L` in order, then the target's defaults, with `-B` prefixes consulted for our own auxiliary files. `-nostdlib`, `-nodefaultlibs` and `-nostartfiles` behave as GCC's.

We do **not** implement GCC's `specs` file mechanism. It is a scripting language nobody should have to learn, and the parts of it that build systems actually rely on are the ones above. A `-specs=` flag is a hard error with a message pointing at the equivalent flags.

## 4.5 Predefined macros, and the trap in them

This is the single most consequential compatibility decision in the driver, and document 01 flagged it: the moment `__GNUC__` is defined, glibc's headers, the kernel's headers and every autoconf probe take the GNU path, and we have signed up for whatever those paths use.

The decision: **we define `__GNUC__` and we claim a specific version, and document 13's extension matrix is the list of promises that claim makes.** The alternative, not defining it, means glibc's `<sys/cdefs.h>` treats us as a pre-standard compiler and the result does not compile at all. There is no third option.

The version we claim is a tunable, `-fgnuc-version=`, and the default is the highest one measured to get a real header set through. Claiming too high a version means headers use features we lack; claiming too low means headers take slow or deprecated paths and some projects refuse to build, and below GCC 7 glibc writes `typedef float _Float32;` over a keyword we already have, which stops most of its headers outright. So the default is 7.0.0, and it moves when there is a measurement over glibc, the macOS SDK and the corpora in document 14 saying it can, not when document 13's matrix reaches some line.

We also define `__rucc__` and `__rucc_version__` so code can detect us specifically, and we do **not** define `__clang__`.

The rest of the predefined set is generated from `TargetInfo`: the `__SIZEOF_*__` family, `__CHAR_BIT__`, the limits macros, `__BYTE_ORDER__`, `__INT*_TYPE__` and the exact-width families, `__SIZE_TYPE__`, `__PTRDIFF_TYPE__`, `__WCHAR_TYPE__`, `__INTPTR_TYPE__`, the `__*_MAX__` family, `__FLT_*__` and `__DBL_*__` and `__LDBL_*__`, the architecture macros (`__x86_64__`, `__aarch64__`, `__riscv`, and their `_LP64` companions), the OS macros (`__linux__`, `__APPLE__`, `_WIN32`), `__ELF__` where applicable, feature-test results from `-m` flags, `__OPTIMIZE__` and `__OPTIMIZE_SIZE__`, `__NO_INLINE__`, `__PIC__` and `__PIE__`, and `__STDC_VERSION__` per the dialect.

`rucc -dM -E -` prints the whole set, and a CI job diffs it against GCC's on the same triple. Divergences are either fixed or recorded with a reason in document 13.

## 4.6 Dialect and semantic flags

`-std=` accepts `c89` `c90` `c99` `c11` `c17` `c23` and their `gnu` variants. **The default is `gnu23`**, matching current GCC. `-ansi` is `-std=c89`. `-pedantic` and `-pedantic-errors` diagnose extensions used in a strict mode.

The flags in this group change what the optimizer may assume and are threaded into the IR as module-level or function-level semantic attributes rather than consulted globally, so that LTO across units compiled with different flags remains correct. This is a real hazard: a project that builds one file with `-fno-strict-aliasing` and another without must not have the second file's assumptions applied to the first after inlining.

`-fwrapv` makes signed overflow defined as two's-complement wrapping. Postgres requires it. `-fno-strict-overflow` is the weaker sibling and the kernel requires it. `-ftrapv` traps instead. `-fno-strict-aliasing` disables type-based alias analysis; the kernel requires it and a great deal of real C is quietly wrong without it. `-fno-delete-null-pointer-checks` forbids inferring non-nullness from a dereference; the kernel requires it. `-ffreestanding` removes the assumption that library functions behave as the standard says, which in particular means no recognition of `memcpy` idioms into calls that do not exist. `-fno-builtin` and `-fno-builtin-<name>` do this selectively. `-fexcess-precision=standard|fast` controls x87 intermediate precision; Postgres requires `standard`. `-ffp-contract=off|on|fast` controls fused multiply-add formation. `-frounding-math` and `-ftrapping-math` constrain FP transformations. `-fshort-enums`, `-fsigned-char` and `-funsigned-char` change the type system and therefore the ABI.

`-ffast-math` is implemented as the documented union of its components and each component is individually available, because `-ffast-math` as an opaque blob is how numerical code silently breaks.

## 4.7 Optimization and code generation flags

`-O0` `-O1` `-O2` `-O3` `-Os` `-Oz` `-Og` select the pipelines in document 09. `-Ofast` is accepted and maps to `-O3 -ffast-math`, with a warning, matching GCC's deprecation of it.

Individual `-f<pass>` and `-fno-<pass>` flags exist for every pass, which is required for bisection: when a project miscompiles at `-O2`, the first diagnostic step is to find the pass. `-fdisable-<pass>[=<functions>]` and `-fenable-<pass>[=<functions>]` take a pass and, optionally, the functions the answer is about, and `-fpass-fuel=<pass>=<n>` runs a pass for exactly *n* transformations and then stops, which is how a miscompiling transformation is bisected to a single site. `-fpass-fuel-global=<n>` is the same limit across every pass, which is the search that says which pass to run the other one on. Document 15 depends on all of them.

A gate covers the functions it names and nothing else, and the last gate that covers a function is the one that decides for it, so everything a gate does not name keeps the answer the level already gave. That is what makes `-fenable-<pass>=3` mean "also run it there" rather than "run it only there", and it is what makes which pass and which function two independent bisections rather than one search over both at once. A function is named either by the name it has in the source, which is what `-fopt-info` prints, or by the position it has in the module counting from zero, which is what a script has without having read the file. A pass a gate enables that the level did not choose joins the pipeline, so a gate reaches a pass at `-O0` as well, which is where a bisection would rather start. `rucc --print-pipeline` says which passes a gate touched and what it said about them.

A pass name is a name the compiler holds rather than a string the driver keeps a list of, so `-f<pass>` and `-fno-<pass>` are read by asking the optimizer whether it has a pass by that name, and a `-f` flag naming something that is neither a pass nor a flag the rest of the compiler answers to is refused as an unknown option rather than ignored. The flags are read in the order they were typed and applied to the level's pipeline in that order, so the last spelling of a name decides, and turning on a pass the level already runs leaves it where the level put it rather than moving it to the end. `rucc --print-pipeline` prints what that arithmetic came to, which is the thing to look at before concluding that a `-f` flag did not take.

`-fdump-ir=all`, `-fdump-ir=before-<pass>` and `-fdump-ir=after-<pass>` write the IR out around a pass, one file per dump, named for the input file and then the pass, so that a bisection that has found the pass has somewhere to look next. The pass a dump names is checked while the command line is read rather than when the dump is taken, because a misspelled pass name discovered after the compilation has finished is discovered too late to be any use. A dump that could not be written is an error and not a warning, for the reason `-Zrule-coverage=` gives in section 4.11.

Code generation flags: `-fPIC`, `-fPIE`, `-fno-plt`, `-fno-omit-frame-pointer`, `-fomit-frame-pointer`, `-fstack-protector` and its `-strong`/`-all` variants, `-fstack-clash-protection`, `-fcf-protection=`, `-fno-common`, `-ffunction-sections`, `-fdata-sections`, `-fvisibility=`, `-mcmodel=`, `-mno-red-zone`, `-fpatchable-function-entry=`, `-mfentry`, `-pg`. The last several are kernel requirements and are specified in document 13.

`-fsanitize=address,undefined,thread,memory` and the `-fsanitize=kernel-*` variants are codegen features specified in document 12. `-fsanitize=undefined` is the one we implement first because it is the cheapest and it is what finds bugs in the corpus.

`-fsafety=off|detect|enforce|kernel` selects a tier of the memory safety monitor, which is specified in `spec/safe-memory/` and whose flag surface is section 15.4 of that document set. It is a tier rather than a plane at a time because the tiers are the product and the modifiers are how somebody who has read the threat model departs from one. The default is `off`, and a command line without it is compiled by the pipeline it was compiled by before the monitor existed, which is what lets the feature be built in the open. Where a tier covers the same ground as one of the sanitizers above it supersedes it, and asking for both is an error rather than instrumenting twice.

`-flto` and `-flto=thin` are specified in document 09; `-fprofile-generate` and `-fprofile-use` in the same document.

## 4.8 Debug info

`-g`, `-g0` through `-g3`, `-gdwarf-4`, `-gdwarf-5` (the default), `-gsplit-dwarf`, `-fdebug-prefix-map=`, `-ffile-prefix-map=`, `-gz` for compressed sections. `-fno-eliminate-unused-debug-types` and friends are accepted. Document 11 owns the emission.

`-g` at `-O0` must produce debug info good enough that every local variable is inspectable at every point in its scope, because that is the actual reason people use `-O0`. `-g` at `-O2` produces best-effort location lists and is honest in document 16 about what fraction of variables remain inspectable.

## 4.9 Linking

We do not have our own linker before 1.0. The driver locates one in this order: an explicit `-fuse-ld=<name>`, then `mold`, then `lld`, then the platform default (`ld` on Linux, `ld` on macOS, `link.exe` under the MSVC triples). `mold` first because it is dramatically faster and because a compiler that is 2x faster than Clang while the link takes twelve seconds has not helped anybody.

The driver constructs the link line with the right startup files, the right default libraries, the right dynamic linker path and the right `--sysroot`, per target. This is unglamorous and it is where cross-compilation actually breaks; document 14's corpus catches it.

`-Wl,` passes through, `-Xlinker` passes through, `-Wa,` reaches the assembler, `-Wp,` reaches the preprocessor.

The linker is invoked directly rather than through the system `cc`, which is what makes the line above ours to get right rather than something a driver we did not write decides. Two consequences. The first is that the startup files and the library directories are looked for on the machine rather than configured, because a compiler that has to be told where `crt1.o` is on each installation is a compiler nobody can install. The second is that a `-l` is an input and not a setting: a library written between two files on the command line resolves for the file before it and not for the file after, so the driver keeps objects and libraries in one list in the order they were typed.

`crtbegin.o`, `crtend.o` and the runtime libraries are on the line, found on the machine the same way the startup files and the library directories are. The order is the C library, then our `librucc_builtins.a`, then the machine's `libgcc`, and where two of them define the same name the first of them is the one that answers. That is deliberate in both places it happens. The block routines are in the C library on a hosted target and in ours only for a freestanding one, and glibc writes them in assembly per microarchitecture while ours is a word at a time loop, so a link that took ours would be slower at the routine every program reaches. The wide arithmetic is in ours and in `libgcc` both and the two are ABI-identical on purpose, so which one answers is not a correctness question, and ours goes first because it is ours. A static link puts the whole set inside `--start-group`, because `libc.a` refers to the unwinder and the unwinder refers back into `libc.a`, and a linker walking the list once resolves whichever it reaches first and calls the other undefined. A dynamic link needs no group and asks for `libgcc_s` `--as-needed`, so a program that never unwinds does not acquire a dependency on it.

An internal linker remains a post-1.0 possibility, and document 11 says what would make it worth doing.

## 4.10 Driver-level diagnostics for build systems

Two features that exist purely to make other people's builds debuggable, both implemented in M2.

`rucc --print-config` dumps the fully resolved `Options` and `TargetInfo` as JSON: every search path, every predefined macro, every enabled pass, the chosen linker. When a build behaves differently under `rucc` than under `gcc`, this is the first thing to diff.

`rucc --print-pipeline` dumps the passes the level and the `-f` flags between them came to, in the order they will run, one per line with the description the pass gives of itself. It needs no input file, for the same reason `--print-config` does not: the question it answers is about the flags and not about a translation unit.

`RUCC_LOG=` provides structured tracing at phase and pass granularity via `tracing`, with timing. `-ftime-report` prints the per-phase and per-pass time breakdown in GCC's format, and it is how axis 3 regressions get attributed.

## 4.11 The unstable options

GCC has no `-Z`, which is what makes it the right prefix for the flags that are ours: a measurement or a debugging aid that no build system should ever be passing, spelled so that it cannot collide with a flag we are copying and so that a reader of a command line can tell at a glance which parts of it are compatibility and which are ours. Nothing under `-Z` is promised. One of them may change its spelling or go away in a patch release, none of them appears in `--help`, and every one of them is listed here.

`-Zrule-coverage=FILE` writes which lowering rules the run fired. The file holds one line per rule in the target's rule file, in the order that file writes them, each saying `fired` or `unused` and then the rule file, the line and the pattern. The whole rule set is listed rather than only the part that fired, so that one of these files says what there was to cover as well as what was covered, and one file is written for the whole command line rather than one per input, because the question is what this run of the compiler reached. What reads them is the harness in `tamnd/rucc-compat`, which unions them over a corpus and reports the rules the corpus never reaches; document 20 section 20.9 is the design and what it is for. It changes nothing about the code that comes out, and a file it could not write is an error rather than a warning, because a measurement that quietly did not happen is worse than one that stopped.

`-Zverify-each` runs the IR verifier after every pass that changed anything, rather than only where the pipeline would have run it. It is off by default in a release build and on by default in a debug one, because the cost is a walk of the function per pass and the thing it buys is that a pass which breaks the IR is reported by name instead of being found later as a strange failure in the back end. A verifier complaint under it is an internal compiler error and names the pass, which is the whole point of the flag.
