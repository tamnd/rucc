//! `-fprofile-update=atomic` and `prefer-atomic` add to the `-fprofile-arcs` counters atomically.
//!
//! The kernel passes `prefer-atomic` with its gcov flags whenever the probe in its Makefile finds
//! the flag adds no new undefined symbol, which on x86 it never does, so gcc's counters there are
//! `lock`ed adds. rucc checked the flag and dropped it, and two CPUs counting the same edge could
//! lose counts. Where a pointer is thirty two bits gcc falls back to the load and the store, and so
//! does rucc.

use std::process::Command;

const SOURCE: &str = "long long x;\nvoid f(void) { x++; }\n";

fn assembly(name: &str, target: &str, update: &str) -> String {
    let dir = std::env::temp_dir().join(format!("rucc-arcs-atomic-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args([target, "-O2", "-fno-pic", "-fprofile-arcs", update, "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{name}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("assembly is text")
}

#[test]
fn the_counters_are_added_to_atomically_when_asked() {
    for update in ["-fprofile-update=atomic", "-fprofile-update=prefer-atomic"] {
        let x86 = assembly("x86", "--target=x86_64-unknown-linux-gnu", update);
        assert!(x86.contains("lock"), "{update} on x86_64 has no locked add:\n{x86}");
        let a64 = assembly("a64", "--target=aarch64-unknown-linux-gnu", update);
        let exclusive = ["ldxr", "ldaxr", "ldadd"].iter().any(|op| a64.contains(op));
        assert!(exclusive, "{update} on AArch64 has no atomic add:\n{a64}");
        let i686 = assembly("i686", "--target=i686-unknown-linux-gnu", update);
        assert!(!i686.contains("lock"), "{update} on i686 falls back to single:\n{i686}");
    }
    let single = assembly("single", "--target=x86_64-unknown-linux-gnu", "-fprofile-update=single");
    assert!(!single.contains("lock"), "single is a load and a store:\n{single}");
}
