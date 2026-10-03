//! Loops that count the bits of a value, which `-O2` turns into one count where the processor the
//! function is built for has the instruction, and leaves alone where it does not. The loops are
//! the ones gcc's `number_of_iterations_popcount` and its neighbours take, written the way C
//! programs write them.

use std::process::Command;

const SOURCE: &str = "\
int pop(unsigned x) { int n = 0; while (x) { x &= x - 1; n++; } return n; }
int popl(unsigned long x) { int n = 0; while (x) { x &= x - 1; n++; } return n; }
int bits(unsigned x) { int n = 0; while (x) { x >>= 1; n++; } return n; }
int ctz(unsigned x) { int n = 0; if (!x) return 32; while (!(x & 1)) { x >>= 1; n++; } return n; }
int clz(unsigned x) { int n = 0; if (!x) return 32; while (!(x & 0x80000000u)) { x <<= 1; n++; } return n; }
";

/// What the compiler writes for [`SOURCE`] at `-O2` with those flags, one function at a time.
fn listing(flags: &[&str]) -> Vec<(String, String)> {
    let dir = std::env::temp_dir().join(format!(
        "rucc-bit-loops-{}-{}",
        std::process::id(),
        flags.join("")
    ));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-unknown-linux-gnu", "-O2", "-fno-asynchronous-unwind-tables", "-S"])
        .args(flags)
        .args(["-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8(out.stdout).expect("a listing is text");
    let mut functions: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        if let Some(name) =
            line.strip_suffix(':').filter(|name| SOURCE.contains(&format!(" {name}(")))
        {
            functions.push((name.to_owned(), String::new()));
        } else if let Some((_, body)) = functions.last_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    assert_eq!(functions.len(), 5, "{text}");
    functions
}

/// Whether a function body jumps back to a label above it, which is what a loop is in a listing.
///
/// A jump back to a `ret` is not one. It is where a branch that skips the count joins the end.
fn loops(body: &str) -> bool {
    let lines: Vec<&str> = body.lines().collect();
    let mut seen = Vec::new();
    for (at, line) in lines.iter().enumerate() {
        if let Some(label) = line.strip_suffix(':') {
            seen.push((label, at));
            continue;
        }
        if !line.trim_start().starts_with('j') {
            continue;
        }
        let Some(to) = line.split_whitespace().nth(1) else { continue };
        let Some(&(_, from)) = seen.iter().find(|(label, _)| *label == to) else { continue };
        let first = lines[from..].iter().find(|line| !line.ends_with(':'));
        if !first.is_some_and(|line| line.trim_start().starts_with("ret")) {
            return true;
        }
    }
    false
}

/// With all three counts each loop is one instruction and nothing goes round.
#[test]
fn every_loop_is_one_count_where_the_processor_has_all_three() {
    for (name, body) in listing(&["-march=x86-64-v3"]) {
        let wanted = match name.as_str() {
            "pop" | "popl" => "popcnt",
            "bits" | "clz" => "lzcnt",
            _ => "tzcnt",
        };
        assert!(body.contains(wanted), "{name} is {wanted}: {body}");
        assert!(!loops(&body), "{name} does not go round: {body}");
    }
}

/// With only `popcnt` the set bit counts go and the others stay loops, since a leading or trailing
/// zero count with no instruction behind it is longer than the loop it would replace.
#[test]
fn only_the_counts_the_processor_has_take_a_loop_out() {
    for (name, body) in listing(&["-mpopcnt"]) {
        let counted = matches!(name.as_str(), "pop" | "popl");
        assert_eq!(body.contains("popcnt"), counted, "{name}: {body}");
        assert_eq!(loops(&body), !counted, "{name}: {body}");
    }
}

/// On a plain x86-64 every loop stays, which is what gcc 16 does too.
#[test]
fn a_processor_with_no_count_keeps_every_loop() {
    for (name, body) in listing(&[]) {
        assert!(!body.contains("popcnt") && !body.contains("lzcnt") && !body.contains("tzcnt"));
        assert!(loops(&body), "{name}: {body}");
    }
}

/// The counts answer what the loops did, for every value with one bit set, every value with one
/// bit clear, every run of low ones and a spread of others, zero among them.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn the_counts_answer_what_the_loops_did() {
    let check = "\
int pop(unsigned x); int popl(unsigned long x); int bits(unsigned x); int ctz(unsigned x); int clz(unsigned x);
static int ones(unsigned long x) { int n = 0; for (int i = 0; i < 64; i++) n += (x >> i) & 1; return n; }
static int width(unsigned x) { int n = 0; for (int i = 0; i < 32; i++) if ((x >> i) & 1) n = i + 1; return n; }
static int low(unsigned x) { for (int i = 0; i < 32; i++) if ((x >> i) & 1) return i; return 32; }
static int high(unsigned x) { for (int i = 31; i >= 0; i--) if ((x >> i) & 1) return 31 - i; return 32; }
static int same(unsigned long x) {
  unsigned u = (unsigned)x;
  return pop(u) == ones(u) && popl(x) == ones(x) && bits(u) == width(u) && ctz(u) == low(u) && clz(u) == high(u);
}
int main(void) {
  unsigned long seed = 88172645463325252ul;
  for (int i = 0; i < 64; i++)
    if (!same(1ul << i) || !same((1ul << i) - 1) || !same(~(1ul << i))) return 1;
  for (int i = 0; i < 100000; i++) {
    seed ^= seed << 13; seed ^= seed >> 7; seed ^= seed << 17;
    if (!same(seed >> (i & 63))) return 2;
  }
  return !same(0) || !same(~0ul);
}
";
    let dir = std::env::temp_dir().join(format!("rucc-bit-loops-run-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    std::fs::write(dir.join("loops.c"), SOURCE).expect("the fixture can be written");
    std::fs::write(dir.join("check.c"), check).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .current_dir(&dir)
        .args(["-O2", "-mpopcnt", "loops.c", "check.c", "-o", "check"])
        .output()
        .expect("the compiler is built before its own tests run");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let ran = Command::new(dir.join("check")).status().expect("what was linked can be run");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(ran.code(), Some(0), "a count disagreed with the loop it replaced");
}
