# GNU compatibility

This is the document most likely to be underestimated. Document 00 names the GNU extension surface as one of the two things that could actually sink the project, and the reason is that it is not one feature but several hundred, discovered one build failure at a time, with no specification and no complete list.

## 13.1 The obligation

Defining `__GNUC__` is a promise. glibc's headers, the kernel's headers, and a large fraction of every real C project branch on it, and each branch reaches for extensions that must then all work. Document 04 sets `-fgnuc-version=` to the highest version measured to get a real header set through, which is 7.0.0, and raises it as this document's matrix fills in; this document *is* the matrix.

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

## 13.3 Statement, expression and declaration extensions

**Statement expressions** (`({ ... })`) are everywhere in the kernel, every `min()`, `max()`, `container_of()` variant uses one. The hard part is not parsing but semantics: the value is that of the last statement, temporaries live until the end of the enclosing full expression, and a `goto` out of one must run cleanups. They interact with VLAs and with `__attribute__((cleanup))` in ways that need explicit handling.

**`typeof` and `typeof_unqual`** are C23 now, which simplifies matters; `__typeof__` remains as the spelling that works in all modes.

**`__FUNCTION__` and `__PRETTY_FUNCTION__`** are gcc's spellings of `__func__`, described in document 07 along with it. In C the three say the same thing, and the pretty one spells out a signature only in C++.

**Labels as values** (`&&label` and `goto *p`) require an IR-level indirect branch with a successor list and constrain the optimizer: a block whose address is taken cannot be deleted or merged, and its address must survive as a relocatable value. Interpreters use these, and so do a few places in the kernel.

**Nested functions are not supported.** This is the settled exception from document 06. They require executable trampolines on the stack, which is incompatible with every modern hardening measure, GCC's own support is fragile, and the kernel does not use them. `-fnested-functions` produces an error that says exactly this.

**An assembler name on a declaration** (`extern int open (const char *, int, ...) __asm__ ("open64");`) says what symbol the name stands for, and it is read today. This is how the C library redirects a name: `open` under `_FILE_OFFSET_BITS=64` is declared with one of these, every `_FORTIFY_SOURCE` wrapper is the same trick, and a compiler that walks past it links the program against the wrong function or against nothing at all. What it renames is the name and not the one declaration that wrote it, so it is kept where the declarations of a name are merged and it reaches the definition below as well, which matters because the grammar has nowhere to write one on a definition and gcc stops at the brace too. A second name that disagrees with the first is dropped with a warning worded the way gcc words it, since the name may already have been used by then and every use of a name is one symbol. It stands for a `static` and for a local `static` as well as for a name the linker sees, because the question it answers is what to call the symbol and every kind of symbol has one. An object that lives on the stack has no symbol to rename and gcc warns and carries on, which is what happens here and in gcc's words. The one reading of this syntax that is not here yet is the local variable kept in a named machine register, `register long r __asm__ ("r0")`, which is a feature of its own rather than a renaming, and it warns in its own words instead of being passed off as the case above: a program that writes one is about to hand that register to an `asm` statement and deserves to be told it did not get it.

The rest: `__builtin_choose_expr`, `__builtin_types_compatible_p`, binary conditional `x ?: y`, `case a ... b` ranges, zero-length arrays, arrays of length zero as the last member, casts to union type, arithmetic on `void*` and on function pointers, non-constant initializers for aggregates, designated initializer ranges (`[0 ... 9] = x`), `__extension__`, alignment-of on expressions, complex number extensions including `__real__`/`__imag__`, and empty structs.

## 13.4 Attributes

Both `__attribute__((...))` and `[[gnu::...]]` spellings, on declarations, types, statements and labels, at every syntactic position GCC allows, which includes several that are surprising and which real headers use.

The set falls into groups by what they affect:

**Codegen and calling convention:** `noinline always_inline flatten hot cold noreturn pure const malloc returns_twice naked no_instrument_function no_sanitize target target_clones optimize interrupt`.

**Layout and storage:** `aligned packed section used unused retain visibility weak alias weakref common nocommon mode transparent_union designated_init`.

