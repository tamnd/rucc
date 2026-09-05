# 25. Tail calls

A call in tail position can reuse the caller's stack frame instead of building a new one, turning
`return f(x)` into a jump. Two distinct transformations share the name and they belong in different
places: tail *recursion* elimination is a middle-end transformation that turns self-recursion into a
loop, and *sibling call* optimization is a back-end transformation that turns any tail call into a
jump.

GCC does the first in `gcc/tree-tailcall.cc`, 2,267 lines, which also marks calls for the second,
performed in `gcc/calls.cc`. The file header at `gcc/tree-tailcall.cc:57` describes the arrangement:
it "implements the tail recursion elimination. It is also used to analyze the tail calls in general,
passing the results to the rtl level where they are used for sibcall optimization."

## 25.1 Tail recursion elimination and the accumulator trick

The basic case is straightforward: `return f(a, b)` where `f` is the current function becomes
assignment of the parameters and a branch to the entry block. In rucc's IR the entry block has
parameters, so the branch carries the new arguments, and there is no assignment step at all.

What is not straightforward, and what makes GCC's version worth 2,267 lines, is that most recursion
in real C is not in tail position. `gcc/tree-tailcall.cc:74` gives the canonical example:

```c
int sum (int n) { if (n > 0) return n + sum (n - 1); else return 0; }
```

The call is followed by an add, so it is not a tail call. GCC's transformation introduces two
accumulators, additive and multiplicative, initialised to 0 and 1, with the invariant that at the
return statement the function returns `a_acc + x * m_acc` instead of `x`. Then
`return a + m * f(...)` becomes: increase `a_acc` by `a * m_acc`, multiply `m_acc` by `m`, and tail
call. The algebra is spelled out at `gcc/tree-tailcall.cc:112`:

> a_acc + (a + m * f(...)) * m_acc = (a_acc + a * m_acc) + (m * m_acc) * f(...)

The result is the loop `while (n > 0) acc += n--;`. If an accumulator is provably unchanged it is
omitted.

**This is a genuinely elegant transformation and it is also narrow.** It applies when the operation
between the call and the return is addition or multiplication by a value independent of the call.
That covers `sum`, `factorial`, and a fair amount of teaching code, and it covers very little of what
real C programs do, because real recursive C functions are tree walkers whose recursion is not linear.

**rucc's position: build the plain case, not the accumulator case, in M4.** Plain tail recursion,
where the call is directly the returned value, is perhaps 100 lines given block parameters and it
removes an entire stack frame per iteration, including the risk of stack overflow, which is a
correctness-adjacent property that people rely on. The accumulator version is recorded as a
post-1.0 item with a note that its value should be measured before it is built, since it is the sort
of transformation that appears in compiler textbooks far more often than it appears in profiles.

## 25.2 Sibling calls

The general case: `return f(x)` for any `f`. If the callee's stack argument area fits in the
caller's, and no local's address has escaped to the callee, the caller can pop its frame, place the
arguments, and jump.

The value is not primarily speed, though it saves a call and a return. It is that a chain of tail
calls runs in constant stack space, which is the difference between a working program and a stack
overflow for anything written in continuation-passing style, for state machines that dispatch by
tail call, and for interpreters using tail-call threading.

**The conditions, from `gcc/calls.cc:3080` and `gcc/tree-tailcall.cc:154` onwards**, are a long list
and every entry is there because of a bug:

- The caller does not use `alloca` (`gcc/tree-tailcall.cc:177`). A variable-size frame cannot be
  popped before the jump.
- The caller does not use varargs (`gcc/tree-tailcall.cc:158`).
- The caller does not use `setjmp` (`gcc/tree-tailcall.cc:197`) or `__builtin_eh_return`
  (`gcc/tree-tailcall.cc:205`), or setjmp-longjmp exceptions (`gcc/tree-tailcall.cc:187`).
- The argument size is not variable (`gcc/calls.cc:3093`).
- The callee's outgoing argument area is no larger than the caller's incoming one, or the target
  supports growing it.
- **No local variable's address is passed to the callee, or otherwise escapes.** This is the one that
  matters most and the one that is easy to miss: the caller's frame is destroyed before the callee
  runs, so any pointer into it dangles. GCC warns about it under `-Wmusttail-local-addr`
  (`gcc/tree-tailcall.cc:212`).
- The call is not inside another call being expanded (`gcc/calls.cc:3084`).
- The return values are compatible: the caller returns exactly what the callee returns, in the same
  location.

There is also, at `gcc/calls.cc:3102`, a workaround for PR90329 concerning Fortran hidden string
length arguments, which is included here only as evidence of how many special cases accumulate around
this transformation over twenty years.

**rucc's escape condition is document 08.4's analysis**, and here it must be exactly right rather
than merely conservative, because the failure is a dangling pointer into a reused frame. The rule is
the whitelist rule from 08.6 stated at its strongest: a tail call is performed only when every local
whose address was taken is provably not reachable from any argument, and "provably" means the
analysis said no, not that it failed to say yes.

## 25.3 `musttail`

GCC 15 added `[[gnu::musttail]]` and `__attribute__((musttail))` on a return statement
(`gcc/doc/extend.texi:3481`), which requires the tail call and **reports an error if it cannot be
generated** rather than silently falling back.

This changes the transformation's character. An optional optimization that sometimes does not fire is
fine; a guaranteed one is a language feature that a program's correctness depends on, because code
written with `musttail` will overflow the stack without it. GCC's implementation reflects this: every
one of the refusal conditions in 25.2 has an associated `maybe_error_musttail` call with a specific
message, so the programmer is told *which* condition failed.

