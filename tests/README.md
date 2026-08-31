# Tests

The workspace's unit tests live next to the code they test, as `#[cfg(test)]` modules. This directory is for the parts that do not belong to any one crate.

Nothing is here yet. The layout below is what `spec/15-testing.md` describes, and each directory appears when the milestone that needs it does.

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
