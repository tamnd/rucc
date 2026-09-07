# The runtime: builtins, start files, the unwinder, TLS

Everything that must exist on the target that is neither the user's code nor the libc. Parent document 12 introduces `rucc-builtins`; this document is what happens to it at thirty targets, and it opens with the arrangement that has to change.

## 10.1 The thing that is currently non-portable

`crates/rucc-driver/src/link.rs` places `librucc_builtins.a` on the link line and follows it with **the machine's `libgcc`, for the unwinder**. Natively, that is correct and economical: the host has a `libgcc` and it has an unwinder in it.

Cross, it is wrong in three independent ways. The host's `libgcc` is built for the host architecture. There may not be one, a Windows or macOS host has no `libgcc` at all. And when we ship the sysroot ourselves, reaching outside it for a library is exactly the host contamination document 08.5 forbids and document 02 claim 5 tests for.

**So: for any target whose sysroot we supply, the builtins and the unwinder both come from us.** For a native build against the host's own toolchain, the current arrangement stays, because it is fewer moving parts and it is what a user expects when they have a working GCC installed. The driver chooses between the two on exactly the question "is this compilation using a sysroot we provide", which is a single predicate and belongs in the driver, not scattered.

## 10.2 rucc-builtins per target

The compiler emits calls to helper routines the ISA cannot do in one instruction. The set is per-architecture and mostly predictable:

| category | needed on |
|---|---|
| 64-bit divide/modulo | every 32-bit target (i686, armv7, wasm32) |
| 128-bit divide/modulo, `__int128` arithmetic | every 64-bit target |
| soft float (`__addsf3`, `__divdf3`, …) | armv7 soft-float, and any target without an FPU |
| float↔int conversions with unusual widths | most |
| `long double` / binary128 arithmetic | AArch64 Linux, RISC-V, s390x, ppc64le, **all of it in software** |
| atomics beyond the lock-free width | the `libatomic` set, per target |
| `memcpy`/`memset`/`memmove`/`memcmp` | freestanding, where libc is absent |
| stack probes | Windows (`__chkstk`), and large frames elsewhere |
| TLS access helpers | `__tls_get_addr` is libc's; `__aeabi_read_tp` is ours on ARM |

**The naming divergence is a real per-target fact, not a detail.** Document 01.11: ARM's AEABI defines `__aeabi_idivmod` and friends with a two-value return in two registers, which is a different signature from the generic `__divmoddi4`, and it is the documented case where compiler-rt and libgcc genuinely differ. On ARM32 the compiler must emit AEABI spellings with AEABI conventions. Elsewhere, the libgcc names are the de facto standard because a program linking rucc-built objects with GCC-built objects must agree, and GCC's names are the incumbent, document 06.9's policy applied to the runtime.

**Binary128 is the largest single piece.** Full IEEE quad-precision arithmetic in software, add, multiply, divide, square root, conversions, comparisons, for four architectures where `long double` is binary128. It is a known quantity (compiler-rt and libgcc both have it, and the algorithms are textbook) and it is a few thousand lines that must be *correct*, testable against a reference by exhaustive testing on reduced formats and random testing on the full one.

**How it is built.** `rucc-builtins` is C compiled by rucc itself for each target, per parent document 12. That is a bootstrapping property worth keeping: it means every target's runtime is evidence that the target's codegen works, and a target whose builtins do not build is a target that is not ready. The archives are built at rucc's release time for every tier-1 and tier-2 target and shipped, because building them on demand puts a compile in the middle of every user's first link.

## 10.3 Start files

