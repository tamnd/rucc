# The OSS-Fuzz replay

`spec/safe-memory/12-corpus-and-evidence.md` section 12.7 says the cheapest way to find what ASan missed is to replay the corpora OSS-Fuzz has already accumulated against a Tier D build, and section 12.5 says what comes out goes on a scoreboard rather than in somebody's terminal. This file is that scoreboard for the replay. `cargo xtask replay` produces the numbers and holds every report to the triage list in `xtask/src/replay.rs`, and this file keeps the result of the last full run of each corpus where a person can find it.

## How to read this

Every project is built from its own unmodified release at `-O0` with `-fsafety=detect`, which is Tier D, and linked with the harness under `tests/replay` that stands in for the project's OSS-Fuzz target. Every input runs in a process of its own with a sixty second alarm, so an input that dies or hangs is that input's result and not the end of the run.

An input is counted as reported when it made at least one report. A shape is the judgement and the width of the access, which is what the reader can tell apart. Every shape is in one of the five buckets of section 12.6, and a shape on no list fails the run, so a row here with a shape and no bucket cannot happen.

A clean row means the paths that corpus reached had nothing the monitor could see. It does not mean the project is memory safe, for the reasons section 12.9 gives. A row where many inputs ran out of time is a weaker clean result than one where every input finished, and it says so.

## The table

| Project | Stands in for | Corpus pin | Inputs | Died | Out of time | Reported | Shapes | Buckets |
| --- | --- | --- | ---: | ---: | ---: | ---: | ---: | --- |
| brotli | `brotli_decode_fuzzer` | 2026-09-19 | 4,421 | 0 | 0 | 0 | 0 | |
| zlib | `zlib_uncompress_fuzzer` | 2026-09-21 | 1,551 | 0 | 0 | 0 | 0 | |
| SQLite | `ossfuzz` | 2026-10-05 | 23,270 | 0 | 0 | 341 | 1 | 1 in bucket 1 |
| Lua | `fuzz_lua` | 2026-09-21 | 18,693 | 0 | 513 | 0 | 0 | |
| zstd | `simple_decompress` | 2026-10-05 | 17,088 | 0 | 0 | 704 | 1 | 1 in bucket 1, 2 in bucket 5 built away |
| libjpeg-turbo | `libjpeg_turbo_fuzzer` | 2026-10-05 | 7,899 | 0 | 107 | 149 | 3 | 3 in bucket 1 |

## The shapes

**SQLite, J1 over 8 bytes, bucket 1.** OP_Insert copies `z` and `n` out of a register nothing wrote. During VACUUM the transfer optimisation runs OP_SeekEnd, OP_RowCell, then OP_Insert with `OPFLAG_PREFORMAT`, and OP_RowCell fills the btree's preformat buffer rather than the register. The value is never used, which is why ASan, MSan and valgrind are quiet about it. Four statements reproduce it: a table, an index on it, one row, and `VACUUM`. Upstream: not reported yet. tamnd/rucc#1542.

**zstd, J2, bucket 1.** Pointers the decoder forms outside the buffer they were derived from and then compares or drops without reading through, at about ten sites. The first 1,500 inputs were read site by site and 66 of them reported. Most of them are `match = oLitEnd - sequence.offset` in `ZSTD_execSequence`, formed before the offset is checked. The others are `BIT_initDStream` on a stream shorter than eight bytes, `ZSTD_overlapCopy8`, the Huffman jump tables, the literal buffer placement and one `p+32 <= bEnd` in xxhash. C leaves that arithmetic undefined and every input still decodes correctly or is rejected correctly, which is the evidence question 12 of `spec/safe-memory/17-open-questions.md` asks for. Upstream: not reported yet. tamnd/rucc#1499.

**zstd, J1 over 2 bytes and J1 over 8 bytes, bucket 5.** zstd's access method 1 writes the Huffman table as `u64` and reads it as `U16`, and writes literals as `u16` that are later hashed as `u64`. Both are effective type violations zstd chooses on purpose, and its own headers call method 1 a compiler extension. The libraries row builds method 0 with `MEM_FORCE_MEMORY_ACCESS=0` and `XXH_FORCE_MEMORY_ACCESS=0`, which is the declared exemption, and under it neither shape appears. tamnd/rucc#2870.

**libjpeg-turbo, J1 over 1 byte and J1 over 2 bytes, bucket 1.** The upsamplers in `jdsample.c` read rows of downsampled samples the IDCT never wrote, at six sites in `int_upsample`, `h2v1_upsample`, `h2v2_upsample` and the three fancy upsamplers, as one byte samples on 40 inputs and as two byte samples on 91. Decoding all 131 inputs with the heap as malloc leaves it and then filled with two different patterns gives byte for byte the same output every time, so the values reach nothing. This is SQLite's kind of read and question 13 of `spec/safe-memory/17-open-questions.md` is about it. Upstream: not reported yet. tamnd/rucc#1499.

**libjpeg-turbo, J2, bucket 1.** `tj3Decompress16` forms its row pointers at `turbojpeg-mp.c` lines 244 and 246 past the end of the output buffer on 18 inputs. Those are lossless images with a precision below 8, and upstream's harness only tests for 8 and 12, so it sizes the buffer for one byte a sample and then calls the 16 bit function. `jpeg_read_scanlines` rejects the precision before any row is written, so nothing is written through the pointers. The other half of it is that the fuzzer never decodes a lossless image of precision 2 to 7 or 9 to 11. Upstream: not reported yet. tamnd/rucc#1499.

## The runs

| Project | Date | Commit | Drivers |
| --- | --- | --- | ---: |
| brotli | 2026-09-21 | tamnd/rucc#1532 | 1 |
| zlib | 2026-09-21 | tamnd/rucc#1532 | 1 |
| SQLite | 2026-10-05 | c26112c | 3 |
| Lua | 2026-09-21 | tamnd/rucc#1569 | 1 |
| zstd | 2026-10-05 | c26112c | 3 |
| libjpeg-turbo | 2026-10-05 | 4690906 | 1 |

The corpus pin in the table is the day the corpus was downloaded, and it is the pin `xtask/src/replay.rs` records. SQLite and zstd were downloaded again on 2026-10-05 for the runs above, which moved SQLite from 22,578 inputs to 23,270 and zstd from 17,310 to 17,088, and their pins moved with them. A run with more than one driver splits the corpus with `RUCC_REPLAY_JOBS` into runs in the order one driver would take them, so the inputs and the reports are the same and only the wall clock is not. SQLite took 6,712 seconds over three drivers and zstd took 674. libjpeg-turbo took most of a working day on one driver on a machine whose load stayed above ten times its core count, which is most of why 107 of its inputs ran out of time, and that row is a weaker clean result than the others for it.
