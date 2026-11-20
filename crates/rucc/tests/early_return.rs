//! Which `return` the front end marks as written inside a test, which the branch predictor reads
//! as gcc's early return guess.
//!
//! Design: section 11.2 of `spec/optimizer/11-profile-and-frequency.md`, and tamnd/rucc#3182.
//!
//! gcc marks a `return` as not taken when it is inside an arm of an `if` or of a `?:`, or on the
//! right of `&&` or `||`. The cases of a `switch` and the body of a loop are not inside a test in
//! that sense, so a `return` there is not marked, and neither is the one at the end of a function.

use std::process::Command;

/// The IR for `source`, for x86-64 Linux at `-O0`, where nothing has moved a `return` yet.
fn ir(what: &str, source: &str) -> String {
    let dir = std::env::temp_dir().join(format!("rucc-early-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-unknown-linux-gnu", "-O0", "--emit=ir", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("the IR is text")
}

/// How many returns in one function are marked and how many are not.
fn returns(text: &str, name: &str) -> (usize, usize) {
    let open = format!("func @{name}(");
    let body: Vec<&str> = text
        .lines()
        .map(str::trim)
        .skip_while(|line| !line.starts_with(&open))
        .skip(1)
        .take_while(|line| *line != "}")
        .collect();
    let early = body.iter().filter(|line| line.starts_with("return.early")).count();
    let plain =
        body.iter().filter(|line| **line == "return" || line.starts_with("return ")).count();
    (early, plain)
}

#[test]
fn a_return_inside_an_if_is_marked_and_the_one_after_it_is_not() {
    let text = ir("if", "int f(int m) { if (m) return 1; return 2; }\n");
    assert_eq!(returns(&text, "f"), (1, 1), "{text}");
}

#[test]
fn both_arms_of_an_if_else_are_marked() {
    let text = ir("else", "int f(int m) { if (m) return 1; else return 2; }\n");
    assert_eq!(returns(&text, "f"), (2, 0), "{text}");
}

#[test]
fn a_return_from_a_void_function_inside_an_if_is_marked() {
    let text = ir("void", "void g(void);\nvoid f(int m) { if (m) return; g(); }\n");
    assert_eq!(returns(&text, "f"), (1, 1), "{text}");
}

#[test]
fn a_return_in_a_case_or_a_loop_body_is_not_marked() {
    let text = ir(
        "case",
        "int f(int m) { switch (m) { case 0: return 1; case 1: return 2; } return 3; }\n\
         int g(int n) { while (n) return n; return 0; }\n\
         int h(int n) { for (;;) { if (n) return n; } }\n",
    );
    assert_eq!(returns(&text, "f"), (0, 3), "{text}");
    assert_eq!(returns(&text, "g"), (0, 2), "{text}");
    // The `if` inside the loop is a test, so the `return` in it is marked.
    assert_eq!(returns(&text, "h"), (1, 0), "{text}");
}

#[test]
fn a_return_in_a_statement_expression_inside_a_test_is_marked() {
    let text = ir(
        "expr",
        "int f(int m) { int x = m ? ({ if (m > 9) return 7; m; }) : 0; return x; }\n\
         int g(int m) { int x = m && ({ return 5; 1; }); return x; }\n\
         int k(int m) { ({ return m; }); }\n",
    );
    assert_eq!(returns(&text, "f"), (1, 1), "{text}");
    assert_eq!(returns(&text, "g"), (1, 1), "{text}");
    assert_eq!(returns(&text, "k"), (0, 1), "{text}");
}
