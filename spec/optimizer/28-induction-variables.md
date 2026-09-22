# 28. Induction variables

A loop's addressing is arithmetic on the loop counter, and that arithmetic is what the loop actually
spends its time on. `a[i]` is `a + i*4`, computed fresh each iteration, when it could be a pointer
incremented by 4. Choosing what the loop increments, and how the uses are expressed in terms of it,
is induction variable optimization, and it is the single largest determinant of inner loop quality
on scalar code.

`gcc/tree-ssa-loop-ivopts.cc` is 8,261 lines, the second largest file in the tree middle end after
value numbering. `gcc/gimple-ssa-strength-reduction.cc` adds 4,162 for the cases ivopts leaves.

## 28.1 What GCC does, from its own summary

`gcc/tree-ssa-loop-ivopts.cc:20` describes four steps.

**One: find the interesting uses.** Three kinds: uses of induction variables in non-linear
expressions, addresses of arrays, and comparisons of induction variables. Uses are grouped, and
specifically "address type uses are grouped together if their iv bases are different in constant
offset", which is the recognition that `a[i]`, `a[i+1]` and `a[i+2]` should share one induction
variable.

**Two: find candidates.** The existing induction variables, plus new ones derived from the uses.

**Three: choose the optimal set** by a cost function with three parts:

- *Group and use costs.* Each use picks the best candidate and adds the cost of adapting it, "adding
  base and offset for arrays, etc."
- *Variable costs.* Each candidate costs something to increment each iteration. "The original
  variables are somewhat preferred," which is a bias toward not changing things.
- *Set cost.* "Depending on the size of the set, extra cost may be added to reflect register
  pressure."

And then: "All the costs are defined in a machine-specific way, using the target hooks and machine
descriptions to determine them."

**Four: rewrite** the uses in terms of the chosen set and let dead code elimination remove the rest.

The header adds, at `gcc/tree-ssa-loop-ivopts.cc:64`: "All of this is done loop by loop. Doing it
globally is theoretically possible, it might give a better performance... but getting all the
interactions right would be complicated."

## 28.2 Why this is a set-selection problem and not a rewriting problem

The framing is what makes ivopts hard and it is worth being precise about, because the naive
implementation is a strength reducer that replaces each multiply as it finds it, and that
implementation produces worse code than doing nothing on real loops.

Consider `for (i = 0; i < n; i++) sum += a[i] * b[i];`. The uses are `a + i*4`, `b + i*4`, and the
comparison `i < n`. Options:

- Keep `i`, compute both addresses. One increment, two multiplies (or two scaled-index addressing
  modes if the target has them, in which case this is optimal).
- Two pointers `pa`, `pb`, incremented by 4 each, compare `pa` against `a + n*4`. Two increments, no
  multiplies, no separate counter. Better on a target without scaled indexing.
- Two pointers plus `i`. Three increments. Worse than both.

Which is best depends entirely on the target's addressing modes, and the difference is roughly 30% of
the loop's instruction count. **There is no target-independent right answer**, which is why GCC's
costs come from the machine description and why this pass, uniquely in the tree middle end, is deeply
target-aware.

The optimization is a set cover: choose a set of candidates minimising total cost, where each use is
served by its cheapest available candidate. That is NP-hard in general, so GCC does greedy search
with pruning, bounded by three parameters: `iv-max-considered-uses` `Init(250)`
(`gcc/params.opt:364`), `iv-consider-all-candidates-bound` `Init(40)` (`gcc/params.opt:360`) below
which the search is exhaustive, and `iv-always-prune-cand-set-bound` `Init(10)`
(`gcc/params.opt:356`).

## 28.3 What rucc builds

Document 07.4 already commits to affine chains of recurrences plus pointer chrecs, which is the
analysis half: for each value in the loop, is it of the form `base + i*step`, and what are `base` and
`step`. That analysis is a prerequisite and it exists for trip counts anyway.

**On top of it, M4 builds a restricted ivopts of perhaps 800 lines.**

*Use collection.* Addresses, comparisons, and other uses, grouped by base modulo a constant offset,
exactly as GCC does. The grouping is the cheapest large win in the pass.

That last sentence is worth holding against the measurement in 28.8, which says the grouping is
smaller than it sounds. On the corpus it takes 2455 uses down to 2123 groups, so about one use in
seven is merged away, and GCC's own grouping over the same sources takes 1947 down to 1909. It is
worth having, since the cost table has a row per group, and it is not where the pass is won.

