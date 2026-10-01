//! `__attribute__((target_clones(...)))`, end to end: the versions, the resolver and the indirect
//! function under the function's own name, and what is refused and warned about on the way.
//!
//! Design: `spec/13-gnu-compat.md` section 13.4.
//!
//! A test of the whole compiler rather than of one crate, because the attribute is read in the
//! checker, built in the lowering and written by the assembler as an indirect function, and the
//! program only runs if all three agree on the names. The assembly is read for the shape gcc 16
//! writes, and on an x86-64 Linux host the program is also run, with the version the resolver
//! picked checked against what `__builtin_cpu_supports` says the processor has.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The target is written down rather than taken from the host, since only x86-64 ELF is built.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-clones-{}-{what}", std::process::id()));
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

/// The assembly of one source for a target, and what was said, with the compiler refusing it an
/// error of the test's.
fn assembly(what: &str, target: &str, source: &str) -> (bool, String, String) {
    let dir = dir(what);
    std::fs::write(dir.join("a.c"), source).expect("the fixture can be written");
    let target = format!("--target={target}");
    let said = run(&dir, &[&target, "-O2", "-S", "-o", "-", "a.c"]);
    let _ = std::fs::remove_dir_all(&dir);
    said
}

/// The directive lines and labels of an assembly listing, trimmed, which is the shape asserted.
fn shape(out: &str) -> Vec<&str> {
    out.lines()
        .map(str::trim)
        .filter(|line| {
            line.ends_with(':')
                || [".globl", ".weak", ".type", ".set"].iter().any(|d| line.starts_with(d))
        })
        .collect()
}

#[test]
fn every_version_is_built_with_a_resolver_and_an_indirect_function() {
    let (ok, out, err) = assembly(
        "shape",
        TARGET,
        "__attribute__((target_clones(\"sse4.2,avx2\", \"arch=x86-64-v3\", \"default\", \"avx2\")))\n\
         int f(int x) { return x * 2; }\n\
         static __attribute__((target_clones(\"popcnt\", \"default\"))) int g(int x) { return x; }\n\
         int h(int x) { return f(x) + g(x); }\n",
    );
    assert!(ok, "{err}");
    assert_eq!(err, "");
    let shape = shape(&out);
    // The versions highest priority first, avx2 once however many times it was written, and each
    // one local, which is gcc's order and gcc's binding.
    let versions: Vec<&str> = shape
        .iter()
        .copied()
        .filter(|line| line.starts_with("f.") && line.ends_with(':'))
        .collect();
    assert_eq!(
        versions,
        ["f.arch_x86_64_v3:", "f.avx2:", "f.sse4_2:", "f.default:", "f.resolver:"],
        "{out}"
    );
    assert!(!shape.iter().any(|line| line.starts_with(".globl\tf.")), "{out}");
    // The resolver of an external function is weak, and of a static one local.
    assert!(shape.contains(&".weak\tf.resolver"), "{out}");
    assert!(
        !shape.iter().any(|line| line.contains("g.resolver")
            && !line.ends_with(':')
            && !line.starts_with(".type")
            && !line.starts_with(".set")),
        "{out}"
    );
    // The name itself is the indirect function, global for `f` and local for `g`.
    for line in [".globl\tf", ".type\tf, @gnu_indirect_function", ".set\tf,f.resolver"] {
        assert!(shape.contains(&line), "{line} missing:\n{out}");
    }
    for line in [".type\tg, @gnu_indirect_function", ".set\tg,g.resolver"] {
        assert!(shape.contains(&line), "{line} missing:\n{out}");
    }
    assert!(!shape.contains(&".globl\tg"), "{out}");
    assert!(out.contains("call\t__cpu_indicator_init"), "{out}");
    // The level is a bit of `__cpu_features2`, and the extensions are bits of `__cpu_model`.
    assert!(out.contains("__cpu_features2+8(%rip)"), "{out}");
    assert!(out.contains("__cpu_model+12(%rip)"), "{out}");
}

#[test]
fn the_last_declaration_names_the_versions() {
    let (ok, out, err) = assembly(
        "last",
        TARGET,
        "__attribute__((target_clones(\"avx2\", \"default\"))) int f(int);\n\
         int f(int x) { return x * 3; }\n\
         __attribute__((target_clones(\"sse4.2\", \"avx\", \"default\"))) int f(int);\n",
    );
    assert!(ok, "{err}");
    let versions: Vec<&str> =
        shape(&out).into_iter().filter(|line| line.starts_with("f.")).collect();
    assert_eq!(versions, ["f.avx:", "f.sse4_2:", "f.default:", "f.resolver:"], "{out}");
}

#[test]
fn what_gcc_refuses_is_refused_in_its_words() {
    for (written, error) in [
        (
            "\"avx2\", \"bogus\", \"default\"",
            "attribute 'target_clone' argument 'bogus' is unknown",
        ),
        ("\"cmov\", \"default\"", "attribute 'target_clone' argument 'cmov' is unknown"),
        (
            "\"no-avx2\", \"default\"",
            "ISA 'no-avx2' is not supported in 'target' attribute, use 'arch=' syntax",
        ),
        ("\"avx2\", \"\"", "an empty string cannot be in 'target_clones' attribute"),
        ("\"avx2\", \"sse4.2\"", "'default' target was not set"),
        ("1", "'target_clones' attribute argument not a string constant"),
        ("\"arch=haswell\", \"default\"", "'arch=haswell' is not built yet"),
    ] {
        let source =
            format!("__attribute__((target_clones({written}))) int f(int x) {{ return x; }}\n");
        let (ok, _, err) = assembly("refused", TARGET, &source);
        assert!(!ok, "{written} was taken");
        assert!(err.contains(error), "{written}: {err}");
    }
    let (ok, _, err) =
        assembly("none", TARGET, "__attribute__((target_clones)) int f(int x) { return x; }\n");
    assert!(!ok);
    assert!(
        err.contains("wrong number of arguments specified for 'target_clones' attribute"),
        "{err}"
    );
}

