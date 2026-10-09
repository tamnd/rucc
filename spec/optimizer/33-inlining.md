# 33. Inlining

Replace a call with the callee's body. It is the only interprocedural transformation that most
programs need, it is the enabler for every intraprocedural pass in documents 14 through 31, and its
entire difficulty is a single question that has no correct answer: which calls.

`gcc/ipa-inline.cc` is 3,450 lines of decision making. `gcc/ipa-fnsummary.cc` is 5,343 lines
computing the numbers the decisions consume. `gcc/ipa-inline-transform.cc` is 884 lines of callgraph
surgery, `gcc/ipa-inline-analysis.cc` 599 more, and the actual body-copying lives in
`gcc/tree-inline.cc`, 6,800 lines. Call it 17,000 lines, of which the part that does the work is
maybe 15%.

## 33.1 Two inliners, and why

`gcc/ipa-inline.cc:20` explains the split, and it is the most useful piece of architecture in the
area.

**`pass_early_inlining`** is described at `gcc/ipa-inline.cc:37` as a "Simple local inlining pass
inlining callees into current function. This pass makes no use of whole unit analysis and thus it can
do only very simple decisions based on local properties." Its power comes from ordering:

> The strength of the pass is that it is run in topological order (reverse postorder) on the
> callgraph. Functions are converted into SSA form just before this pass and optimized subsequently.
> As a result, the callees of the function seen by the early inliner was already optimized and
> results of early inlining adds a lot of optimization opportunities for the local optimization.

So the early inliner is not really an optimization. It is a *preparation* pass whose purpose is to
make the callee's already-optimized body visible inside the caller before the caller is analysed, so
that every subsequent analysis sees through the call. The source is candid about its limits: "Because
of lack of whole unit knowledge, the pass cannot really make good code size/performance tradeoffs."
It compensates by allowing growth of `early-inlining-insns` when the callee is a leaf, on the
reasoning that "the optimizations performed later are very likely to eliminate the cost."

**`pass_ipa_inline`** is the real one, and the header lists its three steps at
`gcc/ipa-inline.cc:69`: greedy inlining of small functions ordered by badness; removal of functions
that became unreachable, since "Inlining leads to devirtualization and other modification of callgraph
so functions may become unreachable during the process"; and inlining of functions called exactly
once and not exported, "This should almost always lead to reduction of code size by eliminating the
need for offline copy of the function."

That third step is the one worth noticing. A static function with one call site should always be
inlined, because inlining it deletes the original. It is free, it is easy to detect, and it is the
highest-value inlining decision in ordinary C, where `static` helper functions called from one place
are everywhere. GCC enables it at `-O1` and above: `OPT_LEVELS_1_PLUS_NOT_DEBUG` sets
`-finline-functions-called-once` at `gcc/opts.cc:629`.

## 33.2 The two questions: can, and want

The code separates legality from desirability rigorously, and the separation is worth copying
verbatim.

`can_inline_edge_p` (`gcc/ipa-inline.cc:371`) and `can_inline_edge_by_limits_p` (527) answer whether
inlining is *permitted*: the body is available, the calling conventions match, the target attributes
are compatible, the optimization flags do not conflict, no `noinline`, no `always_inline` violation.
`can_early_inline_edge_p` (693) is the early inliner's stricter version.

`want_inline_small_function_p` (969), `want_inline_self_recursive_call_p` (1109) and
`want_inline_function_to_all_callers_p` (1252) answer whether it is *desirable*.

The failure reasons are an enumerated type: `gcc/cif-code.def` defines 32 `DEFCIFCODE` entries, each
a distinct reason a call was not inlined, each with a message the user can see through
`-fopt-info-inline-missed`. This is the same pattern documents 08.5 and 25.3 identified: **the
analysis returns "no, because `<reason>`", never a bare boolean.** Here it is done at the largest
scale in the compiler, and the payoff is that "why was this not inlined" is answerable without a
debugger.

