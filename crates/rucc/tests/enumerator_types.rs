//! What type an enumerator has while its enumeration is still being read, end to end.
//!
//! Inside the list an enumerator whose value does not fit in an `int` has the type of the
//! expression that gave it its value, or of the enumerator before it, and it is only once the list
//! is closed that every enumerator takes the enumeration's own type. The kernel's blk-mq.h reads
//! the first answer: `BLK_MQ_TAG_MAX = BLK_MQ_NO_TAG - 1` with `BLK_MQ_NO_TAG = -1U`, which gcc
//! folds as an `unsigned int` subtraction and which is an overflow if it is read as an `int`.
//! Every assertion below also holds under gcc 13.

use std::process::Command;

const TARGET: &str = "x86_64-linux-gnu";

const SOURCE: &str = "
enum { A = -1U, B = A - 1, C = sizeof(A), D = sizeof(B), E = A > 0 };
_Static_assert(B == 0xfffffffeU, \"b\");
_Static_assert(C == 4 && D == 4 && E == 1, \"c\");
enum { H = 0xffffffffU, I, J = sizeof(I) };
_Static_assert(J == 8, \"j\");
enum { K = 0x80000000, L = sizeof(K) };
_Static_assert(L == 4, \"l\");
enum { M = -1U, N = 1 };
_Static_assert(sizeof(M) == 4 && M > 0, \"m\");
";

#[test]
fn an_enumerator_too_wide_for_an_int_keeps_the_type_it_was_given_until_the_list_ends() {
    let dir = std::env::temp_dir().join(format!("rucc-enumerator-types-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args(["-fsyntax-only", "-Werror"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(stderr.is_empty(), "{stderr}");
}
