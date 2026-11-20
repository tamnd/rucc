//! `<limits.h>` under `-ffreestanding` includes the system's `<limits.h>` too, as GCC's does.
//!
//! FreeBSD's rtld builds strcspn.c and strspn.c of libc with `-ffreestanding`, and both use
//! `LONG_BIT`, which only the system header defines. The ranges of the types are still the
//! compiler's own.

use std::process::Command;

/// The part of a system `<limits.h>` that the test reads, with a `CHAR_BIT` that is wrong on
/// purpose, so that the test sees whose value wins.
const SYSTEM: &str = "\
#ifndef _LIMITS_H_
#define _LIMITS_H_
#define LONG_BIT 64
#define CHAR_BIT 7
#endif
";

const SOURCE: &str = "\
#include <limits.h>
#ifndef LONG_BIT
#error LONG_BIT is not defined
#endif
#if CHAR_BIT != 8
#error CHAR_BIT is not the compiler's
#endif
int x;
";

#[test]
fn freestanding_limits_h_reads_the_systems_too() {
    let root = std::env::temp_dir().join(format!("rucc-limits-h-{}", std::process::id()));
    let include = root.join("usr/include");
    std::fs::create_dir_all(&include).expect("a temporary tree can be made");
    std::fs::write(include.join("limits.h"), SYSTEM).expect("limits.h");
    let file = root.join("one.c");
    std::fs::write(&file, SOURCE).expect("the fixture can be written");
    let mut failed = Vec::new();
    for (target, args) in [
        ("x86_64-unknown-freebsd15.1", &["-ffreestanding"][..]),
        ("x86_64-unknown-freebsd15.1", &["-ffreestanding", "-m32"][..]),
        ("x86_64-unknown-freebsd15.1", &[][..]),
        ("x86_64-unknown-linux-gnu", &["-ffreestanding"][..]),
    ] {
        let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
            .arg(format!("--target={target}"))
            .arg(format!("--sysroot={}", root.display()))
            .args(args)
            .arg("-fsyntax-only")
            .arg(&file)
            .output()
            .expect("the compiler is built before its own tests run");
        if !out.status.success() {
            failed.push(format!("{target} {args:?}: {}", String::from_utf8_lossy(&out.stderr)));
        }
    }
    let _ = std::fs::remove_dir_all(&root);
    assert!(failed.is_empty(), "{failed:#?}");
}