#[test]
fn what_gcc_drops_is_dropped_with_its_warning() {
    for (source, warning, versions) in [
        (
            "__attribute__((target_clones(\"avx2\"))) int f(int x) { return x; }\n",
            "single 'target_clones' attribute is ignored",
            false,
        ),
        (
            "__attribute__((target_clones(\"avx2\", \"default\"), target(\"sse4.2\")))\n\
             int f(int x) { return x; }\n",
            "ignoring attribute 'target' because it conflicts with attribute 'target_clones'",
            true,
        ),
        (
            "__attribute__((target_clones(\"avx2\", \"default\"), always_inline))\n\
             int f(int x) { return x; }\n",
            "ignoring attribute 'always_inline' because it conflicts with attribute 'target_clones'",
            true,
        ),
        (
            "__attribute__((target_clones(\"avx2\", \"default\"))) int v;\n",
            "'target_clones' attribute ignored",
            false,
        ),
    ] {
        let (ok, out, err) = assembly("dropped", TARGET, source);
        assert!(ok, "{err}");
        assert!(err.contains(warning), "{source}: {err}");
        assert!(err.contains("[-Wattributes]") || err.contains("E0703"), "{err}");
        assert_eq!(out.contains("f.resolver"), versions, "{source}:\n{out}");
    }
}

#[test]
fn a_target_without_indirect_functions_refuses_it() {
    let source =
        "__attribute__((target_clones(\"avx2\", \"default\"))) int f(int x) { return x; }\n";
    for (target, error) in [
        ("x86_64-w64-mingw32", "the call requires 'ifunc', which is not supported by this target"),
        ("x86_64-apple-darwin", "the call requires 'ifunc', which is not supported by this target"),
        ("aarch64-unknown-linux-gnu", "'target_clones' is not built for aarch64 yet"),
    ] {
        let (ok, _, err) = assembly("elsewhere", target, source);
        assert!(!ok, "{target} took it");
        assert!(err.contains(error), "{target}: {err}");
    }
}

/// The program run: a function in eight versions, the resolver called by its symbol, and the
/// version it picked compared with the first one in gcc's order whose extensions
/// `__builtin_cpu_supports` says the processor has.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const PROGRAM: &str = r#"
#include <stdio.h>
#include <string.h>
#define CLONES "arch=x86-64-v4", "avx512f", "arch=x86-64-v3", "avx2", "popcnt", "arch=x86-64-v2", \
    "sse4.2", "sse3", "default"
__attribute__((target_clones(CLONES))) int f(int x) { static int calls; return x * 2 + ++calls; }
extern void *pick(void) __asm__("f.resolver");
#define V(n, s) extern int n(int) __asm__("f." s);
V(v4, "arch_x86_64_v4") V(a512, "avx512f") V(v3, "arch_x86_64_v3") V(a2, "avx2")
V(pop, "popcnt") V(v2, "arch_x86_64_v2") V(s42, "sse4_2") V(s3, "sse3") V(dflt, "default")
int main(void) {
  __builtin_cpu_init();
  struct { const char *name; void *at; int has; } order[] = {
    {"v4", v4, __builtin_cpu_supports("x86-64-v4")},
    {"avx512f", a512, __builtin_cpu_supports("avx512f")},
    {"v3", v3, __builtin_cpu_supports("x86-64-v3")},
    {"avx2", a2, __builtin_cpu_supports("avx2")},
    {"popcnt", pop, __builtin_cpu_supports("popcnt")},
    {"v2", v2, __builtin_cpu_supports("x86-64-v2")},
    {"sse4.2", s42, __builtin_cpu_supports("sse4.2")},
    {"sse3", s3, __builtin_cpu_supports("sse3")},
    {"default", dflt, 1},
  };
  void *got = pick();
  for (unsigned i = 0; i < sizeof order / sizeof *order; i++) {
    if (order[i].has) {
      if (order[i].at != got) { printf("wanted %s\n", order[i].name); return 1; }
      break;
    }
  }
  int (*through)(int) = f;
  printf("%d %d\n", f(20), through(20));
  return 0;
}
"#;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn the_resolver_picks_the_version_the_processor_runs_best() {
    let dir = dir("run");
    std::fs::write(dir.join("a.c"), PROGRAM).expect("the fixture can be written");
    for flags in [&["-O0"][..], &["-O2"], &["-O2", "-fPIC", "-pie"], &["-O1", "-static"]] {
        let mut args = flags.to_vec();
        args.extend(["a.c", "-o", "prog"]);
        let (ok, _, said) = run(&dir, &args);
        assert!(ok, "{flags:?}: {said}");
        let out = Command::new(dir.join("prog")).output().expect("what was linked can be run");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{flags:?}: {stdout}");
        // The call through the pointer is to the same version as the call by name, and its `static`
        // has counted the first.
        assert_eq!(stdout, "41 42\n", "{flags:?}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn has_attribute_answers_yes_only_where_it_is_built() {
    let source = "#if __has_attribute(target_clones)\nint built;\n#endif\n";
    for (target, built) in
        [(TARGET, true), ("x86_64-w64-mingw32", false), ("aarch64-unknown-linux-gnu", false)]
    {
        let (ok, out, err) = assembly("asked", target, source);
        assert!(ok, "{target}: {err}");
        assert_eq!(out.contains("built"), built, "{target}:\n{out}");
    }
}
