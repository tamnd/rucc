//! Integer work on the Plan 9 targets uses general registers only, end to end.
//!
//! Design: tamnd/rucc#4158, and 08.6 and 09.3 of the rucc-plan9 plan.
//!
//! The 9front kernel does not save the vector registers on entry, so a struct copy or a fill
//! that used one would overwrite the state of a user process. The rule applies to all code on
//! these targets, which `docs/TARGETS.md` writes down. A copy too large to write out calls
//! `memmove`, because a kernel or a library that gives only `memmove` must link.

use std::path::PathBuf;
use std::process::Command;

fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir()
        .join(format!("rucc-plan9-integer-registers-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// The assembly the compiler wrote, after it took the source.
fn compile(target: &str, level: &str, what: &str, source: &str) -> String {
    let path = fixture(&format!("{target}{level}-{what}"), source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args([&format!("--target={target}"), level, "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{target} {level}: {err}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Whether one operand names a vector or a floating point register of the target.
fn vector(target: &str, word: &str) -> bool {
    if target.starts_with("x86_64") {
        return ["%xmm", "%ymm", "%zmm"].iter().any(|bank| word.starts_with(bank));
    }
    let mut chars = word.chars();
    matches!(chars.next(), Some('q' | 'v' | 'd' | 's' | 'h' | 'b'))
        && chars.next().is_some_and(|c| c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_digit() || c == '.' || c.is_ascii_alphabetic())
}

/// The instructions with a vector register in them.
fn vectors(target: &str, asm: &str) -> Vec<String> {
    asm.lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('.') && !line.ends_with(':'))
        .filter(|line| {
            line.split(|c: char| {
                c.is_whitespace() || matches!(c, ',' | '[' | ']' | '(' | ')' | '!')
            })
            .any(|word| vector(target, word))
        })
        .map(str::to_owned)
        .collect()
}

const INTEGER: &str = "typedef struct Small Small;\n\
                       typedef struct Mid Mid;\n\
                       typedef struct Big Big;\n\
                       struct Small { long a, b, c, d; };\n\
                       struct Mid { char name[28]; long long q[5]; };\n\
                       struct Big { long long a[24]; };\n\
                       void copysmall(Small *d, Small *s) { *d = *s; }\n\
                       void copymid(Mid *d, Mid *s) { *d = *s; }\n\
                       void copybig(Big *d, Big *s) { *d = *s; }\n\
                       void zerosmall(Small *d) { Small z = {0}; *d = z; }\n\
                       void zeromid(Mid *d) { Mid z = {0}; *d = z; }\n\
                       void clear(char *p) { __builtin_memset(p, 0, 100); }\n\
                       void fill(char *p, int c) { __builtin_memset(p, c, 33); }\n\
                       void move(char *d, char *s) { __builtin_memcpy(d, s, 120); }\n\
                       int sum(int *a, int n) { int i, t = 0; for(i = 0; i < n; i++) t += a[i]; \
                       return t; }\n\
                       void add(long long *a, long long *b, int n) { int i; \
                       for(i = 0; i < n; i++) a[i] += b[i]; }\n\
                       extern void take(Mid);\n\
                       void pass(Mid *m) { take(*m); }\n";

#[test]
fn integer_work_uses_no_vector_register() {
    for target in ["x86_64-plan9", "aarch64-plan9"] {
        for level in ["-O0", "-O1", "-O2", "-O3", "-Os"] {
            let asm = compile(target, level, "integer", INTEGER);
            let found = vectors(target, &asm);
            assert!(found.is_empty(), "{target} {level}: {found:?}");
        }
    }
}

#[test]
fn a_float_still_goes_in_a_vector_register() {
    // The rule is about integer work. kencc keeps a float in `X0` and `F0` too, so the check above
    // is not passing because nothing at all is found.
    let source = "double twice(double x) { return x * 2.0; }\n";
    for target in ["x86_64-plan9", "aarch64-plan9"] {
        let asm = compile(target, "-O2", "float", source);
        assert!(!vectors(target, &asm).is_empty(), "{target}: {asm}");
    }
}

#[test]
fn a_large_copy_calls_memmove_and_not_memcpy() {
    // Only a call the compiler adds is in the rule. A call that the program names stays as it is.
    let source = "struct Huge { long long a[128]; };\n\
                  void copy(struct Huge *d, struct Huge *s) { *d = *s; }\n\
                  void clear(struct Huge *d) { struct Huge z = {0}; *d = z; }\n";
    for target in ["x86_64-plan9", "aarch64-plan9"] {
        for level in ["-O0", "-O2"] {
            let asm = compile(target, level, "large", source);
            assert!(!asm.contains("memcpy"), "{target} {level}: {asm}");
            assert!(asm.contains("memmove"), "{target} {level}: {asm}");
            assert!(asm.contains("memset"), "{target} {level}: {asm}");
        }
    }
    // Every other target keeps `memcpy`, which is the faster of the two in its C library.
    let asm = compile("x86_64-linux-gnu", "-O2", "large", source);
    assert!(asm.contains("memcpy"), "{asm}");
}