*Candidate generation.* The existing induction variables, plus one candidate per distinct
(base, step) among the address uses, plus the "final value" candidate for the loop's exit comparison.

*Selection.* Greedy: start with the set of original induction variables, repeatedly consider adding
the candidate that most reduces total cost, stop when nothing improves. Bounded by GCC's three
parameters. Not exhaustive even below the 40-candidate bound, because the exhaustive search's value
over greedy has never been published and rucc can measure it later.

*Costs from document 40, target-parameterised.* This is the place where document 40's cost model must
be genuinely target-aware rather than a table of nominal instruction counts, and it needs three
things per target: the legal addressing mode forms, the cost of an address computation not expressible
as an addressing mode, and the number of allocatable registers for the set-size penalty.

*Rewriting*, then document 17's DCE removes the now-dead original variables. The pass does not delete
anything itself, which keeps it simpler and follows the general discipline of one job per pass.

## 28.4 Linear function test replacement

The sub-transformation worth naming separately, because it is where the correctness risk is.

`for (i = 0; i < n; i++) *p++ = 0;` has two induction variables, `i` and `p`, and only `p` is used in
the body. Rewriting the exit test from `i < n` to `p < limit` where `limit = p0 + n` removes `i`
entirely.

**The trap: the rewritten comparison must be equivalent, including at the boundaries.** `p < p0 + n`
where the multiplication `n * sizeof(*p)` overflows the pointer type is not equivalent to `i < n`.
And a signed counter rewritten as an unsigned pointer comparison changes behaviour when the original
would have wrapped.

GCC handles this by requiring the new comparison to be provably equivalent given the trip count
analysis, and the trip count analysis's `assumptions` field (document 07.5) is where the conditions
land. rucc does the same: the rewrite is performed only when the trip count is a `Bound` rather than
an `Estimate`, and when the derived limit provably does not overflow.

**What the proof is when the count is an expression.** Most loops have one. `for (i = 0; i < n; i++)` gets a count of `n` rather than a number, and the limit for one of those is the count worked out in the preheader, clamped at zero and multiplied by the step. The clamp is the entry assumption paid for rather than leaned on, the same trade 28.9 makes: a count taken from a distance that came out negative is a loop that runs no times, and a limit equal to where the pointer starts is a test that refuses the first time it is asked.

The overflow proof is the part with no number to check, and what stands in for the number is a bound on it. A count built on a value of `b` bits is at most two to the `b` whichever way the exit test read it, so the whole walk is at most that, times the count's own scale, plus its offset, times the step. Worked out in a hundred and twenty eight bit number and refused unless it fits in sixty four, which is the same check the number case makes, asked of a bound instead. `for (int i = 0; i < n; i++)` over four byte elements needs thirty four bits and passes with room to spare, and that is most of the loops a C program writes. A counter as wide as a pointer has no width to argue from and is refused.

GCC 16 rewrites the pointer-wide case anyway. `for (size_t i = 0; i < n; i++) t += p[i];` comes out as `cmpq %rcx, %rdi; jne` with the limit formed by one `leaq` in the preheader, and the argument it must be resting on is that the loop forms every one of those addresses itself, so a walk that wrapped the address space was a program that had already left its object. That argument is a real one and rucc does not make it, because it rests on the program being free of undefined behaviour rather than on anything in the IR, and the measurement that would say what it is worth has not been made. `crate::range` is the other way to widen this, since a length checked before it is walked is bounded by the check and the width is only what is left when nothing checked it.

The neighbouring transformation is 28.9, final value replacement, and the two are worth reading
together because one of them needs the overflow proof above and the other looks like it does and
does not.

The second half of the same transformation is the **countdown form**: rewriting `i = 0; i < n; i++`
as `j = n; j != 0; j--`, so the exit test is a comparison against zero, which most targets get for
free from the decrement's flags. GCC's ivopts does this as part of doloop support
(`gcc/tree-ssa-loop-ivopts.cc:70`), adding a dedicated candidate
`{(may_be_zero ? 1 : (niter + 1)), +, -1}` for targets with hardware loop instructions.

rucc's targets are x86-64, AArch64 and RISC-V64, none of which have hardware loop counters, but all
of which have cheap compare-against-zero. So the countdown rewrite is worth doing for the flags, not
for a loop instruction, and it should be a candidate in the selection rather than an unconditional
rewrite, because it is only profitable when nothing else needs `i`.

