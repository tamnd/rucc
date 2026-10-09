//! Counting arcs with `-fprofile-arcs` on a target whose pointers are thirty two bits.
//!
//! Every counter after the first is found by adding its offset to the array, and that offset was a
//! number in sixty four bits whatever the target. On i686 the step that splits sixty four bit
//! numbers in two does not know a `ptr_add` of one, so it left the function whole and the backend
//! stopped with "no rule lowers `load.i64`". Most of the kernel's i386 allmodconfig units that
//! failed did so this way, in `jhash2`, `signal_pending` and the like.

use std::process::Command;

/// A loop, whose counters are past the first one in the array.
const LOOP: &str = r#"
unsigned f(const unsigned *k, unsigned length) {
    unsigned c = 0;
    while (length > 3) {
        c += k[0];
        length -= 3;
        k += 3;
    }
    return c;
}
"#;

/// A `switch` whose cases fall through, which is the shape of `jhash2`.
const SWITCH: &str = r#"
unsigned f(const unsigned *k, unsigned length) {
    unsigned c = 0;
    switch (length) {
    case 3: c += k[2];
    case 2: c += k[1];
    case 1: c += k[0]; break;
    case 0: break;
    }
    return c;
}
"#;

fn assembly(name: &str, source: &str, level: &str) -> String {
    let dir =
        std::env::temp_dir().join(format!("rucc-arcs-i686-{}-{name}{level}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=i686-unknown-linux-gnu", level, "-fno-pic", "-fprofile-arcs"])
        .args(["-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{name} {level}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("assembly is text")
}

#[test]
fn a_function_with_counters_compiles_for_i686() {
    for level in ["-O0", "-O1", "-O2", "-Os", "-O3"] {
        assembly("loop", LOOP, level);
        assembly("switch", SWITCH, level);
    }
}

#[test]
fn a_counter_is_added_to_in_two_halves() {
    let text = assembly("halves", LOOP, "-O2");
    assert!(text.contains("8(%eax)"), "{text}");
    assert!(text.contains("12(%eax)"), "{text}");
    assert!(!text.contains("%rax"), "{text}");
}
