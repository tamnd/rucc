//! A compound literal that initializes an object is written straight into the object.
//!
//! Before, the literal had a slot of its own, was built there, and was copied into the object, so
//! the frame held every literal as well as the object. The kernel's 6.12
//! `damon_test_three_regions_in_vmas` fills an array of six `vm_area_struct` with
//! `(struct vm_area_struct){ ... }` each, and its frame was 2304 bytes on AArch64, over
//! FRAME_WARN, where gcc's is 1280.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// An array of six records from six literals, the same with plain braces, one record from one
/// literal, and a record of two records from two literals.
const SOURCE: &str = r"
struct S { unsigned long start, end; long a[20]; };
struct P { struct S x, y; };
extern int use(struct S *, int);
extern int usep(struct P *);

int six(void)
{
	struct S v[] = {
		(struct S){.start = 10, .end = 20},
		(struct S){.start = 20, .end = 25},
		(struct S){.start = 200, .end = 210},
		(struct S){.start = 210, .end = 220},
		(struct S){.start = 300, .end = 305},
		(struct S){.start = 307, .end = 330},
	};
	return use(v, 6);
}

int braces(void)
{
	struct S v[] = {
		{.start = 10, .end = 20},
		{.start = 20, .end = 25},
		{.start = 200, .end = 210},
		{.start = 210, .end = 220},
		{.start = 300, .end = 305},
		{.start = 307, .end = 330},
	};
	return use(v, 6);
}

int one(void)
{
	struct S x = (struct S){.start = 1};
	return use(&x, 1);
}

int two(void)
{
	struct P p = { .x = (struct S){.start = 1}, .y = (struct S){.end = 2} };
	return usep(&p);
}
";

/// A directory of its own for one compile, so that two tests running at once do not share files.
fn scratch(what: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rucc-literal-{}-{n}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir
}

/// The `.su` one compile of a program wrote.
fn usage(target: &str, source: &str) -> String {
    let dir = scratch(target);
    std::fs::write(dir.join("one.c"), source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .current_dir(&dir)
        .arg(format!("--target={target}"))
        .args(["-O2", "-fstack-usage", "-S", "one.c", "-o", "one.s"])
        .output()
        .expect("the compiler is built before its own tests run");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let usage = std::fs::read_to_string(dir.join("one.su")).expect("one.su was written");
    let _ = std::fs::remove_dir_all(&dir);
    usage
}

/// The bytes the `.su` says a function takes.
fn frame(usage: &str, function: &str) -> u32 {
    usage
        .lines()
        .find_map(|line| {
            let mut fields = line.split_whitespace();
            let place = fields.next()?;
            place.ends_with(&format!(":{function}")).then(|| fields.next()?.parse().ok())?
        })
        .unwrap_or_else(|| panic!("no line for {function} in\n{usage}"))
}

/// gcc 14 takes 1088 bytes for `six` on AArch64, 208 for `one` and 384 for `two`, and rucc took
/// 2144, 368 and 736. Now the literals cost about what plain braces cost.
#[test]
fn a_literal_that_initializes_an_object_takes_no_slot_of_its_own() {
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        let usage = usage(target, SOURCE);
        let braces = frame(&usage, "braces");
        let six = frame(&usage, "six");
        assert!(six <= braces + 32, "{target}: {six} bytes for six against {braces} with braces");
        let one = frame(&usage, "one");
        assert!(one <= 224, "{target}: {one} bytes for one");
        let two = frame(&usage, "two");
        assert!(two <= 400, "{target}: {two} bytes for two");
    }
}

/// What is not named is still zero, a literal inside a literal still lands where it should, and a
/// literal in a loop starts again each time round.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn a_literal_written_in_place_holds_what_its_initializer_says() {
    let source = r#"
extern int printf(const char *, ...);
struct S { int a, b, c[5]; };
struct P { struct S x; int k; struct S y; };

__attribute__((noinline)) unsigned long sum(const int *p, int n)
{
	unsigned long t = 0;
	for (int i = 0; i < n; i++)
		t = t * 31 + p[i];
	return t;
}

