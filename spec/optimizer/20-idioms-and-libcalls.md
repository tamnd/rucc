# 20. Idiom recognition and library calls

Two related things. Recognising that a loop or an expression written by hand is really a named
operation the machine or the library implements in one step, and knowing enough about the C library
to fold, specialise or eliminate calls into it.

Both are compatibility obligations as much as optimizations. A program compiled by GCC gets
`strlen("hello")` folded to 5 and gets a zeroing loop turned into `memset`, and code written on the
assumption that this happens is common enough that not doing it shows up as a performance complaint
rather than a missed-optimization report. And on the other side, a freestanding build where the
compiler emits a call to `memcpy` that does not exist is a link failure.

The relevant GCC sources are large: `gcc/builtins.cc` at 12,777 lines, `gcc/gimple-fold.cc` at
11,597, `gcc/tree-ssa-strlen.cc` at 6,173, `gcc/tree-loop-distribution.cc` at 4,037 and
`gcc/gimple-ssa-sprintf.cc` at 4,773. `gcc/builtins.def` declares 898 builtins in 1,289 lines, of
which about 340 are library builtins with the `DEF_LIB_BUILTIN` family of macros.

## 20.1 What a builtin is, and the three-way split

The word covers three different things and conflating them is the source of most confusion.

**Compiler intrinsics** with no library counterpart: `__builtin_expect`, `__builtin_unreachable`,
`__builtin_constant_p`, `__builtin_clz`, `__builtin_trap`, the overflow-checking arithmetic builtins,
the atomic builtins. These have no external symbol; the compiler must implement them or fail.

**Library functions the compiler knows about**: `memcpy`, `strlen`, `malloc`, `abs`, `printf`. These
exist as symbols and the compiler additionally knows their semantics, so it can fold, specialise,
attach attributes, or emit them without the program having declared them.

**Target intrinsics**: `__builtin_ia32_*` and the like. Not M4.

For 100% GCC compatibility the first group is mandatory and its list is fixed by what real headers
use. The second group is where the optimization is.

**The flags that turn it off.** `-fno-builtin` means the compiler may not assume a function with a
standard name has standard semantics, because the program may have redefined it. `-ffreestanding`
implies `-fno-builtin` for most of the library. GCC keeps folding `__builtin_memcpy` under both,
because the explicit `__builtin_` prefix is the programmer asserting the semantics. rucc follows
exactly this, and the test is a translation unit defining its own `strlen` compiled with and without
the flag.

There is one asymmetry worth knowing: even under `-ffreestanding`, GCC may still *generate* calls to
`memcpy`, `memmove`, `memset` and `memcmp`, because struct assignment has to lower to something. The
C standard permits this and the kernel accommodates it by providing those four. rucc must document
that it does the same, and must not generate calls to anything else.

## 20.2 Folding calls with constant arguments

The cheapest and most valuable group. `strlen` of a string literal is its length. `memcpy` of a
constant small size is a sequence of loads and stores. `strcmp` of two literals is a constant.
`abs` of a constant is a constant. Every one of these is a rewrite rule in document 13's terms
except that the operand is a call rather than an arithmetic node.

**How rucc expresses them.** As rules, in the same DSL, with the call's callee identity as part of
the pattern. That requires the rule language to match on a call node with a known target, which is a
small extension and worth making, because the alternative is a hand-written folder that grows a
special case per function and is never verified.

The SMT obligation from document 13.3 does not apply to most of these: the specification of `strlen`
is not expressible in the bitvector theory the verifier uses. So they are marked `unverified` per
13.3 and their correctness rests on tests. That is the honest position and the marking makes it
visible rather than pretending.

**The size threshold.** `memcpy` of 16 bytes becomes two 8-byte load-store pairs. `memcpy` of 4096
bytes stays a call. The crossover is a cost-model number, it depends on the target's vector width
and on whether the library's implementation is good, and it belongs in document 40. GCC's equivalent
knob for the comparison case is `builtin-string-cmp-inline-length` (`gcc/params.opt:125`).

