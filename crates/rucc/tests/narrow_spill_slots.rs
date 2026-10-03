//! A `double` or a `float` spilled out of a vector register takes a slot of its own size.
//!
//! The slot used to be as wide as the register class, sixteen bytes and sixteen aligned, and was
//! written with `movaps`, so a function keeping a few doubles across calls paid twice the bytes
//! gcc does. tamnd/rucc#2206 has `weigh` below at 96 bytes where gcc 14 takes 64, and the same
//! slots all over Postgres: `relation_needs_vacanalyze` has eight of them. A scalar now goes into
//! eight or four bytes with `movsd` or `movss`, and only a value that fills the register keeps
//! sixteen.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The reduced program from the issue, the same shape with floats, and the same shape with a
/// whole vector, which has to keep its sixteen bytes.
const SOURCE: &str = r"
extern double measure(double x);
extern float sense(float x);

typedef double pair __attribute__((vector_size(16)));
extern pair twist(pair x);

double weigh(double x, double y, double z)
{
	double a = measure(x);
	double b = measure(y);
	double c = measure(z);

	return a * 60000.0 + b * 60000.0 + c + x + y + z;
}

float poll(float x, float y, float z)
{
	float a = sense(x);
	float b = sense(y);
	float c = sense(z);

	return a * 2.0f + b * 3.0f + c + x + y + z;
}

pair spin(pair x, pair y)
{
	pair a = twist(x);
	pair b = twist(y);

	return a + b + x + y;
}
";

/// A directory of its own for one compile, so that two tests running at once do not share files.
fn scratch(what: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rucc-narrow-{}-{n}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir
}

/// The `.su` and the assembly for the fixture compiled with `flags`.
fn compile(flags: &[&str]) -> (String, String) {
    let dir = scratch(&flags.join(""));
    std::fs::write(dir.join("one.c"), SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .current_dir(&dir)
        .arg("--target=x86_64-linux-gnu")
        .args(flags)
        .args(["-fstack-usage", "-S", "one.c", "-o", "one.s"])
        .output()
        .expect("the compiler is built before its own tests run");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let usage = std::fs::read_to_string(dir.join("one.su")).expect("one.su was written");
    let asm = std::fs::read_to_string(dir.join("one.s")).expect("one.s was written");
    let _ = std::fs::remove_dir_all(&dir);
    (usage, asm)
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

/// The instructions of one function, from its label to the end of its unwind information.
fn body<'a>(asm: &'a str, function: &str) -> Vec<&'a str> {
    let label = format!("{function}:");
    asm.lines()
        .skip_while(|line| line.trim() != label)
        .take_while(|line| !line.contains(".cfi_endproc"))
        .map(str::trim)
        .collect()
}

/// The lines of a function that move a register of the vector file to or from the frame, which
/// at `-O0` includes the locals the program wrote as well as the spills.
fn frame_moves<'a>(lines: &[&'a str]) -> Vec<&'a str> {
    lines
        .iter()
        .copied()
        .filter(|line| {
            (line.contains("(%rsp)") || line.contains("(%rbp)")) && line.contains("%xmm")
        })
        .collect()
}

#[test]
fn a_spilled_double_takes_eight_bytes() {
    for level in ["-O0", "-O2"] {
        let (usage, asm) = compile(&[level]);
        let lines = body(&asm, "weigh");
        let moves = frame_moves(&lines);
        assert!(!moves.is_empty(), "{level}: weigh spills nothing\n{}", lines.join("\n"));
        for line in &moves {
            assert!(line.starts_with("movsd"), "{level}: weigh spills with {line}");
        }
        // Five doubles over the calls, which is forty bytes and the return address, where it was
        // five slots of sixteen and was 96. gcc 14 takes 64.
        let bytes = frame(&usage, "weigh");
        assert!(bytes <= 64, "{level}: weigh takes {bytes} bytes\n{usage}");
        if level == "-O2" {
            assert_eq!(bytes, 48, "weigh\n{usage}");
        }
    }
}

