# 41. Correctness

Documents 12 through 40 describe roughly forty transformations, each of which rewrites a program on
the strength of an argument about what the program is allowed to do. This document is about the
machinery that catches the cases where the argument was wrong.

The framing that organises it: an optimizing compiler has **three** failure modes, not one, and they
need three different defences.

**A miscompile.** The compiler emits code that does something the source did not permit. This is the
one everyone means by "compiler bug" and it is the hardest to find because the compiler does not know
it happened.

**An internal error.** A pass asserts, or a later pass finds an invariant broken. This is a good
outcome relative to the first, and most of the machinery in this document exists to convert instances
of the first into instances of the second.

**A silent capability loss.** A transformation stops firing because a predicate it depends on became
conservative, and nothing fails. The output is correct and slower, and only measurement finds it. This
is document 42's problem, listed here because it is the one people forget is a correctness question at
all: a compiler whose optimizations quietly stop working is not meeting spec 00's second axis.

## 41.1 The semantic flags

The single most useful artefact from GCC's source for rucc's correctness story is the enumerated list
of places where a flag changes what the compiler is allowed to assume. Every one of these is a
decision rucc must make explicitly, and a compiler that implements a transformation without knowing
which flag gates it has a latent miscompile.

From `gcc/common.opt`, with line numbers:

| Flag | Line | Default | What it licenses |
|---|---:|---|---|
| `-fstrict-aliasing` | 3101 | on at `-O1`+ | Type-based alias analysis |
| `-fstrict-overflow` | 3105 | on | Signed overflow is UB. "Negated as `-fwrapv -fwrapv-pointer`" |
| `-fwrapv` | 3649 | off | Signed arithmetic overflow wraps |
| `-fwrapv-pointer` | 3645 | off | Pointer overflow wraps |
| `-ftrapv` | 3216 | off | Signed overflow traps |
| `-fdelete-null-pointer-checks` | 1404 | `Init(-1)`, target-dependent | A dereference implies non-null |
| `-fisolate-erroneous-paths-dereference` | 3292 | `-O2` | Turn a provably-UB path into a trap |
| `-fisolate-erroneous-paths-attribute` | 3298 | off | Same, driven by `nonnull` / `returns_nonnull` |
| `-fsemantic-interposition` | 2925 | `Init(1)`, on | A definition may be replaced at load time |
| `-fallow-store-data-races` | 1123 | off | The compiler may introduce a store |
| `-fstore-merging` | 1951 | `-O2` | Adjacent stores may be merged |
| `-ftrapping-math` | 3212 | `Init(1)`, on | FP operations may raise exceptions |
| `-fsigned-zeros` | 3008 | `Init(1)`, on | `-0.0` is distinguishable from `+0.0` |
| `-ffinite-math-only` | 1779 | off | No NaNs or infinities occur |
| `-fassociative-math` | 3421 | off | FP addition and multiplication may be reassociated |
| `-freciprocal-math` | 3426 | off | `a/b` may become `a * (1/b)` |
| `-funsafe-math-optimizations` | 3434 | off | The umbrella for the previous four |
| `-frounding-math` | 2853 | off | The rounding mode may change at run time |
| `-fexcess-precision=` | 1741 | `fast` or `standard` | Whether intermediates may be wider |
| `-ffp-contract=` | 1803 | `Init(FP_CONTRACT_FAST)` | Whether `a*b+c` may become an FMA |

**Two of these deserve emphasis because they are the ones a new compiler gets wrong.**

`-fsemantic-interposition` defaults **on**, meaning that by default GCC assumes any non-static
function in a shared library may be replaced at load time by a different implementation, so its body
may not be used to infer anything about its behaviour. Document 34's IPA analyses and document 33's
inliner both depend on this and rucc's default must match GCC's or the two compilers will disagree
about a large class of programs.

`-fallow-store-data-races` defaults **off**, and the reason is the C11 memory model: a compiler may
not introduce a store to a location the program did not store to, because another thread may be
writing it. This constrains document 27's LICM (store motion out of a loop requires the store to be
unconditional in the loop), document 17's store elimination, and document 22's if-conversion (converting
`if (c) x = v;` to an unconditional store is illegal). Each of those documents recorded the
constraint; this is where it is named.