int main(void)
{
	unsigned long total = 0;
	for (int i = 0; i < 3; i++) {
		struct S v[] = {
			(struct S){.a = i, .c[4] = 7},
			(struct S){.b = i + 1},
			(struct S){0},
		};
		total += sum((int *)v, sizeof v / sizeof(int));
		struct P p = {
			.x = (struct S){.c = {1, 2}},
			.k = i,
			.y = (struct S){.a = 5, .c[2] = i},
		};
		total += sum((int *)&p, sizeof p / sizeof(int));
		v[0].c[0] = 9;
	}
	printf("%lu\n", total);
	return 0;
}
"#;
    let mut expected = 0_u64;
    let sum = |p: &[u64]| p.iter().fold(0_u64, |t, &x| t.wrapping_mul(31).wrapping_add(x));
    for i in 0..3 {
        let mut v = [0_u64; 21];
        v[0] = i;
        v[6] = 7;
        v[8] = i + 1;
        expected = expected.wrapping_add(sum(&v));
        let mut p = [0_u64; 15];
        p[2] = 1;
        p[3] = 2;
        p[7] = i;
        p[8] = 5;
        p[12] = i;
        expected = expected.wrapping_add(sum(&p));
    }
    for level in ["-O2", "-O0"] {
        let dir = scratch(&format!("run{level}"));
        std::fs::write(dir.join("lit.c"), source).expect("the fixture can be written");
        let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
            .current_dir(&dir)
            .args([level, "lit.c", "-o", "lit"])
            .output()
            .expect("the compiler is built before its own tests run");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let ran = Command::new(dir.join("lit")).output().expect("what was linked can be run");
        assert!(ran.status.success(), "{level}");
        assert_eq!(String::from_utf8_lossy(&ran.stdout).trim(), expected.to_string(), "{level}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A literal that reads nothing is assigned straight into the object too. The kernel's 7.3
/// `mldsa_verify` clears a 1024 byte member with `ws->tmp = (struct mldsa_ring_elem){};` in a
/// loop, and its frame was 1648 bytes on x86-64, where gcc 14's is 208.
#[test]
fn a_literal_that_reads_nothing_is_assigned_with_no_slot_of_its_own() {
    let source = r"
struct E { int x[256]; };
struct W { long a; struct E tmp; };
extern void g(struct W *);

void clear(struct W *ws, int k)
{
	for (int i = 0; i < k; i++) {
		ws->tmp = (struct E){};
		g(ws);
	}
}

void some(struct W *ws)
{
	ws->tmp = (struct E){ .x = { 1, 2 }, .x[200] = 3 };
	g(ws);
}
";
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        let usage = usage(target, source);
        for function in ["clear", "some"] {
            let bytes = frame(&usage, function);
            assert!(bytes <= 64, "{target}: {bytes} bytes for {function}");
        }
    }
}

/// An assigned literal that reads the object it is assigned to is still built first, so the
/// object is read before it is written, and one that reads nothing leaves no old byte behind.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn an_assigned_literal_holds_what_its_initializer_says() {
    let source = r#"
extern int printf(const char *, ...);
struct S { int a, b; char s[6]; int c[4]; };

__attribute__((noinline)) void fill(struct S *p)
{
	__builtin_memset(p, 0x5a, sizeof *p);
}

int main(void)
{
	struct S v;
	fill(&v);
	v = (struct S){ .b = 3, .s = "hi", .c[3] = -1 };
	printf("%d %d %s %d %d %d\n", v.a, v.b, v.s, v.s[5], v.c[0], v.c[3]);
	v.a = 1;
	v = (struct S){ .a = v.b, .b = v.a };
	printf("%d %d %d\n", v.a, v.b, v.c[3]);
	return 0;
}
"#;
    for level in ["-O2", "-O0"] {
        let dir = scratch(&format!("assign{level}"));
        std::fs::write(dir.join("lit.c"), source).expect("the fixture can be written");
        let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
            .current_dir(&dir)
            .args([level, "lit.c", "-o", "lit"])
            .output()
            .expect("the compiler is built before its own tests run");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let ran = Command::new(dir.join("lit")).output().expect("what was linked can be run");
        assert!(ran.status.success(), "{level}");
        assert_eq!(String::from_utf8_lossy(&ran.stdout), "0 3 hi 0 0 -1\n3 1 0\n", "{level}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
