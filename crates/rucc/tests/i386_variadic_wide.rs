//! A `long long` named ahead of the `...` on i386, at a call and in a definition. Every argument of
//! a variadic call is on the stack in the order it is written, even under `-mregparm`, so the two
//! halves sit where the whole did and the rest follow. The kernel's `__ext4_error` takes a `__u64`
//! block number ahead of its format, and every ext4 function that reports an error used to be
//! refused with an `iconst.i64` nothing lowered.

use std::process::Command;

const SOURCE: &str = "\
#include <stdarg.h>
typedef unsigned long long u64;
int callee(int a, u64 b, const char *fmt, ...) {
  va_list ap; va_start(ap, fmt);
  int x = va_arg(ap, int);
  va_end(ap);
  return a + (int)(b >> 32) + fmt[0] + x;
}
int caller(void) { return callee(7, 0x100000000ull, \"hi\", 42); }
";

fn build(flags: &[&str]) -> std::process::Output {
    let dir = std::env::temp_dir().join(format!(
        "rucc-i386-variadic-wide-{}{}",
        std::process::id(),
        flags.join("")
    ));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=i686-unknown-linux-gnu", "-fno-pic", "-S", "-o", "-"])
        .args(flags)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    out
}

#[test]
fn a_wide_named_argument_of_a_variadic_call_is_two_words_on_the_stack() {
    for level in ["-O0", "-O2"] {
        for regparm in ["-mregparm=0", "-mregparm=3"] {
            let out = build(&[level, regparm]);
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(out.status.success(), "{level} {regparm}: {stderr}");
            let text = String::from_utf8_lossy(&out.stdout);
            let start = text.find("\ncaller:").expect("the caller is written");
            let caller = &text[start..];
            let call = caller.find("call\tcallee").expect("the caller calls");
            let before = &caller[..call];
            for at in ["(%esp)", "4(%esp)", "8(%esp)", "12(%esp)", "16(%esp)"] {
                assert!(before.contains(at), "{level} {regparm}: nothing at {at}\n{text}");
            }
        }
    }
}
