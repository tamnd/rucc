//! The copies on the two edges out of one branch, where both write the same register.
//!
//! The kernel's `__jump_label_patch` picks the bytes it expects at a jump label site with
//! `if (type == JUMP_LABEL_JMP) expect = nop; else expect = code;`. Each arm is a copy into the
//! register that `memcmp` is then given. The pass that lifts the copies on a split edge in front
//! of the branch lifted both, since each saw the register as dead on the other edge before the
//! other edge lost its copy. The second copy overwrote the first, the kernel expected a jump where
//! the code held a nop, and 7.2 stopped with a BUG in `sched_init`. The fixture is that function
//! with the kernel's types made small, and it exits 0 when the right bytes were expected.

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
const PATCH: &str = r#"
#include <string.h>

struct jump_entry { int code; int target; long key; };
struct patch { const void *code; int size; };

const void *const nops[6] = { 0, 0, "\x66\x90", 0, 0, "\x0f\x1f\x44\x00\x00" };
static union {
    unsigned char text[5];
    struct { unsigned char op; int disp; } __attribute__((packed));
} insn;

__attribute__((noinline)) int entry_size(struct jump_entry *entry) {
    return entry->key & 1 ? 5 : 2;
}

__attribute__((noinline)) static void *gen(unsigned char op, const void *addr, const void *dest,
                                           int size) {
    insn.op = op;
    insn.disp = (long)dest - (long)((const char *)addr + size);
    return &insn.text;
}

__attribute__((noinline)) struct patch patch(struct jump_entry *entry, int type, const void *addr) {
    const void *expect, *code, *nop;
    const void *dest = (const char *)addr + 12;
    int size = entry_size(entry);
    switch (size) {
    case 2: code = gen(0xeb, addr, dest, 2); nop = nops[size]; break;
    case 5: code = gen(0xe9, addr, dest, 5); nop = nops[size]; break;
    default: __builtin_trap();
    }
    if (type == 1)
        expect = nop;
    else
        expect = code;
    if (memcmp(addr, expect, size))
        __builtin_trap();
    if (type == 0)
        code = nop;
    return (struct patch){ .code = code, .size = size };
}

int main(void) {
    struct jump_entry entry = { 0, 0, 0 };
    unsigned char nop_site[16] = { 0x66, 0x90 };
    unsigned char jmp_site[16] = { 0xeb, 0x0a };
    struct patch on = patch(&entry, 1, nop_site);
    struct patch off = patch(&entry, 0, jmp_site);
    return *(const unsigned char *)on.code == 0xeb && on.size == 2
        && *(const unsigned char *)off.code == 0x66 ? 0 : 1;
}
"#;

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn the_copy_on_each_arm_reaches_memcmp() {
    use std::process::Command;

    let dir = std::env::temp_dir().join(format!("rucc-edge-copies-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let source = dir.join("patch.c");
    let program = dir.join("patch");
    std::fs::write(&source, PATCH).expect("the fixture can be written");
    for level in ["-O1", "-O2"] {
        let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
            .arg(level)
            .arg("-o")
            .arg(&program)
            .arg(&source)
            .output()
            .expect("the compiler is built before its own tests run");
        let said = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "the compiler refused the fixture at {level}:\n{said}");
        let status = Command::new(&program).status().expect("the program runs");
        assert_eq!(status.code(), Some(0), "the wrong bytes were expected at {level}");
    }
    std::fs::remove_dir_all(&dir).ok();
}