**Diagnostics and analysis:** `deprecated warning error format format_arg nonnull returns_nonnull warn_unused_result sentinel access counted_by`. The `format` attribute has to actually work. It is the mechanism behind `printf` format checking, which is one of the few warnings that finds real bugs, and the kernel defines its own `printf`-like functions with it.

**Lifetime and cleanup:** `cleanup`, `constructor`/`destructor` with priorities.

**Kernel-specific pressure points:** `no_stack_protector`, `no_caller_saved_registers`, `naked`, `section` combined with linker-script placement, and `error`/`warning` attributes, which the kernel uses to turn a link-time failure into a compile-time message and which therefore must fire at exactly the right point, after optimization, when it is known the call survives.

Five of them are read today for one thing each, which is whether the definition exists at all. A function with internal linkage that nothing in the translation unit refers to is not emitted, since nothing outside the file can name it either, and `used`, `retain`, `constructor`, `destructor` and `alias` are how a program says that something reaches the definition from where the compiler cannot see it: a linker script, the run-up to `main`, or a second name given in a string. None of the five does anything else yet. Both spellings of each are read, and so is the armoured `__used__` form, because a header writes the armour precisely so that a program's own macro cannot take the plain name.

Two more are read for layout. `packed` gives a record and a member an alignment of one, and `aligned(n)` says what a record, a member, an object, a function or a typedef is aligned to, each in both spellings and in the armoured form as well. What `__has_attribute` answers about any of these is the status column of the matrix and nothing else, so a row that says unimplemented about an attribute this compiler honours costs a header the path it asked for, and a row that says implemented about one it ignores costs the program a compile.

`aligned` on a declaration is a raise and never a lower, which is the one place it does not agree with `_Alignas`. C23 6.7.5p5 makes an `_Alignas` below the type's own alignment a constraint violation and this compiler diagnoses it, and GCC's attribute below it is ignored without a word, so `int y __attribute__((aligned(2)))` keeps the four an `int` already had. `__alignof__` of such a declaration answers what that object got rather than what its type has, which is the question a program asking it is asking, and it is the answer whether the object is a global, a local or a function. The assembler is told the same number, as the `.p2align` in front of the label and, for a function, as the alignment of the text section as well, because a function sits at a fixed offset inside that section and is at a multiple of two hundred and fifty six only if the section is at one too.

On a typedef the same attribute means something else, and GCC lets it lower. There it says what the type is aligned to rather than putting a floor under it, so `typedef int L __attribute__((aligned(2)))` really is an `int` at a multiple of two and `struct { char c; L x; }` really is six bytes. The size is left alone, which is GCC's answer rather than something skipped here: `sizeof` an over aligned typedef is the size of what it stands for, and GCC refuses an array of one rather than padding the elements out to fit. The alignment is kept on the typedef node in the type table, so two names for one type that asked for different alignments are two types, and where a typedef of a typedef asked twice the nearer one wins, because that is the one the declaration was written with.

An unimplemented attribute warns and is ignored, matching GCC. An attribute whose *silent* ignoring would change semantics (`packed`, `aligned`, `section`, `no_sanitize`, `naked`) is an error instead if unimplemented, because ignoring those produces wrong code rather than slow code. The distinction is a column in the matrix.

The first of those errors is written today, and it is `scalar_storage_order`, which is refused with `E0688` where the attribute is written. It says the scalars in a record are stored in the byte order the target does not have, so a compilation that read past it lays the record out in the host's order and hands back every field with its bytes the wrong way round, and every program that writes it is reading a wire format or a disk image and would rather not build than get that. There is no partial reading of it either, since honouring it means a byte swap on every load and store through the record and the layout is the same either way, so a compiler that did half of it would be a compiler that got it wrong in a smaller place. The refusal is written at the attribute rather than driven off the matrix's `answer` column, because turning that column on for every row that carries it would refuse `section` and `naked` in the same change and those are a larger piece of work with their own answers.

