# The parser

## 6.1 From preprocessing tokens to tokens

The boundary between document 05 and this one is phase 7. Preprocessing tokens become tokens: identifiers matching the keyword set for the active `-std=` become keywords, preprocessing numbers are converted to typed integer or floating constants, character constants and string literals have their escapes resolved and their encoding prefixes applied, and adjacent string literals are concatenated.

Keyword recognition is a lookup in the interner performed once at intern time, not a string comparison: `Symbol` values below a fixed threshold are keywords, and which of them are *active* depends on the dialect. `restrict` is a keyword in C99 and later and an ordinary identifier in C89; `typeof` is a keyword in C23 and an identifier before it, while `__typeof__` is always a keyword. This dialect gate is a bitmask test on the symbol, not a table lookup.

Numeric conversion is where a compiler quietly loses correctness. Integer constants are parsed into `u128` with overflow detected rather than wrapped, and the type is selected by the standard's table walk over the candidate list for the given suffix and base, with a diagnostic when no type fits. Floating constants are parsed with a correctly-rounded decimal-to-binary conversion, not `strtod` on the host, which would make output depend on the host's libc and break determinism, and not a naive `mantissa * 10^exp`, which is wrong in the last bit for a meaningful fraction of literals. Hexadecimal floating constants are exact by construction and are the easy case. C23 digit separators are stripped during conversion. `_BitInt` suffixes (`wb`, `uwb`) select the exact-width types.

Character and string literals handle the `u8`, `u`, `U` and `L` prefixes, with `L` mapping to the target's `wchar_t`, which is 32-bit on Linux and 16-bit on Windows, a difference that must come from `TargetInfo` and not from the host.

## 6.2 The AST

Arena-allocated, index-referenced, following document 03. Three arenas per translation unit: expressions, statements, declarations. Every node is at most 32 bytes; anything larger is an index into a side table.

Spans are stored out of line, in a parallel `Vec` indexed by node id. Almost no consumer of the AST reads spans, and keeping them out of the node makes the arrays that *are* scanned twice as dense.

The AST is deliberately close to the source. It is not desugared: `for` loops stay `for` loops, compound assignment stays compound assignment, array subscripting is not turned into pointer arithmetic. Desugaring happens at IR construction in document 08. Keeping the AST faithful is what makes `--emit=ast` useful for debugging and what makes source-level diagnostics accurate; a desugared AST produces error messages about code the user did not write.

## 6.3 Recursive descent, with a Pratt expression parser

Statements and declarations are recursive descent, one function per grammar production, which is readable and gives precise control over error recovery. Expressions are a Pratt parser over a precedence table, which handles C's fifteen levels in one loop instead of fifteen mutually recursive functions and is measurably faster.

Lookahead is bounded. The parser has a token buffer supporting `peek(n)` for small *n* and a save/restore for the two constructs that genuinely need speculation, described below. There is no general backtracking, because unbounded backtracking is how a parser becomes quadratic on adversarial input and how a fuzzer finds a compile-time bomb.

## 6.4 The typedef problem

C's grammar is ambiguous without knowing which identifiers are type names: `(A)*B` is a cast if `A` is a type and a multiplication if it is not. This is the central structural problem in parsing C and the choice of solution shapes everything.

**We track scopes in the parser and resolve the ambiguity there.** The parser maintains a scope stack mapping `Symbol` to one of *typedef name*, *ordinary identifier*, or *not declared*, updated as declarations are parsed. There is no feedback channel to the lexer, which is the traditional approach and which makes lookahead and error recovery painful because the lexer's state depends on how far the parser has got.

The specific hazards this must get right, each of which is a real bug in real compilers:

A declaration's declarator introduces its name into scope *at the end of the declarator*, so `typedef int T; void f(int T, T x);` has `T` as a parameter name and `T x` is then an error, while `typedef int T; T T;` declares a variable `T` of type `T`.

Inside a function's parameter list, a declaration scope is opened that closes at the end of the declarator, unless the function has a body, in which case parameters live in the function's block scope.

`struct`, `union` and `enum` tags occupy a separate namespace from ordinary identifiers, as do labels, as do struct members. Four namespaces, and conflating them produces wrong parses of legal code.

A `typedef` name can be shadowed by an inner declaration, and re-exposed when the inner scope closes.

The two places bounded speculation is still required. **Ambiguity between a compound literal and a parenthesized cast**: `(T){...}` versus `(expr)` followed by a block; resolved by peeking past the closing parenthesis for `{`. **Old-style function definitions**: `int f(a, b) int a; int b; {...}` is distinguished from a modern prototype by what follows the closing parenthesis. K&R definitions are still required. The kernel does not use them but a great deal of older code in any real corpus does, and `-std=c23` makes them an error while `gnu17` and earlier accept them.

## 6.5 Declarators

Declarator parsing is the part of a C parser that is genuinely hard, because C's declaration syntax is inside-out: in `int (*f[3])(char)`, the type is built by reading outward from the name. The standard approach, build a chain of type constructors while descending and apply them in reverse on the way out, is what we do, with the chain kept in a small stack-allocated vector rather than by recursion, so that pathological nesting does not overflow the stack. Depth is capped with a diagnostic at 200, matching GCC's spirit if not its exact number.

Abstract declarators, where the name is absent, share the same code path with a flag, because duplicating it is how the two drift apart.

Array declarators carry a size expression that may be a constant, `*` for a variably-modified type in a prototype, or an arbitrary expression for a VLA. The `static` and qualifier syntax inside array parameter brackets (`int a[static restrict 4]`) is parsed and carried, because it affects diagnostics and can inform alias analysis even though it does not change the ABI.