**What stops it, measured.** The candidate is there. `candidates` makes one 504 times over the corpus and 35 times in the SQLite amalgamation, which is only the loops whose trip count is a number rather than an expression, and it has never once been chosen. Three separate things were in the way, and the first is the only one this was expected to be.

The first is that the exit group cost nothing whichever candidate served it. That was right as far as it went, since the point being made was that the test can be asked of any variable and is therefore no reason to keep the one it names, and it hid the whole of what a countdown buys, which is not a cheaper comparison but no comparison at all. The cost table now has a row for each shape, `compare_reg` and `compare_zero`, because whether those two differ is a fact about the machine rather than about the pass. They differ on x86-64 and on AArch64, where the decrement sets the flags the branch reads, and they do not differ on RISC-V64, where compare and branch are the same instruction either way.

The second is that the search could not express the exchange. Selection starts from the loop's own variables and took one candidate in or out at a time, and a countdown is neither of those moves. Put next to the counter it buys the comparison and pays for a second variable, which on x86-64 is exactly a wash, and taking the counter away first leaves a set that serves no use at all. Neither half is an improvement on its own, so the search never arrived at the set where both have happened, however much cheaper that set is. `select` now considers replacing a member of the set with a non-member as a single move, which is still greedy and is the move GCC's `try_improve_iv_set` has.

The third is that being chosen would not help yet. The rewrite half only knows how to make address walks: it collects the groups whose own derived candidate was chosen, and when there are none it returns before it looks at anything else, so `Origin::Countdown` appears nowhere in it. Lifting the bias, which is the only way the search picks a countdown with today's numbers, duly gets the selection to report a changed set for `for (i = 0; i < 1000; i++) f();`, and the assembly that comes out is the same `addl $1, %ebx; cmpl $1000, %ebx` it was before.

So neither the price nor the exchange moves a byte. Over the 2663 corpus sources, pricing the exit test changes nothing, the exchange move changes nothing on top of that, and lifting the bias as well changes nothing on top of that, all three measured as assembly text at `-O2`. The first two are right regardless of whether anything uses them, and they are what the third needs. The bias is the question in front of the rewrite: a countdown costs `IVOPTS_NEW_VARIABLE_BIAS` at three increments and buys one cycle on x86-64, so the search will go on refusing it while the bias applies to it in full, and whether a variable that replaces one should pay the same preference as a variable that joins one is a question for the corpus rather than for a comment.

**What the rewrite does, measured.** `count_down` is the third thing, and it is much shorter than the walk rewrite next to it because a countdown serves no use: there is nothing in the body to repoint, so the whole of it is a variable, a decrement and a test. The interesting part is where the decrement goes.

It goes in the header, in front of the test, rather than in the latch behind it. The two are the same arithmetic and they are not the same code. The whole saving claimed here is that the decrement leaves the flags the branch reads and the comparison is then no instruction at all, and what collects that saving is the redundant comparison pass in the back end, section 37.4, which takes out a comparison against zero of a register the instruction in front of it just wrote, in the same block, with nothing in between. A decrement in the latch is in another block and reaches the test round a back edge, so the comparison stays and the loop has paid for a second variable and bought nothing. That was measured before it was reasoned about: the first version put the decrement in the latch and came out 196 instructions worse over the corpus.

Putting it in the header costs one on the number the variable starts at. The test is now on the decremented value, so the header runs out a turn earlier than it would have, and starting at one above the trip count puts it back. The header runs one more time than the latch does, which is the turn the test refuses on, and that is the turn the extra one pays for.

Two smaller things fell out of the same measurement. The exit test has to be `!=` rather than an ordering, because a subtraction says whether its answer was zero and a comparison of that answer against zero would agree, and it says whether the subtraction overflowed where the comparison would have said it did not. `rucc_target::Zeroing` marks `sub` good for equality and for nothing else, so an ordering here leaves the test standing. And the decrement has to carry both `nsw` and `nuw`, because a test with no sign in it is read unsigned and a loop whose count rests on an assumption nothing discharges is a loop `loop-delete` will not take out. Getting that wrong turned eight fully deleted loops in the corpus back into live ones, which is a rewrite paying for itself with somebody else's money.

The numbers. `for (int i = 0; i < 1000; i++) f();` went from `addl $1, %ebx; cmpl $1000, %ebx; jl` to `subl $1, %ebx; je`. Over the 2663 corpus sources at `-O2`, measured as assembly text, 183777 instructions became 183704. Sixty eight sources differ: 47 are smaller, 18 are the same size written differently, and 3 are larger. The rewrite fires 86 times and refuses 4 loops because the number of turns and one over does not fit the type the loop counts in.

