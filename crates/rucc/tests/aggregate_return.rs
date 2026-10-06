//! `__attribute__((callee_pop_aggregate_return(n)))`, end to end: who takes the address of a
//! returned structure off the stack at both ends of a call on 32-bit x86, and what gcc says about
//! the attribute where it means nothing, in its words.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const LINUX: &str = "i686-unknown-linux-gnu";
const MINGW: &str = "i686-w64-windows-gnu";
const X86_64: &str = "x86_64-unknown-linux-gnu";

const SOURCE: &str = "\
struct big { int a[4]; };
struct big plain(int x) { struct big b = {{x, x, x, x}}; return b; }
__attribute__((callee_pop_aggregate_return(0))) struct big kept(int x)
{ struct big b = {{x, x, x, x}}; return b; }
__attribute__((callee_pop_aggregate_return(1))) struct big popped(int x)
{ struct big b = {{x, x, x, x}}; return b; }
struct big later(int x) __attribute__((callee_pop_aggregate_return(0)));
struct big later(int x) { struct big b = {{x, x, x, x}}; return b; }
__attribute__((stdcall)) struct big called(int x) { struct big b = {{x, x, x, x}}; return b; }
__attribute__((stdcall, callee_pop_aggregate_return(0))) struct big called_kept(int x)
{ struct big b = {{x, x, x, x}}; return b; }
typedef struct big (*kept_t)(int) __attribute__((callee_pop_aggregate_return(0)));
typedef struct big (*popped_t)(int) __attribute__((callee_pop_aggregate_return(1)));
int call_plain(struct big (*f)(int)) { return f(1).a[0]; }
int call_kept(kept_t f) { return f(1).a[0]; }
int call_popped(popped_t f) { return f(1).a[0]; }
";

/// A directory of this test's own, empty. The tests in this file run on threads of one process,
/// so the process id alone would give two of them the same one.
fn dir() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rucc-aggregate-return-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir
}

fn run(dir: &Path, target: &str, flags: &[&str], source: &str) -> (bool, String, String) {
    std::fs::write(dir.join("a.c"), source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={target}"))
        .args(["-O2", "-fno-asynchronous-unwind-tables", "-S", "-o", "-"])
        .args(flags)
        .arg("a.c")
        .current_dir(dir)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(dir);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    (out.status.success(), stdout, String::from_utf8_lossy(&out.stderr).into_owned())
}

/// The listing of each function, by name.
fn bodies(target: &str, flags: &[&str]) -> Vec<(String, String)> {
    let (ok, listing, err) = run(&dir(), target, flags, SOURCE);
    assert!(ok, "{err}");
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

/// The body of the function of that name, decorated or not.
fn body<'a>(bodies: &'a [(String, String)], name: &str) -> &'a str {
    let named = |named: &str| {
        let named = named.strip_prefix('_').unwrap_or(named);
        named == name || named.split('@').next() == Some(name)
    };
    bodies.iter().find(|(n, _)| named(n)).map(|(_, body)| body.as_str()).expect(name)
}

/// What a `ret` in that body takes off the stack, as written after it.
fn rets(body: &str) -> Vec<&str> {
    body.lines().filter_map(|line| line.trim().strip_prefix("ret")).map(str::trim).collect()
}

/// How many times a caller puts back the word a callee took off the stack.
fn put_back(body: &str) -> usize {
    body.matches("subl\t$4, %esp").count()
}

/// i386 Linux has the callee pop the address and Windows has the caller, and the attribute turns
/// either into the other at both ends of a call. Under `regparm` the address is in `eax` and there
/// is nothing to pop, and `stdcall` pops it with the rest whatever the attribute says.
#[test]
fn the_attribute_says_who_pops_a_returned_address() {
    let linux = bodies(LINUX, &[]);
    assert_eq!(rets(body(&linux, "plain")), ["$4"], "{linux:?}");
    assert_eq!(rets(body(&linux, "kept")), [""], "{linux:?}");
    assert_eq!(rets(body(&linux, "popped")), ["$4"], "{linux:?}");
    assert_eq!(rets(body(&linux, "later")), [""], "a redeclaration keeps it: {linux:?}");
    assert_eq!(rets(body(&linux, "called_kept")), rets(body(&linux, "called")), "{linux:?}");
    // A caller of a function that pops puts the word back, and one of a function that keeps it
    // has nothing to put back.
    let (plain, kept) = (body(&linux, "call_plain"), body(&linux, "call_kept"));
    assert_eq!(put_back(plain), put_back(kept) + 1, "{plain}\n{kept}");
    assert_eq!(put_back(body(&linux, "call_popped")), put_back(plain), "{linux:?}");

    let mingw = bodies(MINGW, &[]);
    assert_eq!(rets(body(&mingw, "plain")), [""], "{mingw:?}");
    assert_eq!(rets(body(&mingw, "kept")), [""], "{mingw:?}");
    assert_eq!(rets(body(&mingw, "popped")), ["$4"], "{mingw:?}");
    let (plain, popped) = (body(&mingw, "call_plain"), body(&mingw, "call_popped"));
    assert_eq!(put_back(popped), put_back(plain) + 1, "{plain}\n{popped}");

    let regparm = bodies(LINUX, &["-mregparm=3"]);
    for name in ["plain", "kept", "popped"] {
        assert_eq!(rets(body(&regparm, name)), [""], "{name}: {regparm:?}");
    }
}

