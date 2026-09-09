//! The differential ABI harness.
//!
//! Design: `spec/cross-compile/14-testing.md` section 14.3, which is layer 6 of the ladder in
//! section 14.1, and `spec/cross-compile/06-abis.md` section 6.2 items 4 to 6.
//!
//! `tests/abi-corpus` asks whether two compilers agree about where the members of a struct are,
//! and `_Static_assert` can answer that without running anything. This asks the question that has
//! no static answer: whether they agree about which register an argument arrives in, whether an
//! aggregate travels as a copy or as the address of one, and where a return value comes back.
//! None of that is visible in the source, so the only way to check it is to compile one side of a
//! call with one compiler and the other side with the other, link the two together, run the
//! program, and see whether the values arrived.
//!
//! Section 14.3's argument for doing this at all is document 01.8's result: ABI Cafe found GCC,
//! Clang and rustc disagreeing on x86-64 Linux, the most exercised ABI in existence. Implementing
//! the psABI document is not evidence that you implemented it.
//!
//! # The four builds
//!
//! Both directions, and both controls.
//!
//! | caller | callee | what a failure means |
//! | --- | --- | --- |
//! | reference | reference | the corpus is wrong, and nothing else here means anything |
//! | rucc | rucc | rucc disagrees with itself, so one of the two sides has a bug |
//! | rucc | reference | rucc gets an argument wrong on the way out, or a return on the way in |
//! | reference | rucc | rucc gets an argument wrong on the way in, or a return on the way out |
//!
//! The two mixed builds are the point and the two matching ones are what make a failure readable.
//! A corpus that is wrong about C fails all four, and saying so in one line is better than four
//! lines about calling conventions.
//!
//! # Why this is not a `#[test]`
//!
//! For the reason `xtask/src/safety.rs` gives: it needs a machine the compiler emits code for.
//! The only back end is x86-64, so a test in the workspace would fail or skip on an arm mac, and
//! a suite that skips is a suite nobody notices has stopped running. `xtask/src/runner.rs` is
//! where the container comes from on a machine that is not the one this builds for.
//!
//! # What the first run of it found
//!
//! Nothing about rucc, and something about the apparatus. All four builds agree on every value in
//! the corpus. What they also do, on a developer machine where the container runs under qemu, is
//! crash at random about once in every one hundred and thirty runs, including the build where
//! both sides are the reference compiler and there is no disagreement to have. So the script
//! tries a crash again rather than reporting it, the reasoning is written above `SCRIPT`, and the
//! number is in `spec/cross-compile/14-testing.md` section 14.4's list of what qemu does that the
//! hardware does not.
//!
//! # What this does not cover yet
//!
//! One target. The corpus is the same C for all forty two rows of the table and only one of them
//! has a back end, so what runs here is x86-64 System V and the rest of the matrix is waiting on
//! the emulation layer of section 14.4. Variadic signatures are the other gap, and they are the
//! piece where Darwin arm64 and Windows diverge from everyone else.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::Command;

use crate::runner::{Runner, TRIPLE};
use crate::{Error, Result, indent, root};

/// The optimization level both compilers are asked for.
///
/// None. A calling convention is not an optimization and the two sides have to agree at every
/// level, but a difference that only shows up at `-O2` is a difference in what got inlined into
/// what, and this corpus cannot tell that apart from a difference in the ABI. Nothing is inlined
/// across the boundary here because neither compiler can see the other's source, so `-O0` asks
/// the question with the fewest other things going on.
const LEVEL: &str = "-O0";

/// The four builds, as the pair of compilers each one uses.
///
/// In this order, so the control is first and a corpus that is simply wrong says so before four
/// paragraphs about registers.
const PAIRS: &[(&str, &str)] = &[("cc", "cc"), ("rucc", "rucc"), ("rucc", "cc"), ("cc", "rucc")];

