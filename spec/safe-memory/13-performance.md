# Performance: the budget, and the rules for reporting against it

Document 02 sets three numbers per tier. This document says how they are measured, what they are measured against, and (the part that most published work in this area gets wrong) what must be reported alongside them for the number to mean anything.

The governing claim: **this design's cost is dominated by memory traffic, not by instructions, and any methodology that counts instructions will produce a number that is both flattering and useless.**

## 13.1 Why instruction counts are the wrong metric

Document 05 section 5.5 already states the prediction: the thing that will actually hurt is that a pointer chase through a linked structure touches two cache lines per node instead of one. That is not visible in an instruction count. A check that adds four instructions and one L2 miss to a loop iteration costs far more than a check that adds twelve instructions and no miss, and the two are indistinguishable to `perf stat -e instructions`.

The evidence that this is the right worry is Cpp2Rust's evaluation: 2% on WOFF2 and 6x on Brunsli, in the same 13,000-line study, with the difference attributed to pointer-arithmetic density. Two orders of magnitude of spread within one tool, driven entirely by data-structure shape.

**Required metrics for every measurement in this project:**

| Metric | Why |
|---|---|
| Wall-clock time | the number anyone actually cares about |
| L1D, L2, LLC miss counts | the predicted dominant cost |
| Total memory traffic (bytes from DRAM) | aux plane and shadow traffic, directly |
| Peak RSS | document 02's memory budget |
| Branch mispredictions | checks are branches; predictable ones are nearly free, and *which* is empirical |
| Register spill/fill counts | document 05 section 5.2.1's stated risk |
| Static: checks emitted, discharged, remaining | document 07 section 7.8; the causal story |
| Static: code size | checks are code; icache pressure is a second-order cost that becomes first-order in a kernel |

Instruction count is *permitted* as a supporting metric and is never the headline.

**What it is good for, worked through.** The headline is the wall clock and the supporting metric answers a different question, which is where the time went. Counting instructions under callgrind on the Tier E set at `-O2`, both sides the same commit, splits the eight programs into two groups that want completely different work out of us:

| program | off | on | ratio | in the runtime | share |
|---|---|---|---|---|---|
| a-binary-tree-walk | 295577596 | 2229672545 | 7.54x | 1693447163 | 76.0% |
| a-byte-at-a-time-copy | 131891502 | 263718047 | 2.00x | 0 | 0.0% |
| a-linked-list-traversal | 101941985 | 970542037 | 9.52x | 765000000 | 78.8% |
| a-matrix-multiply | 121697825 | 435473067 | 3.58x | 60254327 | 13.8% |
| a-pointer-chasing-hash-table | 208906963 | 2815978229 | 13.48x | 2327804368 | 82.7% |
| a-program-that-does-nothing | 118931 | 118939 | 1.00x | 0 | 0.0% |
| a-strided-column-sum | 158795097 | 337658630 | 2.13x | 19641246 | 5.8% |
| a-string-scan | 747341242 | 1267156107 | 1.70x | 0 | 0.0% |

The runtime column is every instruction retired inside a `__rucc_` entry point, so it is the checks plus the guard's extent queries. Three programs spend three quarters or more of their instructions there and cost 1.5x to 2.5x on the clock, which is this section's claim stated as a number: they retire seven to thirteen times the instructions for twice the time, because the machine is stalled on the next node either way. The other four have no runtime left to speak of. On `a-matrix-multiply` the 13.8 percent is 160003 calls to `__rucc_extent` at about 377 instructions each, which is the loop split's guard asking once per entry to the inner loop, and no bounds check and no derivation check runs anywhere in the program. On the other three it is nothing at all.

So on half the set the checks are gone at run time and the program is still 1.5x to 2.8x, and every instruction of that is code the compiler emits around where the checks used to be. That is not a thing a wall clock ratio can tell you and it is not a thing an instruction count should ever be the headline for. It is the supporting metric doing the job it is kept for, and it is tracked on #893.

