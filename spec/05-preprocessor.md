# The preprocessor

The preprocessor is where a third of the compile time goes and where a surprising share of the compatibility bugs live. It gets its own document and its own crate.

## 5.1 Translation phases

The standard's eight phases are implemented as five, with the collapsed ones producing identical observable behavior.

Phase 1 maps physical source bytes to the source character set and handles the byte order mark. We do not implement trigraphs by default; C23 removed them and `-trigraphs` re-enables them for the two projects a decade that need it.

Phase 2 splices lines ending in backslash-newline. This must be done lazily rather than by rewriting the buffer, because spans must still point at real byte offsets in the real file and because rewriting a 60 KB header to remove three backslashes is wasted work. The lexer carries a splice table for the file and adjusts as it crosses one.

Phase 3 decomposes into preprocessing tokens and comments. Comments become a single space. Preprocessing numbers are their own category and are deliberately looser than the numeric constant grammar: `0x1p+3` and `1.2.3` are both valid pp-numbers, and only phase 7 rejects the second.

Phases 4 through 6 are directive execution, macro expansion, escape-sequence conversion and adjacent string concatenation, run as one pass over the token stream.

Phases 7 and 8 belong to the parser and the linker and leave this document.

## 5.2 Lexing performance

The lexer is the single hottest loop in the compiler at `-O0` and is written accordingly.

Files are never read into a `String`. The scanner works on `&[u8]` and only validates UTF-8 inside identifiers, string literals and comments, because the rest of a C file is ASCII by construction and validating it twice is waste. A file large enough for the copy to cost more than the page faults is memory-mapped instead of read, with the crossover measured rather than assumed; below it a plain read wins and mapping every small header would be a pessimisation.

The inner dispatch is a 256-entry table from first byte to token class, which turns the "what kind of token starts here" decision into one load. Whitespace runs, line comments and block comments are skipped with `memchr`-class SIMD searches rather than byte loops; on a heavily-commented header this is worth several times the naive scan. Identifier scanning uses a SIMD classification of the identifier-continue character set, falling back to the scalar path when a byte above 0x7F appears.

Identifiers are hashed during the scan, not after, and interned into `Symbol(u32)` immediately. After the lexer no part of the compiler compares identifier text.

Every token is 16 bytes: kind, `Symbol` or literal index, and a `Span` of file id plus byte offset plus length. Tokens are produced into a `Vec` per file region rather than through an iterator, because the macro expander needs to look backward and forward.

The one measurement that matters for axis 3 is preprocessed lines per second on a header-heavy input, and document 16 makes it a tracked benchmark from M1.

## 5.3 Macro expansion

We implement the standard algorithm with **hide sets**, following Prosser's formulation, rather than the "expansion depth counter" approximation that several small compilers use. The approximation gets the common cases right and produces subtly wrong results on mutually recursive macros, which appear in real code more often than one would hope, and being wrong here is invisible until it is catastrophic.

Each token carries a hide set: the set of macro names that must not be re-expanded at this token. Object-like macro expansion adds the macro's own name to the hide set of every token it produces. Function-like expansion adds the macro's name to the intersection of the hide sets of the macro name token and the closing parenthesis token, then adds the macro's name. The hide set is a small interned bitset rather than a `HashSet` per token, because per-token allocation here is unaffordable.

Argument substitution rules, in order and each individually a known source of bugs: an argument is fully macro-expanded before substitution *unless* it is an operand of `#` or `##`; `#` stringifies with the whitespace rules that collapse internal runs to one space and drop leading and trailing space, escaping `\` and `"` inside string and character literals; `##` concatenates the preceding and following tokens and re-lexes the result, and a result that is not a single valid pp-token is a constraint violation we diagnose rather than accept; placemarker tokens handle empty arguments around `##` so that `a ## b` with empty `b` yields `a` rather than a paste error.

Variadic macros support both the standard `__VA_ARGS__` and the GNU `args...` named form. `__VA_OPT__(x)` from C23 is implemented, and the GNU `, ## __VA_ARGS__` comma-swallowing extension is implemented because a vast amount of existing code uses it and will for another decade.

Rescanning after replacement continues from the start of the replacement list and may consume tokens from *after* the macro invocation, which is why the expander operates on a token stream with pushback rather than on isolated lists.

**Spans through expansion.** Every token produced by expansion carries both its spelling location and its expansion location, plus a pointer into an expansion trace. This is what lets a diagnostic print the chain from the error site up through three nested macros to the user's call, which is the feature that makes C error messages tolerable and which document 03 committed to.