rucc takes the same shape: `enum InlineFailure` with a variant per reason, surfaced by
`-fopt-info-inline` and asserted on in tests. Getting this right at the start costs nothing; adding
it later means threading a reason through fifty return statements.

## 33.3 Summaries, and the idea worth stealing

`gcc/ipa-fnsummary.cc:20` describes what is computed per function: body size, "size after specializing
into given context", average execution time in a given context, and frame size; and per call: the
call statement's size and time and "how often the parameters change".

The idea that makes this more than a line count is at line 36:

> The summaries are context sensitive. Context means
> 1) partial assignment of known constant values of operands
> 2) whether function is inlined into the call or not.
> ... To represent function size and time that depends on context (i.e. it is known to be optimized
> away when context is known either by inlining or from IP-CP and cloning), we use predicates.

**Predicated size and time.** Rather than one number per function, each basic block's size and time
is tagged with a predicate over the parameters, and the summary is evaluated against the actual
argument values at a call site. A function that is 200 instructions in general but 12 instructions
when its first argument is `NULL` has both facts recorded, and a call site passing `NULL` sees the
12.

This is the single most valuable idea in the inliner and it is what makes `estimate_edge_growth`
(`gcc/ipa-inline.h:94`) meaningfully different from "size of callee minus size of call". It is also
what document 29.2 independently arrived at for unrolling: **cost must be estimated after the folding
that the transformation enables**. Two unrelated parts of GCC discovered the same requirement, which
is strong evidence it is a general principle and not a local trick.

The predicates are also what connect this document to document 34's interprocedural constant
propagation: IPA-CP computes which arguments are constant, the fnsummary predicates say what that is
worth, and the inliner and the cloner both consume the product.

Two more summary details worth recording. `uninlined-function-insns` `Init(2)`
(`gcc/params.opt:1230`) is the charge for a call's prologue, epilogue and overhead, so the model knows
that removing a call saves something even when the body is copied verbatim.
`estimate_min_edge_growth` (`gcc/ipa-inline.h:86`) gives a cheap lower bound used to reject candidates
before the expensive context-sensitive evaluation, which is how the quadratic is kept in hand.

## 33.4 The badness function

`gcc/ipa-inline.cc:1288` states the framing: "A cost model driving the inlining heuristics in a way
so the edges with smallest badness are inlined first. After each inlining is performed the costs of
all caller edges of nodes affected are recomputed so the metrics may accurately depend on values such
as number of inlinable callers of the function or function body size."

So it is a priority queue, a Fibonacci heap of edges keyed by badness, with recomputation after every
decision. That recomputation is the expensive part and it is also what makes the heuristic work: the
value of inlining a function depends on how many callers it still has.

`edge_badness` at 1295 has four cases.

**Growth is zero or negative.** Badness is set to an extreme negative value: "Always prefer inlining
saving code size." An inline that shrinks the program is unconditionally taken, no cost model
consulted. This is the correct default and it covers the called-once case, tiny wrappers, and
functions whose body is smaller than the call sequence.

**The caller is `DECL_EXTERNAL`.** Badness is `sreal::max()`, never inlined: "Inlining into EXTERNAL
functions is not going to change anything unless they are themselves inlined."

**A profile or a branch-probability guess is available.** The formula, quoted from the comment at
1348:

```
            time_saved * caller_count
goodness =  -------------------------------------------------
            growth_of_caller * overall_growth * combined_size

badness = - goodness
```

Every term earns its place. `time_saved` comes from the predicated summaries, so it already accounts
for folding. `caller_count` weights hot call sites. `growth_of_caller` is the direct size cost.
`combined_size` is `caller size + growth`, which penalises inlining into functions that are already
large, since those are the ones that stop being inlinable themselves. And `overall_growth` is the
size increase across the *whole program* if this callee is inlined everywhere, which is what
distinguishes a function with one caller from a function with fifty.

Then a non-obvious refinement at 1430: when `overall_growth` is under 256 it is *squared*, with the
comment "Strongly prefer functions with few callers that can be inlined fully. The square root here
leads to smaller binaries at average. Watch however for extreme cases and return to linear function
when growth is large." A nonlinearity, justified by a measurement, with a documented fallback to
linear at the extreme. That is what a tuned heuristic looks like and it is worth showing to anyone
who thinks cost models are principled.