**What the census bought, first instalment.** The 160003 extent queries on the matrix multiply are four per entry to the inner loop and two of them ask about a pointer that has not moved since it was allocated, so half of them are loop invariant and were being asked 40000 times anyway. Hoisting those is `licm` and `licm` was not looking, for three separate reasons given in that file's header and in `crates/rucc-opt/src/pipeline.rs`. With all three addressed the same eight programs measure:

| program | before | after | change |
|---|---|---|---|
| a-binary-tree-walk | 2229672545 | 2229672398 | 0.0% |
| a-byte-at-a-time-copy | 263718047 | 237358054 | -10.0% |
| a-linked-list-traversal | 970542037 | 970541890 | 0.0% |
| a-matrix-multiply | 435473067 | 373113826 | -14.3% |
| a-pointer-chasing-hash-table | 2815978229 | 2815890537 | 0.0% |
| a-program-that-does-nothing | 118939 | 118939 | 0.0% |
| a-strided-column-sum | 337658630 | 249064524 | -26.2% |
| a-string-scan | 1267156107 | 1069410366 | -15.6% |

The matrix multiply's extent cost is 60254327 before and 30255077 after, which is the half that was invariant, exactly. Against the uninstrumented baseline in the table above the overhead on the four array programs falls by a fifth on the copy and the multiply, by 38 percent on the scan and by half on the column sum, which takes that row from 2.13x to 1.57x. The three pointer chasers do not move, and that is the census being right about them: their cost is the checks themselves and no amount of work on the guard touches it. The rest of the guard is still #893.

## 13.2 The baseline question

The most common dishonesty in this literature is an unstated baseline. Ours are stated:

**For overhead numbers, the baseline is `rucc -O2` with safety off, same commit, same flags otherwise.** Not GCC, not Clang, not `rucc -O0`. This isolates the cost of the monitor from the cost of the compiler, which is the parent's document 16 problem and is measured separately.

**For competitive comparisons against ASan, MSan, Fil-C or KASAN, the baseline is that tool's own baseline compiler** (Clang for the sanitizers, Fil-C's own baseline for Fil-C, GCC for KASAN) and both ratios are reported. Comparing our ratio-against-rucc with ASan's ratio-against-clang is legitimate only as a ratio-of-ratios, and only if `rucc -O2` and `clang -O2` are within a stated distance of each other on the same benchmark, which the parent's document 16 measures. If `rucc -O2` is 15% slower than `clang -O2` on a benchmark, a 1.3x safety overhead on `rucc` is a 1.5x absolute cost and the absolute number is the one a user experiences. **Report both; lead with the absolute.**

**For Tier K, the baseline is the same kernel config with `CONFIG_RUCC_SAFETY=n`,** and the comparison points are `CONFIG_KASAN_GENERIC` and `CONFIG_KASAN_HW_TAGS` builds of the same tree.

## 13.3 The budget, decomposed

Document 02's tier table gives totals. A total is not actionable; this is where the total comes from, so that a regression can be attributed.

**Tier E, 1.3x geomean budget:**

| Component | Predicted | Notes |
|---|---|---|
| Surviving bounds checks | 8-12% | after document 07; the literature says 80% of dynamic checks are eliminable |
| Surviving liveness checks | 5-10% | worse than bounds, per document 08.8: any call kills the fact |
| Aux plane traffic | 8-15% | document 05's headline risk; entirely data-shape dependent |
| Call-frame capability passing | 2-5% | document 05.3; eliminable per document 17 question 4 |
| Allocation-path plane writes | 1-3% | proportional to allocation size, not count |
| Lost optimizations | 3-8% | document 07.4: checks in loop bodies block vectorization |
| **Total** | **27-53%** | budget is 30% |

The honest reading of that table is that **the budget is at the optimistic end of the prediction**, and if the aux traffic or the lost optimizations land at the top of their ranges, Tier E is 1.5x and is a draw with Fil-C rather than a win. Document 02 section 2.6 says a Tier E above 2x should stop the project; between 1.3x and 2x it is a narrower win than claimed and the specification should say so rather than quietly moving the target.

