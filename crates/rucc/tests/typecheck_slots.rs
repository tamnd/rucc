//! Two locals compared by address only to check their types give up their slots.
//!
//! The kernel's `typecheck` writes `(void)(&__dummy == &__dummy2)` so that the compiler warns when
//! the two types differ, and `rt2x00_set_field32` in `drivers/net/wireless/ralink/rt2x00` runs it
//! for every register field it sets. rucc kept the comparison, so both locals had their address
//! taken and a slot each for every field, and `rt2800_init_registers` took 2800 bytes of frame where
//! gcc takes 96. Two different slots are never at one address, so the comparison is now a number
//! and the locals go away.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The register macros of `rt2x00reg.h` cut down, with three registers of two fields each.
const SOURCE: &str = r"
struct f32 { unsigned bit_offset; unsigned bit_mask; };
#define F(m) ((struct f32){ __builtin_ctz(m), (m) })
#define SET(reg, fld, v) ({ \
	({ struct f32 __dummy; typeof(fld) __dummy2; (void)(&__dummy == &__dummy2); 1; }); \
	*(reg) &= ~(fld).bit_mask; \
	*(reg) |= ((v) << (fld).bit_offset) & (fld).bit_mask; })

extern unsigned rd(int);
extern void wr(int, unsigned);

int g(int x)
{
	unsigned reg = rd(1);
	SET(&reg, F(0xff), x);
	SET(&reg, F(0xff00), 2);
	wr(1, reg);
	reg = rd(2);
	SET(&reg, F(0xf0), x);
	SET(&reg, F(0x10000), 1);
	wr(2, reg);
	reg = rd(3);
	SET(&reg, F(0xf0), x);
	SET(&reg, F(0x10000), 1);
	wr(3, reg);
	return 0;
}
";

/// A directory of its own for one compile, so that two tests running at once do not share files.
fn scratch(what: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("rucc-typecheck-{}-{n}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir
}

/// The `.su` one compile of a program wrote.
fn usage(target: &str, source: &str) -> String {
    let dir = scratch(target);
    std::fs::write(dir.join("one.c"), source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .current_dir(&dir)
        .arg(format!("--target={target}"))
        .args(["-O2", "-fstack-usage", "-S", "one.c", "-o", "one.s"])
        .output()
        .expect("the compiler is built before its own tests run");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let usage = std::fs::read_to_string(dir.join("one.su")).expect("one.su was written");
    let _ = std::fs::remove_dir_all(&dir);
    usage
}

/// The bytes the `.su` says a function takes.
fn frame(usage: &str, function: &str) -> u32 {
    usage
        .lines()
        .find_map(|line| {
            let mut fields = line.split_whitespace();
            let place = fields.next()?;
            place.ends_with(&format!(":{function}")).then(|| fields.next()?.parse().ok())?
        })
        .unwrap_or_else(|| panic!("no line for {function} in\n{usage}"))
}

/// gcc 14 takes 32 bytes for `g` on AArch64, and rucc took 144. Now it is gcc's 32 there, and no
/// more than that on x86-64 either.
#[test]
fn locals_compared_only_for_their_types_take_no_slots() {
    for target in ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"] {
        let bytes = frame(&usage(target, SOURCE), "g");
        assert!(bytes <= 48, "{target}: {bytes} bytes for g");
    }
}

/// Past the end of one local can be where the next one starts, so that comparison stays as it is
/// and still reads the two addresses.
#[test]
fn one_past_the_end_is_still_compared() {
    let source = r"
int g(void)
{
	int a, b;
	return &a + 1 == &b;
}
";
    let dir = scratch("past");
    std::fs::write(dir.join("one.c"), source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .current_dir(&dir)
        .args(["--target=x86_64-unknown-linux-gnu", "-O2", "-S", "one.c", "-o", "one.s"])
        .output()
        .expect("the compiler is built before its own tests run");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let asm = std::fs::read_to_string(dir.join("one.s")).expect("one.s was written");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(asm.contains("lea"), "the two addresses are not taken:\n{asm}");
}
