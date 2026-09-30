# Known divergence

`spec/cross-compile/14-testing.md` section 14.4 says that every target's support statement records whether its evidence is hardware or emulation, and that a known divergence list is maintained per target and published. This is that list.

The rule it exists to serve is that an emulated pass is worth what the emulator is worth. qemu user mode is what makes claim 4 of document 02 affordable, and it is not the same thing as the target. A pass under emulation is real evidence and it is weaker evidence, and the only way to say how much weaker is to write down every case where a test was skipped, adjusted, or expected to differ, with the reason.

## How to read this

Absence from this document is not a clean bill of health. Most rows of `docs/TARGETS.md` have no execution evidence at all yet, because there is one back end, and a row with no evidence has nothing to diverge from. Those rows are listed too, in the last section, so that a reader who looks up their target finds an answer rather than a gap.

Each entry says what was run, on what, and what the difference was. A divergence that was found once and never chased is still an entry, because the honest reading of a short list is that the list is short, not that the emulator is faithful.

## Where the evidence comes from today

| what runs | on | what it says |
|---|---|---|
| rucc's own suite | x86-64 Linux hardware, in CI | the compiler works on the one target it emits code for |
| `cargo xtask abi-differential`, four builds | x86-64 Linux hardware, in CI | rucc and the reference agree about the calling convention there |
| `cargo xtask abi-differential`, four builds | x86-64 under qemu, in a container, on an arm64 developer machine | the same, and this is where the emulator was measured |
| `cargo xtask abi-differential --target x86_64-windows-gnu`, nine builds | under Wine on x86-64 Linux, and natively on a `windows-2025` runner, in CI | rucc and MinGW GCC agree about the Microsoft x64 convention, and the native run is the one that settles anything Wine might be hiding |
| `tests/sqlite/windows.sh`, SQLite's veryquick suite built by rucc and by MinGW GCC | under Wine on x86-64 Linux, nightly | rung 1 for `x86_64-windows-gnu`: nothing fails in rucc's build that passes in MinGW GCC's |
| `ARCH=aarch64 tests/sqlite/windows.sh`, the same suite built by rucc and by llvm-mingw's clang | natively on the `windows-11-arm` runner, nightly | rung 1 for `aarch64-windows-gnu`: nothing fails in rucc's build that passes in clang's |
| `bin/run-corpus` in tamnd/rucc-cross | four musl rows under qemu, and the runner's own row on hardware | the reference and the emulator agree about the corpus there |
| `bin/run-abi-signatures` in tamnd/rucc-cross | the same rows | the signature corpus is a program that runs, on those architectures |

The last two build with the reference compiler on both sides. They say nothing about rucc and they are not meant to. What they characterize is the emulator and the corpus, which is the thing that has to be trusted before an emulated rucc result can mean anything.

## x86-64 Linux

**qemu drops programs on the floor, at roughly one run in a hundred and thirty.** Measured, not inferred. The differential harness of section 14.3 runs four builds of one program, and one of the four has the reference compiler on both sides and no disagreement in it to find. Running that control build nine hundred times under qemu, inside the container an arm64 macOS machine uses to run x86-64 binaries, produced seven segmentation faults. The core named `qemu-x86_64`. The builds with rucc on one side or on both crash at the same rate, which is the whole point: the rate is a property of the emulator and not of any compiler.

The consequence is a rule rather than a note. A crash under emulation is tried again, up to three times, and only reported if it keeps happening. A wrong value is believed the first time, because no emulator has ever been observed to change one number in a program and leave it running. Both harnesses that run under qemu follow that rule, and both print the retry count rather than swallowing it, so the rate can be read off any log rather than taken from this paragraph.

The same job on a real x86-64 runner has not crashed once. So this entry is about the emulated path and the hardware path is clean, which is exactly the distinction section 14.4 asks these entries to make.

## The architectures the reference corpora run on

`aarch64-linux-musl`, `riscv64-linux-musl`, `armv7-linux-musleabihf` and `x86_64-linux-musl` run the layout, executing and signature corpora under qemu, built by the reference on both sides, and `x86_64-linux-gnu` runs them on the runner's own hardware. The first run of the signature corpus over that set was five rows passing, none failing and no crash retried, with the ninety two functions including the thirty four variadic ones. So the corpus is a program that runs on four architectures rather than one, which is a fact worth having about the corpus before it is used to say anything about the compiler.

Nothing has diverged on any of them so far, and that is a weaker statement than it sounds. What has been run is a corpus that prints values and checks them, at `-O0` and `-O2`, and none of it touches the four areas where qemu is known to be least faithful: memory ordering, denormal and rounding mode handling, syscall coverage beyond what a printing program needs, and timing.

**Concurrency and atomics are not validated on any of these rows.** qemu user mode does not model weak memory faithfully, so a race that real AArch64, ppc64 or RISC-V hardware exposes will pass here. Nothing in the corpus is threaded today, which means the gap is not currently being papered over, but it also means no row can claim atomics work on the strength of these runs.

## The rows with no execution evidence

