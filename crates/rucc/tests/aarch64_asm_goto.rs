//! An `asm goto` on AArch64, which is how the arm64 kernel asks whether the machine has a feature.
//!
//! `alternative_has_cap_likely` is a `b` to a label in a template the kernel patches into a no-op
//! at boot when the feature is there, so every test of a CPU feature in the kernel is one of these.
//! The template is kept as text and each label in it is the block that arm of the statement goes
//! to, named once the layout has numbered the blocks, the same as on x86-64.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const SOURCE: &str = "\
static inline __attribute__((always_inline)) int clear(int c) {
  asm goto(\"cbz %w0, %l[no]\\n.pushsection .data\\n.hword %1\\n.popsection\" : : \"r\"(c), \"i\"(7) : : no);
  return 1;
no:
  return 0;
}
static inline __attribute__((always_inline)) int has(void) {
  asm goto(\"b %l1\" : : \"i\"(3) : : yes);
  return 0;
yes:
  return 1;
}
int f(int x, int c) { if (clear(c)) return x + 1; return x - 1; }
int g(int x) { if (has()) return x * 3; return x; }
";

fn compile(flags: &[&str]) -> std::process::Output {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rucc-a64-goto-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=aarch64-unknown-linux-gnu", "-fno-pic", "-o", "-"])
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    out
}

/// The lines of one function, labels included, without its directives.
fn body(text: &str, name: &str) -> Vec<String> {
    text.lines()
        .skip_while(|line| *line != format!("{name}:"))
        .skip(1)
        .take_while(|line| !line.starts_with("\t.size"))
        .filter(|line| !line.starts_with("\t.cfi"))
        .map(|line| line.trim().to_string())
        .collect()
}

#[test]
fn a_label_is_the_block_its_arm_goes_to() {
    let text = String::from_utf8(compile(&["-O2", "-S"]).stdout).expect("a listing is text");
    assert_eq!(
        body(&text, "f"),
        [
            ".Lf_0:",
            "cbz w1, .Lf_2",
            ".pushsection .data",
            ".hword 7",
            ".popsection",
            ".Lf_1:",
            "add w0, w0, #1",
            "ret",
            ".Lf_2:",
            "sub w0, w0, #1",
            "ret",
        ],
        "{text}"
    );
    let g = body(&text, "g");
    assert_eq!(g[..2], [".Lg_0:", "b .Lg_2"], "{text}");
    let yes = g.iter().position(|line| line == ".Lg_2:").expect("the label is written");
    assert_eq!(g[yes + 1], "add w0, w0, w0, lsl #1", "{text}");
}

/// The integrated assembler takes the jumps, at every level.
#[test]
fn the_object_assembles() {
    for level in ["-O0", "-O2"] {
        let out = compile(&[level, "-c"]);
        assert!(!out.stdout.is_empty(), "{level}");
    }
}
