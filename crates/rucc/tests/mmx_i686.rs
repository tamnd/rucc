//! The MMX loops lib/raid6 writes by hand for i386.
//!
//! `mmx.c` and `sse1.c` work out the RAID-6 syndrome in the MMX registers, and the xor routines
//! for the processors before SSE do the same. The assembler did not know `%mm0` was a register,
//! and the clobber list that names the x87 stack and those registers was refused. Neither holds
//! anything this compiler put there, since a `long double` is stored again after each operation.

use std::process::Command;

/// A shortened `raid6_mmx1_gen_syndrome`, with the stores `sse1.c` writes and the clobbers that
/// say the template leaves the x87 state as it found it.
const SYNDROME: &str = r#"
typedef unsigned long long u64;
static const u64 poly = 0x1d1d1d1d1d1d1d1dULL;
void gen(unsigned char *p, unsigned char *q, int bytes) {
    asm volatile("movq %0,%%mm0" : : "m" (poly));
    asm volatile("pxor %mm5,%mm5");
    for (int d = 0; d < bytes; d += 8) {
        asm volatile("prefetchnta %0" : : "m" (p[d]));
        asm volatile("movq %0,%%mm2" : : "m" (p[d]));
        asm volatile("pcmpgtb %mm4,%mm5");
        asm volatile("paddb %mm4,%mm4");
        asm volatile("pand %mm0,%mm5");
        asm volatile("pxor %mm5,%mm4");
        asm volatile("pxor %mm2,%mm4");
        asm volatile("movntq %%mm4,%0" : "=m" (q[d]));
    }
    asm volatile("sfence\n\temms" : : : "memory", "st", "st(1)", "mm0", "mm7");
}
"#;

fn compile(level: &str, emit: &str) -> std::process::Output {
    let dir =
        std::env::temp_dir().join(format!("rucc-mmx-i686-{}{level}{emit}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SYNDROME).expect("the fixture can be written");
    let out_path = dir.join("one.out");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=i686-unknown-linux-gnu", level, emit, "-o"])
        .arg(&out_path)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    out
}

#[test]
fn an_mmx_loop_assembles_for_i686() {
    for level in ["-O0", "-O1", "-O2", "-Os", "-O3"] {
        for emit in ["-c", "-S"] {
            let out = compile(level, emit);
            assert!(
                out.status.success(),
                "{level} {emit}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
}