/// Where gcc takes it without a word: before the declarator and after it, in C23's brackets, on a
/// typedef, a pointer, a member and a parameter, with an enumerator, and on a function that
/// returns nothing in memory, where it means nothing.
#[test]
fn the_attribute_is_taken_where_gcc_takes_it() {
    let source = "struct big { int a[4]; };\n\
        enum { ONE = 1 };\n\
        __attribute__((callee_pop_aggregate_return(1))) struct big a(void);\n\
        struct big b(void) __attribute__((__callee_pop_aggregate_return__(0)));\n\
        [[gnu::callee_pop_aggregate_return(ONE)]] struct big c(void);\n\
        typedef struct big f_t(void) __attribute__((callee_pop_aggregate_return(0)));\n\
        struct big (*p)(void) __attribute__((callee_pop_aggregate_return(0)));\n\
        struct s { struct big (*m)(void) __attribute__((callee_pop_aggregate_return(1))); };\n\
        void k(struct big (*q)(void) __attribute__((callee_pop_aggregate_return(0))));\n\
        int d(void) __attribute__((callee_pop_aggregate_return(1)));\n\
        _Static_assert(__has_attribute(callee_pop_aggregate_return), \"gcc 4.6\");\n";
    for target in [LINUX, MINGW] {
        let (ok, _, err) = run(&dir(), target, &["-Wall", "-Wextra", "-std=gnu23"], source);
        assert!(ok, "{target}: {err}");
        assert_eq!(err, "", "{target}");
    }
}

/// What gcc says where the attribute means nothing, and that it only warns there.
#[test]
fn callee_pop_aggregate_return_is_checked_in_gcc_s_words() {
    let name = "'callee_pop_aggregate_return' attribute";
    let constant = format!("{name} requires an integer constant argument");
    let one = format!("argument to {name} is neither zero, nor one");
    let types = format!("{name} only applies to function types");
    let wrong = format!("error: wrong number of arguments specified for {name}");
    let decl = |arg: &str| {
        format!(
            "struct big {{ int a[4]; }};\nint n;\n\
             __attribute__((callee_pop_aggregate_return{arg})) struct big f(void);\n"
        )
    };
    for (target, source, said, errors, warnings) in [
        (LINUX, decl("(2)"), one.clone(), 0, 1),
        (LINUX, decl("(-1)"), one.clone(), 0, 1),
        (LINUX, decl("(1.0)"), constant.clone(), 0, 1),
        (LINUX, decl("(\"1\")"), constant.clone(), 0, 1),
        (LINUX, decl("(n)"), constant.clone(), 0, 1),
        (LINUX, decl("(nope)"), "error: 'nope' undeclared here (not in a function)".to_owned(), 1, 1),
        (LINUX, decl("(nope)"), constant.clone(), 1, 1),
        (LINUX, decl(""), wrong.clone(), 1, 0),
        (LINUX, decl("(0, 1)"), "expected 1, found 2".to_owned(), 1, 0),
        (LINUX, "int v __attribute__((callee_pop_aggregate_return(1)));\n".to_owned(), types.clone(), 0, 1),
        (LINUX, "int v __attribute__((callee_pop_aggregate_return(5)));\n".to_owned(), types.clone(), 0, 1),
        (X86_64, decl("(1)"), format!("{name} only available for 32-bit"), 0, 1),
        (X86_64, decl("(5)"), format!("{name} only available for 32-bit"), 0, 1),
    ] {
        let (ok, _, err) = run(&dir(), target, &[], &source);
        assert_eq!(ok, errors == 0, "{source}\n{err}");
        assert!(err.contains(&said), "{source}\nwanted {said:?}, got:\n{err}");
        assert_eq!(err.matches("error:").count(), errors, "{source}\n{err}");
        assert_eq!(err.matches("warning:").count(), warnings, "{source}\n{err}");
    }
    // gcc 4.5 has never heard of it.
    let (ok, _, err) = run(&dir(), LINUX, &["-fgnuc-version=4.5.0"], &decl("(5)"));
    assert!(ok, "{err}");
    assert!(err.contains(&format!("warning: {name} directive ignored")), "{err}");
    assert!(!err.contains("zero"), "{err}");
}