The three that got larger are all the same shape and the worst is 4 instructions. A countdown for an inner loop has to be set back to its starting number every time the outer loop comes round, and where the counter it replaced was already in a register the allocator had, the new variable takes a callee saved one and brings a push, a pop and a couple of moves with it. That is a real cost the model does not have a row for, and it is small enough to leave until something makes it bigger.

Two places where nothing happened are worth recording. The SQLite amalgamation went from 243607 instructions to 243606, one, because `candidates` only makes a countdown for a loop whose trip count is a number and almost every loop in SQLite counts to something it works out. Widening that to a count the loop works out in its preheader is the obvious next thing and is tamnd/rucc#1696. And `-Os` is unchanged on every one of the 2663 sources, because ivopts is not in the pipeline at `-Os` at all.

The bias question the paragraph above left open is answered in the pass rather than in the corpus. `IVOPTS_NEW_VARIABLE_BIAS` is a preference against a variable the pass invents to serve uses, which is one more thing in a register than the program asked for. A countdown serves no use, the only reason to have one is to take the exit test off the counter, and the pass will not rewrite the test while anything else still wants the counter. So it is a variable that replaces one rather than one that joins one, the set it is in is the size the set was, and charging it the preference would be charging it for a register it does not take. It is exempt.

## 28.5 Straight-line strength reduction

`gcc/gimple-ssa-strength-reduction.cc:20` opens with the best one-line description of a pass's scope
in the source tree:

> There are many algorithms for performing strength reduction on loops. This is not one of them.
> IVOPTS handles strength reduction of induction variables just fine. This pass is intended to pick
> up the crumbs it leaves behind, by considering opportunities for strength reduction along dominator
> paths.

The case: `a = b * 4; ... c = (b + 1) * 4;` becomes `c = a + 4`. Not a loop, so ivopts never sees it,
and a multiply becomes an add. It also handles multiplies implicit in addressing.

The header notes the restrictions: integer only, and division and modulo are not attempted because
"such opportunities are relatively uncommon."

**Not in M4.** The reasoning: document 12's e-graph plus document 19's reassociation already
canonicalize `(b+1)*4` into `b*4 + 4`, and hash-consing then recognises `b*4` as the existing `a`.
That is the same result reached by a mechanism that exists for other reasons. Whether it actually
happens is measurable, and document 42 should check specifically for it, because if the e-graph does
not produce this then a 4,162-line GCC pass is doing something rucc has no answer for.

This is one of the cleaner tests of the document 12 thesis and it is worth calling out as such.

## 28.6 The relationship to the rest of the loop pipeline

**Ivopts runs last** among the loop passes that operate on a single loop, after LICM and before or
after unrolling depending on a judgment call.

Before unrolling: the unroller duplicates a body already expressed in good form, and each copy needs
its offsets adjusted, which is easy since they are constant offsets from one base.

After unrolling: ivopts sees the unrolled body's four uses at offsets 0, 4, 8, 12 and groups them,
which is exactly what its constant-offset grouping is for, and picks one candidate for all four.

**GCC runs ivopts after unrolling** and rucc should do the same, for the grouping reason. That means
the unroller must produce addresses in a form ivopts recognises, which is document 29's obligation.

**Ivopts and vectorization conflict.** The vectorizer wants to see the original array references,
`a[i]`, not a pointer rewritten by ivopts. GCC's answer is that the vectorizer runs before ivopts.
rucc's vectorization is post-M4, and the ordering constraint should be recorded now: ivopts is the
last thing in the loop pipeline, and anything that wants to pattern-match array accesses runs before
it.

**And there is a tension with document 19's pointer arithmetic normalisation.** Document 19.6
normalises to `base + i*scale + C`. Ivopts then rewrites that into an incremented pointer. The two
are not in conflict, because 19's form is what ivopts consumes, but it does mean that after ivopts
the pointer arithmetic is no longer in 19's canonical form and any pass running afterwards must not
assume it is. Document 37's addressing mode selection consumes what ivopts produces.

## 28.7 How this is wrong

**The rewritten exit condition is not equivalent.** 28.4. This is the highest-severity bug in the
document: an off-by-one in the exit test writes one element past the array, and it depends on the
trip count, so it fires on some inputs and not others.

