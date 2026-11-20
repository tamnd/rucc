//! A call to a function that reads a `volatile` is made as many times as the program makes it.
//!
//! The unit tests in `rucc-opt` cover the summary, which is where the bug in #2241 was: modref took
//! a `volatile` load for a plain read, so the callee looked like it only read memory, and `licm`
//! moved the call out of the loop and made it once. What is left is the whole trip, which is only
//! visible in the listing, so this runs the compiler over C and reads where the call ended up.

use std::path::PathBuf;
use std::process::Command;

/// The target is written down rather than taken from the host, so that the listing is the same on
/// every machine that runs the suite.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// Five reads of `ticks` the program asked for, one in each call.
const SOURCE: &str = "\
static volatile int ticks;
__attribute__((noinline)) static int sample(void) {
    return ticks & 0;
}
int total(void) {
    int total = 0;
    for (int i = 0; i < 5; i++) {
        total += sample();
    }
    return total;
}
";

/// The fixture, under a directory of its own so that two runs at once do not share a file.
fn fixture(level: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-volatile-{}-{level}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    path
}

/// The listing for `total` at that level, one line each.
fn total_at(level: &str) -> Vec<String> {
    let path = fixture(level);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .arg(format!("-{level}"))
        .args(["-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.lines()
        .skip_while(|line| *line != "total:")
        .take_while(|line| !line.starts_with("\t.size\ttotal"))
        .map(|line| line.trim().to_owned())
        .collect()
}

/// Whether some jump after this line goes back to a label before it, which is a loop around it.
fn in_a_loop(lines: &[String], at: usize) -> bool {
    let before: Vec<&str> = lines[..at].iter().filter_map(|line| line.strip_suffix(':')).collect();
    lines[at..].iter().any(|line| {
        line.starts_with('j')
            && line.split_whitespace().nth(1).is_some_and(|target| before.contains(&target))
    })
}

#[test]
fn a_call_that_reads_a_volatile_stays_in_its_loop() {
    for level in ["O1", "O2", "O3", "Os"] {
        let lines = total_at(level);
        let calls: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, line)| line.starts_with("call") && line.ends_with("sample"))
            .map(|(at, _)| at)
            .collect();
        let made = calls.len() == 5 || calls.iter().all(|&at| in_a_loop(&lines, at));
        assert!(!calls.is_empty() && made, "-{level}:\n{}", lines.join("\n"));
    }
}
