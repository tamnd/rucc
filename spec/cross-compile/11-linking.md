# Linking

The parent README's position is that rucc shells out to a linker before 1.0. That is the right call and this document does not reopen it. What it does is state what changes when the link is a *cross* link, because that is where the shell-out stops being a detail and starts being the part most likely to fail.

The framing: **a native link works because the machine is already configured. A cross link works only if we configure it.** Every default the system linker has, search paths, the dynamic linker path, the default libraries, the emulation mode, is a host default, and every one of them is wrong when cross compiling.

## 11.1 What rucc's driver does today

`LinkOptions` already carries `use_ld`, `search`, `passthrough`, `prefixes`, `sysroot`, `is_static`, `shared`, `pie`, `no_stdlib`, `no_startfiles`, `no_defaultlibs`, `export_dynamic`, `strip`, `no_builtins_lib`, and `Item::{File, Library}` preserving user order. That is a good surface and most of it survives unchanged. The gaps are: no target parameter reaches the link-line construction, the linker is chosen by what is on `PATH`, and the sysroot is a user-provided path rather than a thing we can produce.

## 11.2 Which linker, per format

| format | linker | why |
|---|---|---|
| ELF | **`lld`, bundled** | the only maintained linker that cross-links every ELF architecture we target, from every host, in one binary |
| Mach-O | the platform's `ld` (macOS host) or `ld64.lld` | document 07.3: the format moves, and the platform linker is the reference |
| COFF/PE | `lld-link` for MSVC, `ld.lld` or binutils `ld` for mingw | mingw's `ld` is what mingw-w64 targets in practice |
| wasm | `wasm-ld` | there is no alternative |

**The uncomfortable part, stated plainly.** Bundling `lld` means shipping several megabytes of LLVM-derived code inside a compiler whose thesis is that LLVM is too big. Document 02.4 objects to `zig cc` on exactly that ground, so the objection applies to us and we do not get to ignore it.

The distinction that makes it survivable, and it is a real distinction rather than a rationalization: **rucc's claim is about the compile path, not the link path.** Parent document 02's axes are compile throughput and code quality, both measured to the object file. Linking happens once per program, not once per translation unit, and in a `-O0` incremental build, the case rucc exists for, the link is a small fraction of wall time and the compile is nearly all of it. LLVM's cost in rucc's argument is a *per-translation-unit* cost.

But the size argument does not survive that way, and document 13 has a binary-size budget. So:

**Position.** The linker is a separate, optional, on-demand component with the same distribution mechanism as the sysroots (document 13), not a statically linked part of the `rucc` binary. `rucc` uses a linker found on the system when one is suitable, and fetches or unpacks a bundled one when it is not. `mold` and `wild` are supported as `-fuse-ld=` choices for ELF native links, where they are faster; neither cross-links the full architecture set today, so neither can be the default.

Document 16 keeps open the question of whether rucc should eventually link ELF itself. The honest answer for now: an ELF linker for eight architectures with relaxation, TLS relaxation, `--gc-sections` and LTO is a multi-year project, and doing it badly is worse than shelling out.

## 11.3 What a cross link line needs that a native one does not

Every item here is something the system linker would have gotten right by default natively and gets wrong when cross compiling:

1. **The emulation / target mode.** `-m elf_x86_64`, `-m aarch64linux`, `/MACHINE:ARM64`, `-arch arm64 -platform_version macos 14.0 14.0`. Not optional and not inferable from the input objects reliably enough to omit.
2. **The sysroot**, and search paths *rooted in it*. `--sysroot` plus explicit `-L` for the sysroot's `lib` and `lib64` and, on multiarch distributions, `lib/<triple>`.
3. **The dynamic linker path.** `-dynamic-linker /lib64/ld-linux-x86-64.so.2`, per architecture and per libc, a literal string that is wrong by default.
4. **The start files**, by absolute path from our sysroot, in the right order: `Scrt1.o crti.o [crtbegin] … [crtend] crtn.o`.
5. **The default libraries**, which we supply rather than inherit: our stubs, `libc_nonshared.a`, the unwinder stub per document 10.4.
6. **`librucc_builtins.a` for the target**, not the host, and not the host's `libgcc`.
7. **No host paths at all.** No `/usr/lib`, no `/usr/local/lib`, no `LIBRARY_PATH` from the environment. This is the mirror of document 08.5 and it is what document 02 claim 5 tests.
8. **Format-specific extras:** `-pie`/`-no-pie` and `-z relro -z now` on ELF; `--build-id` or its absence for reproducibility; `-Wl,--as-needed` semantics; on Darwin, `-platform_version` and `-syslibroot`; on Windows, `/SUBSYSTEM` and `/ENTRY` and the import library set.

The construction of that line is `(tuple, sysroot, options) → argv`, a pure function with no environment reads, which makes it directly testable: a golden-file test per target comparing the generated argv against a recorded one. That test is cheap, catches regressions in the highest-consequence code in the driver, and needs no target machine.

## 11.4 Reproducibility at link time

Document 02 claim 5 requires byte-identical output from different hosts. Linking is where it is lost, and the causes are known:

- **Absolute paths leaking into the output**, via `.comment`, DWARF `DW_AT_comp_dir`, `-build-id` computed over paths, or Darwin's `LC_UUID`. The fix is `-ffile-prefix-map` applied by default when cross compiling, and a deterministic or suppressed build-id.
- **Timestamps.** PE headers carry one and it must be set from `SOURCE_DATE_EPOCH` or zeroed. Archives carry per-member timestamps and must be written in deterministic mode.
- **The linker's own version** appearing in the output, which is an argument for pinning the bundled linker's version per rucc release and recording it in `--print-sysroot-provenance`.
- **Ordering.** Input order is preserved by us; symbol table ordering inside archives we generate is ours to make deterministic (document 09.8).

The CI job is document 02 claim 5's: cross compile rung 1 for every tier-1 and tier-2 target from all three hosts, diff. It is the only test that finds these, and it finds them all at once.

## 11.5 LTO

rucc's LTO is over its own IR, with its own inputs, so a cross LTO link needs the plugin path or the linker's ability to invoke us. `lld` supports the LLVM plugin interface and a generic `-plugin` interface; we use the latter, and the plugin is a mode of the `rucc` binary rather than a separate shared object, which avoids shipping a `.so` per host.

The cross-specific hazard: LTO makes the compile happen *during* the link, which means the target configuration must be recoverable from the IR rather than from the command line. The tuple, the features and the data layout are serialized into every IR module and checked for agreement across modules at link time. Mismatched modules are an error, not a merge. This is the case where LLVM historically produced wrong code and there is no reason to repeat it.

## 11.6 The `-fuse-ld` and discovery rules

1. `-fuse-ld=<name>` names a linker explicitly. Honoured, and an error if unusable for the target.
2. Otherwise, if a suitable linker for the target format and architecture is on `PATH`, including `<triple>-ld`, use it.
3. Otherwise, use our bundled/fetched one.
4. Otherwise, diagnose with the exact reason and the exact command that would fix it.

"Suitable" is checked, not assumed: a host `ld` that cannot produce the target's format fails at step 2's check rather than at the link, and the diagnostic says which check failed. Silent fallback to a linker that produces a bad binary is the failure mode this ordering exists to prevent.

`-print-prog-name=ld` and `-print-search-dirs` report the outcome, per document 12.
