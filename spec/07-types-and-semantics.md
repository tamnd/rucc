# Types, semantics, and the undefined behavior model

Semantic analysis takes the AST from document 06 and produces a typed AST: every expression carries a `TypeId`, every implicit conversion is an explicit node, every constant that can be folded is folded, and every declaration is resolved to a symbol. Nothing downstream re-derives a type.

## 7.1 Type representation

Types are interned. `TypeId` equality is type identity, which turns the most frequent operation in the compiler into an integer comparison.

The representation separates **canonical** types from **sugar**. `typedef int32_t;` gives a sugar node pointing at canonical `int`. Every semantic decision uses the canonical form; every diagnostic uses the sugar form, so the error says `int32_t` rather than `int`. Compilers that discard sugar produce error messages nobody can act on, and compilers that make semantic decisions on sugar produce wrong answers. Both directions are common bugs.

The type universe: the basic types including `bool`, the character types with `char` distinct from both `signed char` and `unsigned char`, the standard and extended integer types, `_BitInt(N)` for `N` from 1 to a target-defined maximum, the real and complex floating types including `_Decimal32/64/128` **[deferred past 1.0, see document 19]**, `void`, pointers, arrays with constant, variable or unspecified size, functions, structs, unions, enums with their underlying type, atomic types, qualified types, and the GNU vector extension types.

A type is spelled the way it would be written, which means it is assembled outward from the hole where the name goes rather than printed left to right: an array of pointers to functions is `int (*f[3])(char)` and there is no other way to write it. The abstract form is the same walk with an empty hole, and the one place it is not simply the declaration with the name deleted is the space in front of the declarator. `int[3]` and `int(void)` have nothing between the type and the suffix because there is nothing there to separate, and `int (*)[3]` keeps the space because there is. That is GCC's spelling and it is what a build that greps a message for a type expects to find.

A GNU vector is a lane type and a lane count, built by `__attribute__((vector_size(n)))` where `n` is the size of the whole thing in bytes. Its size is that many bytes and it is aligned to its own size, the lane count has to be a power of two, and the lane has to be one of the arithmetic types, which is narrower than GCC by one case: GCC builds a vector of pointers and this compiler does not have one yet. Every operator that takes an arithmetic type takes a vector of one and means that operator on each lane, a scalar written beside a vector stands for itself in every lane, a subscript reads one lane and is an lvalue when the vector is one, and a cast between a vector and anything of the same size reinterprets the bytes rather than converting a value. What is deliberately not done yet is holding one in a register: a vector is an object in memory and each lane is computed on its own, which is correct arithmetic and is not the instruction the machine has, and the two things that reading depends on are in `tamnd/rucc#200`.

A comparison of two vectors is the one operator whose answer is not the type of its operands. `a < b` over vectors of `float` is a vector of the signed integers of the lane width, and each lane is all ones where the comparison held and zero where it did not, which is what makes the answer something to mask with rather than something to branch on. The lane type of the answer is the signed integer of the same width as the lane compared, so a comparison of `unsigned int` lanes answers in `int` lanes and a comparison of `float` lanes answers in `int` lanes as well. That leaves the answer with a type the operands cannot hold, so the assignment rules take GNU's lax conversion too: two vectors of the same size convert between each other by reinterpreting the bytes, which is what GCC's `vector_types_convertible_p` decides and is why `unsigned int` lanes can be assigned the mask they were compared into. All ones rather than one is the part a program depends on, since the mask is written `(a > b) & a` far more often than it is tested.

Qualifiers (`const`, `volatile`, `restrict`, `_Atomic`) are carried on the type as a bitmask in the interned key rather than as wrapper nodes, so `const int` and `int` are two interning entries with the same base. `_Atomic` is subtle: `_Atomic(T)` is a distinct *type*, not merely a qualifier, and its size and alignment may differ from `T`'s. The prior art in document 01 recorded "`_Atomic` parsed but the qualifier is not tracked through the type system" as a known limitation, which is precisely the shortcut that makes atomics silently wrong, and we do not take it.

## 7.2 Conversions and arithmetic

Integer promotions, the usual arithmetic conversions, and the C23 adjustments to them are implemented from the standard's text rather than from memory, with a test per clause. The places this is routinely got wrong: bit-fields promote based on their declared width, not their declared type, so a 3-bit `unsigned` field promotes to `int`; `_BitInt` types do *not* undergo integer promotion; enums promote based on their underlying type, which C23 lets the user specify; and the usual arithmetic conversions between a signed and an unsigned type of equal rank produce the unsigned type, which is the source of more real bugs in C code than any other rule and therefore deserves a good warning rather than silence.

