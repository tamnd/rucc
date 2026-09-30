//! What the Linux kernel asks the compiler about itself before it builds anything, asked of the
//! binary with a GCC release claimed.
//!
//! Design: `spec/04-driver-and-cli.md` sections 4.5 and 4.6.
//!
//! Kconfig runs `scripts/cc-version.sh $(CC)`, which pipes a few lines into
//! `$(CC) -E -P -x c -` and reads back `GCC` and the three `__GNUC__` numbers, turns them into
//! `10000x + 100y + z` and refuses a compiler older than `scripts/min-tool-version.sh gcc`, which
//! is 8.1.0 in 7.2.8. `CC_IS_GCC` is that first word, and `GCC_VERSION` is the number. The top
//! Makefile copies the first line of `$(CC) --version` into `CONFIG_CC_VERSION_TEXT`, kernels from
//! 4.18 to 5.11 set `CC_IS_GCC` from `grep gcc` on the same line, and `GCC_PLUGINS` is offered only
//! when `include/plugin-version.h` is under what `$(CC) -print-file-name=plugin` prints. Each of
//! those is the command line the kernel writes, word for word.
//!
//! The parsing in `cc_version` is the script's own, done in Rust because the suite runs no shell
//! scripts. These use `/dev/null` as the kernel does, so they are compiled on Unix only.

#![cfg(unix)]

use std::io::Write as _;
use std::process::{Command, Stdio};

/// The target is written down rather than taken from the host, so this is the same question on
/// every machine that runs the suite.
const TARGET: &str = "--target=x86_64-unknown-linux-gnu";

/// What `scripts/cc-version.sh` feeds the preprocessor, as the here document in the script has it
/// once the tabs are stripped.
const PROBE: &str = "\
#if defined(__clang__)
Clang	__clang_major__  __clang_minor__  __clang_patchlevel__
#elif defined(__GNUC__)
GCC	__GNUC__  __GNUC_MINOR__  __GNUC_PATCHLEVEL__
#else
unknown
#endif
";

/// The compiler as `CC` names it, with these flags after it.
fn rucc(flags: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_rucc"));
    command.env("LC_ALL", "C").arg(TARGET).args(flags);
    command
}

/// Standard output of the compiler given these flags and this input.
fn run(flags: &[&str], input: &str) -> (bool, String) {
    let mut child = rucc(flags)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("the compiler is built before its own tests run");
    let mut stdin = child.stdin.take().expect("a pipe to write the input into");
    stdin.write_all(input.as_bytes()).expect("the input can be written");
    drop(stdin);
    let out = child.wait_with_output().expect("the compiler finished");
    (out.status.success(), String::from_utf8(out.stdout).expect("the answer is text"))
}

/// `x.y.z` as the script's `get_canonical_version` has it.
fn canonical(version: &str) -> u32 {
    let mut parts = version.split('.').map(|part| part.parse::<u32>().unwrap_or(0));
    let mut next = || parts.next().unwrap_or(0);
    10000 * next() + 100 * next() + next()
}

/// What `scripts/cc-version.sh` prints for a compiler given these flags, or why it would stop.
fn cc_version(flags: &[&str]) -> Result<String, String> {
    let mut line = flags.to_vec();
    line.extend(["-E", "-P", "-x", "c", "-"]);
    let (_, out) = run(&line, PROBE);
    let words: Vec<&str> = out.split_whitespace().collect();
    let (name, min) = match words.first() {
        Some(&"GCC") => ("GCC", "8.1.0"),
        Some(&"Clang") => ("Clang", "17.0.1"),
        _ => return Err(format!("unknown C compiler, the preprocessor said `{out}`")),
    };
    let version = words.get(1..4).map(|v| v.join(".")).unwrap_or_default();
    if canonical(&version) < canonical(min) {
        return Err(format!("C compiler is too old, {name} {version} is below {min}"));
    }
    Ok(format!("{name} {}", canonical(&version)))
}

/// The first line of `$(CC) --version`, which is `CONFIG_CC_VERSION_TEXT`.
fn version_text(flags: &[&str]) -> String {
    let mut line = flags.to_vec();
    line.push("--version");
    let (ok, out) = run(&line, "");
    assert!(ok, "--version exits zero");
    out.lines().next().unwrap_or_default().to_owned()
}

