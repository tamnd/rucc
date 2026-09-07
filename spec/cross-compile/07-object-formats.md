# The object formats

rucc writes objects itself, parent README, "no external assembler". That is a strength for throughput and a liability for breadth, because every format we write is a format we must keep current with a linker we do not control. This document is about writing, not reading; rucc reads objects only for `-flto` inputs of its own making.

The load-bearing distinction: **an object format has a version treadmill and an ABI does not.** SysV AMD64 has not changed materially in a decade. Mach-O changed how *every pointer in the file* is represented in 2021 and again for `__init_offsets`, and the linker that consumes it ships with an OS release. Budget accordingly.

## 7.1 The four formats

| format | targets | writer status | the recurring cost |
|---|---|---|---|
| ELF | Linux, BSD, freestanding, and everything else | present | per-architecture relocation sets; low |
| Mach-O | Darwin | present | **moves under us; §7.3** |
| COFF/PE | Windows | present | `.pdata`/`.xdata`; import libraries; `/DEBUG` |
| wasm | wasm32 | absent | a different model entirely; §7.5 |

Four is the whole list. No XCOFF (AIX), no ELFv1 function descriptors, no a.out, no OMF. Adding a fifth format is a much larger event than adding an architecture and document 04's entry rule should be read with that in mind.

## 7.2 ELF: mostly volume

Per new architecture, ELF costs: an `EM_` machine number, the relocation type enumeration, the addend conventions, and any architecture-specific section types or flags. That is genuinely a data file, and it is the strongest evidence for document 02 claim 3, most of the object-format work for targets 4 through 12 is filling in a table.

The parts that are not volume:

**Relocation semantics, not just numbers.** RISC-V's relaxable relocations must survive to the linker (document 05.3), so the writer needs a notion of "this fixup is not mine to resolve" that a self-contained assembler naturally wants to resolve. `R_RISCV_ALIGN` with padding, and paired `R_RISCV_RELAX` markers, are relocations that carry an instruction to the linker rather than an address.

**TLS relocation sequences** are per-architecture, come in four models each, and are the one place where the *code sequence and the relocation must match exactly* or the linker's relaxation produces something that assembles and crashes.

**Section attributes and `.init_array`.** Ordinary, but the constructor mechanism differs enough between platforms that document 10 owns it.

**`.eh_frame` versus `.debug_frame`**, and the `.eh_frame_hdr` binary search table the linker builds. rucc emits CFI; on ELF that is uniform across architectures apart from the register numbering, which is another per-architecture table.

**Endianness.** Every ELF field is written in the target's byte order, which is document 05.7's point arriving in the object writer. `ELFDATA2MSB` for s390x, and the writer is parameterized rather than host-native.

**Big/little and 32/64.** i686, armv7 and wasm32 introduce ELF32, which is a second set of structure layouts, not a flag. Real work, but done once.

## 7.3 Mach-O: the format that keeps moving

Two properties make Darwin the most expensive format to *stay* correct on, distinct from the cost of getting it correct once.

**Chained fixups replaced relocation-and-rebase.** Since macOS 12 / iOS 15 (2021), `LC_DYLD_CHAINED_FIXUPS` (`0x34|LC_REQ_DYLD`) and `LC_DYLD_EXPORTS_TRIE` (`0x33|LC_REQ_DYLD`) replace `LC_DYLD_INFO_ONLY`: the fixups are a linked list threaded *through the pointers in the data itself*, with the pointer's own bits carrying the offset to the next fixup. This is the linker's job, not the object writer's, the `.o` still carries ordinary relocations, but it constrains what our objects may contain, and if we ever link Mach-O ourselves it is a substantial feature. It is also why an old `ld64` and a new one produce structurally different binaries from the same objects.

**`__init_offsets` replaced `__mod_init_func`.** Constructors are 32-bit offsets in a read-only section rather than pointers in a writable one, chosen by the linker based on the deployment target. That means the *platform version load command we emit determines which constructor mechanism is used*, so `LC_BUILD_VERSION` with the right minos is not cosmetic.

