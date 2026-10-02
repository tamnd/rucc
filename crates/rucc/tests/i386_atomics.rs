//! A `long long` that `__atomic` or `__sync` touches on i386, which is `lock cmpxchg8b` there and
//! used to be refused with "no rule lowers a `cmpxchg` producing a `i64`". gcc writes the same
//! instruction for every one of these on a Pentium or later and calls nothing.

use std::process::Command;

const SOURCE: &str = "\
typedef long long s64;
s64 swap(s64 *p, s64 o, s64 n) { return __sync_val_compare_and_swap(p, o, n); }
int strong(s64 *p, s64 *o, s64 n) { return __atomic_compare_exchange_n(p, o, n, 0, 5, 5); }
s64 load(s64 *p) { return __atomic_load_n(p, 5); }
void store(s64 *p, s64 v) { __atomic_store_n(p, v, 5); }
s64 exchange(s64 *p, s64 v) { return __atomic_exchange_n(p, v, 5); }
s64 add(s64 *p, s64 v) { return __atomic_fetch_add(p, v, 5); }
s64 nand(s64 *p, s64 v) { return __atomic_fetch_nand(p, v, 5); }
";

fn listing(level: &str, pic: &str) -> String {
    let dir =
        std::env::temp_dir().join(format!("rucc-i386-atomics-{}-{level}{pic}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=i686-unknown-linux-gnu", level, pic, "-fno-asynchronous-unwind-tables"])
        .args(["-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("a listing is text")
}

/// Each one is the locked compare and exchange of eight bytes and no call, at both levels and
/// whether or not `ebx` is holding the address of the global offset table.
#[test]
fn every_eight_byte_atomic_is_cmpxchg8b() {
    for level in ["-O0", "-O2"] {
        for pic in ["-fno-pic", "-fpic"] {
            let listing = listing(level, pic);
            let body = |name: &str| {
                let start = listing.find(&format!("\n{name}:\n")).expect(name);
                let rest = &listing[start + name.len() + 3..];
                let end = rest.find("\n\t.size").unwrap_or(rest.len());
                rest[..end].to_owned()
            };
            for name in ["swap", "strong", "load", "store", "exchange", "add", "nand"] {
                let body = body(name);
                assert!(body.contains("lock; cmpxchg8b"), "{level} {pic} {name}: {body}");
                assert!(!body.contains("call\t__"), "{level} {pic} {name}: {body}");
            }
        }
    }
}
