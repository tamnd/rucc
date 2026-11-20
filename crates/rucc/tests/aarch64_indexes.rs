//! An array subscript on AArch64 shifted and widened inside the load or store, as gcc writes it.
//!
//! `arr[i]` with an `int` subscript is the subscript sign extended to 64 bits, shifted by the size
//! of the element and added to the address. The access has room for all three, so gcc writes
//! `ldr x0, [x1, w0, sxtw #3]`. Before this the extension and the shift were two instructions in
//! front of it, on every array read and write the kernel does with an `int` or `unsigned` index.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const SOURCE: &str = "\
extern long arr[];
extern unsigned char bytes[];
long at(int i) { return arr[i]; }
long wide(long i) { return arr[i]; }
int word(int *p, unsigned i) { return p[i]; }
void put(long *p, int i, long v) { p[i] = v; }
int byte(int i) { return bytes[i]; }
double real(double *p, int i) { return p[i]; }
void bump(long *p, int i) { p[i] += 7; }
long both(long *p, int i) { return p[i] + i; }
long walk(long *p, int i, long n) {
  long s = p[i], j = i;
  while (s < n) { s += j; j += 2; }
  return s;
}
";

fn listing() -> String {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rucc-a64-indexes-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=aarch64-unknown-linux-gnu", "-O2", "-fno-pic", "-S", "-o", "-"])
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
fn the_shift_and_the_widening_go_into_the_access() {
    let text = listing();
    for (name, access) in [
        ("at", "ldr x0, [x1, w0, sxtw #3]"),
        ("wide", "ldr x0, [x1, x0, lsl #3]"),
        ("word", "ldr w0, [x0, w1, uxtw #2]"),
        ("put", "str x2, [x0, w1, sxtw #3]"),
        ("byte", "ldrb w0, [x1, w0, sxtw]"),
        ("real", "ldr d0, [x0, w1, sxtw #3]"),
    ] {
        let body = body(&text, name);
        assert!(body.iter().any(|line| line == access), "{name} {access}:\n{text}");
        for gone in ["sxtw x", "mov w", "lsl x"] {
            assert!(!body.iter().any(|line| line.starts_with(gone)), "{name} {gone}:\n{text}");
        }
    }
}

/// A load and a store of one element share the index, and both take it.
#[test]
fn every_access_that_reads_the_index_takes_it() {
    let text = listing();
    let bump = body(&text, "bump");
    let widened = bump.iter().filter(|line| line.ends_with(", sxtw #3]")).count();
    assert_eq!(widened, 2, "{text}");
}

/// The widened subscript is wanted as a number as well, so its `sxtw` stays, and the access still
/// takes the shift.
#[test]
fn an_index_something_else_reads_keeps_its_widening() {
    let text = listing();
    let both = body(&text, "both");
    assert!(both.iter().any(|line| line.starts_with("sxtw x")), "{text}");
    assert!(both.iter().any(|line| line.ends_with(", lsl #3]")), "{text}");
}

/// The widened subscript is where a loop starts counting, which an edge carries into the loop and
/// no instruction reads. It is wanted as a number all the same, so its `sxtw` stays. With it gone
/// the loop's counter started from nothing and the allocator refused the function.
#[test]
fn an_index_carried_into_a_loop_keeps_its_widening() {
    let text = listing();
    let walk = body(&text, "walk");
    assert!(walk.iter().any(|line| line.starts_with("sxtw x")), "{text}");
    assert!(walk.iter().any(|line| line.ends_with(", lsl #3]")), "{text}");
}
