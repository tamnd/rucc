//! `-mfpmath=387` on i386, which is how NetBSD builds its 32 bit compat libraries and FreeBSD its
//! lib32 and boot code. Every `float` and `double` operation is done on the x87 stack, so the
//! assembly has no SSE register and no SSE instruction in it, at `-O0` and at `-O2`.

use std::process::Command;

const SOURCE: &str = "\
typedef struct { float f; double d; } pair;
float add(float a, float b) { return a + b; }
double mix(float a, double b, int c, long long d) { return a * b - c / b + d; }
int less(double a, double b) { return a < b; }
double pick(double a, double b) { return a < b ? a : b; }
float neg(float a) { return -a; }
unsigned to_unsigned(float a) { return a; }
double from_unsigned(unsigned a) { return a; }
long long to_wide(double a) { return a; }
unsigned bits(float a) { union { float f; unsigned u; } x; x.f = a; return x.u; }
pair make(float f, double d) { pair p = { f * 2, d / 3 }; return p; }
double sum(const float *p, int n) { double s = 0.25; for (int i = 0; i < n; i++) s += p[i]; return s; }
double call(double x) { return add((float)x, 1.5f) + mix(2, x, 3, 4); }
";

fn build(level: &str) -> std::process::Output {
    let dir =
        std::env::temp_dir().join(format!("rucc-i386-x87-floats-{}{level}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=i686-unknown-linux-gnu", "-mfpmath=387", "-S", "-o", "-", level])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    out
}

#[test]
fn every_float_operation_is_on_the_x87_stack_with_no_sse_instruction() {
    for level in ["-O0", "-O2"] {
        let out = build(level);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{level}: {stderr}");
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines().map(str::trim).filter(|line| !line.starts_with('.')) {
            let op = line.split_whitespace().next().unwrap_or_default();
            // The scalar and packed SSE forms end in `ss`, `sd`, `ps` or `pd`, and no integer
            // instruction does. An x87 one can, as `fstps` does, and each of those starts with f.
            let sse = line.contains("%xmm")
                || (!op.starts_with('f')
                    && ["ss", "sd", "ps", "pd"].iter().any(|it| op.ends_with(it)))
                || ["cvt", "ucomis", "comis"].iter().any(|it| op.starts_with(it))
                || op == "movd"
                || op == "movq";
            assert!(!sse, "{level}: an SSE instruction: {line}\n{text}");
        }
        for wanted in ["flds\t", "fldl\t", "fstps\t", "fstpl\t", "fucomip\t", "fildl\t"] {
            assert!(text.contains(wanted), "{level}: no {wanted}\n{text}");
        }
    }
}
