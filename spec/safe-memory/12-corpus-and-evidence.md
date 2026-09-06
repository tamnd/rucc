# The corpus, the scoreboard, and the triage process

Document 02 says the honest form of the reach claim is "these named projects, at this tier, with this many declared exemptions, finding this many upstream-confirmed bugs." This document names the projects, defines the scoreboard, and specifies what happens when a report arrives, which is the process that decides whether axis 2 ever converges.

The governing observation: **the corpus is the specification's contact with reality, and everything else in these nineteen documents is a hypothesis until it runs.**

## 12.1 What the corpus is for

Three distinct jobs, often conflated, which need different code:

1. **Precision.** Does the tool fire on correct code? Needs *large, diverse, exercised* code, and it does not matter whether it has bugs.
2. **Soundness.** Does the tool find real bugs? Needs code with *known* bugs, which means historical CVEs with reproducers.
3. **Cost.** What does it cost? Needs benchmarks with stable, meaningful workloads.

A single list serving all three serves none well. The corpus is therefore three lists with deliberate overlap.

## 12.2 Tier 1: the precision corpus

Chosen for: heavy pointer use, mature test suites, wide deployment, and (critically) coverage of the idioms in document 03 section 3.5. A library that does not do anything interesting with pointers teaches us nothing.

| Project | Why it is here | The idiom it stresses |
|---|---|---|
| **zlib**, **zstd**, **brotli** | small, ubiquitous, buffer-heavy | raw buffer arithmetic, one-past-end |
| **SQLite** | the best test suite in C; ~600x test-to-code ratio | arena allocation, `MAP_FIXED`, tagged pointers |
| **OpenSSL**, **BoringSSL** | security-critical, huge, heavy macro use | custom allocators, `container_of`-alikes, constant-time code |
| **libpng**, **libjpeg-turbo**, **libwebp** | classic memory-CVE territory | hand-rolled bounds arithmetic, assembly kernels |
| **curl** | protocol parsing, wide platform surface | `struct sockaddr` punning, syscall boundary |
| **Lua**, **CPython** | GC'd runtimes written in C | pointer-to-integer round trips, tagged values, `setjmp` |
| **musl**, **glibc** subset | libc itself; the interposition surface | everything, including the parts we interpose |
| **jemalloc**, **mimalloc** | allocators | document 10.4's interposition API, in anger |
| **git** | large, pointer-dense, well-tested | `container_of`, custom allocators, mmap |
| **ffmpeg** | enormous, assembly-heavy, format parsing | document 10.6's asm boundary at scale |
| **nginx** | pool allocator, event loop | `__rucc_alloc_purge`, long-lived process |
| **Linux kernel** subsystems | the goal | document 11, all of it |

**The entry criterion for a project is that its own test suite passes at Tier D with zero reports.** Not "few reports." Zero, or a written entry in document 17 naming the idiom. A project that cannot reach zero is the most valuable thing in the corpus, because it has found a hole in document 04's model.

**musl and glibc deserve a note.** Instrumenting libc is both the hardest case and the most valuable one, hardest because libc is where every dark corner of C lives, most valuable because a fully instrumented libc removes the largest boundary in document 10. musl first, because it is 30k lines and readable; glibc's instrumented subset later and probably never in full.

## 12.3 Tier 2: the CVE reproduction corpus

Document 02 targets 200 cases at S6. Each case is a directory:

```
cve/CVE-2016-XXXX/
  META.toml          project, version, CWE, document-03 class, upstream fix commit
  build.sh           builds the vulnerable version
  input/             the triggering input
  expect.toml        which check should fire, at which source location
```

`expect.toml` is what makes this a regression suite rather than a demo. It names the exact check (`S1` at `png_handle_iCCP`, `deflate.c:1043`) so a change that still detects the bug but reports it at the wrong place, or as the wrong class, is a test failure. A detection that points at the wrong line is worth much less to a developer and the suite should defend the difference.

Sources for cases: the projects' own security advisories, OSS-Fuzz's public reproducers (which come with inputs, which is most of the work), and the Juliet suite for synthetic coverage of classes the real cases miss.

**Both directions are tested.** The pre-fix build must report; the post-fix build must not. The second half catches the failure mode where a check fires on everything.

## 12.4 Tier 3: the cost corpus

The parent's document 16 benchmark set, plus the pointer-heavy additions this specification needs, because the parent's set was chosen to measure compiler quality and this one has to measure metadata traffic.

Additions: a linked-list traversal microbenchmark and a binary-tree microbenchmark, both of which are the worst case for document 05's two-lines-per-node problem; a pointer-chasing hash table; and a `memcpy`-dominated loop, which is the case where our checks should be nearly free and where any measured overhead indicates something is wrong.

Document 13 owns the methodology. The corpus's job here is only to be stable and to include the pathological cases rather than avoiding them.

## 12.5 The scoreboard

One machine-readable file per nightly run, published, never manually edited. It is the project's honesty mechanism: a specification can promise anything, and a scoreboard that has been running for six months cannot.

