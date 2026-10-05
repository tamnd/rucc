//! What `-frucc-trace` says each optimizer pass did, which is what a count of firings over the
//! corpus adds up. Section 42.2 of `spec/optimizer/42-measurement.md` and tamnd/rucc#2967.

use std::path::PathBuf;
use std::process::Command;

/// The target is written down rather than taken from the host, so the pipeline is the same on
/// every machine that runs the suite.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// A function with something in it for the folder to do.
const SOURCE: &str = "int f(int a) { int b = 2 * 3; return a + b - 6; }\n";

/// A directory of its own for each test, so two of these running at once do not share a file.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-fired-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir
}

/// The line the compiler appended to its trace for one file.
fn trace(what: &str, level: &str) -> String {
    let dir = dir(what);
    let source = dir.join("one.c");
    std::fs::write(&source, SOURCE).expect("the fixture can be written");
    let trace = dir.join("trace.jsonl");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .arg(level)
        .arg(format!("-frucc-trace={}", trace.display()))
        .args(["-S", "-o"])
        .arg(dir.join("one.s"))
        .arg(&source)
        .output()
        .expect("the compiler is built before its own tests run");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let line = std::fs::read_to_string(&trace).expect("the trace was written");
    let _ = std::fs::remove_dir_all(&dir);
    line
}

/// The names `--print-pipeline` gives for a level, in order. Each pass is a line of its own, as
/// its place in the list, its name and what it does.
fn pipeline(level: &str) -> Vec<String> {
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args([level, "--print-pipeline"])
        .output()
        .expect("the compiler is built before its own tests run");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout)
        .expect("the pipeline is text")
        .lines()
        .filter_map(|line| {
            let (at, rest) = line.split_once(": ")?;
            at.parse::<u32>().ok()?;
            Some(rest.split(',').next()?.to_owned())
        })
        .collect()
}

/// The `fired` object of a trace line, as written.
fn fired(line: &str) -> &str {
    let at = line.find("\"fired\":").expect("the line has a fired object");
    &line[at + "\"fired\":".len()..line.len() - "}\n".len()]
}

#[test]
fn every_pass_that_ran_is_in_the_trace_whether_it_fired_or_not() {
    for level in ["-O0", "-O1", "-O2", "-O3", "-Os"] {
        let line = trace(&level[1..], level);
        let fired = fired(&line);
        let passes = pipeline(level);
        assert!(!passes.is_empty(), "{level} has no pipeline");
        for pass in passes {
            assert!(fired.contains(&format!("\"{pass}\":{{")), "{level}: no {pass} in {fired}");
        }
    }
}

#[test]
fn a_pass_that_fired_says_what_it_did_and_how_many_times() {
    let line = trace("o2", "-O2");
    let fired = fired(&line);
    assert!(
        fired.contains(
            "\"fold\":{\"optimized: instruction with constant operands folded to a constant\":1}"
        ),
        "{fired}"
    );
    assert!(fired.contains("\"licm\":{}"), "a pass with nothing to do is still written: {fired}");
}