A bit-field wider than an `int` is the one place where neither wording says what both compilers do. C17 says such a field promotes to `unsigned int`, which would turn a 40-bit field into a 32-bit value, and C23 says it keeps its declared type, which would turn it into a 64-bit one. GCC gives it a type whose precision is the width instead, so `x.b << 32` on a 40-bit field is zero rather than a value with a bit above the fortieth, and Clang agrees. That type is `_BitInt(40)` and saying it that way is what makes the usual arithmetic conversions give the right answer for a pair of such fields without knowing where either came from: the rules already rank a `_BitInt` by its width, so a 33-bit field and a 41-bit field meet at 41 bits and the multiply of the two wraps there. A field as wide as the type it was declared with keeps that type, since there is no precision to lose. The width the arithmetic happens at is a middle end fact and the machine has four integer widths, so the back end puts the rest into the ones it has; section 10.2 of `spec/10-backend.md` says how.

Every implicit conversion becomes an explicit node in the typed AST. Nothing downstream infers "there must be a conversion here"; if the tree says the operands are `int` and `long`, that is a bug in sema, and the IR builder is entitled to assume it never happens. The verifier in document 08 checks it.

Array-to-pointer and function-to-pointer decay, and lvalue conversion, are explicit nodes for the same reason.

Assigning one pointer to another where the pointees are unrelated is an error, which is where GCC 14 landed and where GCC 16 still is. The exception is a pointee that is the same integer type written with the other sign, which is a warning: the two point at the same bytes and the code that does it is usually reading a byte buffer rather than confusing two types. Rank and not width decides which case it is, so `unsigned char *` to `char *` is the warning and `long long *` to `long *` is the error even though the two are the same width here. The three character types share one rank and are three distinct types, so `signed char *` to `char *` is the warning as well, which is what GCC says about it. GCC keeps that warning off until `-Wall` or `-pedantic` asks for it and there is no such switch here yet, so it is always on.

## 7.3 Compatibility and composite types

Type compatibility is a distinct relation from identity and C's rules for it are more permissive than intuition suggests. Two struct types declared in different translation units with the same tag, the same member names and compatible member types are compatible. Function types are compatible if their return types are compatible and their parameter lists are, with an unprototyped declaration compatible with a prototype under conditions involving default argument promotions. Arrays with and without a size are compatible and their composite has the size. Enum types are compatible with their underlying integer type in specified ways.

C23 changed this area meaningfully: struct and union types with the same tag and members are now compatible across translation units by a defined rule, and identical `typedef`s may be redeclared. These changes matter for a compiler that will be compiling code written against both old and new rules, and the dialect gates the behavior.

The composite type construction is used at redeclaration and at conditional-expression type computation, and getting it wrong produces either spurious errors on valid code or silently wrong ABI decisions.

## 7.4 Initialization

Initializer processing is a small, tedious, high-bug-density algorithm and it gets its own module with its own test suite.

The semantics are a cursor walking a nested aggregate while consuming an initializer list. The hard parts, each historically a bug in every C compiler: **brace elision**, where `struct {int a[2]; int b;} x = {1,2,3};` is legal and the sub-object boundaries are inferred; **designated initializers**, which move the cursor and after which the walk *continues from the new position* rather than resuming where it was; **overlapping designators**, where a later initializer overwrites an earlier one and the earlier one's side effects still occur; **partial initialization**, where remaining members are zero-initialized with the correct semantics for padding; **flexible array members**, which may be initialized in some contexts and not others; **string literal initialization** of char arrays, including the case where the array is exactly the length without room for the terminator; and **compound literals**, whose lifetime is automatic at block scope and static at file scope.

Static initializers are evaluated by the constant evaluator and lowered to a byte-level image with a relocation list, which is what the object writer in document 11 consumes. Producing that image directly, rather than a tree the backend re-walks, is what makes a one-megabyte `#embed` initializer or a large lookup table compile in reasonable time.

The entries reach the image in the order they were written, which is not the order the bytes go in, so the image sorts them by offset first. The sort is stable and that is what settles the overlapping case: among the entries at one offset the written order is kept and only the last of them stands. A bit-field is the exception in both halves of the rule, because several fields share one offset without writing over anything and only the bits of the field named are replaced when it is named twice.

## 7.5 The constant evaluator