`-fstrict-overflow` at line 3105 has no `Var()` and its description is "Treat signed overflow as
undefined. Negated as `-fwrapv -fwrapv-pointer`". **It is not a variable, it is an alias**, which is
the right design: there is no `flag_strict_overflow` for a pass to consult inconsistently. rucc
should copy this exactly, because a boolean that means "the absence of two other booleans" is a bug
waiting for the moment somebody sets one of them.

## 41.2 `-ffast-math` is a set, not a flag

`gcc/opts.cc:3527` and :3555:

```c
static void
set_fast_math_flags (struct gcc_options *opts, int set)
{
  if (!opts->frontend_set_flag_unsafe_math_optimizations)
    {
      opts->x_flag_unsafe_math_optimizations = set;
      set_unsafe_math_optimizations_flags (opts, set);
    }
  if (!opts->frontend_set_flag_finite_math_only)
    opts->x_flag_finite_math_only = set;
  if (!opts->frontend_set_flag_errno_math)
    opts->x_flag_errno_math = !set;
  ...
}

static void
set_unsafe_math_optimizations_flags (struct gcc_options *opts, int set)
{
  if (!opts->frontend_set_flag_trapping_math)
    opts->x_flag_trapping_math = !set;
  if (!opts->frontend_set_flag_signed_zeros)
    opts->x_flag_signed_zeros = !set;
  if (!opts->frontend_set_flag_associative_math)
    opts->x_flag_associative_math = set;
  if (!opts->frontend_set_flag_reciprocal_math)
    opts->x_flag_reciprocal_math = set;
}
```

Three structural observations.

**`-ffast-math` has no variable of its own.** It sets eight others, and `fast_math_flags_set_p` at
:3569 reconstructs the answer by checking all of them. No pass ever asks "is `-ffast-math` on"; every
pass asks the specific question it needs. This is the discipline document 32.11 asked for and it is
the correct one: a transformation gated on `-ffast-math` is a transformation whose author did not
know which assumption it needed.

**The `frontend_set_` guards mean an explicit flag wins over the umbrella**, regardless of order on
the command line. `-ffast-math -fno-associative-math` and `-fno-associative-math -ffast-math` behave
the same. This is a compatibility obligation, not a nicety: rucc claiming GCC compatibility must
reproduce it, and the mechanism, a "was this set explicitly" bit per flag, has to exist in the option
representation from the start because retrofitting it means touching every flag.

**`-ffast-math` also sets `flag_errno_math` off**, which is not a mathematical assumption at all: it
is a statement that `errno` need not be set by library math functions, which is what licenses turning
a `sqrt` call into an instruction. It sits in this set because it is what users want, not because it
follows.

## 41.3 Purity, and the exhaustiveness discipline

Documents 08, 17, 20 and 34 all depend on classifying what a call can do, and all of them deferred the
classification's completeness argument here.

GCC's answer is the `ECF_` flag set at `gcc/tree-core.h:46` onward, nineteen bits. The ones that
matter for optimization:

| Flag | Line | Meaning |
|---|---:|---|
| `ECF_CONST` | 46 | Result depends only on arguments; reads no memory |
| `ECF_PURE` | 51 | Reads memory but does not write it |
| `ECF_LOOPING_CONST_OR_PURE` | 56 | Const or pure, but may not terminate |
| `ECF_NORETURN` | 59 | Does not return |
| `ECF_MALLOC` | 62 | Returns a pointer that does not alias anything live |
| `ECF_NOTHROW` | 68 | Does not throw |
| `ECF_RETURNS_TWICE` | 74 | `setjmp` |
| `ECF_NOVOPS` | 78 | Does not read or write memory the compiler models |
| `ECF_LEAF` | 81 | Calls nothing in this translation unit |
| `ECF_RET1` | 84 | Returns its first argument |

**`ECF_LOOPING_CONST_OR_PURE` is the one to notice.** A function may be const in the sense that its
result depends only on its arguments while still failing to terminate, and deleting a call to it is
therefore not obviously safe. GCC keeps the two properties separate. C's forward progress rules mean
the C front end can often drop the distinction, but the fact that GCC found the distinction necessary
is a warning that "pure" is not one property.

**The exhaustiveness discipline for rucc.** Purity is not a boolean and it is not a lattice the
compiler may extend casually. rucc's version should be:

- One enum with an explicit variant per level, including the looping distinction, and no `Unknown`
  that silently means `Unknown`. Where the answer is not known, the variant is `Opaque` and it is the
  *most* conservative value, so that a bug in the classifier is a missed optimization rather than a
  miscompile.