There is also a **wrapper penalty**, at 1379 through 1428, for the shape

```c
inline_caller () { do_fast_job...; if (need_more_work) noninline_callee (); }
```

where inlining the rarely-taken `noninline_callee` into the wrapper makes the wrapper too big to
inline itself. The condition is five clauses deep: growth exceeds overall growth, the callee has a
single caller, the caller is not itself inlined, the edge frequency is below one, and either the
callee is not declared inline while the caller is, or the callee is a split part whose entry
probability is below `partial-inlining-entry-probability`. This is a heuristic patching a known
regression, it is documented as such, and it is exactly the kind of thing that only exists because
somebody measured a real program.

**No profile and no useful frequency.** Badness is growth, shifted right by the loop depth of the
call site, capped at depth 8. A call inside two loops is 4 times more attractive than the same call
at top level. Three lines, and on code without profile data it does most of the work.

## 33.5 The hints

`gcc/ipa-fnsummary.h:30` defines eight, and they modify badness multiplicatively at
`gcc/ipa-inline.cc:1487` onward by shifting:

| Hint | Meaning | Shift |
|---|---|---:|
| `indirect_call` | specialization turns an indirect call direct | 2 |
| `loop_iterations` | inlining makes a trip count known | 2 |
| `loop_stride` | inlining makes a stride known | 2 |
| `builtin_constant_p` | a `__builtin_constant_p` depends on a parameter | 4 |
| `declared_inline` | the user wrote `inline` | 3 |
| `known_hot` | profile says hot | (via badness numerator) |
| `same_scc` | caller and callee in the same recursion cycle | -3 |
| `in_scc` | callee is in some cycle | -2 |
| `cross_module` | different translation units, LTO only | -1 |

Plus: recursive edges are penalised by 4, and `DECL_DISREGARD_INLINE_LIMITS`, meaning
`always_inline`, is favoured by 4.

The first three hints are the compiler recognising that **inlining's value is mostly in what it
enables, not in the call it removes**. Making a trip count known lets document 29 unroll; making an
indirect call direct lets everything downstream see a callee at all. The `builtin_constant_p` hint
gets the largest shift because that idiom's entire purpose is to select a specialization, which is
document 14.4's concern.

The `same_scc` and `in_scc` penalties encode "Inlining within same strongly connected component of
callgraph is often a loss due to increased stack frame usage and prologue setup costs."

Separately, `want_inline_small_function_p` at `gcc/ipa-inline.cc:1021` scales the *size limits* by
hints rather than the badness: `inline_insns_single` (470) multiplies
`max-inline-insns-single` by `inline-heuristics-hint-percent` when one hint group fires, and by that
percentage squared over 100 when both fire, capped at 10,000x. At `-O2` with both groups that is
400x the limit; at `-O3`, where the percentage is 600, it is 3,600x. So the hints do not nudge, they
suspend the limits.

## 33.6 The parameters, and what the levels actually change

The full set, from `gcc/params.opt`:

| Parameter | Line | Default | `-O3` |
|---|---:|---:|---:|
| `early-inlining-insns` | 149 | 6 | 14 |
| `inline-heuristics-hint-percent` | 236 | 200 | 600 |
| `inline-min-speedup` | 240 | 30 | 15 |
| `inline-unit-growth` | 244 | 40 | |
| `large-function-insns` | 408 | 2700 | |
| `large-function-growth` | 404 | 100 | |
| `large-unit-insns` | 420 | 10000 | |
| `large-stack-frame-growth` | 416 | 1000 | |
| `max-early-inliner-iterations` | 593 | 1 | |
| `max-inline-insns-auto` | 633 | 15 | 30 |
| `max-inline-insns-single` | 645 | 70 | 200 |
| `max-inline-insns-size` | 649 | 0 | |
| `max-inline-insns-small` | 653 | 0 | |
| `max-inline-insns-recursive` | 637 | 450 | |
| `max-inline-recursive-depth` | 657 | 8 | |
| `min-inline-recursive-probability` | 857 | 10 | |
| `max-inline-functions-called-once-insns` | 629 | 4000 | |
| `max-inline-functions-called-once-loop-depth` | 625 | 6 | |
| `uninlined-function-insns` | 1230 | 2 | |