**The one group that cannot be a rule.** `printf("hello world\n")` writes what `puts("hello world")`
writes, and a program is entitled to notice which of the two it was compiled into: the `builtins`
cases of `gcc.c-torture/execute` define a `puts` of their own and abort when it is not called. A rule
rewrites one instruction into instructions, and this rewrite needs two things a rule has no way to
reach. The name `puts` has to be interned before any call can carry it, and the string handed to it
is not one the module already holds, since "hello world" with a terminator is not a suffix of
"hello world\n" with one. So the printf family is a module at a time transformation beside the two
in document 34.6 rather than a rule, it runs before the function pipeline starts, and it is at `-O1`
and above where gcc folds these as well. The rules themselves are in `crates/rucc-opt/src/libcall.rs`
and the three way split of 20.1 turns the whole of it off.

**The other way round, which is a call that answers rather than writes.** `strstr(s, "")` is `s`,
since the empty string is found at once wherever it is looked for. `strstr(s, "w")` is
`strchr(s, 'w')`, which is a search for a character rather than for a string and is worth doing
wherever the haystack came from. `strstr` of two strings the module holds is the answer itself,
which is a place in the haystack or a null pointer, and nothing is called at all. These live in the
same module at a time pass because the second of them has to name `strchr` before a call can carry
it, and they differ from the printf family in what the result is for: a `printf` whose result is
read is left alone, and a `strstr` is folded whether its result is read or not.

**The rest of the family that answers.** `strchr`, `strrchr`, `strlen`, `strnlen`, `strcmp`,
`strncmp`, `strspn`, `strcspn`, `strpbrk` and `memchr` over strings the module holds are each a
number or a place in one of their arguments, worked out the way the library would work it out.
`index` and `rindex` are the older spellings of the first two and are folded as the same two
searches, which is what `gcc.c-torture/execute/builtins/strchr.c` and its `strrchr` companion ask for
beside the modern names. Three of them have a shape rule that does not need the string at all:
nothing is inside an empty set, so `strspn(s, "")` is zero, `strcspn(s, "")` is `strlen(s)`, and
`strpbrk(s, "")` is a null pointer whatever `s` is, and a set of one character makes
`strpbrk(s, "c")` a `strchr(s, 'c')` the same way a needle of one character does for `strstr`. A
fourth is that a string has one terminator in it, so `strrchr(s, 0)` is `strchr(s, 0)` and which end
the walk started at stops mattering. `strlen` answers two more shapes gcc 16 answers. A pointer that
may be any of several held strings is their length where they all have the same one, which is what
`foo` in `builtins/strlen-3.c` is after a loop that picks one of four. A held string stepped into by
an amount the program works out is its length less the step, where the arithmetic that made the step
says it is no more than the length: a mask, a remainder by a constant and a widening of either are
read for how large they can be, so `strlen("hello world" + (x & 7))` is `11 - (x & 7)`. A step that
can pass the terminator is left as a call, since past it the compiler knows nothing. The third
shape is a local array the program has just written a string into a byte at a time, as `str` in
`builtins/strlen.c` is. The walk goes back from the call through its own block, keeps the last
constant byte stored at each place in the array, and stops at a call, at a store wider than a byte
and at a store through anything that is not a local, since each of those could have written the
array some other way. A store into another local is passed over, because two locals are two objects.
The answer is the length only where the bytes it kept reach a terminator from the place asked about
without a gap. `strcpy` takes the same length a choice of strings of one length has, so the loop in
`builtins/strcpy-2.c` that leaves one of four seven character strings behind is a `memcpy` of eight
bytes.

**Moves that cannot overlap.** `memmove` of nothing is its destination, and `memmove` is `memcpy`
where the two sides cannot overlap. That is so for a single byte, which is read before it is written;
for a read only source whose definition the link cannot swap, since the destination is written and
the source cannot be; and for two different objects of which one is a local, since a local overlaps
nothing else. A local and a pointer loaded from somewhere are not two objects, because the pointer
may hold the local's address, and two places in one global are not either. `bcopy` is the same move
with its addresses the other way round and no answer, so a `bcopy` that moves nothing goes and one
that cannot overlap is a `memcpy` whose answer nothing reads. A comparison answers one of minus one, zero and one, because the
sign is what the standard promises and the magnitude is not, and that is what gcc leaves behind as
well.

**Two of them read a count rather than a terminator.** `memchr` and `strnlen` are told how many
bytes they may look at, so they read the object rather than the string in it, and `memchr` finds a
byte that sits past the terminator where the object still holds one. That makes the object's size
the limit rather than the string's length: a count the object does not have that many bytes for is a
read whose end the compiler cannot see, and the call stays. The count itself is almost always
written as an `int` in the source and widened on the way into a `size_t` parameter, so the pass
looks under the widening for it, and refuses a source constant that was negative.

