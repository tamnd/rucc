//! The hypervisor instructions and registers in FreeBSD's riscv vmm, assembled out of inline `asm`.
//!
//! FreeBSD 15.1 builds `vmm.ko` on riscv64. Its C has three kinds of line that name the hypervisor
//! extension: `hfence.gvma` in `sys/riscv/include/cpufunc.h`, `hlvx.hu` inside `.option norvc` in
//! `sys/riscv/vmm/vmm_riscv.c`, and `csr_read` and `csr_write` of the `h` and `vs` registers. The
//! source below has the same lines, under the flags of `sys/conf/kern.mk`. The words are the ones
//! GNU as 2.42 writes for them, with the registers masked out where the compiler picks them.

use std::path::PathBuf;
use std::process::Command;

/// The riscv64 lines of FreeBSD 15.1's `sys/conf/kern.mk`.
const KERNEL: [&str; 6] = [
    "--target=riscv64-unknown-freebsd15.1",
    "-march=rv64imafdch_zifencei",
    "-mabi=lp64",
    "-mcmodel=medany",
    "-ffreestanding",
    "-O2",
];

const SOURCE: &str = r#"
#define csr_read(csr) ({ unsigned long val; __asm __volatile("csrr %0, " #csr : "=r" (val)); val; })
#define csr_write(csr, val) __asm __volatile("csrw " #csr ", %0" :: "r" ((unsigned long)(val)))

static __inline void
hfence_gvma(void)
{
	__asm __volatile("hfence.gvma" ::: "memory");
}

unsigned long
fetch(unsigned long guest_addr)
{
	unsigned long val;

	__asm __volatile(".option push\n"
			 ".option norvc\n"
			"hlvx.hu %[val], (%[addr])\n"
			".option pop\n"
	    : [val] "=r" (val)
	    : [addr] "r" (guest_addr)
	    : "a1", "memory");
	return (val);
}

unsigned long
swap(unsigned long hstatus, unsigned long hgatp)
{
	unsigned long old = csr_read(hstatus);

	csr_write(hstatus, hstatus);
	csr_write(hgatp, hgatp);
	hfence_gvma();
	return (old + csr_read(htval) + csr_read(htinst) + csr_read(vsatp));
}
"#;

/// The words of `.text` in the object the source compiles to under the kernel's flags.
fn text() -> Vec<u32> {
    let dir: PathBuf = std::env::temp_dir().join(format!("rucc-hyp-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    std::fs::write(dir.join("vmm.c"), SOURCE).expect("the source can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .env("LC_ALL", "C")
        .args(KERNEL)
        .args(["-c", "vmm.c", "-o", "vmm.o"])
        .current_dir(&dir)
        .output()
        .expect("the compiler is built before its own tests run");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let object = std::fs::read(dir.join("vmm.o")).expect("the object was written");
    let _ = std::fs::remove_dir_all(&dir);
    let text = section(&object, ".text").expect("the object has a .text");
    text.chunks_exact(4).map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]])).collect()
}

fn number(bytes: &[u8], at: usize, len: usize) -> u64 {
    let mut out = [0u8; 8];
    out[..len].copy_from_slice(&bytes[at..at + len]);
    u64::from_le_bytes(out)
}

/// One section of a 64 bit little endian ELF object.
fn section<'a>(object: &'a [u8], want: &str) -> Option<&'a [u8]> {
    let start = number(object, 0x28, 8) as usize;
    let size = number(object, 0x3a, 2) as usize;
    let count = number(object, 0x3c, 2) as usize;
    let names = number(object, 0x3e, 2) as usize;
    let at = |which: usize| start + which * size;
    let strings = number(object, at(names) + 24, 8) as usize;
    (0..count).find_map(|which| {
        let header = at(which);
        let name = strings + number(object, header, 4) as usize;
        let end = object[name..].iter().position(|&byte| byte == 0).expect("a name ends");
        let offset = number(object, header + 24, 8) as usize;
        let len = number(object, header + 32, 8) as usize;
        (&object[name..name + end] == want.as_bytes()).then(|| &object[offset..offset + len])
    })
}

#[test]
fn the_vmm_lines_are_the_words_gnu_as_writes() {
    let words = text();
    let count = |mask: u32, want: u32| words.iter().filter(|&&w| w & mask == want).count();
    // `hfence.gvma` with no operand is one whole word.
    assert_eq!(count(!0, 0x6200_0073), 1, "hfence.gvma in {words:08x?}");
    // `hlvx.hu`: funct7 0x32, rs2 3 and funct3 4, with rd and rs1 the compiler's.
    assert_eq!(count(0xfff0_707f, 0x6430_4073), 1, "hlvx.hu in {words:08x?}");
    // A CSR instruction is its number, then rs1, funct3 and rd. `csrr` is `csrrs` from `zero`,
    // and `csrw` is `csrrw` into `zero`.
    let read = |number: u32| count(0xffff_f07f, number << 20 | 0x2073);
    let write = |number: u32| count(0xfff0_7fff, number << 20 | 0x1073);
    assert_eq!(read(0x600), 1, "csrr hstatus in {words:08x?}");
    assert_eq!(read(0x643), 1, "csrr htval in {words:08x?}");
    assert_eq!(read(0x64a), 1, "csrr htinst in {words:08x?}");
    assert_eq!(read(0x280), 1, "csrr vsatp in {words:08x?}");
    assert_eq!(write(0x600), 1, "csrw hstatus in {words:08x?}");
    assert_eq!(write(0x680), 1, "csrw hgatp in {words:08x?}");
}
