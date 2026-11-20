//! A `long long` shifted by a number on i386, which is the two halves moved by that much. The
//! kernel takes a `u64` apart with `>> 32` everywhere, and that should be the high half and
//! nothing else, not the run time choice a shift by a variable needs.

use std::process::Command;

const SOURCE: &str = "\
unsigned hi(unsigned long long x) { return x >> 32; }
unsigned long long shl(unsigned long long x) { return x << 40; }
unsigned long long lsr(unsigned long long x) { return x >> 5; }
long long asr(long long x) { return x >> 47; }
unsigned long long any(unsigned long long x, int k) { return x << k; }
";

fn listing(level: &str) -> String {
    let dir = std::env::temp_dir().join(format!("rucc-i386-shift-{}-{level}", std::process::id()));
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
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("a listing is text")
}

/// The body of one function, from its label to the next one's.
fn body<'a>(listing: &'a str, name: &str) -> &'a str {
    let start = listing.find(&format!("\n{name}:\n")).expect(name);
    let rest = &listing[start + 1..];
    let end = rest.find("\t.size").unwrap_or(rest.len());
    &rest[..end]
}

#[test]
fn a_shift_by_a_number_chooses_nothing_at_run_time() {
    for level in ["-O0", "-O2"] {
        let listing = listing(level);
        for name in ["hi", "shl", "lsr", "asr"] {
            let body = body(&listing, name);
            assert!(!body.contains("cmov") && !body.contains("%cl"), "{level} {name}: {body}");
        }
        let hi = body(&listing, "hi");
        assert!(!hi.contains("shr") && !hi.contains("sar"), "{level}: {hi}");
        let any = body(&listing, "any");
        assert!(any.contains("%cl"), "{level}: {any}");
    }
}