Two of these have **no initialiser**, so they are zero: `max-inline-insns-size` and
`max-inline-insns-small`. That is not an oversight. `growth <= param_max_inline_insns_size` at
`gcc/ipa-inline.cc:1035` therefore means "growth is zero or less", which is the free-inline
fast path; and at `-O1`, where `-finline-functions` is off, the
`growth >= param_max_inline_insns_small` test at 1055 is always true, so every non-declared-inline
callee falls into the branch that requires `!growth_positive_p`. **`-O1` inlines only what is free.**

**And now the claim document 03.2 made, discharged.** The `-O3` list at `gcc/opts.cc:702` enables
thirteen flags and changes five parameters, and all five parameters are inliner parameters. Of the
thirteen flags, one is `-fgcse-after-reload`, one is `-fipa-cp-clone`, one is `-fsplit-paths`, one is
`-ftree-partial-pre`, one is `-fversion-loops-for-strides`, and seven are the loop restructuring
transformations document 30 declined. The vectorizer is *not* among them: `-ftree-loop-vectorize`
and `-ftree-slp-vectorize` are already on at `-O2` (`gcc/opts.cc:691`), and `-O3` only upgrades the
cost model from `very-cheap` to `dynamic` (714).

So the difference between `gcc -O2` and `gcc -O3` is, in order of likely effect on ordinary C: a much
braver inliner, a less conservative vectorizer cost model, and a set of loop transformations that
document 30.8 predicts are worth low single digits. **Document 03.2's claim is correct**, and its
practical consequence is large: rucc's `-O3` can be mostly a parameter table, and the expensive
things in documents 30 through 32 are optional extras rather than the definition of the level.

## 33.7 What rucc builds

Inlining is in M4 and it is one of the higher-priority items in it, because everything else in this
directory works better after it.

**One inliner, run twice**, following GCC's split but without duplicating the machinery.

*Pass one, early, at `-O1` and above.* Bottom-up over the callgraph in reverse postorder of the
condensation, so each function is optimized before its callers see it. Only free or nearly free
decisions: growth at most `early-inlining-insns` (6, 14 at `-O3`) and only for leaf callees; plus
`always_inline`, which is a legality obligation and not a heuristic. Recursion is handled by
processing strongly connected components as units and not inlining within them.

*Pass two, at `-O2` and above.* The badness heap of 33.4. Recomputation of affected edges after each
decision. The unit growth bound.

**The summary, with predicates.** This is the part that must not be skipped. A per-function summary
of size and time, where each block carries a predicate over the parameters, evaluated at each call
site with the known-constant arguments. Without it, the cost model is a line count and the inliner
will decline exactly the calls most worth inlining, which are the ones where an argument constant
collapses the body. Perhaps 600 lines, and it is shared with document 34.

**The badness function**, in document 40 with everything else, in the profile-guess form of 33.4's
third case since M4 has no profile data per document 11.5. The loop-depth shift of the fourth case
is the fallback where even branch probabilities are absent.

**The failure enum**, per 33.2.

**What rucc does not build in M4:** recursive inlining as loop unrolling, which is
`recursive_inlining` at `gcc/ipa-inline.cc:1773` plus four parameters, and which pays off on a narrow
set of programs; function splitting, which is document 34's; and cross-module anything, which is
document 35's. Also not built: `flatten_function` (2514), which implements the `flatten` attribute,
though the attribute must still be accepted and warned about if ignored, per document 08.4's
exhaustiveness rule.