**Everything is versioned by deployment target.** `LC_BUILD_VERSION` carries platform, minos and sdk, and both the linker and dyld make decisions from it. Document 03's `os_version` field in the tuple is why: `aarch64-apple-macos14.0` and `aarch64-apple-macos11.0` are meaningfully different targets and a three-field triple cannot say so.

**Other structural facts:** section names are `(segment, section)` pairs; symbols carry a leading underscore; there is no `.eh_frame_hdr` but there is `__unwind_info`, a compact per-function encoding that `ld64` synthesizes from `__eh_frame` and that dyld uses at run time, emitting only `__eh_frame` works but is slow and increasingly discouraged; subsections-via-symbols (`MH_SUBSECTIONS_VIA_SYMBOLS`) is how dead stripping works and requires the writer to not emit inter-symbol implicit ordering assumptions.

**The consequence for this project.** Darwin is the one target where "we wrote a correct object file" has a shelf life. The mitigation is not cleverness; it is that document 14 runs the Darwin tier-1 suite against the *current* Xcode linker on every release, and document 16 keeps "how often does Darwin break" as a tracked number.

## 7.4 COFF/PE

**`.pdata`/`.xdata`.** Windows x64 and ARM64 exception handling is table-driven from a per-function record describing the prologue as unwind codes. As document 06.4 notes, only certain prologue shapes are expressible, so this format constrains code generation. The writer must emit both sections and the linker merges them; on ARM64 there is additionally a packed unwind encoding for simple frames that is worth emitting because it is much smaller.

**Import libraries are objects.** Linking against a DLL requires an import library, which is an archive of tiny COFF objects with `IMPORT` records, or an actual short-import format. For mingw-w64 the `.a` files are provided; for MSVC the `.lib` files come from the SDK. Generating import libraries from a `.def` file is a thing we will need (document 09), and it is a COFF writer feature.

**Symbol naming and section flags.** Leading underscore on i386 and not on x64; `COMDAT` sections for inline functions and template-like deduplication, selected by a selection kind; `/ALTERNATENAME` and weak externals having COFF-specific semantics unlike ELF weak symbols.

**Debug info** on Windows is CodeView in a PDB, not DWARF. rucc emits DWARF, which mingw-w64 tooling handles and MSVC tooling does not. **Position: we emit DWARF on Windows and say so.** PDB generation is a large independent project and it is not on the path to claim 1.

## 7.5 wasm: a different kind of file

The wasm object format is not a container of machine code with relocations; it is a module with typed function definitions, an import section, a table, and a linking section (a custom section) that `wasm-ld` consumes. Symbols are typed, function signatures participate in linking, and there is no notion of an arbitrary byte offset into a function.

Combined with document 05.9's point that wasm's *code generation* is a different shape, the honest conclusion is: **wasm is a second backend, not a third architecture.** It shares the frontend, the type layout engine, the constant evaluator and most of the middle end, and shares essentially nothing below that. Document 04 keeps it at tier 2 and document 02's per-target effort claim explicitly excludes it.

## 7.6 Archives, and the small formats

`.a`, the archive format, differs by platform: System V/GNU with a `/` name table and a `/` symbol index, BSD/Darwin with `#1/` long names and a `__.SYMDEF`, and Windows with its own first and second linker members. rucc must *write* archives for `librucc_builtins.a` per target and for generated import libraries, and *read* them only if it ever links itself. Three variants, small, but a real per-platform difference that is easy to forget until an archive built on Linux is rejected on Darwin.

Also small and also required: linker response files (quoting rules differ), and the `@file` convention, because thirty-target link lines exceed Windows' command-line limit.

## 7.7 What is deliberately not built

**We do not write a debugger-quality PDB.** §7.4.

**We do not implement chained fixups as a producer.** We produce objects; the platform linker produces the image. If document 11 ever concludes that rucc should link Mach-O itself, this becomes the largest single item in it, and that is an argument against.

**We do not read arbitrary objects.** rucc is not `objdump` and not a linker. It reads only what LTO requires, and LTO inputs are rucc's own bitcode-equivalent, not machine objects.

**We do not support formats without a live linker.** That is the operational form of document 04's exclusion rule applied to formats: if no maintained linker consumes it, we do not emit it.
