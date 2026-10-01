//! The compiler started through the names a cross build of the Linux kernel gives it.
//!
//! Design: `spec/04-driver-and-cli.md` section 4.3.
//!
//! `make CROSS_COMPILE=aarch64-linux-gnu-` calls `aarch64-linux-gnu-gcc`, and so does every build
//! that picks its compiler by prefix. Here the binary is reached through links with those names, as
//! it is on a machine where rucc stands in for the cross gcc, and each one has to build for the
//! target its name carries. Links are a Unix thing, so this is compiled on Unix only.

#![cfg(unix)]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// A directory of its own holding a link to the compiler called `name`. The caller removes it with
/// [`done`].
fn link(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-cross-names-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join(name);
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_rucc"), &path).expect("the link can be made");
    path
}

/// Removes the directory [`link`] made.
fn done(program: &Path) {
    let _ = std::fs::remove_dir_all(program.parent().expect("the link is in a directory"));
}

/// The compiler under `program`, given these flags and this standard input.
fn run(program: &Path, flags: &[&str], input: &str) -> Output {
    let mut child = Command::new(program)
        .env("LC_ALL", "C")
        .args(flags)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the compiler is built before its own tests run");
    let mut stdin = child.stdin.take().expect("a pipe to write the input into");
    stdin.write_all(input.as_bytes()).expect("the input can be written");
    drop(stdin);
    child.wait_with_output().expect("the compiler finished")
}

/// `e_machine` of the object the compiler writes for a one line C file.
fn machine(program: &Path, flags: &[&str]) -> u16 {
    let object = program.with_file_name("a.o");
    let path = object.to_str().expect("the path is text");
    let mut all = vec!["-x", "c", "-c", "-", "-o", path];
    all.extend_from_slice(flags);
    let out = run(program, &all, "int f(void) { return 1; }\n");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let bytes = std::fs::read(&object).expect("the object was written");
    assert_eq!(&bytes[..4], b"\x7fELF", "a Linux target writes ELF");
    u16::from_le_bytes([bytes[18], bytes[19]])
}

const EM_386: u16 = 3;
const EM_X86_64: u16 = 62;
const EM_AARCH64: u16 = 183;

#[test]
fn aarch64_linux_gnu_gcc_builds_for_aarch64() {
    let gcc = link("aarch64-linux-gnu-gcc");
    let out = run(&gcc, &["-dumpmachine"], "");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "aarch64-unknown-linux-gnu");
    assert_eq!(machine(&gcc, &[]), EM_AARCH64);
    done(&gcc);
}

#[test]
fn x86_64_linux_gnu_gcc_builds_for_x86_64() {
    let gcc = link("x86_64-linux-gnu-gcc");
    let out = run(&gcc, &["-dumpmachine"], "");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "x86_64-unknown-linux-gnu");
    assert_eq!(machine(&gcc, &[]), EM_X86_64);
    done(&gcc);
}

#[test]
fn a_target_written_on_the_command_line_wins_over_the_name() {
    let gcc = link("aarch64-linux-gnu-gcc-14");
    assert_eq!(machine(&gcc, &["--target=x86_64-linux-gnu"]), EM_X86_64);
    done(&gcc);
}

/// The name implies the 32-bit target, and the object is a 32-bit one. What must not happen is a
/// 64 bit object for the host, which kbuild would take and link into a 32 bit image.
#[test]
fn i686_linux_gnu_gcc_builds_for_i386() {
    let gcc = link("i686-linux-gnu-gcc");
    let out = run(&gcc, &["-dumpmachine"], "");
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "i686-unknown-linux-gnu");
    assert_eq!(machine(&gcc, &[]), EM_386);
    done(&gcc);
}
