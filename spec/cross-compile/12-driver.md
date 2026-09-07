# The driver

The compiler is judged through the driver. A user cross compiling for the first time interacts with a flag surface, a set of `-print-` queries and a diagnostic, and if those are wrong the quality of the code generator is not reachable. This document specifies that surface.

The governing principle, from document 00's settled decisions: **one binary, all targets, no build-time configuration.** Every default is computed from the tuple at run time. Nothing is baked in at build time except the tuple *table*.

## 12.1 Selecting a target

Three ways, in precedence order:

1. `--target=<tuple>` / `-target <tuple>`. The clang spelling, which is what everything scripts against.
2. **The invocation name.** `aarch64-linux-gnu-rucc` sets the target to `aarch64-linux-gnu`. This is the GCC cross-toolchain convention and supporting it is what makes rucc a drop-in for build systems that compute `$(CROSS_COMPILE)gcc`. Cheap: parse `argv[0]`, strip a known suffix, try to parse the prefix as a tuple, ignore it if it does not parse. Also implies the whole symlink family, `<triple>-rucc`, `<triple>-cc`, and per document 12.6 the `gcc`/`cc` aliases.
3. The host, by default.

`-march=`, `-mcpu=` and `-mtune=` refine features within the selected target and never change the tuple; document 03's rule is that anything changing how a function is called or a struct is laid out is in the tuple, and everything else is a feature.

## 12.2 The `-print-` surface

These are the interface for build systems and the interface for debugging a broken cross build, so they are specified rather than left to accumulate:

| query | returns |
|---|---|
| `-print-target-triple` | the canonical tuple as rucc spells it |
| `-print-llvm-triple` | the LLVM spelling, for interoperating with LLVM tools |
| `-print-targets` | every tuple rucc knows, one per line, with its tier |
| `-print-search-dirs` | the header and library directories actually in effect |
| `-print-sysroot` | the sysroot in effect, ours or the user's |
| `-print-sysroot-provenance` | **every non-rucc input: name, source, hash, licence** (document 02 claim 5) |
| `-print-file-name=<f>` | the resolved absolute path, or empty |
| `-print-prog-name=<p>` | the linker or tool actually chosen (document 11.6) |
| `-print-multi-lib`, `-print-multi-directory` | the multilib set, for build systems that ask |
| `-dumpmachine` | GCC's spelling of `-print-target-triple` |
| `-dumpversion` | GCC's version protocol |
| `-print-libgcc-file-name` | our builtins archive, because configure scripts ask |

Two spellings for the triple rather than one, per document 03.4, because a tuple is not a canonical object and the two consumers want different strings. Pretending there is one right answer is how the `arm64`/`aarch64` and `thumbv7a-vita-eabihf` classes of bug happen.

`-print-targets` matters more than it looks: it is what makes document 02 claim 1's parity script possible, and it is the machine-readable form of the support table.

## 12.3 The GCC compatibility surface

rucc is invoked by build systems that were written for GCC, so the flags that must work are the ones autoconf, CMake, meson and kernel Makefiles emit, not the ones a person types. The cross-relevant set beyond what rucc already has:

`--sysroot`, `-isysroot`, `-isystem`, `-iquote`, `-idirafter`, `-nostdinc`, `-nostdlibinc`, `-nobuiltininc`; `-B<prefix>`; `-static`, `-static-pie`, `-shared`, `-rdynamic`; `-mfloat-abi=`, `-mabi=`, `-mcmodel=`, `-mno-red-zone`, `-mgeneral-regs-only`; `-m32`/`-m64`/`-mx32`; `--print-multi-*`; `-Wl,`, `-Xlinker`, `-Wa,` (accepted and mapped, since we have no external assembler), `-specs=` (recognized and diagnosed as unsupported rather than ignored).

**Diagnose rather than ignore.** A flag we do not implement that changes semantics must be an error; a flag that is a no-op for us must be silently accepted. The distinction is the difference between "rucc does not support that" and "rucc miscompiled my kernel". `-specs=` is the canonical example: ignoring it produces a build that looks fine.

**`-m32` is a target change**, not a feature, and it maps to a different tuple. Deriving the 32-bit counterpart of a tuple is a table, and on targets where there is none it is an error naming the reason.

## 12.4 Multilib

