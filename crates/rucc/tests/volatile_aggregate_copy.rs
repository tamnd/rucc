//! A copy into a volatile complex or structure object is a volatile access.
//!
//! The type of an assignment and of an initializer entry is not qualified, so the copy was a plain
//! `memcpy`, and at -O2 the passes made it shorter. With `volatile long double complex d`, the
//! write `d = g(d)` kept only the 10 bytes of the real half that the read `__real__ d` used. C17
//! 6.7.3p7 makes each access to a volatile object one that the program does (tamnd/rucc#4535).

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const SOURCE: &str = r"
long double _Complex g(long double _Complex);
struct S { int a[8]; };
struct S make(void);

double h(void)
{
	volatile long double _Complex d = 1.0L;
	d = g(d);
	return __real__ d;
}

int k(void)
{
	volatile struct S s = make();
	s = make();
	return s.a[0];
}
";

/// A directory of its own for one compile, so that two tests running at once do not share files.
fn scratch() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rucc-volatile-copy-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir
}

/// The IR of the fixture at -O2.
fn ir() -> String {
    let dir = scratch();
    std::fs::write(dir.join("one.c"), SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .current_dir(&dir)
        .args(["--target=x86_64-linux-gnu", "-O2", "--emit=ir", "one.c", "-o", "one.ir"])
        .output()
        .expect("the compiler is built before its own tests run");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let ir = std::fs::read_to_string(dir.join("one.ir")).expect("one.ir was written");
    let _ = std::fs::remove_dir_all(&dir);
    ir
}

/// The body of one function in the IR.
fn body<'a>(ir: &'a str, name: &str) -> &'a str {
    let start = ir.find(&format!("func @{name}(")).unwrap_or_else(|| panic!("no {name} in\n{ir}"));
    let rest = &ir[start..];
    &rest[..rest.find("\n}").map_or(rest.len(), |end| end + 2)]
}

/// The copies of a function that are volatile and move all 32 bytes of the object.
fn whole(body: &str) -> usize {
    body.lines()
        .filter(|line| line.contains("memcpy") && line.contains("volatile"))
        .filter(|line| line.contains("size 32"))
        .count()
}

#[test]
fn a_copy_into_a_volatile_complex_is_volatile_and_whole() {
    let ir = ir();
    let h = body(&ir, "h");
    assert_eq!(whole(h), 2, "the initialization and the assignment of d write all of d, {h}");
}

#[test]
fn a_copy_into_a_volatile_structure_is_volatile_and_whole() {
    let ir = ir();
    let k = body(&ir, "k");
    assert_eq!(whole(k), 2, "the initialization and the assignment of s write all of s, {k}");
}