`strnlen` over a string whose terminator is inside the object is the exception to both. It reads
no further than the terminator whatever the count is, so the answer is the smaller of the length
and the count, and every count is one the call could have been given, including one the source
wrote as a negative number, which is a very large one once it is a `size_t`. Where the count is not
known at all the answer still is not a call: the empty string is zero, and anything longer is the
smaller of the two worked out when the program runs, which is an `icmp ult` and a `select` in the
count's own type. That is what gcc leaves behind as well, and it is what
`gcc.c-torture/execute/builtins/strnlen.c` asks for with `strnlen ("12", x & 3) <= 2`. That program
looks like a question about value ranges and is not one, because the answer does not need the
range of the count, only the count.

**The pass runs before anything has folded.** It is one walk over the module ahead of the function
pipeline, so a count or an index the source worked out from constants is still the arithmetic when
it looks: `s1 + (x & 3)` with `x` known is a `ptr_add` of a `sext` of an `and` of two constants, and
`++n` is an `add`. Every count, character and index the pass reads goes through
`fold::evaluated`, which looks a few instructions deep through integer arithmetic over constants
and answers with the same arithmetic the constant folder uses, so the number it reads is the number
that pass would have written later. It changes nothing in the function. The depth is the same
bound the address walk has, which is enough for anything written in a single expression and keeps
the walk from following a long chain.

**One of the comparisons answers without either string.** `strcmp` and `strncmp` read the first
byte of both of their strings before they can answer anything, so where one string is known and the
other is not there is still an answer in the two cases that first byte settles on its own: a
`strncmp` whose count is one, which is that byte and nothing else, and a comparison against the
empty string, whose terminator stops the walk however many bytes the count allowed. The answer is
the difference between the byte the compiler knows and the byte the other string holds, both read as
`unsigned char`, which is what the standard says a comparison compares, and it is the one answer in
this pass that is an instruction rather than a constant or an address, because that byte is in
memory and has to be read. The read is safe wherever the call was, since the call was going to make
it. A count of zero is simpler still: it reads neither string, so the answer is zero whatever the
two of them hold and whether they are there to be read at all. A comparison declared to answer
something no wider than a byte is left alone, because there is no room in it for the difference.
`gcc.c-torture/execute/builtins/strcmp.c` writes `strcmp (bar, "")` and `strcmp ("", bar)` over a
mutable global, and aborts if the library is reached, so passing it means the fold happened.

**A checking call whose check cannot fail is the plain call.** `_FORTIFY_SOURCE` turns `memcpy(d, s,
n)` into `__memcpy_chk(d, s, n, size)`, where `size` is what `__builtin_object_size` said the
destination holds, and the checking function aborts when the copy would not fit. Where the size is
all ones, which is the builtin saying it does not know, or where the count is known and fits, the
check is one that cannot fail and the call is the plain one with the size dropped. The same rule
covers `__memmove_chk`, `__mempcpy_chk` and `__memset_chk` with a count, `__strncpy_chk` and
`__stpncpy_chk` with a count as well, `__strcpy_chk` and `__stpcpy_chk` with the longest the source
string can be plus its terminator, and `__snprintf_chk` and `__vsnprintf_chk` with the bound they
were given. `__sprintf_chk` and `__vsprintf_chk` have a flag as well as a size, and are plain only
where the flag is zero or the format is one the compiler can read and holds nothing that could write
through `%n`. Where the check can fail, gcc still makes the call cheaper, and so does this pass: a
checking `stpcpy` or `mempcpy` whose answer nothing reads is the `strcpy` or `memcpy` version, and a
checking `strcpy` of a known string is a checking `memcpy` of its length plus one. An append of the
empty string, or a counted append of zero bytes, is the destination, and a counted append whose
count covers the whole of a known source is the uncounted one. A count is read as the largest number
it can be, through a conditional expression or a block parameter, so `l1 ? sizeof (buf) : 4` fits an
eight byte `buf` whichever arm was taken. The plain calls these leave behind have folds of their
own: `strcpy` and `stpcpy` of a known string are `memcpy` of its length plus one, `sprintf` of a
format with nothing to convert, or of `"%s"` and a known string, is `strcpy` answering the length,
`strncat` whose count covers a known string is `strcat`, and `mempcpy` is `memcpy` with the end of
the copy worked out beside it, which is an address this can write only where the count is known.
Every one of these rules is gcc 16's, measured by compiling the same calls with it. The plain name
has to be one the module either does not declare or declares with the shape the call has, since a
program with its own `memcpy` of some other shape has a function the pass knows nothing about. Once
a call is plain the ordinary folds apply to it, so the pass looks at a function more than once, and
three rounds is the longest chain any of these makes: a checking `sprintf` of a literal is
`sprintf`, which is `strcpy` answering the length, which is `memcpy`. Every plan in a round is
worked out from the body as it was, so a plan applied after another is renamed through what the
other one replaced: in `mempcpy (mempcpy (p, a, 4), b, 4)` the second copy's destination is the
first one's answer, which the first fold took away.

