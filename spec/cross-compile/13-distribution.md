# Distribution: size, the cache, the licence walls, provenance

`zig cc` is self-contained and the download is over 50 MB compressed, most of it LLVM. rucc's argument in document 02.4 is that carrying LLVM is the thing it exists not to do. That argument obliges a number, so this document commits to one and specifies what goes in the binary, what goes beside it, and what is fetched.

## 13.1 The budget

| component | budget | note |
|---|---|---|
| `rucc` itself, all targets | **≤ 25 MB** | the compiler; every backend; the target table |
| compiler headers | ~200 KB | 9 headers plus intrinsics |
| libc descriptions (glibc abilist blob, musl, mingw defs) | ~1 MB | compressed, all architectures, all versions |
| glibc header tree (generic + per-arch) | ~8 MB | document 08.3's merge is what makes this possible |
| musl headers, per arch | ~2 MB total | |
| mingw-w64 headers + runtime archives | ~15 MB | the largest single bundled item |
| start files, all targets | ~1 MB | small objects |
| `librucc_builtins.a`, all tier-1/2 targets | ~10 MB | |
| **base distribution** | **≤ 60 MB uncompressed, ≤ 25 MB compressed** | |
| linker (on demand) | 15 to 40 MB | document 11.2: separate, not in the binary |
| Darwin SDK, MSVC SDK | **not distributed** | §13.4 |

The base is a target, published per release, and a regression against it is a release-blocking item in the same way a benchmark regression is. If the compiler alone exceeds 25 MB the size argument against LLVM has been lost on our own terms and document 16 should record it.

**Two configurations, not a continuum.** A `rucc-minimal` with the host target's data only, for people who are not cross compiling, and the full distribution. Not thirty per-target packages: the combinatorics are a support burden and the whole point of document 00's first settled decision is that there is one binary.

## 13.2 The on-demand cache

Some inputs are too large to bundle, some cannot be bundled legally, and some are user-specific. Those live in a cache:

```
$RUCC_CACHE_DIR (default: $XDG_CACHE_HOME/rucc or ~/.cache/rucc)
  sysroots/<tuple>/<content-hash>/{include,lib}
  stubs/<tuple>/<glibc-version>/*.so
  linkers/<host>/<version>/
  sdk/<name>/<version>/          # user-supplied, never auto-fetched
  manifest.json
```

Rules:

- **Content-addressed.** A directory's name contains the hash of its contents, so two rucc versions share what is identical and no upgrade invalidates a cache wholesale.
- **Generated artifacts are reproducible.** A stub `.so` for a tuple is byte-identical whoever generates it (document 09.8), so the cache is an optimization and never a correctness input. Deleting the cache changes nothing but time.
- **Fetches are verified.** Every downloaded artifact has a hash pinned in the rucc release, checked before use, and a mismatch is a hard failure with no override flag.
- **Fetches are opt-in and visible.** `rucc` never downloads during an ordinary compile without saying so. `rucc --fetch <tuple>` is the explicit command, `--offline` forbids it entirely, and CI is expected to use `--offline` with a pre-populated cache.
- **Concurrent-safe.** Parallel builds invoke rucc many times at once; cache population uses atomic rename into place, never in-place mutation.

The stub generator being deterministic is what makes all of this simple: there is no invalidation logic to get wrong, because nothing in the cache can be stale in a way that matters.

## 13.3 What is generated versus bundled

Bundle what is expensive to generate and needed on the first compile: headers, start files, builtins archives. Generate what is cheap and combinatorial: stub shared objects (per tuple per libc version, bundling the cross product is what would blow the budget), import libraries from `.def` files, and the toolchain directory of document 12.6.

That split is exactly why the budget in §13.1 closes. The glibc cross product of eight architectures times a dozen supported versions is unbundlable; the abilist blob that generates it is one megabyte.

## 13.4 The two walls

**Apple.** The macOS SDK headers and `.tbd` files are under the Xcode licence, which limits use to Apple-branded hardware. We do not redistribute them, we do not fetch them on the user's behalf, and we do not embed a copy. Document 08.6 specifies the behaviour: use the installed SDK on a macOS host; on other hosts, require a user-supplied path and say why.

**Microsoft.** The Windows SDK and MSVC CRT are not redistributable, but Microsoft publishes them through a manifest that permits a user, accepting the licence, to download them, which is what `cargo-xwin` does. We copy that mechanism: `rucc --fetch-msvc-sdk` prints the licence, requires explicit acceptance, downloads to the cache and records provenance. It is never implicit.

**And the answer to both is the same:** neither is on the path to document 02 claim 1, because zig's list of libc-supported targets is met with mingw-w64 for Windows, and Darwin is a target zig also cannot self-host headers for. The walls are real and they bound the same thing for everyone.

Everything else we ship, glibc headers (LGPL), musl (MIT), mingw-w64 (permissive), the BSD headers, the NDK's stubs, is redistributable, and the LGPL case is satisfied by shipping unmodified upstream headers with their notices and by the merged tree being a derived work we publish the generator for.

## 13.5 Provenance

`rucc --print-sysroot-provenance` emits, for the current target, every input that is not rucc's own code: name, upstream project, version, source URL, content hash, licence identifier, and whether it was bundled, generated or fetched. Machine-readable, stable format.

This exists for three reasons and each is sufficient on its own: it is what document 02 claim 5 requires; it is what an organization with a software bill of materials obligation needs; and it is what makes a report of "rucc produced a bad binary for target T" reproducible, because the *inputs* are named rather than implied.

The same information, for all targets, ships as a manifest in the distribution so it can be audited without running the compiler.

## 13.6 Reproducibility of the distribution itself

Building rucc's release artifacts is reproducible: pinned inputs by hash, `SOURCE_DATE_EPOCH`, no timestamps in archives, deterministic ordering everywhere. The merged glibc header tree and the abilist blob are *generated artifacts checked against their generator*: the release process regenerates them and requires byte equality with what is committed, so a silent divergence between the checked-in tree and the sources it claims to come from is a build failure.

This is bootstrappability discipline applied to the sysroot: the claim "this header tree is the merge of these upstream releases" is only meaningful if it is mechanically checked, and the check costs a CI job.

## 13.7 What this rules out

Signing and notarization of produced binaries: not ours. A package index or dependency resolution: document 08.7. Per-target release packages: §13.1. Anything that requires network access during an ordinary compile: §13.2.