Where one target directory holds several variants, `lib` and `lib32`, or ARM's soft/softfp/hard trees, the driver must pick a subdirectory from the flags. Clang 22's `multilib.yaml` (document 01.3) is the modern form: a declarative file mapping flag sets to directories, replacing hard-coded logic.

**Position: adopt the `multilib.yaml` shape.** For sysroots we generate ourselves the question does not arise, because we lay them out one variant per tuple and there is nothing to select. It arises for *user-provided* sysroots, which are exactly the ones with distribution layouts, and adopting an existing declarative format there means users can reuse files they already have. Support the format for user sysroots; do not use it internally.

## 12.5 Configuration files

`rucc.cfg` / `@file`, in the clang style: a file of flags, findable next to the binary and by `--config`, applied before the command line. This is how a user pins a target, a sysroot path and a set of flags without wrapping the compiler in a script, and it is how a project checks its cross configuration into version control.

Constrained deliberately: no conditionals, no variable expansion beyond `<CFGDIR>`, no includes more than one level deep. A configuration language in a compiler driver is a reliability hazard and there is a long history of it.

Environment variables are read from a **short, documented list** (`SOURCE_DATE_EPOCH`, `RUCC_TARGET`, `RUCC_SYSROOT`, `RUCC_CACHE_DIR`) and nothing else. Not `CPATH`, not `LIBRARY_PATH`, not `C_INCLUDE_PATH` when cross compiling, those are host configuration and document 08.5 excludes them. Native compilation honours them for compatibility.

## 12.6 The drop-in question

A large fraction of the practical value here is being usable without changing a build system. Three levels:

1. **`rucc --target=` works**, needs the user to edit `CC`.
2. **`<triple>-rucc` works**, needs the user to set `CROSS_COMPILE`, which build systems already have a slot for. §12.1.
3. **A generated toolchain directory** containing `<triple>-gcc`, `<triple>-cc`, `<triple>-ld`, `<triple>-ar`, `<triple>-ranlib`, `<triple>-strip`, `<triple>-nm`, `<triple>-objcopy` as symlinks or shims, which is what a `./configure` script actually looks for. `rucc --emit-toolchain-dir=<path> --target=<t>` produces it.

Level 3 is the one that makes an unmodified autotools project cross compile, and it is a small amount of code with a disproportionate effect. The binutils shims are the awkward part, we are not writing an `objcopy`, so they are thin wrappers over `llvm-*` equivalents when available and an honest error when not, and `--emit-toolchain-dir` reports which of the eight it could actually provide.

**We do not lie about being GCC.** `<triple>-gcc` as a name is a compatibility affordance; `--version` says rucc, and `__GNUC__` is defined because the ecosystem requires it and rucc already makes that choice.

## 12.7 Diagnostics for cross-specific failures

The failures that will actually happen, each with the diagnostic that is required rather than merely nice:

| failure | the diagnostic must say |
|---|---|
| unknown tuple | the closest known tuples, and `-print-targets` |
| known tuple, tier 3 | that it is not supported, what "tier 3" means, and what does work for it |
| Darwin target, no SDK | that the SDK cannot be redistributed, and the two lawful ways to supply one (document 08.6) |
| MSVC target, no SDK | the same, plus the mingw-w64 alternative that needs nothing |
| glibc version below our floor | the floor, and that musl is available |
| feature flag not valid for target | which target it *is* valid for |
| sanitizer on a non-tier-1 target | that the runtime is not built for this target, not a silent no-op (document 10.7) |
| linker unsuitable | which check failed, and the exact command that fixes it |

Every one of these is a first-contact failure. A user whose first cross compilation fails with "cannot find crt1.o" concludes the tool does not work; the same user seeing "no glibc sysroot for `s390x-linux-gnu`; available: musl. Run `rucc --print-targets` for the full list" concludes it does.

## 12.8 What stays out of the driver

The driver resolves flags to a `(TargetTuple, TargetFeatures, Options)` triple and a link argv, and does nothing else. It does not contain per-target `match` arms on `Arch` or `Os`, document 02 claim 3's `cargo xtask layers` check applies to it, and the driver is the crate where that check will most often fire and most often be tempting to suppress.

The data those matches would have contained lives in the target table, which is data at rank 0, read by the driver like everything else.
