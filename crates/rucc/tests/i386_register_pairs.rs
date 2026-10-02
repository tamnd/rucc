//! A `long long` local register variable on i386, which is the register it names and the one gcc
//! numbers after it. The kernel's `put_user` hands `__put_user_8` its value with
//! `register u64 val asm("%eax")` under `"r"`, which gcc reads as `edx:eax`, and `get_user` takes
//! one back with `register u64 val asm("%edx")` under `"=r"`, which is `ecx:edx`. Both used to be
//! refused with a `register_value` producing an `i64` nothing lowered.

use std::process::Command;

const SOURCE: &str = "\
typedef unsigned long long u64;
int put(u64 *ptr, u64 x) {
  int ret; register u64 val asm(\"%eax\") = x; void *p = ptr;
  asm volatile(\"call __put_user_8\" : \"=c\"(ret) : \"0\"(p), \"r\"(val) : \"ebx\");
  return ret;
}
int get(u64 *ptr, u64 *x) {
  int ret; register u64 val asm(\"%edx\");
  asm volatile(\"call __get_user_8\" : \"=a\"(ret), \"=r\"(val) : \"0\"(ptr));
  *x = val; return ret;
}
";

fn build(level: &str) -> std::process::Output {
    let dir = std::env::temp_dir()
        .join(format!("rucc-i386-register-pairs-{}{level}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=i686-unknown-linux-gnu", "-fno-pic", level, "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    out
}

/// The lines of one function's body.
fn body<'a>(text: &'a str, name: &str) -> Vec<&'a str> {
    let start = text.find(&format!("\n{name}:")).expect("the function is written");
    let rest = &text[start + name.len() + 2..];
    let end = rest.find("\tret").expect("the function returns");
    rest[..end].lines().map(str::trim).collect()
}

#[test]
fn a_long_long_register_variable_is_the_register_and_the_next() {
    for level in ["-O0", "-O2"] {
        let out = build(level);
        assert!(out.status.success(), "{level}: {}", String::from_utf8_lossy(&out.stderr));
        let text = String::from_utf8_lossy(&out.stdout);
        let get = body(&text, "get");
        let call = get.iter().position(|line| line.contains("__get_user_8")).unwrap();
        let after = get[call..].join("\n");
        assert!(after.contains("movl\t%edx, ("), "{level}: the low half is edx\n{text}");
        assert!(after.contains("movl\t%ecx, 4("), "{level}: the high half is ecx\n{text}");
        let put = body(&text, "put");
        let call = put.iter().position(|line| line.contains("__put_user_8")).unwrap();
        let before = put[..call].join("\n");
        assert!(before.contains(", %eax"), "{level}: the low half goes in eax\n{text}");
        assert!(before.contains(", %edx"), "{level}: the high half goes in edx\n{text}");
    }
}