**rucc must implement `musttail` for GCC compatibility**, and implementing it well means the error
messages are as specific as GCC's. That has a design consequence worth stating: the tail call
analysis cannot be a predicate returning a boolean. It returns either "yes" or "no, because
`<reason>`", exactly as document 08.5 requires alias queries to return an attributed answer. The
same discipline, for the same reason: a negative answer that cannot explain itself is undebuggable.

This is also the strongest argument for doing the analysis in the middle end rather than the back
end. A `musttail` that fails must fail at a source location with a message, and by the time the back
end is placing arguments the source location is thin.

## 25.4 Where it runs in rucc

**One analysis, two consumers.** A pass in the middle end, at `-O2` and above and additionally
whenever any `musttail` attribute is present at any level including `-O0`, which walks the return
statements, identifies tail positions, checks the conditions, and either performs the recursion
elimination directly or marks the call as a sibling call for the back end.

The mark is a flag on the call instruction. Document 36's lowering reads it and emits the frame
teardown before the jump; document 39's register allocation must know that the call does not return,
so values live across it are not live at all, which shortens live ranges usefully.

**Ordering.** After inlining, because inlining creates tail calls when the inlined callee's tail call
becomes the caller's. Before the loop pipeline, so that a recursion turned into a loop gets the loop
optimizations, which is most of the value of the transformation and is easy to lose by scheduling it
after. GCC places `pass_tail_recursion` in the early pipeline and `pass_tail_calls` late, for exactly
this split.

**And it interacts with document 21.** Turning recursion into a loop creates a back edge to the entry
block, which is not permitted: the entry block may not have predecessors, since its parameters are
the function's parameters. So the transformation splits the entry block, putting the parameters in
the original and the body in a new header that the recursion branches to. That is mechanical, and
forgetting it produces an IR the verifier rejects, which is the good outcome.

## 25.5 What is deliberately not built

**The accumulator transformation.** 25.1.

**Tail calls through function pointers.** Legal and useful, since interpreter dispatch is exactly
this, and it requires the same conditions plus knowing the callee's signature, which an indirect call
site has from its type. Worth building; not M4, because M4's escape analysis on an indirect call
gives up anyway, so the transformation would never fire.

**Tail calls to a different function with a larger argument area.** Requires target support for
growing the frame before the jump, which some ABIs allow and some do not. M4's condition is
"no larger than the caller's", which is the portable subset.

**Mutual recursion turned into a loop.** Needs the call graph and is document 34's territory, and it
is subsumed by sibling calls anyway: mutual tail recursion runs in constant stack with sibling calls
without any loop being formed.

## 25.6 How this is wrong

**A pointer into the caller's frame is passed and the frame is destroyed.** The dangling-frame bug.
It is silent, it corrupts memory, and it depends on the callee's behaviour, so it reproduces
intermittently. Every other bug in this document is preferable.

**A `musttail` call is silently not made.** The program stack-overflows on deep recursion, in
production, and the compiler said nothing. The rule is that `musttail` failure is an error, never a
warning, never silent.

**A `musttail` call is made when it should not be.** The inverse, and worse. `musttail` is a request,
not a licence to violate the conditions in 25.2. If the caller used `alloca`, the answer is an error,
not a tail call.

**Tail recursion elimination changes the semantics of a variable's lifetime.** A local in the
recursive function has a distinct instance per invocation; after the transformation there is one.
That is fine as long as no address escaped, which is the same condition as above, and it is fine for
debug information only in the sense that the debugger now sees one frame where the user expected
many. `-Og` should not do this and document 03.4's `-Og` list does not include it.

**The return value is transformed wrongly by the accumulators.** Not applicable in M4 since the
accumulators are not built, and recorded because the algebra at `gcc/tree-tailcall.cc:112` is exactly
the kind of thing that is right in the paper and wrong in the code for signed overflow: `a * m_acc`
can overflow where the original `a + m * f(...)` did not, and under `nsw` semantics that is undefined
behaviour introduced by the compiler. If the accumulator version is ever built, the accumulators must
use wrapping arithmetic.

**A varargs caller tail calls.** The condition list. The caller's variable arguments live in its
frame.

**Exception or cleanup code follows the call.** `gcc/calls.cc:3082` notes "Don't try if there's
cleanups, as we know there's code to follow the call". A call is only in tail position if nothing
executes after it, and destructors, cleanup attributes, and stack protector epilogues are code.
Stack protection in particular: a function compiled with `-fstack-protector` has a canary check in
its epilogue, and a tail call skips it. GCC's answer is to not tail call from such functions; rucc's
must be the same, and it must be stated, because otherwise the two features silently conflict.

## 25.7 What it costs

The analysis is one walk of the return statements, with a check per candidate that is linear in the
call's arguments plus an escape query. Cheap.

The recursion transformation is a block split and an edge redirect. Cheap.

The sibling call marking costs nothing; the back-end work is in document 36 and is a different frame
teardown sequence, not extra analysis.

The measurement in document 42 is unusual for this document because the interesting number is not
speed. It is: on the corpus, how many calls does `gcc -O2` turn into sibling calls that rucc does
not, broken down by refusal reason. That breakdown is available for free because 25.3 already
requires the analysis to attribute its refusals, and it is the direct measure of whether the
condition list is complete or over-conservative.
