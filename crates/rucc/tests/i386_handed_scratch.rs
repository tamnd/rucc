//! `esi` and `edi` handed out on i386 where nothing needs them for a reload, the way x86-64 hands
//! out `r10` and `r11`. Both used to be held back in every function, which left four registers
//! where gcc has six, and a loop with three sums, a count and its end kept two of them on the
//! stack. Neither has a low byte, so a value an instruction names the byte of is kept out of both,
//! and the byte a comparison writes for its branch is not one of those.

use std::process::Command;

const SOURCE: &str = "\
unsigned mix(const unsigned *p, unsigned n) {
    unsigned a = p[0], b = p[1], c = p[2];
    for (unsigned i = 0; i < n; i++) { a += b ^ i; b += c; c += a; }
    return a ^ b ^ c;
}
unsigned count(const unsigned char *p, const unsigned char *e, unsigned char k) {
    unsigned a = 0, b = 0;
    for (; p != e; p++) { a += *p == k; b += *p < k; }
    return a ^ b;
}
";

fn listing(level: &str) -> String {
    let dir = std::env::temp_dir()
        .join(format!("rucc-i386-handed-scratch-{}-{level}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=i686-unknown-linux-gnu", level, "-fno-pic"])
        .args(["-fno-asynchronous-unwind-tables", "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("a listing is text")
}

fn body(listing: &str, name: &str) -> String {
    let start = listing.find(&format!("\n{name}:\n")).expect(name);
    let rest = &listing[start + name.len() + 3..];
    let end = rest.find("\n\t.size").unwrap_or(rest.len());
    rest[..end].to_owned()
}

#[test]
fn six_values_in_a_loop_are_all_in_registers() {
    for level in ["-O2", "-Os"] {
        let mix = body(&listing(level), "mix");
        assert!(mix.contains("%esi") && mix.contains("%edi"), "{level}:\n{mix}");
        let stored = mix.lines().filter(|line| line.trim_end().ends_with("(%esp)")).count();
        assert_eq!(stored, 0, "{level}:\n{mix}");
    }
}

#[test]
fn a_value_named_as_a_byte_is_not_in_esi_or_edi() {
    for level in ["-O2", "-Os"] {
        let count = body(&listing(level), "count");
        assert!(!count.contains("xchgl"), "{level}:\n{count}");
        assert_eq!(count.matches("\tcmpb\t").count(), 2, "{level}:\n{count}");
    }
}
