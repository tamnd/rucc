//! A variable on AArch64 read with `adrp` and the low twelve bits of its address folded into the
//! load or store, as gcc writes it.
//!
//! Before this every read of a variable was `adrp`, then `add` of the low bits, then the access
//! itself. The access has an immediate offset of its own and the relocation for it exists, so the
//! `add` is one instruction more than needed on every global the kernel touches. The fold only
//! happens where the alignment of the variable says the access cannot cross into the next page,
//! and an address that is wanted as an address keeps its `add`.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const SOURCE: &str = "\
extern int g;
static int s;
extern long arr[];
struct pt { int x, y; long z; };
extern struct pt p;
struct pt q;
extern char c;
extern short h;
extern double d;
int read(void) { return s + g; }
void write(int v) { g = v; s = v; }
long third(void) { return arr[3]; }
void member(int v) { q.y = v; }
int narrow(void) { return c + h + (int)d; }
long fields(void) { return p.x + p.y + p.z; }
int *address(void) { return &g; }
";

fn listing(flags: &[&str]) -> String {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rucc-a64-low-bits-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=aarch64-unknown-linux-gnu", "-O2", "-S", "-o", "-"])
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("a listing is text")
}

/// The instructions of one function, without its labels and directives.
fn body(text: &str, name: &str) -> Vec<String> {
    text.lines()
        .skip_while(|line| *line != format!("{name}:"))
        .skip(1)
        .take_while(|line| !line.starts_with("\t.size"))
        .filter(|line| line.starts_with('\t') && !line.starts_with("\t."))
        .map(|line| line.trim().to_string())
        .collect()
}

#[test]
fn a_read_or_write_of_a_variable_folds_the_low_bits_into_the_access() {
    let text = listing(&["-fno-pic"]);
    for (name, folded) in [
        ("read", &[":lo12:s]", ":lo12:g]"][..]),
        ("write", &[":lo12:g]", ":lo12:s]"]),
        ("third", &[":lo12:arr+24]"]),
        ("member", &[":lo12:q+4]"]),
        ("narrow", &[":lo12:c]", ":lo12:h]", ":lo12:d]"]),
    ] {
        let body = body(&text, name);
        assert!(!body.is_empty(), "{name}:\n{text}");
        assert!(!body.iter().any(|line| line.starts_with("add x")), "{name}:\n{text}");
        for want in folded {
            assert!(body.iter().any(|line| line.ends_with(want)), "{name} {want}:\n{text}");
        }
    }
}

/// The page of `arr+24` is the one `adrp` has to name, since `arr` may sit near the end of one.
#[test]
fn the_page_is_the_one_the_access_lands_in() {
    let text = listing(&["-fno-pic"]);
    assert!(body(&text, "third").contains(&"adrp x0, arr+24".to_string()), "{text}");
    assert!(body(&text, "member").iter().any(|line| line.ends_with(", q+4")), "{text}");
}

/// A record aligned to eight with a member at eight may straddle a page, so its members are read
/// from the address. So is a variable whose address is the answer.
#[test]
fn what_could_cross_a_page_keeps_its_add() {
    let text = listing(&["-fno-pic"]);
    for name in ["fields", "address"] {
        let body = body(&text, name);
        assert!(body.iter().any(|line| line.starts_with("add x")), "{name}:\n{text}");
    }
}

/// A static in a position independent executable is near too, so it folds the same way, and a
/// variable from elsewhere is still read out of the table.
#[test]
fn a_position_independent_executable_folds_its_own_variables() {
    let text = listing(&[]);
    let read = body(&text, "read");
    assert!(read.iter().any(|line| line.ends_with(":lo12:s]")), "{text}");
    assert!(read.iter().any(|line| line.contains(":got:g")), "{text}");
}
