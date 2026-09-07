# Link-time libc: stubs, symbol versions, and import libraries

The insight that makes self-contained cross compilation possible, in one sentence: **at link time a shared library is only a list of symbol names, their types, and their versions, and the code is irrelevant.** So the linker's input can be synthesized from a description, and the description is small.

This is the technique `zig cc` is built on and it is the technique rucc adopts. This document specifies it per libc, with the failure modes that come from getting the description slightly wrong.

## 9.1 The general mechanism

For a target whose libc we ship:

1. A **description** of the libc's exported interface: symbol name, kind (function, object), size for objects, symbol version, and the architecture/version combinations in which it exists.
2. A **generator** that turns the description plus a tuple into a stub shared object, a valid ELF `.so` with the right `SONAME`, the right `DT_NEEDED` chain, the right `.dynsym`, the right `.gnu.version_d` version definitions, and no code.
3. The linker links against the stub. At run time the program loads the *real* libc, and because the names and versions match, it resolves.

The stub is generated on demand from the tuple, cached (document 13), and is a few hundred kilobytes rather than the megabytes a real libc would be. One description serves every architecture, because the per-architecture differences are in the description's coverage bits, not in separate files.

**What the stub must get exactly right**, because each of these is a distinct failure mode:

| property | if wrong |
|---|---|
| symbol present | link error, which is loud and therefore fine |
| symbol absent that should be | link succeeds against a symbol that will not resolve at run time |
| symbol *version* | resolves to the wrong implementation, or fails at load with "version `GLIBC_2.34` not found" |
| **object symbol size** | copy relocations copy the wrong number of bytes; silent corruption |
| symbol type (func vs object) | wrong relocation form chosen; PLT/GOT confusion |
| `SONAME` | the wrong library is loaded, or none |
| weak vs global | a weak-linked optional symbol becomes mandatory |

Rows 3 and 4 are the silent ones and they are why document 08.4's execution check exists.

## 9.2 glibc

The hard one, and the one that matters most, because it is what almost every Linux binary in production links against.

**The description is `abilist`.** glibc's source tree carries, per architecture, a set of `.abilist` files: one line per exported symbol giving the version node it was added in, its type and its size. This is a machine-readable, upstream-maintained, complete description of glibc's ABI per architecture per release. It is the artifact that makes this feasible. Zig compresses the union of all of them across architectures and versions into a single binary blob of a few hundred kilobytes; we do the same, and the format is ours.

**Symbol versioning is the part with no shortcut.** glibc exports multiple implementations of the same name under different version nodes, `memcpy@GLIBC_2.2.5` and `memcpy@GLIBC_2.14`, `pthread_create@GLIBC_2.2.5` and `@GLIBC_2.34` after the libpthread merge. A link picks the *highest version node not exceeding the target's glibc version*, which is why document 03's tuple carries `env_version` and why `x86_64-linux-gnu.2.28` is a different target from `x86_64-linux-gnu.2.39`. Getting this right is what "build on new, run on old" means, and it is the single most valuable property of the whole scheme: it is why zig users cross compile from a modern machine to a decade-old CentOS and it works.

The stub must therefore emit real `.gnu.version_d` records forming the version-node chain, not just versioned symbol names.

**The pieces that are not the stub**, and are genuine object files that must be present:

- `Scrt1.o`, `crt1.o`, `crti.o`, `crtn.o`, the start files. Real code, per architecture, small. Either built from glibc source per architecture at our build time, or written by us. Document 10 owns this decision.
- `libc_nonshared.a`, a static archive of the handful of symbols glibc deliberately does not export from the shared object (`__libc_csu_init` historically, `atexit`, `stat`/`fstat` inline wrappers on older versions, `__stack_chk_fail_local`). It is not optional and it cannot be stubbed, because its contents are linked *into* the program.
- `ld-linux-*.so.N` as the interpreter path, a string in `PT_INTERP`, per architecture, and getting it wrong produces "No such file or directory" when running a binary that exists.

**The `_STAT_VER` lesson**, document 01.2. `stat` historically went through `__xstat` with a version constant that differs per architecture and glibc version. Zig's stubs and headers disagreed about it and the result compiled, linked, and failed only on old glibc. The general form: **any symbol whose correct use depends on a constant defined in the headers must be validated by the headers and the stub together, not separately.** The check is document 08.4's execution test against a real image of the oldest supported glibc, and there is no static substitute for it.

**Version floor.** We support glibc from some minimum, 2.17 is the practical floor (CentOS 7 era) and 2.28 the comfortable one, to current (2.43, document 01.6). The floor is a published number, not an accident, and each supported version is a row in document 14's execution matrix, not a claim.

## 9.3 musl

The easy one, and therefore the first one, per document 02.6's ordering.

Single version node, no symbol versioning, one `libc.a` that is both the static and the shared implementation, permissively licensed, and small enough to ship or build per architecture outright. Static linking is the normal case and produces a genuinely dependency-free binary.

