# Distribution: size, the cache, the licence walls, provenance

`zig cc` is self-contained and the download is over 50 MB compressed, most of it LLVM. rucc's argument in document 02.4 is that carrying LLVM is the thing it exists not to do. That argument obliges a number, so this document commits to one and specifies what goes in the binary, what goes beside it, and what is fetched.

## 13.1 The budget

| component | budget | note |
|---|---|---|
| `rucc` itself, all targets | **≤ 25 MB** | the compiler; every backend; the target table |
| compiler headers | ~200 KB | 9 headers plus intrinsics |
| libc descriptions (glibc abilist blob, musl, mingw defs) | ~1 MB | compressed, all architectures, all versions |
| glibc header tree (shared + per-family) | 5.3 MB | measured: 411 shared files and 77 per family in 453 copies, seven families for nine ABIs, 677 KB gzipped |
| musl headers, per arch | ~2 MB total | |
| Linux uapi headers (shared + per-arch `asm/`) | 11.4 MB | measured: 970 shared files and 333 per architecture copies over seven architectures, 2.1 MB gzipped; document 08.3's third tree |
| mingw-w64 headers + runtime archives | ~15 MB | the largest single bundled item |
| start files, all targets | ~1 MB | small objects |
| `librucc_builtins.a`, all tier-1/2 targets | ~10 MB | |
| **base distribution** | **≤ 75 MB uncompressed, ≤ 30 MB compressed** | |
| linker (on demand) | 15 to 40 MB | document 11.2: separate, not in the binary |
| Darwin SDK, MSVC SDK | **not distributed** | §13.4 |

**Where the total came from, and why it moved twice.** The kernel header row was missing from the first version of this table, and adding it showed that the rows already summed to 62 MB against a stated base of 60, so the total was restated rather than nudged. Then the two header rows stopped being estimates. Both trees are produced by `tamnd/rucc-cross`, `bin/kernel-headers` and `bin/glibc-headers`, so the numbers in them are what our own output weighs rather than what zig's copy of it does, and that moved the kernel row up from 10.5 MB to 11.4 MB and the glibc row down from 8 MB to 5.3 MB. The rows come to 70.9 MB and the base stays 75, with the slack being the difference between a measurement and a budget.

What the two measurements are made of. The kernel tree is 970 headers every architecture installs identically, 9.1 MB, plus 333 per architecture copies over seven architectures, 2.2 MB, and 1.8 MB and 258 KB of that under `gzip -9`. The glibc tree is 411 shared headers, 3.1 MB, plus 77 files that differ between families in 453 copies, 2.2 MB, and seven families serve the nine glibc ABIs in the target table because the types a program gets are decided by the size of a pointer and a long rather than by the instruction set. Both records are in that repository, both trees are produced on two host architectures in its nightly run and required to come out byte for byte identical, and the glibc tree is also checked by reassembling each ABI's install from the shared tree plus its family's.

The kernel row is in the base rather than in §13.2's cache because no Linux target compiles a program that calls `ioctl` without it, and a payload every Linux row needs before anything works is not on demand in any useful sense. 31 of glibc's installed headers and 3 of musl's reach into it, so it is not an extra for programs that make system calls directly either.

The base is a target, published per release, and a regression against it is a release-blocking item in the same way a benchmark regression is. If the compiler alone exceeds 25 MB the size argument against LLVM has been lost on our own terms and document 16 should record it.

**Two configurations, not a continuum.** A `rucc-minimal` with the host target's data only, for people who are not cross compiling, and the full distribution. Not thirty per-target packages: the combinatorics are a support burden and the whole point of document 00's first settled decision is that there is one binary.

## 13.2 The on-demand cache

Some inputs are too large to bundle, some cannot be bundled legally, and some are user-specific. Those live in a cache:

```
$RUCC_CACHE_DIR (default: $XDG_CACHE_HOME/rucc or ~/.cache/rucc)
  sysroots/<tuple>/{include,lib,manifest}
  kernel-headers/{generic,<kernel-arch>,manifest}
  stubs/<tuple>/<glibc-version>/*.so
  linkers/<host>/<version>/
  sdk/<name>/<version>/          # user-supplied, never auto-fetched
  manifest.json
```

Rules:

- **Named by the tuple, described by a manifest.** A sysroot's directory is `sysroots/<tuple>` and the record of what is in it is the `manifest` file at the top of that directory, with a line per input and a digest over the whole record. A directory's name cannot hold the hash of its contents, which is what the first version of this section asked for: the path has to be computable before anything has been read, by the producer that is about to write the files and by the compiler that is about to read them, and neither of them has the contents when it asks. What that rule wanted is in the manifest instead, and more of it than a name could hold. The sharing half is already true and for a simpler reason, which is that the path carries no rucc version, so two rucc versions share every directory and an upgrade invalidates nothing. The saying half is `rucc -print-sysroot-digest`, the sha256 of the record, which is one line where the record is a few hundred and is the same number `sha256sum` prints for the file. Two hosts compare a digest rather than a few thousand files, and what is still missing is something to compare against, which is the distribution manifest below.
- **Generated artifacts are reproducible.** A stub `.so` for a tuple is byte-identical whoever generates it (document 09.8), so the cache is an optimization and never a correctness input. Deleting the cache changes nothing but time.
- **Fetches are verified.** Every downloaded artifact has a hash pinned in the rucc release, checked before use, and a mismatch is a hard failure with no override flag.
- **Fetches are opt-in and visible.** `rucc` never downloads during an ordinary compile without saying so. `rucc --fetch <tuple>` is the explicit command, `--offline` forbids it entirely, and CI is expected to use `--offline` with a pre-populated cache.
- **Concurrent-safe.** Parallel builds invoke rucc many times at once; cache population uses atomic rename into place, never in-place mutation.
- **We verify and install, and we do not transport.** rucc has no HTTP client and no TLS stack in it. The bytes are moved by a program the machine already has, and everything that decides whether the result is correct is ours. §13.8 is the whole of that decision.

The stub generator being deterministic is what makes all of this simple: there is no invalidation logic to get wrong, because nothing in the cache can be stale in a way that matters.

## 13.3 What is generated versus bundled

Bundle what is expensive to generate and needed on the first compile: headers, start files, builtins archives. Generate what is cheap and combinatorial: stub shared objects (per tuple per libc version, bundling the cross product is what would blow the budget), import libraries from `.def` files, and the toolchain directory of document 12.6.

That split is exactly why the budget in §13.1 closes. The glibc cross product of nine ABIs times a dozen supported versions is unbundlable; the abilist blob that generates it is one megabyte.

## 13.4 The two walls

**Apple.** The macOS SDK headers and `.tbd` files are under the Xcode licence, which limits use to Apple-branded hardware. We do not redistribute them, we do not fetch them on the user's behalf, and we do not embed a copy. Document 08.6 specifies the behaviour: use the installed SDK on a macOS host; on other hosts, require a user-supplied path and say why.

**Microsoft.** The Windows SDK and MSVC CRT are not redistributable, but Microsoft publishes them through a manifest that permits a user, accepting the licence, to download them, which is what `cargo-xwin` does. We copy that mechanism: `rucc --fetch-msvc-sdk` prints the licence, requires explicit acceptance, downloads to the cache and records provenance. It is never implicit.

**What exists, and the half that does not.** `rucc_sysroot::Wall` is this section as data: which of the two walls a target is behind, what is behind it, the licence that puts it there and the lawful ways to get it, in one place so that the compile, the link and the fetch say the same thing. Three behaviours follow from it and all three are written. A target behind a wall has no bundled tree, ever, so nothing names a directory under the cache that no producer may publish. A compile that found no headers for such a target is refused with the licence and the ways out rather than failing at the first `#include` as though a directory had gone missing, and `-nostdinc` is still the way to compile a program that reads none of the library. And `rucc --fetch` of one of those targets says that nothing will ever be pinned for it, which is a different sentence from the one a target whose artifact has not been published yet gets. The Darwin side of section 8.6 is written with it: the installed SDK on a macOS host, for every Apple target and not only the one the host happens to be, and a user supplied path anywhere else, from `-isysroot` or `SDKROOT`. What is not written is `--fetch-msvc-sdk`, which is the only download either wall has behind it, and until it exists an MSVC target is served by naming an installed SDK or by building for mingw-w64.

