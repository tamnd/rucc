//! Two string literals spelled the same in one unit are one object, as gcc makes them.
//!
//! The kernel's sound codecs name `"DSP1"` and its kind two thousand times in their routing
//! tables. One object for each made `cs47l90.o` hold 144113 literals where gcc has 1256, and the
//! assembler spent most of the unit on them.

use std::process::Command;

/// The listing rucc writes for a source at `-O2`.
fn listing(what: &str, source: &str) -> String {
    let dir = std::env::temp_dir().join(format!("rucc-literals-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-unknown-linux-gnu", "-O2", "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "the compiler refused the fixture:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// How many literal objects a listing defines.
fn literals(text: &str) -> usize {
    text.lines().filter(|l| l.starts_with(".Lstr") && l.ends_with(':')).count()
}

#[test]
fn a_literal_spelled_twice_is_one_object() {
    let text = listing(
        "twice",
        "const char *const routes[][2] = { { \"DSP1\", \"AIF1\" }, { \"DSP1\", \"AIF2\" },\n\
         { \"DSP1\", \"AIF1\" } };\n\
         const char *f(void) { return \"DSP1\"; }\n",
    );
    assert_eq!(literals(&text), 3, "{text}");
}

#[test]
fn a_wide_literal_is_not_the_narrow_one() {
    let text = listing(
        "wide",
        "typedef int wchar_t;\n\
         const char *a = \"\\1\\0\\0\\0\\0\\0\\0\";\n\
         const wchar_t *b = L\"\\1\";\n",
    );
    assert_eq!(literals(&text), 2, "{text}");
}

#[test]
fn func_is_not_merged_with_a_literal() {
    let text = listing(
        "func",
        "const char *f(void) { return __func__; }\n\
         const char *g(void) { return \"f\"; }\n",
    );
    assert_eq!(literals(&text), 1, "{text}");
    assert!(text.contains("__func__."), "{text}");
}
