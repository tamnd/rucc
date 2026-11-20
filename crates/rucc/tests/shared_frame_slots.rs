//! Locals in blocks that never overlap share a slot when their addresses go to a call.
//!
//! The code generator shares the bytes of two locals only when it can follow where their addresses
//! go, and an array handed to a call is one it cannot follow. A function with a buffer in each of
//! four blocks one after the other kept four buffers where gcc keeps one, which is the shape of
//! `do_select` in the kernel and of the `frame-size` cases in rucc-corpus. The walk that still has
//! the blocks gives them one `alloca` instead, and the inliner does the same for the buffers of
//! bodies it splices into one caller, which is the other half of the same shape in the kernel.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

const SOURCE: &str = r"
extern void fill(unsigned int *buf, unsigned int n);
extern void fillc(char *buf, unsigned int n);

unsigned int apart(unsigned int i)
{
	unsigned int s = 0;
	if (i & 1) {
		unsigned int a[160];
		fill(a, 160);
		s += a[3];
	}
	if (i & 2) {
		unsigned int b[160];
		fill(b, 160);
		s += b[5];
	}
	switch (i >> 2) {
	case 0: {
		unsigned int c[160];
		fill(c, 160);
		s += c[7];
		break;
	}
	default: {
		unsigned int d[160];
		fill(d, 160);
		s += d[9];
		break;
	}
	}
	return s;
}

unsigned int nested(unsigned int i)
{
	unsigned int s = 0;
	{
		unsigned int a[160];
		fill(a, 160);
		if (i) {
			unsigned int b[160];
			fill(b, 160);
			s += b[1];
		}
		s += a[2];
	}
	return s;
}

static inline __attribute__((always_inline)) unsigned int part(unsigned int x)
{
	unsigned int buf[160];
	fill(buf, 160);
	return buf[x & 7];
}

unsigned int helpers(unsigned int x)
{
	x = part(x);
	x = part(x + 1);
	x = part(x + 2);
	return part(x + 3);
}

unsigned int sizes(unsigned int i)
{
	unsigned int s = 0;
	if (i) {
		unsigned int a[160];
		fill(a, 160);
		s += a[3];
	} else {
		char b[200];
		fillc(b, 200);
		s += b[5];
	}
	return s;
}
";

fn scratch(what: &str) -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("rucc-scoped-{}-{n}-{what}", std::process::id()));
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

/// One buffer of the fixture.
const BUFFER: u32 = 640;

#[test]
fn four_buffers_in_four_blocks_are_one_buffer_at_o2() {
    let usage = usage(&["-O2"]);
    let bytes = frame(&usage, "apart");
    assert!(bytes < 2 * BUFFER, "apart takes {bytes} bytes\n{usage}");
}

#[test]
fn four_inlined_helpers_with_a_buffer_each_are_one_buffer_at_o2() {
    let usage = usage(&["-O2"]);
    let bytes = frame(&usage, "helpers");
    assert!(bytes < 2 * BUFFER, "helpers takes {bytes} bytes\n{usage}");
}

#[test]
fn a_block_inside_another_does_not_share_with_it() {
    let usage = usage(&["-O2"]);
    let bytes = frame(&usage, "nested");
    assert!(bytes >= 2 * BUFFER, "nested takes {bytes} bytes\n{usage}");
}

#[test]
fn locals_of_two_sizes_share_only_in_the_frame() {
    // The front end gives each its own local, since the size of the local is the size
    // `__builtin_object_size` reads and one local for both would tell the smaller one it is as
    // large as the larger. `rucc_opt::objsize` has answered that before the back end lays out the
    // frame, so the slot allocator can still put the two in the same bytes, and the frame holds
    // the larger of them rather than both.
    let usage = usage(&["-O2"]);
    let bytes = frame(&usage, "sizes");
    assert!(bytes < BUFFER + 200, "sizes takes {bytes} bytes\n{usage}");
}

#[test]
fn nothing_is_shared_at_o0_or_under_stack_reuse_none() {
    for flags in [&["-O0"][..], &["-O2", "-fstack-reuse=none"]] {
        let usage = usage(flags);
        for function in ["apart", "helpers"] {
            let bytes = frame(&usage, function);
            assert!(bytes >= 4 * BUFFER, "{flags:?}: {function} takes {bytes} bytes\n{usage}");
        }
    }
}
