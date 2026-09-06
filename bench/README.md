# Benchmarks

Programs that exist to be timed. Nothing in here asserts anything, so nothing in here can fail. A benchmark that fails is a test in the wrong directory.

```
bench/
  safety/      programs timed with the monitor off and on, for the overhead number    S1
```

## safety

Seven small C programs, each one a shape that `spec/safe-memory/13-performance.md` says the cost of the monitor depends on. `cargo xtask cost` compiles each of them twice from the same source at the same optimization level, once with `-fsafety=off` and once with `-fsafety=detect`, runs both, and prints a ratio per program.

The programs are chosen for their memory access pattern and not for being realistic. A benchmark set of real programs is milestone S5's, and it is a different job: those numbers say what a user would feel, and these say where the cost comes from. The linked list one is the one to watch, because document 05 section 5.5 predicts two cache lines per node where an unchecked program touches one, and that is a cost no instruction count can see.

| program | what it is for |
| --- | --- |
| `a-program-that-does-nothing` | the startup charge on its own, which is section 13.4 rule 5's cold start row |
| `a-linked-list-traversal` | the predicted worst case, a second line per node |
| `a-binary-tree-walk` | the same access pattern through a recursive call, where the checks meet a live stack |
| `a-pointer-chasing-hash-table` | an unpredictable index followed by a short walk, where a check costs a misprediction rather than a miss |
| `a-byte-at-a-time-copy` | the memcpy row in the only form available before the boundary wrappers exist |
| `a-matrix-multiply` | dense indexing in cache, where the cost is the added instructions and nothing else |
| `a-string-scan` | one cursor walking forward, the cheapest thing there is to check |

### How it is run

Ten timed runs per build, after three that are thrown away, and the number reported is the median with the interquartile range under it. The rounds are interleaved rather than grouped, so a machine that gets slower halfway through slows both sides of every ratio instead of one side of half of them.

Both sides are `-O0`. Section 13.2 says the baseline for an overhead claim is `rucc -O2` with safety off, and that is the right baseline for a claim about a tier's budget, which this is not. S1 has no check elimination in it on purpose, and the milestone calls its own number the unoptimized baseline for that reason. `-O0` on both sides isolates the monitor from the optimizer. The `-O2` comparison belongs to S4, where the question is how much of this the rules take back.

The table is per program because section 13.4 rule 1 says a geomean may appear beside a table and never instead of one, and the worst case is printed as a headline because rule 2 says it is one.

### What the number does not include

Wall clock only. Section 13.1 also asks for cache misses, memory traffic, peak RSS, branch mispredictions and spill counts, and says an instruction count is never the headline. None of those counters are readable through a container on a developer machine, and reading them on a CI runner means `perf`, which needs a permission the runner does not give. So the reported number is the one anybody can reproduce and the missing ones are named in the output rather than quietly skipped.

### Where it means anything

On an x86-64 Linux machine. That is the only back end, so anywhere else the programs run in a container under emulation, which changes the ratio between the cost of an instruction and the cost of a cache miss, and that ratio is the entire subject. The task says so in its own output when it happens. An emulated run is worth doing to check the apparatus works and is worth nothing as a measurement.

The nightly workflow runs it natively on `ubuntu-24.04`, and that job's log is where the baseline comes from.
