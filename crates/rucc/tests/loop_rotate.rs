//! A loop tested at the top whose last block only steps it on is laid out with the test last, so
//! that each time round takes one jump rather than two or three.
//!
//! Issue 1994. The carry loop of Postgres' `accum_sum_carry` came out with a block after its exit
//! test that copies the carry for the next turn and jumps back to the top, so a digit with no
//! carry took a jump to the arm that clears it, a jump back to the store and the jump back to the
//! top. `loop_rotate.c` is that loop and the inner loops of `mul_var` and `accum_sum_add`, which
//! are one block each and are here so that they stay that way. The unit tests in `rucc-codegen`
//! cover the layout. This runs the compiler, reads the blocks and jumps it wrote, and counts the
//! jumps taken on every way round each loop.

use std::process::Command;

/// The assembly the compiler writes for the fixture for x86-64 at `-O2`.
fn assembly() -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/loop_rotate.c");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg("--target=x86_64-unknown-linux-gnu")
        .args(["-S", "-o", "-", "-O2"])
        .arg(path)
        .output()
        .expect("the compiler is built before its own tests run");
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "the compiler refused the fixture:\n{said}");
    String::from_utf8(out.stdout).expect("what the compiler writes is text")
}

/// The lines of one function, from its label to the next function's.
fn body<'a>(asm: &'a str, name: &str) -> Vec<&'a str> {
    let start = format!("{name}:");
    asm.lines()
        .map(str::trim)
        .skip_while(|line| *line != start)
        .skip(1)
        .take_while(|line| !line.ends_with(':') || line.starts_with(".L"))
        .collect()
}

/// The blocks of a function's lines as edges `(from, to, taken)`, where `taken` is false for
/// the fall into the next label.
fn edges(lines: &[&str]) -> Vec<(String, String, bool)> {
    let mut found = Vec::new();
    let mut here: Option<String> = None;
    let mut falls = false;
    for line in lines {
        if let Some(label) = line.strip_suffix(':') {
            if let Some(from) = here.as_ref().filter(|_| falls) {
                found.push((from.clone(), label.to_owned(), false));
            }
            here = Some(label.to_owned());
            falls = true;
            continue;
        }
        let Some(from) = here.as_ref() else { continue };
        let (op, to) = line.split_once('\t').unwrap_or((*line, ""));
        if op.starts_with('j') {
            found.push((from.clone(), to.to_owned(), true));
            falls = op != "jmp";
        } else if op == "ret" {
            falls = false;
        }
    }
    found
}

/// How many jumps are taken on each way round a loop in the edges, one count for each cycle
/// that does not pass through a block twice.
fn turns(edges: &[(String, String, bool)]) -> Vec<usize> {
    fn walk(
        edges: &[(String, String, bool)],
        start: &str,
        path: &mut Vec<String>,
        taken: usize,
        found: &mut Vec<usize>,
    ) {
        let at = path.last().expect("a walk has a block").clone();
        for (from, to, jumps) in edges {
            if *from != at {
                continue;
            }
            let taken = taken + usize::from(*jumps);
            if to == start {
                found.push(taken);
            } else if to.as_str() > start && !path.contains(to) {
                path.push(to.clone());
                walk(edges, start, path, taken, found);
                path.pop();
            }
        }
    }
    let mut found = Vec::new();
    let mut starts: Vec<&String> = edges.iter().map(|(from, _, _)| from).collect();
    starts.sort();
    starts.dedup();
    for start in starts {
        walk(edges, start, &mut vec![start.clone()], 0, &mut found);
    }
    found
}

#[test]
fn every_way_round_the_numeric_loops_takes_one_jump() {
    let asm = assembly();
    for name in ["carry_inner", "mul_inner", "accum_inner"] {
        let turns = turns(&edges(&body(&asm, name)));
        assert!(!turns.is_empty(), "{name} has no loop:\n{asm}");
        assert!(
            turns.iter().all(|&taken| taken == 1),
            "{name} goes round in {turns:?} jumps:\n{asm}"
        );
    }
}