## 5.4 Directives

`#if` expressions are evaluated over `intmax_t`/`uintmax_t` with the usual arithmetic conversions, with undefined identifiers becoming `0` after macro expansion, `defined X` and `defined(X)` handled before expansion, and the pathological case (`defined` produced *by* a macro expansion) handled the way GCC handles it and diagnosed under `-pedantic`, because the standard leaves it undefined and real code does it.

`#include`, `#include_next` and computed includes (`#include MACRO`) are supported. The search order is document 04's. `#include_next` continues from the directory after the one containing the current file, which glibc and the kernel both rely on.

**Multiple-include optimization** is not optional for performance. A file whose entire content is wrapped in `#ifndef GUARD / #define GUARD / ... / #endif` is recorded with its guard symbol, and subsequent includes are skipped without opening the file once the guard is defined. `#pragma once` does the same by file identity, resolved through canonicalized device and inode on Unix and file id on Windows so that symlinked and hardlinked headers are recognized as the same file.

`#define`, `#undef`, `#line`, `#error`, `#warning` (a GNU extension, universally used), `#pragma`, the GNU line marker, and the null directive. Unknown directives are errors, except that unknown `#pragma` is silently ignored per the standard.

The GNU line marker is a `#` followed by a number rather than by a name, and it is read as well as written, because `-E` output handed back to the compiler is full of them. It is `#line` with three differences. Nothing on it is macro expanded, since the tokens came from a preprocessor rather than from a person. Zero is a line number, since a generator counting from zero is allowed to say so. And the flags may follow the file name, of which `3` and `4` say nothing this phase acts on while `1` and `2` are the nesting: a `1` records the file being entered and a `2` returns to one, and a `2` naming a file that is in neither the markers nor the real include stack is ignored with a warning, as GCC ignores it. GCC asks whether the file named is the one directly outside and this asks whether it is anywhere outside, because a marker set that leaves out a return marker is common and the nesting it does describe is still enough to say what a `2` means.

`#line` is applied and not merely recorded. The line it names is the line after the directive, the name it names holds until another directive changes it, and a directive with no name keeps whichever one is in force. What moves is the presented position: `__LINE__` and `__FILE__`, the position a diagnostic prints, and the markers `-E` writes. What does not move is where the bytes are, because the text a caret is drawn under is still read out of the real file at the real offset. `-E` writes a marker for each directive where the directive was written rather than where its effect first shows, which is what GCC does and is not the same place: a file included from below a `#line` is added to the source map after the file that includes it, so ordering the markers by position would put the rename after the header instead of before it.

`_Pragma("...")` is destringized and processed as a directive, including in macro expansion results, which is how `#pragma GCC diagnostic` gets used inside macros. A run of text lines is expanded in one batch, so a line spelling `_Pragma` directly is expanded on its own: `pop_macro` changes what the names after it mean, and a pragma that took effect after the line below it had already been expanded would be a pragma that did nothing.

The `#pragma` set we act on: `GCC diagnostic push/pop/ignored/warning/error`, `GCC poison`, `GCC system_header`, `GCC visibility push/pop`, `GCC push_options/pop_options/optimize/target`, `pack(push,n)`/`pack(pop)`/`pack(n)` including the MSVC spellings, `once`, `push_macro("NAME")`/`pop_macro("NAME")`, `weak`, and `STDC FP_CONTRACT/FENV_ACCESS/CX_LIMITED_RANGE`. Everything else is ignored with a note under `-Wunknown-pragmas`.

`#embed` from C23 is implemented with its `limit`, `prefix`, `suffix` and `if_empty` parameters. It is genuinely useful, it is cheap to implement, and implementing it means the resource-embedding hack of generating a C array with a script goes away. The implementation reads the file once and produces an integer token sequence directly, with a fast path in the parser that recognizes an `#embed` initializer and fills the array without materializing millions of tokens. The naive implementation is unusably slow on a one-megabyte file and this is the known trap.

