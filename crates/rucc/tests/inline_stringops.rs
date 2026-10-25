//! `-finline-stringops`, which Linux 7.3 passes as `=memset` to the vDSO (tamnd/rucc#3761).
//!
//! The kernel probes for the flag with `cc-option`, and where the compiler takes it the vDSO is
//! built with it, so a fill there must never become a call: the vDSO is linked against nothing.
//! gcc 16 writes every `__builtin_memset` and every structure fill inline under the flag, a length
//! the program works out included, and so does `-ffreestanding` or `-fno-builtin`, where only the
//! plain call to `memset` stays a call. Each case below is what gcc 16 does with the same source.

use std::path::PathBuf;
use std::process::{Command, Output};

const FILLS: &str = r#"
typedef unsigned long size_t;
struct big { char a[333]; };
void zero(char *p, size_t n) { __builtin_memset(p, 0, n); }
void byte(char *p, int c, size_t n) { __builtin_memset(p, c, n); }
void *kept(char *p, size_t n) { return __builtin_memset(p, 0x5a, n); }
void large(char *p) { __builtin_memset(p, 1, 4096); }
void clear(struct big *b) { *b = (struct big){0}; }
"#;

const PLAIN: &str = r#"
typedef unsigned long size_t;
extern void *memset(void *, int, size_t);
void plain(char *p, size_t n) { memset(p, 0, n); }
"#;

fn compile(name: &str, source: &str, flags: &[&str]) -> (Output, String) {
    let dir = std::env::temp_dir().join(format!("rucc-stringops-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path: PathBuf = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(flags)
        .args(["-S", "-o"])
        .arg(dir.join("one.s"))
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let asm = std::fs::read_to_string(dir.join("one.s")).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);
    (out, asm)
}

#[test]
fn every_fill_is_written_inline_under_the_flag() {
    for target in
        ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu", "i686-unknown-linux-gnu"]
    {
        for level in ["-O0", "-O2", "-Os"] {
            for extra in ["-fhosted", "-ffreestanding", "-fno-builtin"] {
                let target_flag = format!("--target={target}");
                let flags = [target_flag.as_str(), level, extra, "-finline-stringops=memset"];
                let (out, asm) = compile(&format!("{target}{level}{extra}"), FILLS, &flags);
                let said = String::from_utf8_lossy(&out.stderr);
                assert!(out.status.success(), "{target} {level} {extra}: {said}");
                assert!(!asm.contains("memset"), "{target} {level} {extra}:\n{asm}");
            }
        }
    }
}

#[test]
fn without_the_flag_a_fill_of_unknown_length_is_still_a_call() {
    let flags = ["--target=x86_64-unknown-linux-gnu", "-O2"];
    let (out, asm) = compile("default", FILLS, &flags);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(asm.contains("memset"), "{asm}");
}

#[test]
fn a_plain_call_is_inline_only_where_the_name_means_the_library() {
    let hosted = ["--target=x86_64-unknown-linux-gnu", "-O2", "-finline-stringops=memset"];
    let (out, asm) = compile("hosted", PLAIN, &hosted);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(!asm.contains("memset"), "{asm}");
    for extra in ["-ffreestanding", "-fno-builtin", "-fno-builtin-memset"] {
        let flags = ["--target=x86_64-unknown-linux-gnu", "-O2", extra, "-finline-stringops"];
        let (out, asm) = compile(extra, PLAIN, &flags);
        assert!(out.status.success(), "{extra}: {}", String::from_utf8_lossy(&out.stderr));
        assert!(asm.contains("memset"), "{extra}:\n{asm}");
    }
}

#[test]
fn the_flag_is_taken_and_refused_as_gcc_takes_and_refuses_it() {
    let source = "int x;\n";
    for flag in [
        "-finline-stringops",
        "-fno-inline-stringops",
        "-finline-stringops=memset",
        "-finline-stringops=memcmp,memcpy,memmove,memset",
        "-fno-inline-stringops=memcpy",
    ] {
        let (out, _) = compile("taken", source, &["--target=x86_64-unknown-linux-gnu", flag]);
        assert!(out.status.success(), "{flag}: {}", String::from_utf8_lossy(&out.stderr));
    }
    for (flag, said) in [
        ("-finline-stringops=", "missing argument"),
        ("-finline-stringops=strlen", "unavailable stringop for inlining `strlen`"),
    ] {
        let (out, _) = compile("refused", source, &["--target=x86_64-unknown-linux-gnu", flag]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{flag} was taken");
        assert!(stderr.contains(said), "{flag}: {stderr}");
    }
    // The flag came in gcc 14, so a compiler claiming 13 does not know it.
    let flags =
        ["--target=x86_64-unknown-linux-gnu", "-fgnuc-version=13.3.0", "-finline-stringops=memset"];
    let (out, _) = compile("gcc13", source, &flags);
    assert!(!out.status.success(), "gcc 13 took the flag");
}
