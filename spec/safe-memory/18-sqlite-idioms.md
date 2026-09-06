# The SQLite idiom audit

Document 03 section 3.5 lists fifteen legitimate C idioms that a naive reading of the model in document 04 rejects, and gives each one a resolution. That table was written from experience and from what the literature reports, not from reading a specific program. This document is the first time it has been checked against real source, and the source is SQLite, because document 16 puts SQLite at Tier D as the first corpus member and document 12 explains why: it is small enough to read in full, it has a test suite with genuinely high coverage, it is deliberately written to be portable to hostile compilers, and it is already the thing Fil-C reports on.

The result is that section 3.5 holds up better than expected on the rows it has, and misses three things. Nine of the fifteen rows are exercised, five are not exercised at all by this program, one is not applicable to C. Every exercised row's stated resolution is the right one, and none of them needed changing. What section 3.5 does not have is a row for an allocator that lives inside the instrumented program rather than behind the interposed boundary, a row for a bulk write that starts at one member and runs across several, and a row for a program that converts pointers to integers constantly without ever converting one back. Those three are now questions 8, 9 and 10 in document 17.

This document exists because the second exit criterion for milestone S3 asks for exactly it, and because a table of mitigations that has never met a program is a table of guesses.

## 18.1 How this was gathered

SQLite 3.45.1, the public amalgamation, 255680 lines of `sqlite3.c` and its two headers. Every line number below is a line number in that file. The build is the default one, which matters more than it sounds: a large fraction of what looks alarming in SQLite is behind a `#define` that is off unless you ask for it, and an audit that counts those is an audit of a program nobody compiles. Where an idiom is conditional, the condition is stated and whether it is on by default is stated with it.

The method was to take each of the fifteen rows, decide what its idiom looks like as text, search for that shape, and then read the surrounding code to decide whether the hit is really the idiom or only looks like it. That last step removed more hits than it kept. A `memcpy` between two objects of the same type is not type punning, a cast to `char*` for pointer arithmetic is not an aliasing question, and a union with a discriminant is not a union pun. The verdicts below are about what the program actually does, not about what a grep says.

What this cannot do is tell you what happens at run time. The audit is static, so it establishes that an idiom is present in the source and reachable in a default build, and it does not establish how often it executes or on what data. That has to wait for the monitor to exist, which is what makes this an audit rather than a measurement.

## 18.2 The row by row verdict

| Row from 3.5 | Exercised | Verdict |
|---|---|---|
| `container_of` | no | no macro of that shape, and no place that derives an enclosing object from a member pointer |
| union punning | no | `MemValue` is a discriminated union and every read matches the last store, so Y3 never fires |
| `char*` and `memcpy` punning | yes, heavily | 478 `memcpy` sites; the character type exemption covers all of them |
| pointer to integer round trip | no round trip | pointers become integers constantly and never become pointers again, see 18.5 |
| low bit tagged pointers | no | no tagging anywhere |
| one past the end pointers | yes | the model already permits it |
| null pointer `offsetof` | yes, and inert | 14735 defines its own only `#ifndef offsetof`, so with a GNU compatible compiler the builtin wins |
| allocators that carve one allocation into many | yes, on by default | lookaside, see 18.3 |
| `sockaddr` confusion | no | SQLite opens no sockets |
| `T x[1]` and flexible array members | yes, eight of them | bounds come from the allocation, which is the stated resolution |
| reading padding | not found | the only whole buffer `memcmp` is over a `char` array |
| `setjmp` and `longjmp` | no | zero uses |
| VLAs and `alloca` | only behind a flag | `SQLITE_USE_ALLOCA`, off by default, otherwise the call falls back to `sqlite3DbMallocRaw` |
| placement new | not applicable | this is a C program |

Nine yes, five no, one not applicable. No row's resolution had to change as a result of reading the program, which is the most useful thing this audit says.

## 18.3 The rows SQLite exercises hardest

