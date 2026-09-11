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

Applying both: the glibc header payload is *one* shared tree plus a small tree per architecture family, not a tree per architecture per version. `bin/glibc-headers` in `tamnd/rucc-cross` produces it and the measurement is in document 13: 411 shared files and 77 that differ between families in 453 copies, seven families for the nine glibc ABIs in the target table, 5.3 MB against the 31 MB that nine separate installs of the same release weigh.

**The per-architecture directory is named by the libc and not by us.** Zig 0.16 ships twelve glibc directories and seventeen musl ones for the same machines, because glibc installs one `bits/` tree per architecture family and musl installs one per architecture and ABI. glibc's `x86` directory serves i386, x86-64 and x32 together, and the files in it do the splitting themselves: 22 of its 31 `bits/` headers branch on `__x86_64__`, `__ILP32__` or `__WORDSIZE`, starting with `bits/wordsize.h`. musl has separate `i386`, `x86_64` and `x32` directories and no branching. So the name of that directory is a question for the libc rather than a scheme of ours, which makes it a function of the architecture and the environment both, and `Sysroot::header_arch` in `rucc-sysroot` takes both.

**The third tree, which is nobody's libc.** `linux/`, `asm/` and `asm-generic/` belong to the kernel, and they are not optional: 31 of glibc's installed headers and 3 of musl's include one of them, `sys/ioctl.h` and the socket headers among them, so a sysroot without them fails on ordinary programs rather than on exotic ones. They are also the same files for every target that shares an architecture, because the system-call interface does not know or care which libc is calling it. So they go in the cache beside the sysroots rather than inside each one, which turns nine megabytes per Linux row into nine megabytes plus a small `asm/` per architecture. A Linux target therefore searches four directories at step 3 and not two, in the order §8.5 gives.

The kernel also has its own name for the machine, a third one after ours and the libc's: its directories under `arch/` are `arm64` and not `aarch64`, `s390` and not `s390x`, one `x86` for both widths and one `powerpc` for every width and endianness. Three namings of the same hardware is not a thing to fix, it is a thing to write down once, so `rucc-sysroot` has one function per naming and no caller guesses.

**How the version reaches the tree, and when it does not.** Zig's own `generic-glibc/features.h` says it at line 548: `/* zig patch: we pass -D__GLIBC_MINOR__=XX depending on the target. */`, with `#define __GLIBC__ 2` left in place. So the tree keeps the major version, does not define the minor at all, and the compiler supplies the minor from the tuple's `env_version`. That is the real spelling rather than a name of ours, because `__GLIBC_PREREQ` in the same file reads it and so does every autoconf probe ever written.

The condition on that is the part worth stating, because it is where a version macro goes wrong. The compiler defines `__GLIBC_MINOR__` only when step 3 resolved to the tree we bundle. A host glibc defines it in its own `features.h` and so does a tree the user named with `--sysroot`, and a second definition with a different value is a warning on every compilation of every file, so the same condition that chooses the directories chooses this. `rucc_sysroot::bundled_glibc_minor` is that function, and `None` covers musl and mingw too, where no such macro exists and a program probing for one has to hear no.

A release older than the tree is served by defining the macro lower, which turns off the declarations added after it and is the whole point of the merge. A release newer than the tree is refused by name, with both versions in the message, because that is the one direction that cannot be approximated: every `__GLIBC_PREREQ` in the program would answer yes and the declarations behind them would not be there, so the failure would land at a missing declaration at best and a missing symbol at link time at worst. The bundled release is a fact about the tree rather than a choice, it is the glibc pinned in `sysroots/manifest` in `tamnd/rucc-cross`, and `rucc_sysroot::BUNDLED_GLIBC` is where the two are kept equal.

**The honest cost.** Producing and maintaining that merged tree is real, ongoing work that scales with glibc releases, not with our target count. It is the single largest maintenance liability in this specification, and document 16 keeps it open with the question "who regenerates the header tree when glibc 2.44 ships, and how is it validated".

## 8.4 Validating a header tree

A merged multi-version header tree can be wrong in a way that produces a program that compiles and misbehaves, the `_STAT_VER` case in document 01.2 is precisely that: the stub and the header disagreed about a versioning constant, and the result linked and then failed at run time on old glibc.

Three checks, all mechanical:

1. **Structural equivalence.** For each supported (arch, glibc version), compile a corpus of `_Static_assert`s over every type the headers define, size, alignment, every member offset, against our merged tree and against the real distribution headers of that version. Any disagreement is a bug in the merge. This reuses document 06.8 mechanism 1 wholesale.
2. **Macro equivalence.** `-dM -E` over each header, both trees, diffed. Catches constants that changed value, which the struct assertions do not see.
3. **Execution.** Rung 0 and rung 1 built against the merged tree and run on a real distribution image of that glibc version, per document 02 claim 4. This is what catches the `_STAT_VER` class, and it is the only check that does.

The first two are cheap enough to run per commit. The third is per release, per (arch, libc version) in tier 1.

A fourth check applies to a sysroot we produced rather than to a header tree we merged: build it twice on two hosts and compare. Document 02 claim 5 asks for byte identical compiler output across hosts, and that cannot hold unless the inputs are byte identical first, so this one runs before the claim it serves is worth measuring.

**What that check found the first time it ran.** All four musl targets were produced on a macOS AArch64 laptop and on a Linux x86-64 machine, from the same pinned musl 1.2.5 source and the same pinned cross compiler. The 219 headers matched immediately, because they are copied rather than built. The six compiled artifacts did not, and the reason is worth writing down because it will recur for every libc we ever build: clang writes the working directory into `DW_AT_comp_dir` of every object it emits, including objects assembled from `.s` files with no debug information requested, so two machines with different home directories produce different files with identical instructions. Passing `-fdebug-compilation-dir=.` and `-ffile-prefix-map` removes it, and with those the four targets reproduce byte for byte, `libc.a` included. The producer is `bin/sysroot` in `tamnd/rucc-cross`, which is where fetching lives for the reason section 8.7 gives.

