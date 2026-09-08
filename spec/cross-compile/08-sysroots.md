# Sysroots, and why the headers are the hard half

The conventional framing is that cross compilation needs a sysroot, and a sysroot is headers plus libraries. That framing hides the asymmetry this document is about: **the link-time libraries can be synthesized, and the headers cannot.** Document 09 shows that a glibc `.so` can be replaced by a few hundred kilobytes of generated stubs. Nothing equivalent exists for `/usr/include`, because headers are not an interface description, they are program text, full of macros, inline functions, `static_assert`s and conditional compilation on version macros, and they must be *the same text* the target's real libc was built against.

Everything expensive about being self-contained is on the header side.

## 8.1 What rucc does today

`crates/rucc-driver/src/library.rs` models a `Machine { host, sysroot, sdk, include }` and computes `candidates()`, which returns an empty list when cross compiling to a different OS with no sysroot. `link.rs` looks for directories at run time rather than being configured at build time, deliberate, and correct for a compiler that must not bake in the machine it was built on.

Both are right, and both assume the sysroot is somebody else's problem. Document 00's second settled decision reverses that: **the sysroot is the compiler's problem, not the user's.** This document is what that costs.

## 8.2 The four cases, in ascending difficulty

| case | headers | link inputs | licence | status |
|---|---|---|---|---|
| **freestanding** | 9 compiler headers | none | ours | done |
| **musl** | musl's, one copy per arch | stubs or the real static libc | MIT | easy; do first |
| **glibc** | multi-version, per-arch | generated stubs, §9 | LGPL | hard; the main work |
| **mingw-w64** | mingw's + Windows API | generated import libs | permissive + PD-ish | medium |
| **Darwin** | **cannot ship**, §8.6 | `.tbd` from the SDK | **Apple EULA** | fetch-only |
| **MSVC** | **cannot ship** | SDK `.lib` | **non-redistributable** | fetch-only |
| **BSDs** | ship-able (BSD licences) | stubs | permissive | medium |

Two of the seven are legal walls, not technical ones, and document 13 owns them. The rest is engineering.

## 8.3 The multiplication problem, and the fix

Naively, headers are `arch × os × libc × libc-version` directory trees. glibc's headers differ per architecture (`bits/*.h`, `stat.h` layouts, syscall constants) and per version (new symbols, changed macros, `__GLIBC_MINOR__`). Shipping full copies for eight architectures × six supported glibc versions is on the order of several hundred megabytes, and document 13 has a binary-size budget that this destroys.

Zig solved this and the solution is the one to adopt. Two techniques, both from document 01.2:

**Multi-version headers.** One header tree per libc, in which the per-version differences are expressed as `#if __GLIBC_MINOR__ >= n` inside a single file rather than as separate trees. The compiler defines the version macros from the tuple's `env_version` (document 03's field), and one tree serves every version. Zig's `generic-glibc` is exactly this, and the `ziglang/universal-headers` project is the tooling that *derives* such a tree by diffing real header sets, the technique, not just the artifact, is available.

**Per-architecture only where it must be.** Most of glibc's headers are architecture-independent. The `bits/` directory and a handful of others are not. Splitting into `generic` plus `<arch>` cuts the multiplication from a product to a sum.

Applying both: the glibc header payload is roughly *one* generic tree plus eight small per-architecture trees, not forty-eight trees. Document 13 puts the number on it.

**The honest cost.** Producing and maintaining that merged tree is real, ongoing work that scales with glibc releases, not with our target count. It is the single largest maintenance liability in this specification, and document 16 keeps it open with the question "who regenerates the header tree when glibc 2.44 ships, and how is it validated".

## 8.4 Validating a header tree

A merged multi-version header tree can be wrong in a way that produces a program that compiles and misbehaves, the `_STAT_VER` case in document 01.2 is precisely that: the stub and the header disagreed about a versioning constant, and the result linked and then failed at run time on old glibc.

Three checks, all mechanical:

1. **Structural equivalence.** For each supported (arch, glibc version), compile a corpus of `_Static_assert`s over every type the headers define, size, alignment, every member offset, against our merged tree and against the real distribution headers of that version. Any disagreement is a bug in the merge. This reuses document 06.8 mechanism 1 wholesale.
2. **Macro equivalence.** `-dM -E` over each header, both trees, diffed. Catches constants that changed value, which the struct assertions do not see.
3. **Execution.** Rung 0 and rung 1 built against the merged tree and run on a real distribution image of that glibc version, per document 02 claim 4. This is what catches the `_STAT_VER` class, and it is the only check that does.

The first two are cheap enough to run per commit. The third is per release, per (arch, libc version) in tier 1.

## 8.5 The search-path rules

Cross compilation makes header search a target property rather than a machine property. The rule:

1. `-I` in order.
2. The compiler's own headers (`stddef.h`, `stdarg.h`, `stdint.h`, `float.h`, `limits.h`, `stdbool.h`, `stdalign.h`, `stdnoreturn.h`, `iso646.h`, plus the intrinsic headers). **Always present, on every target including freestanding, and never taken from a sysroot.**
3. The target's libc headers: from `--sysroot` if given, otherwise from our bundled tree for that tuple, otherwise, and only when the target is the host, from the host's directories as `library.rs` computes them today.
4. Nothing else. No `/usr/local/include` when cross compiling, ever; it is a host directory and its presence in a cross build is a bug.

`-nostdinc` removes 3, `-nobuiltininc` removes 2, `--sysroot` replaces 3's root, `-isysroot` is the Darwin spelling and applies to 3 only. `-print-search-dirs` and a new `-print-sysroot` report what was chosen, which is document 12's surface.

**The failure this ordering prevents** is host contamination: a cross build that silently picks up a host header, produces something that works on the build machine, and does not work anywhere else. Document 02 claim 5 (byte-identical output across hosts) is the test that catches it, and it catches it *only* because the rule above makes step 3 host-independent when cross compiling.

## 8.6 Darwin, and what "cannot ship" means precisely

The macOS SDK headers are covered by the Xcode licence agreement, which restricts use to Apple-branded hardware. We do not redistribute them and we do not embed them.

What we do instead, following the settled decision in document 00:

- **On macOS hosts**, use the installed SDK: `xcrun --show-sdk-path`, `-isysroot`, and the `.tbd` files in it. This is the ordinary path and it needs no special machinery.
- **On non-macOS hosts**, `rucc --target=aarch64-apple-macos14` reports that a Darwin SDK is required, names the two lawful ways to get one (a macOS machine, or an Xcode download by the user under their own licence), and accepts a path. Document 13 specifies the cache format and the provenance record.
- **We never make the fetch automatic and silent.** A tool that downloads Apple's SDK on a Linux CI box on the user's behalf makes a licence decision that is not ours to make.

**What we can do without the SDK:** emit correct Mach-O objects, and link freestanding Darwin-format binaries. That is "targets" in document 02.3's vocabulary and not "compiles for", and the support table says so.

MSVC is the same shape with a different licence and a friendlier answer: `cargo-xwin` demonstrates a user-initiated, licence-accepting fetch of the Windows SDK and CRT, and document 13 copies its mechanism. And unlike Darwin, Windows has a fully redistributable alternative, mingw-w64, which is why the default Windows environment for a cross build is `gnu` and not `msvc`.

## 8.7 What we do not become

We ship a libc and the compiler's runtime. We do not ship zlib, OpenSSL, curl, ncurses or X11 for thirty targets, and we do not acquire a package format, a dependency resolver or a mirror. Document 02.5 states this as a non-goal; it is repeated here because the sysroot machinery is exactly the machinery a distribution would need, and the gravity toward becoming one is strong.

The boundary, stated so it can be enforced: **we ship what is required to compile and link a program that uses only the C standard library and the platform's system-call interface.** Anything above that line is the user's `--sysroot`, and `--sysroot` composes with our bundled headers rather than replacing them wholesale, the layering in §8.5 is what makes "my sysroot plus your libc" work.