- The classifier is one function, `purity_of(callee) -> Purity`, and **it is exhaustive over the
  callee representation**: a `match` with no wildcard arm, so that adding a new kind of callee (an
  indirect call, an internal function, an inline asm, a builtin) is a compile error until somebody
  classifies it. This is the single most valuable thing Rust offers a compiler over C++ here and it
  should not be given away with a `_ =>` arm.
- The builtin table carries the purity, and the table is generated from one source with a test that
  every builtin has an entry. A builtin defaulting to `Opaque` when the table is missing an entry is
  correct; a builtin defaulting to `Const` is a miscompile generator.
- **A function's purity may be strengthened by analysis (document 34) but the analysis result and the
  declared attribute are separate fields**, because a user writing `__attribute__((const))` on a
  function that is not const is asserting something the compiler must honour, and conflating the two
  loses the ability to check the assertion under a sanitizer.

## 41.4 What GCC verifies, and when

`gcc/passes.cc:2088`, inside `execute_function_todo`:

```c
  /* If we've seen errors do not bother running any verifiers.  */
  if (flag_checking && !seen_error ())
    {
      dom_state pre_verify_state = dom_info_state (fn, CDI_DOMINATORS);
      dom_state pre_verify_pstate = dom_info_state (fn, CDI_POST_DOMINATORS);

      if (flags & TODO_verify_il)
	{
	  if (cfun->curr_properties & PROP_gimple)
	    {
	      if (cfun->curr_properties & PROP_cfg)
		verify_gimple_in_cfg (cfun, !from_ipa_pass);
	      else
		verify_gimple_in_seq (gimple_body (cfun->decl));
	    }
	  if (cfun->curr_properties & PROP_ssa)
	    verify_ssa (true, !from_ipa_pass);
	  if ((cfun->curr_properties & PROP_cfg)
	      && !from_ipa_pass)
	    verify_flow_info ();
	  if (current_loops
	      && ! loops_state_satisfies_p (LOOPS_NEED_FIXUP))
	    {
	      verify_loop_structure ();
	      if (loops_state_satisfies_p (LOOP_CLOSED_SSA))
		verify_loop_closed_ssa (false);
	    }
	  if (cfun->curr_properties & PROP_rtl)
	    verify_rtl_sharing ();
	}

      /* Make sure verifiers don't change dominator state.  */
      gcc_assert (dom_info_state (fn, CDI_DOMINATORS) == pre_verify_state);
      gcc_assert (dom_info_state (fn, CDI_POST_DOMINATORS) == pre_verify_pstate);
    }
```

Six things are worth taking from twenty lines.

**Verification is driven by the properties the IR currently has**, not by which pass just ran. The
same code verifies after every pass and asks only what is applicable. rucc's pass manager already
has spec 09's property system; this is what it is for.

**The verifiers are guarded by `!seen_error ()`.** After a user error the IR is legitimately
malformed, and running verifiers then produces internal errors that hide the real diagnostic. rucc's
`rucc-diag` needs the same gate and it needs it from the beginning, because the bug it prevents
presents as "the compiler ICEs on invalid input", which is the worst class of diagnostic quality bug.

**Loop verification is conditional on `LOOPS_NEED_FIXUP` being clear**, which is the mechanism
document 26 described: a pass may leave the loop tree stale and say so, and verification respects the
declaration. A verification framework without a way to say "this is known-stale right now" gets
disabled instead.

**Loop-closed SSA is verified only when the IR claims to be in it.** Document 26.4's finding that
block parameters make LCSSA nearly free does not remove the need to check the claim.

**The verifiers must not change dominator state**, asserted explicitly. A verifier with a side effect
makes checking builds behave differently from release builds, which is how a bug becomes
unreproducible. rucc's verifiers should take the IR by shared reference, which makes this a type
error rather than an assertion.

**All of it is under `flag_checking`.** GCC's build-time levels, from `gcc/doc/install.texi:2306`:
`release` is the cheapest set (`assert,runtime`) and is always on unless explicitly disabled; `yes`
adds `misc,gc,gimple,rtlflag,tree,types`; `extra` adds checks "that might affect code generation and
should therefore not differ between stage1 and later stages"; `all` adds everything but `valgrind`.
The distribution: **2,013 `gcc_checking_assert` and 6,889 `gcc_assert` across `gcc/*.cc` and
`gcc/*.h`**, so roughly a quarter of GCC's assertions are compiled out of a release build.

