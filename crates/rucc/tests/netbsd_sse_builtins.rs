//! The x86 SSE builtins of gcc that NetBSD's `<crypto/arch/x86/immintrin.h>` calls.
//!
//! The kernel builds with `-nostdinc`, so it cannot use the compiler's `<immintrin.h>`, and the
//! header writes each intrinsic as a call to the gcc builtin behind it when the compiler says it is
//! gcc. Seven units of the AES and ChaCha code under `sys/crypto` stopped on these names in the
//! first build of the GENERIC kernel with rucc. Each name here is checked lane by lane against the
//! same operation written in plain C, in the forms the header calls them in: a parameter cast to
//! the type the builtin takes, a count of `uint8_t`, and an immediate in a macro.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-netbsd-sse-{}-{what}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir.canonicalize().expect("the directory is there")
}

/// Whether the compiler finished, and what it said.
fn run(dir: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .env("LC_ALL", "C")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("the compiler is built before its own tests run");
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

/// Each builtin against the lanes plain C gives. `main` gives back the line of the first check
/// that disagrees, or zero.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const PROGRAM: &str = r"
typedef char v16qi __attribute__((vector_size(16)));
typedef short v8hi __attribute__((vector_size(16)));
typedef int v4si __attribute__((vector_size(16)));
typedef long long v2di __attribute__((vector_size(16)));
typedef float v4sf __attribute__((vector_size(16)));
typedef long long m128i __attribute__((vector_size(16), may_alias));
typedef unsigned char u8;
typedef unsigned short u16;
typedef unsigned int u32;
typedef unsigned long long u64;

/* The forms of NetBSD's <crypto/arch/x86/immintrin.h>: a parameter cast to the type the builtin takes. */
static inline m128i slli32(m128i v, u8 bits) { return (m128i)__builtin_ia32_pslldi128((v4si)v, (int)bits); }
static inline m128i srli32(m128i v, u8 bits) { return (m128i)__builtin_ia32_psrldi128((v4si)v, (int)bits); }
static inline m128i slli64(m128i v, u8 bits) { return (m128i)__builtin_ia32_psllqi128((v2di)v, (int)bits); }
static inline m128i srli64(m128i v, u8 bits) { return (m128i)__builtin_ia32_psrlqi128((v2di)v, (int)bits); }
static inline m128i slli16(m128i v, u8 bits) { return (m128i)__builtin_ia32_psllwi128((v8hi)v, (int)bits); }
static inline m128i srli16(m128i v, u8 bits) { return (m128i)__builtin_ia32_psrlwi128((v8hi)v, (int)bits); }
static inline m128i shuffle8(m128i v, m128i m) { return (m128i)__builtin_ia32_pshufb128((v16qi)v, (v16qi)m); }
#define shuffle32(v, m) ((m128i)__builtin_ia32_pshufd((v4si)(m128i)(v), (int)(m)))
#define shuffleps(x, y, m) ((v4sf)__builtin_ia32_shufps((v4sf)(x), (v4sf)(y), (int)(m)))
#define slli128(v, n) ((m128i)__builtin_ia32_pslldqi128((v2di)(m128i)(v), 8 * (int)(n)))
#define srli128(v, n) ((m128i)__builtin_ia32_psrldqi128((m128i)(v), 8 * (int)(n)))
#define alignr8(hi, lo, n) ((m128i)__builtin_ia32_palignr128((v2di)(m128i)(hi), (v2di)(m128i)(lo), 8 * (int)(n)))

static int bad;
static void same(const void *a, const void *b, int line) {
    const u8 *x = a, *y = b;
    for (int i = 0; i < 16; i++)
        if (x[i] != y[i]) { bad = bad ? bad : line; return; }
}
#define SAME(a, b) do { __typeof__(a) a_ = (a), b_ = (b); same(&a_, &b_, __LINE__); } while (0)

static volatile u8 counts[] = {0, 1, 7, 15, 16, 31, 32, 33, 63, 64, 200};

int main(void) {
    union { m128i v; u8 b[16]; u16 h[8]; u32 w[4]; u64 q[2]; float f[4]; } a, b, m, r, want;
    for (int i = 0; i < 16; i++) { a.b[i] = (u8)(0x91 + 17 * i); b.b[i] = (u8)(0x10 + i); }
    v4sf fa = {1, 2, 3, 4}, fb = {5, 6, 7, 8};

    /* Unpacks and moves of halves. */
    v4si ia = (v4si)a.v, ib = (v4si)b.v;
    SAME(__builtin_ia32_punpckldq128(ia, ib), ((v4si){ia[0], ib[0], ia[1], ib[1]}));
    SAME(__builtin_ia32_punpckhdq128(ia, ib), ((v4si){ia[2], ib[2], ia[3], ib[3]}));
    SAME(__builtin_ia32_punpcklqdq128((v2di)a.v, (v2di)b.v), ((v2di){(long long)a.q[0], (long long)b.q[0]}));
    SAME(__builtin_ia32_punpckhqdq128((v2di)a.v, (v2di)b.v), ((v2di){(long long)a.q[1], (long long)b.q[1]}));
    SAME(__builtin_ia32_movhlps(fa, fb), ((v4sf){7, 8, 3, 4}));
    SAME(__builtin_ia32_movlhps(fa, fb), ((v4sf){1, 2, 5, 6}));
    SAME(__builtin_ia32_unpcklps(fa, fb), ((v4sf){1, 5, 2, 6}));
    SAME(__builtin_ia32_unpckhps(fa, fb), ((v4sf){3, 7, 4, 8}));
    for (int i = 0; i < 8; i++) { want.b[2 * i] = a.b[i]; want.b[2 * i + 1] = b.b[i]; }
    r.v = (m128i)__builtin_ia32_punpcklbw128((v16qi)a.v, (v16qi)b.v); SAME(r.v, want.v);
    for (int i = 0; i < 8; i++) { want.b[2 * i] = a.b[8 + i]; want.b[2 * i + 1] = b.b[8 + i]; }
    r.v = (m128i)__builtin_ia32_punpckhbw128((v16qi)a.v, (v16qi)b.v); SAME(r.v, want.v);
    for (int i = 0; i < 4; i++) { want.h[2 * i] = a.h[i]; want.h[2 * i + 1] = b.h[i]; }
    r.v = (m128i)__builtin_ia32_punpcklwd128((v8hi)a.v, (v8hi)b.v); SAME(r.v, want.v);
    for (int i = 0; i < 4; i++) { want.h[2 * i] = a.h[4 + i]; want.h[2 * i + 1] = b.h[4 + i]; }
    r.v = (m128i)__builtin_ia32_punpckhwd128((v8hi)a.v, (v8hi)b.v); SAME(r.v, want.v);

    /* Shifts of each lane, by a count that is not a constant, and by constants. */
    for (unsigned k = 0; k < sizeof counts; k++) {
        u8 n = counts[k];
        for (int i = 0; i < 4; i++) want.w[i] = n < 32 ? a.w[i] << n : 0;
        r.v = slli32(a.v, n); SAME(r.v, want.v);
        for (int i = 0; i < 4; i++) want.w[i] = n < 32 ? a.w[i] >> n : 0;
        r.v = srli32(a.v, n); SAME(r.v, want.v);
        for (int i = 0; i < 2; i++) want.q[i] = n < 64 ? a.q[i] << n : 0;
        r.v = slli64(a.v, n); SAME(r.v, want.v);
        for (int i = 0; i < 2; i++) want.q[i] = n < 64 ? a.q[i] >> n : 0;
        r.v = srli64(a.v, n); SAME(r.v, want.v);
        for (int i = 0; i < 8; i++) want.h[i] = n < 16 ? (u16)(a.h[i] << n) : 0;
        r.v = slli16(a.v, n); SAME(r.v, want.v);
        for (int i = 0; i < 8; i++) want.h[i] = n < 16 ? (u16)(a.h[i] >> n) : 0;
        r.v = srli16(a.v, n); SAME(r.v, want.v);
    }
    for (int i = 0; i < 4; i++) want.w[i] = a.w[i] >> 7;
    r.v = (m128i)__builtin_ia32_psrldi128((v4si)a.v, 7); SAME(r.v, want.v);
    for (int i = 0; i < 4; i++) want.w[i] = 0;
    r.v = (m128i)__builtin_ia32_pslldi128((v4si)a.v, 40); SAME(r.v, want.v);

    /* pshufb: the low four bits name a byte, and a set top bit gives zero. */
    for (int i = 0; i < 16; i++) m.b[i] = (u8)(i % 3 == 0 ? 0x80 | i : 31 - 2 * i);
    for (int i = 0; i < 16; i++) want.b[i] = m.b[i] & 0x80 ? 0 : a.b[m.b[i] & 15];
    r.v = shuffle8(a.v, m.v); SAME(r.v, want.v);

    /* The immediates. */
    r.v = shuffle32(a.v, 0x1b);
    for (int i = 0; i < 4; i++) want.w[i] = a.w[3 - i];
    SAME(r.v, want.v);
    r.v = shuffle32(a.v, 0x4e);
    want.w[0] = a.w[2]; want.w[1] = a.w[3]; want.w[2] = a.w[0]; want.w[3] = a.w[1];
    SAME(r.v, want.v);
    SAME(shuffleps(fa, fb, 0xb1), ((v4sf){2, 1, 8, 7}));
    for (int n = 0; n <= 17; n += 1) {
        for (int i = 0; i < 16; i++) want.b[i] = i < n ? 0 : a.b[i - n];
        switch (n) {
        case 0: r.v = slli128(a.v, 0); break;
        case 4: r.v = slli128(a.v, 4); break;
        case 8: r.v = slli128(a.v, 8); break;
        case 15: r.v = slli128(a.v, 15); break;
        case 16: r.v = slli128(a.v, 16); break;
        default: continue;
        }
        SAME(r.v, want.v);
        for (int i = 0; i < 16; i++) want.b[i] = i + n < 16 ? a.b[i + n] : 0;
        switch (n) {
        case 0: r.v = srli128(a.v, 0); break;
        case 4: r.v = srli128(a.v, 4); break;
        case 8: r.v = srli128(a.v, 8); break;
        case 15: r.v = srli128(a.v, 15); break;
        case 16: r.v = srli128(a.v, 16); break;
        }
        SAME(r.v, want.v);
    }
    u8 both[48] = {0};
    for (int i = 0; i < 16; i++) { both[i] = b.b[i]; both[16 + i] = a.b[i]; }
    for (int i = 0; i < 16; i++) want.b[i] = both[i + 4];
    r.v = alignr8(a.v, b.v, 4); SAME(r.v, want.v);
    for (int i = 0; i < 16; i++) want.b[i] = both[i + 12];
    r.v = alignr8(a.v, b.v, 12); SAME(r.v, want.v);
    r.v = alignr8(a.v, b.v, 16); SAME(r.v, a.v);
    for (int i = 0; i < 16; i++) want.b[i] = both[i + 20];
    r.v = alignr8(a.v, b.v, 20); SAME(r.v, want.v);
    for (int i = 0; i < 16; i++) want.b[i] = 0;
    r.v = alignr8(a.v, b.v, 32); SAME(r.v, want.v);
    return bad;
}
";

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn netbsd_sse_builtins_give_the_lanes_of_the_instructions() {
    let dir = dir("run");
    std::fs::write(dir.join("a.c"), PROGRAM).expect("the fixture can be written");
    for level in ["-O0", "-O1", "-O2"] {
        let (ok, said) = run(&dir, &[level, "a.c", "-o", "prog"]);
        assert!(ok, "{level}: {said}");
        let out = Command::new(dir.join("prog")).output().expect("what was linked can be run");
        assert_eq!(out.status.code(), Some(0), "{level}: the check on this line disagrees");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Calls gcc refuses, and a count and an index vector that rucc would have to read twice and
/// that are not a name, a constant or a cast of one.
const WRONG: &str = r"
typedef int v4si __attribute__((vector_size(16)));
typedef int v2si __attribute__((vector_size(8)));
int g(void);
v4si a(v4si v) { return __builtin_ia32_pslldi128(v, g()); }
v4si b(v4si v) { return __builtin_ia32_punpckldq128(v); }
v4si c(v2si v) { return __builtin_ia32_punpckldq128(v, v); }
v4si d(v4si v, int n) { return __builtin_ia32_pshufd(v, n); }
v4si e(v4si v, v4si w) { return __builtin_ia32_pslldi128(v, w); }
v4si f(v4si v, v4si *m) { return (v4si)__builtin_ia32_pshufb128((__attribute__((vector_size(16))) char)v, *m); }
";

#[test]
fn netbsd_sse_builtins_refuse_what_they_cannot_answer() {
    let dir = dir("wrong");
    std::fs::write(dir.join("a.c"), WRONG).expect("the fixture can be written");
    let (ok, said) = run(&dir, &["--target=x86_64-unknown-linux-gnu", "-fsyntax-only", "a.c"]);
    assert!(!ok, "{said}");
    for line in [
        "a.c:5:25: error: argument 2 of '__builtin_ia32_pslldi128' must be a name, a constant, or a cast of one when it is not a constant [E0715]",
        "a.c:6:25: error: too few arguments to function '__builtin_ia32_punpckldq128' [E0511]",
        "a.c:7:53: error: incompatible type for argument 1 of '__builtin_ia32_punpckldq128' [E0715]",
        "a.c:8:32: error: the last argument of '__builtin_ia32_pshufd' must be an 8-bit immediate [E0715]",
        "a.c:9:61: error: incompatible type for argument 2 of '__builtin_ia32_pslldi128' [E0715]",
        "a.c:10:40: error: argument 2 of '__builtin_ia32_pshufb128' must be a name, a constant, or a cast of one when it is not a constant [E0715]",
    ] {
        assert!(said.contains(line), "{line}\n{said}");
    }
    // On a target that is not x86 the names are not builtins, as in gcc.
    let (ok, said) = run(&dir, &["--target=aarch64-unknown-linux-gnu", "-fsyntax-only", "a.c"]);
    assert!(
        !ok && said.contains("implicit declaration of function '__builtin_ia32_pslldi128'"),
        "{said}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
