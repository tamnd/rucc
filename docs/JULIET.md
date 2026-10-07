# Juliet, per row

`spec/safe-memory/14-verification.md` section 14.6 says every row of document 03's matrix that has a CWE column runs the Juliet cases for that CWE at every tier, and reports detected, missed and false positive as raw counts with the missed cases listed by test id. `cargo xtask juliet` does that and writes every id to `target/juliet/report.txt`, and this file keeps the result of the last full run where a person can find it, with what the misses have in common and where each kind is written down.

Section 14.6 is also plain that Juliet is synthetic and uniform in shape, and that a tool can score perfectly on it and be useless. These numbers are a floor and a regression detector. The number that means something is the replay of real corpora in `docs/REPLAY.md` and the CVE corpus of document 12.

## How to read this

The suite is Juliet C/C++ 1.3 from NIST's SARD, C cases only. Files with `w32` or `wchar_t` in the name are left out the way Juliet's own Linux makefiles leave them out, and so are the 228 cases that read their input from a listening socket, because nothing here connects to them. That leaves 8,654 cases.

Every case is built at `-O2` at each of the three tiers, `-fsafety=detect`, `-fsafety=enforce` and `-fsafety=kernel`, together with Juliet's support files built the same way, and linked with a driver that calls the bad half or the good half. Each half runs once with a ten second alarm. A half reported when the monitor's banner is anywhere in its output.

Many cases take the value that goes wrong from outside, and with nothing there the bad half keeps a value its own check turns away and does nothing wrong. So each half is given 10 on its standard input, in the variable `ADD` and in `/tmp/file.txt`, which is one past the ten element buffers the overflow cases index. CWE-124 and CWE-127 go wrong on a negative number, so they are given -1. Juliet's `main` seeds `rand` with the time, and the driver seeds it with 17 instead, or 6 for CWE-124 and CWE-127, which are the first seeds under glibc that make the `rand` sources and the coin flips of variant 12 take the bad side.

**Detected** is a case whose bad half reported. **Missed** is one whose bad half did not. **False+** is one whose good half reported. **Set aside** is a case whose result says nothing about the mistake it is filed under, for one of the reasons below, and every one of them is listed in `xtask/src/juliet.rs` with what its halves do so it can be checked against the source. **Not built** is a case that did not compile or link.

## The table

| CWE | What | Rows | Cases | Detected | Missed | False+ | Set aside | Not built |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 121 | stack based buffer overflow | S2, S8 | 2,470 | 1,707 | 681 | 0 | 82 | 0 |
| 122 | heap based buffer overflow | S1, S8 | 1,632 | 1,406 | 74 | 0 | 152 | 0 |
| 124 | buffer underwrite | S8 | 720 | 570 | 140 | 0 | 10 | 0 |
| 126 | buffer overread | S3, S8 | 562 | 384 | 134 | 0 | 44 | 0 |
| 127 | buffer underread | S8 | 720 | 502 | 208 | 0 | 10 | 0 |
| 401 | memory leak | T9 | 698 | 0 | 698 | 0 | 0 | 0 |
| 415 | double free | T2 | 190 | 190 | 0 | 0 | 0 | 0 |
| 416 | use after free | T1, T5, T6, C4 | 118 | 118 | 0 | 0 | 0 | 0 |
| 457 | use of uninitialized variable | Y6, Y7 | 540 | 300 | 240 | 0 | 0 | 0 |
| 476 | null pointer dereference | S6 | 234 | 211 | 0 | 0 | 23 | 0 |
| 562 | return of stack variable address | T4 | 2 | 0 | 2 | 0 | 0 | 0 |
| 590 | free of memory not on the heap | T3 | 510 | 510 | 0 | 0 | 0 | 0 |
| 761 | free of a pointer not at the start | T3 | 190 | 152 | 0 | 0 | 38 | 0 |
| 843 | type confusion | Y1 to Y5 | 68 | 0 | 26 | 0 | 42 | 0 |
| | all of them | | 8,654 | 6,050 | 2,203 | 0 | 401 | 0 |

The numbers are the detect tier's. CWE-127 is the exception: at enforce and at kernel it detects 499 and misses 211. All of the difference is in the `char_declare_cpy` family, whose misses depend on what the frame holds below the buffer, as the string walk below says, and the frame is not laid out the same at every tier. Not one good half reported at any tier, and every case built and linked at every tier.

Juliet 1.3 has no C cases for CWE-125 (row S1), CWE-787 (rows S1 and S4), CWE-908 (row Y6) or CWE-362 (rows C1, C2 and C3), so those rows have no Juliet number, and that is not the same as passing.

## Set aside

**The variant 32 cases under CWE-121, 124, 126, 127 and 476.** Both halves read the pointer they are about to overwrite through a second pointer to it before anything has written it, which is row Y6, and the monitor refuses that first. Set aside when the good half reported.

**CWE-843, every case.** Both halves point at a local declared in a block and read it after the block has closed, which is row T4. Set aside when the good half reported, which is 21 variants of each of the two families. The other 13 of each are among the misses below.