That ratio is the model for rucc. `debug_assert!` for the ones that cost, `assert!` for the ones that
do not, and **the verifiers behind a `-Zverify-each` style flag that CI turns on and users do not
pay for**. The important discipline is not the ratio, it is that the expensive checks exist at all
and that CI runs with them on.

## 41.5 The debug-info invariant

`gcc/common.opt:1304`:

```
fcompare-debug=
Common Driver JoinedOrMissing RejectNegative Var(flag_compare_debug_opt)
-fcompare-debug[=<opts>]	Compile with and without e.g. -gtoggle, and compare the final-insns dump.
```

and `config/bootstrap-debug.mk`:

```
# This BUILD_CONFIG option builds checks that toggling debug
# information generation doesn't affect the generated object code.
...
STAGE2_CFLAGS += -gtoggle
do-compare = $(SHELL) $(srcdir)/contrib/compare-debug $$f1 $$f2
```

**The invariant: `-g` must not change the generated code.** GCC checks it by compiling everything
twice during bootstrap and comparing. This is a strong invariant with a cheap check and it catches a
specific, common, subtle bug class: a pass that consults something reachable only when debug
information exists, most often by iterating uses and counting a debug use as a real one.

Document 36.3 already noted GCC's `avoid_deep_ter_for_debug` at `gcc/cfgexpand.cc:7047`, which is a
place where debug information *does* change the code, deliberately, and is therefore an admission
that the invariant is hard.

**rucc's version.** rucc's IR (spec 05, `crates/rucc-debug`) has to make the same choice GCC did:
either debug information is carried in instructions that participate in use lists, in which case every
pass must be careful, or it is carried out of band. The out-of-band choice makes the invariant easy
and makes debug information harder to keep accurate through transformations, which is the trade GCC
made in the other direction and then paid for. Either way, **`-fcompare-debug` as a CI job compiling
the corpus twice and diffing the object files is a few lines of shell and it should exist before it is
needed**, because retrofitting the invariant after a year of passes have violated it is a large,
uninteresting piece of work.

## 41.6 Finding the guilty pass

`gcc/doc/invoke.texi:20602` documents `-fdisable-{ipa,rtl,tree}-<pass>` and `-fenable-...`, "intended
for use for debugging GCC", with two features worth copying:

**A duplicated pass is addressed by name plus a sequence number.** `-fdisable-tree-ccp1` disables the
first `ccp` instance only. Document 17.5's decision that rucc's cleanup group repetitions are
explicit rather than implicit makes this naming scheme fall out for free, which is one more argument
for that decision.

**The argument is a range list over function IDs.** `-fdisable-tree-cunroll=1` disables the pass for
the function with cgraph uid 1 only, and ranges and assembler names are accepted. **This turns "which
pass miscompiles this program" and "which function does it miscompile" into two independent bisections
that a script can run**, and combined they localise a wrong-code bug to a pass and a function without
a debugger. The function IDs come from the dump headers, so the workflow is closed.

rucc should have `-fdisable-<pass>[=<range>]` and `-fenable-<pass>[=<range>]` from the point at which
there is more than one pass. The cost is a lookup in the pass manager and it repays itself the first
time a wrong-code bug is reported against a program that takes two minutes to compile.

Alongside it: `-fopt-info` (`gcc/doc/invoke.texi:20403`) for what fired, and the per-pass dumps of
spec 09. The three together are the debugging interface, and rucc's advantage is that it can make the
dumps stable and diffable by construction rather than as an afterthought.

## 41.7 Testing that finds miscompiles

**Differential testing against GCC** is rucc's central asset and its central risk. rucc claims GCC
compatibility, so for any program with defined behaviour, `rucc -O2` and `gcc -O2` must agree on
output. That is a test oracle most compilers do not have. The risk is that agreement on a program with
*undefined* behaviour is not required, and a random program generator that emits undefined behaviour
produces false reports.

**Csmith** solves this by generating C programs that are UB-free by construction, and GCC credits it
at `gcc/doc/contrib.texi:72`. **YARPGen** is the more recent generator with better coverage of the
optimizations that documents 19, 25 and 32 describe, and its second version specifically targets loop
optimizations and vectorization by generating programs with known-idiomatic loop structure. Both are
external tools rucc can use directly; neither needs to be written.

