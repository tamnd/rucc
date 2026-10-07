//! The SVE state saves of the arm64 kernel's `asm/fpsimd.h`, which load and store every `z` and
//! `p` register at a number of vector lengths from a base, and clear the predicates with
//! `pfalse`. fpsimd.c, entry-common.c and the KVM switch code are built with them.

use std::process::Command;

/// The kernel's helpers, with its `.irp` over the registers.
const SOURCE: &str = r#"#define FOR_EACH_Z_REG(idx_str, asm_str) "	.irp " idx_str ",0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,26,27,28,29,30,31\n" asm_str "\n" "	.endr\n"
#define FOR_EACH_P_REG(idx_str, asm_str) "	.irp " idx_str ",0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15\n" asm_str "\n" "	.endr\n"
#define __SVE_PREAMBLE ".arch_extension sve\n"
void save_z(void *state) {
  asm volatile(__SVE_PREAMBLE FOR_EACH_Z_REG("n", "str	z\\n, [%[zregs], #\\n, MUL VL]") : : [zregs] "r" (state) : "memory");
}
void load_p(const void *pregs, const void *pffr, int ffr) {
  if (ffr)
    asm volatile(__SVE_PREAMBLE "	ldr	p0, [%[pffr]]\n" "	wrffr	p0.b\n" : : [pffr] "r" (pffr) : "memory");
  asm volatile(__SVE_PREAMBLE FOR_EACH_P_REG("n", "ldr	p\\n, [%[pregs], #\\n, MUL VL]\n") : : [pregs] "r" (pregs) : "memory");
}
void save_ffr(void *pregs, void *pffr) {
  asm volatile(__SVE_PREAMBLE "	rdffr	p0.b\n" "	str	p0, [%[pffr]]\n" "	ldr	p0, [%[pregs]]\n" : : [pregs] "r" (pregs), [pffr] "r" (pffr) : "memory");
}
void flush(void) {
  asm volatile(__SVE_PREAMBLE FOR_EACH_P_REG("n", "pfalse	p\\n\\().b") "	wrffr	p0.b\n");
}
"#;

#[test]
fn the_sve_state_saves_assemble() {
    let dir = std::env::temp_dir().join(format!("rucc-aarch64-sve-state-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    std::fs::write(dir.join("a.c"), SOURCE).expect("the fixture can be written");
    for level in ["-O0", "-O2"] {
        let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
            .args(["--target=aarch64-unknown-linux-gnu", level, "-c", "-o", "a.o", "a.c"])
            .current_dir(&dir)
            .output()
            .expect("the compiler is built before its own tests run");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success() && err.is_empty(), "{level}: {err}");
        let object = std::fs::read(dir.join("a.o")).expect("an object");
        // `str z31, [x?, #31, mul vl]` and `pfalse p15.b`, whatever the base register is.
        let words: Vec<u32> = object
            .chunks_exact(4)
            .map(|word| u32::from_le_bytes([word[0], word[1], word[2], word[3]]))
            .collect();
        assert!(words.iter().any(|&word| word & 0xffff_fc1f == 0xe583_5c1f), "{level}");
        assert!(words.contains(&0x2518_e40f), "{level}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