The object size gcc works out after lowering is `rucc_opt::objsize`, which runs before anything
else in every pipeline. Where the front end cannot see the object behind an address read out of a
local, it leaves the question in the IR as an `object_size` instruction rather than answering all
ones, so a destination picked by a branch or a loop, `r = l1 == 1 ? &a.buf1[5] : &a.buf2[4]`, is a
block parameter or a select whose every incoming pointer is in front of the walk. The walk follows
those, and constant non-negative steps, back to an `alloca` or a global whose size can be vouched
for, and takes the larger of the arms for kinds zero and one and the smaller for kind two. A pointer
a loop moves forward is known for the largest kinds only, since each trip can only leave less, and
one it moves backward or by an unknown amount is not known at all. Kind one is answered about the
whole object, because the IR no longer knows the member, which is the larger answer and so never
checks a copy gcc would let through. Kind three is always zero, the answer that checks nothing. At
`-O0` every question is answered as not known, which is what gcc says there too. An address read
out of a parameter or a global is still answered by the front end, because the IR has no more of
where it came from than the front end did, and answering it there is what lets a checking call over
one be the plain call at `-O0`. The string and count walks this pass does over a call's arguments
look through the same block parameters, so `stpcpy (&buf3[16], l)` with `l` chosen in a loop from
three literals still has its length. The `test3` functions of the `builtins/*-chk.c` torture
programs count exactly those checks, and all of them pass at every level.

**A rename does not hide the function.** `extern char *strstr (const char *, const char *) __asm
("my_strstr");` declares the standard `strstr` and says the symbol is `my_strstr`, and a pass with
only the symbol in front of it sees a call to something it has never heard of. The IR carries the
name the source spelled beside the symbol, per document 08.8, and this pass reads it to decide what
the call is, and reads the module the same way to decide what symbol a replacement should name, so
a module that renamed `strchr` as well gets a call to the symbol it renamed it to.
`gcc.c-torture/execute/builtins/strstr-asm.c` is the program written to catch a compiler that got
this wrong, since it aborts unless eight of its nine calls fold.

**The trap.** Inlining `memcpy` as loads and stores requires knowing the alignment, or using
unaligned accesses, and the result must not read or write outside the copied range. An implementation
that rounds the size up to a convenient width writes past the end. This is the single most likely
wrong-code bug in this document and the defence is that the expansion is generated by one function
with an exhaustive test over sizes 0 to 64 and alignments 1 to 16.

## 20.3 Attributes, which are worth more than folding

A call the compiler knows nothing about clobbers all memory, per document 08.4. Knowing that
`strlen` is `pure`, that `malloc` returns a pointer that does not alias anything live, that `memcpy`
writes exactly `n` bytes at the destination and reads exactly `n` at the source, is what lets
redundant load elimination and dead store elimination work across library calls at all.

This is a table, not an algorithm. Roughly forty entries, listed in document 08.4, each recording:
whether the call reads memory, whether it writes memory, which arguments it may read or write and
how much, whether it may not return, whether its result aliases anything.

Two entries carry more weight than the rest.

**`malloc` and friends return non-aliasing pointers.** A freshly allocated block cannot alias
anything the program already holds a pointer to. GCC expresses this as the `malloc` attribute and it
is the single most valuable alias fact available in C, because it is the only way a compiler ever
learns that two pointers are distinct without a `restrict` annotation. rucc's alias analysis must
consume it and document 08's layer structure must have a place for it.