One evaluator, used for six things: `#if` expressions (in document 05's dialect, over `intmax_t`), enum values, bit-field widths, array bounds, static initializers, `_Static_assert` conditions, `constexpr` objects, and `case` labels.

It is a small interpreter over the typed AST with a three-valued result: a value, "not a constant expression" (which is fine in contexts where constancy is optional), or "constant expression with a constraint violation" (which is a diagnostic). Keeping the third case distinct from the second is what allows `1/0` in an unevaluated `sizeof` to be silent while `int a[1/0];` is an error.

Arithmetic is exact where the standard requires it and target-faithful where it does not. Integer arithmetic is performed at the target's widths with overflow detected. Floating arithmetic is performed with a software implementation at the target's format, not the host's. A compiler that folds `0.1f + 0.2f` using the host FPU produces different objects on different hosts, which violates the determinism rule in document 03 and is a real historical bug class in cross-compilers.

`constexpr` from C23 extends the evaluator's reach to any object declared `constexpr`, with the rule that the initializer must be a constant expression and the type must not be variably modified.

## 7.6 Floating point semantics

The default is `-ffp-contract=on` and `-fexcess-precision=fast`, matching GCC.

`FLT_EVAL_METHOD` is a target property. On x86 without SSE it is 2, meaning `float` and `double` operations are evaluated in `long double`; on x86-64 with SSE and on AArch64 it is 0. `-fexcess-precision=standard` forces the standard's behavior of rounding at assignment and cast points, which Postgres requires and which is a real code generation obligation, not a flag we can accept and ignore.

Contraction of `a*b+c` into a fused multiply-add is permitted under `-ffp-contract=on` within a single expression and under `fast` across expressions, and forbidden under `off`. `#pragma STDC FP_CONTRACT` scopes it.

Under default settings, the optimizer may **not** reassociate floating point, may not assume the absence of NaN or infinity, and may not turn division into multiplication by a reciprocal. `-ffast-math` enables each of these as an individually-named component flag, and the IR carries them as per-instruction fast-math flags rather than as a global mode, so LTO across differently-compiled units stays correct.

`-frounding-math` disables constant folding of operations whose result depends on the rounding mode. `#pragma STDC FENV_ACCESS` does the same locally and additionally forbids moving FP operations across `fenv` calls.

## 7.7 The undefined behavior model

This is the section that determines whether the compiler is trustworthy, and the position it takes is deliberate.

**The policy: we exploit a written, closed list of undefined behaviors. Each entry on the list has a flag that disables the exploitation, and a `-fsanitize=undefined` check that detects it at runtime. Undefined behavior not on the list is not exploited.**

The reasoning is not timidity. It is that an optimizer's UB assumptions are the part of a compiler that is hardest to debug from the outside, and a closed written list is the difference between "your code has a bug and here is which rule you broke" and a bisection through forty passes. It is also the position the kernel forced on GCC over twenty years, one `-fno-` flag at a time, and starting where that ended is cheaper than repeating it.

The list, each with its flag and its sanitizer check:

| UB exploited | Disable with | Detected by |
|---|---|---|
| Signed integer overflow is impossible | `-fwrapv`, `-fno-strict-overflow` | `-fsanitize=signed-integer-overflow` |
| A dereferenced pointer is non-null | `-fno-delete-null-pointer-checks` | `-fsanitize=null` |
| Objects are accessed through compatible effective types | `-fno-strict-aliasing` | `-fsanitize=alias` (ours, see below) |
| `restrict`-qualified pointers do not alias | `-fno-restrict` | `-fsanitize=restrict` (ours) |
| Shift counts are less than the operand width | none; always exploited | `-fsanitize=shift` |
| Division does not overflow or divide by zero | none | `-fsanitize=integer-divide-by-zero` |
| Pointer arithmetic stays within an object | `-fno-strict-provenance` | `-fsanitize=pointer-overflow` |
| Loops without side effects terminate | `-fno-finite-loops` | none practical |
| An object's lifetime is respected | none | `-fsanitize=address` |
| Uninitialized reads produce a value, not poison | on by default; `-fstrict-init` opts in | `-fsanitize=memory` |

The last row is a deliberate departure from LLVM. LLVM models uninitialized memory as `poison` and exploits it aggressively, which is correct by the standard and which produces a well-known category of baffling miscompilations in real code. We treat an uninitialized read as producing an unspecified but *stable* value by default, which costs a small amount of optimization and removes an entire class of user-visible surprise. `-fstrict-init` opts into the aggressive model for code that wants it. Document 19 records that this may cost more than expected on some benchmarks and that the decision is measured, not assumed.

`-fno-strict-overflow` versus `-fwrapv` are not the same thing and both are implemented: the former says do not *assume* the absence of overflow, the latter says overflow is *defined* to wrap. The kernel wants the former; Postgres wants the latter.

## 7.8 Provenance and aliasing

The reference semantics for pointer provenance is **PNVI-ae-udi** from [WG14 N3005](https://www.open-std.org/jtc1/sc22/wg14/www/docs/n3005.pdf), as surveyed in document 01. Every storage instance, an object beginning its lifetime, or an allocation, carries an ID unique for the entire execution. A pointer's provenance is the ID of the instance it points into or one past. Addresses may be reused; IDs never are. Integer-to-pointer casts recover provenance only for storage whose address has been *exposed*, and pointers synthesized in the ambiguous corner cases are the user's responsibility to disambiguate.

Concretely, the IR carries provenance as an attribute on pointer values and the alias analysis in document 09 is permitted to conclude "these cannot alias" exactly when the model says so, and not otherwise. This is what makes the alias analysis auditable: when it produces a wrong answer, the question "which rule licensed that" has an answer.

Type-based alias analysis follows the effective type rules of 6.5, with the character-type exemption, the `memcpy` rule, the union-member rules and the common-initial-sequence rule all implemented. `-fno-strict-aliasing` disables TBAA entirely and is what the kernel builds with. The TBAA metadata attached to memory operations in the IR uses a type hierarchy, not a flat type identity, so that a `struct` access and an access to its first member are correctly related.

`-fsanitize=alias` and `-fsanitize=restrict` do not exist in GCC or Clang. They are ours, they are cheap given that the IR already carries the metadata, and they turn "this project miscompiles under strict aliasing" from a two-day bisection into a runtime report. Document 15 runs them over the corpus, and the findings are as likely to be bugs in the corpus as bugs in us, which is itself useful.

## 7.9 Atomics and the memory model

`_Atomic` types, the `<stdatomic.h>` generic functions, and the `__atomic_*` and legacy `__sync_*` GCC builtins.

The memory model is C11's: `relaxed`, `consume` (treated as `acquire`, as every real compiler does, with the reasoning recorded rather than silent), `acquire`, `release`, `acq_rel`, `seq_cst`. Orderings are carried on IR memory operations and constrain the optimizer directly: the alias analysis and the load/store elimination passes consult them, rather than atomics being modelled as opaque calls, which is the shortcut that produces correct-but-slow code and occasionally incorrect code when a later pass forgets.

Atomic operations that fit in a target register lower to native instructions; wider ones lower to `__atomic_*` library calls per the ABI's lock-free-property rules, and `ATOMIC_*_LOCK_FREE` macros must agree with what we actually emit. Getting that agreement wrong means a struct is atomic in one translation unit and locked in another.

## 7.10 Variably modified types

VLAs, and variably-modified types generally, are supported. They are optional in C11 and later but the kernel used to use them, a great deal of scientific code uses them, and `int (*p)[n]` as a parameter type is genuinely useful.

The implementation evaluates the size expression once at the point the declaration is reached and stores it in a hidden temporary, because the standard requires the size to be evaluated exactly once and later uses of `sizeof` on the VLA read the temporary rather than re-evaluating. Scope exit restores the stack pointer, `goto` out of a VLA scope deallocates correctly, and `longjmp` into one is undefined and diagnosed where detectable. `-Wvla` and `-Wvla-larger-than=` exist because the kernel bans them.

## 7.11 `_Generic` and type introspection

`_Generic` selection is performed on the *unqualified, lvalue-converted* type of the controlling expression, which is a rule people get wrong, and the unselected branches are not evaluated but *are* parsed and must be syntactically valid. `__builtin_types_compatible_p` uses the compatibility relation from 7.3, and the kernel uses it heavily to emulate overloading. `__builtin_choose_expr` selects without type-checking the untaken arm, which is the difference between it and the conditional operator and the reason it exists.

## 7.12 Predefined identifiers

`__func__` is declared by the language and not by any header: a function body begins as if `static const char __func__[] = "who";` had been written just inside the brace, so the name is in scope with no declaration in sight and holds the name the definition was written with. gcc adds `__FUNCTION__` and `__PRETTY_FUNCTION__`, which say the same thing in C and differ from it only in C++.

None of the three is a macro, because what it stands for depends on which function is being compiled and the preprocessor does not know that, so the name reaches sema and is answered there. The answer is a string literal with a `const char` element type, which is what makes `__func__[0] = 'x'` the read-only error and leaves everything else to the rules a string already has: it is an lvalue of array type that decays, `sizeof __func__` is the length of the name plus one, and its address is the address of an object with static storage duration. Two mentions of one spelling in one function are one object, and the three spellings are three objects, which is what gcc does and what a program comparing two of them can see.

Outside a function there is no name to answer with. gcc warns and hands back the empty string rather than refusing the program, and that is the answer here as well, because a use out there is meaningless either way and a file that has one still has to build.

## 7.13 What a dialect changes about a return

C89 let a function return without the value it promised and let a function returning `void` return one anyway. C99 removed both, and GCC has since made both errors, but only from C99 onward: at `-std=c89` a missing value is silent and an unwanted one is a warning. That split is kept here, because the code that relies on it is old enough that the reason somebody is compiling it at `-std=c89` is that it does not build any other way.

## 7.14 What sema emits

A typed AST, a symbol table with linkage and storage duration resolved, a list of static initializer images, and diagnostics. It also emits the `--emit=tast` textual form, which prints types explicitly at every node and is the single most useful artifact when an IR bug turns out to be a sema bug.
