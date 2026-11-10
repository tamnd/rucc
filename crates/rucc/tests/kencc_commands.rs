//! The rules of kencc that the 9front commands need, under `-fkencc`, end to end.
//!
//! - A run of commas in an enum is one comma, as `enum ','` in `cc.y` reads it. nusb's asix.c
//!   has `= 0x04, /* ... */,`.
//! - The names of a prototype are not checked. mothra's panel.h has `Panel *pl` two times in
//!   one prototype.
//! - A record given where the type of one of its unnamed members is wanted gives that member,
//!   as `typeext` in `cc/sub.c` does. acid has `*s = *res` with a `Store` and a `Node`.
//! - An array that is initialized with no braces takes the value in its first element. ppp.c
//!   has `Block b[1] = *allocb(Buflen);`.
//! - A definition with `()` takes no parameters, whatever the prototype said. games/mandel.c
//!   defines `void threadmain()` after `void threadmain(int, char *[])`.

use std::path::PathBuf;
use std::process::Command;

const SOURCE: &str = "enum { A = 4, /* note */, B, };\n\
                      void menu(int *pl, int, int *pl);\n\
                      typedef struct Store { int fmt; int ival; } Store;\n\
                      typedef struct Node { char op; Store; } Node;\n\
                      void copy(Store *s, Node *n) { *s = *n; }\n\
                      Store take(Node *n) { return *n; }\n\
                      Node *alloc(void);\n\
                      int first(void) { Store b[1] = *alloc(); return b[0].ival; }\n\
                      void threadmain(int argc, char *argv[]);\n\
                      void threadmain() { }\n\
                      int c = B;\n";

fn fixture(what: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("rucc-kencc-commands-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    path
}

/// Whether the compiler took the source, the assembly, and what it said.
fn compile(what: &str, flags: &[&str]) -> (bool, String, String) {
    let path = fixture(what);
    let out = path.with_extension("s");
    let run = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-plan9", "-O2", "-w", "-S", "-o"])
        .arg(&out)
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let asm = std::fs::read_to_string(&out).unwrap_or_default();
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    (run.status.success(), asm, String::from_utf8_lossy(&run.stderr).into_owned())
}

/// The lines of the function `name` in `asm`.
fn body<'a>(asm: &'a str, name: &str) -> Vec<&'a str> {
    let label = format!("{name}:");
    asm.lines().skip_while(|l| *l != label).take_while(|l| !l.starts_with("\t.size")).collect()
}

#[test]
fn kencc_takes_what_the_commands_write() {
    let (ok, asm, err) = compile("kencc", &["-fkencc"]);
    assert!(ok, "the compiler refused the source:\n{err}");
    // B is 5, after the empty enumerator.
    let c = asm.lines().skip_while(|l| *l != "c:").nth(1).unwrap_or("");
    assert_eq!(c.trim(), ".long\t5");
    // The `Store` in a `Node` is at 4, after the `char`, and the copy reads it from there.
    let copy = body(&asm, "copy").join("\n");
    assert!(copy.contains("4(%"), "{copy}");
    // The first element of the array is the `Store` of the node, so `ival` is at 8.
    let first = body(&asm, "first").join("\n");
    assert!(first.contains("8(%rax)"), "{first}");
    assert!(!body(&asm, "threadmain").is_empty());
}

#[test]
fn c_refuses_them() {
    let (ok, _, err) = compile("c", &[]);
    assert!(!ok);
    assert!(err.contains("expected an identifier, found `,`"), "{err}");
}