**The derived limit overflows.** `p0 + n*4` where `n` is large. In C, pointer arithmetic past
one-past-the-end is undefined, so the compiler may assume it does not overflow, and then computes a
limit that wraps and a loop that never terminates. The assumption is legal and the consequence is a
hang. rucc's rule: the limit computation must be within the object's bounds by the same reasoning
that made the original loop valid, and where that cannot be shown, the rewrite is not made.

**Signedness changes in the rewritten comparison.** A signed `i < n` becoming an unsigned pointer
comparison is a different test when the original could have been negative. Header copying and range
analysis usually establish that `i >= 0`; where they do not, the rewrite waits.

**Too many induction variables are created and the loop spills.** The set cost. Same failure as
document 27.2's and it is more likely here, because ivopts's whole job is to create variables.

**The chosen set is worse than the original.** Possible whenever the cost model is wrong about the
target, and it is why GCC's cost function "somewhat prefers" the original variables. rucc should
have the same bias, and it should be a real number in document 40 rather than a tiebreak, because on
a target whose costs have not been tuned the original variables are the safer default.

**A candidate is derived from a chrec that is not actually affine.** Document 07.4's analysis must
say `Affine` or `Unknown`, never guess. A use whose evolution is not affine is not an induction
variable use and belongs in the "other" category, rewritten in terms of nothing.

**The pass runs on an irreducible region.** No loop, no preheader, no trip count. Skipped, per
document 26.2.

## 28.8 What it costs

Use collection is one walk of the loop body. Candidate generation is linear in uses. Selection is the
expensive part: greedy selection with a cost evaluation per (use, candidate) pair is
`O(uses × candidates)` per step and `O(candidates)` steps, so cubic in the worst case, which is why
all three of GCC's parameters exist. With uses capped at 250 and candidates at 40 that is bounded but
not small, and ivopts is one of the more visible passes in a GCC `-ftime-report`.

rucc's greedy version with the same caps has the same shape. The cost evaluation must be cheap, which
means the addressing-mode legality query in document 40 must be a table lookup and not a call into
instruction selection.

The measurement in document 42, and there are three worth having:

- Inner loop instruction counts against `gcc -O2` on the loop-heavy corpus. This pass is where that
  comparison is won or lost.
- How often the greedy selection differs from the original variable set, which tells whether the pass
  is doing anything.
- The straight-line strength reduction check from 28.5, testing the document 12 thesis directly.

The second of those is taken by `spec/optimizer/tools/ivopts-survey.py`, which reads GCC's
`-fdump-tree-ivopts-details` and rucc's `-fopt-info-all` over the same sources and prints the same
tables for both. Over the 2663 sources of the corpus, GCC's ivopts processes 786 loops and rucc's
770, which is close enough to say the two passes are looking at the same programs.

**Not one of the three bounds binds anywhere.** The largest group count is 16 for GCC and 35 for
rucc against a bound of 250, the largest candidate count is 23 for GCC and 19 for rucc against a
bound of 40, and the largest set either compiler keeps is 3 against a prune threshold of 10. So the
bounds are in for the reason GCC has them, which is that a pathological function exists somewhere
and the pass should degrade rather than hang, and not because they shape the answer on anything
measured here. rucc's use bound is counted against raw uses where GCC's is against groups, which
makes rucc's the stricter of the two, and neither is close enough to the limit for the difference to
matter.

**The two searches are not the same size.** GCC considers 8 candidates per loop on average and rucc
4. The list in 28.3 is where that comes from: GCC adds position variants of each candidate, before
the loop and at the exit, and zero-based and one-based forms of each basic induction variable, where
rucc generates one candidate per distinct base and step. The answers nonetheless agree on the thing
that matters, which is how many variables the loop ends up with: 86 per cent of GCC's loops keep one
and 89 per cent of rucc's do, and neither keeps more than three. A wider candidate list is therefore
not obviously worth buying, and the measurement to make before buying one is what the extra
candidates would cost rather than how many there are.

**The selection does something.** rucc keeps the variables the loop came with in 508 of its 770
loops and changes the set in 262, which is the answer to the second measurement above.

## 28.9 Final value replacement

The value a variable holds after the loop, written down in front of the loop instead of being
arrived at by running it. GCC does it in `scev_const_prop` in `gcc/tree-scalar-evolution.cc`, and
it is the transformation that makes loop deletion apply to loops anybody actually writes, because
a loop whose result nothing reads is rare and a loop whose result is read once afterwards is
everywhere.