**`memcpy`'s access ranges make it transparent to load elimination.** Document 16.2 already commits
to the `translate` step that reads through a `memcpy`. That requires knowing the exact extents, which
requires the table.

The user-facing side of the same mechanism is `__attribute__((pure))`, `const`, `malloc`,
`returns_nonnull`, `alloc_size`, `nonnull` and `access`. All of them are things a GCC-compatible
compiler must parse, and each of them is an alias or range fact the middle end can use. Parsing them
and ignoring them is acceptable for M4 correctness and wastes most of their value; document 08.4's
table should be populated from user attributes as well as from the built-in list, which costs almost
nothing once the table exists.

## 20.4 Loop idioms

A loop that stores a constant to consecutive elements is `memset`. A loop that copies between two
arrays is `memcpy`, or `memmove` if the ranges may overlap. A loop counting set bits is `popcount`.

**GCC's memset and memcpy recognition** lives in `gcc/tree-loop-distribution.cc`, which is
architecturally interesting: it is not a dedicated idiom pass but a loop *distribution* pass that
splits a loop into pieces and then asks what each piece is. The classification is
`classify_partition` at `gcc/tree-loop-distribution.cc:596` and the kinds are `PKIND_NORMAL`,
`PKIND_PARTIAL_MEMSET`, `PKIND_MEMSET`, `PKIND_MEMCPY`, `PKIND_MEMMOVE`
(`gcc/tree-loop-distribution.cc:215`). Doing it this way means `for (i) { a[i] = 0; b[i] = c[i]; }`
becomes a `memset` and a `memcpy`, which a pattern matcher looking at whole loops would miss.

That is a genuinely good design and it is also why it is expensive: distribution needs the
dependence analysis of document 31. Document 30 owns loop distribution and it is post-M4.

**So what does M4 do?** The whole-loop pattern match, which is the 80% case: a loop whose body is a
single store to `base + i*stride` of a loop-invariant value, whose trip count is known, whose stride
equals the element size, and whose stored value is a byte-repeated constant, becomes `memset`. The
same with a load on the right-hand side becomes `memcpy` when the two ranges provably do not
overlap and `memmove` when they may. Perhaps 200 lines, sitting after document 28's induction
variable analysis, which supplies the base-plus-stride form.

**Three preconditions that are easy to get wrong.**

The trip count must be known and non-negative, and a zero-trip loop must produce a `memset` of zero
bytes rather than a call with a wrapped size. Document 07.5's `Estimate` versus `Bound` distinction
is exactly what stops an estimate being used here.

The overlap question is a dependence question and rucc's answer without document 31 is: if both
pointers derive from the same base with constant offsets, compare them; otherwise emit `memmove`.
`memmove` is correct in all cases and slower, so the conservative answer is available and cheap.

And the loop must have no other side effects, no early exit and no volatile access. The whitelist
rule from document 17.1 applies.

**Bit-counting idioms.** GCC recognises these in an unexpected place: `number_of_iterations_popcount`
at `gcc/tree-ssa-loop-niter.cc:2092` recognises the loop

```
_1 = iv_1 + -1
iv_2 = iv_1 & _1
if (iv != 0)
```

as iterating `popcount(src)` times, documented at `gcc/tree-ssa-loop-niter.cc:2077`. It comes out of
trip count analysis rather than idiom recognition because the pass that needed the answer was the
one computing trip counts. The same file handles the shift-based `clz` and `ctz` loops.

rucc's version recognises the same three loops, in the same place, for the same reason: document
07's trip count analysis needs a non-affine answer for them and `popcount` is that answer. Whether
the loop is then *replaced* by the intrinsic is a separate decision and is worth taking, since every
target rucc supports has the instruction.

## 20.5 String and printf specialisation

`gcc/tree-ssa-strlen.cc` tracks, for each pointer, what is known about the length of the string it
points at, propagating through `strcpy`, `memcpy`, stores of a nul byte, and so on. With that
information `strcat(d, s)` becomes a `memcpy` at a known offset, a `strlen` of a string just copied
is free, and `strcpy` of a known-length string becomes `memcpy`, which is much faster because it
needs no byte-at-a-time scan.

This is a real dataflow analysis over a domain of string lengths, it is 6,173 lines, and it is worth
a lot on the specific class of programs that do heavy string manipulation and nothing at all on
everything else.