## 8.5 The search-path rules

Cross compilation makes header search a target property rather than a machine property. The rule:

1. `-I` in order.
2. The compiler's own headers (`stddef.h`, `stdarg.h`, `stdint.h`, `float.h`, `limits.h`, `stdbool.h`, `stdalign.h`, `stdnoreturn.h`, `iso646.h`, plus the intrinsic headers). **Always present, on every target including freestanding, and never taken from a sysroot.**
3. The target's libc headers: from `--sysroot` if given, otherwise from our bundled tree for that tuple, otherwise, and only when the target is the host, from the host's directories as `library.rs` computes them today.
4. Nothing else. No `/usr/local/include` when cross compiling, ever; it is a host directory and its presence in a cross build is a bug.

A tree the user named keeps whatever shape its author gave it. A buildroot or Yocto or distribution tree puts the headers under `usr/include` and ours puts them under two directories named after the tuple, so step 3 takes the directories found under the named root rather than assuming one layout and then finding nothing in it.

For a Linux target the bundled case is four directories and not two, in this order: the libc's per-architecture tree, the libc's generic tree, the kernel's `asm/` for the architecture, and the kernel's shared `linux/` and `asm-generic/`. That is the order `zig cc -E -v` prints for a glibc target and the reasoning behind it is the same as step 3's own. The libc goes first because both trees have a `sys/` and the libc's is the one a program asking for `<sys/types.h>` means, while the kernel's own names are not in a libc at all, so nothing is shadowed by putting it last. The kernel's two do not appear when the user named a sysroot, because a tree somebody else assembled has its own `linux/` from its own kernel version and taking one field from each of two kernel versions is precisely the failure a pinned header tree exists to avoid.

`-nostdinc` removes 3, `-nobuiltininc` removes 2, `--sysroot` replaces 3's root, `-isysroot` is the Darwin spelling and applies to 3 only. `-print-search-dirs` and a new `-print-sysroot` report what was chosen, which is document 12's surface.

**The failure this ordering prevents** is host contamination: a cross build that silently picks up a host header, produces something that works on the build machine, and does not work anywhere else. Document 02 claim 5 (byte-identical output across hosts) is the test that catches it, and it catches it *only* because the rule above makes step 3 host-independent when cross compiling.

## 8.6 Darwin, and what "cannot ship" means precisely

The macOS SDK headers are covered by the Xcode licence agreement, which restricts use to Apple-branded hardware. We do not redistribute them and we do not embed them.

What we do instead, following the settled decision in document 00:

- **On macOS hosts**, use the installed SDK: `xcrun --show-sdk-path`, `-isysroot`, and the `.tbd` files in it. This is the ordinary path and it needs no special machinery.
- **On non-macOS hosts**, `rucc --target=aarch64-apple-macos14` reports that a Darwin SDK is required, names the two lawful ways to get one (a macOS machine, or an Xcode download by the user under their own licence), and accepts a path. Document 13 specifies the cache format and the provenance record.
- **We never make the fetch automatic and silent.** A tool that downloads Apple's SDK on a Linux CI box on the user's behalf makes a licence decision that is not ours to make.

**What we can do without the SDK:** emit correct Mach-O objects, and link freestanding Darwin-format binaries. That is "targets" in document 02.3's vocabulary and not "compiles for", and the support table says so.

The kernel headers are on the other side of the line and worth stating as such, because GPL-2.0 on a file a compiler hands to every program it builds reads alarming until the exception is read. `LICENSES/exception/Linux-syscall-note` says that using the system-call interface does not put the calling program under the GPL, which is why every libc and every cross toolchain in existence ships these headers. Redistributing them carries the GPL's own obligation to offer the source, and it is met the same way glibc's LGPL obligation is met, by the pinned source URL and hash that document 13 requires of every input. `Licence::LinuxUapi` in `rucc-sysroot` spells it `gpl-2.0-with-linux-syscall-note` so that a manifest says which exception is being relied on and not merely which licence.

MSVC is the same shape with a different licence and a friendlier answer: `cargo-xwin` demonstrates a user-initiated, licence-accepting fetch of the Windows SDK and CRT, and document 13 copies its mechanism. And unlike Darwin, Windows has a fully redistributable alternative, mingw-w64, which is why the default Windows environment for a cross build is `gnu` and not `msvc`.

## 8.7 What we do not become

We ship a libc and the compiler's runtime. We do not ship zlib, OpenSSL, curl, ncurses or X11 for thirty targets, and we do not acquire a package format, a dependency resolver or a mirror. Document 02.5 states this as a non-goal; it is repeated here because the sysroot machinery is exactly the machinery a distribution would need, and the gravity toward becoming one is strong.

The boundary, stated so it can be enforced: **we ship what is required to compile and link a program that uses only the C standard library and the platform's system-call interface.** Anything above that line is the user's `--sysroot`, and `--sysroot` replaces step 3 rather than being layered under it, which is what gcc and clang both do with it. A user who wants their own headers and our libc says so with `-isystem`, where the ordering is theirs to state, and that is the spelling for "my headers plus your libc". The reasoning is in issue #907: a rule that appended our tree under somebody else's root would make the search path depend on what happens to be in that root, and a search path that depends on the contents of a directory is one nobody can predict from the command line.
