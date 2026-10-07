//! An operation on floating constants is a constant, as it is with gcc.
//!
//! The kernel writes `usleep_range(DELAY, 1.5 * DELAY)` and `v->rangelow = 87.5 * 16000`, and
//! builds everything with `-mgeneral-regs-only` on arm64, where a `double` the compiler keeps
//! anywhere is a refused build. gcc folds each of those to an integer before any of it reaches a
//! register. Before the fold pass did the same, 33 units of the arm64 allmodconfig were refused.
//!
//! The folded answer has to be the one the machine gives, bit for bit, so the second test computes
//! each shape twice, once from constants and once from the same numbers read through `volatile`,
//! and runs the result.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

/// The shapes from the kernel, cut down.
const KERNEL: &str = "\
void usleep_range(unsigned long min, unsigned long max);
#define LP8720_ENABLE_DELAY 200
void lp872x(void) { usleep_range(LP8720_ENABLE_DELAY, 1.5 * LP8720_ENABLE_DELAY); }
struct v4l2_tuner { unsigned rangelow, rangehigh; };
#define FREQ_MIN 87.5
#define FREQ_MAX 108.0
#define FREQ_MUL 16000
void dsbr100(struct v4l2_tuner *v) {
    v->rangelow = FREQ_MIN * FREQ_MUL;
    v->rangehigh = FREQ_MAX * FREQ_MUL;
}
#define RTW89_DB_INVERT_TABLE_OFFSET (-41.25 * 4)
int rtw89(int db) { return db + (int)RTW89_DB_INVERT_TABLE_OFFSET; }
unsigned long hsr(unsigned long t) { return t + (unsigned long)(1.5 * 100); }
";

fn fixture(name: &str, source: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rucc-float-const-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join(format!("{name}.c"));
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

fn rucc(args: &[&str], path: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(args)
        .arg(path)
        .output()
        .expect("the compiler is built before its own tests run")
}

#[test]
fn the_kernel_shapes_need_no_vector_register() {
    let path = fixture("kernel", KERNEL);
    for level in ["-O1", "-O2", "-Os", "-O3"] {
        let out = rucc(
            &["--target=aarch64-unknown-linux-gnu", level, "-mgeneral-regs-only", "-S", "-o", "-"],
            &path,
        );
        let said = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{level}: {said}");
        let text = String::from_utf8_lossy(&out.stdout);
        // 1400000 is a `mov` and a `movk`, so the three that fit one instruction are asked about.
        for (number, what) in
            [("#300", "1.5 * 200"), ("#-165", "-41.25 * 4"), ("#150", "1.5 * 100")]
        {
            assert!(text.contains(number), "{level}: {what} is not {number}:\n{text}");
        }
        let floating = ["scvtf", "ucvtf", "fmul", "fcvtz", "fmov"];
        assert!(
            !text.lines().any(|line| floating.iter().any(|op| line.trim_start().starts_with(op))),
            "{level}:\n{text}"
        );
    }
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
}

/// Each line is a shape computed from constants and the same shape computed from `volatile`
/// copies of them, compared as bits, so that a NaN and the sign of a zero count.
const SAME: &str = r#"
#include <stdio.h>
#include <string.h>
#include <float.h>
static int bad;
int main(void) {
    volatile double one = 1.0, three = 3.0, tenth = 0.1, fifth = 0.2, zero = 0.0, big = DBL_MAX;
    volatile double tiny = 4.9406564584124654e-324, nearly = 1e-310;
    volatile float onef = 1.0f, threef = 3.0f, tenthf = 0.1f, bigf = FLT_MAX;
    volatile long most = 0x7fffffffffffffffL;
    volatile unsigned long all = ~0UL;
    volatile int odd = 16777217;
    #define D(expr, run) do { double f_ = (expr), r_ = (run); \
        if (memcmp(&f_, &r_, 8)) { printf("%s\n", #expr); bad++; } } while (0)
    #define F(expr, run) do { float f_ = (expr), r_ = (run); \
        if (memcmp(&f_, &r_, 4)) { printf("%s\n", #expr); bad++; } } while (0)
    D(0.1 + 0.2, tenth + fifth);
    D(1.0 / 3.0, one / three);
    D(0.1 - 0.2, tenth - fifth);
    D(0.1 * 0.2, tenth * fifth);
    D(DBL_MAX * 2.0, big * 2.0);
    D(1.0 / 0.0, one / zero);
    D(-1.0 / 0.0, -one / zero);
    D(0.0 / 0.0 == 0.0 / 0.0 ? 1.0 : 2.0, zero / zero == zero / zero ? 1.0 : 2.0);
    D(4.9406564584124654e-324 / 2.0, tiny / 2.0);
    D(1e-310 * 0.5, nearly * 0.5);
    D(-0.0 + 0.0, -zero + zero);
    D(0.0 - 0.0, zero - zero);
    D(-0.0 * 3.0, -zero * three);
    D((double)0x7fffffffffffffffL, (double)most);
    D((double)~0UL, (double)all);
    D((double)0.1f, (double)tenthf);
    F(0.1f * 3.0f, tenthf * threef);
    F(1.0f / 3.0f, onef / threef);
    F(FLT_MAX * 2.0f, bigf * 2.0f);
    F((float)0.1, (float)tenth);
    F((float)1e300, (float)(big / 1e8));
    F((float)16777217, (float)odd);
    F((float)~0UL, (float)all);
    printf("%d\n", bad);
    return bad;
}
"#;

/// Every folded answer is the one the machine gives, at every level that folds.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn every_folded_answer_is_the_one_the_machine_gives() {
    let path = fixture("same", SAME);
    let prog = path.with_extension("");
    for flags in
        [&["-O2"][..], &["-O1"], &["-O2", "-fno-trapping-math"], &["-O2", "-frounding-math"]]
    {
        let mut args = flags.to_vec();
        args.extend(["-o", prog.to_str().expect("a temporary path is text")]);
        let built = rucc(&args, &path);
        assert!(built.status.success(), "{flags:?}: {}", String::from_utf8_lossy(&built.stderr));
        let ran = Command::new(&prog).output().expect("what was linked can be run");
        assert_eq!(
            ran.status.code(),
            Some(0),
            "{flags:?}: {}",
            String::from_utf8_lossy(&ran.stdout)
        );
    }
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
}

/// Under `-frounding-math` an inexact answer is left to the rounding mode the program set.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn under_rounding_math_the_mode_the_program_set_decides() {
    let source = "\
#include <fenv.h>
#include <string.h>
__attribute__((noipa)) double third(void) { return 1.0 / 3.0; }
int main(void) {
    double near = third();
    fesetround(FE_UPWARD);
    double up = third();
    fesetround(FE_TONEAREST);
    return near < up ? 0 : 1;
}
";
    let path = fixture("mode", source);
    let prog = path.with_extension("");
    let built = rucc(
        &["-O2", "-frounding-math", "-o", prog.to_str().expect("a temporary path is text")],
        &path,
    );
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let ran = Command::new(&prog).output().expect("what was linked can be run");
    assert_eq!(ran.status.code(), Some(0), "1.0 / 3.0 was folded under -frounding-math");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
}
