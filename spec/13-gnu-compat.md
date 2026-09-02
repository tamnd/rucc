# GNU compatibility

This is the document most likely to be underestimated. Document 00 names the GNU extension surface as one of the two things that could actually sink the project, and the reason is that it is not one feature but several hundred, discovered one build failure at a time, with no specification and no complete list.

## 13.1 The obligation

Defining `__GNUC__` is a promise. glibc's headers, the kernel's headers, and a large fraction of every real C project branch on it, and each branch reaches for extensions that must then all work. Document 04 sets `-fgnuc-version=` conservatively and raises it as this document's matrix fills in; this document *is* the matrix.

There is no version of this project where we implement "the important ones" and stop. `sqlite3.c` alone uses a couple of dozen; the kernel's `compiler-gcc.h` and `compiler_types.h` use over a hundred, and the ones it uses are not the obvious ones.

## 13.2 The matrix

The single most useful artifact in this document is not prose. It is `crates/rucc-gnu/features.toml`, a machine-readable table with one row per extension:

```toml
[[feature]]
name          = "attribute:cleanup"
kind          = "attribute"
gcc_version   = "3.4"
status        = "implemented"        # unimplemented | partial | implemented | rejected
used_by       = ["linux", "systemd", "glibc"]
has_builtin   = "__has_attribute(cleanup)"
tests         = ["tests/gnu/attr_cleanup.c"]
notes         = "Runs the destructor on every scope exit including goto out of the scope."
```

Four things are generated from this file at build time and are therefore incapable of drifting from reality: the `__has_attribute` / `__has_builtin` / `__has_feature` / `__has_extension` responses document 05 needs; the diagnostic for an unimplemented extension, which names the feature and links to its status rather than saying "unknown attribute"; the coverage report in CI; and the test manifest, so a feature marked `implemented` with no test is a build failure.

**Answering `__has_builtin` untruthfully is worse than answering no.** A header that gets `1` and then fails to compile is a much harder failure to diagnose than one that takes its fallback path. This is the whole reason the table is the source of truth.

