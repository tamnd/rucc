//! A loop that fills or copies bytes, turned into `memset` or `memcpy` on i386. Loop idiom
//! recognition works the length out at sixty four bits, and the step that splits a `long long` into
//! two registers did not know a copy or a fill, so it left the whole function alone and the
//! compiler stopped with "no rule lowers `zext.i32.i64`", or `sext`, or `icmp_ule.i1`. The kernel's
//! `num_to_str` in `lib/vsprintf.c` and the multiply loops in `lib/crypto/mpi` were refused that way
//! on a 32 bit build.

use std::process::Command;

const SOURCE: &str = "\
void pad(char *buf, int num, unsigned int width) {
    int len = num <= 9 ? 1 : 7;
    for (int idx = 0; idx < width - len; idx++)
        buf[idx] = ' ';
}
void copy(unsigned long *restrict to, const unsigned long *restrict from, int n) {
    for (int i = 0; i < n; i++)
        to[i] = from[i];
}
int num_to_str(char *buf, int size, unsigned long long num, unsigned int width) {
    char tmp[24];
    int idx, len;
    if (num <= 9) {
        tmp[0] = '0' + num;
        len = 1;
    } else {
        len = 2;
        tmp[0] = '0' + num % 10;
        tmp[1] = '0' + num / 10 % 10;
    }
    if (len > size || width > size)
        return 0;
    if (width > len) {
        width = width - len;
        for (idx = 0; idx < width; idx++)
            buf[idx] = ' ';
    } else {
        width = 0;
    }
    for (idx = 0; idx < len; ++idx)
        buf[idx + width] = tmp[len - idx - 1];
    return len + width;
}
";

fn listing(level: &str) -> String {
    let dir = std::env::temp_dir().join(format!("rucc-i386-fill-{}-{level}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=i686-unknown-linux-gnu", level, "-fno-asynchronous-unwind-tables", "-S"])
        .args(["-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{level}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("a listing is text")
}

/// Every function compiles at every level, and above `-O0` the two loops are the library calls.
#[test]
fn a_fill_or_a_copy_loop_compiles_on_i386() {
    for level in ["-O0", "-O1", "-O2", "-Os"] {
        let listing = listing(level);
        for name in ["pad", "copy", "num_to_str"] {
            assert!(listing.contains(&format!("\n{name}:\n")), "{level}: {name}");
        }
        if level == "-O2" {
            assert!(listing.contains("memset"), "{level}: {listing}");
            assert!(listing.contains("memcpy"), "{level}: {listing}");
        }
    }
}