**And the answer to both is the same:** neither is on the path to document 02 claim 1, because zig's list of libc-supported targets is met with mingw-w64 for Windows, and Darwin is a target zig also cannot self-host headers for. The walls are real and they bound the same thing for everyone.

Everything else we ship, glibc headers (LGPL), musl (MIT), mingw-w64 (permissive), the BSD headers, the NDK's stubs, is redistributable, and the LGPL case is satisfied by shipping unmodified upstream headers with their notices and by the merged tree being a derived work we publish the generator for.

## 13.5 Provenance

`rucc --print-sysroot-provenance` emits, for the current target, every input that is not rucc's own code: name, upstream project, version, source URL, content hash, licence identifier, and whether it was bundled, generated or fetched. Machine-readable, stable format.

This exists for three reasons and each is sufficient on its own: it is what document 02 claim 5 requires; it is what an organization with a software bill of materials obligation needs; and it is what makes a report of "rucc produced a bad binary for target T" reproducible, because the *inputs* are named rather than implied.

The record has a header as well as a line per input, and what is in the header is what is true of the sysroot rather than of any one file: the format version, the target, and on Linux the kernel release the `linux/` and `asm/` headers came out of. The kernel release is there because it is the one input a per file line cannot carry honestly. One kernel tree serves every Linux target, so it lives in the cache beside the sysroots rather than inside each of them, and it is installed by its own command, which means a sysroot can be produced next to one release and compiled against another without anything going wrong loudly. `rucc_sysroot::Manifest::kernel` is the field and the `kernel` line is the spelling. Nothing checks the version in the header against the headers on disk, and that is the same gap section 13.2 leaves open about the libc version in a cache directory's name.

`rucc --print-sysroot-digest` is the same record as one number, the sha256 of the bytes the flag above prints plus their last newline, so it is what `sha256sum` says about the manifest file. It is section 13.2's cache rule as the thing that rule was for: a sysroot named by what is in it rather than by where it sits. `rucc_sysroot::Manifest::digest` is the function. What it covers is what the manifest covers, which is every file in the sysroot and the kernel release in the header, and not the kernel tree's own files, because those are not in the sysroot.

The same information, for all targets, ships as a manifest in the distribution so it can be audited without running the compiler.

## 13.6 Reproducibility of the distribution itself

Building rucc's release artifacts is reproducible: pinned inputs by hash, `SOURCE_DATE_EPOCH`, no timestamps in archives, deterministic ordering everywhere. The merged glibc header tree and the abilist blob are *generated artifacts checked against their generator*: the release process regenerates them and requires byte equality with what is committed, so a silent divergence between the checked-in tree and the sources it claims to come from is a build failure.

This is bootstrappability discipline applied to the sysroot: the claim "this header tree is the merge of these upstream releases" is only meaningful if it is mechanically checked, and the check costs a CI job.

## 13.7 What this rules out

Signing and notarization of produced binaries: not ours. A package index or dependency resolution: document 08.7. Per-target release packages: §13.1. Anything that requires network access during an ordinary compile: §13.2.

## 13.8 Who transports the bytes

§13.2 says a fetch is verified and opt-in without saying what does the fetching, and that is a dependency question rather than a design one, so it is settled here before any of the mechanism is written. Three answers were available. An HTTP client with a TLS stack behind the dependency wall of `spec/18-package-layout.md` section 18.3. Running `curl`, or PowerShell on Windows, which is no dependency at all and is accused of failing on a machine that has neither. Or fetching is not ours in any form and the documented path is the producer in `tamnd/rucc-cross`, which is where it lives today for the reason document 08.7 gives.

