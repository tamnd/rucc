# Juliet, per row

`spec/safe-memory/14-verification.md` section 14.6 says every row of document 03's matrix that has a CWE column runs the Juliet cases for that CWE at every tier, and reports detected, missed and false positive as raw counts with the missed cases listed by test id. `cargo xtask juliet` does that and writes every id to `target/juliet/report.txt`, and this file keeps the result of the last full run where a person can find it, with what the misses have in common and where each kind is written down.

Section 14.6 is also plain that Juliet is synthetic and uniform in shape, and that a tool can score perfectly on it and be useless. These numbers are a floor and a regression detector. The number that means something is the replay of real corpora in `docs/REPLAY.md` and the CVE corpus of document 12.

## How to read this

The suite is Juliet C/C++ 1.3 from NIST's SARD, C cases only. Files with `w32` or `wchar_t` in the name are left out the way Juliet's own Linux makefiles leave them out, and so are the 228 cases that read their input from a listening socket, because nothing here connects to them. That leaves 8,654 cases.

Every case is built at `-O2` at each of the three tiers, `-fsafety=detect`, `-fsafety=enforce` and `-fsafety=kernel`, together with Juliet's support files built the same way, and linked with a driver that calls the bad half or the good half. Each half runs once with a ten second alarm. A half reported when the monitor's banner is anywhere in its output. The CWE-401 cases are built with `-fsafety-leaks` as well, and for them the banner looked for is the leak report's, `rucc: memory leak`.

Many cases take the value that goes wrong from outside, and with nothing there the bad half keeps a value its own check turns away and does nothing wrong. So each half is given 10 on its standard input, in the variable `ADD` and in `/tmp/file.txt`, which is one past the ten element buffers the overflow cases index. CWE-124 and CWE-127 go wrong on a negative number, so they are given -1. Juliet's `main` seeds `rand` with the time, and the driver seeds it with 17 instead, or 6 for CWE-124 and CWE-127, which are the first seeds under glibc that make the `rand` sources and the coin flips of variant 12 take the bad side.

**Detected** is a case whose bad half reported. **Missed** is one whose bad half did not. **False+** is one whose good half reported. **Set aside** is a case whose result says nothing about the mistake it is filed under, for one of the reasons below, and every one of them is listed in `xtask/src/juliet.rs` with what its halves do so it can be checked against the source. **Not built** is a case that did not compile or link.

## The table

| CWE | What | Rows | Cases | Detected | Missed | False+ | Set aside | Not built |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 121 | stack based buffer overflow | S2, S8 | 2,470 | 2,326 | 74 | 0 | 70 | 0 |
| 122 | heap based buffer overflow | S1, S8 | 1,632 | 1,406 | 74 | 0 | 152 | 0 |
| 124 | buffer underwrite | S8 | 720 | 715 | 0 | 0 | 5 | 0 |
| 126 | buffer overread | S3, S8 | 562 | 466 | 52 | 0 | 44 | 0 |
| 127 | buffer underread | S8 | 720 | 714 | 0 | 0 | 6 | 0 |
| 401 | memory leak | T9 | 698 | 576 | 32 | 0 | 90 | 0 |
| 415 | double free | T2 | 190 | 190 | 0 | 0 | 0 | 0 |
| 416 | use after free | T1, T5, T6, C4 | 118 | 118 | 0 | 0 | 0 | 0 |
| 457 | use of uninitialized variable | Y6, Y7 | 540 | 498 | 42 | 0 | 0 | 0 |
| 476 | null pointer dereference | S6 | 234 | 211 | 0 | 0 | 23 | 0 |
| 562 | return of stack variable address | T4 | 2 | 2 | 0 | 0 | 0 | 0 |
| 590 | free of memory not on the heap | T3 | 510 | 510 | 0 | 0 | 0 | 0 |
| 761 | free of a pointer not at the start | T3 | 190 | 152 | 0 | 0 | 38 | 0 |
| 843 | type confusion | Y1 to Y5 | 68 | 0 | 0 | 0 | 68 | 0 |
| | all of them | | 8,654 | 7,884 | 274 | 0 | 496 | 0 |

