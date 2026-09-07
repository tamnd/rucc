# Spec 2131 / cross-compile: every target, from every host, out of one binary

`rucc --target=aarch64-macos-none hello.c -o hello` on an x86-64 Linux box with no Xcode on it, no sysroot flag, and no second toolchain installed, producing a Mach-O executable that runs. Then the same for thirty-odd other triples.

Written 7 September 2026, against rucc 0.7.7 (M4 complete, M5 in progress, x86-64 Linux only). Extends [tamnd/rucc#8](https://github.com/tamnd/rucc/issues/8), whose title is "M7: Breadth" and whose scope today is the GNU extension surface. This specification argues that "breadth" has a second axis the milestone list never made into a deliverable, and writes it down.

References of the form "document 06" point inside this directory, at `spec/cross-compile/`. References of the form "parent document 12" point one level up, at `spec/`, which is the compiler's own specification. The two were written in separate repositories and the numbering collided, so the distinction is kept rather than renumbered, and every reference in the code says the whole path.

## The claim in one sentence

**`zig cc` proved that a self-contained cross compiler is possible; nobody has proved that an optimizing one is, and rucc is the only project in a position to try.**

That sentence is the whole of the argument, so it is worth unpacking both halves.

The first half is settled fact. `zig cc` ships one archive, about 45 MB compressed, and out of it will cross compile C to roughly forty target triples with a working libc, working start files, working headers and a working linker, on any of five hosts, with no sysroot, no `apt install gcc-aarch64-linux-gnu`, and no per-target toolchain download. Zig 0.16.0, released 14 April 2026, does this for glibc 2.2.5 through 2.43, musl 1.2.5, mingw-w64, FreeBSD 15.0, NetBSD 10.1, OpenBSD, WASI, and Apple platforms back to macOS 13.0. The machinery is public and the technique is understood. Nothing about it is a research problem any more.

The second half is the gap. `zig cc` is Clang, and Clang is 30 million lines and a 100 MB+ binary before you add a target. Every other self-contained cross toolchain in existence is either the same trick around the same Clang (`cargo-zigbuild`, `hermetic_cc_toolchain`, `cargo-xwin`) or a per-target GCC that has to be built for the triple pair it serves. The small compilers, chibicc, cproc, slimcc, kefir, TCC, are portable in the sense that they compile *on* many machines, not in the sense that they compile *for* many. And GCC's own cross story is that the compiler binary is built for one target: `aarch64-linux-gnu-gcc` and `x86_64-linux-gnu-gcc` are two programs, with two sets of baked-in paths, because gcc settles its directories at configure time.

rucc is in an unusual position. Its target facts are already data rather than `#ifdef`s (`rucc-target`), its object writers are its own rather than an assembler's, its `library.rs` and `link.rs` already ask the machine rather than a configure script, and its instruction selection is a rule set that a solver checks rather than hand-written C. Every one of those is a decision that was made for a different reason and that happens to be the precondition for this. The cost of doing it now is a fraction of the cost of retrofitting it, and the M10 exit criterion, bring up a fourth target, publish the effort number, is already a bet that the abstraction holds.

## What "all platforms" is allowed to mean

The phrase in the issue is "all major target platform, and could make cross compiler like all platform gcc support and all features of `zig cc`". That is three different scopes and they have to be separated before anything can be planned against them.

**"All platform gcc support"** is not a target. GCC 16.1 supports 49 target families including ia64, m68k, vax, pdp11, s390 31-bit (deprecated, removed in glibc 2.44), and a dozen embedded DSPs with three users each. Chasing that list is how a compiler project dies. Document 04 defines a tier system and puts an explicit floor under what gets in.

**"All features of `zig cc`"** is a target, and a good one, because it is a finite and enumerable list that somebody else has already validated against real users. Document 02 turns it into a falsifiable claim: *for every target triple `zig cc` will compile and link a hosted C program to, rucc does the same, from the same hosts, out of one binary, with no external toolchain.* That is checkable by a script, on a schedule, and it either passes or it does not.

**"All major target platform"** is the part that needs a definition, and document 04 gives one: a target is in scope if a program somebody is paid to maintain runs on it. That admits x86-64, AArch64, RISC-V, LoongArch, ppc64le, s390x, armv7, i686 and wasm32; it admits Linux, Darwin, Windows, the three BSDs, WASI and freestanding; it admits glibc, musl, bionic, mingw-w64, MSVC and none. It excludes the museum.

## The four settled decisions

**One binary, all targets, no build-time configuration.** rucc is not configured for a target pair. `Triple::host()` is a default, not a constraint, and `--target=` changes everything downstream of it including the preprocessor's predefined macros, the type widths, the ABI, the object format, the relocations, the start files and the link line. This is already true of the code as written; document 03 makes it true of the model, which today has three fields where it needs eight.

**The sysroot is the compiler's problem, not the user's.** A cross toolchain that requires `--sysroot` is not a cross toolchain, it is a compiler with a flag. Document 08 and document 09 are the two halves of this: headers that describe the target's library rather than the host's, and link-time stubs that name the target's symbols with the target's versions. Both are generated artifacts with a generator checked in, not vendored trees, for the same reason document 06 of the real-corpus spec pins sources rather than vendoring them.

**Where we cannot ship it, we say so precisely and fetch it reproducibly.** Two platforms are legally closed: the macOS SDK is restricted to Apple-branded hardware by §2.5 of the Xcode licence, and the MSVC CRT and Windows SDK are not redistributable. `zig cc` handles the first with clean-room `.tbd` stubs and its own headers and handles the second by shipping mingw-w64 instead of MSVC. rucc does the same, plus a `rucc sysroot fetch` that acquires the closed pieces on the user's own machine under the user's own acceptance, the way `xwin` and `msvc-wine` do. Document 13 states the position and the boundary.

**A target is not supported until something executes on it.** Document 14. Building is not evidence; a cross compiler that emits a plausible-looking ELF for a machine nobody ran it on is a compiler with an untested backend and a confident README. Every tier-1 and tier-2 target runs rung 0 and rung 1 of the parent ladder, natively where CI has the hardware and under QEMU user-mode where it does not, and the difference between those two kinds of evidence is recorded on every row of the table.

## What this is not

It is not a second compiler and it is not a fork. Everything here lands in `tamnd/rucc`, mostly in `rucc-target`, `rucc-driver`, `rucc-object` and a new `rucc-sysroot`, plus a generator repository for the stub data.

It is not a licence to add targets ahead of the ladder. A backend that compiles hello world for s390x and cannot build SQLite is worth less than nothing, because it converts an honest "unsupported" into a dishonest "supported". Document 04's tier definition makes rung 1 the entry price for tier 2 and rung 2 the entry price for tier 1.

It is not the internal linker. Document 11 states what cross linking demands and why shelling out to `lld` for the non-native formats is the answer for 1.0, and it is careful to keep that separable from parent document 19's open question about writing our own.

It is not C++, not a JIT, not GPU offload, and not the MSVC dialect. Parent document 14.6 excludes those and nothing here reopens them.

## The documents

| | | |
|---|---|---|
| 00 | this file | the claim, the settled decisions, what to read first |
| 01 | `01-research-2026.md` | what zig, gcc, clang, the linkers and the ABI tooling actually do, verified 7 September 2026 |
| 02 | `02-the-goal.md` | the five claims, how each is falsified, and the explicit non-goals |
| 03 | `03-target-model.md` | replacing the three-field `Triple` with a target tuple that can express what targets differ by |
| 04 | `04-target-matrix.md` | the tiers, the rows, and the entry price for each tier |
| 05 | `05-architectures.md` | per-architecture technical detail and what each demands of the backend |
| 06 | `06-abis.md` | the ABIs beyond parent document 12's five, and the ones that are traps |
| 07 | `07-object-formats.md` | ELF, Mach-O, COFF and wasm as things we write rather than read |
| 08 | `08-sysroots.md` | the header problem, and why it is harder than the library problem |
| 09 | `09-libc-stubs.md` | glibc's abilist, symbol versioning, musl, mingw-w64, the BSD stubs, `.tbd` |
| 10 | `10-runtime.md` | `rucc-builtins` per target, start files, the unwinder, TLS, and static linking |
| 11 | `11-linking.md` | which linker, per format, and what a cross link needs that a native one does not |
| 12 | `12-driver.md` | the flag surface, `-print-*`, multilib, config files, and the `triple-gcc` symlink protocol |
| 13 | `13-distribution.md` | binary size, the on-demand cache, the two legal walls, and reproducibility |
| 14 | `14-testing.md` | QEMU, real hardware, ABI differential testing, and the CI budget that makes it affordable |
| 15 | `15-plan.md` | how this maps onto M6 through M10, and what issue #8 becomes |
| 16 | `16-open-questions.md` | the six things that are not decided, each with the measurement that decides it |

## What to read first

If you are deciding whether this is worth doing, read 02 and 04.

If you are implementing it, read 03 and 08 and then 09, in that order, because the target model is upstream of everything and the sysroot is the part that is actually hard. The backend work in 05 and 06 is large but it is understood; nobody has ever been surprised by AAPCS64. The header multiplexing in 08 is the part where `zig cc` itself is still on its second attempt.

If you only want to know what it costs, document 15's table.

## The one number that matters

`zig cc` reaches its target list by carrying LLVM. rucc's whole argument is that LLVM's breadth is not free and its compile throughput is the price. If reaching thirty targets costs rucc its throughput claim or its verified-rule-set claim, this document is wrong and should be abandoned rather than negotiated. Document 02 states that as an explicit falsification condition, with the measurement attached, and document 16 keeps it open.