**Allocator carving, and it is on by default.** SQLite's lookaside allocator takes one `malloc` of 120 kilobytes per connection and cuts it into fixed size slots. `setupLookaside` at 178888 does the cutting, and the loop at 178942 walks the arena with `p = (LookasideSlot*)&((u8*)p)[sz];`, threading every slot onto a free list through a `pNext` field stored in the slot itself. Allocation pops the list, and `sqlite3DbFreeNN` at 30284 decides whether a pointer belongs to the arena by comparing it against `pStart` and `pEnd`, and if it does, pushes it back on the list by writing `pBuf->pNext` into the slot it just freed. There is a second size class, and `pMiddle` splits the arena between them.

This is the row's resolution working exactly as document 10 says it does, and it is also the sharpest possible statement of why that API has to exist. Without `__rucc_alloc_split`, the monitor sees one storage instance of 120 kilobytes for the whole arena. Every overflow from one lookaside slot into the next is inside that instance and therefore invisible, and every use of a freed slot is a use of live storage and therefore invisible too. SQLite allocates most of its small objects here, so the loss is not marginal. That is not a false positive, it is a false negative, and it is the thing that makes an unannotated sub-allocator worse than no allocator at all. Document 17 question 8 is about who is expected to write those annotations.

There is a second, smaller instance of the same shape in the page cache: `sqlite3PCacheBufferSetup` at 55021 carves a caller-supplied buffer with `pBuf = (void*)&((char*)pBuf)[sz];`, and it is reachable only through `SQLITE_CONFIG_PAGECACHE`, which is off unless the embedder sets it.

**The `T x[1]` idiom, eight times, three of them with a comment saying so.** `Table.aCol[1]` at 18472, `ExprList.a[1]` at 19105, `SrcList.a[1]` at 19232, `With.a[1]` at 20279, `Module.zName[1]` at 20310 marked "MUST BE LAST", `UnpackedRecord.aType[1]` at 23124 marked "MUST BE LAST", `Fts3SegReader.aSegment[1]` at 65095, and `SortSubtask.aTask[1]` at 102545. Every one of them is allocated with a computed size larger than `sizeof` the struct and then indexed past one. The stated resolution is that bounds come from the allocation and only S4 sees the declared extent, and S4 treats a trailing array as unbounded within the allocation. All eight fit that.

**One past the end, deliberately and repeatedly.** `zTerm = &z[nByte];` at 34454 and `u32 *aEnd = (u32 *)&a[nByte];` at 65356 both form the end pointer of a buffer and compare against it without dereferencing, which the model permits for the same reason C does. `memset(&aNew[nCurrent], ...)` at 63609 forms a pointer at the old end of a grown array and writes forward from it, which is in bounds of the new allocation.

**Punning through `memcpy` and `char*`.** 478 `memcpy` calls. The one worth naming is `memcpy(pTo, pFrom, MEMCELLSIZE)` at 83374 and three other sites, where `MEMCELLSIZE` is `offsetof(Mem,db)` from 23239, so it copies a prefix of a `Mem` including the whole `MemValue` union as bytes regardless of which member is active. That is the character type rule and the `memcpy` rule from 6.5, both of which are in the model rather than exceptions to it, so Y2 and Y3 do not fire. It is also the second instance of the shape that becomes question 9, since the copy is sized by an `offsetof` and stops in the middle of a structure.

**Its own `offsetof`, which never gets used.** 14735 defines `#define offsetof(STRUCTURE,FIELD) ((int)((char*)&((STRUCTURE*)0)->FIELD))`, which is exactly the hand rolled form the row names, and it is inside `#ifndef offsetof`. `rucc` claims GNU compatibility and defines the builtin, so the header's version is never compiled. The row's resolution says the hand rolled form is recognized and folded, and it should stay in the specification because a program that does not define `__GNUC__` will reach it, but for SQLite specifically the row is satisfied by not arising.

## 18.4 The rows SQLite does not exercise

Worth writing down, because a row that no corpus member exercises is a row whose resolution is untested no matter how many corpus members pass.

There is no `container_of` and nothing shaped like it, which means the single most delicate resolution in section 3.5, the rewrite rule that recognizes a subtraction of a constant `offsetof` as a widening and is SMT verified like any other rule, gets no coverage from SQLite at all. It will get coverage from the kernel, where the idiom is everywhere, and that is a long way off. Until then the rule is verified but not exercised.

