//! A local that the arms of a `switch` on a `sizeof` read at every width, where only one arm can
//! run (tamnd/rucc-kernel#109).
//!
//! Every `this_cpu_*` macro in the kernel is a `switch (sizeof(...))` with an arm for one, two,
//! four and eight bytes. `percpu_counter_add_batch` keeps an `s64` that the macro for an `s32`
//! counter reads four bytes at a time, and the arms for one and two bytes cut it into pieces of
//! one, one, two and four bytes, which the loop put back together with shifts on every pass. gcc
//! 16 keeps the local whole. rucc now takes the arms that cannot run out before it cuts the local.

use std::process::Command;

const SOURCE: &str = r#"
typedef long long s64;
typedef unsigned char u8;
typedef unsigned short u16;
typedef unsigned int u32;
#define SWAP(type, how) do { \
        type *old = (type *)&count; type was = *old; \
        asm volatile("cmpxchg %2, %1" : "+a"(was), "+m"(*p) : how((type)(count + amount)) : "memory"); \
        ok = was == *old; if (!ok) *old = was; \
    } while (0)
s64 counted(int *p, s64 amount) {
    s64 count = *p;
    do {
        if (count + amount > 100)
            return count;
    } while (!({
        _Bool ok;
        switch (sizeof(*p)) {
        case 1: SWAP(u8, "q"); break;
        case 2: SWAP(u16, "r"); break;
        case 4: SWAP(u32, "r"); break;
        default: ok = 1; break;
        }
        ok;
    }));
    return 0;
}
"#;

#[test]
fn the_arms_that_cannot_run_do_not_cut_the_local() {
    let dir = std::env::temp_dir().join(format!("rucc-sized-arms-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-unknown-linux-gnu", "-O2", "-S", "-o"])
        .arg(dir.join("one.s"))
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let asm = std::fs::read_to_string(dir.join("one.s")).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);
    // A piece of one or two bytes is widened with `movzb` or `movzw` each time it is put back.
    assert!(!asm.contains("movzb") && !asm.contains("movzw"), "{asm}");
    assert!(!asm.contains("cmpxchgb") && !asm.contains("cmpxchgw"), "{asm}");
}