Function declarators distinguish four forms with different semantics: a prototype with named or abstract parameters, `(void)` for explicitly no parameters, `()` which in C23 means the same as `(void)` and before C23 means unspecified, and the old-style identifier list. The C23 change to `()` is a real behavioral difference that projects hit when moving to `gnu23`, and we diagnose the cases where it matters under a dedicated warning.

## 6.6 C23 syntax

Implemented in full, because the default dialect is `gnu23`:

`[[attribute]]` syntax with the standard attributes `deprecated`, `fallthrough`, `maybe_unused`, `nodiscard`, `noreturn`, `unsequenced` and `reproducible`, plus vendor-namespaced ones like `[[gnu::packed]]`. Attribute *placement* is part of the grammar and appears in a dozen positions; getting the positions right matters because the meaning of an attribute depends on what it appertains to.

`typeof` and `typeof_unqual`. `auto` as a type specifier for inferred types, which coexists with `auto` as the ancient storage class specifier and is disambiguated by whether another type specifier is present. `constexpr` on objects, with the initializer evaluated by the constant evaluator in document 07. `nullptr` and `nullptr_t`. `true`, `false` and `bool` as keywords. `static_assert` with an optional message. `enum` with an explicit underlying type, and enumerators of type `unsigned long long` or `_BitInt`. `_BitInt(N)` as a first-class type. Binary literals and digit separators. `u8` character constants. Empty initializer braces `{}`. Unnamed parameters in definitions. Labels before declarations and at the end of compound statements. `__VA_OPT__`, covered in document 05.

C2y features, including `defer`, are parsed behind `-std=gnu2y` only, and document 19 tracks the decision on whether to ship any of them before 1.0.

## 6.7 GNU syntax

The syntactic extensions, as opposed to the builtins and attributes catalogued in document 13:

Statement expressions `({ ... })`, whose value is the last expression statement. These interact badly with everything: they can appear in initializers of static objects, they can contain `return`, they can contain VLAs, and each interaction needs a rule rather than an accident.

`__attribute__((...))` with GCC's placement rules, which are baroque and inconsistent and must be matched rather than rationalized: before a declaration, after the declaration specifiers, after a declarator, after a struct or enum body, on a label, on a parameter. GCC's own documentation admits ambiguity in some positions; where it is ambiguous we match GCC's observed behavior and record the case in a test.

`asm` statements in GCC's full form: output operands with constraints and modifiers, input operands, clobbers, `volatile`, `inline`, and `goto` with a label list. `asm goto` **with outputs** is required by the kernel and is implemented. Top-level `asm` at file scope. The constraint language itself is target-specific and is specified in document 11.

Case ranges `case 1 ... 5:`. Labels as values, `&&label` and `goto *p`, required by Postgres's expression interpreter and used in the kernel. Local labels via `__label__`. Designated initializer ranges `[0 ... 9] = x` and the obsolete `field: value` form. The omitted-middle conditional `x ?: y`. `__extension__` to suppress pedantic diagnostics on a subexpression. Empty structures. Arithmetic on `void *` and on function pointers, with a size of 1. Zero-length arrays as the pre-C99 flexible array member. Anonymous struct and union members, which are C11 but were a GNU extension long before. Casts to union type. Transparent unions. Nested functions are **not supported**; they require trampolines on the stack, the kernel does not use them, and GCC itself has been trying to deprecate them. A clear diagnostic saying so is better than a bad implementation.

`__builtin_offsetof`, `__builtin_choose_expr`, `__builtin_types_compatible_p` and `__builtin_va_arg` are syntax rather than functions, their operands are types or unevaluated, and so they are parsed here rather than treated as calls. `__builtin_choose_expr` in particular must not type-check the untaken branch, which is the entire reason it exists.

## 6.8 Error recovery

The goal is many useful errors per run without cascades, and the strategy is per-construct rather than global.

At statement level, on an unexpected token, skip to the next `;` or `}` at the current brace depth and resume. At declaration level, skip to the next `;` at file scope. In an expression, insert an error node and continue at the current position rather than skipping, because expressions are short and skipping loses the rest of the statement.

Brace, bracket and parenthesis matching uses the indentation and the token stream together: an unclosed `{` reports the opening location and guesses the intended closing point from indentation, which is how modern compilers avoid the "500 errors at end of file" failure.

Every recovery inserts a poisoned node. Poisoned nodes suppress downstream diagnostics that mention them, which is the mechanism that actually prevents cascades, not error counting, not a "we already reported an error here" flag.

After 20 errors, the compiler stops with a note, matching GCC's `-fmax-errors` default behavior. `-fmax-errors=0` removes the limit.

## 6.9 Testing this stage

Three test families, all in document 15 and all enabled by the round-trip property in document 03.

**Round-trip**: for every file in the corpus, parse, print the AST's textual form, re-parse, and compare the two ASTs structurally. This finds both printer bugs and parser state bugs.

**Differential parse**: for every file in the corpus, compare our accept/reject decision against GCC's and Clang's. A file all three reject is uninteresting; a file we reject and they accept is a bug in us, and a file we accept and they reject is usually also a bug in us.

**Fuzzing**: the parser is fed both random bytes and structure-aware mutations of corpus files, with the invariants that it never panics, never allocates unboundedly, and never takes superlinear time in input length. The last of these has caught quadratic behavior in every C parser that has been fuzzed for it.
