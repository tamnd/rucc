//! Which directory a standard header comes from on NetBSD, where the system has its own.
//!
//! NetBSD's GCC keeps the intrinsics in a directory of its own and takes `<stdarg.h>`, `<stddef.h>`
//! and the other headers of the C standard from /usr/include. `<err.h>` includes `<stdarg.h>` and
//! `<syslog.h>` includes `<sys/stdarg.h>`, which define `va_start` the same way. When the
//! compiler's own `<stdarg.h>` came first, `va_start` had two definitions in other words, and
//! NetBSD builds with `-Wsystem-headers -Werror`, so `bin/date` stopped there.

use std::path::{Path, PathBuf};
use std::process::Command;

/// `<sys/stdarg.h>` of NetBSD, the part of it that defines the names. `<stdarg.h>` is the same
/// file with the same guard.
const STDARG: &str = "\
#ifndef _SYS_STDARG_H_
#define _SYS_STDARG_H_
typedef __builtin_va_list va_list;
#define va_start(ap, last) __builtin_va_start((ap), (last))
#define va_arg __builtin_va_arg
#define va_end(ap) __builtin_va_end(ap)
#define __va_copy(dest, src) __builtin_va_copy((dest), (src))
#define va_copy(dest, src) __va_copy((dest), (src))
#endif
";

/// A tree with only the two headers in it, and a file to compile.
fn tree(name: &str, source: &str) -> (PathBuf, PathBuf) {
    let root =
        std::env::temp_dir().join(format!("rucc-netbsd-headers-{name}-{}", std::process::id()));
    let include = root.join("usr/include");
    std::fs::create_dir_all(include.join("sys")).expect("a temporary tree can be made");
    std::fs::write(include.join("stdarg.h"), STDARG).expect("stdarg.h");
    std::fs::write(include.join("sys/stdarg.h"), STDARG).expect("sys/stdarg.h");
    let file = root.join("one.c");
    std::fs::write(&file, source).expect("the fixture can be written");
    (root, file)
}

/// Compiles `file` for `target` against `root` with NetBSD's warning flags, and says whether it
/// compiled and what the compiler printed.
fn compile(target: &str, root: &Path, file: &Path) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={target}"))
        .arg(format!("--sysroot={}", root.display()))
        .args(["-fsyntax-only", "-Wall", "-Wsystem-headers", "-Werror"])
        .arg(file)
        .output()
        .expect("the compiler is built before its own tests run");
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test]
fn stdarg_and_sys_stdarg_are_one_header_on_netbsd() {
    let source = "\
#include <stdarg.h>
#include <sys/stdarg.h>
int sum(int n, ...) {
    va_list ap;
    int total = 0;
    va_start(ap, n);
    while (n-- > 0)
        total += va_arg(ap, int);
    va_end(ap);
    return total;
}
";
    let (root, file) = tree("stdarg", source);
    let (ok, said) = compile("x86_64--netbsd", &root, &file);
    let _ = std::fs::remove_dir_all(&root);
    assert!(ok, "{said}");
    assert_eq!(said, "");
}

/// A header the system does not have still comes from the compiler, as NetBSD's GCC takes
/// `<xmmintrin.h>` from its own directory.
#[test]
fn a_header_netbsd_does_not_have_is_still_the_compilers() {
    let source = "#include <xmmintrin.h>\n__m128 zero(void) { return _mm_setzero_ps(); }\n";
    let (root, file) = tree("xmm", source);
    let (ok, said) = compile("x86_64--netbsd", &root, &file);
    let _ = std::fs::remove_dir_all(&root);
    assert!(ok, "{said}");
}

/// FreeBSD's reference is the GCC of the ports tree, whose own directory has `<stdarg.h>` and comes
/// before /usr/include. So there the compiler's header still comes first, and a system one that
/// would stop the compile is never read.
#[test]
fn freebsd_still_takes_the_compilers_stdarg_first() {
    let (root, file) = tree("freebsd", "#include <stdarg.h>\nva_list ap;\n");
    std::fs::write(root.join("usr/include/stdarg.h"), "#error the system's stdarg.h\n")
        .expect("stdarg.h");
    let (ok, said) = compile("x86_64-unknown-freebsd15.1", &root, &file);
    let _ = std::fs::remove_dir_all(&root);
    assert!(ok, "{said}");
}
