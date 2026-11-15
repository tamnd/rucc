//! A static initializer may copy a structure out of a `static const` object, the way gcc takes it.
//!
//! Linux 7.4 writes `.parent = clk_main[0]` in the zx297520v3 clock table, where `clk_main` is a
//! `static const` array of structures, and the nt36532 panel and the Qualcomm PCIe multiphy copy
//! whole `static const` structures the same way. Each was "initializer element is not constant"
//! here. The bytes laid in are the ones the object's own initializer put in that part.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const SOURCE: &str = r"
struct p { int fw; int id; };
struct q { int a; struct p parent; int div; };
static const struct p clk_main[] = { { 11, -1 }, { 12, 13 } };
static const struct p one = { 21, 3 };
static const int k = 5;
static const struct q nest = { 7, clk_main[0], 8 };
static const struct q t[] = {
	{ .a = 1, .parent = clk_main[1], .div = 2 },
	{ .a = 4, .parent = one, .div = k },
	nest,
};
const struct q *get(int i) { return &t[i]; }
";

const NOT_CONST: &str = r"
struct p { int fw; int id; };
struct q { int a; struct p parent; };
static struct p loose = { 1, 2 };
static const struct q t = { 0, loose };
const struct q *get(void) { return &t; }
";

/// A directory of its own for one compile, so that two tests running at once do not share files.
fn scratch() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rucc-const-record-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir
}

fn compile(source: &str, level: &str) -> std::process::Output {
    let dir = scratch();
    std::fs::write(dir.join("one.c"), source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .current_dir(&dir)
        .args(["--target=x86_64-linux-gnu", level, "-S", "-o", "-", "one.c"])
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    out
}

#[test]
fn a_structure_read_out_of_a_const_object_is_its_bytes() {
    for level in ["-O0", "-O2"] {
        let out = compile(SOURCE, level);
        assert!(out.status.success(), "{level}: {}", String::from_utf8_lossy(&out.stderr));
        let text = String::from_utf8(out.stdout).expect("a listing is text");
        let image = text.split("\nt:").nth(1).expect("the table is in the listing");
        let image = image.split(".size").next().unwrap_or(image);
        let numbers: Vec<&str> = image
            .lines()
            .filter_map(|line| line.trim().strip_prefix(".long"))
            .map(str::trim)
            .collect();
        let want = ["1", "12", "13", "2", "4", "21", "3", "5", "7", "11", "4294967295", "8"];
        assert_eq!(numbers, want, "{level}:\n{text}");
    }
}

#[test]
fn a_structure_read_out_of_an_object_that_is_not_const_is_still_refused() {
    let out = compile(NOT_CONST, "-O2");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("initializer element is not constant"), "{stderr}");
}
