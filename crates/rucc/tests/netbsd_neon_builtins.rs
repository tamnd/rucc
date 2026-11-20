//! The AArch64 Advanced SIMD builtins of gcc that NetBSD's `<crypto/arch/arm/arm_neon.h>` calls.
//!
//! The kernel builds with `-nostdinc`, so it cannot use the compiler's `<arm_neon.h>`, and the
//! header writes each intrinsic as a call to the gcc builtin behind it when the compiler says it is
//! gcc. Three units of the AES and ChaCha code under `sys/crypto` stopped on these names in the
//! build of the aarch64 GENERIC kernel with rucc. Each name here is checked lane by lane against
//! the same operation written in plain C, in the forms the header calls them in: a pointer cast to
//! `__builtin_aarch64_simd_si`, a vector cast to the type the builtin takes, and a count of
//! `uint8_t` passed to an inline function.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-netbsd-neon-{}-{what}", std::process::id()));
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
/// that disagrees, or zero. gcc 12 and later compile it too, at `-O2`, where the counts are
/// constants once the inline functions are in.
const PROGRAM: &str = r"
typedef __Int32x4_t int32x4_t;
typedef __Uint32x4_t uint32x4_t;
typedef __Int8x16_t int8x16_t;
typedef __Uint8x16_t uint8x16_t;
typedef unsigned char u8;
typedef unsigned int u32;

/* The forms of NetBSD's <crypto/arch/arm/arm_neon.h>. */
#define INL static inline __attribute__((__always_inline__))
INL uint32x4_t ld32(const u32 *p32) {
    const __builtin_aarch64_simd_si *p = (const __builtin_aarch64_simd_si *)p32;
    return (uint32x4_t)__builtin_aarch64_ld1v4si(p);
}
INL uint8x16_t ld8(const u8 *p8) {
    const __builtin_aarch64_simd_qi *p = (const __builtin_aarch64_simd_qi *)p8;
    return (uint8x16_t)__builtin_aarch64_ld1v16qi(p);
}
INL void st32(u32 *p32, uint32x4_t v) {
    __builtin_aarch64_simd_si *p = (__builtin_aarch64_simd_si *)p32;
    __builtin_aarch64_st1v4si(p, (int32x4_t)v);
}
INL void st8(u8 *p8, uint8x16_t v) {
    __builtin_aarch64_simd_qi *p = (__builtin_aarch64_simd_qi *)p8;
    __builtin_aarch64_st1v16qi(p, (int8x16_t)v);
}
INL int32x4_t shl_s32(int32x4_t v, u8 bits) { return (int32x4_t)__builtin_aarch64_ashlv4si(v, bits); }
INL uint32x4_t shl_u32(uint32x4_t v, u8 bits) { return (uint32x4_t)__builtin_aarch64_ashlv4si((int32x4_t)v, bits); }
INL uint32x4_t shr_u32(uint32x4_t v, u8 bits) { return __builtin_aarch64_lshrv4si_uus(v, bits); }
INL uint32x4_t shr_u32_old(uint32x4_t v, u8 bits) { return (uint32x4_t)__builtin_aarch64_lshrv4si((int32x4_t)v, bits); }
INL uint8x16_t shr_u8(uint8x16_t v, u8 bits) { return __builtin_aarch64_lshrv16qi_uus(v, bits); }
INL uint8x16_t shr_u8_old(uint8x16_t v, u8 bits) { return (uint8x16_t)__builtin_aarch64_lshrv16qi((int8x16_t)v, bits); }
INL int32x4_t sli_s32(int32x4_t vins, int32x4_t vsh, u8 bits) { return (int32x4_t)__builtin_aarch64_ssli_nv4si(vins, vsh, bits); }
INL uint32x4_t sri_u32(uint32x4_t vins, uint32x4_t vsh, u8 bits) { return __builtin_aarch64_usri_nv4si_uuus(vins, vsh, bits); }