`for (i = 0; i < 1000000; i++) total += seed;` followed by a use of `total`. The value the loop
hands over is the sum computed in the body, which is `seed` the first time round and goes up by
`seed` every time after, so it is `{seed, +, seed}`. The trip count is 999,999. The closed form is
`seed + seed * 999999`, which folds to `seed * 1000000`. GCC emits the one multiply,
`imull $1000000, seed_in(%rip), %esi`, and no loop at all. Once that is written down, nothing
outside the loop reads anything the loop computes, and the loop goes by 17.1.

**The off-by-one is the part to get right.** The count is the iteration at which the exit test
first fails, which is how many times the back edge is taken, and it is one less than how many
times a block in front of that test runs. The closed form of a chrec at the count is the value it
has on entry to the iteration that leaves, so a caller wants either `base + step * count` or the
same thing at `count + 1`, depending on where in the loop the value it asks about is defined.
Taking the chrec of the value that is actually handed over, rather than of the one the header
carries, is what puts the `seed +` on the front above. Getting this wrong is a wrong answer on
every input rather than on some of them, which is the one mercy of it.

**It needs no overflow proof, and 28.4 does.** This reads like the same correctness problem and it
is not. A value that steps by a fixed amount evolves in its own type, which is to say modulo two
to the width, and addition modulo two to the width is associative, so adding `step` to `base`
`count` times and working out `base + step * count` in the same width are the same number whatever
either of them does to the top bit. 28.4's claim is a different one: that one comparison holds
exactly where another does, and a limit that wraps makes that false. So the arithmetic written
down here carries neither `nsw` nor `nuw`, and the promise the loop's own increment carried is not
copied onto it, because that promise is about the sequence and says nothing about the closed form.

**What the trip count has to be.** Two different answers for two different questions, and
conflating them is the mistake this section exists to prevent.

Deleting the loop needs only that the loop comes back. `for (i = 0; i < n; i++)` comes back
whatever `n` is: the counter either reaches `n` or is already past it, and both are a finite
number of steps. So a count that is an expression rather than a number is enough, and document
07.5's assumption for this case, that the loop is entered at all, is about which number the count
is rather than about whether there is one.

Writing the final value down needs the count to be the right number, because it is multiplied by.
A symbolic count is usable there too, but only with the entry assumption discharged, which means
clamping it at zero, and with the widening the exit test's reading calls for. 07.7 is the warning
about the second of those: a limit past the middle of a thirty two bit type is a large number to
an unsigned test and a negative one to a signed test, and a closed form computed from the wrong
reading turns a loop over three billion elements into one that runs no times.

A loop ending on `!=` is neither of those. It stops on the one iteration where the counter is the
limit, so a counter that starts past the limit, or that steps over it, goes round until it wraps.
That is a loop that may not come back, and 17.2 says rucc does not delete one of those. The
assumption is separate for exactly this reason.

**When it is worth doing.** Only when it lets the loop go. Writing the closed form down in front
of a loop that stays costs a multiply and saves nothing, because the loop still carries the value
round its own back edge. That is a cost rule rather than a correctness one, and it should be
reconsidered when there is a pass that removes a block parameter whose only reader is the argument
it passes to itself, which 17.1 already names as a transformation worth having.

**Where it goes in the pipeline.** Last, after everything that might make a loop empty. That has a
consequence worth writing down: loop closed form is gone by then. `canon` puts a block in the way
of every exit edge so a value leaving a loop leaves through a block parameter, and `simplify-cfg`
has every reason to fold that block away again, so by the end of the pipeline a value defined in a
loop is named directly in the block after it. A pass doing this has to handle both roads out, and
a pass that handles only the loop closed form one deletes nothing in a real program.

**rucc's position.** Both halves are in `crates/rucc-opt/src/loop_delete.rs`, one pass rather than
two, because the value question is only asked to make the loop question answerable and a pass that
answered it on its own would be writing multiplies nobody wanted. The closed form is worked out on
the invariant representation before anything is written down, since this is the last pass and
there is no later fold, so a loop adding one a million times leaves a constant behind and a loop
adding an invariant leaves one multiply.

Symbolic counts are taken for both questions. The count is clamped at zero in front of the loop,
which is the entry assumption paid for rather than leaned on, and it is read the way the exit test
read it, a sign extension for an ordering on signed values and a zero extension for one on
unsigned. The clamp is done in sixty four bits and the product is cut back to the type the value
evolved in, which loses nothing, since cutting a product and multiplying a cut are the same number
modulo two to the width. On the corpus `loop-deletion` at a million iterations with an unknown
bound and its total read afterwards went from 5,826,206 instructions to 835,834, where gcc 16
pays 804,628.