**The reduction step matters as much as the generator.** A Csmith program that miscompiles is 5,000
lines. C-Reduce turns it into 20. Without reduction, differential testing produces reports nobody
acts on. rucc's interestingness test for C-Reduce is a script that compiles with rucc and with GCC and
compares output, plus a check that the reduced program is still UB-free, which is where a sanitizer
build is needed: **compile the candidate with `gcc -fsanitize=undefined,address` and run it; if it
reports, the candidate is not interesting.** `gcc/flag-types.h:305` lists the sanitizer bits and
`SANITIZE_UNDEFINED` at :346 is the set that matters.

**Self-hosting is not available to rucc** in the way bootstrapping is to GCC, since rucc is written in
Rust. That removes GCC's strongest single test, the three-stage bootstrap comparison, and it means
rucc has to buy the equivalent coverage elsewhere: a large corpus of real C compiled and run, which is
document 42's job, plus the differential testing above.

**The four bootstrap-derived invariants rucc can still have**, each as a CI job:

- **`-fcompare-debug`** over the corpus, per 41.5.
- **Optimization-level consistency**: a program's output must not depend on `-O` level. This is
  cheap, it is the single most effective wrong-code test, and it subsumes a large fraction of what
  bootstrap comparison catches.
- **Flag consistency**: `-fno-strict-aliasing` must never produce different output from
  `-fstrict-aliasing` on a corpus that is aliasing-clean, and similarly for each flag in 41.1's table.
  A difference means either the corpus has UB or the flag is being consulted somewhere it should not
  be. Both are worth knowing.
- **Verifier-on builds** of the whole corpus, per 41.4.

## 41.8 Translation validation as a CI layer

Document 05.4 recorded the research position, and there are two places rucc is already committed to
it: **Crocus-style SMT verification of the lowering rules** (spec 10.2's verification obligation, read
in document 36) and verification of the e-graph's rewrite rules (documents 12.3 and 13).

The honest framing is that translation validation comes in three strengths and rucc should be clear
about which it is buying.

**Rule verification.** Each rewrite rule is checked once, offline, against a bitvector SMT solver: for
all inputs, the left side and the right side agree. This is what Crocus does for Cranelift's ISLE
rules and what Alive2 does for LLVM's InstCombine. **It scales, because the number of rules is small
and fixed, and it is checked at development time, not compile time.** It catches the largest single
category of real compiler bugs, which is a peephole that is wrong for one width or one edge value.
This is the strength rucc has committed to and it is the right one.

**Per-compilation validation.** After each pass, prove that the output refines the input. This is
CompCert-adjacent and it does not scale to a production compiler on real functions, though it is
tractable on small ones. rucc's version, if any, is a CI job over small functions, not a compile-time
feature.

**End-to-end verification.** CompCert. Not what rucc is; recorded so nobody confuses the first
strength for it.

**The CI shape.** Rule verification is a build-time step over the rule files, producing a report of
verified, unverified and timed-out rules, with a policy that the unverified set may not grow. A rule
that cannot be verified is not forbidden, since some rules genuinely involve things a bitvector solver
cannot express, but it must be listed by name with a reason, exactly like the coverage exception list
of document 36.4. **The list is the artefact; the count going up is the alarm.**

## 41.9 The undefined-behaviour asymmetry

One point that belongs in this document because it affects every pass and is easy to get backwards.

An optimizer exploits undefined behaviour by assuming it does not happen. Document 10's value ranges
assume signed arithmetic does not overflow; document 16's load elimination assumes a
dereference means the pointer is valid; document 27's LICM hoists a division on the assumption the
divisor is not zero on a path that would execute it. **Each of these is a place where the compiler
makes a program that was already broken behave differently, and there is a real cost to users.**

GCC's mitigations, and rucc's obligations:

**`-fisolate-erroneous-paths-dereference`** at `gcc/common.opt:3292`, on at `-O2`: when a path is
provably UB, do not silently optimize on the assumption it is unreachable, but turn it into a trap.
The path stops being a source of surprising behaviour and becomes a crash at the right place. This is
strictly better than exploitation for a bounded cost and rucc should implement it at the same level.

**`-Wstrict-overflow`** and the family of warnings that fire when a transformation depended on UB.
These are hard to get right, they produce false positives, and GCC's have a reputation for it. rucc's
version should start narrow: warn only where a transformation *removed a check the user wrote* on the
strength of UB, which is the case users actually care about and the case where the warning is almost
never wrong.

