//! A `cmpb` of two bytes in `esi` and `edi`, which i386 has no low byte for, is a `cmpl` of the
//! whole registers when both were just widened from a byte with `movzbl` and the branch behind it
//! reads only the zero or the carry. The NetBSD libc `strncmp` had two exchanges on either side of
//! the `cmpb`.

use std::process::Command;

const SOURCE: &str = "\
int strncmp(const char *s1, const char *s2, unsigned n)
{
    if (n == 0)
        return 0;
    do {
        if (*s1 != *s2++)
            return *(const unsigned char *)s1 - *(const unsigned char *)--s2;
        if (*s1++ == 0)
            break;
    } while (--n != 0);
    return 0;
}
";

#[test]
fn two_widened_bytes_are_compared_whole() {
    let dir = std::env::temp_dir().join(format!("rucc-i386-byte-compare-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    for level in ["-Os", "-O2"] {
        let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
            .args(["--target=i686-unknown-linux-gnu", level, "-fno-pic", "-ffreestanding"])
            .args(["-fno-asynchronous-unwind-tables", "-S", "-o", "-"])
            .arg(&path)
            .output()
            .expect("the compiler is built before its own tests run");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let text = String::from_utf8(out.stdout).expect("a listing is text");
        assert!(!text.contains("xchgl"), "{level}:\n{text}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
