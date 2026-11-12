//! A `static` function that only this file calls takes its first three words of arguments in
//! `eax`, `edx` and `ecx` on i386 when the compiler optimizes, which is what gcc does
//! (`ix86_function_regparm`). The NetBSD boot code is built at `-Os` with a size limit, and the
//! stack stores and loads that rucc put around every such call made it larger than gcc's
//! (tamnd/rucc-bsd#5).
//!
//! A function whose address leaves the call keeps the stack, since a caller this file does not
//! see may use it. So does a variadic one, and so does every function at `-O0`, under `-pg` and
//! under `-finstrument-functions`.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const SOURCE: &str = "\
extern volatile int v;
static __attribute__((noinline)) int local(int a, int b, int c) { return a * 100 + b * 10 + c; }
static __attribute__((noinline)) int taken(int a, int b, int c) { return a * 100 + b * 10 + c; }
static __attribute__((noinline)) int listed(int n, ...) { return n * 100; }
int (*keep)(int, int, int) = taken;
int use(void) { return local(v, v, v) + taken(v, v, v) + listed(v, v); }
";

fn compile(flags: &[&str]) -> String {
    // The tests in this file run on threads of one process, so each takes a directory of its own.
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("rucc-i386-local-registers-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=i686-unknown-linux-gnu", "-fno-asynchronous-unwind-tables", "-S"])
        .args(flags)
        .args(["-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{flags:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("a listing is text")
}

/// The body of one function in a listing, from its label to the next `.size`.
fn body<'a>(listing: &'a str, name: &str) -> &'a str {
    let start = listing.find(&format!("\n{name}:")).expect("the function is in the listing");
    let rest = &listing[start..];
    &rest[..rest.find(".size").unwrap_or(rest.len())]
}

#[test]
fn a_function_only_called_reads_its_arguments_from_registers() {
    for level in ["-O1", "-O2", "-Os"] {
        let listing = compile(&[level]);
        let local = body(&listing, "local");
        assert!(!local.contains("(%esp)"), "{level}:\n{local}");
        assert!(local.contains("imull\t$100, %eax"), "{level}:\n{local}");
        assert!(local.contains("imull\t$10, %edx"), "{level}:\n{local}");
        assert!(local.contains("%ecx"), "{level}:\n{local}");
    }
}

#[test]
fn a_function_whose_address_is_taken_keeps_the_stack() {
    let listing = compile(&["-O2"]);
    let taken = body(&listing, "taken");
    assert!(taken.contains("4(%esp)"), "{taken}");
    let listed = body(&listing, "listed");
    assert!(listed.contains("4(%esp)"), "{listed}");
}

#[test]
fn no_function_changes_without_the_optimizer_or_under_a_hook() {
    for flags in [&["-O0"][..], &["-O2", "-pg"], &["-O2", "-finstrument-functions"]] {
        let listing = compile(flags);
        let local = body(&listing, "local");
        assert!(local.contains("(%esp)"), "{flags:?}:\n{local}");
        let call = body(&listing, "use");
        assert!(
            call.contains("%ecx, 4(%esp)") || call.contains("%ecx, (%esp)"),
            "{flags:?}:\n{call}"
        );
    }
}