**The defaults themselves.** `-fstrict-aliasing` on at `-O2` and `-fwrapv` off are GCC's, and
compatibility means rucc matches them. But `-fno-strict-aliasing` must actually work, meaning
document 08's alias analysis must have a clean switch that disables the type-based component only,
exactly as `gcc/alias.cc:420` and :556 do ("Disable TBAA oracle with `!flag_strict_aliasing`"), and
the flag-consistency CI job of 41.7 is what keeps it working.

## 41.10 What rucc builds

- **The flag table**, one file, one enum, every entry in 41.1's table with the pass that consults it
  named. Plus the "was this set explicitly" bit per flag, per 41.2, in the option representation from
  the start.
- **`-ffast-math` as a setter of eight flags with no variable of its own**, per 41.2, and a lint that
  no pass mentions it.
- **The purity enum and its exhaustive classifier**, per 41.3, with `Opaque` as the conservative
  default and no wildcard match arm.
- **The verifiers**, one per invariant, driven by IR properties, guarded on no-errors-seen, taking the
  IR immutably. Estimated 1,500 lines: SSA dominance and single-assignment, block parameter arity
  agreement across all predecessors, CFG consistency, type agreement per instruction, loop tree
  freshness, and after selection, register class and constraint satisfaction (document 39's checker,
  which is the largest single one).
- **`-fdisable-<pass>[=<range>]` and `-fenable-<pass>[=<range>]`**, per 41.6, roughly 100 lines given
  the pass manager.
- **`-fcompare-debug` as a CI job**, per 41.5.
- **The differential-testing harness**: Csmith and YARPGen generation, C-Reduce reduction with a
  sanitizer-based UB filter, GCC as the oracle, per 41.7. This is scripting, not compiler code, and it
  is the highest-value thing in this list.
- **The four consistency CI jobs**, per 41.7.
- **The rule verification report**, per 41.8, with the unverified list as a checked-in file.

## 41.11 How this is wrong

**The oracle is wrong.** Differential testing against GCC finds places rucc differs from GCC,
including places where GCC is the one with the bug. Reports need triage against the standard, not
against GCC. This is a cost of the compatibility goal and it is worth it, but it should be expected
rather than discovered.

**The corpus is UB-ridden.** Real C programs contain undefined behaviour, so the optimization-level
consistency job produces failures that are the program's fault. The response is a sanitizer pass over
the corpus first and an allowlist of known-dirty programs, and the allowlist must have a reason per
entry or it becomes a place to hide failures.

**Verifiers check the invariants somebody thought of.** The invariant that gets violated is the one
nobody wrote a check for. The partial defence is that every wrong-code bug found should add a verifier
check if one could have caught it, and that this is a rule rather than a suggestion.

**Exhaustive matching is defeated by one wildcard.** 41.3's discipline is worth exactly as much as the
lint that enforces it. A `_ => Purity::Const` arm added under deadline pressure is a miscompile
generator that will not be found for a year.

**Rule verification proves the rule, not its application.** A rule that is correct for all inputs can
still be applied where its guard does not hold, if the guard is implemented separately from the
statement that was verified. The mitigation is that the guard must be part of the verified statement,
which constrains how the rule DSL of spec 10.2 is written and is a reason to settle that DSL before
writing many rules.

**Flags are consulted inconsistently.** The specific failure: two passes disagree about whether
signed overflow is UB, one folds on the assumption it is, the other does not, and the result is
inconsistent rather than merely conservative. The defence is that flags are read through one accessor
per semantic question, not by touching the flag directly, and that the accessor is where the
`-fstrict-overflow`-is-an-alias logic lives.

**A conservative bug is invisible.** The third failure mode from this document's opening. Only
document 42's measurement finds it, which is the argument for measuring optimization firing counts and
not only run time.

## 41.12 The decision

Correctness for rucc is: one flag table with explicit-set tracking, one purity classifier that cannot
be extended without a compile error, verifiers driven by IR properties and run in CI rather than in
release builds, offline SMT verification of the rewrite and lowering rules with a checked-in
unverified list, per-pass and per-function disable flags for bisection, and a differential-testing
harness against GCC with UB filtering.

**The finding that shapes it:** GCC's correctness machinery is overwhelmingly *build-time and
CI-time*, not compile-time. 2,013 of its 8,902 assertions are compiled out of a release build, its
strongest single check is a bootstrap comparison that users never run, and its most useful debugging
feature is a pair of flags nobody is supposed to use. A new compiler tends to invest in the parts users
see. The evidence from the incumbent is that the parts users never see are where the correctness
actually comes from.