/// Compiles the signature corpus four ways, runs each, and reports what disagreed.
///
/// # Errors
///
/// [`Error::Io`] when the corpus will not compile or there is no way to run an x86-64 Linux
/// program, and [`Error::Failed`] with one entry per build whose values did not all arrive.
pub(crate) fn differential() -> Result<()> {
    let runner = Runner::find("this harness")?;
    let work = build()?;
    let ran = read(&runner.run(&work, "the harness")?);

    let mut problems = Vec::new();
    for (caller, callee) in PAIRS {
        let name = format!("{caller}-{callee}");
        let Some(run) = ran.get(&name) else {
            problems.push(format!("{name}: did not run at all"));
            continue;
        };
        match run.status {
            Some(0) => {
                println!("differential: {caller} calling {callee}, every value arrived");
                // A retry left its note in the output, and a run that needed one is worth saying
                // out loud rather than swallowing. It is the emulation, but a rate that starts
                // climbing is a thing somebody should get to see.
                for line in run.output.lines() {
                    println!("differential:   {line}");
                }
            }
            Some(1) => problems.push(format!(
                "{name}: a {caller} caller and a {callee} callee do not agree\n{}",
                indent(run.output.trim_end())
            )),
            Some(_) => problems.push(format!(
                "{name}: crashed on every attempt\n{}",
                indent(run.output.trim_end())
            )),
            None => problems
                .push(format!("{name}: did not build or link\n{}", indent(run.output.trim_end()))),
        }
    }

    if problems.is_empty() {
        println!("differential: {} builds, {TRIPLE}, {runner}", PAIRS.len());
        return Ok(());
    }
    Err(Error::Failed { task: "differential", problems })
}

/// Builds the compiler, compiles the corpus with it, and lays out the directory the runner runs.
///
/// rucc produces assembly here and the reference compiler produces objects inside the runner,
/// which is what `xtask/src/safety.rs` does and for the same reason: rucc cannot assemble or
/// link, and the machine that can is the one the programs run on.
fn build() -> Result<PathBuf> {
    let source = root().join("tests").join("abi-signatures");
    let work = root().join("target").join("abi-differential");
    if work.exists() {
        std::fs::remove_dir_all(&work)
            .map_err(|e| Error::Io(format!("could not clear {}: {e}", work.display())))?;
    }
    std::fs::create_dir_all(&work)
        .map_err(|e| Error::Io(format!("could not make {}: {e}", work.display())))?;

    for name in ["abi.h", "report.c", "caller.c", "callee.c"] {
        std::fs::copy(source.join(name), work.join(name)).map_err(|e| {
            Error::Io(format!(
                "could not copy {name}: {e}. Run `cargo xtask abi-signatures` to write the corpus."
            ))
        })?;
    }

    let status = Command::new("cargo")
        .args(["build", "-q", "--release", "-p", "rucc"])
        .current_dir(root())
        .status()
        .map_err(|e| Error::Io(format!("could not run cargo: {e}")))?;
    if !status.success() {
        return Err(Error::Io("the compiler did not build".to_owned()));
    }
    let rucc = root().join("target").join("release").join("rucc");

    for name in ["caller", "callee"] {
        let out = Command::new(&rucc)
            .args(["-S", &format!("--target={TRIPLE}"), LEVEL])
            .arg("-o")
            .arg(work.join(format!("rucc-{name}.s")))
            .arg(source.join(format!("{name}.c")))
            .current_dir(&source)
            .output()
            .map_err(|e| Error::Io(format!("could not run the compiler: {e}")))?;
        if !out.status.success() {
            return Err(Error::Io(format!(
                "rucc would not compile {name}.c:\n{}",
                indent(String::from_utf8_lossy(&out.stderr).trim_end())
            )));
        }
    }

    std::fs::write(work.join("run.sh"), SCRIPT)
        .map_err(|e| Error::Io(format!("could not write the script: {e}")))?;
    Ok(work)
}

