//! An enumerator wider than a `long` on a target where `long` is thirty two bits. The bound an
//! enumeration with no written type is held to is the widest integer type there is, which gcc
//! names as `uintmax_t`, and on i386 and Windows that is `long long` rather than `long`. The i386
//! kernel's `perf_event.h` has `PERF_CONTEXT_HV = (__u64)-32` and `bpf.h` has
//! `BPF_F_CTXLEN_MASK = (0xfffffULL << 32)`, and both were refused as out of range.

use std::process::Command;

const SOURCE: &str = "\
enum perf { HV = (unsigned long long)-32, MAX = (unsigned long long)-4095 };
enum { M = 0xffffffffULL, C = (0xfffffULL << 32) };
enum neg { N = -(1LL << 40), P = 1 };
_Static_assert(sizeof(enum perf) == 8, \"\");
_Static_assert(sizeof(enum neg) == 8, \"\");
_Static_assert(sizeof(HV) == 8, \"\");
_Static_assert((unsigned long long)C == 0xfffff00000000ULL, \"\");
_Static_assert(HV > 0 && N < 0, \"\");
";

#[test]
fn a_sixty_four_bit_enumerator_is_taken_where_long_is_thirty_two_bits() {
    let dir = std::env::temp_dir().join(format!("rucc-i386-enum-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    for target in ["i686-unknown-linux-gnu", "x86_64-unknown-linux-gnu", "x86_64-w64-windows-gnu"] {
        let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
            .arg(format!("--target={target}"))
            .arg("-fsyntax-only")
            .arg(&path)
            .output()
            .expect("the compiler is built before its own tests run");
        assert!(out.status.success(), "{target}: {}", String::from_utf8_lossy(&out.stderr));
    }
    let _ = std::fs::remove_dir_all(&dir);
}
