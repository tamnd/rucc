//! A structure passed by value in the argument area is used where it arrived.
//!
//! The parameter used to get a slot of its own and the bytes the caller left were copied into it,
//! so a function handing such a structure on to another call copied it twice, once in and once out,
//! and paid for the slot in between. tamnd/rucc#2209 has `create_default` below at 80 bytes where
//! gcc 14 takes 8, and the same copy in Postgres wherever a `pg_compress_specification` is passed
//! along. The bytes the caller left are the callee's to read and write until it returns, so the
//! body now works on them in place and the call copies them once.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The reduced program from the issue, and the same shape with the structure written to before it
/// is passed on, which the caller's copy has to absorb.
const SOURCE: &str = r"
typedef struct CompressSpec {
	int algorithm;
	int level;
	long workers;
	long long_distance;
	char *detail;
} CompressSpec;

extern void *create_archive(const char *path, int mode, CompressSpec spec, int sync);

void *create_default(const char *path, CompressSpec spec)
{
	return create_archive(path, 1, spec, 1);
}

void *create_tuned(const char *path, CompressSpec spec)
{
	spec.level = 9;
	return create_archive(path, 2, spec, 0);
}
";

/// A directory of its own for one compile, so that two tests running at once do not share files.
fn scratch(what: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rucc-byval-{}-{n}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir
}

/// The `.su` for the fixture compiled with `flags`.
fn usage(flags: &[&str]) -> String {
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

#[test]
fn a_structure_handed_on_is_copied_once_and_has_no_slot() {
    for level in ["-O0", "-O2"] {
        let usage = usage(&[level]);
        // The 32 bytes of the outgoing argument area and the return address, rounded up to the
        // sixteen the call wants. It was 80 with the slot. gcc 14 takes 8, because it makes the
        // call in tail position a jump, which is tamnd/rucc#2205.
        for function in ["create_default", "create_tuned"] {
            let bytes = frame(&usage, function);
            assert!(bytes <= 48, "{level}: {function} takes {bytes} bytes\n{usage}");
            if level == "-O2" {
                assert_eq!(bytes, 48, "{function}\n{usage}");
            }
        }
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

/// The same functions with the callee defined and kept out of line, writing to its own copy, and a
/// function that takes the address of its parameter and changes it. The caller's structure has to
/// come out of all of them as it went in.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const PROGRAM: &str = r"
#define OUT __attribute__((noinline))

typedef struct CompressSpec {
	int algorithm;
	int level;
	long workers;
	long long_distance;
	char *detail;
} CompressSpec;

OUT void *create_archive(const char *path, int mode, CompressSpec spec, int sync)
{
	long sum = spec.algorithm * 1000 + spec.level * 100 + spec.workers * 10 + spec.long_distance;
	spec.workers = -1;
	spec.level = -1;
	return (char *)path + sum + mode * 10000 + sync * 100000 + (spec.detail[0] - 'a');
}

OUT void *create_default(const char *path, CompressSpec spec)
{
	return create_archive(path, 1, spec, 1);
}

OUT void *create_tuned(const char *path, CompressSpec spec)
{
	spec.level = 9;
	return create_archive(path, 2, spec, 0);
}

OUT long count(const CompressSpec *spec)
{
	return spec->workers + spec->long_distance;
}

OUT long through(CompressSpec spec)
{
	spec.workers += 40;
	return count(&spec) + spec.workers;
}

static char room[200000];

int main(void)
{
	char detail[] = { 'c', 0 };
	CompressSpec spec = { 1, 2, 3, 4, detail };
	char *base = room;

	if ((char *)create_default(base, spec) - base != 1234 + 10000 + 100000 + 2)
		return 1;
	if ((char *)create_tuned(base, spec) - base != 1934 + 20000 + 2)
		return 2;
	if (through(spec) != 43 + 4 + 43)
		return 3;
	if (spec.algorithm != 1 || spec.level != 2 || spec.workers != 3 || spec.long_distance != 4)
		return 4;
	return 0;
}
";

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn a_structure_used_in_place_reads_and_writes_its_own_bytes() {
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