The initial population comes from three sources: [MaskRay's inventory of the GNU C extensions the Linux kernel uses](https://maskray.me/blog/2025-09-14-c-extensions-used-by-linux-kernel), the GCC manual's attribute and builtin chapters, and the preprocessed output of the corpus in document 15. Grep the kernel's own build for `__attribute__` and `__builtin_` and the ground truth falls out.

## 13.3 Statement and expression extensions

**Statement expressions** (`({ ... })`) are everywhere in the kernel, every `min()`, `max()`, `container_of()` variant uses one. The hard part is not parsing but semantics: the value is that of the last statement, temporaries live until the end of the enclosing full expression, and a `goto` out of one must run cleanups. They interact with VLAs and with `__attribute__((cleanup))` in ways that need explicit handling.

**`typeof` and `typeof_unqual`** are C23 now, which simplifies matters; `__typeof__` remains as the spelling that works in all modes.

**Labels as values** (`&&label` and `goto *p`) require an IR-level indirect branch with a successor list and constrain the optimizer: a block whose address is taken cannot be deleted or merged, and its address must survive as a relocatable value. Interpreters use these, and so do a few places in the kernel.

**Nested functions are not supported.** This is the settled exception from document 06. They require executable trampolines on the stack, which is incompatible with every modern hardening measure, GCC's own support is fragile, and the kernel does not use them. `-fnested-functions` produces an error that says exactly this.

The rest: `__builtin_choose_expr`, `__builtin_types_compatible_p`, binary conditional `x ?: y`, `case a ... b` ranges, zero-length arrays, arrays of length zero as the last member, casts to union type, arithmetic on `void*` and on function pointers, non-constant initializers for aggregates, designated initializer ranges (`[0 ... 9] = x`), `__extension__`, alignment-of on expressions, complex number extensions including `__real__`/`__imag__`, and empty structs.

## 13.4 Attributes

Both `__attribute__((...))` and `[[gnu::...]]` spellings, on declarations, types, statements and labels, at every syntactic position GCC allows, which includes several that are surprising and which real headers use.

The set falls into groups by what they affect:

**Codegen and calling convention:** `noinline always_inline flatten hot cold noreturn pure const malloc returns_twice naked no_instrument_function no_sanitize target target_clones optimize interrupt`.

**Layout and storage:** `aligned packed section used unused retain visibility weak alias weakref common nocommon mode transparent_union designated_init`.

**Diagnostics and analysis:** `deprecated warning error format format_arg nonnull returns_nonnull warn_unused_result sentinel access counted_by`. The `format` attribute has to actually work. It is the mechanism behind `printf` format checking, which is one of the few warnings that finds real bugs, and the kernel defines its own `printf`-like functions with it.

**Lifetime and cleanup:** `cleanup`, `constructor`/`destructor` with priorities.

**Kernel-specific pressure points:** `no_stack_protector`, `no_caller_saved_registers`, `naked`, `section` combined with linker-script placement, and `error`/`warning` attributes, which the kernel uses to turn a link-time failure into a compile-time message and which therefore must fire at exactly the right point, after optimization, when it is known the call survives.

An unimplemented attribute warns and is ignored, matching GCC. An attribute whose *silent* ignoring would change semantics (`packed`, `aligned`, `section`, `no_sanitize`, `naked`) is an error instead if unimplemented, because ignoring those produces wrong code rather than slow code. The distinction is a column in the matrix.

## 13.5 Builtins

Several hundred. Grouped by how they are implemented:

**Constant-folded in the frontend:** `__builtin_constant_p`, `__builtin_types_compatible_p`, `__builtin_choose_expr`, `__builtin_offsetof`, `__builtin_classify_type`, the `__builtin_*_p` numeric classification family.

`__builtin_constant_p` deserves its own note. Its result depends on how much optimization has run, which makes it the one builtin whose value is not a frontend property. The kernel's `BUILD_BUG_ON` and its `min()`/`max()` macros depend on it folding after inlining, so it must be an IR intrinsic that later passes fold, not a frontend decision, and it must fold to `0` rather than being left unresolved at `-O0`.

**IR intrinsics:** the overflow-checked arithmetic family (`__builtin_add_overflow` and friends, including the `_p` predicate forms), `__builtin_expect` and `__builtin_expect_with_probability`, `__builtin_unreachable`, `__builtin_assume_aligned`, `__builtin_prefetch`, `__builtin_trap`, the bit manipulation family (`clz ctz popcount parity ffs bswap`, each in three widths), `__builtin_return_address` and `__builtin_frame_address`, `__builtin_object_size` and `__builtin_dynamic_object_size`, the latter two being what `_FORTIFY_SOURCE` is built on, which means glibc's headers stop working correctly without them.

**Library calls with known semantics:** the `mem*` and `str*` family, the math functions, so that `strlen` of a literal folds and `memcpy` of a small constant size becomes loads and stores. `-fno-builtin` and `-fno-builtin-<name>` disable this per document 04.

**Atomics:** the `__atomic_*` family with memory orderings and the legacy `__sync_*` family, mapped onto document 08's atomic instructions.

**Varargs:** `__builtin_va_start`, `va_arg`, `va_end`, `va_copy`, plus `__builtin_va_arg_pack` for the FORTIFY wrappers.

**Target-specific:** the x86 intrinsic headers (`immintrin.h` and the tree below it), the ARM NEON intrinsics, and the RISC-V vector intrinsics. This is a large body of work that is mostly mechanical and mostly generated from the same tables that drive instruction selection. It is required by any project doing SIMD by hand, which includes every video codec and most cryptography.

**Not implemented and diagnosed as such:** the OpenMP builtins, the coroutine builtins, and the C++-specific ones.

Whatever a builtin lowers to, something has to declare it first. No header does and none could, since the name is reserved to the implementation and that is the whole point of the prefix, so the compiler declares it. The type comes from the same `features.toml` the matrix above is built from, one row per builtin carrying the prototype as a string, which is what keeps the answer to `__has_builtin` and the type the builtin is called at from being two lists that drift. It is a string rather than a structure because `size_t` is a different type on two targets and the table has no target, and the vocabulary it may be written in is closed and checked by that crate's build script, so a typo fails the build rather than the compile of whoever first calls the builtin. A builtin whose type depends on what it is handed has no signature and the absence is the record of that: `__builtin_constant_p` takes anything, the overflow family takes three types that have to agree, and the atomics are a family rather than a function.

The declaration is made when a name is looked up and not found rather than before the first line is read, which is where GCC makes it. The two reach the same place, because the declaration goes in the file scope either way and a program that declares a builtin itself has its own declaration found first, and the difference is a couple of thousand names that never enter the symbol table of a translation unit that does not use them.

## 13.6 Pragmas

`#pragma GCC diagnostic push/pop/ignored/warning/error` with the full warning-name vocabulary, `#pragma GCC visibility`, `#pragma GCC optimize` and `#pragma GCC target` (both of which change per-function codegen state and therefore become function attributes in the IR, per document 04's rule about per-function flags), `#pragma pack`, `#pragma weak`, `#pragma once`, `#pragma message`, `#pragma STDC FP_CONTRACT/FENV_ACCESS/CX_LIMITED_RANGE`, and `_Pragma`'s destringizing operator.

Unknown pragmas are ignored silently in the default mode and warned under `-Wunknown-pragmas`, matching GCC.

## 13.7 The flags the kernel forces

The kernel's build sets a specific and unusual flag set, and every one of these must work, not merely be accepted:

`-ffreestanding`, `-fno-common`, `-fno-strict-aliasing`, `-fno-delete-null-pointer-checks`, `-fno-stack-protector` (per-file, in some places), `-mno-red-zone`, `-mcmodel=kernel`, `-fno-asynchronous-unwind-tables`, `-fno-omit-frame-pointer` or its opposite depending on the ORC configuration, `-fno-PIE`, `-mno-sse`/`-mno-mmx`/`-mgeneral-regs-only` to keep FPU state out of kernel code, `-fcf-protection`, `-mfunction-return=thunk-extern` and `-mindirect-branch=thunk-extern` for the Spectre mitigations, `-fpatchable-function-entry` for ftrace, `-falign-functions`, `-fconserve-stack`, `-fmacro-prefix-map`, and `-fsanitize=kernel-address`/`kernel-hwaddress` for KASAN.

Three deserve emphasis. **`-mgeneral-regs-only`** must be a hard constraint on the register allocator and on the optimizer's vectorizer, not a hint. A single auto-vectorized loop in kernel code corrupts userspace FPU state. **`-fno-delete-null-pointer-checks`** removes a specific optimizer assumption and must be threaded as a per-function attribute so LTO stays correct. **`-mfunction-return=thunk-extern`** replaces every `ret` with a jump to an external thunk, which touches the epilogue, the unwind info, and every tail call.

`-fno-strict-aliasing` is the one that most simplifies our life: it turns off document 07's TBAA entirely for kernel code, which removes a whole class of possible miscompilation from the hardest target on the ladder.

## 13.8 Testing

The matrix generates a test per feature, but per-feature tests only prove the feature exists in isolation. Three stronger mechanisms:

**Header compilation.** Compile every header in glibc, musl, and the kernel's `include/linux` standalone, in every relevant mode. Headers use extensions in combinations no test writes.

**Preprocessed-output diffing** against GCC on the corpus, per document 05, which catches `__has_*` disagreements before they become mysterious build failures.

**The corpus itself**, document 15. Real projects are the only complete specification of what "GCC compatible" means, which is uncomfortable but true, and it is why the target ladder in document 14 is the real test plan for this document rather than anything written here.
