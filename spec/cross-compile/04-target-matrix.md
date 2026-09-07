# The matrix

Which targets, in which order, at what standard of evidence, and what the price of admission to each tier is.

## 4.1 The entry rule

**A target is in scope if a program somebody is paid to maintain runs on it.**

That is the filter, and it is deliberately about commercial reality rather than about architecture elegance. It admits s390x, which nobody would design today and which runs bank clearing. It excludes m68k, which is charming and whose users are hobbyists with a working GCC.

Two exclusions on top of it, both adopted from zig and both principled rather than pragmatic:

**No proprietary OS whose headers cannot be obtained and audited.** Zig 0.16 removed Solaris, AIX and z/OS for exactly this reason and kept illumos because it is open. Applied to us, this excludes AIX, z/OS, QNX, VxWorks, Integrity and every other commercial RTOS. It does *not* exclude Darwin or Windows: the headers are obtainable, they are just not redistributable, which is document 13's problem rather than this one's.

**No target we cannot execute on.** Document 02's claim 4. If neither CI hardware nor qemu-user can run the output, the backend is unverifiable and the target is tier 3 forever.

## 4.2 The tiers

Adapted from zig's four-tier model, with the ordering property that document 01.1 identifies as correct: *having a sysroot is a lower bar than generating good code*.

| tier | what is true | what the README calls it |
|---|---|---|
| **1** | rungs 0, 1 and 2 pass, on hardware, at every optimization level; ABI differential green against GCC; code quality measured and within parent document 02's bound; every hardening flag works | supported |
| **2** | rungs 0 and 1 pass, on hardware or under qemu-user; ABI differential green; sysroot ships or fetches; code quality measured and published whatever it says | supported, with the evidence column saying how |
| **3** | objects are emitted and a linked program runs the smoke test; no rung passes yet | in progress, named in the table, not claimed |
| **4** | the target tuple parses and `--print-config` is correct; nothing is emitted | recognized |

The gap between tier 3 and tier 2 is where almost all the work is, and it is entirely rung 1: SQLite's test suite is the thing that turns "emits plausible ELF" into "the backend is right".

Tier 4 exists so that `rucc --target=sparc64-linux-gnu` says "recognized target, no backend, tracked as #NNN" instead of "unsupported target triple". A user who is told the compiler has never heard of their machine will conclude the wrong thing.

## 4.3 The table

Rows are `(arch, os, env)`; the three support columns are document 02.3's three nested senses. `→` marks the tier this specification is committed to reaching by the end of the plan in document 15; the tier before it is where 0.7.7 stands today.

### Linux

| target | today | → | runs rucc | notes |
|---|---|---|---|---|
| `x86_64-linux-gnu` | 2 | 1 | yes | the reference target; everything is measured here first |
| `x86_64-linux-musl` | 4 | 1 | yes | the easiest sysroot in existence; first cross target |
| `aarch64-linux-gnu` | 4 | 1 | yes | parent M6; `char` is unsigned here and signed on x86-64, which is the classic bug |
| `aarch64-linux-musl` | 4 | 1 | yes | |
| `riscv64-linux-gnu` | 4 | 1 | later | parent M9; the middle-end canary, no condition codes, no complex addressing |
| `riscv64-linux-musl` | 4 | 2 | no | |
| `i686-linux-gnu` | 4 | 2 | no | parent M10's fourth-target candidate; x87 `long double` is *the* type here, not a corner |
| `armv7-linux-gnueabihf` | 4 | 2 | no | the other M10 candidate; soft/softfp/hard is an ABI in the env field |
| `armv7-linux-musleabihf` | 4 | 2 | no | |
| `loongarch64-linux-gnu` | 4 | 2 | no | LP64D only; `$r21` reserved by psABI and used by Linux as percpu base |
| `powerpc64le-linux-gnu` | 4 | 2 | no | ELFv2; the TOC and the local/global entry point split |
| `s390x-linux-gnu` | 4 | 2 | no | **the big-endian row**; the only one with a commercial user base |
| `aarch64-linux-android` | 4 | 2 | no | bionic, API-level stub sets, RELR/packed-reloc trap below API 35 |
| `x86_64-linux-android` | 4 | 3 | no | emulator target; low demand, cheap once aarch64 works |
| `riscv64-linux-android` | 4 | 4 | no | requires RVA23 baseline; recognized only |
| `x86_64-linux-gnux32` | 4 | 4 | no | ILP32 on a 64-bit ISA; exists to prove `DataModel` is real |

