//! `-mrtd`, which makes every 32 bit x86 function that is not variadic pop its own stack
//! arguments, as `stdcall` does but with no change to the name. FreeBSD builds its `boot2` with
//! `-mrtd -mregparm=3` (tamnd/rucc#4092). The returns checked here are the ones gcc 16 gives for
//! the same source with `-m32 -mrtd`.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const SOURCE: &str = "\
int f(int a, int b) { return a + b; }
int g(int a, ...) { return a; }
int __attribute__((cdecl)) h(int a, int b) { return a - b; }
int k();
int k(a, b) int a, b; { return a * b; }
struct S { int x[3]; };
struct S r(int a) { struct S s = {{a}}; return s; }
void v(void) {}
long long m(long long x, int y) { return x + y; }
int u();
int call(void) { return f(1, 2) + k(5, 6) + u(1, 2, 3); }
int (*fp)(int, int) = f;
int viaptr(void) { return fp(1, 2); }
";

/// The listing of each function, by name.
fn bodies(flags: &[&str]) -> Vec<(String, String)> {
    // The tests in this file run on threads of one process, so the process id alone would give
    // two of them the same directory and one could remove it while the other is still writing.
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rucc-rtd-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=i686-unknown-linux-gnu", "-std=gnu17", "-O2", "-fno-inline"])
        .args(["-fno-asynchronous-unwind-tables", "-S"])
        .args(flags)
        .args(["-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let listing = String::from_utf8(out.stdout).expect("a listing is text");
    let mut bodies: Vec<(String, String)> = Vec::new();
    for line in listing.lines() {
        if let Some(name) = line.strip_suffix(':').filter(|name| !name.starts_with('.')) {
            bodies.push((name.to_owned(), String::new()));
        } else if let Some((_, body)) = bodies.last_mut() {
            body.push_str(line);
            body.push('\n');
        }
    }
    bodies
}

fn body<'a>(bodies: &'a [(String, String)], name: &str) -> &'a str {
    bodies.iter().find(|(named, _)| named == name).map(|(_, body)| body.as_str()).expect(name)
}

/// The return of a function, which is `ret $n` when it pops `n` bytes.
fn ret(body: &str) -> &str {
    body.lines().map(str::trim).find(|line| line.starts_with("ret")).expect("a function returns")
}

/// A function that is not variadic pops the words it takes, and that includes an old style
/// definition and the hidden address of a returned structure. A variadic function and one
/// marked `cdecl` leave them to the caller.
#[test]
fn a_function_that_is_not_variadic_pops_its_arguments() {
    let bodies = bodies(&["-mrtd"]);
    for (name, pops) in [
        ("f", "ret\t$8"),
        ("g", "ret"),
        ("h", "ret"),
        ("k", "ret\t$8"),
        ("r", "ret\t$8"),
        ("v", "ret"),
        ("m", "ret\t$12"),
    ] {
        assert_eq!(ret(body(&bodies, name)), pops, "{name}: {bodies:?}");
    }
}

/// Without the flag every function leaves its arguments to the caller.
#[test]
fn without_the_flag_the_caller_pops() {
    let bodies = bodies(&[]);
    for name in ["f", "k", "m"] {
        assert_eq!(ret(body(&bodies, name)), "ret", "{name}: {bodies:?}");
    }
}

/// The caller knows the callee took its arguments, so after each call it moves the stack
/// pointer back down to where it keeps it. That holds for a function with no prototype, which
/// takes what the call passes, and for a call through a pointer.
#[test]
fn the_caller_counts_what_the_callee_popped() {
    let bodies = bodies(&["-mrtd"]);
    let call = body(&bodies, "call");
    let after: Vec<&str> = call
        .lines()
        .zip(call.lines().skip(1))
        .filter(|(line, _)| line.trim_start().starts_with("call\t") && !line.contains("get_pc"))
        .map(|(_, next)| next.trim())
        .collect();
    assert_eq!(after, ["subl\t$8, %esp", "subl\t$8, %esp", "subl\t$12, %esp"], "{call}");
    assert!(body(&bodies, "viaptr").contains("call\t*%eax\n\tsubl\t$8, %esp\n"), "{bodies:?}");
}

/// With `-mregparm=3`, as `boot2` has it, the arguments of each of these fit in registers, so
/// there is nothing on the stack for the callee to pop.
#[test]
fn registers_leave_nothing_to_pop() {
    let bodies = bodies(&["-mrtd", "-mregparm=3"]);
    for name in ["f", "k", "r", "m"] {
        assert_eq!(ret(body(&bodies, name)), "ret", "{name}: {bodies:?}");
    }
}