**The decision: rucc verifies and installs, and rucc does not transport.** A TLS stack is the largest thing anybody would propose putting behind section 18.3's wall, it would be the first entry in that table whose reason is not a file format, and it would be running inside the process that compiles other people's code. Nothing about moving a file needs to be in that process. Everything about deciding whether the file is the right one does, and that is the half we keep.

So `rucc --fetch <tuple>` runs a downloader the machine already has, in a fixed order, with the URL rucc pinned: `curl`, then `wget`, then `powershell -Command Invoke-WebRequest`. It writes into a temporary path inside the cache, never over anything. Then rucc computes the sha256 of what arrived with its own code and compares it against the hash pinned in this release, and a mismatch deletes the file and fails with no flag to continue past it. The transport being somebody else's program is exactly the reason the check cannot be.

The division of trust is worth stating because it looks like a weakness and is not. The downloader authenticates the connection and we authenticate the bytes. A pinned hash is what the correctness of the result rests on, and it rests on it whether the connection was trustworthy or not, which is the same reason §13.2 forbids an override flag. What we lose by not owning the transport is a better error message when a proxy is misconfigured. What we would gain by owning it is nothing the hash does not already give us.

Then the archive is unpacked with the platform's `tar`, which every host we support has, including Windows since 1803. The hash was checked over the archive before anything was unpacked, so what the unpacker does is bounded by a file we have already identified. The record that comes out of the archive is the producer's, and it is checked in both directions: every line against the file it names, and every file against the lines. That is a change from how this paragraph first read, which had rucc walk the tree and write the manifest itself, and the reason it changed is that a walk cannot know the three fields §13.5 asks for. Which release a file came out of, the URL that release was fetched from and the licence it arrives under are things the producer knows and the bytes on disk do not carry, so a manifest written here would be a record with the provenance left out of it. Checking both directions keeps what the walk was for, which is that a digest is a statement about the files that are there: a line with no file is a refusal, a file with no line is a refusal too, and an install either ends with the record and the tree agreeing or does not happen. Only then is the result renamed into `sysroots/<tuple>`, which is the atomic rename §13.2 asks for, and it is atomic because the temporary path and the destination are in one cache directory by construction.

**What exists.** All three parts are written. `rucc_driver::fetch` runs the downloaders in the order above and renames only after the hash matches, `rucc_driver::install` checks the archive, unpacks it with the platform's `tar`, checks the record against the tree and the tree against the record and renames the result into place, and `rucc_driver::artifact` is the pinned table those two are pointed at. `rucc --fetch <tuple>` is the command that joins them, `--offline` is accepted everywhere and refused beside `--fetch` because the two ask for opposite things, and a cross link that is missing a sysroot names the fetch when this release pins one for that target and says that it pins none when it does not. The table has no rows in it, which is the last thing here that is waiting on something outside this repository: a row is a URL and a sha256, and neither can be written honestly before the producer in `tamnd/rucc-cross` has published a file at one with the other. So every `--fetch` today ends in the message that says so, by name, with where a sysroot comes from, rather than in a download of nothing.

A machine with no downloader at all is the objection to the second answer and it gets an answer rather than a failure. The error names the URL, the sha256 and the exact path to put the file at, and `rucc --fetch <tuple>` run again picks a file up from that path and carries on from the verification step. A person who can reach the artifact from another machine can finish the job with a copy, and nothing in that path is different from the downloaded one after the first step.

An ordinary compile fetches nothing, with no flag and no exception. `--fetch` is the only code path that can run a downloader, `--offline` refuses even that, and a compile that is missing a sysroot says what to run rather than running it. The third answer's premise is kept whole by that: the default is still that rucc is a compiler which does not touch the network, and building a sysroot from source is still the producer in `tamnd/rucc-cross` rather than something this binary learns to do.
