# Tests

The workspace's unit tests live next to the code they test, as `#[cfg(test)]` modules. This directory is for the parts that do not belong to any one crate.

The layout below is what `spec/15-testing.md` describes, and each directory appears when the milestone that needs it does.

```
tests/
  golden/      .c inputs with expected --emit=tast, --emit=ir and --emit=mir-final output,
               regenerated with `cargo xtask bless`                                    M2
  accept/      programs that must compile, and programs that must not, at each -std=   M2
  safety/      programs with a known memory safety verdict, compiled with -fsafety     S1
  exec/        programs with a known exit status or output, run on every target        M3
  corpus/      checkouts and build recipes for the target ladder projects              M5
  fuzz/        cargo-fuzz targets against the driver, the parser and the IR            M3
```

Two rules from `spec/15-testing.md` section 15.7 that matter more than the layout:

No test is deleted to make CI green. A test that has to stop running is marked, given an issue number, and counted in a report that is visible.

A golden file is compared byte for byte. `.gitattributes` marks the expected-output extensions as binary so that git cannot rewrite line endings on a Windows checkout and turn a real difference into a passing test or a passing test into a failure.

## golden

Each case is a `.c` file with a `.tast` beside it holding the typed tree it produces, and an `.ir` beside it holding the IR the walk lowers it to. The harness is `crates/rucc/tests/golden.rs`, which runs the compiler the same way for every case, at a fixed target and from the top of the repository, so that the expectation is a fact about the compiler and not about the machine CI happened to run on or where the checkout is.

Change a case, or change the compiler, and `cargo xtask bless` rewrites the expectations. Running it is half of the job. The other half is reading the diff, because a golden file that gets blessed without anybody looking at what changed is a test that has stopped testing.

A case that produces a diagnostic is refused rather than blessed. The expectations hold the tree and not the messages.

The one refusal that is not a mistake is a construct the walk to the IR has not been written for yet, which is a case with a `.tast` and no `.ir` beside it. The suite checks that such a case still cannot be lowered, so the day it starts lowering is the day the suite asks for the expectation rather than a day the coverage quietly went missing.

## accept

Each case is a `.c` file whose leading comments say what is supposed to happen to it, under each of the ten dialects.

```c
/* accept: c99 c11 c17 c23 gnu */
/* reject: c89 */
/* message: unknown type name */
/* gap: #98 c89 */
```

`accept` and `reject` take a list of dialects, and between them they have to name every one, because a dialect nobody mentioned is a dialect nobody thought about. `all`, `iso` and `gnu` stand for the obvious groups. `message` is a substring every rejection has to contain, so a program rejected for the wrong reason is not counted as a pass. `warns` is the same for an acceptance, and without it an acceptance has to be silent.

`gap` names the dialects where the compiler does not do this yet, and the issue that says when it will. Those pairs are run backwards: the case is expected to fail, and the suite fails if it starts passing. That is how the first rule above is kept, and the count is `KNOWN_GAPS` in `crates/rucc/tests/accept.rs`, which is a number in the source rather than a line in a log so that changing it is something somebody has to approve.

The directives use the old kind of comment because a case that runs under `-std=c89` cannot use `//` for its own directives.

## safety

Each case is a C program with a verdict in the comments at the top of it, and the suite compiles it, links it against `rucc-safe-rt`, runs it, and holds what came out to what the file said would.

```c
/* row: T1 */
/* refuse: J1 */
/* says: which has been freed */
```

`row` names the row of `spec/safe-memory/03-bug-model.md` the case is about, or the idiom from section 3.5 that must not produce a report. Every case has one, because the count of rows covered is what milestone S1 is measured by and a program belonging to no row does not move it.

`refuse` is the judgement of `spec/safe-memory/04-safety-model.md` section 4.4 the report has to name, and `says` is a substring it has to contain, as many times as there are things worth pinning. A program refused for the wrong reason is not a pass, in the same way a program rejected for the wrong reason is not one in `accept`. The opposite verdict is `allow`, which asks for no report at all and an exit status of zero.

`gap` names the issue that will make the case pass, for a row nothing catches yet. Those run backwards: the refusal must not happen, and the suite fails the day it starts happening and asks for the line to be taken out. That is the same rule as `accept`'s gaps and it is there for the same reason, which is that the alternative is deleting the case and forgetting the row exists.

`blocked` is the same thing one step earlier, for a program the compiler cannot build yet. The compilation has to fail, and the suite fails the day it succeeds. A blocked case still carries its verdict, so the day the construct lowers the case runs against an expectation somebody wrote before they knew what the compiler would do, and it is not counted in the rows covered, because a program that does not build is not evidence about the monitor.

The comments that are not directives are prose, and every case has some. A program in this suite is here because of a specific bug or a specific idiom, and the reason belongs beside the program rather than in a table somewhere else.

Run it with `cargo xtask safety`. The programs are x86-64 Linux ones because that is the only back end, so on an x86-64 Linux machine they run directly and anywhere else they run in a container, which is one `docker run` for the whole suite. This is not part of `cargo xtask ci`: a developer on an arm mac should not need a container running to check their work, and CI runs it as its own job on a machine where it costs nothing.

The cases declare `malloc` and `free` themselves rather than including a header, because rucc has no built-in system include directories and a case that included one would be testing whichever headers the machine happened to have.