**Not in M4.** What is in M4 is the constant-argument folding of 20.2 and the attribute table of
20.3, which together capture the common cases: `strlen` of a literal, `strcpy` from a literal into a
known-size buffer.

`gcc/gimple-ssa-sprintf.cc`'s 4,773 lines compute the exact or bounded output length of a
`printf`-family call, which serves the `-Wformat-overflow` warnings and lets `sprintf(buf, "%d", 42)`
become a store. This is primarily a diagnostics feature and it belongs with the warning
infrastructure rather than the optimizer. Not M4, noted so that `-Wformat-truncation` is not
mistakenly promised.

## 20.6 Recognising an idiom is not always a win

Three cases where the transformation is a regression, all of which have bitten real compilers.

**A `memset` of a small constant size becomes a library call and back.** The loop is recognised as
`memset`, the `memset` is expanded inline as stores, and the result is worse than the original loop
because the expansion did not know the alignment. Rule: recognise the idiom, then immediately ask
whether it expands better than the original, and keep the original if not. That requires the
recognition to be reversible, which it is if it runs before the expansion decision rather than
producing a call the expander then sees fresh.

**A `popcount` intrinsic on a target without the instruction** expands to a longer sequence than the
loop, or to a library call. The lowering in document 36 must know the target's instruction set and
the recognition must consult it. Same rule.

**`memcpy` inlined at a size that turns out to be large at runtime.** A `memcpy` with an unknown size
must stay a call, and a size that ranges over `[0, 1000000]` per document 10 is unknown. Only a
constant, or a range with a small enough maximum, justifies inlining.

The general form of all three: idiom recognition is a canonicalization, and canonicalization is only
free when the reverse direction is available. This is one of the arguments for the e-graph in
document 12, which holds both forms and lets extraction choose, and it is worth noting because it is
one of the few places where equality saturation's actual power, rather than hash-consing's, would
be used. Document 12.3's arm C should be measured on exactly this.

## 20.7 How this is wrong

**A user's function with a standard name is assumed to have standard semantics.** The `-fno-builtin`
handling. gcc 16's rule, measured rather than read, is that a definition in the file changes
nothing: a unit that defines `strlen`, `printf` and `__vprintf_chk` and calls all three gets
`strlen ("abc")` as three and both prints as `puts`, and only `-fno-builtin` says otherwise. The
library call fold follows that, and `execute/vprintf-chk-1.c` is the torture program that checks
it. What it does not do is fold inside such a body, since a `puts` of the program's own that
prints with `printf` of a newline would otherwise become a call to itself.

**An inlined `memcpy` reads or writes out of range.** 20.2's trap.

**`memmove` semantics are given to a generated `memcpy`.** Overlapping ranges. When in doubt,
`memmove`.

**A recognised `memset` has the wrong byte value.** `for (i) a[i] = 0x1234;` on a `short` array is
not a `memset` unless the value's bytes are all equal, and `memset` takes an `int` whose low byte is
used. Both halves of that are easy to get wrong.

**A folded `strlen` disagrees with the string's actual contents** because the array was not
nul-terminated, or because a later store changed it. Folding `strlen` of an array requires knowing
no intervening store touched it, which is a memory SSA question, not a syntactic one.

**A builtin is folded that the target does not have.** Covered in 20.6 and it is a code quality bug
rather than a correctness one, unless the lowering fails outright.

**`__builtin_unreachable` is trusted and the program reaches it.** This is undefined behaviour and
the compiler is entitled to do anything, and what it usually does is delete the surrounding checks
and produce something spectacularly wrong. rucc treats it as a terminator producing no successors,
which makes the following code unreachable and deleted by document 21. That is correct and it is
worth a note in the manual, because the failure mode confuses people.

## 20.8 What it costs

Constant-argument folding is a rewrite rule firing at construction. Free.

The attribute table is a lookup. Free, and it pays for itself many times over in document 08.

Loop idiom recognition is a match against each innermost loop with a known trip count, so linear in
loops. Cheap. Its cost is the analysis it depends on, which documents 07 and 28 pay for anyway.

The measurement in document 42: on the corpus, how many `memset` and `memcpy` calls does `gcc -O2`
generate from loops that rucc leaves as loops, and what does it cost in run time. That number
decides whether loop distribution ever needs building, and it is one of the few places where the gap
against GCC is directly attributable to a single missing pass.
