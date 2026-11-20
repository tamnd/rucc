//! A subscript off a null pointer, through the optimizer.
//!
//! `ptr_add` is matched by the simplify rules as an add at the address width, so `null + x` looks
//! like `0 + x` to them. Neither `x` on its own nor `x + null` is a pointer, and the verifier the
//! debug compiler runs after every pass refused both. The kernel writes it in lib/test_bitmap.c,
//! where `bitmap_write` is called on a `(void *)0` map to check it does nothing for a zero width.

use std::process::Command;

const TARGET: &str = "x86_64-unknown-linux-gnu";

fn compiles(what: &str, source: &str) {
    let dir = std::env::temp_dir().join(format!("rucc-null-base-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args(["-O2", "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{what} was refused:\n{said}");
    assert!(!said.contains("internal error"), "{what}:\n{said}");
}

#[test]
fn a_subscript_off_a_null_pointer_stays_a_pointer() {
    compiles(
        "subscript",
        "void f(unsigned long i) { unsigned long *map = (void *)0; map[i] &= 3; }\n",
    );
}

#[test]
fn a_bitmap_write_through_a_null_map_compiles() {
    compiles(
        "bitmap",
        r#"
static inline __attribute__((always_inline))
void write(unsigned long *map, unsigned long value, unsigned long start, unsigned long nbits)
{
	unsigned long index = start / 64, offset = start % 64;
	unsigned long mask = ~0UL >> (-nbits & 63);
	if (!nbits || nbits > 64)
		return;
	map[index] &= ~(mask << offset);
	map[index] |= (value & mask) << offset;
	if (64 - offset >= nbits)
		return;
	map[index + 1] |= value >> (64 - offset);
}
void f(void)
{
	unsigned long z = 0;
	write((void *)0, 0, 0, *(volatile unsigned long *)&z);
}
"#,
    );
}