The `__has_*` family is implemented: `__has_include`, `__has_include_next`, `__has_embed`, `__has_attribute`, `__has_c_attribute`, `__has_builtin`, `__has_feature`, `__has_extension`. These must be correct rather than optimistic. A project that asks `__has_builtin(__builtin_foo)` and gets `1` from a compiler that does not have it fails in a much more confusing way than one that gets `0`. Document 13's matrix is the source of truth for these answers and the matrix is machine-readable so the answers and the implementation cannot drift. They are builtin macros and not conditional-expression syntax, so they answer in ordinary text as well as in a `#if`, which is what both GCC and clang do. The three that take a header name are the exception: outside a directive the line has already been scanned as ordinary tokens, so `<stdio.h>` is a run of comparisons rather than a header name, and both compilers make that an error rather than guessing.

## 5.5 The header cache

This is the performance idea in this document and it is the one that attacks the redundancy that actually exists in C builds: the same fifty headers are preprocessed identically in each of two hundred translation units.

**The observation.** Processing a header is a pure function of the file's contents, the include search path, and *the subset of the macro state that the header actually reads*. That last qualifier is what makes caching viable: `<stdio.h>` reads a few dozen feature-test macros and is oblivious to the other four thousand macros defined in a real translation unit.

**The mechanism.** While preprocessing a file, record every macro name queried (by `#ifdef`, `#if defined`, expansion, or `#undef`) together with the definition observed. On completion, the cache entry is keyed by the hash of the file's content, the resolved absolute path, and the sorted list of (queried name, definition hash) pairs. The value is the produced token stream, the set of macro definitions and undefinitions the file performed, and the list of files it included, recursively.

On a later include, we compute the same key against the current macro state; on a hit we splice in the recorded token stream and apply the recorded definitions without opening the file.

**The soundness condition** is that the recorded query set is complete: every way a header's behavior can depend on preprocessor state must be captured. The known channels are macro queries, `__has_include` results, `__COUNTER__`, `__LINE__`, `__FILE__`, `__DATE__`, `__TIME__`, `#pragma once` state and the include-guard set. Files touching `__COUNTER__`, `__DATE__` or `__TIME__` are simply not cached. `__LINE__` and `__FILE__` are position-dependent but position-stable within a file and are handled by recording the offset base rather than by disabling the cache.

**The safety discipline.** This is a cache that can silently miscompile if the dependency capture is incomplete, which is exactly the failure mode of Clang's implicit modules. Three defences: `-fno-header-cache` disables it entirely and is what a bug report is asked to try first; a CI job compiles the whole corpus with and without the cache and diffs the preprocessed output byte for byte; and the cache is disabled by default in M2 and M3, enabled by default only when that CI job has been green across the full corpus for a full milestone.

**Storage.** In-process for the duration of a multi-file invocation, always. On disk under `$XDG_CACHE_HOME/rucc` when `-fheader-cache-dir=` or the default persistent mode is active, content-addressed, with a size cap and LRU eviction. The on-disk form is the same serialization the in-process form uses, which is straightforward given the index-based data representation in document 03.

**Expected win**, to be confirmed rather than assumed: the redundant work is real and large, and document 16 makes "total wall time to build SQLite from a clean tree" a headline benchmark precisely because it is the number this feature moves. Document 19 records the risk that the key computation itself costs more than the parse it saves on small headers; the mitigation is a size threshold below which caching is skipped.

## 5.6 `-E` output fidelity

`-E` output must be usable as input, and must be *diffable against GCC's*, because that diff is the fastest way to find a preprocessor bug.

Line markers follow GCC's format exactly, including the `1`, `2`, `3` and `4` flags for entering a file, returning from a file, system header, and extern "C" respectively. Whitespace is preserved to the degree GCC preserves it: original line structure is maintained, and a space is inserted between tokens that would otherwise paste into a different token. `-P` suppresses line markers, `-C` retains comments, `-CC` retains them through macro expansion, `-dM` prints the final macro set, `-dD` prints definitions in place, `-dI` retains `#include` directives.

The CI job that diffs our `-E` output against GCC's over the corpus is one of the highest-value tests in document 15, because preprocessor bugs otherwise surface as inexplicable parse errors thousands of lines away.

## 5.7 Diagnostics specific to this stage

Unterminated conditionals report the location of the opening `#if`, not the end of the file. Unterminated comments and string literals report the opening. A macro invocation with the wrong argument count reports both the invocation and the definition. A `##` producing an invalid token shows both operands. An `#include` that fails prints the full search path that was tried, in order, which is the single most requested diagnostic in this area and which GCC does not do well.

`-Wundef` warns on undefined identifiers in `#if`, which is off by default because the entire ecosystem relies on the behavior, and on by default under `-Wextra`.
