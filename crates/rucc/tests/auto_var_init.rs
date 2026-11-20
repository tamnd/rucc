//! `-ftrivial-auto-var-init=` and the `uninitialized` attribute, end to end.
//!
//! Design: `spec/04-driver-and-cli.md` section 4.12, and tamnd/rucc#2282.
//!
//! What is checked is the IR, where the fill is one `memset` and the padding put back is one more
//! for each run of it, which is what gcc 13 writes into the same objects.

use std::process::Command;

/// The IR for `source` under `flags`, for x86-64 Linux at `-O0`.
fn ir(what: &str, flags: &[&str], source: &str) -> String {
    let dir = std::env::temp_dir().join(format!("rucc-avi-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-unknown-linux-gnu", "-O0", "--emit=ir", "-o", "-"])
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("the IR is text")
}

/// The fills in one function, as the byte, the size and the offset from the object's start, in
/// the order they are written.
fn fills(text: &str, name: &str) -> Vec<(i64, u64, u64)> {
    let open = format!("func @{name}(");
    let body: Vec<&str> = text
        .lines()
        .map(str::trim)
        .skip_while(|line| !line.starts_with(&open))
        .skip(1)
        .take_while(|line| *line != "}")
        .collect();
    let value = |name: &str| -> i64 {
        let def = format!("{name} = iconst.");
        let line = body.iter().find(|line| line.starts_with(&def)).expect("a constant");
        line.rsplit(' ').next().and_then(|n| n.parse().ok()).expect("a number")
    };
    let mut offsets = std::collections::HashMap::new();
    let mut found = Vec::new();
    for line in &body {
        // The flags come after the name, as in `ptr_add.nuw`.
        if let Some((to, rest)) = line.split_once(" = ptr_add") {
            let (_, rest) = rest.split_once(' ').expect("operands");
            let (_, by) = rest.split_once(", ").expect("two operands");
            offsets.insert(to.to_string(), value(by) as u64);
        }
        if let Some(rest) = line.strip_prefix("memset ") {
            let parts: Vec<&str> = rest.split(", ").collect();
            let size = parts[2].strip_prefix("size ").and_then(|n| n.parse().ok()).expect("size");
            let at = offsets.get(parts[0]).copied().unwrap_or(0);
            found.push((value(parts[1]), size, at));
        }
    }
    found
}

const SOURCE: &str = "\
void g(void *);
struct S { char c; int i; long l; };
struct B { unsigned a:3; unsigned b:9; char c; };
int scalar(void) { int x; g(&x); return x; }
void st(void) { struct S s; g(&s); }
void bf(void) { struct B s; g(&s); }
void arr(void) { char a[40]; g(a); }
_Bool bo(void) { _Bool b; g(&b); return b; }
void keep(void) { int x __attribute__((uninitialized)); g(&x); }
void given(void) { struct S s = { 1 }; g(&s); }
";

#[test]
fn zero_fills_every_local_without_an_initializer() {
    let text = ir("zero", &["-ftrivial-auto-var-init=zero"], SOURCE);
    assert_eq!(fills(&text, "st"), [(0, 16, 0)], "{text}");
    assert_eq!(fills(&text, "bf"), [(0, 4, 0)], "{text}");
    assert_eq!(fills(&text, "arr"), [(0, 40, 0)], "{text}");
    assert_eq!(fills(&text, "scalar"), [(0, 4, 0)], "{text}");
    assert_eq!(fills(&text, "keep"), [], "{text}");
    // The initializer already zeroes what it does not name.
    assert_eq!(fills(&text, "given"), [(0, 16, 0)], "{text}");
    let plain = ir("plain", &[], SOURCE);
    for name in ["st", "bf", "arr", "keep"] {
        assert_eq!(fills(&plain, name), [], "{name}: {plain}");
    }
}

/// gcc 13 writes `0xfe` and then zero over the padding, down to the four bits the two bit-fields
/// leave over in their second byte, and a `bool` on its own is zero.
#[test]
fn pattern_is_gcc_s_byte_with_the_padding_zero() {
    let text = ir("pattern", &["-ftrivial-auto-var-init=pattern"], SOURCE);
    assert_eq!(fills(&text, "st"), [(-2, 16, 0), (0, 3, 1)], "{text}");
    assert_eq!(fills(&text, "bf"), [(-2, 4, 0), (14, 1, 1), (0, 1, 3)], "{text}");
    assert_eq!(fills(&text, "arr"), [(-2, 40, 0)], "{text}");
    assert_eq!(fills(&text, "bo"), [(0, 1, 0)], "{text}");
    assert_eq!(fills(&text, "scalar"), [(-2, 4, 0)], "{text}");
    assert_eq!(fills(&text, "keep"), [], "{text}");
}

/// The fill is where the declaration is, so one a `switch` jumps past is not filled, and an array
/// whose length is known only at run time is filled over that length.
#[test]
fn the_fill_is_where_the_declaration_is_reached() {
    let source = "void g(void *);\n\
                  int sw(int k) { switch (k) { int y; case 1: g(&y); return y; } return 0; }\n\
                  void vla(int n) { char a[n]; g(a); }\n";
    let text = ir("where", &["-ftrivial-auto-var-init=zero"], source);
    assert_eq!(fills(&text, "sw"), [], "{text}");
    let vla: Vec<&str> = text.lines().skip_while(|line| !line.starts_with("func @vla(")).collect();
    assert!(vla.iter().any(|line| line.trim_start().starts_with("memset")), "{text}");

    // A local the program never takes the address of is a value, and it starts as the constant.
    let source = "int maybe(int c) { int x; if (c) x = 5; return x; }\n";
    let text = ir("value", &["-ftrivial-auto-var-init=pattern"], source);
    assert!(text.contains("iconst.i32 -16843010"), "{text}");
}
