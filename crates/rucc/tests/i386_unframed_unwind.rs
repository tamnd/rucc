//! The unwind rows of an i386 function whose frame goes at `-Os` once its calls push their
//! arguments.
//!
//! The prologue remembers the body's rules on its `subl`, and each epilogue that is not the last
//! one puts them back after its `ret`. When the `subl` went, its rows went with it, so the
//! assembler stopped at a `.cfi_restore_state` with nothing remembered (E0653, NetBSD's
//! `pcio.c`, tamnd/rucc#4512). The pushes after such a `ret` were also counted from where the
//! epilogue left the frame and not from the body's rules.

use std::process::Command;

/// No register saved, so nothing is left in the prologue once the `subl` goes.
const BARE: &str = "\
extern int g(int, int);
int s(int x)
{
	if (x)
		return g(x, 1) + 1;
	g(2, x);
	return 3;
}
";

/// `%ebx` saved, so the `pushl` of it is the prologue once the `subl` goes. The shape of
/// `putchar` in `pcio.c`.
const SAVED: &str = "\
extern int g(int, int);
extern void h(int) __attribute__((regparm(1)));
extern int dev, port;
static inline void put(int c)
{
	switch (dev) {
	case 0:
		h(c);
		break;
	case 1: case 2: case 3: case 4:
		g(port, c);
		break;
	}
}
void f(int c)
{
	if (c == 10)
		put(13);
	put(c);
}
";

fn compile(source: &str, name: &str, out: &str) -> std::process::Output {
    compile_with(source, name, out, &[])
}

/// The same with those flags too.
fn compile_with(source: &str, name: &str, out: &str, extra: &[&str]) -> std::process::Output {
    let dir = std::env::temp_dir().join(format!("rucc-unframed-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    let mut command = Command::new(env!("CARGO_BIN_EXE_rucc"));
    // With the unwind table asked for, since a 32 bit x86 unit on a BSD has none by default
    // (#4513), and without one there are no rows to check. A unit that asks for one, or that is
    // built with `-fexceptions`, still has the rows this is about.
    command.args(["--target=i486--netbsdelf", "-ffreestanding", "-Os"]);
    command.arg("-fasynchronous-unwind-tables").args(extra);
    if out == "-" {
        command.args(["-S", "-o", "-"]);
    } else {
        command.args(["-c", "-o"]).arg(dir.join(out));
    }
    let output =
        command.arg(&path).output().expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    output
}

/// The listing, and that each `.cfi_restore_state` in it has a state remembered in front of it.
fn listing(source: &str, name: &str) -> String {
    listing_with(source, name, &[])
}

/// The same with those flags too.
fn listing_with(source: &str, name: &str, extra: &[&str]) -> String {
    let out = compile_with(source, name, "-", extra);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let asm = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut depth = 0_u32;
    for line in asm.lines().map(str::trim) {
        match line {
            ".cfi_startproc" => depth = 0,
            ".cfi_remember_state" => depth += 1,
            ".cfi_restore_state" => {
                assert!(depth > 0, "a restore with nothing remembered\n{asm}");
                depth -= 1;
            }
            _ => {}
        }
    }
    asm
}

#[test]
fn a_function_with_nothing_saved_assembles() {
    let out = compile(BARE, "bare", "one.o");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn a_function_with_a_register_saved_assembles() {
    let out = compile(SAVED, "saved", "one.o");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn the_pushes_after_a_ret_count_the_frame_from_the_body() {
    let asm = listing(BARE, "rows");
    assert!(!asm.contains("subl"), "the frame goes\n{asm}");
    let rows: Vec<&str> =
        asm.lines().filter_map(|line| line.trim().strip_prefix(".cfi_def_cfa_offset ")).collect();
    // Each call: two pushes and the way back, and the first `ret` changes nothing.
    assert_eq!(rows, ["8", "12", "4", "8", "12", "4"], "{asm}");
}

#[test]
fn the_saved_register_is_remembered_after_its_push() {
    let asm = listing(SAVED, "remembered");
    assert!(!asm.contains("subl"), "the frame goes\n{asm}");
    let lines: Vec<&str> = asm.lines().map(str::trim).collect();
    let push = lines.iter().position(|&line| line == "pushl\t%ebx").expect("a push of %ebx");
    assert_eq!(
        lines[push + 1..push + 4],
        [".cfi_def_cfa_offset 8", ".cfi_offset 3, -8", ".cfi_remember_state"],
        "{asm}"
    );
}

/// With `-g` the pushes run too, and the rows are the same ones, with each state put back
/// remembered first.
#[test]
fn the_rows_are_the_same_with_the_debugging_information() {
    let rows = |asm: &str| -> Vec<String> {
        (asm.lines().map(str::trim))
            .filter(|line| line.starts_with(".cfi_") && !line.starts_with(".cfi_sections"))
            .map(str::to_owned)
            .collect()
    };
    for (name, source) in [("bare", BARE), ("saved", SAVED)] {
        let plain = listing(source, &format!("{name}-plain"));
        let debug = listing_with(source, &format!("{name}-g"), &["-g"]);
        assert_eq!(rows(&plain), rows(&debug), "{name}\n{debug}");
        let out = compile_with(source, &format!("{name}-g-object"), "one.o", &["-g"]);
        assert!(out.status.success(), "{name}: {}", String::from_utf8_lossy(&out.stderr));
    }
}