static int bad;
static void same(const void *a, const void *b, int line) {
    const u8 *x = a, *y = b;
    for (int i = 0; i < 16; i++)
        if (x[i] != y[i]) { bad = bad ? bad : line; return; }
}
#define SAME(a, b) do { __typeof__(a) a_ = (a), b_ = (b); same(&a_, &b_, __LINE__); } while (0)

static union { uint32x4_t v; u32 w[4]; u8 b[16]; } a, b, want;

#define LEFT32(n) do { \
    for (int i = 0; i < 4; i++) want.w[i] = a.w[i] << n; \
    SAME(shl_u32(a.v, n), want.v); \
    SAME((uint32x4_t)shl_s32((int32x4_t)a.v, n), want.v); \
    for (int i = 0; i < 4; i++) want.w[i] = n ? (b.w[i] << n) | (a.w[i] & ((1u << n) - 1)) : b.w[i]; \
    SAME((uint32x4_t)sli_s32((int32x4_t)a.v, (int32x4_t)b.v, n), want.v); \
} while (0)
#define RIGHT32(n) do { \
    for (int i = 0; i < 4; i++) want.w[i] = n < 32 ? a.w[i] >> n : 0; \
    SAME(shr_u32(a.v, n), want.v); \
    SAME(shr_u32_old(a.v, n), want.v); \
    for (int i = 0; i < 4; i++) want.w[i] = n < 32 ? (b.w[i] >> n) | (a.w[i] & ~(0xffffffffu >> n)) : a.w[i]; \
    SAME(sri_u32(a.v, b.v, n), want.v); \
} while (0)
#define RIGHT8(n) do { \
    for (int i = 0; i < 16; i++) want.b[i] = n < 8 ? a.b[i] >> n : 0; \
    SAME(shr_u8((uint8x16_t)a.v, n), (uint8x16_t)want.v); \
    SAME(shr_u8_old((uint8x16_t)a.v, n), (uint8x16_t)want.v); \
} while (0)

int main(void) {
    for (int i = 0; i < 16; i++) { a.b[i] = (u8)(0x91 + 17 * i); b.b[i] = (u8)(0x3c ^ (29 * i)); }

    /* ld1 and st1, at addresses aligned only to a lane, through pointers of other types. */
    u32 words[9];
    u8 bytes[20];
    for (int i = 0; i < 9; i++) words[i] = 0x01010101u * (u32)i;
    for (int i = 0; i < 20; i++) bytes[i] = (u8)(7 * i);
    for (int i = 0; i < 4; i++) want.w[i] = words[1 + i];
    SAME(ld32(words + 1), want.v);
    for (int i = 0; i < 16; i++) want.b[i] = bytes[3 + i];
    SAME(ld8(bytes + 3), (uint8x16_t)want.v);
    st32(words + 1, a.v);
    if (words[0] != 0 || words[1] != a.w[0] || words[4] != a.w[3] || words[5] != 0x05050505u) return __LINE__;
    st8(bytes + 3, (uint8x16_t)b.v);
    if (bytes[2] != 14 || bytes[3] != b.b[0] || bytes[18] != b.b[15] || bytes[19] != 133) return __LINE__;

    /* Shifts, by each end of the range and inside it. */
    LEFT32(0); LEFT32(1); LEFT32(7); LEFT32(16); LEFT32(31);
    RIGHT32(1); RIGHT32(7); RIGHT32(16); RIGHT32(31); RIGHT32(32);
    RIGHT8(1); RIGHT8(3); RIGHT8(7); RIGHT8(8);

    /* The same with a constant count written in the call. */
    for (int i = 0; i < 4; i++) want.w[i] = a.w[i] >> 32 - 1 >> 1;
    SAME(__builtin_aarch64_lshrv4si_uus(a.v, 32), want.v);
    for (int i = 0; i < 4; i++) want.w[i] = (b.w[i] << 5) | (a.w[i] & 31);
    SAME((uint32x4_t)__builtin_aarch64_ssli_nv4si((int32x4_t)a.v, (int32x4_t)b.v, 5), want.v);
    for (int i = 0; i < 4; i++) want.w[i] = (b.w[i] >> 9) | (a.w[i] & 0xff800000u);
    SAME(__builtin_aarch64_usri_nv4si_uuus(a.v, b.v, 9), want.v);
    return bad;
}
";