#[test]
fn a_spilled_float_takes_four_bytes() {
    for level in ["-O0", "-O2"] {
        let (usage, asm) = compile(&[level]);
        let lines = body(&asm, "poll");
        let moves = frame_moves(&lines);
        assert!(!moves.is_empty(), "{level}: poll spills nothing\n{}", lines.join("\n"));
        for line in &moves {
            assert!(line.starts_with("movss"), "{level}: poll spills with {line}");
        }
        // Five floats are twenty bytes, which round up to twenty four with the return address on
        // top.
        let bytes = frame(&usage, "poll");
        assert!(bytes <= 48, "{level}: poll takes {bytes} bytes\n{usage}");
        if level == "-O2" {
            assert_eq!(bytes, 32, "poll\n{usage}");
        }
    }
}

#[test]
fn a_spilled_vector_keeps_the_whole_register() {
    for level in ["-O0", "-O2"] {
        let (usage, asm) = compile(&[level]);
        let lines = body(&asm, "spin");
        let moves = frame_moves(&lines);
        assert!(!moves.is_empty(), "{level}: spin spills nothing\n{}", lines.join("\n"));
        for line in &moves {
            let narrow = line.starts_with("movsd") || line.starts_with("movss");
            assert!(!narrow, "{level}: spin spills with {line}\n{}", lines.join("\n"));
        }
        // Three vectors over the calls at sixteen bytes each.
        let bytes = frame(&usage, "spin");
        assert!(bytes >= 3 * 16, "{level}: spin takes {bytes} bytes\n{usage}");
    }
}

/// Whether the compiler finished, and what it said.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn run(dir: &std::path::Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(args)
        .current_dir(dir)
        .output()
        .expect("the compiler is built before its own tests run");
    (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
}

/// The same functions with the callees defined and kept out of line, so the values spilled over
/// the calls come back from their narrow slots and the answer says whether they came back whole.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const PROGRAM: &str = r"
#define OUT __attribute__((noinline))
typedef double pair __attribute__((vector_size(16)));

OUT double measure(double x) { return x * 0.5 + 1.0; }
OUT float sense(float x) { return x * 0.25f - 1.0f; }
OUT pair twist(pair x) { pair one = { 1.0, -1.0 }; return x * one; }

OUT double weigh(double x, double y, double z)
{
	double a = measure(x);
	double b = measure(y);
	double c = measure(z);

	return a * 60000.0 + b * 60000.0 + c + x + y + z;
}

OUT float poll(float x, float y, float z)
{
	float a = sense(x);
	float b = sense(y);
	float c = sense(z);

	return a * 2.0f + b * 3.0f + c + x + y + z;
}

OUT pair spin(pair x, pair y)
{
	pair a = twist(x);
	pair b = twist(y);

	return a + b + x + y;
}

int main(void)
{
	volatile double x = 3.0, y = 5.0, z = 7.0;
	volatile float f = 4.0f, g = 8.0f, h = 12.0f;
	pair p = { 1.5, 2.5 }, q = { 3.5, 4.5 };
	pair r = spin(p, q);

	if (weigh(x, y, z) != 2.5 * 60000.0 + 3.5 * 60000.0 + 4.5 + 15.0)
		return 1;
	if (poll(f, g, h) != 0.0f * 2.0f + 1.0f * 3.0f + 2.0f + 24.0f)
		return 2;
	if (r[0] != 10.0 || r[1] != 0.0)
		return 3;
	return 0;
}
";

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn values_spilled_into_narrow_slots_come_back_whole() {
    let dir = scratch("run");
    let dir = dir.canonicalize().expect("the directory is there");
    std::fs::write(dir.join("a.c"), PROGRAM).expect("the fixture can be written");
    for level in ["-O0", "-O1", "-O2"] {
        let (ok, said) = run(&dir, &[level, "a.c", "-o", "prog"]);
        assert!(ok, "{level}: {said}");
        let status = Command::new(dir.join("prog")).status().expect("what was linked can be run");
        assert!(status.success(), "{level}: the program got the wrong answer: {status}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
