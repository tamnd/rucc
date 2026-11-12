//! The order the objects of a unit are laid out in at `-O2`, which is gcc's.
//!
//! gcc lays them out newest first, and an object is as new as its entry in gcc's symbol table.
//! The entry is made at the first declaration that defines the object, so an `extern` one above
//! it does not count unless it names a section, and the entry for a `static` in a function body
//! is made when gcc analyzes the body, after it has read the whole file and in the order its walk
//! over the call graph reaches the body. The order is seen from outside where objects share a
//! section, as the NetBSD kernel's link sets do, and in the padding between objects of different
//! sizes.
//! tamnd/rucc#4338.
//!
//! Each order below is the one gcc 13 lays the source out in at `-O2`.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The target is written down rather than taken from the host, since the listing is read.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-object-order-{}-{what}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir.canonicalize().expect("the directory is there")
}

/// Whether the compiler finished, what it wrote on its standard output and what it said.
fn run(dir: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("the compiler is built before its own tests run");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The assembly of one source at `-O2`.
fn assembly(what: &str, source: &str) -> String {
    let dir = dir(what);
    std::fs::write(dir.join("a.c"), source).expect("the fixture can be written");
    let target = format!("--target={TARGET}");
    let (ok, out, said) = run(&dir, &[target.as_str(), "-O2", "-fno-pic", "-S", "-o", "-", "a.c"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(ok, "the source compiles:\n{said}");
    out
}

/// The names whose labels the listing has, in the order it has them. A `static` in a function
/// is labelled with its name, a dot and a number.
fn labels<'a>(text: &str, names: &[&'a str]) -> Vec<&'a str> {
    let mut found = Vec::new();
    for line in text.lines() {
        let Some(label) = line.strip_suffix(':') else { continue };
        let name = names.iter().copied().find(|&name| {
            label == name
                || label.strip_prefix(name).and_then(|rest| rest.strip_prefix('.')).is_some_and(
                    |number| !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()),
                )
        });
        if let Some(name) = name.filter(|name| !found.contains(name)) {
            found.push(name);
        }
    }
    found
}

/// An object declared `extern` above its definition is as new as the definition. This is the
/// shape of a NetBSD link set, where a header declares the entries and the file defines them.
#[test]
fn an_object_declared_extern_first_is_as_new_as_its_definition() {
    let source = "\
#define S __attribute__((section(\"set_x\"))) __attribute__((used))
extern unsigned e; extern _Bool c; extern _Bool d;
static int a S; static _Bool b S;
_Bool c S = 0; _Bool d S = 0; unsigned e S = 7;
int use_a(void) { return a; }
int use_b(void) { return b; }
";
    let text = assembly("extern", source);
    assert_eq!(labels(&text, &["a", "b", "c", "d", "e"]), ["e", "d", "c", "b", "a"], "{text}");
}

/// An object declared `extern` with a section above its definition is as old as that `extern`
/// declaration, since gcc makes the entry to keep the section in. This is the shape of a NetBSD
/// module hook, which a header declares `extern` and `__cacheline_aligned`. In `sys/net/if.c`
/// the hook `if_vlan_vlan_input_hook` then comes after `ifnet_mtx` in gcc's
/// `.data.cacheline_aligned`.
#[test]
fn an_object_declared_extern_with_a_section_first_is_as_old_as_that_declaration() {
    let source = "\
#define S __attribute__((section(\"set_x\")))
extern long h S; extern long g;
long m S = 1;
long h = 2;
long g S = 3;
long x S = 4;
";
    let text = assembly("extern-section", source);
    assert_eq!(labels(&text, &["h", "g", "m", "x"]), ["x", "g", "m", "h"], "{text}");
}

/// A `static` in a function body is newer than every object at file scope, whatever the line it
/// is written on, and the one in the body written later is the newer.
#[test]
fn a_static_in_a_body_is_newer_than_every_object_at_file_scope() {
    let source = "\
int x;
int y = 1;
int x = 2;
extern int e;
int usee(void) { return e; }
int z = 3;
int e = 4;
static int f(void);
int a = 5;
int callf(void) { return f(); }
static int f(void) { static int s = 6; return s++; }
int b = 7;
int g(void) { static int t = 8; return t++; }
int c = 9;
";
    let text = assembly("bodies", source);
    let names = ["x", "y", "z", "e", "a", "b", "c", "s", "t"];
    assert_eq!(labels(&text, &names), ["t", "s", "c", "b", "a", "e", "z", "y", "x"], "{text}");
}

/// The bodies are analyzed in the order of gcc's walk over the call graph: what the unit has to
/// emit first, the oldest first, then what each calls, and in front of that what it names
/// otherwise. In a body, the `static` named first is the oldest.
#[test]
fn the_statics_in_bodies_are_in_the_order_the_call_graph_reaches_them() {
    let source = "\
static int g2(void); static int g1(void);
int use(void) { return g1() + g2(); }
int k = 1;
int h(void) { static int s3 = 3; static int s4 = 4; return s3++ + s4++; }
static int g1(void) { static int s1 = 1; return s1++; }
int m = 2;
static int g2(void) { static int s2 = 2; return s2++; }
int h2(void) { static int s5 = 5; { static int s6 = 6; s6++; } return s5++; }
";
    let text = assembly("graph", source);
    let names = ["k", "m", "s1", "s2", "s3", "s4", "s5", "s6"];
    let order = ["s5", "s6", "s4", "s3", "s2", "s1", "m", "k"];
    assert_eq!(labels(&text, &names), order, "{text}");
    let source = "\
static int g1(void) { static int s1 = 1; return s1++; }
static int g2(void) { static int s2 = 2; return s2++; }
static int g3(void) { static int s3 = 3; return s3++; }
static int g4(void) { static int s4 = 4; return s4++; }
int u(void) { return g2() + g1(); }
int (*tab[])(void) = { g4, g3 };
";
    let text = assembly("table", source);
    let names = ["tab", "s1", "s2", "s3", "s4"];
    assert_eq!(labels(&text, &names), ["s4", "s3", "s1", "s2", "tab"], "{text}");
}

/// A zero filled `static` is `.local` and `.comm` in the listing, as gcc writes it, and the
/// assembler puts it after the globals in `.bss`. A global keeps its label.
#[test]
fn a_zero_filled_static_is_written_as_a_local_common() {
    let source = "\
int e1;
int f(void) { static int s; return s++ + e1; }
static int c;
int h(void) { return c++; }
";
    let text = assembly("local-common", source);
    assert!(text.contains("\t.local\tc\n\t.comm\tc,4,4\n"), "{text}");
    assert!(text.contains("\t.comm\ts."), "{text}");
    assert_eq!(labels(&text, &["e1", "s", "c"]), ["e1"]);
}
