//! An `asm goto` with an output, inside a loop, whose label leaves the loop.
//!
//! The arm64 kernel's `get_user` is one of these, and spidev's ioctl reads its arguments with it in
//! a loop. The output is written by the instruction that ends the block, so it can be named in the
//! blocks that instruction branches to and not as an argument on its own edges. Loop
//! canonicalization passed it to the exit through the label edge anyway, which the verifier refused
//! as an internal error. It showed only when nothing initialized the stack, since with
//! `-ftrivial-auto-var-init=pattern` the output is written before the loop as well.

use std::path::Path;
use std::process::Command;

/// The value read before the label is taken, used after the loop through the label.
const LEFT_BY_LABEL: &str = r#"
int f(int *p, int n) {
    unsigned long v;
    for (int i = 0; i < n; i++)
        asm goto("1: ldtr %w0, [%1]" : "=r"(v) : "r"(p + i) : : bad);
    return 0;
bad:
    return (int)v;
}
"#;

/// The same with the label inside the loop body and the value read on both ways out.
const LABEL_IN_BODY: &str = r#"
int f(int *p, int n) {
    int s = 0;
    for (int i = 0; i < n; i++) {
        unsigned long v;
        asm goto("1: ldtr %w0, [%1]" : "=r"(v) : "r"(p + i) : : bad);
        s += (int)v;
        continue;
    bad:
        return s + (int)v;
    }
    return s;
}
"#;

/// A program whose exit status says whether the value came out of the loop right: the sum of the
/// first three, then the sum of the first three times ten plus the negative fourth that took the
/// label.
const RUNS: &str = r#"
__attribute__((noinline)) int f(int *p, int n) {
    int s = 0;
    for (int i = 0; i < n; i++) {
        long v;
        asm goto("ldrsw %0, [%1]\n\ttbnz %0, #63, %l2" : "=r"(v) : "r"(p + i) : "cc" : bad);
        s += (int)v;
        continue;
    bad:
        return s * 10 + (int)v;
    }
    return s;
}
int main(void) {
    int a[] = {1, 2, 3, -4, 5};
    return f(a, 3) == 6 && f(a, 5) == 56 ? 0 : 1;
}
"#;

fn compile(dir: &Path, source: &str, args: &[&str]) -> std::process::Output {
    std::fs::create_dir_all(dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(args)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run")
}

#[test]
fn an_output_leaving_a_loop_by_its_label_compiles() {
    for (name, source) in [("left", LEFT_BY_LABEL), ("body", LABEL_IN_BODY)] {
        for level in ["-O1", "-O2", "-Os", "-O3"] {
            let dir = std::env::temp_dir()
                .join(format!("rucc-goto-loop-{}-{name}{level}", std::process::id()));
            let object = dir.join("one.o");
            let object = object.to_str().expect("a temporary path is text");
            let args = ["--target=aarch64-unknown-linux-gnu", level, "-c", "-o", object];
            let out = compile(&dir, source, &args);
            let _ = std::fs::remove_dir_all(&dir);
            assert!(
                out.status.success(),
                "{name} {level}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
}

#[cfg(target_arch = "aarch64")]
#[test]
fn an_output_leaving_a_loop_by_its_label_has_the_value_it_read() {
    for level in ["-O0", "-O1", "-O2", "-O3"] {
        let dir = std::env::temp_dir().join(format!("rucc-goto-run-{}{level}", std::process::id()));
        let program = dir.join("one");
        let program_text = program.to_str().expect("a temporary path is text");
        let out = compile(&dir, RUNS, &[level, "-o", program_text]);
        assert!(out.status.success(), "{level}: {}", String::from_utf8_lossy(&out.stderr));
        let status = Command::new(&program).status().expect("the program runs");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(status.success(), "{level}: {status}");
    }
}
