//! A value a loop carries round stays in one register from one turn to the next.
//!
//! Issue 1965. Once a `static` function with a `switch` in it is inlined into a loop, the default
//! arm of the `switch` is laid out after the addition that joins it, and the sum the loop carries
//! is still live in that arm. The allocator used to take that as the sum being wanted after the
//! addition, so the new sum went into a register of its own and was copied back over the old one
//! at the bottom of every turn. The unit tests in `rucc-regalloc` cover the decision. These run
//! the compiler and read the loop it wrote.

use std::path::PathBuf;
use std::process::Command;

/// Written down rather than taken from the host, so the assertions are about the compiler and
/// not about the machine the suite ran on.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// The shape from the issue, cut down: a sum and a counter carried round a loop, and a `switch`
/// with a default arm inlined into the middle of it.
const SOURCE: &str = "\
static int affine(int x) {
    switch (x) {
    case 0: return 3; case 1: return 5; case 2: return 7; case 3: return 9;
    case 4: return 11; case 5: return 13; case 6: return 15; case 7: return 17;
    default: return 0;
    }
}

long run(const int *stream, int n) {
    long total = 0;
    for (int at = 0; at < n; at++)
        total += affine(stream[at & 4095]);
    return total;
}
";

/// The assembly the compiler writes for the fixture at this level.
fn assembly(level: &str) -> String {
    let dir = std::env::temp_dir().join(format!(
        "rucc-loop-carried-{}-{}",
        std::process::id(),
        level.trim_start_matches('-')
    ));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path: PathBuf = dir.join("carried.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args(["-S", "-o", "-", level])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "the compiler refused the fixture at {level}:\n{said}");
    let _ = std::fs::remove_dir_all(&dir);
    String::from_utf8(out.stdout).expect("what the compiler writes is text")
}

/// The instructions in front of every jump back round a loop, back to the branch or the label
/// before them, which is the code that runs on the way round and nowhere else.
///
/// A jump back round a loop is one to a label written earlier with no `ret` in between, and it may
/// be the loop's own test when nothing else stands at the bottom of it. A jump to a label written
/// earlier that does have a `ret` in between is a block sharing the epilogue, not a loop.
fn back_edges(asm: &str) -> Vec<Vec<String>> {
    let lines: Vec<&str> = asm.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    let mut seen: Vec<(&str, usize)> = Vec::new();
    let mut edges = Vec::new();
    for (at, line) in lines.iter().enumerate() {
        if let Some(label) = line.strip_suffix(':') {
            seen.push((label, at));
            continue;
        }
        let Some((opcode, target)) = line.split_once(char::is_whitespace) else { continue };
        if !opcode.starts_with('j') {
            continue;
        }
        let Some(&(_, from)) = seen.iter().find(|(label, _)| *label == target.trim()) else {
            continue;
        };
        if lines[from..at].contains(&"ret") {
            continue;
        }
        let mut edge = Vec::new();
        for before in lines[..at].iter().rev() {
            if before.ends_with(':') || before.starts_with('j') {
                break;
            }
            if !before.starts_with('.') {
                edge.push((*before).to_owned());
            }
        }
        edges.push(edge);
    }
    edges
}

/// Whether an instruction copies one register into another. A move that widens, such as
/// `movslq`, makes a value the loop did not have before it and is not a copy.
fn copy(inst: &str) -> bool {
    let Some((opcode, operands)) = inst.split_once(char::is_whitespace) else { return false };
    matches!(opcode, "mov" | "movb" | "movw" | "movl" | "movq")
        && operands.split(',').all(|operand| operand.trim().starts_with('%'))
}

#[test]
fn the_sum_and_the_counter_are_not_copied_on_the_way_round() {
    for level in ["-O2", "-Os"] {
        let asm = assembly(level);
        let edges = back_edges(&asm);
        assert!(!edges.is_empty(), "no loop at {level}:\n{asm}");
        for edge in edges {
            assert!(
                !edge.iter().any(|inst| copy(inst)),
                "a copy on the back edge at {level}: {edge:?}\n{asm}"
            );
        }
    }
}
