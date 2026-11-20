//! A call reads the structure it passes by value, even when its callee is `const`.
//!
//! The caller copies the bytes into the argument area at the call, so the stores that made the
//! object are read there. Dead store elimination took a `const` callee at its word and removed the
//! stores, value numbering made two calls one when the object changed between them, and loop
//! invariant code motion took the call out of a loop that wrote the object. msun's
//! ctrig_test is the first: `conjl` is `const` and takes its `long double complex` by value, and
//! the argument of `conjl(1.0)` was never written, so the test compared against stack garbage.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const SOURCE: &str = r"
struct S { int a[8]; };
__attribute__((const)) int f(struct S s);
__attribute__((const)) _Complex long double cj(_Complex long double z);

int twice(int v)
{
	struct S s = {{0}};
	s.a[0] = v;
	int x = f(s);
	s.a[0] = v + 1;
	int y = f(s);
	return x * 10 + y;
}

int sum(int v)
{
	struct S s = {{0}};
	int t = 0;
	for (int i = 0; i < 3; i++) {
		s.a[0] = v + i;
		t += f(s);
	}
	return t;
}

long double one(void)
{
	return __real__ cj(1.0L);
}
";

/// A directory of its own for one compile, so that two tests running at once do not share files.
fn scratch() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rucc-const-byval-{}-{n}", std::process::id()));
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

#[test]
fn a_const_callee_still_reads_the_structure_it_is_passed() {
    let ir = ir();
    let twice = body(&ir, "twice");
    let calls = twice.lines().filter(|line| line.contains("call") && line.contains("@f(")).count();
    assert_eq!(calls, 2, "the object changed between the calls, {twice}");
    let stores = twice.lines().filter(|line| line.trim_start().starts_with("store")).count();
    assert!(stores >= 2, "each call reads what was stored before it, {twice}");

    let sum = body(&ir, "sum");
    assert!(
        sum.lines().any(|line| line.trim_start().starts_with("store")),
        "the loop writes the object each call reads, so the store stays in it, {sum}"
    );

    let one = body(&ir, "one");
    assert!(
        one.contains("0x3fff8000000000000000"),
        "the 1.0 that cj is passed is written before the call, {one}"
    );
}
