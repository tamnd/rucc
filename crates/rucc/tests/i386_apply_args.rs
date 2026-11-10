//! `__builtin_apply_args` and `__builtin_return` on i386. libobjc's `sendmsg.c` forwards a message
//! with the two, and each of its three builds for i386 stopped with "no rule lowers a `apply_args`
//! producing a `ptr`", in the NetBSD build of libobjc for i386 (tamnd/rucc-bsd#3).
//!
//! The block is gcc's for i386: where the arguments are at 0 and `eax`, `edx` and `ecx` at 4, 8
//! and 12. What `__builtin_return` reads is gcc's block for what a call came back with: `eax` at 0,
//! `edx` at 4 and `st0` at 8 in the x87 format.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The shape of libobjc's forwarders, and a function that reads the arguments through the block.
const SOURCE: &str = "\
void *forward(void *, void *);
int word(void *rcv, void *op, ...) {
  void *args = __builtin_apply_args();
  void *res = forward(rcv, args);
  if (res)
    __builtin_return(res);
  return 0;
}
double real(void *rcv, void *op, ...) {
  void *args = __builtin_apply_args();
  __builtin_return(forward(rcv, args));
}
long long wide(void *rcv) {
  __builtin_return(forward(rcv, __builtin_apply_args()));
}
int second(int a, ...) {
  int **args = __builtin_apply_args();
  return args[0][1];
}
__attribute__((regparm(3))) int third(int a, int b, int c) {
  int *args = __builtin_apply_args();
  return args[1] + args[2] + args[3];
}
";

fn compile(level: &str) -> (bool, String, String) {
    // The tests in this file run on threads of one process, so each takes a directory of its own.
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir()
        .join(format!("rucc-i386-apply-args-{}-{level}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=i686-unknown-linux-gnu", level, "-fno-asynchronous-unwind-tables", "-S"])
        .args(["-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    let listing = String::from_utf8(out.stdout).expect("a listing is text");
    (out.status.success(), listing, String::from_utf8_lossy(&out.stderr).into_owned())
}

/// The body of one function in a listing, from its label to the next `.size`.
fn body<'a>(listing: &'a str, name: &str) -> &'a str {
    let start = listing.find(&format!("\n{name}:")).expect("the function is in the listing");
    let rest = &listing[start..];
    &rest[..rest.find(".size").unwrap_or(rest.len())]
}

#[test]
fn the_forwarders_compile_at_every_level() {
    for level in ["-O0", "-O1", "-O2", "-Os"] {
        let (ok, _, stderr) = compile(level);
        assert!(ok, "{level}: {stderr}");
    }
}

#[test]
fn a_double_comes_back_from_the_x87_slot_of_the_block() {
    let (ok, listing, stderr) = compile("-O2");
    assert!(ok, "{stderr}");
    let real = body(&listing, "real");
    assert!(real.contains("fldt\t8("), "{real}");
}

#[test]
fn a_regparm_function_saves_its_three_registers_in_order() {
    let (ok, listing, stderr) = compile("-O2");
    assert!(ok, "{stderr}");
    let third = body(&listing, "third");
    for (reg, at) in [("eax", 4), ("edx", 8), ("ecx", 12)] {
        let store = format!("movl\t%{reg}, {at}(");
        assert!(third.contains(&store), "no `{store}` in:\n{third}");
    }
}
