//! The type names a BSD target predefines, held against the types the compiler gives `sizeof`, a
//! pointer difference and `__builtin_imaxabs`. A freestanding `stdint.h` is written out of the
//! macros, and `-Wformat` and `_Generic` read the types, so the two have to name the same type.

use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Whether the source compiles for the target with `-fsyntax-only`, and what the compiler said.
fn accepts(target: &str, source: &str) -> (bool, String) {
    // The tests in this file run on threads of one process, so each takes its own directory.
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rucc-bsd-types-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={target}"))
        .args(["-fsyntax-only", "-ffreestanding"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

/// The macros name the types the compiler uses, on every BSD row.
const AGREE: &str = "\
#define IS(e, t) _Static_assert(_Generic((e), t: 1, default: 0), #e \" is \" #t)
IS(sizeof 0, __SIZE_TYPE__);
IS((int *)0 - (int *)0, __PTRDIFF_TYPE__);
IS(__builtin_imaxabs(0), __INTMAX_TYPE__);
IS(L'a', __WCHAR_TYPE__);
";

#[test]
fn the_type_macros_name_the_types_the_compiler_uses_on_each_bsd() {
    for target in [
        "x86_64-unknown-freebsd15.1",
        "aarch64-unknown-freebsd15.0",
        "riscv64-unknown-freebsd",
        "i386-unknown-freebsd14.5",
        "x86_64--netbsd",
        "aarch64--netbsd",
        "i486--netbsdelf",
        "x86_64-unknown-dragonfly",
        "x86_64-unknown-openbsd7.8",
        "i386-unknown-openbsd",
    ] {
        let (ok, said) = accepts(target, AGREE);
        assert!(ok, "{target}: {said}");
    }
}

/// gcc's `openbsd-stdint.h` and `i386/openbsdelf.h` make `intmax_t` a `long long` and, on i386,
/// `size_t` an `unsigned long`. The other BSDs keep `long` and `unsigned int`.
#[test]
fn openbsd_makes_intmax_long_long_and_the_i386_size_long() {
    let names = "\
#define IS(e, t) _Static_assert(_Generic((e), t: 1, default: 0), #e \" is \" #t)
IS(__builtin_imaxabs(0), long long);
IS((__INT64_TYPE__)0, long long);
IS((__INT_FAST8_TYPE__)0, int);
IS((__WINT_TYPE__)0, int);
IS((__SIG_ATOMIC_TYPE__)0, int);
";
    for target in ["x86_64-unknown-openbsd7.8", "i386-unknown-openbsd"] {
        let (ok, said) = accepts(target, names);
        assert!(ok, "{target}: {said}");
    }
    let (ok, said) = accepts(
        "i386-unknown-openbsd",
        "_Static_assert(_Generic(sizeof 0, unsigned long: 1, default: 0), \"\");\n",
    );
    assert!(ok, "{said}");
    let (ok, said) = accepts(
        "i386-unknown-freebsd",
        "_Static_assert(_Generic(sizeof 0, unsigned int: 1, default: 0), \"\");\n",
    );
    assert!(ok, "{said}");
    let (ok, _) = accepts(
        "x86_64-unknown-freebsd",
        "_Static_assert(_Generic(__builtin_imaxabs(0), long: 1, default: 0), \"\");\n",
    );
    assert!(ok);
}
