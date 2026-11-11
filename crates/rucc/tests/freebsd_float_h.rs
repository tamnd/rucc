//! Which `<float.h>` a FreeBSD target reads on amd64 and i386.
//!
//! The GCC that builds the FreeBSD world (the devel/freebsd-gcc ports) has no `<float.h>` of its
//! own on amd64 and i386, because the port deletes it. So that GCC reads the system's header, which
//! says that a long double has 64 bits with `-m32` too. The compiler itself says 53 there, and the
//! long double functions of the lib32 msun stop with "Unsupported long double format" on 53.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The part of FreeBSD's sys/x86/include/float.h that the test reads.
const SYSTEM: &str = "\
#ifndef _MACHINE_FLOAT_H_
#define _MACHINE_FLOAT_H_
#define LDBL_MANT_DIG 64
#endif
";

/// A sysroot with a system `<float.h>` in it if `system` says so, and a file that stops unless
/// `LDBL_MANT_DIG` is `want`.
fn tree(name: &str, system: bool, want: u32) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("rucc-float-h-{name}-{}", std::process::id()));
    let include = root.join("usr/include");
    std::fs::create_dir_all(&include).expect("a temporary tree can be made");
    if system {
        std::fs::write(include.join("float.h"), SYSTEM).expect("float.h");
    }
    let file = root.join("one.c");
    let source = format!(
        "#include <float.h>\n#if LDBL_MANT_DIG != {want}\n#error LDBL_MANT_DIG is not {want}\n#endif\nint x;\n"
    );
    std::fs::write(&file, source).expect("the fixture can be written");
    (root, file)
}

/// Compiles `file` for `target` with `args` against `root`, and says whether it compiled and what
/// the compiler printed.
fn compile(target: &str, args: &[&str], root: &Path, file: &Path) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={target}"))
        .arg(format!("--sysroot={}", root.display()))
        .args(args)
        .arg("-fsyntax-only")
        .arg(file)
        .output()
        .expect("the compiler is built before its own tests run");
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test]
fn freebsd_on_x86_reads_the_systems_float_h() {
    for (name, target, args) in [
        ("m32", "x86_64-unknown-freebsd15.1", &["-m32"][..]),
        ("amd64", "x86_64-unknown-freebsd15.1", &[][..]),
        ("i386", "i386-unknown-freebsd15.1", &[][..]),
    ] {
        let (root, file) = tree(name, true, 64);
        let (ok, said) = compile(target, args, &root, &file);
        let _ = std::fs::remove_dir_all(&root);
        assert!(ok, "{target} {args:?}: {said}");
    }
}

/// With no `<float.h>` in the system, the compiler's own header answers, with the compiler's 53.
#[test]
fn freebsd_with_no_system_float_h_reads_the_compilers() {
    let (root, file) = tree("own", false, 53);
    let (ok, said) = compile("x86_64-unknown-freebsd15.1", &["-m32"], &root, &file);
    let _ = std::fs::remove_dir_all(&root);
    assert!(ok, "{said}");
}

/// Other targets keep the compiler's header first, as their GCC does.
#[test]
fn linux_still_reads_the_compilers_float_h() {
    let (root, file) = tree("linux", true, 64);
    std::fs::write(root.join("usr/include/float.h"), "#error the system's float.h\n")
        .expect("float.h");
    let (ok, said) = compile("x86_64-unknown-linux-gnu", &["-m32"], &root, &file);
    let _ = std::fs::remove_dir_all(&root);
    assert!(ok, "{said}");
}
