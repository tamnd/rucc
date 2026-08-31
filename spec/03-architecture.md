# Architecture

## 3.1 The pipeline

One translation unit flows through seven representations. Each arrow is a total function from one representation to the next, each representation has a textual form that round-trips, and each stage can be run standalone from the command line.

```
source bytes
  → [rucc-cpp]      token stream, macro-expanded      --emit=tokens / -E
  → [rucc-parse]    AST, untyped, with full spans     --emit=ast
  → [rucc-sema]     typed AST, constants folded       --emit=tast
  → [rucc-ir]       SSA IR, target-independent        --emit=ir
  → [rucc-opt]      SSA IR, optimized                 --emit=ir-opt
  → [rucc-codegen]  MIR, virtual regs, target ops     --emit=mir
  → [rucc-codegen]  MIR, physical regs, ABI-lowered   --emit=mir-final
  → [rucc-asm]      assembly text or encoded bytes    -S / --emit=obj
  → [rucc-object]   ELF / Mach-O / COFF               -c
```

The `--emit=` flags are not a debugging afterthought bolted on later. They are the reason the compiler is testable: a bug is localized by bisecting the stage at which the textual form first looks wrong, and every stage's parser means a hand-written or fuzzer-generated input can be injected at any point. Document 15 depends on this entirely.

Two properties are enforced by CI. **Round-trip**: parsing the textual form of stage N and re-printing it produces byte-identical output. **Injection**: feeding a stage's printed output back into stage N+1 produces the same result as running the pipeline straight through. Without the second property the textual forms drift into being lossy approximations of the real data structures, which is how this idea usually dies.

## 3.2 Layers and the dependency rule

Five layers. Dependencies point strictly downward and a CI check in `xtask` fails the build on an upward edge. Every architectural rule that is not mechanically enforced will be violated within a year, so the ones that matter get a test.

```
  L4  driver          rucc, rucc-driver
  L3  pipeline        rucc-cpp rucc-parse rucc-sema rucc-ir rucc-opt
                      rucc-codegen rucc-asm rucc-object rucc-debug rucc-link
  L2  target          rucc-target rucc-abi
  L1  foundation      rucc-source rucc-diag rucc-data
  L0  build tools     rucc-rules (the DSL compiler), rucc-verify (SMT)
```

`rucc-target` sits below the pipeline rather than beside it, which is the decision that makes cross-compilation work: no pipeline crate may ask what machine it is running on. There is exactly one place in the codebase permitted to read `std::env::consts::ARCH`, and it is the driver, deciding the *default* target when `--target` is absent. `xtask` greps for the rest.

## 3.3 Data representation, fixed before any pass is written

These rules exist because axis 3 in document 02 is decided here and nowhere else. They are cheap now and unaffordable later.

**Arenas, not `Box`.** Every AST node, IR instruction and MIR instruction lives in a typed arena owned by the translation unit. Nothing is individually freed; the arena is dropped wholesale when the unit finishes. This removes essentially all allocator traffic from the hot path and removes the destructor storm at the end.

**Indices, not references.** Every reference between nodes is a 32-bit newtype index: `ValueId`, `BlockId`, `InstId`, `TypeId`, `SymbolId`. Four bytes instead of eight, no lifetimes in the data structures, no aliasing problems when mutating the IR during a pass, trivially serializable, and the whole IR is `Send` without effort. The cost is that every access goes through the arena, which is one bounds-checked index into a `Vec`, measurably cheaper than a pointer dereference to a cold cache line.

**Structure of arrays for anything iterated in bulk.** Instruction opcodes live in one `Vec`, operand ranges in another, source spans in a third. A pass that scans opcodes touches one cache line per sixteen instructions instead of one per instruction. Spans in particular are almost never read during optimization and must not sit in the hot struct.

**Interning for identifiers, strings and types.** Identifiers become `Symbol(u32)` at the lexer and are never compared as strings again. Types are interned so that type equality is index equality, which matters enormously in C where the same type is constructed thousands of times. The type interner is per-session and shared across translation units in a single invocation.

**Sorted vectors and dense maps, not `HashMap<String, _>`.** Where a hash map is genuinely needed, it is `FxHashMap` keyed on a `u32` newtype. Where a dense mapping over all values is needed it is a `Vec` indexed directly, which is the common case in a pass.

**No `String` after the lexer.** Source text is memory-mapped and referenced by span. Diagnostics format lazily, at print time, so a warning that is suppressed costs nothing.

## 3.4 No global state, ever

Everything hangs off a `Session`, passed explicitly. No `thread_local!`, no `static mut`, no lazily-initialized global interner. Three reasons: the compiler must be usable as a library, multiple translation units must compile concurrently in one process without interference, and global state makes deterministic output impossible to guarantee.

```rust
pub struct Session<'a> {
    pub opts: &'a Options,          // fully resolved from the command line
    pub target: &'a TargetInfo,     // the target machine, never the host
    pub source: &'a SourceMap,      // files, spans, line tables
    pub diags: &'a DiagCtx,         // emitter, counts, error limit
    pub types: &'a TypeInterner,
    pub symbols: &'a SymbolInterner,
}
```

`DiagCtx` uses interior mutability behind a lock so a `&Session` can be shared across threads while diagnostics are emitted from all of them. Diagnostics carry an ordering key so parallel emission still produces deterministic output; see 3.7.

## 3.5 Parallelism

Two levels, and the second is the one that is unusual.

**Across translation units**, in-process. `rucc a.c b.c c.d` compiles all of them concurrently on a `rayon` pool sharing one `Session`, one type interner and one symbol interner. Existing compilers achieve this only by having the build system fork processes, which pays process startup and re-reads every header per process. This is a large and cheap win on real builds and it is the main reason `Session` is thread-safe rather than a convenience.

