//! `-ffixed-x18` on AArch64, as the FreeBSD kernel builds with it: `x18` holds the per-CPU
//! pointer there, so no instruction the compiler writes may read or write it.
//!
//! The flags are the AArch64 lines of FreeBSD 15.1's sys/conf/kern.mk. The source keeps thirty
//! values live at once, and aarch64-linux-gnu-gcc 14 at -O2 puts one of them in `x18` without the
//! flag and none with it, so the code wants every register there is.

use std::io::Write as _;
use std::process::{Command, Stdio};

/// The FreeBSD kernel's flags for AArch64.
const KERNEL: [&str; 6] = [
    "--target=aarch64-unknown-freebsd15.1",
    "-ffreestanding",
    "-mgeneral-regs-only",
    "-ffixed-x18",
    "-mbranch-protection=standard",
    "-mno-outline-atomics",
];

/// The listing of the source on standard input, with these flags added to the kernel's.
fn listing(flags: &[&str], source: &str) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .env("LC_ALL", "C")
        .args(KERNEL)
        .args(flags)
        .args(["-S", "-o", "-", "-x", "c", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the compiler is built before its own tests run");
    let mut stdin = child.stdin.take().expect("a pipe to write the input into");
    stdin.write_all(source.as_bytes()).expect("the input can be written");
    drop(stdin);
    let out = child.wait_with_output().expect("the compiler finished");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success() && err.is_empty(), "{flags:?}: {err}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The lines of a listing that name `x18` or `w18`.
fn uses(text: &str) -> Vec<&str> {
    let named = |word: &str| matches!(word, "x18" | "w18");
    text.lines()
        .filter(|line| line.starts_with('\t') && !line.starts_with("\t."))
        .filter(|line| line.split(|c: char| !c.is_ascii_alphanumeric()).any(named))
        .collect()
}

const HEAVY: &str = "\
void heavy(long *p, long *q) {
    long a0 = p[0], a1 = p[1], a2 = p[2], a3 = p[3], a4 = p[4], a5 = p[5], a6 = p[6];
    long a7 = p[7], a8 = p[8], a9 = p[9], a10 = p[10], a11 = p[11], a12 = p[12];
    long a13 = p[13], a14 = p[14], a15 = p[15], a16 = p[16], a17 = p[17], a18 = p[18];
    long a19 = p[19], a20 = p[20], a21 = p[21], a22 = p[22], a23 = p[23], a24 = p[24];
    long a25 = p[25], a26 = p[26], a27 = p[27], a28 = p[28], a29 = p[29];
    __asm__ volatile(\"\" ::: \"memory\");
    q[0] = a0 * a29; q[1] = a1 * a28; q[2] = a2 * a27; q[3] = a3 * a26; q[4] = a4 * a25;
    q[5] = a5 * a24; q[6] = a6 * a23; q[7] = a7 * a22; q[8] = a8 * a21; q[9] = a9 * a20;
    q[10] = a10 * a19; q[11] = a11 * a18; q[12] = a12 * a17; q[13] = a13 * a16;
    q[14] = a14 * a15;
    q[15] = a0 + a1 + a2 + a3 + a4 + a5 + a6 + a7 + a8 + a9 + a10 + a11 + a12 + a13 + a14;
    q[16] = a15 + a16 + a17 + a18 + a19 + a20 + a21 + a22 + a23 + a24 + a25 + a26 + a27
        + a28 + a29;
}
";

/// Code that wants a scratch register for something other than a value: a frame too large for
/// one immediate, a loop of exclusive loads and stores, a variadic function, 128 bit arithmetic
/// and values that live across calls.
const SCRATCH: &str = "\
long g(long);
long big(int i) { volatile char buf[70000]; buf[i] = 1; return buf[i + 1]; }
long bump(long *p, long v) { return __atomic_fetch_add(p, v, __ATOMIC_SEQ_CST); }
int swap(int *p, int old, int new) {
    return __atomic_compare_exchange_n(p, &old, new, 0, __ATOMIC_ACQ_REL, __ATOMIC_ACQUIRE);
}
long sum(int n, ...) {
    __builtin_va_list ap; long s = 0;
    __builtin_va_start(ap, n);
    for (int i = 0; i < n; i++) s += __builtin_va_arg(ap, long);
    __builtin_va_end(ap);
    return s;
}
unsigned __int128 mul(unsigned __int128 a, unsigned __int128 b) { return a * b / (b | 1); }
long across(long a, long b, long c, long d, long e, long f, long h, long i) {
    long x = g(a), y = g(b), z = g(c), w = g(d);
    return x * a + y * b + z * c + w * d + g(e) * f + g(h) * i + g(x + y + z + w);
}
";

#[test]
fn x18_is_never_used_under_the_freebsd_kernel_flags() {
    for opt in ["-O0", "-O1", "-O2", "-Os"] {
        for (name, source) in [("heavy", HEAVY), ("scratch", SCRATCH)] {
            let text = listing(&[opt], source);
            assert!(uses(&text).is_empty(), "{name} {opt}: {:?}\n{text}", uses(&text));
        }
    }
}

#[test]
fn the_check_would_see_x18_if_it_were_there() {
    // The kernel reads its per-CPU data through `x18` in inline `asm`, and `uses` finds that line.
    let source =
        "long pcpu(void) { long v; __asm__(\"ldr %0, [x18, #8]\" : \"=r\"(v)); return v; }";
    let text = listing(&["-O2"], source);
    assert_eq!(uses(&text).len(), 1, "{text}");
}