```toml
[run]
date = "2026-11-04"; commit = "..."; tier = "D"

[[project]]
name = "sqlite"; version = "3.47.0"
builds = true
tests_passed = 271843; tests_failed = 0
reports_total = 0
reports_by_class = {}
[project.trust]                        # document 10.2, every row
declared_regions = 2                   # with reasons
asm_sites = 0
recovered_capabilities = 41
exposed_instances = 3
transfers = 118
uninstrumented_objects = ["libm.so"]
[project.checks]                       # document 07.8
emitted = 1_204_112
discharged = 1_061_338                 # 88.1%
remaining = 142_774
[project.cost]
time_vs_baseline = 3.41; memory_vs_baseline = 1.72
```

Four properties are deliberate:

**Trust-set counts are first-class, not a footnote.** A run where `discharged` goes up and `recovered_capabilities` goes up with it has not gotten better; it has gotten quieter. Both numbers move together in the same file so the trade is visible.

**Cost is per project.** Never a single geomean, per document 02 axis 3.

**`reports_total = 0` is the passing state for tier-1 projects** and any nonzero value is a release-blocking bug in `rucc` until triaged otherwise.

**Nothing is manually suppressed.** There is no suppression file. A false positive is either fixed in the model, or it becomes a document 17 entry with a written reason and an explicit exemption region in the *build*, which is counted in `declared_regions`. The distinction matters: a suppression file makes a problem invisible, a counted exemption makes it a number that a reviewer sees going up.

## 12.6 Triage: what happens when a report arrives

The process, because axis 2 lives or dies here. Every report from a tier-1 project goes through this and lands in exactly one bucket.

**1. Is it a real bug in the project?** Then: reduce it, report it upstream, add it to the CVE corpus, and, this is the part projects skip, *record whether upstream agreed*. An upstream-confirmed bug is the strongest evidence this specification can produce and document 02's axis 4 counts them. An upstream rejection ("that's intentional, the standard permits it") is even more informative, because it usually means the model is wrong.

**2. Is it a false positive because the model is wrong?** The model in document 04 does not match what C actually permits, or what the platform actually guarantees. **This is a release-blocking bug in `rucc`,** it gets fixed in document 04 or 09, and the idiom is added to document 03 section 3.5's table. This bucket is the reason the specification is written in files that can be edited rather than in code comments.

**3. Is it a false positive because the implementation is wrong?** An ordinary bug in `rucc-safety` or `rucc-safe-rt`. Fixed; a regression test added.

**4. Is it a false positive because a boundary was mis-declared?** A wrapper in document 10.3's table has the wrong effects clause, or an allocator is not calling the interposition API correctly. Fixed in the table, which is data.

**5. Is it a legitimate exemption?** Hand-written assembly, a genuine `MAP_FIXED` case the API cannot express, a construct C forbids that the project does deliberately and knowingly. Becomes a declared region with a written reason, counted, and a document 17 entry if the reason generalizes.

**There is no sixth bucket.** In particular there is no "won't fix, add to suppressions." Every report is one of the five, and the count of each is in the scoreboard.

## 12.7 Finding bugs the corpus does not contain

The corpus's test suites exercise what the maintainers thought to test, which is not where the bugs are. Three additional sources of executed operations, per document 02's coverage limit.

**Re-running OSS-Fuzz corpora.** Every OSS-Fuzz project publishes its accumulated corpus. Running those inputs against a Tier D build of the same project is the cheapest possible way to find bugs ASan missed, and the classes ASan misses (post-quarantine use-after-free, intra-object overflow, uninitialized reads, type confusion) are precisely the ones we add. **This is the highest expected-value activity in the entire project relative to its cost**, because the inputs already exist, the harnesses already exist, and the only new thing is the checker. Document 16 puts it in S5, early.

**New fuzzing under Tier D.** Standard libFuzzer/AFL++ loops with the Tier D build. Slower per iteration than ASan, catching more per iteration; whether that trade is favorable is an empirical question and one worth publishing either way.

**syzkaller against Tier K.** Document 11 section 11.8.

## 12.8 The comparison protocol

When we claim to find something an existing tool does not, the claim has to be run properly or it is worthless.

- **Same inputs.** The identical corpus, replayed against both builds.
- **Same wall-clock and CPU-hours**, not the same iteration count. A 4x slower build does 1/4 the iterations; normalizing by iterations flatters us and normalizing by time is the honest comparison. Report both, lead with time.
- **Same project version and configuration.**
- **Deduplicated by hand.** Automatic stack-hash deduplication merges distinct bugs and splits single ones; a headline count derived from it is not trustworthy.
- **Published in both directions.** Bugs ASan finds that we do not are reported in the same table. There will be some (ASan's redzones catch a small overflow past an allocation whose bounds we round to 16-byte granularity differently) and hiding them would poison every other number in the file.

## 12.9 What the evidence cannot show

Stated because the scoreboard's authority depends on its limits being visible.

A green scoreboard means the corpus's executed paths contain no violations the monitor detects. It does not mean the corpus is memory-safe: unexecuted paths (the coverage limit), model-external bugs (the model limit), and uninstrumented code (the boundary limit) are all invisible, and the third of those is at least quantified in the trust-set counts.

A high discharge rate means most checks were proved unnecessary. It does not mean they *were* unnecessary, an unsound elimination shows up as a high discharge rate and a clean run, which is exactly what success looks like. Document 14 section 14.3's differential check accounting exists because the scoreboard cannot distinguish the two on its own, and it runs on every nightly for that reason.