### Darwin

| target | today | → | runs rucc | notes |
|---|---|---|---|---|
| `aarch64-macos` | 4 | 1 | yes | parent M6; the four AAPCS64 divergences of parent doc 12.3 |
| `x86_64-macos` | 4 | 2 | yes | still shipping; Rosetta means it is also how aarch64 hosts test x86-64 |
| `aarch64-ios` | 4 | 3 | no | distinct `Os` from macOS: different platform in `LC_BUILD_VERSION`, different availability macros |
| `aarch64-ios-sim`, `*-maccatalyst` | 4 | 4 | no | recognized; zig added catalyst in 0.16 and it is three table rows, not work |

### Windows

| target | today | → | runs rucc | notes |
|---|---|---|---|---|
| `x86_64-windows-gnu` | 4 | 1 | yes | mingw-w64; **the default env for Windows targets**, because it is the one we can ship |
| `x86_64-windows-msvc` | 4 | 2 | yes | needs a fetched SDK; ABI is ours, dialect is not |
| `aarch64-windows-gnu` | 4 | 2 | yes | |
| `aarch64-windows-msvc` | 4 | 3 | yes | |
| `arm64ec-windows-msvc` | 4 | 4 | no | recognized; separate symbol namespace, thunks, ARM64X hybrid images, see doc 06.8 |
| `i686-windows-gnu` | 4 | 3 | no | stdcall/fastcall/thiscall and name decoration; the only place C has mangling |

### BSD and the rest

| target | today | → | runs rucc | notes |
|---|---|---|---|---|
| `x86_64-freebsd`, `aarch64-freebsd` | 4 | 2 | later | stub libraries, as zig 0.15.1 does; FreeBSD 14+ |
| `x86_64-netbsd`, `aarch64-netbsd` | 4 | 3 | no | NetBSD 10.1+ |
| `x86_64-openbsd` | 4 | 3 | no | dynamic libc only; zig 0.16 added this and it took a release |
| `x86_64-illumos` | 4 | 4 | no | recognized; open, therefore admissible, therefore not excluded on principle |
| `*-none` (freestanding, every arch) | 3 | 1 | no | **the kernel target.** No libc, nine headers, `-ffreestanding`. Parent rung 4 needs this and it needs it at tier 1 |
| `wasm32-wasip1` | 4 | 2 | no | one linear memory, no signals, structured control flow, see doc 05.9 |
| `wasm32-wasip3` | 4 | 3 | no | *different ABI from wasip1/p2*: `context.get`/`context.set` for stack pointer and TLS base |
| `wasm32-freestanding` | 4 | 2 | no | |

**Count at the end of the plan:** 10 rows at tier 1, 14 at tier 2, 8 at tier 3, 7 recognized. Thirty-two rows that compile and link, which is the number document 02's claim 1 is measured against.

## 4.4 Hosts

Hosts are cheap and targets are expensive, and the asymmetry should be exploited. A host costs a CI runner, a release artifact and the small amount of code that reads the machine, `library.rs`'s candidate lists, `link.rs`'s linker search, and the sysroot cache location.

| host | today | → | how it is tested |
|---|---|---|---|
| `x86_64-linux-gnu` | yes | yes | native runners |
| `x86_64-linux-musl` | no | yes | the static-binary release artifact, which is how most people will get rucc |
| `aarch64-linux-gnu` | no | yes | native runners; also the cheapest way to catch host-endianness assumptions that are not there |
| `x86_64-macos` | yes | yes | |
| `aarch64-macos` | yes | yes | |
| `x86_64-windows` | yes | yes | |
| `aarch64-windows` | no | yes | |
| `s390x-linux-gnu` | no | tier 3 | **the big-endian host.** Zig 0.16 fixed big-endian *host* bugs in 2026, which is evidence that they exist and are found late |