Every other row of `docs/TARGETS.md` is compiled and not run. The static layers cover them: the tuple and driver golden files, the `_Static_assert` layout corpus compiled against the reference for every row, and the signature corpus compiled both halves for every row that has a spelling. What none of that reaches is register assignment, and section 14.1 is explicit that layer 6 is the only layer that can see a calling convention.

Three groups cannot be run here for reasons that are not about emulation at all, and they are worth separating out so nobody goes looking for a qemu bug that is not the problem.

The glibc rows link dynamically against a loader and a shared libc for the target, and neither is on the disk. Fetching a glibc runtime per architecture is what document 13 is about, and until that exists these rows compile and do not run.

The android rows have no bionic available. What the reference ships is bionic's headers, so those rows compile and there is nothing to link against.

The Darwin and Windows rows need a different kind of emulator than qemu user mode, which runs Linux binaries. The Darwin rows run on the machine they were built for, when that machine is a mac. `x86_64-windows-gnu` runs in two places on every pull request: natively on a `windows-2025` runner, and under Wine on Linux. Wine is not an emulator but a second implementation of the Windows API, so a program that passes under Wine and fails on Windows is a Wine or C runtime difference rather than a compiler bug, and the native job is the one that settles it. The two jobs also have to write the same objects byte for byte. The other Windows rows are compiled and not run yet.

`loongarch64-linux-gnu` is the row that will stay emulated the longest. Document 04.6 records that the hardware is not purchasable outside China, so unless that changes it is a qemu-only target permanently, and its entry in this document is the whole of its evidence rather than a supplement to hardware.

## x86_64-windows-gnu

**Rung 1 runs under Wine, and the reference build is the oracle rather than a clean pass.** `tests/sqlite/windows.sh` builds SQLite 3.53.4's testfixture twice from Linux, once with rucc and once with Ubuntu's MinGW GCC 13, against one Tcl 8.6.16 built by that GCC, and runs `test/veryquick.test` from both under Wine. Neither build passes everything there, so the rule is that nothing may fail in rucc's build that passes in GCC's. What differs between the two builds, and what is left out, is listed here.

**The two builds link different C runtimes.** rucc's sysroot is UCRT and Ubuntu's MinGW GCC links msvcrt, and so does the Tcl DLL both testfixtures load. `date4.test` compares SQLite's `strftime` with the C library's for every conversion, and msvcrt lacks the C99 ones, so GCC's build fails about 24,800 of those cases and rucc's build passes all of them. That part of the suite has no oracle on this row. The environment is the other place the split shows: Tcl's `set env(...)` lands in msvcrt's copy, and a UCRT `getenv` does not see it, which broke `vtabH.test` in rucc's build until the script started setting `fstreeDrive` before Wine does.

**Three test files are removed, and a few cases fail in both builds.** `symlink2.test`, `win32lock.test` and `win32longpath.test` each end the whole suite under Wine, the first on a `del` that fails, the second on a file lock that answers busy where Windows waits, and the third on a long path it cannot delete. Sixteen cases fail in both builds and are left to the reference to excuse: writes to a read-only database succeed (`delete-8.*`, `backup2-6`), an unopenable file reports an I/O error instead (`pager1.4.7.2`, `pager1.4.8.1`), four `temptable-6.*` cases, `sessionnoact-4.3`, `extension01-1.6` and `win32longpath-1.3`.

**On AArch64, rung 1 runs on Windows itself.** No server here has an ARM64 Wine, so `ARCH=aarch64` runs the script in Git for Windows' bash on the `windows-11-arm` runner, where Tcl 8.6.16 and the reference testfixture are built by llvm-mingw's clang 23 and both builds link the same UCRT. Nothing is removed from the suite there. On the first runs both builds failed only `sessionnoact-4.3` and `vtabH-3.1`, 2 errors out of about 393,300 tests each, in about 21 minutes per build.

**The build flags carry workarounds for SQLite's source.** `ext/misc/fileio.c` uses Windows types and `dirent` without including their headers and uses `S_ISLNK`, so both builds get `-include windows.h -include dirent.h -DS_ISLNK=0*`. Including `windows.h` first means `off_t` is fixed at 32 bits before `sqliteInt.h` asks for `_FILE_OFFSET_BITS=64`, and the mingw-w64 headers in rucc's sysroot then declare `struct stat` with a 32 bit `st_size` and send `fstat` to `fstat64`, so `test_fs.c` read every file as empty. MinGW GCC does the same with those headers. `-D_FILE_OFFSET_BITS=64` on the command line keeps them consistent.

## The MSVC rows

**Structured exception handling is refused.** `__try`, `__except`, `__finally` and `__leave` are keywords on the `*-windows-msvc` rows, so that a file using them gets an error that names the construct rather than a parse error about an undeclared `__try`. That error is "structured exception handling (`__try`) is not supported; see docs/DIVERGENCE.md", and this entry is the one it points at. MSVC and clang both compile these statements. They need a scope table in the unwind data and a filter that runs as a funclet during the first pass of the unwinder, and rucc writes unwind records that describe frames and nothing more, so there is no reading of `__try` that would compile to something right. A program that needs it has to be built with MSVC or clang-cl for now. The mingw rows are not affected, since gcc has no `__try` either and the words are ordinary names there.
