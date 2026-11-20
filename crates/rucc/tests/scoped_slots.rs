//! Locals of blocks that never run together share their bytes even when their addresses are
//! handed to a call.
//!
//! Before, a local whose address went anywhere this compiler could not follow kept bytes of its
//! own for the whole of the function, which is the right answer for a local of the function's
//! outermost block and a waste for one declared inside an arm of an `if`. Postgres fills in a
//! structure of a few hundred bytes in each arm of the `if` chains in its redo routines and hands
//! it to a parser by address, and rucc gave every one of them its own place: tamnd/rucc#2201 has
//! `xact_redo` at 1296 bytes where gcc 14 takes 368. The front end now says where each such local
//! stops, and the slots are shared across those ends.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The reduced program from the issue: four structures of 288 bytes, one in each arm, each passed
/// by address to a call that fills it in and then to one that reads it.
const SOURCE: &str = r"
struct parsed {
	long words[36];
};

extern void parse_commit(const char *rec, struct parsed *out);
extern void parse_abort(const char *rec, struct parsed *out);
extern void parse_prepare(const char *rec, struct parsed *out);
extern void parse_assign(const char *rec, struct parsed *out);
extern void replay(const struct parsed *what, int kind);

void redo(const char *rec, int info)
{
	if (info == 0) {
		struct parsed parsed;
		parse_commit(rec, &parsed);
		replay(&parsed, 0);
	} else if (info == 1) {
		struct parsed parsed;
		parse_abort(rec, &parsed);
		replay(&parsed, 1);
	} else if (info == 2) {
		struct parsed parsed;
		parse_prepare(rec, &parsed);
		replay(&parsed, 2);
	} else {
		struct parsed parsed;
		parse_assign(rec, &parsed);
		replay(&parsed, 3);
	}
}
";

/// The size of one of the four structures, and so the least the frame can be.
const ONE: u32 = 288;

/// A directory of its own for one compile, so that two tests running at once do not share files.
fn scratch(what: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rucc-scoped-{}-{n}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir
}

/// The `.su` one compile of a program wrote.
fn usage(target: &str, source: &str, args: &[&str]) -> String {
    let dir = scratch(&format!("{target}{}", args.join("")));
    std::fs::write(dir.join("one.c"), source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .current_dir(&dir)
        .arg(format!("--target={target}"))
        .args(args)
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

const TARGETS: [&str; 2] = ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu"];

/// gcc 14 takes 336 bytes at `-O2`, and rucc took 1184 on both machines, which is the four
/// structures side by side. Now it is one structure and what the function needs besides, 320 bytes
/// on both, and no more than gcc's.
#[test]
fn four_structures_in_four_arms_are_one_structure_s_bytes() {
    for target in TARGETS {
        for args in [&["-O2"][..], &["-O1"], &["-O0", "-fstack-reuse=all"]] {
            let bytes = frame(&usage(target, SOURCE, args), "redo");
            assert!(
                (ONE..=ONE + 48).contains(&bytes),
                "{target} {args:?}: {bytes} bytes for redo, wanted one structure's worth"
            );
        }
    }
}

/// `-O0` keeps every local in a place of its own, so that a person stepping through the code sees
/// each of them, and so does `-fstack-reuse=none` at any level.
#[test]
fn nothing_shares_where_stack_reuse_is_off() {
    for target in TARGETS {
        for args in [&["-O0"][..], &["-O2", "-fstack-reuse=none"]] {
            let bytes = frame(&usage(target, SOURCE, args), "redo");
            assert!(bytes >= 4 * ONE, "{target} {args:?}: {bytes} bytes for redo");
        }
    }
}

/// Two arrays in two blocks one after the other, inside a loop, each filled in through a pointer
/// handed to a function the optimizer does not look into and read back afterwards.
const TWO: &str = r#"
extern int printf(const char *, ...);

__attribute__((noinline)) void fill(int *into, int n, int from)
{
	for (int i = 0; i < n; i++)
		into[i] = from + i;
}

__attribute__((noinline)) int sum(const int *what, int n)
{
	int total = 0;
	for (int i = 0; i < n; i++)
		total += what[i];
	return total;
}

__attribute__((noinline)) int first_of(const int *what)
{
	return what[0];
}

__attribute__((noinline)) int both(int turns)
{
	int total = 0;
	for (int turn = 0; turn < turns; turn++) {
		{
			int first[100];
			fill(first, 100, turn);
			total += sum(first, 100) + first_of(first);
		}
		{
			int second[100];
			fill(second, 100, 1000 + turn);
			total += sum(second, 100) - first_of(second);
		}
	}
	return total;
}

int main(void)
{
	printf("%d\n", both(3));
	return 0;
}
"#;

/// The two arrays are one array's bytes, on both machines: 448 on x86-64 and 464 on AArch64,
/// where they were 848 and 864 with an array each.
#[test]
fn two_arrays_in_blocks_one_after_the_other_share_their_bytes() {
    for target in TARGETS {
        let bytes = frame(&usage(target, TWO, &["-O2"]), "both");
        assert!((400..=480).contains(&bytes), "{target}: {bytes} bytes for both");
    }
}

/// And the answers are still the answers, which is what would change if the second array were
/// written over the first while the first was still being read.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn two_arrays_that_share_their_bytes_still_give_the_right_answers() {
    let mut total: i64 = 0;
    for turn in 0..3 {
        total += (0..100).map(|i| turn + i).sum::<i64>() + turn;
        total += (0..100).map(|i| 1000 + turn + i).sum::<i64>() - (1000 + turn);
    }
    for level in ["-O2", "-O0"] {
        let dir = scratch(&format!("run{level}"));
        std::fs::write(dir.join("two.c"), TWO).expect("the fixture can be written");
        let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
            .current_dir(&dir)
            .args([level, "-fstack-reuse=all", "two.c", "-o", "two"])
            .output()
            .expect("the compiler is built before its own tests run");
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        let ran = Command::new(dir.join("two")).output().expect("what was linked can be run");
        assert!(ran.status.success(), "{level}");
        assert_eq!(String::from_utf8_lossy(&ran.stdout).trim(), total.to_string(), "{level}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