Estimated size: 400 lines of decision, 600 of summary, 800 of the actual body copying and callgraph
update. The body copying is not trivial in rucc's IR, but it is much less than GCC's 6,800 lines
because there is one IR, no `gimplification` to redo, and block parameters mean the return value
plumbing is an edge argument rather than a phi insertion.

**One structural note in rucc's favour, and it is the fifth item in the block-parameter tally.**
Inlining a callee with multiple `return` statements requires a merge point holding the returned
value. With phi nodes, that is a phi inserted in a block created for the purpose, and the caller's
existing phis in the successor must be updated. With block parameters, the callee's returns become
`BlockCall`s to a continuation block that takes the value as a parameter, which is what the
representation already does for every other join. The transformation is: rename the callee's blocks
into the caller, replace each `ret v` with `br continuation(v)`, replace the call with a jump to the
callee's entry, and split the caller's block at the call site. No phi surgery anywhere.

**What is built so far.** The `always_inline` half of pass one, in `rucc_opt::inline`, which runs first in the pipeline at every level including `-O0`, since gcc honours the attribute there too. It settles each callee before its callers, leaves a call inside a cycle alone, and splices the body in the way the paragraph above describes. It refuses a callee that takes a structure by value, calls `va_start`, takes a label's address, calls `setjmp` or has been put in memory SSA form, and each refusal is a missed remark under `-fopt-info` that names the reason. It also fills in `__builtin_va_arg_pack` and `__builtin_va_arg_pack_len`, which only mean anything while inlining. The pack becomes the anonymous arguments of the call being inlined, passed on to the call in the body that the pack was the last argument of. Under the x86-64 System V convention a structure goes in registers whole or not at all, so one that fitted in the outer call but does not fit in the inner one, because the inner call has used more registers by then, is stored to a slot in the caller and passed in memory, which is what gcc does. A small structure that went to memory in the outer call and would fit in the inner one is the case still refused. A copy of a function that still holds either builtin after the pass, because some call to it was refused, becomes a declaration, so that call goes to the library's function of the same name. From `-O1` up the same step also inlines a call to a function declared `inline` whose body, with its own calls already settled, is no larger than `INLINE_INSNS_SINGLE` instructions, which is gcc's `max-inline-insns-single` of 70, or 200 at `-O3`. It counts IR instructions rather than gcc's estimate, so the same body comes out larger here and the limit is the careful side of gcc's. A callee declared `inline` is also refused when it calls `alloca` with a size only known at run time, as gcc refuses it. `-fno-inline` turns this half off and leaves `always_inline` alone. The entry block of the copy takes the call's arguments directly rather than as block parameters, so a constant argument is a constant in the body for the first `fold`, which is what lets `__builtin_constant_p` in an inlined body answer one for it. A body that takes the address of its own label is copied with the label, so each copy has an address of its own, and one that jumps through such an address, or whose labels a static table names, is refused. From `-O1` up the same step inlines the one call to a `static` function that nothing reaches any other way, which is the called-once rule of 33.1 and gcc's `-finline-functions-called-once`, up to `INLINE_CALLED_ONCE_INSNS` instructions, gcc's `max-inline-functions-called-once-insns` of 4000. Another way to reach it is a second call, a tail call, an address taken by an instruction or written into an image, or an alias, and `used`, `noinline`, `optnone` and `naked` each keep it a call. Nothing refers to the out of line copy after that and it is not emitted, so the program loses the call and grows by nothing (#1932). A call more than `INLINE_CALLED_ONCE_LOOP_DEPTH` loops deep stays a call, which is gcc's `max-inline-functions-called-once-loop-depth` of 6, counted the way gcc counts, so a call in one loop is one deep, and `-fopt-info-missed` says so. `-fno-inline-functions-called-once` turns the called-once rule off on its own and leaves the `inline` hint and `always_inline` as they are (#1966). The same round copies a `static` function called from more than one place into all its callers when that leaves the program no larger, which is gcc's `want_inline_function_to_all_callers_p`: it runs after the called-once round, so a body is weighed with what it called once already in it, and each copy costs the body as the constants its call passes leave it, read off the summary of 33.3 and weighed as gcc's `estimate_num_insns` weighs it, less the call, which is one and one more for each argument and the result, and the copies together may come to no more than the body, which goes once they are in. Counted one to an instruction, a wrapper that passes six arguments on looked smaller than the call of it and SQLite grew by two percent at `-O1`. gcc copies none of the calls unless it can copy all of them, so one in the callee itself, in an `optnone` function, in one built for less than the callee, in another function this round may copy or more than `INLINE_CALLED_ONCE_LOOP_DEPTH` loops deep keeps every call. A `static` function with a cheap arm a mode picks and forty lines of tail, called from three places of which two pass the mode, goes into all three at `-O1` and is dropped, as with gcc 16, where before it stayed three calls (#3150). A conditional branch weighs two in that estimate, as gcc's holds its compare, and a branch on the join the front end makes of `a && b` weighs one, since gcc branches on the second test straight away, so a `static` function of four tests called from five places stays five calls at `-O1`, as with gcc 16, where at one to a branch it went into all five (#3379). Neither of those two cases is taken when it would grow the caller's frame too far, which is gcc's stack test in `caller_growth_limits`: the locals the caller keeps in memory, with everything inlined into it so far and the new body's, may come to `INLINE_FRAME_GROWTH` percent more than the caller's own, gcc's `large-stack-frame-growth` of 1000, or to `INLINE_LARGE_FRAME` bytes, gcc's `large-stack-frame` of 256, whichever is more. gcc also lets a call through when a sibling already made the frame that large, trusting the two bodies to share bytes, and that clause is left out because the slot allocator here does not share an array whose address goes to a call. Without the test, `select_default_timezone` in postgres had a frame of 44560 bytes where gcc has 16 (#1989). Under `-fconserve-stack` the two numbers are gcc's for that flag, 40 percent and 100 bytes, so a helper with a 200 byte buffer called once stays a call, as it does with gcc. The Linux kernel builds with the flag because its stacks are a few pages (#2284). From `-O2` up a second pass in `rucc_opt::inline::heap` weighs again the calls the first one left as too large, which is gcc's `inline_small_functions` (#2897). It keeps them in a heap ordered by the badness of 33.4, which `rucc_cost::badness` computes, takes the least bad first, and weighs again every call whose caller or callee a copy changed. A call over its limit goes in when a hint of 33.5 raises the limit for it, scaled by `INLINE_HINT_PERCENT` the way gcc scales it: a loop count or stride the copy knows, a call through a parameter the copy makes direct, or a `__builtin_constant_p` the call answers. A count every call to the function passes gives no hint, since gcc runs `ipa-cp` first and that count is already a constant in the body by the time it asks, and `-fno-ipa-cp` gives the hint back. It also goes in when the copy saves `INLINE_MIN_SPEEDUP` percent of the time the call and its caller take, which is gcc's `big_speedup_p`, a call counting `INLINE_CALL_TIME` there rather than its size. Two bounds stop it. The unit may not grow past `INLINE_UNIT_GROWTH` percent of itself, counted from at least `LARGE_UNIT_INSNS`, and a caller over `LARGE_FUNCTION_INSNS` may not grow past `LARGE_FUNCTION_GROWTH` percent, which are gcc's `inline-unit-growth`, `large-unit-insns`, `large-function-insns` and `large-function-growth`. The frame test above holds for it too. A call to a function that runs once each time the program does is not hot, and goes in only when the program does not grow by it, which is the last test in gcc's `want_inline_small_function_p`. `main` runs once, so does a function that never comes back, and so does a `static` function every call to which is made outside a loop in one of those, which is what gcc's `ipa_propagate_frequency` finds. A large function with a loop that only `main` calls stays a call, as it does with gcc, where the count each call passes would have copied it. The first pass asks the same of a call that `main`, or a function that never comes back, makes outside its loops, which is the test gcc's early inliner words as "call is cold and code would grow": such a call goes in when it grows the caller by no more than `early-inlining-insns`, or when the copies of all the calls that are not hot cost less than the body they let go. A setter of ten lines that `main` calls four times stays four calls, as it does with gcc 16, where it used to go into each, and a lookup of four lines still goes in (#3224). At `-O3` the percentage is 600 and the speedup 15, as in gcc, and `-fno-inline-small-functions` turns the pass off. Each call it takes says "call inlined by the second pass" under `-fopt-info`, and each it leaves says which limit stopped it. On the shapes of #2897, `mix` called with a constant count and with an unknown one, and `apply` called with the address of a function and with a pointer it was given, it copies and leaves the same four calls gcc 16 does. On wasm a call in a loop is hinted too when its callee is `static`, every call to the callee is in that caller and the caller is not over `LARGE_FUNCTION_INSNS`, because the copy of the callee goes away with it, and clang, which a wasm build is measured against, inlines more than gcc. `TargetInfo::loop_hint` says which targets do this, and only wasm32 does (#2866). The frequency in the badness is still the guess of 40.11 rather than a profile. A function declared `always_inline` whose body gcc says can never be copied into a caller is refused with gcc's error at the name in its definition, whether or not anything calls it, which is what gcc does for every function it compiles (#3059). The reasons are gcc's and are asked by the lowering, which only lowers what the file reaches: a call to `setjmp` or another function that comes back twice, `__builtin_longjmp`, `va_start`, `__builtin_apply_args` and a jump through a label's address, the first of these in the body being the one named, and before any of them a `__builtin_setjmp` and a label's address kept in a `static`, which gcc words as the function never being copied. A call inside a cycle is not refused, since gcc refuses one at `-O0`, `-O1` and `-Og` and leaves it a call at `-O2`, `-O3` and `-Os` for reasons that come from the order of its passes, and it stays a call here at every level. A call to a function declared `inline` or small enough to take is weighed by the callee's summary of 33.3 (#3058), which the first pass makes once for each callee and the second pass once for each version of a callee it profiles. The summary keeps each instruction under the condition on the parameters for its block to stay, a disjunction of conjunctions of a branch going one way as gcc's predicates are, and under the parameters whose being known folds the instruction itself away, and adds up the instructions that share both. A call's size and time are the entries still there once the branches its constants decide are decided, which is a few branches worked out rather than the body walked and folded again. The rules are the walk's, and every debug build checks the summary against the walk on every call it answers, so the decisions are the ones the walk made. A body whose conditions need more than `INLINE_SUMMARY_CLAUSES` clauses, or conditions in a clause, which is gcc's `MAX_CLAUSES` of 8, or that has more than 64 parameters, has no summary and is walked as before. A call the constants cut down says so with a note under `-fopt-info-all`. On the shapes of #3058, an `if` and a `switch` on a mode with a long tail and a test inside a test, rucc copies the arms the constants pick wherever gcc 16 does, and where gcc 16 calls the part of the callee it split off, rucc calls the whole callee, since it does not split functions yet. At `-O3` the inliner takes gcc's limits for that level, `INLINE_INSNS_AUTO_O3` of 30 for how much a call to a function nobody declared `inline` may grow its caller and `INLINE_EARLY_INSNS_O3` of 14 for how much a call that is not hot may, where `-O1`, `-O2` and `-Os` have 15 and 6 (#3174). `--param` with gcc's name sets the row of both levels. A `static` function holding a loop of eight lines, called from three places, stays a call at `-O2` and is copied into each caller at `-O3`, and called from three `cold` functions a body of five lines is, which is what gcc 16 does with both. With that the early inliner of 33.7 is built: a call no larger than the call goes in first, before anything is inlined into its callee, then each call within the limits of its level, a callee that makes calls of its own held to the early limit for each of them and itself together, and a call inside a cycle is left alone.

## 33.8 How this is wrong

**`always_inline` is not honoured and the program does not compile.** In C this attribute is a
contract, and code in the wild, notably kernel headers and intrinsics wrappers, depends on it. GCC
errors when it cannot be satisfied. rucc must too, with the reason from the failure enum, and this
is a correctness obligation rather than a heuristic.

**A `noinline`, `noclone` or `weak` function is inlined.** `weak` is the subtle one: a weak symbol may
be replaced at link time, so its body is not authoritative and inlining it is wrong unless
`-fno-semantic-interposition` applies. Same for any interposable symbol in a shared library.

**Target attributes differ between caller and callee.** Inlining a function compiled for AVX-512 into
one compiled for a baseline target emits instructions the caller's target does not have. This is a
whole clause of `can_inline_edge_by_limits_p` and it is a real miscompilation, not a quality issue.

**Optimization attributes differ.** A callee marked `optimize("O0")` inlined into an `-O2` caller is
then optimized at `-O2`, which is not what the user asked for. GCC refuses. rucc must record the
per-function optimization level and refuse across a mismatch.

**Setjmp, computed goto, variable-length arrays, alloca.** A callee containing `alloca` inlined into
a loop grows the caller's stack per iteration. GCC has specific handling; rucc's M4 answer is to
refuse to inline any callee containing `alloca` or a VLA, which is a legal and cheap position.

**The stack frame explodes.** `large-stack-frame-growth` `Init(1000)` exists because inlining
combines frames, and deep inlining of functions with large local arrays overflows the stack in
programs that previously worked. This is a runtime failure with no compile-time symptom and it is why
the parameter is expressed as a percentage bound and not a hint.

**Compile time explodes.** Inlining is the classic superlinear pass: inline A into B, then B is
larger, then inlining B into C costs more, and the total can be quadratic in the callgraph. The unit
growth bound is what makes it linear-ish in practice, and it must be a hard bound, not advisory.

**Debug information becomes wrong.** An inlined body's line numbers belong to the callee's file, and
the caller's frame must describe the inlined frame so a backtrace shows both. DWARF has
`DW_TAG_inlined_subroutine` for this and getting it wrong makes every profile and every crash dump
misleading. This is not optional for a compiler that claims GCC compatibility, and it is the reason
document 03.3's `-Og` should not inline anything beyond the free cases.

**Profile counts are not scaled.** The callee's block counts must be scaled by the call edge's
frequency when merged into the caller, and the callee's remaining offline copy's counts reduced by
the same amount. Getting it wrong biases every later decision and document 11.1's quality tracking
will not catch it, because the result is still a valid profile, just a wrong one.

**Growth is estimated on the unfolded body.** 33.3's predicated summaries. Without them, the
estimate is systematically too large exactly where inlining is most valuable, which biases the whole
heuristic in a direction that looks conservative and responsible and is neither.

## 33.9 What it costs, and what to measure

Compile time: the summary computation is one walk per function plus predicate construction, so
linear. The badness heap is `O(E log E)` with recomputation of affected edges, and the recomputation
is where the time goes on large units. The transformation itself is proportional to the code
produced, and the code produced is bounded by `inline-unit-growth`.

Space: the summaries are the compiler's largest interprocedural data structure and they must be
streamed or discarded for document 35's LTO. In M4, one translation unit at a time, they fit.

Document 42 owes five numbers here.

- **Code size and run time with the inliner off, at `-O2`,** which prices the whole document at once.
- **The `-O2` versus `-O3` decomposition:** rucc `-O2`, rucc `-O2` with the `-O3` inliner parameters
  only, and rucc `-O3`. If the middle number is close to the third, document 33.6's claim is
  confirmed on rucc's own code generator and documents 30 through 32 stay deferred.
- **The predicated-summary check:** how many inlining decisions change when predicates are replaced
  by a flat size estimate. This directly measures whether 33.3's central idea earns its 600 lines.
- **How often the unit growth bound binds**, which tells whether the heuristic or the bound is
  actually making the decisions. If the bound binds on most units, the badness function is not being
  consulted and the tuning effort belongs elsewhere.
- **Agreement with `gcc -O2` on which calls get inlined.** Not expected to be high, and a large
  divergence in one direction on small static functions would indicate a bug rather than a taste
  difference.
