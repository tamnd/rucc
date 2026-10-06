//! Comparisons of one value joined by `&&` and `||` at `-O1`, `-Os` and `-Oz`, where nothing puts
//! them in one block and `rangetest` reads them as the chain of branches they are, which is
//! tamnd/rucc#3017. The shapes are the four of section 19.4 of
//! `spec/optimizer/19-reassociation-and-arithmetic.md`.

use std::process::Command;

const SHAPES: &str = "\
void hit(void);
void one(int x) { if (x == 1 || x == 2 || x == 3) hit(); }
void digit(int c) { if (c >= '0' && c <= '9') hit(); }
void none(int x) { if (x != 3 && x != 4 && x != 5) hit(); }
void letter(int c) { if ((c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z')) hit(); }
";

/// A directory of its own for each test, so that two of them running at once write nothing the
/// other reads.
fn scratch(what: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-range-chains-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir
}

/// The listing for [`SHAPES`] with those flags, one function at a time, and what the compiler said.
fn listing(flags: &[&str]) -> (Vec<(String, String)>, String) {
    let dir = scratch(&flags.join(""));
    let path = dir.join("one.c");
    std::fs::write(&path, SHAPES).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-unknown-linux-gnu", "-fno-asynchronous-unwind-tables", "-S"])
        .args(flags)
        .args(["-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "{said}");
    let text = String::from_utf8(out.stdout).expect("a listing is text");
    let mut functions: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        if let Some(name) =
            line.strip_suffix(':').filter(|name| SHAPES.contains(&format!(" {name}(")))
        {
            functions.push((name.to_owned(), String::new()));
        } else if let Some((_, body)) = functions.last_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    assert_eq!(functions.len(), 4, "{text}");
    (functions, said)
}

/// How many conditional jumps a body has.
fn branches(body: &str) -> usize {
    body.lines()
        .filter(|line| {
            let line = line.trim_start();
            line.starts_with('j') && !line.starts_with("jmp")
        })
        .count()
}

#[test]
fn each_chain_is_one_branch_where_short_circuit_does_not_run() {
    for level in ["-O1", "-Os", "-Oz"] {
        let (functions, _) = listing(&[level]);
        for (name, body) in &functions {
            let wanted = if name == "letter" { 3 } else { 1 };
            assert!(branches(body) <= wanted, "{level} {name}\n{body}");
        }
    }
}

#[test]
fn the_merge_is_said() {
    let (_, said) = listing(&["-O1", "-fopt-info-all"]);
    for name in ["one", "digit", "none"] {
        let line = format!("{name}: optimized: branches on comparisons of one value merged");
        assert!(said.contains(&line), "{said}");
    }
}

/// The shapes and a few more, run over every value near the constants and the ends of the type,
/// with a checksum of the answers. Every level has to give the answer `-O0` gives.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const PROGRAM: &str = "\
#include <limits.h>
#include <stdio.h>
static int n;
__attribute__((noinline)) void hit(void) { n++; }
__attribute__((noinline)) void one(int x) { if (x == 1 || x == 2 || x == 3) hit(); }
__attribute__((noinline)) void digit(int c) { if (c >= '0' && c <= '9') hit(); }
__attribute__((noinline)) void none(int x) { if (x != 3 && x != 4 && x != 5) hit(); }
__attribute__((noinline)) void letter(int c) { if ((c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z')) hit(); }
__attribute__((noinline)) void bits(int x) { if (x == 1 || x == 3 || x == 5 || x == 9 || x == 12) hit(); }
__attribute__((noinline)) void edges(unsigned x) { if (x == 0 || x == UINT_MAX || x == 1) hit(); }
__attribute__((noinline)) void wide(long x) { if (x < -5 || x > 5) hit(); }
__attribute__((noinline)) void space(unsigned char c) { if (c == ' ' || c == '\\t' || c == '\\n' || c == '\\r') hit(); }
__attribute__((noinline)) int value(int x) { int r = 4; if (x == 10 || x == 11 || x == 12) r = 9; return r; }
int main(void) {
    unsigned long sum = 0;
    int ends[] = {INT_MIN, INT_MIN + 1, -1, INT_MAX - 1, INT_MAX};
    for (int i = -400; i <= 400 + 5; i++) {
        int x = i <= 400 ? i : ends[i - 401];
        n = 0;
        one(x); sum = sum * 3 + n; n = 0;
        digit(x); sum = sum * 3 + n; n = 0;
        none(x); sum = sum * 3 + n; n = 0;
        letter(x); sum = sum * 3 + n; n = 0;
        bits(x); sum = sum * 3 + n; n = 0;
        edges((unsigned)x); sum = sum * 3 + n; n = 0;
        wide((long)x * 3); sum = sum * 3 + n; n = 0;
        space((unsigned char)x); sum = sum * 3 + n;
        sum = sum * 3 + (unsigned long)value(x);
    }
    printf(\"%lu\\n\", sum);
    return 0;
}
";

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn every_level_gives_the_answer_o0_gives() {
    let dir = scratch("run");
    std::fs::write(dir.join("a.c"), PROGRAM).expect("the program can be written");
    let mut answers = Vec::new();
    for level in ["-O0", "-O1", "-Os", "-Oz", "-O2", "-O3"] {
        let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
            .args([level, "a.c", "-o", "prog"])
            .current_dir(&dir)
            .output()
            .expect("the compiler is built before its own tests run");
        assert!(out.status.success(), "{level}: {}", String::from_utf8_lossy(&out.stderr));
        let ran = Command::new(dir.join("prog")).output().expect("what was linked can be run");
        assert!(ran.status.success(), "{level}: {}", ran.status);
        answers.push((level, String::from_utf8_lossy(&ran.stdout).into_owned()));
    }
    let _ = std::fs::remove_dir_all(&dir);
    for (level, answer) in &answers[1..] {
        assert_eq!(answer, &answers[0].1, "{level} gave another answer than -O0");
    }
}
