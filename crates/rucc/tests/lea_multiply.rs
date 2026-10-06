//! A multiply by 3, 5 or 9 is one `lea` on x86-64, and by twice, four or eight times one of those
//! is that `lea` and a shift, as gcc writes them, where rucc wrote an `imul`.
//!
//! tamnd/rucc#1994. Postgres' `pfree` finds the methods of a memory context in a table of
//! structures of 72 bytes, so every call multiplied the index by 72, which is three cycles on the
//! way to the load of the function it calls.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-lea-multiply-{}-{what}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir.canonicalize().expect("the directory is there")
}

/// Whether the compiler finished, and what it said.
fn run(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("the compiler is built before its own tests run");
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

/// A function for each multiplier, one that indexes a table of 72 byte structures the way
/// `pfree` does, and a `main` that prints what each gives for values that wrap.
const PROGRAM: &str = r#"
int printf(const char *, ...);

#define TIMES(k) __attribute__((noinline)) unsigned long times##k(unsigned long x) { return x * k; }
TIMES(3) TIMES(5) TIMES(9) TIMES(6) TIMES(10) TIMES(18) TIMES(12) TIMES(20) TIMES(36)
TIMES(24) TIMES(40) TIMES(72)

struct methods {
  void (*alloc)(void);
  long sizes[7];
  int id;
};

__attribute__((noinline)) int id_of(const struct methods *table, long i) { return table[i].id; }

int main(void) {
  unsigned long (*each[])(unsigned long) = {times3, times5, times9, times6, times10, times18,
                                            times12, times20, times36, times24, times40, times72};
  unsigned long xs[] = {0, 1, 7, -1UL, 0x8000000000000001UL, 0x123456789abcdefUL};
  for (int f = 0; f < 12; f++) {
    unsigned long sum = 0;
    for (int i = 0; i < 6; i++)
      sum = sum * 31 + each[f](xs[i]);
    printf("%lu %lu %lu\n", each[f](7), each[f](-1UL), sum);
  }
  struct methods table[5];
  for (int i = 0; i < 5; i++)
    table[i].id = i * i + 1;
  printf("%d %d %d\n", id_of(table, 0), id_of(table, 3), id_of(table + 4, -2));
  return 0;
}
"#;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn a_multiply_by_a_constant_gives_the_same_answers() {
    // What each line prints, worked out with unsigned arithmetic that wraps at sixty four bits.
    const WANT: &str = "\
21 18446744073709551613 9469328624507629877\n\
35 18446744073709551611 9633299682942865923\n\
63 18446744073709551607 9961241799813338015\n\
42 18446744073709551610 491913175305708138\n\
70 18446744073709551606 819855292176180230\n\
126 18446744073709551598 1475739525917124414\n\
84 18446744073709551604 983826350611416276\n\
140 18446744073709551596 1639710584352360460\n\
252 18446744073709551580 2951479051834248828\n\
168 18446744073709551592 1967652701222832552\n\
280 18446744073709551576 3279421168704720920\n\
504 18446744073709551544 5902958103668497656\n\
1 10 5\n";
    let dir = dir("run");
    std::fs::write(dir.join("a.c"), PROGRAM).expect("the fixture can be written");
    for level in ["-O0", "-O1", "-O2", "-O3", "-Os"] {
        let (ok, said) = run(&dir, &[level, "a.c", "-o", "prog"]);
        assert!(ok, "{level}: {said}");
        let out = Command::new(dir.join("prog")).output().expect("what was linked can be run");
        assert!(out.status.success(), "{level}: the program failed");
        assert_eq!(String::from_utf8_lossy(&out.stdout), WANT, "{level}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// The lines of one function in an assembly listing, trimmed, up to its `.size`.
fn body<'a>(asm: &'a str, name: &str) -> Vec<&'a str> {
    let start = asm.find(&format!("\n{name}:")).expect("the function is there");
    let end = asm[start..].find(&format!(".size\t{name}")).map_or(asm.len(), |at| start + at);
    asm[start..end].lines().map(str::trim).filter(|line| !line.is_empty()).collect()
}

/// At `-O2` none of the multipliers is an `imul`, each is one `lea`, and the ones that are a
/// power of two times 3, 5 or 9 have one shift after it. An element of an array of 72 byte
/// structures is read with the shift as the scale of the load.
#[test]
fn a_multiply_by_three_five_or_nine_is_a_lea() {
    let dir = dir("asm");
    std::fs::write(dir.join("a.c"), PROGRAM).expect("the fixture can be written");
    let (ok, said) =
        run(&dir, &["--target=x86_64-unknown-linux-gnu", "-O2", "-S", "a.c", "-o", "a.s"]);
    assert!(ok, "{said}");
    let asm = std::fs::read_to_string(dir.join("a.s")).expect("a.s was written");
    for k in [3, 5, 9, 6, 10, 18, 12, 20, 36, 24, 40, 72] {
        let name = format!("times{k}");
        let lines = body(&asm, &name);
        assert!(!lines.iter().any(|line| line.starts_with("imul")), "{name}: {lines:#?}");
        let leas = lines.iter().filter(|line| line.starts_with("lea")).count();
        assert_eq!(leas, 1, "{name}: {lines:#?}");
        let shifts = lines.iter().filter(|line| line.starts_with("sal") || line.starts_with("shl"));
        assert_eq!(shifts.count(), usize::from(![3, 5, 9].contains(&k)), "{name}: {lines:#?}");
    }
    // The element's address is the base and the `lea` scaled by eight, inside the load.
    let lines = body(&asm, "id_of");
    let unwanted = ["imul", "sal", "shl", "add"];
    assert!(
        !lines.iter().any(|line| unwanted.iter().any(|it| line.starts_with(it))),
        "id_of: {lines:#?}"
    );
    assert!(lines.iter().any(|line| line.starts_with("movl\t64(%rdi,")), "id_of: {lines:#?}");
    let _ = std::fs::remove_dir_all(&dir);
}
