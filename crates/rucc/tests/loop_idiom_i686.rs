//! A loop that becomes a `memset` or a `memcpy` on a target whose pointers are thirty two bits.
//!
//! Loop idiom recognition built the start of the call and a length that is a number in sixty four
//! bits whatever the target, so on i686 the start was a `ptr_add` of a sixty four bit offset and
//! the backend stopped with "no rule lowers `trunc.i64.i32`". The kernel's perf uncore driver and
//! ohci-hcd were two of the units that showed it. Both are now as wide as a pointer. A length the
//! loop works out is still sixty four bits, and the step that splits those on i386 takes its low
//! half, as `i386_fill_length.rs` shows.

use std::process::Command;

/// A fill of a member that does not start the record, so the start is an offset as well.
const FILL: &str = r#"
struct map { int seg; int pbus[256]; };
void fill(struct map *m) {
    for (int i = 0; i < 256; i++)
        m->pbus[i] = -1;
}
"#;

/// A copy between two pointers that do not overlap.
const COPY: &str = r#"
void copy(int *restrict to, const int *restrict from) {
    for (int i = 0; i < 64; i++)
        to[i] = from[i];
}
"#;

/// Counts that are not numbers, a narrow one, a `long long` one that stays a loop, and a walk that
/// starts at an index the caller hands in.
const COUNTED: &str = r#"
void some(short *p, unsigned char n) {
    for (int i = 0; i < n; i++)
        p[i] = 0;
}
void wide(char *p, long long n) {
    for (long long i = 0; i < n; i++)
        p[i] = 0;
}
void row(char *restrict to, const char *restrict from, unsigned n, int k) {
    for (unsigned i = 0; i < n; i++)
        to[k + i] = from[k + i];
}
"#;

fn assembly(name: &str, source: &str, level: &str) -> String {
    let dir =
        std::env::temp_dir().join(format!("rucc-idiom-i686-{}-{name}{level}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=i686-unknown-linux-gnu", level, "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{name} {level}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("assembly is text")
}

#[test]
fn a_loop_that_is_a_library_call_compiles_for_i686() {
    for level in ["-O1", "-O2", "-Os", "-O3"] {
        assembly("fill", FILL, level);
        assembly("copy", COPY, level);
        assembly("counted", COUNTED, level);
    }
}

#[test]
fn the_call_is_handed_a_length_in_thirty_two_bits() {
    let fill = assembly("fill", FILL, "-O2");
    assert!(fill.contains("memset"), "{fill}");
    assert!(fill.contains("$1024"), "{fill}");
    let copy = assembly("copy", COPY, "-O2");
    assert!(copy.contains("memcpy"), "{copy}");
    assert!(copy.contains("$256"), "{copy}");
    let counted = assembly("counted", COUNTED, "-O2");
    assert!(counted.contains("memset"), "{counted}");
    assert!(counted.contains("memcpy"), "{counted}");
}