`vector_size` is the one attribute here that builds a type rather than changing a layout, and it is read today. `int __attribute__((vector_size(16)))` is four `int` in a row that every operator works on at once, which document 07 section 7.1 describes, and the attribute is read where the declaration is checked so that the typedef it is nearly always written on declares the vector rather than the lane. An argument that is not one integer constant is refused with `E0689`, and a size of zero, a size that is not a whole number of lanes, a lane count that is not a power of two and a lane type that cannot be one are refused with `E0690`. Each of those is worded the way gcc 16 words it, because a configure script greps for exactly those words. The lane type is the one place this is narrower than gcc: gcc builds a vector of pointers and this does not have one yet, so it says the same thing gcc says about a lane type it does refuse. Reading it was not optional: a compiler that walks past the attribute declares the lane type instead and quietly computes on one lane where the program asked for all of them, which is a wrong answer rather than a missing feature, and twelve programs in the GCC torture suite were getting one.

## 13.5 Builtins

Several hundred. Grouped by how they are implemented:

**Constant-folded in the frontend:** `__builtin_constant_p`, `__builtin_types_compatible_p`, `__builtin_choose_expr`, `__builtin_offsetof`, `__builtin_classify_type`, the `__builtin_*_p` numeric classification family.

`__builtin_constant_p` deserves its own note. Its result depends on how much optimization has run, which makes it the one builtin whose value is not a frontend property. The kernel's `BUILD_BUG_ON` and its `min()`/`max()` macros depend on it folding after inlining, so it must be an IR intrinsic that later passes fold, not a frontend decision, and it must fold to `0` rather than being left unresolved at `-O0`.

**Answered as comparisons in the frontend:** the floating point classification family, which is `isnan`, `isinf`, `isfinite`, `isnormal`, `signbit`, `isinf_sign`, `fpclassify`, the six ordered comparison macros and the width bearing spellings of each. None of these may become a call, because `math.h` defines the macros of those names as exactly these builtins and no library defines a function under any of them, so a compiler that lowered one would be emitting a reference to a name that does not exist. Each becomes a comparison against a constant of the format instead: the infinities for `isinf` and `isfinite`, the smallest normal for `isnormal`, and the value being unordered with itself for `isnan`. `signbit` and `isnormal` ask their question of the bits rather than of the number, since the encoding of a value whose sign bit is clear rises with the value in every format here, which is both cheaper and the only way to ask it of the eighty bit format on x86-64, where nothing lowers a cast from an integer back into it. Each is a node of its own rather than a rewriting into the operators, because the value is compared more than once and `isnan(f())` calls `f` once. Each folds where its operand is a constant, which is what lets one initialize an object with static storage duration. `fpclassify` is the one with more than two operands: five integer constant expressions in front of the value, one of which is the answer, and gcc requires them to be constants for the same reason this does.

**IR intrinsics:** the overflow-checked arithmetic family (`__builtin_add_overflow` and friends, including the `_p` predicate forms), `__builtin_expect` and `__builtin_expect_with_probability`, `__builtin_unreachable`, `__builtin_assume_aligned`, `__builtin_prefetch`, `__builtin_trap`, the bit manipulation family (`clz ctz popcount parity ffs bswap`, each in three widths), `__builtin_return_address` and `__builtin_frame_address`, `__builtin_object_size` and `__builtin_dynamic_object_size`, the latter two being what `_FORTIFY_SOURCE` is built on, which means glibc's headers stop working correctly without them.

Two of those intrinsics are answered in the front end today rather than built. `__builtin_expect` and `__builtin_expect_with_probability` say which way a branch is expected to go and nothing reads a branch weight until the optimizer, so the value handed back is the first argument and the hint is dropped, with `Opcode::Expect` left in the IR for the day something reads one. Leaving them as calls in the meantime was not an option, because a builtin nothing lowers reaches the assembler as a call to a name no object file defines, and glibc writes `getc` and `putc` in terms of `__builtin_expect` as soon as `__OPTIMIZE__` is set, so every optimized program that included `<stdio.h>` failed to link on a name it never wrote.

`__builtin_unreachable` is the third of that kind and is a promise rather than a hint. It becomes a node with nothing under it and lowers to `unreachable_hint`, which the code generator writes no instruction for, so the promise costs nothing to honour and reading it later costs nothing to add. What is deliberately not done with it is treating the rest of the block as dead. gcc does, and that is an optimization and not the meaning: the promise being false is undefined behaviour, a compiler may do anything at all with the code below, and continuing to translate it is the choice that keeps a program built at `-O0` behaving the way its author watched it behave. The case the builtin is usually written for arrives at the right instruction anyway, because a `switch` whose default arm is `__builtin_unreachable()` and which has no return after it is a function body that runs off the bottom, which the walk already ends with the `unreachable` terminator. That terminator writes no instruction either, and the epilogue lands at the end of the block the way it does on any block that goes nowhere, which is what gcc 16.2.0 emits at `-O0` for both spellings.