The numbers are the detect tier's. CWE-126 is the exception: at kernel it detects 467 and misses 51, because the two `CWE170` variant 12 cases are caught at some tiers and not others, because whether one is caught depends on a byte of the frame nothing wrote, as the unterminated copies below say, and the frame is not laid out the same at every tier. CWE-401 is not run at enforce, because document 03 gives row T9 nothing to enforce. Not one good half reported at any tier, and every case built and linked at every tier.

Juliet 1.3 has no C cases for CWE-125 (row S1), CWE-787 (rows S1 and S4), CWE-908 (row Y6) or CWE-362 (rows C1, C2 and C3), so those rows have no Juliet number, and that is not the same as passing.

## Set aside

**The variant 32 cases under CWE-121, 124, 126, 127 and 476.** Both halves read the pointer they are about to overwrite through a second pointer to it before anything has written it, which is row Y6, and the monitor refuses that first. Set aside when the good half reported.

**CWE-843, every case.** Both halves point at a local declared in a block and read it after the block has closed, which is row T4. Set aside when the good half reported, which is every variant of the two families. Until tamnd/rucc#3663 the 8 variants of each that store the pointer in memory and read it back were missed, because the load threw out the capability of a local whose block had closed, so neither half was refused.

**CWE-122, the sizeof_double, sizeof_int64_t and sizeof_struct families.** The bad half allocates the size of a pointer for an object of eight bytes, and on LP64 a pointer is eight bytes, so it is given all it uses. Set aside when neither half reported.

**The CWE129_connect_socket families under CWE-121, 122 and 126, and CWE761 char_connect_socket.** The bad half reads from a server on the loopback that nothing here runs, so it keeps the value it started with and does nothing wrong. Set aside when neither half reported.

**CWE-401, the malloc_realloc families.** The bad half loses its first block only when realloc fails, and here it does not, so realloc takes the block over and the bad half frees what it gives back. Set aside when neither half reported.

**CWE-476, the null_check_after_deref family.** The bad half checks what malloc gave it for null only after writing through it, which goes wrong only when malloc fails, and here it does not. Set aside when neither half reported.

## The misses

Every missed case is in one of these, and each one is an open issue rather than an unexplained number.

**An unterminated copy that ends on a zero, 52 cases.** The `CWE170` families under CWE-126 copy 99 bytes into a buffer of 100 and print it without writing the last byte. The buffer's capability reaches the `printf` in Juliet's `printLine`, so when that byte is not zero the walk goes past the buffer and is refused. Here it is a zero left in the frame by an earlier call, so the walk stops inside the buffer, and what goes wrong is a read of a byte nothing wrote, which is row Y6 and is not asked inside a wrapper. Whether it is a zero depends on what ran before, which is why the two variant 12 cases are caught at some tiers and on some commits and not others. tamnd/rucc#3271.

**Reads of what nothing wrote, 42 cases.** Flow variants 63 and 64 of the CWE-457 families pass the address of the local to a sink in another file, which reads it there. Handing the address to a function the caller cannot see counts as letting the local go, so the stack plane is never told the local starts out unwritten and the read in the sink is not refused. Every other variant of every family is detected. tamnd/rucc#3271.

**Wide strings, 76 cases.** The `CWE135` families under CWE-121 and CWE-122 measure a wide string with `strlen`, which stops at the first byte of its first character, size a buffer from that, and copy the whole string into it with `wcscpy`. The wide string functions are not interposed yet, so the copy is not judged. tamnd/rucc#3268.

**An overrun that stays inside its struct, 72 cases.** The `char_type_overrun_memcpy` and `char_type_overrun_memmove` families under CWE-121 and CWE-122 copy the size of a whole struct into the char array that is its first field, which writes over the pointer after it and stays inside the struct. That is row S4 by way of row S8. The copy is judged against the whole struct, which it fits, and `-fsafety-subobject` does not change that, because it looks with the type plane and a byte copy may write bytes of any type. The bad half then prints through the pointer it overwrote and dies of a segfault the monitor did not report. These families have only variants 01 to 18, and every one of them misses. tamnd/rucc#3327.

