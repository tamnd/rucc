//! How an AArch64 prologue saves the registers a call preserves.
//!
//! The frame code was written for x86-64 first, where a push is a word. On this machine a push is
//! sixteen bytes because the stack pointer may never be off that alignment, so a register pushed
//! alone wastes half of its push, which shows in the listing as a `str` where gcc writes an `stp`.
//!
//! And a call keeps only the low eight bytes of `v8` to `v15`, so that is all a prologue owes its
//! caller of each, and a value wider than that cannot be left in one over a call.

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

/// Four quad `long double`s, each read after a call, which is what a register gcc's callee gives
/// back only the bottom of cannot hold.
const QUADS: &str = "\
void g(void);
long double quads(long double a, long double b, long double c, long double d) {
  g();
  a = a + b;
  g();
  b = b + c;
  g();
  return a * b + c * d;
}
";

/// The fixture, under a directory of its own so that two of these running at once do not write
/// the same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-a64-frame-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// The assembly of the fixture for one target, from its function's label to its `ret`.
fn listing(target: &str, label: &str) -> Vec<String> {
    listing_of(target, label, PRESSURE, "pressure")
}

/// The assembly of some source for one target, from a function's label to its `ret`.
fn listing_of(target: &str, label: &str, source: &str, what: &str) -> Vec<String> {
    let path = fixture(&format!("{target}-{what}"), source);
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

/// `v8` to `v15` are saved as `d8` to `d15`, eight bytes each, since that is all of them a caller
/// may count on getting back, and the unwind table says where each went.
#[test]
fn the_preserved_vector_registers_are_saved_as_their_low_half() {
    for (target, label) in TARGETS {
        let asm = listing(target, label);
        let text = asm.join("\n");
        let stored = |reg: &str| asm.iter().any(|line| line.starts_with(&format!("str {reg}, [")));
        let loaded = |reg: &str| asm.iter().any(|line| line.starts_with(&format!("ldr {reg}, [")));
        assert!(stored("d8") && loaded("d8"), "{target}: d8 is not saved\n{text}");
        // A spill is still as wide as the register, so this is only about the saved ones.
        for n in 8..16 {
            let whole = [format!("str q{n}, ["), format!("ldr q{n}, [")];
            assert!(
                !asm.iter().any(|line| whole.iter().any(|one| line.starts_with(one.as_str()))),
                "{target}: all of v{n} is saved\n{text}"
            );
        }
        // DWARF numbers the vector registers from 64, so `d8` is 72 and `d9` is 73, and the two
        // are a double apart.
        let at = |reg: u32| {
            let prefix = format!(".cfi_offset {reg}, ");
            asm.iter()
                .find_map(|line| line.strip_prefix(&prefix)?.parse::<i32>().ok())
                .unwrap_or_else(|| panic!("{target}: no row for d{}\n{text}", reg - 64))
        };
        assert_eq!(at(73).abs_diff(at(72)), 8, "{target}\n{text}");
    }
}

/// A quad `long double` live over a call is not left in `v8` to `v15`, whose top half the callee
/// is free to destroy, so the function saves none of them and keeps the values in memory instead.
#[test]
fn a_quad_long_double_is_not_kept_in_a_register_a_call_keeps_half_of() {
    let target = "aarch64-unknown-linux-gnu";
    let asm = listing_of(target, "quads", QUADS, "quads");
    let text = asm.join("\n");
    for n in 8..16 {
        let names = [format!("v{n}."), format!("q{n},"), format!("d{n},")];
        assert!(
            !asm.iter().any(|line| names.iter().any(|name| line.contains(name.as_str()))),
            "{target}: v{n} holds something over a call\n{text}"
        );
    }
}