**Within a translation unit**, at function granularity, for everything after IR construction. Once the SSA IR for a unit exists, optimization and code generation of each function are independent; only the module-level symbol table is shared, behind a lock touched rarely. The frontend (preprocessing, parsing, semantic analysis) is inherently sequential in C because a `typedef` on line 900 changes how line 901 parses, and we do not attempt to parallelize it.

The `-j` flag controls the pool. `-j1` is deterministic by construction; higher values are deterministic by the rules in 3.7.

## 3.6 No query engine, and why

The obvious modern move is a `salsa`-style demand-driven query system, as `rustc` uses. We are not doing it, and the reasoning should be recorded so it is not relitigated every six months.

A query engine buys incremental recompilation within a translation unit. C's compilation model does not have within-unit incrementality: the build system's unit of work is already one `.c` file, and `make` already skips the ones that did not change. What C actually re-does redundantly is *header processing*, which happens across units, not within one. So the win a query engine would deliver is a win we do not need, and the memoization overhead lands directly on axis 3.

What we build instead is a persistent, content-addressed header cache in the preprocessor, described in document 05. That attacks the redundancy that actually exists.

This decision is revisited only if an IDE integration becomes a goal, and it is not one before 1.0.

## 3.7 Determinism

Byte-identical output for byte-identical input, on every host, at every `-j`. This is not a nice-to-have: it is what makes differential testing, reduction and bisection work at all, and it is what reproducible-build distributions require.

The rules. No iteration over a hash map ever influences output; where an unordered collection must be traversed, it is sorted by a stable key first, and `xtask` has a lint for `.iter()` on `HashMap` in the pipeline crates. No addresses, pointer values or allocation order influence any decision. Parallel work is deterministic because each unit of parallel work writes only to its own slot and results are merged in index order, never in completion order. Diagnostics carry `(file_id, byte_offset, sequence)` and are sorted before printing. Timestamps, hostnames, absolute paths and environment variables are excluded from output unless `-g` requires a path, in which case `-fdebug-prefix-map` applies and `SOURCE_DATE_EPOCH` is honored.

A CI job compiles the corpus twice at `-j1` and `-j16` on two different hosts and diffs the objects.

## 3.8 The error model

Three categories, handled differently, and the distinction is load-bearing.

**User errors**: bad C, bad flags, missing files. These are `Diagnostic` values pushed into `DiagCtx`, never `Result::Err` propagated up. The compiler continues after most errors so that one run reports many problems, and each stage decides its own recovery strategy. After a stage completes, the driver checks the error count and stops before the next stage if it is nonzero, because running semantic analysis over an AST with recovery holes produces cascading nonsense.

**Internal errors**: a broken invariant, a case the compiler cannot handle. These are `panic!` through an `ice!` macro that catches the unwind at the driver boundary and prints a proper ICE report: the flags, the version, the stage, the function being compiled, and instructions to re-run with `-fdump-ice-repro`, which writes a self-contained reproducer (the preprocessed source plus the exact flags) into the current directory. This is the single highest-value debugging feature in the compiler and it costs about two hundred lines.

**Unsupported constructs**: valid C we have not implemented. These are diagnostics with a distinct code prefix (`E9xxx`) so that a corpus run can separate "we are wrong" from "we are incomplete", which are entirely different work items. Document 15's corpus report counts them separately.

There are no `unwrap()` calls on user-influenced data in the pipeline crates, enforced by a clippy configuration in CI. `unwrap()` on a datum that is structurally guaranteed is fine and is written as `expect("invariant: ...")`.

## 3.9 Diagnostics as a product feature

The frontend keeps full spans on everything, including macro expansion stacks, because C's error messages are historically terrible and this is a place where a new compiler can be visibly better for free.

Every diagnostic has a stable code, a primary span, optional secondary spans with their own labels, optional notes, and optional machine-applicable fix-its. Macro-expanded locations print the expansion chain, which is the single most useful thing a C compiler can do and which GCC does poorly. `-fdiagnostics-format=json` emits the structured form for editors. Every diagnostic code has an entry in the error index explaining the rule, with a minimal example.

`-Werror`, `-Wno-`, `-Wall`, `-Wextra` and the individual GCC warning names are honored for compatibility, and unknown `-W` flags warn rather than fail, matching GCC's behavior. Autoconf scripts depend on this.

## 3.10 The build-time layer

`rucc-rules` and `rucc-verify` are compile-time tools, not runtime dependencies. `rucc-rules` reads the rewrite rules in document 09 and the lowering rules in document 10 and generates Rust matcher code, which is compiled into the compiler. `rucc-verify` reads the same rules plus their SMT specifications and discharges them to a solver.

The verification runs in CI, not in the user's build. A contributor without an SMT solver installed can build and test the compiler; they cannot merge a new rule, because the verification job is required. This keeps the solver out of the dependency tree of anyone who just wants to compile C.

Generated code is checked into the repository, regenerated by `cargo xtask codegen`, and CI fails if the checked-in output differs from a fresh generation. This keeps builds fast and hermetic and makes rule changes visible in diffs.

## 3.11 What the driver does

`rucc-driver` is a library; `rucc` is a thin binary around it. Everything the command line can do is available through the library API, with diagnostics delivered to a callback rather than stderr. This costs nothing and means the test harness, the corpus runner and any future editor integration drive the real compiler rather than a subprocess.

The driver's job, in order: parse the command line into a fully-resolved `Options`; resolve the target triple into a `TargetInfo`; expand `@file` response files; decide the phase sequence per input (preprocess, compile, assemble, link) from the file extensions and the `-E`/`-S`/`-c` flags; run the phases with the parallelism from 3.5; and invoke the linker. Document 04 specifies the flag surface.
