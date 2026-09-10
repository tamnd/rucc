# Benchmarks

Programs that exist to be timed. Nothing in here asserts anything, so nothing in here can fail. A benchmark that fails is a test in the wrong directory.

```
bench/
  safety/      programs timed with the monitor off and on, for the overhead number    S1
```

## safety

Eight small C programs, each one a shape that `spec/safe-memory/13-performance.md` says the cost of the monitor depends on. `cargo xtask cost` compiles each of them twice from the same source at the same optimization level, once with `-fsafety=off` and once with `-fsafety=detect`, runs both, and prints a ratio per program.

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
| `a-strided-column-sum` | a sweep whose span is the whole buffer and whose reads are one element per row |

### How it is run

Ten timed runs per build, after three that are thrown away, and the number reported is the median with the interquartile range under it. The rounds are interleaved rather than grouped, so a machine that gets slower halfway through slows both sides of every ratio instead of one side of half of them.

Both sides get the same level, and it is `-O0` unless the task is told otherwise. Section 13.2 says the baseline for an overhead claim is `rucc -O2` with safety off, and that is the right baseline for a claim about a tier's budget, which S1's number is not. S1 has no check elimination in it on purpose, and the milestone calls its own number the unoptimized baseline for that reason, so `-O0` on both sides is the default and stays the default because it isolates the monitor from the optimizer.

The other number belongs to S4, where the question is how much of this the elimination rules take back, and it is the same eight programs at `-O2`. That is one flag rather than a second task, so `cargo xtask cost -O2` is how it is asked for, and `-O1` and `-Os` are accepted too. Both sides always get the level that was asked for: a ratio between an optimized program with the monitor off and an unoptimized one with it on would be a measurement of the optimizer.

The table is per program because section 13.4 rule 1 says a geomean may appear beside a table and never instead of one, and the worst case is printed as a headline because rule 2 says it is one.

### What the number does not include

Wall clock only. Section 13.1 also asks for cache misses, memory traffic, peak RSS, branch mispredictions and spill counts, and says an instruction count is never the headline. None of those counters are readable through a container on a developer machine, and reading them on a CI runner means `perf`, which needs a permission the runner does not give. So the reported number is the one anybody can reproduce and the missing ones are named in the output rather than quietly skipped.

The spill counts are the exception, and they are `cargo xtask pressure` rather than a counter read at run time. The compiler knows what its allocator put on the stack, so `-Zregister-pressure=FILE` asks it, and the task compiles the same eight programs at `-O2` with the monitor off and on and prints the difference. It runs anywhere, since nothing is executed. Document 05 section 5.2.1 is why the number matters: a capability in flight is four words in registers, and if materializing one spills something else in a hot loop then no amount of check elimination saves it.

### Where it means anything

On an x86-64 Linux machine. That is the only back end, so anywhere else the programs run in a container under emulation, which changes the ratio between the cost of an instruction and the cost of a cache miss, and that ratio is the entire subject. The task says so in its own output when it happens. An emulated run is worth doing to check the apparatus works and is worth nothing as a measurement.

The nightly workflow runs it natively on `ubuntu-24.04`, and that job's log is where the baseline comes from.