Why it is the right first target for the whole sysroot pipeline: it exercises the header tree, the search paths, the start files, the runtime library and the link line **without** the two hardest pieces, stub generation and version nodes. If musl cross compilation works end to end and rung 1 passes under it, the pipeline is right and only the glibc-specific parts remain.

The one musl-specific care: musl's headers are strict about feature-test macros and deliberately lack many glibc extensions, so the rung 0/1 corpus will surface portability assumptions in the *test programs* rather than in the compiler. That is a useful filter and not a problem.

## 9.4 mingw-w64

Windows without MSVC. Redistributable, actively maintained, and the reason document 02.5 can decline the MSVC dialect without declining Windows.

**Import libraries instead of stubs.** A Windows program links against `.a`/`.lib` import libraries describing DLL exports. mingw-w64 ships `.def` files, plain text lists of exported names, ordinals and stdcall decorations, for the system DLLs, and generating import libraries from `.def` files is a mechanical transformation that produces small COFF archives. That is the direct analogue of §9.1 and it is easier, because there is no versioning.

**What must be right:** the i386 name decoration (`_f@8` for `__stdcall`), ordinal-only exports, and `DATA` exports, which need an indirection through `__imp__symbol` rather than a jump thunk. Getting the `DATA` case wrong produces a program that links and dereferences a thunk as data.

**mingw's own runtime.** `libmingwex.a` supplies the C functions msvcrt lacks, `libmsvcrt.a` is the import library for the CRT, and there are start files (`crt2.o`, `crtbegin.o`, `crtend.o`, `dllcrt2.o` for DLLs). These are real code and we either ship the built archives or build them. Shipping them is lawful and is the simpler answer.

**The CRT choice.** `msvcrt.dll` (ancient, present everywhere, weak `printf`) versus `ucrt` (modern, correct, present since Windows 10). Default to UCRT, matching current mingw-w64 and current zig, and spell the alternative in the environment field.

## 9.5 The BSDs

FreeBSD, NetBSD, OpenBSD and DragonFly are ELF, permissively licensed, and their headers and libc symbol lists are redistributable. Mechanically this is the musl case with a larger interface and per-OS-version differences.

Two divergences worth knowing before starting:

- **OpenBSD deliberately breaks ABI stability** between releases and requires syscalls to originate from libc, so a statically linked or direct-syscall binary is rejected by the kernel. This makes "build on new, run on old" false by design on OpenBSD, and the target's support statement must say so rather than implying glibc-like behaviour.
- **FreeBSD's version macro** (`__FreeBSD_version`) and its symbol versioning (`FBSD_1.x` nodes) are real, so FreeBSD is closer to the glibc case than to musl. The tuple's `os_version` field carries it.

## 9.6 Android / Bionic

Worth a section because it is a large real user population and because it is the case that is *easiest* mechanically. The NDK already provides exactly what this document describes, per-API-level stub libraries generated from an interface description, plus headers, and its licence permits redistribution. The work is consuming a description we do not have to invent, and the API level is the tuple's `env_version`.

## 9.7 Darwin `.tbd`

Apple's text-based stub format is a YAML/JSON description of a dylib's exports, and the SDK ships `.tbd` files instead of dylibs. This is §9.1's technique adopted by the platform vendor, which is a pleasant confirmation of the approach and, per document 08.6, one we consume rather than generate, the `.tbd` files come from the SDK, under the SDK's licence.

Linking against a `.tbd` requires the linker to accept it; `ld64` and `lld` do.

## 9.8 The generator, as a component

One crate, at rank 0 or 1 of the layer rule, with no dependency on the codegen crates. Inputs: a `TargetTuple` and the compressed description blob. Outputs: stub `.so` files, import libraries, and a manifest of what was produced with hashes for document 02 claim 5.

Its correctness properties, testable without a target machine:

- Determinism: same tuple, byte-identical stub, on every host. This is a direct component of claim 5 and it is easy to violate with a hash-map iteration order.
- Round trip: the generated stub, read back with `readelf --dyn-syms -V`, reproduces the description exactly. This catches a large class of encoding bugs at zero cost.
- Comparison: for the architectures where we can obtain a real distribution `libc.so`, diff our stub's dynamic symbol table and version definitions against the real one, and require the real one's set to be a superset with matching versions and sizes for the intersection. **This is the single highest-value test in this document**, it validates the description against reality, per architecture, without executing anything.

## 9.9 What is not stubbed

`libm`, `libpthread`, `libdl`, `librt` and `libutil` are, on modern glibc (2.34+), all merged into `libc.so.6` with the separate files kept as empty compatibility stubs. We generate those empty stubs too, because build systems pass `-lm` and `-lpthread` unconditionally and a missing file is a link error. On musl they never existed separately and the same trick applies.

`libgcc_s.so` / the unwinder is not a libc question and document 10 owns it. `libatomic` is ours to provide, not the system's, on targets where wide atomics are not lock-free.
