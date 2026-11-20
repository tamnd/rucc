//! A `char`, `short` or `int` read and widened on AArch64 with the one load that does both, as gcc
//! writes it.
//!
//! `ldrb` and `ldrh` already clear every bit above what they read, and `ldrsb`, `ldrsh` and `ldrsw`
//! sign extend on the way in. Before this every narrow read was the plain load followed by a
//! `uxtb`, `sxth` or `sxtw` of what it read, which is one instruction more on every `char` and
//! `short` field the kernel reads into an `int`.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const SOURCE: &str = "\
extern unsigned char uc;
extern signed char sc;
extern unsigned short uh;
extern short sh;
extern int iw;
extern unsigned uw;
int zero_byte(void) { return uc; }
long sign_byte(void) { return sc; }
long zero_half(void) { return uh; }
int sign_half(void) { return sh; }
long sign_word(void) { return iw; }
unsigned long zero_word(void) { return uw; }
int indexed(signed char *p, int i) { return p[i]; }
";

fn listing() -> String {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rucc-a64-widening-{}-{n}", std::process::id()));
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
fn a_narrow_read_is_widened_by_the_load() {
    let text = listing();
    for (name, load) in [
        ("zero_byte", "ldrb w0, [x0, :lo12:uc]"),
        ("sign_byte", "ldrsb x0, [x0, :lo12:sc]"),
        ("zero_half", "ldrh w0, [x0, :lo12:uh]"),
        ("sign_half", "ldrsh w0, [x0, :lo12:sh]"),
        ("sign_word", "ldrsw x0, [x0, :lo12:iw]"),
        ("zero_word", "ldr w0, [x0, :lo12:uw]"),
        ("indexed", "ldrsb w0, [x0, w1, sxtw]"),
    ] {
        let body = body(&text, name);
        let tail = [load.to_string(), "ret".to_string()];
        assert!(body.ends_with(&tail), "{name}:\n{text}");
        assert!(body.len() <= 3, "{name}:\n{text}");
    }
}