The rest of the intrinsics are not built yet, and a call to one is refused where it is written with `E0686` naming the builtin. That is the general rule rather than a list: a row in `crates/rucc-gnu/features.toml` whose status is not `implemented` and which carries no `library` is a name nothing here builds anything for and nothing outside here defines, so a call to it would go to a symbol no object file has. Refusing it is what `__has_builtin` already answers for the same set, and the alternative is a linker asking the programmer to supply a builtin. What the rule does not touch is the name: `sizeof` does not evaluate its operand, so the type of a call to one of these is still answered, and a program that defines the name itself gets the function it wrote.

**Library calls with known semantics:** the `mem*` and `str*` family, the math functions, so that `strlen` of a literal folds and `memcpy` of a small constant size becomes loads and stores. `-fno-builtin` and `-fno-builtin-<name>` disable this per document 04.

The first of that family is the integer absolute value, which is `abs`, `labs` and `llabs`. These are the family where the plain name is enough: the names are reserved to the implementation by C23 7.1.3, so a program that writes one means the function the library promises and a compiler that knows what that one does may write the arithmetic instead of the call. What it writes is the sign of the value spread over every bit, an exclusive or with that, and a subtraction of it, which is four instructions with no branch and no condition code. The most negative value comes back as itself, because that is what the arithmetic gives and its magnitude is not representable, which is where C says the result is undefined and where gcc's own pair of instructions lands too.

The plain name is only the library's where nothing else has taken it, so the declaration is looked at as well as the name. It has to be a function rather than a pointer some object holds, it has to have external linkage, and its type has to be the one the library gives that name, spelled with a prototype. A `static long long llabs(long long)` is a program meaning its own function and gcc calls it, measured on 16.2.0. `-fno-builtin`, `-fno-builtin-<name>` and `-ffreestanding` are the same question asked from the command line, and the last of the three is there because a freestanding program has no C library for the name to be the name of. The `__builtin_` spellings go on meaning the library's function through all of it, which is what the prefix is for and what lets a freestanding build reach one deliberately.

Nothing here waits for a constant. gcc expands the call inline at `-O0` and so does this, because the point is not that `llabs(-1)` is one, it is that the call does not happen: `gcc.c-torture/execute/20021127-1.c` defines `llabs` to abort and expects never to reach it.

**Atomics:** the `__atomic_*` family with memory orderings and the legacy `__sync_*` family, mapped onto document 08's atomic instructions.

**Varargs:** `__builtin_va_start`, `va_arg`, `va_end`, `va_copy`, plus `__builtin_va_arg_pack` for the FORTIFY wrappers.

**Target-specific:** the x86 intrinsic headers (`immintrin.h` and the tree below it), the ARM NEON intrinsics, and the RISC-V vector intrinsics. This is a large body of work that is mostly mechanical and mostly generated from the same tables that drive instruction selection. It is required by any project doing SIMD by hand, which includes every video codec and most cryptography.

**Not implemented and diagnosed as such:** the OpenMP builtins, the coroutine builtins, and the C++-specific ones.

Whatever a builtin lowers to, something has to declare it first. No header does and none could, since the name is reserved to the implementation and that is the whole point of the prefix, so the compiler declares it. The type comes from the same `features.toml` the matrix above is built from, one row per builtin carrying the prototype as a string, which is what keeps the answer to `__has_builtin` and the type the builtin is called at from being two lists that drift. It is a string rather than a structure because `size_t` is a different type on two targets and the table has no target, and the vocabulary it may be written in is closed and checked by that crate's build script, so a typo fails the build rather than the compile of whoever first calls the builtin. A builtin whose type depends on what it is handed has no signature and the absence is the record of that: `__builtin_constant_p` takes anything, the overflow family takes three integer types that need not agree and does its arithmetic at whichever type represents every value of all three, and the atomics are a family rather than a function.

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