**Tier D, 4x geomean budget:** Tier E's cost plus the type plane (15-25%, document 09.8), the byte-granular init plane (10-20%), the epoch plane (<5%), and (the large one) the loss of most check elimination, because Tier D's whole point is that checks are not removed on the strength of anything but a verified rule. The 4x figure assumes the type and init planes' *maintenance* traffic dominates their check traffic, which is the opposite of the usual assumption and is document 09's least certain claim.

**Tier K, 3x budget:** measured on the kernel's own benchmarks, not on SPEC. Section 13.6.

## 13.4 The reporting rules

Binding on every performance claim this project makes, internally or publicly.

**1. Per-benchmark, always.** A geomean may be reported *in addition to* the full table, never instead of it. Document 02 axis 3.

**2. The worst case is a headline number.** If one benchmark is 8x, "4x geomean" is a misleading summary and the 8x goes in the abstract.

**3. State what was excluded.** If a benchmark does not build at a tier, or a region was declared exempt to make it build, that appears next to the number. A benchmark that runs fast because half of it was exempted is not a data point, and the trust-set counts from document 12.5 are reported alongside every cost number for exactly this reason.

**4. Report the discharge rate with the cost.** "3.4x with 88% of checks discharged" and "3.4x with 40% discharged" describe very different situations: the first says the design is near its ceiling, the second says there is headroom.