The last row is the one to argue about, and the argument for it is document 01.1's observation: a cross compiler has two ends, and the host end is where nobody looks. A compiler that byte-swaps correctly when writing a little-endian object from a big-endian host has been tested by exactly one thing, and it is that row. It does not need to be a supported host; it needs to be a CI job that builds rucc under qemu-system and cross compiles rung 0.

## 4.5 The demand each target places

Grading by demand rather than by size, in the manner of the real-corpus specification's rung system. A target is worth adding early if it demands something no existing target does, because that is what finds the leak.

| demand | first target that has it | why it matters |
|---|---|---|
| a second object format | `aarch64-macos` (Mach-O) | relocations, sections, symbols, and chained fixups are all different |
| a third object format | `x86_64-windows-gnu` (COFF) | plus `.pdata`/`.xdata` unwind, which constrains the prologue |
| no condition codes | `riscv64` | every branch is a compare-and-branch; the middle end's assumptions surface |
| no complex addressing modes | `riscv64` | address folding that x86-64 hid now costs instructions |
| unsigned plain `char` | `aarch64-linux` | already a `TargetInfo` field; the test is whether anything ignored it |
| 32-bit pointers | `i686`, `armv7`, `wasm32` | pointer-sized integer assumptions, `size_t` vs `long`, and `long long` alignment |
| x87 as the only float | `i686` | `long double` is not a corner case here, it is the default double path in some code |
| **big endian** | `s390x` | byte order in constants, bitfields, unions, struct layout, and every object we write |
| a soft-float ABI | `armv7-*eabi` | `-mfloat-abi` changes argument registers, so it is in the tuple |
| a length-agnostic vector | `aarch64` SVE, `riscv64` V | the vector type has no fixed `sizeof` |
| a version-selected libc | `x86_64-linux-gnu.2.28` | symbol version nodes, and the whole reason for document 09 |
| an API-level libc | `aarch64-linux-android31` | the same mechanism spelled differently |
| a deployment target | `aarch64-macos.13` | availability macros change which declarations exist |
| a separate symbol namespace | `arm64ec-windows-msvc` | two symbol tables in one link |
| no signals, no `setjmp` as we know it | `wasm32-wasi` | structured control flow; `longjmp` needs an exception mechanism |
| **no libc at all** | `*-none` | the freestanding path, which the kernel needs and which no hosted target exercises |

Read that table as an ordering hint: `musl` first because it is cheapest, then `aarch64-macos` because Mach-O is the biggest single new thing, then `riscv64` because it is the canary, then `s390x` because big-endian is the assumption that is everywhere and named nowhere, then the rest.

## 4.6 What "on hardware" means, and the honest alternative

Tier 1 requires execution on hardware. Tier 2 permits qemu-user, and the table says which.

The gap is real. Document 01.10's caveat is the operative one: a compiler test suite exercises only the ISA subset the compiler emits, so passing under qemu-user is evidence about the code's portability rather than about the emulator's fidelity, and it is *also* evidence about the emulator that the emulator did not earn. QEMU has had wrong-flag bugs, wrong-rounding bugs and unimplemented-instruction bugs, and every one of them presents as a compiler bug first.

So the rule: **when a test fails only under emulation, the emulator is a suspect and the finding is recorded with that ambiguity attached.** A "known qemu divergence" list, with issue numbers, checked in, and its length reported next to the pass rate. A list that grows without anybody looking at it is where a real miscompilation hides.

Hardware access, in decreasing order of realism: GitHub-hosted aarch64 runners (available), self-hosted riscv64 boards, cloud s390x and ppc64le (IBM and OSUOSL both offer this to open source), and for LoongArch essentially nothing outside China. LoongArch is therefore capped at tier 2 by hardware availability and the table says so rather than pretending.

## 4.7 What this table is not allowed to become

It is not allowed to grow a "builds" column. Document 02.3 defines three senses of support and none of them is "the compiler exited zero". The evidence for a row is a test suite that ran.

It is not allowed to have a row whose tier went up without a rung passing. Tier is computed from the corpus results, mechanically, by the reporting job, not asserted in a markdown table by a human. The table in this document is the *plan*; the table in the README is generated.

It is not allowed to be reordered for convenience. The order in 4.5 is by what each target teaches, and the temptation when a target is hard will be to take an easier one out of order and count it. That is how a project ends up with four x86-family targets and no evidence that the abstraction works.