There are no tagged pointers, no `setjmp`, no sockets and no genuine union puns. SQLite is unusually disciplined about all four, which is a fact about SQLite rather than about C, and it means the corpus needs a member that is not disciplined before those rows mean anything. Document 12's corpus list has candidates for each.

## 18.5 What SQLite does that section 3.5 has no row for

**An allocation that hands back an interior pointer.** In a default build `SQLITE_MALLOCSIZE` is undefined, so `sqlite3MemMalloc` at 26461 does `p = SQLITE_MALLOC(nByte+8); p[0] = nByte; p++;` and returns the incremented pointer, and `sqlite3MemFree` at 26494 does `p--` before calling `free`. Everything stays within one `malloc` block, so nothing here is a violation and nothing here is a false positive. What it costs is precision: the instance the monitor records is the block, the object the program thinks it has starts eight bytes into it, and an underflow of eight bytes or fewer from any SQLite allocation lands in the size header instead of outside the instance. That is a detection gap of exactly eight bytes at the front of every allocation in the program, and it is the same fix as the lookaside case. The variant where `SQLITE_MALLOCSIZE` is `malloc_usable_size`, at 26441, needs both `SQLITE_USE_MALLOC_H` and `SQLITE_USE_MALLOC_USABLE_SIZE` and is off by default; when it is on, SQLite's idea of an allocation's size is the allocator's rounded size rather than the requested one, and the monitor has to agree with the allocator or report on every rounded up byte.

**A bulk write that starts at one member and runs across several.** `PARSE_HDR(X)` at 19815 is `(((char*)(X))+offsetof(Parse,zErrMsg))` and `PARSE_TAIL(X)` at 19819 is `(((char*)(X))+PARSE_RECURSE_SZ)`, and the two are used at 120887 to `memcpy` a run of the `Parse` structure out, `memset` it, and `memcpy` it back, and at 141313 to `memset` two runs of it. `MEMCELLSIZE` above is the same shape at a smaller scale. Section 3.5's `container_of` row is about deriving the enclosing object from a member; this is about writing a range that begins at one member and covers many, which is a different question with the same flavour, and the difference matters because S4 sub-object bounds would reject the run at the first member boundary it crosses while the `container_of` rule says nothing about it. The character type and `memcpy` rules mean Y2 and Y3 are quiet, so this is purely an S4 question, and S4 is off by default. It still needs a written answer before S4 can ever be turned on, which is question 9.

**Pointers turned into integers constantly, and never turned back.** `SQLITE_WITHIN(P,S,E)` at 14889 is `(((uptr)(P)>=(uptr)(S))&&((uptr)(P)<(uptr)(E)))`, and the free path at 30333 and 30372 open codes the same comparison, so every single `sqlite3DbFree` compares a pointer of unknown origin against the bounds of an arena it may or may not belong to. `SQLITE_PTR_TO_INT` and `SQLITE_INT_TO_PTR` at 13956 are used at 18018 to stash a small integer in a `void*` field and read it back as an integer, which is the mirror of the round trip row and not the same thing, because no pointer is ever recovered. Under PNVI-ae-udi none of this is a violation: comparison is not an access, and an integer that becomes a pointer again is the only case the exposed address rule has to adjudicate. What it does do is *expose* every one of those storage instances, and an exposed instance is one the optimizer may assume much less about, which is the input to document 07's discharge rate. SQLite exposes every allocation it frees. Question 10 asks what that costs.

## 18.6 What this audit does not establish

It does not establish that SQLite runs clean under Tier D, because Tier D does not run yet, and it does not establish that these are the only idioms in SQLite that matter, because the method was to look for known shapes rather than to read 255680 lines. It establishes that the fifteen rows written before anyone read a program survive the first program they were read against, that the three things they miss are findable by reading, and that the two rows that will hurt most on this corpus member are the two about allocators, both of which point at the same piece of unbuilt machinery in document 10.

The next corpus member should be audited the same way and the differences recorded, because the value of this document is not in the fifteen verdicts, it is in whether the sixteenth row keeps being needed.