**CWE-122, the sizeof_double, sizeof_int64_t and sizeof_struct families.** The bad half allocates the size of a pointer for an object of eight bytes, and on LP64 a pointer is eight bytes, so it is given all it uses. Set aside when neither half reported.

**The CWE129_connect_socket families under CWE-121, 122 and 126, and CWE761 char_connect_socket.** The bad half reads from a server on the loopback that nothing here runs, so it keeps the value it started with and does nothing wrong. Set aside when neither half reported.

**CWE-476, the null_check_after_deref family.** The bad half checks what malloc gave it for null only after writing through it, which goes wrong only when malloc fails, and here it does not. Set aside when neither half reported.

## The misses

Every missed case is in one of these, and each one is an open issue rather than an unexplained number.

**A pointer to a local or a global that went through memory or came in as an argument, 1,047 cases.** The capability of a local reaches a wrapper such as `memcpy` through the frame handover, and that only works when the pointer the wrapper is given can be traced back to the local in the same function. In the flow variants 32 (a pointer to a pointer), 34 (a union), 45 (a static global), 51 to 54 (a chain of calls across files) and 63 to 68 (a pointer to a pointer, a struct or an array passed across files) the trace ends, and in some families it ends in variants 41 and 44 as well, where the pointer is passed to a function in the same file. Every stack family under CWE-121, 124, 126 and 127 misses those variants, and the 26 CWE-843 misses are the same variants. The heap families do not, because the heap planes find a heap object from its address alone. The 54 CWE-126 `CWE170` cases are this too: each passes an unterminated local to Juliet's `printLine`, which is in another file, so the `printf` in it is judged with no capability for the local. That is 607 under CWE-121, 140 under CWE-124, 134 under CWE-126, 140 under CWE-127 and 26 under CWE-843. tamnd/rucc#3269.

**Leaks, 698 cases.** Every CWE-401 case. Row T9 is a sweep at exit for blocks nothing points at any more, and it does not exist yet. tamnd/rucc#3254.

**Reads of what nothing wrote, 240 cases.** The CWE-457 families read a scalar that was never written, or the contents of an `alloca` that were never filled, and neither is refused yet. tamnd/rucc#3271.

**A string walk that starts below a local, 68 cases.** The CWE-127 `cpy` and `ncpy` families point 8 bytes below a local buffer and hand that to `strcpy` or `strncpy`. When there is a zero byte in those 8 bytes the walk stops there, before it reaches the buffer, and a walk that never reaches the buffer is never judged against it. Whether there is one depends on what the frame holds below the buffer, which is why `char_declare_cpy` misses 11 of its 19 variants here and a slightly different set at the other tiers. tamnd/rucc#3315.

**Wide strings, 76 cases.** The `CWE135` families under CWE-121 and CWE-122 measure a wide string with `strlen`, which stops at the first byte of its first character, size a buffer from that, and copy the whole string into it with `wcscpy`. The wide string functions are not interposed yet, so the copy is not judged. tamnd/rucc#3268.

**An overrun that stays inside its struct, 72 cases.** The `char_type_overrun_memcpy` and `char_type_overrun_memmove` families under CWE-121 and CWE-122 copy the size of a whole struct into the char array that is its first field, which writes over the pointer after it and stays inside the struct. That is row S4 by way of row S8. The copy is judged against the whole struct, which it fits, and `-fsafety-subobject` does not change that, because it looks with the type plane and a byte copy may write bytes of any type. The bad half then prints through the pointer it overwrote and dies of a segfault the monitor did not report. These families have only variants 01 to 18, and every one of them misses. tamnd/rucc#3327.

**A local's address returned, 2 cases.** The two CWE-562 cases return the address of a local array, and that is not refused yet. tamnd/rucc#3272.

### What the last three fixes did

The run before this one went with tamnd/rucc#3286 and missed 2,464 cases. tamnd/rucc#3306 judges the `printf` family where it is called, and took 114 `snprintf` cases off CWE-121's misses, 76 off CWE-122's and all 38 of CWE-416's, which were `printLine` reading freed memory. tamnd/rucc#3317 stops the extent a split loop divides at where the request stopped rather than at the end of its granule, and took the 30 `c_CWE193_char_loop` cases off CWE-122, which write one byte past a block of 10 at `-O2`. The xtask now also sets aside the two variant 32 socket cases it used to count as missed, and the string walk family missed one fewer this time. That is 2,203 misses now, 261 fewer.

## The runs

| Date | Commit | Level | Cases | Seconds |
| --- | --- | --- | ---: | ---: |
| 2026-10-07 | tamnd/rucc#3318 | -O2 | 8,654 | 3,900 |

The run was on the Linux box, with every case built once per tier and both halves run as many at a time as it has processors, on a machine shared with other work whose load stayed between 12 and 19. Building the three tiers took about 20 minutes and running them about 45. The excuse that sets aside the two variant 32 socket cases was fixed after that run, so the CWE-121 and CWE-126 rows come from a second run of only those two at the same commit, which changed nothing else in them. The full list of missed ids, case by case and tier by tier, is what `cargo xtask juliet` writes to `target/juliet/report.txt`, and `cargo xtask juliet 121 126` takes only the CWEs it is given when only those need another look.