**Leaks kept in a global, 32 cases.** Flow variants 45 and 68 of every CWE-401 family keep the pointer to the lost block in a global, so the block is still reachable at exit, and LeakSanitizer and Valgrind would not call it lost either. Row T9 is a sweep for blocks nothing points at, so these stay counted as missed rather than set aside.

### What the fixes since the first run did

The run before this one went with tamnd/rucc#3329 and missed 2,203 cases. tamnd/rucc#3414 hands a local's bounds to a function in another file, and took variants 41, 44, 51 to 54 and 65 off the misses, 275 under CWE-121, 41 under CWE-126 and 10 under CWE-843. tamnd/rucc#3421 refuses a pointer made below or past a local or a global where it is made, and took every miss off CWE-124 and CWE-127, 348 cases, the string walk that started below a local among them. tamnd/rucc#3426 adds the leak sweep of row T9, which detects 576 of the CWE-401 cases where it detected none, with 90 set aside. tamnd/rucc#3432 refuses a read of a returned frame inside a wrapper and after an inlined return, which detects both CWE-562 cases. tamnd/rucc#3446 refuses a read of bytes `__builtin_alloca` or a variable length array took and nothing wrote, and tamnd/rucc#3460 a read of a local held in a register that nothing wrote on the path taken, which together took 198 cases off CWE-457. The `CWE170` variant 12 cases moved at detect, for the reason given above. tamnd/rucc#3610 keeps the capability of a pointer stored in a word of the stack or of a global, and took 165 cases off CWE-121 and 12 off CWE-126, variant 32 of the three `CWE131` families and the loop families in the other variants but 67 among them. tamnd/rucc#3617 hands a wrapper the capability of a pointer read back out of memory, and took 120 cases off CWE-121 and 24 off CWE-126, the copy, concatenation and print families in those variants. tamnd/rucc#3625 passes a structure of one pointer in a register as a pointer rather than an integer, the way clang does, and looks through a store and the load straight behind it when it hands a pointer over, which took the 47 variant 67 cases off CWE-121 and the 6 off CWE-126. One of the `CWE170` variant 12 cases moved back the other way at detect. tamnd/rucc#3663 keeps the capability of a local whose block has closed when a pointer to it is read back out of memory while its function still runs, so both halves of the 16 CWE-843 cases that went through memory are refused, and those are set aside with the rest of the family. The other variant 12 case moved at detect. That is 274 misses now, 1,929 fewer.

## The runs

| Date | Commit | Level | Cases | Seconds |
| --- | --- | --- | ---: | ---: |
| 2026-10-07 | tamnd/rucc#3329 | -O2 | 8,654 | 3,900 |
| 2026-10-09 | tamnd/rucc#3432 | -O2 | 8,654 | 2,875 |
| 2026-10-09 | tamnd/rucc#3460 | -O2 | 8,654 | not timed |
| 2026-10-13 | tamnd/rucc#3610 | -O2 | 8,654 | not timed |
| 2026-10-13 | tamnd/rucc#3617 | -O2 | 8,654 | not timed |
| 2026-10-14 | tamnd/rucc#3625 | -O2 | 8,654 | not timed |
| 2026-10-15 | tamnd/rucc#3663 | -O2 | 8,654 | not timed |

Both runs were on the Linux box, with every case built once per tier and both halves run as many at a time as it has processors, on a machine shared with other work. In the first, whose load stayed between 12 and 19, building the three tiers took about 20 minutes and running them about 45. The excuse that sets aside the two variant 32 socket cases was fixed after that run, so its CWE-121 and CWE-126 rows came from a second run of only those two at the same commit. The second run is the one in the table above, made on the branch of tamnd/rucc#3432 just before it was merged. The full list of missed ids, case by case and tier by tier, is what `cargo xtask juliet` writes to `target/juliet/report.txt`, and `cargo xtask juliet 121 126` takes only the CWEs it is given when only those need another look.
