# Tests

The workspace's unit tests live next to the code they test, as `#[cfg(test)]` modules. This directory is for the parts that do not belong to any one crate.

The layout below is what `spec/15-testing.md` describes, and each directory appears when the milestone that needs it does.

```
tests/
  golden/      .c inputs with expected --emit=tast, --emit=ir and --emit=mir-final output,
               regenerated with `cargo xtask bless`                                    M2
  accept/      programs that must compile, and programs that must not, at each -std=   M2
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
