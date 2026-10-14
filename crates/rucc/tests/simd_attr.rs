//! `__attribute__((simd))`, end to end: the vector versions under the vector function ABI's
//! names, the lanes each one takes, and what is refused and warned about on the way.
//!
//! A test of the whole compiler rather than of one crate, because the attribute is read and the
//! versions are made in the checker, built in the lowering and called from code that only knows
//! the names. The assembly is read for the names gcc 16 gives, and on an x86-64 Linux host the
//! versions are also called, each lane checked against a call of the function itself.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The target is written down rather than taken from the host, since only x86-64 ELF is built.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-simd-{}-{what}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir.canonicalize().expect("the directory is there")
}

/// Whether the compiler finished, what it wrote on its standard output and what it said.
fn run(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("the compiler is built before its own tests run");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The assembly of one source for a target at an optimization level, and what was said.
fn assembly_at(what: &str, target: &str, level: &str, source: &str) -> (bool, String, String) {
    let dir = dir(what);
    std::fs::write(dir.join("a.c"), source).expect("the fixture can be written");
    let target = format!("--target={target}");
    let said = run(&dir, &[&target, level, "-S", "-o", "-", "a.c"]);
    let _ = std::fs::remove_dir_all(&dir);
    said
}

/// The same at `-O2`.
fn assembly(what: &str, target: &str, source: &str) -> (bool, String, String) {
    assembly_at(what, target, "-O2", source)
}

/// The vector versions an assembly listing defines, in the order it defines them.
fn versions(out: &str) -> Vec<&str> {
    out.lines()
        .map(str::trim)
        .filter_map(|line| line.strip_suffix(':'))
        .filter(|label| label.starts_with("_ZGV"))
        .collect()
}

#[test]
fn the_sse2_versions_are_built_under_the_vector_abi_names() {
    for level in ["-O0", "-O2"] {
        let (ok, out, err) = assembly_at(
            "names",
            TARGET,
            level,
            "__attribute__((simd)) double h(double x) { return x * 2; }\n\
             __attribute__((simd(\"notinbranch\"))) float g(float x) { return x + 1; }\n\
             __attribute__((simd(\"inbranch\"))) int k(int);\n\
             __attribute__((__simd__(\"notinbranch\"))) int k(int x) { return x - 1; }\n\
             __attribute__((simd(\"inbranch\"))) int r(int x) __asm__(\"rr\");\n\
             int r(int x) { return x; }\n\
             __attribute__((simd(\"notinbranch\"), weak, visibility(\"hidden\")))\n\
             int w(int x) { return x; }\n",
        );
        assert!(ok, "{level}: {err}");
        assert_eq!(err, "", "{level}");
        assert_eq!(
            versions(&out),
            [
                "_ZGVbN2v_h",
                "_ZGVbM2v_h",
                "_ZGVbN4v_g",
                "_ZGVbN4v_k",
                "_ZGVbM4v_k",
                "_ZGVbM4v_rr",
                "_ZGVbN4v_w"
            ],
            "{level}:\n{out}"
        );
        for line in [".globl\t_ZGVbN2v_h", ".globl\t_ZGVbM4v_rr", ".weak\t_ZGVbN4v_w"] {
            assert!(out.contains(line), "{level}: {line} missing:\n{out}");
        }
        assert!(out.contains(".hidden\t_ZGVbN4v_w"), "{level}:\n{out}");
    }
}

#[test]
fn the_lanes_follow_the_characteristic_type() {
    let (ok, out, err) = assembly(
        "lanes",
        TARGET,
        "enum E { A, B };\n\
         __attribute__((simd(\"notinbranch\"))) char c(char a, long b) { return a + b; }\n\
         __attribute__((simd(\"notinbranch\"))) void v(double d) { (void)d; }\n\
         __attribute__((simd(\"notinbranch\"))) void w(void) { }\n\
         __attribute__((simd(\"notinbranch\"))) _Bool b(enum E e, char *p, float f)\n\
         { return e || p || f; }\n\
         __attribute__((simd(\"notinbranch\"))) int va(int a, ...) { return a; }\n\
         __attribute__((simd(\"notinbranch\"))) double q(short s) { return s; }\n",
    );
    assert!(ok, "{err}");
    assert_eq!(err, "");
    assert_eq!(
        versions(&out),
        ["_ZGVbN16vv_c", "_ZGVbN2v_v", "_ZGVbN4_w", "_ZGVbN16vvv_b", "_ZGVbN4v_va", "_ZGVbN2v_q"],
        "{out}"
    );
}

#[test]
fn only_an_emitted_external_definition_gets_versions() {
    for source in [
        "__attribute__((simd)) double h(double);\n",
        "static __attribute__((simd)) int s(int x) { return x; }\nint t(int x) { return s(x); }\n",
        "__attribute__((simd)) inline int i(int x) { return x; }\n",
        "__attribute__((simd, target_clones(\"avx2\", \"default\"))) int f(int x) { return x; }\n",
        "void outer(void) { __attribute__((simd)) int n(int x) { return x; } (void)n; }\n",
    ] {
        let (ok, out, err) = assembly("none", TARGET, source);
        assert!(ok, "{source}: {err}");
        assert_eq!(err, "", "{source}");
        assert!(versions(&out).is_empty(), "{source}:\n{out}");
    }
}

#[test]
fn what_gcc_refuses_is_refused_in_its_words() {
    for (written, error) in [
        ("1", "attribute 'simd' argument not a string"),
        (
            "\"both\"",
            "only 'inbranch' and 'notinbranch' flags are allowed for '__simd__' attribute",
        ),
        (
            "\"notinbranch\", \"inbranch\"",
            "wrong number of arguments specified for 'simd' attribute",
        ),
    ] {
        let source = format!("__attribute__((simd({written}))) int f(int x) {{ return x; }}\n");
        let (ok, _, err) = assembly("refused", TARGET, &source);
        assert!(!ok, "{written} was taken");
        assert!(err.contains(error), "{written}: {err}");
    }
    let (ok, _, err) = assembly(
        "count",
        TARGET,
        "__attribute__((simd(\"notinbranch\", \"inbranch\"))) int f(int x) { return x; }\n",
    );
    assert!(!ok);
    assert!(err.contains("expected between 0 and 1, found 2"), "{err}");
    let (ok, out, err) = assembly("object", TARGET, "__attribute__((simd)) int v;\n");
    assert!(ok, "{err}");
    assert!(err.contains("'simd' attribute ignored"), "{err}");
    assert!(err.contains("[-Wattributes]") || err.contains("E0703"), "{err}");
    assert!(versions(&out).is_empty(), "{out}");
}

#[test]
fn a_type_without_a_lane_is_warned_about_last_function_first() {
    let (ok, out, err) = assembly(
        "unsupported",
        TARGET,
        "struct P { int a; };\n\
         __attribute__((simd)) _Complex double f1(int a) { return a; }\n\
         __attribute__((simd)) int f2(struct P p) { return p.a; }\n\
         __attribute__((simd)) long double f3(int a) { return a; }\n\
         __attribute__((simd)) int f4(int a, __int128 b) { return a + (int)b; }\n\
         __attribute__((simd)) int f5(_Atomic int a) { return a; }\n\
         __attribute__((simd(\"notinbranch\"))) int f6(int a) { return a; }\n",
    );
    assert!(ok, "{err}");
    let said: Vec<&str> = err.lines().filter(|line| line.contains("warning:")).collect();
    let wanted = [
        "ignoring '#pragma omp declare simd' on function with '_Atomic' qualified non-'uniform' \
         argument",
        "unsupported argument type '__int128' for simd",
        "unsupported return type 'long double' for simd",
        "unsupported argument type 'struct P' for simd",
        "unsupported return type '_Complex double' for simd",
    ];
    assert_eq!(said.len(), wanted.len(), "{err}");
    for (line, wanted) in said.iter().zip(wanted) {
        assert!(line.contains(wanted), "{wanted} is not {line}:\n{err}");
    }
    // Each warning is at the function's name, and the function that has lanes still gets them.
    assert!(err.contains("a.c:6:"), "{err}");
    assert_eq!(versions(&out), ["_ZGVbN4v_f6"], "{out}");
}

/// The versions called by name, each lane compared with a call of the function itself: two lanes
/// of `double` with and without a mask, and sixteen of `char` whose `long` comes in eight vectors,
/// more than there are registers, with a mask after them that goes on the stack.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const PROGRAM: &str = r#"
#include <stdio.h>
typedef double v2d __attribute__((vector_size(16)));
typedef long v2l __attribute__((vector_size(16)));
typedef char v16c __attribute__((vector_size(16)));
typedef int v4i __attribute__((vector_size(16)));
__attribute__((simd)) double h(double x) { return x * 3 + 1; }
__attribute__((simd("inbranch"))) char c(char a, long b) { return a * 2 + (char)b; }
__attribute__((simd("notinbranch"))) int deref(int *p) { return *p + 1; }
extern v2d hn(v2d) __asm__("_ZGVbN2v_h");
extern v2d hm(v2d, v2l) __asm__("_ZGVbM2v_h");
extern v16c cm(v16c, v2l, v2l, v2l, v2l, v2l, v2l, v2l, v2l, v16c) __asm__("_ZGVbM16vv_c");
extern v4i dn(v2l, v2l) __asm__("_ZGVbN4v_deref");
int main(void) {
  v2d x = {2.5, -4};
  v2d n = hn(x);
  v2d m = hm(x, (v2l){0, -1});
  if (n[0] != h(2.5) || n[1] != h(-4) || m[0] != 0 || m[1] != h(-4)) return 1;
  v16c a, mask;
  v2l b[8];
  for (int i = 0; i < 16; i++) {
    a[i] = i;
    mask[i] = i % 3 ? -1 : 0;
    b[i / 2][i % 2] = 100 + i;
  }
  v16c r = cm(a, b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], mask);
  for (int i = 0; i < 16; i++)
    if (r[i] != (i % 3 ? c(i, 100 + i) : 0)) return 2;
  int k[4] = {10, 20, 30, 40};
  v4i d = dn((v2l){(long)&k[0], (long)&k[1]}, (v2l){(long)&k[2], (long)&k[3]});
  for (int i = 0; i < 4; i++)
    if (d[i] != k[i] + 1) return 3;
  printf("%g %g %d\n", n[1], m[1], r[1]);
  return 0;
}
"#;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn the_versions_give_each_lane_its_call() {
    let dir = dir("run");
    std::fs::write(dir.join("a.c"), PROGRAM).expect("the fixture can be written");
    for flags in [&["-O0"][..], &["-O2"], &["-O2", "-fPIC", "-pie"]] {
        let mut args = flags.to_vec();
        args.extend(["a.c", "-o", "prog"]);
        let (ok, _, said) = run(&dir, &args);
        assert!(ok, "{flags:?}: {said}");
        let out = Command::new(dir.join("prog")).output().expect("what was linked can be run");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{flags:?}: {:?} {stdout}", out.status.code());
        assert_eq!(stdout, "-11 -11 103\n", "{flags:?}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn has_attribute_answers_yes_only_where_it_is_built() {
    let source = "#if __has_attribute(simd)\nint built;\n#endif\n\
                  __attribute__((simd)) double h(double x) { return x; }\n";
    for (target, built) in
        [(TARGET, true), ("x86_64-w64-mingw32", false), ("aarch64-unknown-linux-gnu", false)]
    {
        let (ok, out, err) = assembly("asked", target, source);
        assert!(ok, "{target}: {err}");
        assert_eq!(err, "", "{target}");
        assert_eq!(out.contains("built"), built, "{target}:\n{out}");
        assert_eq!(!versions(&out).is_empty(), built, "{target}:\n{out}");
    }
}