**5. Cold-start and steady-state separately.** The plane-mapping and early setup cost is a fixed startup charge, which matters enormously for short-lived processes (`git status`, a compiler invocation, a shell script's thousand `sed` calls) and not at all for a server. A geomean over benchmarks that all run for thirty seconds hides a tool that is unusable for command-line programs.

**6. No cherry-picked compiler flags.** The same optimization level, LTO setting and target CPU on both sides.

**7. Publish the regressions.** A change that improves the geomean and doubles one benchmark is reported as both.

## 13.5 The measurements that decide design questions

Not all measurement is scorekeeping. Five specific numbers settle open questions and document 16 schedules each of them before the thing that depends on it.

**Aux plane locality (S3).** Does allocating aux adjacent to the object, per document 05.2.2, actually avoid the second miss, or does it merely double the object's footprint and evict something else? Done, and the answer is the second one. It was measured by simulation rather than by building the corpus, because the monitor does not exist at S3 and the point of asking early was to find out before S1's allocator and S2's boundary are written around the constraint. The result is in document 05.2.6: shadow-mapped aux is never worse on trips to memory, better on five of seven patterns on page walks, and three to four times smaller, because an aux array is twice the payload and adjacency therefore buys the same page and never the same line. Document 10.4's adopted third-party allocators are acceptable and `rucc-safe-rt`'s allocator is not mandatory. Whether the default should change is document 17 question 11.

**Type-plane granule homogeneity (S3).** Document 17 question 6, and Tier D's 2x memory budget rests on it entirely. Done, and it was as cheap as this document expected: `--emit=type-granules` reads the layouts the front end already has and counts heterogeneous granules, so it needed none of the type plane and none of DWARF. The result is in document 05.2.5 and it changed the granule from 16 bytes to 8, which is the whole argument for running the cheap experiments before the expensive implementation.

**PICO+CHOP composition (S4).** Document 17 question 3, and Tier E's budget rests on it. Measured as the discharge rate with each elimination source enabled independently and together; if the sources overlap heavily, the combined rate is much lower than the sum and Tier E's budget is wrong. Done, and the answer is in document 17 question 3. It is two answers that point opposite ways. The sources barely overlap in which checks they discharge, 26 percent of SQLite's checks together against 31 percent added up, so the rate is close to the sum. They do not compose on the clock at all: the loop passes alone take 78 percent of the Tier E overhead off and the discharge pass alone takes 7 percent, and the two together are the same 1.74x to 1.80x as the loop passes on their own. The reason is that the checks the two sides prove are worth different amounts, since a quarter of a program's checks can be a quarter that never runs twice. Tier E's budget is wrong and more sources of facts will not fix it.

**Call-frame elision rate (S4).** Document 17 question 4. Measured as the fraction of instrumented calls where the frame is dropped. Done as far as it can be done before a frame exists, and the answer is in document 17 question 4: on SQLite at `-O2` the rule fires on 22 percent of the calls that hand a pointer over, and the reason it does not fire on the rest is almost entirely that the callee still has checks in it rather than that the callee is in another translation unit. The number is a static count of where the rule would fire, because `rucc-safe-rt` has the reader side of the frame and no call site writes one, so there is nothing yet to count at run time.

**Register pressure (S4).** Document 05.2.1's stated risk. Measured as spill/fill delta on the pointer-heavy benchmarks, by `cargo xtask pressure`, which asks the allocator rather than the machine and so needs nothing executed. Done, and the risk as stated did not turn up while a different one did. The monitor takes the set from 24 stack slots and 86 stack moves to 89 and 374, and the pointer-heavy rows are not where it happens: the tree walk spills nothing either way and the two chasing rows add 6 and 22 moves. The four array rows add 42, 91, 66 and 61. Compiling both sides with `-fno-split` leaves 40 slots and 170 moves, so 204 of the 288 added moves are the loop split's, which is 71 percent of the pressure the monitor adds and none of it is a materialized capability. It is the guard's carried byte offset per distinct step and the window bound it is tested against, all of them loop-carried, all of them in a frame slot. The representation does not need revisiting. What the split emits around the fast half does, and it is #893.

Each of these can invalidate a number in document 02, and each is scheduled *before* the milestone that would depend on it. That ordering is the point.

## 13.6 Kernel measurement

Different apparatus, because kernel performance is not benchmark performance.

**The workloads:** `kernbench` (build-the-kernel, the traditional one), `hackbench` (scheduler and IPC), `fio` against NVMe and against tmpfs (block and page cache), netperf and iperf3 loopback and NIC (network stack, the most latency-sensitive path), `will-it-scale` (allocator and lock contention), and a syscall microbenchmark that isolates the `copy_*_user` checks from document 11.4.

**Latency, not just throughput.** The kernel's users care about p99. A check that adds 2% to throughput and 40% to p99 latency because it introduces a cache miss on the interrupt path is a failure, and a throughput-only measurement reports it as a success. Report p50, p99 and p99.9.

**Memory is measured as a fraction of RAM,** per document 02's table, not as a ratio, because the kernel's shadow is a fixed fraction of the address space and the meaningful question is how much of a 4 GiB machine is left.

**Boot time** is a first-class metric: plane initialization happens during early boot and a kernel that takes 30 seconds to boot is not testable at scale, which matters directly for syzkaller throughput in document 11.8.

## 13.7 Where the cost is expected to be unacceptable, honestly

Written down in advance so that measuring them is confirmation rather than discovery.

**Pointer-dense linked structures.** Two lines per node. Red-black trees, intrusive lists, graph traversals. This is the kernel's dominant data-structure idiom and it is our worst case. Expect 2-3x at Tier E on a pure pointer-chase microbenchmark and be suspicious of any result better than that.

**Short-lived processes.** Startup cost, per reporting rule 5.

**Large, short-lived, untouched allocations.** Document 08.8's named pathological case: a 12.5% `memset` the program was not doing, on both allocation and free.

**Deep call chains of small functions.** Every instrumented call writes a TLS frame. Inlining removes it, and code that defeats inlining (function pointers, large functions, cross-module calls without LTO) pays it every time.

**Interpreters.** CPython and Lua are in the corpus for precision, and their dispatch loops are pointer-heavy, tagged-pointer-heavy and allocation-heavy at once. Expect the worst numbers in the corpus here.

**A loop the split guard costs more than the checks did.** Not on this list when it was written, and on it now, because the Tier E set found it. A loop whose checks all come out is a loop that carries the guard's bookkeeping instead, one offset per distinct step and a bound to test it against, and on a tight array loop that is more stack traffic than the checks ever were. It shows up as the array half of the table in section 13.1 and as 71 percent of the spilling in section 13.5, and it is the reason the worst Tier E row is a matrix multiply rather than a pointer chase, which is the opposite of what the first entry on this list predicts.

If the measured shape does not match this list, the model is wrong somewhere and finding out where is more valuable than the number itself.