/// The script that assembles each side, links the four combinations and runs them.
///
/// Every combination links the same `report.c`, built here by the reference compiler, because it
/// is the one file that includes a libc header and the two files under test deliberately do not.
///
/// A build that will not link prints its log and says `nolink` rather than a status, which is a
/// different failure from a program that ran and found a value in the wrong place, and the two
/// should not be reported the same way.
///
/// # Why a crash is tried again and a wrong value is not
///
/// Because on this project's developer machines the container runs under qemu, and qemu drops a
/// program on the floor every so often for reasons that have nothing to do with the program. The
/// measured rate is seven crashes in nine hundred runs of the build where both sides are the
/// reference compiler, which is the build that cannot have an ABI disagreement in it at all. A
/// harness that reported that as a finding would be a harness people learn to rerun until it goes
/// green, and then it is not a harness.
///
/// So a program that exits 0 or 1 is believed the first time, because those are the two statuses
/// the corpus chooses for itself and neither of them is a crash. Anything else is tried again, up
/// to three times, and only a build that crashes every time is reported. At the measured rate
/// three crashes in a row is about one run in a hundred million, and a program that really does
/// crash still crashes all three times.
const SCRIPT: &str = "\
#!/bin/sh
exec 2>/dev/null
out=/tmp/differential
mkdir -p \"$out\"
cc -std=c17 -O0 -c report.c -o \"$out/report.o\" >\"$out/report.log\" 2>&1
for name in caller callee; do
    cc -std=c17 -O0 -c \"$name.c\" -o \"$out/cc-$name.o\" >\"$out/cc-$name.log\" 2>&1
    cc -c \"rucc-$name.s\" -o \"$out/rucc-$name.o\" >\"$out/rucc-$name.log\" 2>&1
done
for pair in cc:cc rucc:rucc rucc:cc cc:rucc; do
    caller=${pair%:*}
    callee=${pair#*:}
    name=\"$caller-$callee\"
    printf '<<<pair %s>>>\\n' \"$name\"
    if cc \"$out/$caller-caller.o\" \"$out/$callee-callee.o\" \"$out/report.o\" \\
        -o \"$out/$name\" >\"$out/$name.log\" 2>&1; then
        for attempt in 1 2 3; do
            \"$out/$name\" >\"$out/$name.out\" 2>&1
            status=$?
            [ \"$status\" -lt 2 ] && break
            printf 'exit %s, which is not a status this program chooses. Trying again.\\n' \\
                \"$status\"
        done
        cat \"$out/$name.out\"
        printf '<<<status %s>>>\\n' \"$status\"
    else
        cat \"$out/report.log\" \"$out/cc-caller.log\" \"$out/rucc-caller.log\" \"$out/$name.log\"
        printf '<<<status nolink>>>\\n'
    fi
done
";

/// What one build did.
struct Ran {
    /// Everything it printed, which for a failing run is one line per value that did not arrive.
    output: String,
    /// Its exit status, and [`None`] when it never got as far as running.
    status: Option<i32>,
}

/// Splits what the script printed back into one entry per build.
fn read(text: &str) -> BTreeMap<String, Ran> {
    let mut runs = BTreeMap::new();
    let mut name = String::new();
    let mut output = String::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("<<<pair ").and_then(|l| l.strip_suffix(">>>")) {
            name = rest.to_owned();
            output.clear();
            continue;
        }
        if let Some(rest) = line.strip_prefix("<<<status ").and_then(|l| l.strip_suffix(">>>")) {
            runs.insert(
                std::mem::take(&mut name),
                Ran { output: std::mem::take(&mut output), status: rest.parse().ok() },
            );
            continue;
        }
        let _ = writeln!(output, "{line}");
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_build_that_never_linked_is_not_a_build_that_passed() {
        // `nolink` does not parse as a status, and reading it as a zero would turn a corpus that
        // will not compile into four green ticks.
        let runs = read("<<<pair rucc-cc>>>\nundefined reference\n<<<status nolink>>>\n");
        assert_eq!(runs["rucc-cc"].status, None);
    }

    #[test]
    fn the_lines_a_build_printed_stay_with_that_build() {
        let runs = read(
            "<<<pair cc-cc>>>\n<<<status 0>>>\n<<<pair rucc-cc>>>\ng00: a1 did not arrive\n\
             <<<status 1>>>\n",
        );
        assert_eq!(runs["cc-cc"].output, "");
        assert_eq!(runs["rucc-cc"].status, Some(1));
        assert!(runs["rucc-cc"].output.contains("g00: a1"));
    }
}