#[cfg(all(target_os = "linux", target_arch = "aarch64"))]
#[test]
fn netbsd_neon_builtins_give_the_lanes_of_the_instructions() {
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

/// The same program compiles to an object for AArch64 on any host, and for the NetBSD target the
/// kernel is built for.
#[test]
fn netbsd_neon_builtins_compile_for_aarch64_anywhere() {
    let dir = dir("object");
    std::fs::write(dir.join("a.c"), PROGRAM).expect("the fixture can be written");
    for target in ["--target=aarch64-unknown-linux-gnu", "--target=aarch64-unknown-netbsd"] {
        for level in ["-O0", "-O2"] {
            let (ok, said) = run(&dir, &[target, level, "-c", "a.c", "-o", "a.o"]);
            assert!(ok, "{target} {level}: {said}");
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Calls gcc refuses, and a count that rucc would have to read twice and that is not a name, a
/// constant or a cast of one.
const WRONG: &str = r"
typedef __Int32x4_t v4si;
typedef __Uint32x4_t v4su;
typedef int v2si __attribute__((vector_size(8)));
int g(void);
v4si a(v4si v) { return __builtin_aarch64_ashlv4si(v, 32); }
v4su b(v4su v) { return __builtin_aarch64_lshrv4si_uus(v, 0); }
v4si c(v4si v, v4si w) { return __builtin_aarch64_ssli_nv4si(v, w, g()); }
v4si d(v2si v) { return __builtin_aarch64_ashlv4si(v, 1); }
v4si e(int x) { return __builtin_aarch64_ld1v4si(x); }
void f(int *p) { __builtin_aarch64_st1v4si(p); }
v4si h(v4si v, v4si w) { return __builtin_aarch64_ashlv4si(v, w); }
";

#[test]
fn netbsd_neon_builtins_refuse_what_they_cannot_answer() {
    let dir = dir("wrong");
    std::fs::write(dir.join("a.c"), WRONG).expect("the fixture can be written");
    let (ok, said) = run(&dir, &["--target=aarch64-unknown-linux-gnu", "-fsyntax-only", "a.c"]);
    assert!(!ok, "{said}");
    for line in [
        "a.c:6:55: error: the shift count of '__builtin_aarch64_ashlv4si' is 32, and it must be from 0 to 31 [E0715]",
        "a.c:7:59: error: the shift count of '__builtin_aarch64_lshrv4si_uus' is 0, and it must be from 1 to 32 [E0715]",
        "a.c:8:33: error: argument 3 of '__builtin_aarch64_ssli_nv4si' must be a name, a constant, or a cast of one when it is not a constant [E0715]",
        "a.c:9:52: error: incompatible type for argument 1 of '__builtin_aarch64_ashlv4si' [E0715]",
        "a.c:10:50: error: incompatible type for argument 1 of '__builtin_aarch64_ld1v4si' [E0715]",
        "a.c:11:18: error: too few arguments to function '__builtin_aarch64_st1v4si' [E0511]",
        "a.c:12:63: error: incompatible type for argument 2 of '__builtin_aarch64_ashlv4si' [E0715]",
    ] {
        assert!(said.contains(line), "{line}\n{said}");
    }
    // On a target that is not AArch64 the names are not builtins, as in gcc.
    let (ok, said) = run(&dir, &["--target=x86_64-unknown-linux-gnu", "-fsyntax-only", "a.c"]);
    assert!(!ok && said.contains("unknown type name '__Int32x4_t'"), "{said}");
    std::fs::write(dir.join("b.c"), "int f(void) { return __builtin_aarch64_ld1v4si(0)[0]; }\n")
        .expect("the fixture can be written");
    let (ok, said) = run(&dir, &["--target=x86_64-unknown-linux-gnu", "-fsyntax-only", "b.c"]);
    assert!(
        !ok && said.contains("implicit declaration of function '__builtin_aarch64_ld1v4si'"),
        "{said}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