/// Whether Kconfig would offer `GCC_PLUGINS`, which is
/// `test -e $(shell,$(CC) -print-file-name=plugin)/include/plugin-version.h`.
fn gcc_plugins(flags: &[&str]) -> bool {
    let mut line = flags.to_vec();
    line.push("-print-file-name=plugin");
    let (ok, out) = run(&line, "");
    assert!(ok, "-print-file-name= exits zero");
    std::path::Path::new(out.trim_end()).join("include/plugin-version.h").exists()
}

#[test]
fn cc_version_reads_the_release_claimed() {
    assert_eq!(cc_version(&["-fgnuc-version=14.2.0"]).as_deref(), Ok("GCC 140200"));
    assert_eq!(cc_version(&["-fgnuc-version=8.1"]).as_deref(), Ok("GCC 80100"));
    assert_eq!(cc_version(&[]).as_deref(), Ok("GCC 160000"));
    // What GCC 4.9 gets from a 7.2.8 tree, which wants 8.1 at least.
    let old = cc_version(&["-fgnuc-version=4.9.4"]).unwrap_err();
    assert!(old.contains("GCC 4.9.4 is below 8.1.0"), "{old}");
}

#[test]
fn the_version_text_names_gcc_and_the_release_and_says_rucc() {
    let text = version_text(&["-fgnuc-version=14.2.0"]);
    // `grep -q gcc` in the Kconfig of 4.18 to 5.11.
    assert!(text.contains("gcc"), "{text}");
    assert!(text.contains("rucc"), "{text}");
    assert!(text.ends_with(" 14.2.0"), "{text}");
    let text = version_text(&["-fgnuc-version=4.9.4"]);
    assert!(text.starts_with("gcc (rucc ") && text.ends_with(" 4.9.4"), "{text}");
    // No claim written, no GCC banner.
    assert!(version_text(&[]).starts_with("rucc "));
}

#[test]
fn there_are_no_gcc_plugins_to_offer() {
    assert!(!gcc_plugins(&["-fgnuc-version=14.2.0"]));
    assert!(!gcc_plugins(&["-fgnuc-version=4.9.4"]));
}

#[test]
fn the_dump_flags_answer_as_the_claimed_release() {
    let ask = |flags: &[&str]| run(flags, "").1.trim_end().to_owned();
    assert_eq!(ask(&["-fgnuc-version=14.2.0", "-dumpversion"]), "14");
    assert_eq!(ask(&["-fgnuc-version=14.2.0", "-dumpfullversion"]), "14.2.0");
    assert_eq!(ask(&["-fgnuc-version=4.9.4", "-dumpversion"]), "4.9.4");
    assert_eq!(ask(&["-fgnuc-version=4.9.4", "-dumpfullversion", "-dumpversion"]), "4.9.4");
}

/// The dialect with no `-std=`, which is what a tree that passes none was written against. The
/// kernel has passed `-std=gnu11` since 5.18 and nothing before 3.18 passed anything.
#[test]
fn the_default_dialect_is_the_claimed_releases() {
    let stdc = |flags: &[&str]| {
        let mut line = flags.to_vec();
        line.extend(["-dM", "-E", "-x", "c", "-"]);
        let (ok, out) = run(&line, "");
        assert!(ok, "the macro dump runs");
        let version = out
            .lines()
            .find_map(|l| l.strip_prefix("#define __STDC_VERSION__ "))
            .map(str::to_owned);
        (version, out.contains("#define __GNUC_GNU_INLINE__ 1"))
    };
    assert_eq!(stdc(&["-fgnuc-version=4.9.4"]), (None, true), "gnu89 has no __STDC_VERSION__");
    assert_eq!(stdc(&["-fgnuc-version=7.5.0"]), (Some("201112L".to_owned()), false));
    assert_eq!(stdc(&["-fgnuc-version=14.2.0"]), (Some("201710L".to_owned()), false));
    assert_eq!(stdc(&["-fgnuc-version=15.1.0"]), (Some("202311L".to_owned()), false));
    assert_eq!(stdc(&["-fgnuc-version=4.9.4", "-std=gnu11"]), (Some("201112L".to_owned()), false));
}
