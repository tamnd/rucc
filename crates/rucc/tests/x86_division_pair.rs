//! The quotient and the remainder of one division from the one `div` gcc writes, which leaves the
//! quotient in `eax` and the remainder in `edx`. Each used to be an instruction of its own, so a
//! loop that prints a number a digit at a time divided twice for every digit. FreeBSD's `boot2`
//! prints that way at `-Os`, where a division by ten stays a `div`.

use std::process::Command;

const SOURCE: &str = "\
char *digits(char *p, unsigned u) { do { *p++ = '0' + u % 10; } while (u /= 10); return p; }
int both(int a, int b, int *r) { *r = a % b; return a / b; }
unsigned remainder_first(unsigned a, unsigned b) { unsigned r = a % b; return r * (a / b); }
long wide(long a, long b) { return a / b + a % b; }
unsigned short half(unsigned short a, unsigned short b) { return (unsigned short)(a / b) * (unsigned short)(a % b); }
";

fn assembly(name: &str, target: &str, level: &str) -> String {
    let dir =
        std::env::temp_dir().join(format!("rucc-division-pair-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args([&format!("--target={target}"), "-fno-pic", level, "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{name}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("assembly is text")
}

/// The body of one function in the assembly.
fn body<'a>(text: &'a str, name: &str) -> &'a str {
    let start = text.find(&format!("\n{name}:")).expect("the function is in the assembly");
    let rest = &text[start + 1..];
    &rest[..rest.find("\n\t.size").unwrap_or(rest.len())]
}

fn divisions(body: &str) -> usize {
    let division = |line: &&str| line.starts_with("div") || line.starts_with("idiv");
    body.lines().map(str::trim_start).filter(division).count()
}

#[test]
fn a_digit_and_what_is_left_are_one_division_at_os() {
    for target in ["i686-unknown-linux-gnu", "x86_64-unknown-linux-gnu"] {
        let text = assembly("os", target, "-Os");
        for name in ["digits", "both", "remainder_first", "wide", "half"] {
            let body = body(&text, name);
            assert_eq!(divisions(body), 1, "{target} {name}\n{body}");
        }
    }
}

#[test]
fn a_division_by_a_register_is_one_division_at_o2() {
    let text = assembly("o2", "x86_64-unknown-linux-gnu", "-O2");
    for name in ["both", "remainder_first", "wide"] {
        let body = body(&text, name);
        assert_eq!(divisions(body), 1, "{name}\n{body}");
    }
    // A division by ten is a multiply there, and nothing is left to share.
    assert_eq!(divisions(body(&text, "digits")), 0);
}
