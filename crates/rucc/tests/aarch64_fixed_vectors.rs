//! `-ffixed-` naming a vector register on AArch64, which keeps it out of the allocator. The
//! kernel's AEGIS code loads its S-box into `v16` to `v31` with one `asm` statement and reads it
//! with later ones, and builds the unit with `-ffixed-q16` to `-ffixed-q31` so that nothing in
//! between puts a value there.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Two dozen values live at once, more than the vector registers below `v16` that a call does
/// not keep.
const SOURCE: &str = "\
double many(volatile double *a)
{
    double s0 = a[0];
    double s1 = a[1];
    double s2 = a[2];
    double s3 = a[3];
    double s4 = a[4];
    double s5 = a[5];
    double s6 = a[6];
    double s7 = a[7];
    double s8 = a[8];
    double s9 = a[9];
    double s10 = a[10];
    double s11 = a[11];
    double s12 = a[12];
    double s13 = a[13];
    double s14 = a[14];
    double s15 = a[15];
    double s16 = a[16];
    double s17 = a[17];
    double s18 = a[18];
    double s19 = a[19];
    double s20 = a[20];
    double s21 = a[21];
    double s22 = a[22];
    double s23 = a[23];
    return s0 * s23 + s1 * s22 + s2 * s21 + s3 * s20 + s4 * s19 + s5 * s18
        + s6 * s17 + s7 * s16 + s8 * s15 + s9 * s14 + s10 * s13 + s11 * s12;
}
\
int printf(const char *, ...);
int main(void)
{
    static volatile double in[24];
    for (int i = 0; i < 24; i++)
        in[i] = i + 1;
    long out;
    asm volatile(\"fmov d20, %0\" : : \"r\"(1234L));
    double sum = many(in);
    asm volatile(\"fmov %0, d20\" : \"=r\"(out));
    printf(\"%.0f %ld\\n\", sum, out);
    return 0;
}
";

fn fixed() -> Vec<String> {
    (16..32).map(|n| format!("-ffixed-q{n}")).collect()
}

fn compile(
    target: &str,
    flags: &[String],
    out: &str,
) -> (std::process::Output, std::path::PathBuf) {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rucc-a64-fixed-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let output = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={target}"))
        .args(["-O2", "-o"])
        .arg(dir.join(out))
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    (output, dir)
}

/// The listing of `many` for Linux, under those flags.
fn many(flags: &[String]) -> String {
    let flags = [flags, &["-S".to_string()]].concat();
    let (out, dir) = compile("aarch64-unknown-linux-gnu", &flags, "one.s");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = std::fs::read_to_string(dir.join("one.s")).expect("a listing was written");
    let _ = std::fs::remove_dir_all(&dir);
    text.lines()
        .skip_while(|line| *line != "many:")
        .take_while(|line| !line.starts_with("\t.size"))
        .map(|line| format!("{line}\n"))
        .collect()
}

/// Whether a line names one of `v16` to `v31` in any width.
fn high(line: &str) -> bool {
    line.split(|c: char| !c.is_ascii_alphanumeric()).any(|word| {
        let mut chars = word.chars();
        matches!(chars.next(), Some('b' | 'h' | 's' | 'd' | 'q' | 'v'))
            && chars.as_str().parse::<u8>().is_ok_and(|n| (16..32).contains(&n))
    })
}

#[test]
fn a_fixed_vector_register_is_never_given_to_a_value() {
    let text = many(&[]);
    assert!(text.lines().any(high), "without the flags the high registers are used:\n{text}");
    let text = many(&fixed());
    assert!(!text.lines().any(high), "{text}");
    // Any spelling of the register keeps it back.
    let spellings: Vec<String> =
        (16..32).map(|n| format!("-ffixed-{}{n}", ["v", "d", "s", "b"][n % 4])).collect();
    assert!(!many(&spellings).lines().any(high));
}

#[test]
fn a_register_that_is_not_there_is_an_unknown_option() {
    for flag in ["-ffixed-q32", "-ffixed-q016", "-ffixed-w16"] {
        let (out, dir) = compile("aarch64-unknown-linux-gnu", &[flag.to_string()], "one.o");
        let _ = std::fs::remove_dir_all(&dir);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("unknown option"), "{flag}: {err}");
    }
    let (out, dir) = compile("x86_64-unknown-linux-gnu", &fixed()[..1], "one.o");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(String::from_utf8_lossy(&out.stderr).contains("unknown option"));
}

#[test]
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn what_an_asm_left_in_a_fixed_register_is_still_there() {
    let (out, dir) = compile("aarch64-apple-darwin", &fixed(), "one");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let run = Command::new(dir.join("one")).output().expect("the program runs");
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(String::from_utf8_lossy(&run.stdout), "1300 1234\n");
}
