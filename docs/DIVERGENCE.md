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
| `bin/run-corpus` in tamnd/rucc-cross | four musl rows under qemu, and the runner's own row on hardware | the reference and the emulator agree about the corpus there |
| `bin/run-abi-signatures` in tamnd/rucc-cross | the same rows | the signature corpus is a program that runs, on those architectures |

The last two build with the reference compiler on both sides. They say nothing about rucc and they are not meant to. What they characterize is the emulator and the corpus, which is the thing that has to be trusted before an emulated rucc result can mean anything.

## x86-64 Linux

**qemu drops programs on the floor, at roughly one run in a hundred and thirty.** Measured, not inferred. The differential harness of section 14.3 runs four builds of one program, and one of the four has the reference compiler on both sides and no disagreement in it to find. Running that control build nine hundred times under qemu, inside the container an arm64 macOS machine uses to run x86-64 binaries, produced seven segmentation faults. The core named `qemu-x86_64`. The builds with rucc on one side or on both crash at the same rate, which is the whole point: the rate is a property of the emulator and not of any compiler.

The consequence is a rule rather than a note. A crash under emulation is tried again, up to three times, and only reported if it keeps happening. A wrong value is believed the first time, because no emulator has ever been observed to change one number in a program and leave it running. Both harnesses that run under qemu follow that rule, and both print the retry count rather than swallowing it, so the rate can be read off any log rather than taken from this paragraph.

The same job on a real x86-64 runner has not crashed once. So this entry is about the emulated path and the hardware path is clean, which is exactly the distinction section 14.4 asks these entries to make.

## The architectures the reference corpora run on

`aarch64-linux-musl`, `riscv64-linux-musl`, `armv7-linux-musleabihf` and `x86_64-linux-musl` run the layout, executing and signature corpora under qemu, built by the reference on both sides. Nothing has diverged on any of them so far, and that is a weaker statement than it sounds. What has been run is a corpus that prints values and checks them, at `-O0` and `-O2`, and none of it touches the four areas where qemu is known to be least faithful: memory ordering, denormal and rounding mode handling, syscall coverage beyond what a printing program needs, and timing.

**Concurrency and atomics are not validated on any of these rows.** qemu user mode does not model weak memory faithfully, so a race that real AArch64, ppc64 or RISC-V hardware exposes will pass here. Nothing in the corpus is threaded today, which means the gap is not currently being papered over, but it also means no row can claim atomics work on the strength of these runs.

## The rows with no execution evidence

Every other row of `docs/TARGETS.md` is compiled and not run. The static layers cover them: the tuple and driver golden files, the `_Static_assert` layout corpus compiled against the reference for every row, and the signature corpus compiled both halves for every row that has a spelling. What none of that reaches is register assignment, and section 14.1 is explicit that layer 6 is the only layer that can see a calling convention.

Three groups cannot be run here for reasons that are not about emulation at all, and they are worth separating out so nobody goes looking for a qemu bug that is not the problem.

The glibc rows link dynamically against a loader and a shared libc for the target, and neither is on the disk. Fetching a glibc runtime per architecture is what document 13 is about, and until that exists these rows compile and do not run.

The android rows have no bionic available. What the reference ships is bionic's headers, so those rows compile and there is nothing to link against.

The Darwin and Windows rows need a different kind of emulator than qemu user mode, which runs Linux binaries. The Darwin rows run on the machine they were built for, when that machine is a mac, and the Windows rows run on a Windows runner. Neither is emulation, so neither belongs in this document except to say why it is not here.

`loongarch64-linux-gnu` is the row that will stay emulated the longest. Document 04.6 records that the hardware is not purchasable outside China, so unless that changes it is a qemu-only target permanently, and its entry in this document is the whole of its evidence rather than a supplement to hardware.
