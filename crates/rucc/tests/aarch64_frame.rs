//! How an AArch64 prologue saves the general purpose registers a call preserves.
//!
//! The frame code was written for x86-64 first, where a push is a word. On this machine a push is
//! sixteen bytes because the stack pointer may never be off that alignment, so a register pushed
//! alone wastes half of its push, which shows in the listing as a `str` where gcc writes an `stp`.

use std::path::PathBuf;
use std::process::Command;

const TARGETS: [(&str, &str); 2] =
    [("aarch64-unknown-linux-gnu", "both"), ("aarch64-apple-darwin", "_both")];

/// Twelve integers and twelve doubles, every one of them read after two calls, which is more than
/// either file has registers a call leaves alone.
const PRESSURE: &str = "\
double g(double);
long h(long);
double both(long *p, double *q) {
  long a0 = p[0], a1 = p[1], a2 = p[2], a3 = p[3], a4 = p[4], a5 = p[5];
  long a6 = p[6], a7 = p[7], a8 = p[8], a9 = p[9], a10 = p[10], a11 = p[11];
  double b0 = q[0], b1 = q[1], b2 = q[2], b3 = q[3], b4 = q[4], b5 = q[5];
  double b6 = q[6], b7 = q[7], b8 = q[8], b9 = q[9], b10 = q[10], b11 = q[11];
  double r = g(b0 * b1);
  long s = h(a0 + a1);
  return r + s + a0 * a1 + a2 * a3 + a4 * a5 + a6 * a7 + a8 * a9 + a10 * a11
      + b0 * b1 + b2 * b3 + b4 * b5 + b6 * b7 + b8 * b9 + b10 * b11 + a2 + a3 + b2 + b3;
}
";

/// The fixture, under a directory of its own so that two of these running at once do not write
/// the same file.
fn fixture(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-a64-frame-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, PRESSURE).expect("the fixture can be written");
    path
}

/// The assembly of the fixture for one target, from its function's label to its `ret`.
fn listing(target: &str, label: &str) -> Vec<String> {
    let path = fixture(target);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={target}"))
        .args(["-O2", "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8(out.stdout).expect("assembly is text");
    text.lines()
        .skip_while(|line| *line != format!("{label}:"))
        .map(|line| line.trim().to_owned())
        .take_while(|line| line != "ret")
        .collect()
}

/// `x19` to `x28` go on two to a push, the way gcc and clang save them, and come back the same way.
#[test]
fn the_preserved_general_purpose_registers_go_on_in_pairs() {
    for (target, label) in TARGETS {
        let asm = listing(target, label);
        let text = asm.join("\n");
        assert!(asm.iter().any(|line| line == "stp x19, x20, [sp, #-16]!"), "{target}\n{text}");
        assert!(asm.iter().any(|line| line == "ldp x19, x20, [sp], #16"), "{target}\n{text}");
        assert!(
            !asm.iter().any(|line| line.starts_with("str x2") && line.ends_with("[sp, #-16]!")),
            "{target}: a register pushed alone where it had a partner\n{text}"
        );
        // The first of the pair is at the lower address and the second a word above it, and the
        // unwind table has to say the same or a C++ exception puts back the wrong values.
        let at = |reg: u32| {
            let prefix = format!(".cfi_offset {reg}, ");
            asm.iter()
                .find_map(|line| line.strip_prefix(&prefix)?.parse::<i32>().ok())
                .unwrap_or_else(|| panic!("{target}: no row for x{reg}\n{text}"))
        };
        assert_eq!(at(20), at(19) + 8, "{target}\n{text}");
    }
}
