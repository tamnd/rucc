//! A callee the inliner weighs after cleaning it up is weighed the way gcc's early passes leave it,
//! with a switch whose arms give constants turned into arithmetic and the `expect` of a constant
//! gone, and a call is weighed less the result it hands back as well as the arguments.
//!
//! The shape is `crypto_mldsa_key_size` in crypto/mldsa.c of Linux 7.2: a switch of three key
//! lengths and a `WARN_ON_ONCE(1)` in the default, called by `crypto_mldsa_set_pub_key`, which
//! copies the key into a buffer of the largest length once it has checked the length against what
//! the call says. gcc 13 inlines the call, its ranges then know the length is no longer than the
//! buffer, and the fortified `memcpy` has nothing to warn about. rucc kept the call and the
//! warning with it.

use std::path::PathBuf;
use std::process::Command;

/// The target is written down rather than taken from the host, so the listing is the same on
/// every machine that runs the suite.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// The fixture, written under a directory of its own so that two of these running at once do not
/// write the same file.
fn fixture(source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-cleaned-switch-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// The assembly the compiler writes for that source at `-O2`.
fn compiled(source: &str) -> String {
    let path = fixture(source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args(["-O2", "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "the compiler refused the fixture:\n{said}");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    String::from_utf8(out.stdout).expect("what the compiler writes is text")
}

/// The lines of one function in the listing.
fn body<'a>(listing: &'a str, function: &str) -> Vec<&'a str> {
    let label = format!("{function}:");
    listing
        .lines()
        .skip_while(|line| *line != label)
        .skip(1)
        .take_while(|line| {
            !(line.ends_with(':') && !line.starts_with(['.', '\t', ' ']) && !line.is_empty())
        })
        .collect()
}

/// How many calls to `name` that function makes, a call in tail position being a jump.
fn calls(lines: &[&str], name: &str) -> usize {
    lines
        .iter()
        .filter(|line| {
            let line = line.trim();
            (line.starts_with("call") || line.starts_with("jmp"))
                && line.split_whitespace().nth(1) == Some(name)
        })
        .count()
}

/// The key size and its caller. The `asm` is twelve lines, as long as the kernel's bug table
/// entry on arm64, and the pointer to `key_size` keeps a copy out of line, as the algorithm's
/// table of operations does in the kernel.
const KEY: &str = r#"
struct ctx { char pad[2628]; unsigned strength; };
extern void too_long(void);
static unsigned key_size(struct ctx *c)
{
	switch (c->strength) {
	case 0: return 1312;
	case 1: return 1952;
	case 2: return 2592;
	default: {
		int on = 1;
		if (__builtin_expect(on, 0))
			__asm__ __volatile__("nop; nop; nop; nop; nop; nop; nop; nop; nop; nop; nop; nop");
		return 0;
	}
	}
}
unsigned (*size_of)(struct ctx *) = key_size;
int set_key(struct ctx *c, unsigned len)
{
	if (len != key_size(c))
		return -22;
	if (len > 2592)
		too_long();
	return 0;
}
"#;

#[test]
fn a_switch_of_key_lengths_with_a_warning_is_copied_into_its_caller() {
    let listing = compiled(KEY);
    let set = body(&listing, "set_key");
    assert!(!set.is_empty(), "no set_key in the listing:\n{listing}");
    assert_eq!(calls(&set, "key_size"), 0, "the call stayed:\n{}", set.join("\n"));
    assert_eq!(calls(&set, "too_long"), 0, "the length check stayed:\n{}", set.join("\n"));
}
