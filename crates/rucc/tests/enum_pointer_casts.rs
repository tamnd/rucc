//! A cast between a pointer and an enum is quiet, whatever the widths.
//!
//! gcc only measures a cast against a plain integer type, so an enum on either side says nothing
//! where an `int` would get "cast to pointer from integer of different size". The kernel's
//! lib/kunit/attributes.c returns `(void *) test->attr.speed`, an enum, under `-Werror`.

use std::process::Command;

const TARGET: &str = "x86_64-linux-gnu";

const SOURCE: &str = "
enum speed { SLOW, FAST };
struct attrs { enum speed speed; };
void *boxed(struct attrs *a) { return (void *)a->speed; }
enum speed unboxed(void *p) { return (enum speed)p; }
";

#[test]
fn an_enum_cast_to_a_pointer_or_back_says_nothing_under_werror() {
    let dir = std::env::temp_dir().join(format!("rucc-enum-pointer-casts-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args(["-fsyntax-only", "-Wall", "-Werror"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(stderr.is_empty(), "{stderr}");
}