`crt1.o`/`Scrt1.o` (the entry point, which sets up the stack, calls `__libc_start_main`, and never returns), `crti.o`/`crtn.o` (the `.init`/`.fini` section prologue and epilogue fragments), and `crtbegin.o`/`crtend.o` (the compiler's constructor registration and, historically, the frame-registry).

These are per-architecture assembly, they are small, and they are tightly coupled to the libc, `Scrt1.o` calls a glibc-internal function with a glibc-specific signature.

**Position: take them from the libc, do not write them.** For glibc, build the start files from glibc source at our release time, per architecture, and ship the objects; this is what zig does and it is the only way to stay correct across the glibc versions we support. For musl and mingw-w64, the same. For freestanding, there are no start files and `-nostartfiles` is implied.

The exception is `crtbegin.o`/`crtend.o`, which are the *compiler's* files, not the libc's. Modern ELF uses `.init_array`, which the linker gathers without needing crtbegin at all, so on ELF targets we emit `.init_array` entries and need neither. Darwin uses `__init_offsets` or `__mod_init_func` per document 07.3. Windows uses `.CRT$XCU` sections and the CRT's own initialization. Three mechanisms, all data, none requiring us to ship a crtbegin.

## 10.4 The unwinder: the decision that must be made once

Document 01.11 records that this is a genuine fork in the road, so it gets stated as a decision with its consequences rather than as an option list.

The unwinder is the run-time machinery behind `_Unwind_RaiseException`, `_Unwind_Backtrace` and the personality routine protocol. C does not have exceptions, but C code needs it for: `__attribute__((cleanup))` interacting with unwinding, `-fexceptions` on code that must be transparent to C++ or to `pthread_cancel`, forced unwind for thread cancellation, and backtraces.

The three implementations, libgcc's, LLVM's libunwind, and the standalone `libunwind` (nongnu), export the same `_Unwind_*` names. **Two of them in one process is a bug**, because the personality routine and the exception object must agree, and duplicate symbols resolve arbitrarily.

**Decision: rucc does not ship an unwinder.**

- When the target's system provides one, glibc systems have `libgcc_s.so.1`, Darwin has `libunwind` in `libSystem`, Windows has native SEH-based unwinding in the OS, we link against the target's, from the target's sysroot, never the host's. For glibc targets this means `libgcc_s.so.1` gets a *stub* generated by document 09's generator exactly like `libc.so.6`, which is straightforward because its exported interface is small and stable.
- For musl-static and freestanding targets, where there is nothing, `-fno-exceptions`-style behaviour is the default: `__attribute__((cleanup))` is implemented without unwinding, and a program that needs `_Unwind_*` must supply one, with `-lunwind` and a clear diagnostic if it is absent.

**Why not ship one.** Shipping an unwinder means shipping something that must be the *only* one in the process, in a world where the C++ runtime the user links will also bring one. The duplicate-symbol hazard is not hypothetical, it is diagnosed as a mysterious crash, and it would make rucc-built objects hostile to link into existing programs, which is the property document 06.9 puts above everything else. Reconsider only if a target appears with no system unwinder and a real need for one; document 16 keeps it open.

## 10.5 Thread-local storage

Four models per ELF architecture, general dynamic, local dynamic, initial exec, local exec, each with a specific code sequence and a matching relocation sequence, and the linker relaxes between them (GD→IE→LE) when it can prove the symbol is local or in the executable. **The code sequence and the relocations must match exactly**, because the linker rewrites the instructions in place, and a sequence that assembles but does not match the expected pattern is rewritten into nonsense.

This is the highest-density per-architecture work in the whole backend after argument classification: four sequences × eight architectures, each verified against a real dynamic linker. It is also entirely mechanical, well documented per psABI, and testable by execution.

The non-ELF cases are separate mechanisms and not variants: Darwin uses `__thread_vars`/`__thread_bss` with a per-variable descriptor and a function call through it; Windows uses `_tls_index` and the `.tls` directory, plus `__declspec(thread)` semantics we partly decline per document 02.5. TLSDESC is a fifth ELF model, now the default on several architectures, and is worth implementing on AArch64 and RISC-V where it is the norm.

`_Thread_local` is C11 and in scope; emulated TLS (`-femulated-tls`) is not, on any target we support.

## 10.6 Static linking

Static is the mode where cross compilation is most useful and most likely to be what the user wanted, and it is where the target's libc choice stops being cosmetic.

- **musl static** is the good case: fully static, no interpreter, no `dlopen`, runs anywhere with a compatible kernel. This is the flagship output of the whole specification and the thing a user cross compiling for a container will actually use.
- **glibc static** is supported by glibc and is a trap: NSS (`getaddrinfo`, `getpwnam`) `dlopen`s modules at run time, so a statically linked glibc program that resolves a hostname needs the *same glibc* present on the target machine. We do not prevent it and we warn once, naming the reason, because a silent warning-free static glibc link is how people ship broken binaries.
- **`-static-pie`** requires a self-relocating start file and works on musl and modern glibc. Worth supporting because it is the right default for a hardened static binary.
- **Windows static** means linking mingw's runtime statically, which is ordinary.

The link-line ordering with static archives is order-sensitive in a way shared linking is not, and the driver's ordering, user objects, user libraries, `librucc_builtins.a`, libc, `librucc_builtins.a` again if needed for late-discovered helpers, is a per-target detail the driver owns and document 11 specifies.

## 10.7 Sanitizers, briefly

Parent document 12 has sanitizers. Cross, each one needs its runtime built for the target and each runtime uses substantially more of the platform than the compiler does, interceptors, memory mapping, symbolization. **Position: sanitizer runtimes are tier-1 targets only**, and on other targets the flag is a diagnosed error rather than a silent no-op. A sanitizer that is present and not working is worse than one that is absent.

## 10.8 The summary table

| component | who supplies it | per-target cost |
|---|---|---|
| builtins | us, C compiled by rucc | small, mostly shared; binary128 is the exception |
| start files | the libc, built at release | small, mechanical |
| constructors | us, via `.init_array` / `__init_offsets` / `.CRT$XCU` | three mechanisms total |
| unwinder | **the target system, never us** | a stub, generated |
| TLS | us, in the backend | **four sequences per architecture; the real work** |
| libatomic | us, in builtins | small |
| sanitizer runtimes | us, tier 1 only | large; deliberately not scaled |
