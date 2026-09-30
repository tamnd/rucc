//! What the Linux kernel asks the assembler before it builds anything, asked of the binary.
//!
//! Design: `spec/04-driver-and-cli.md` section 4.9.
//!
//! From 5.12 on, Kconfig runs `scripts/as-version.sh $(CC)` before it reads a single option, and
//! the build stops with "unknown assembler invoked" unless what comes back starts with
//! `GNU assembler` and ends with a version. After that, every `as-instr` in Kconfig pipes one
//! instruction into `$(CC) -Wa,--fatal-warnings -c -x assembler-with-cpp -o /dev/null -` and turns
//! a feature on when that exits zero. Both are the command lines the kernel writes, word for word,
//! so that a change in how this compiler reads any of those words shows up here rather than in a
//! kernel build log.
//!
//! The parsing in `as_version` is the script's own, done in Rust because the suite runs no shell
//! scripts: take the first line, split it on spaces, check the first two words, take the last as
//! the version, drop anything after a hyphen and turn `x.y.z` into `10000x + 100y + z`.
//!
//! These use `/dev/null`, as the kernel does, so they are compiled on Unix only.

#![cfg(unix)]

use std::io::Write as _;
use std::process::{Command, Stdio};

/// The target is written down rather than taken from the host, so this is the same question on
/// every machine that runs the suite.
const TARGET: &str = "--target=x86_64-unknown-linux-gnu";

/// What `scripts/as-version.sh` prints for a compiler given these flags, or why it would stop.
fn as_version(flags: &[&str]) -> Result<String, String> {
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .env("LC_ALL", "C")
        .args(flags)
        .args(["-Wa,--version", "-c", "-x", "assembler-with-cpp", "/dev/null", "-o", "/dev/null"])
        .output()
        .expect("the compiler is built before its own tests run");
    let stdout = String::from_utf8(out.stdout).expect("the banner is text");
    let first = stdout.lines().next().unwrap_or("");
    let words: Vec<&str> = first.split(' ').filter(|word| !word.is_empty()).collect();
    if words.len() < 2 || words[0] != "GNU" || words[1] != "assembler" {
        return Err(format!("unknown assembler invoked, the first line was `{first}`"));
    }
    let version = words[words.len() - 1];
    let version = version.split('-').next().unwrap_or(version);
    let mut parts = version.split('.').map(|part| part.parse::<u32>().unwrap_or(0));
    let major = parts.next().unwrap_or(0);
    let minor = parts.next().unwrap_or(0);
    let patch = parts.next().unwrap_or(0);
    Ok(format!("GNU {}", 10000 * major + 100 * minor + patch))
}

/// Whether Kconfig's `as-instr` would say yes to one instruction.
fn as_instr(instruction: &str) -> bool {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args([TARGET, "-m64", "-Wa,--fatal-warnings", "-c", "-x", "assembler-with-cpp"])
        .args(["-o", "/dev/null", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("the compiler is built before its own tests run");
    let mut stdin = child.stdin.take().expect("a pipe to write the instruction into");
    writeln!(stdin, "{instruction}").expect("the instruction can be written");
    drop(stdin);
    child.wait().expect("the compiler finished").success()
}

#[test]
fn as_version_reads_the_version_it_was_given() {
    let answer = as_version(&["-fgnuc-version=14.2.0", "-fgnu-as-version=2.44"]);
    assert_eq!(answer.as_deref(), Ok("GNU 24400"));
    // The era the harness builds a 4.x kernel in, and a point release, which gas prints in full.
    assert_eq!(as_version(&["-fgnu-as-version=2.25"]).as_deref(), Ok("GNU 22500"));
    assert_eq!(as_version(&["-fgnu-as-version=2.35.1"]).as_deref(), Ok("GNU 23501"));
}

#[test]
fn as_version_reads_the_default_without_the_flag() {
    // 2.46, the binutils that was current when GCC 16 came out, since GCC 16 is the default claim.
    assert_eq!(as_version(&[]).as_deref(), Ok("GNU 24600"));
}

#[test]
fn the_banner_is_one_line_on_standard_output_and_nothing_else() {
    for language in ["assembler", "assembler-with-cpp"] {
        let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
            .args(["-fgnu-as-version=2.44", "-Wa,--version", "-c", "-x", language])
            .args(["/dev/null", "-o", "/dev/null"])
            .output()
            .expect("the compiler is built before its own tests run");
        assert!(out.status.success(), "{language}: {}", String::from_utf8_lossy(&out.stderr));
        let stdout = String::from_utf8(out.stdout).expect("the banner is text");
        let expected =
            format!("GNU assembler (rucc {} integrated) 2.44\n", env!("CARGO_PKG_VERSION"));
        assert_eq!(stdout, expected, "{language}");
        assert!(out.stderr.is_empty(), "{language}: {}", String::from_utf8_lossy(&out.stderr));
    }
}

#[test]
fn an_instruction_the_kernel_probes_for_is_assembled_under_fatal_warnings() {
    // `X86_KERNEL_IBT` asks for `endbr64` in arch/x86/Kconfig. The probe passes
    // `-Wa,--fatal-warnings`, so a compiler that refused that flag would say no to every
    // instruction whatever its assembler could encode.
    assert!(as_instr("endbr64"), "endbr64 was refused");
    // And the probe still says no to what the assembler cannot encode, which is the half that
    // keeps a feature switched off when the output would not have it.
    assert!(!as_instr("not_an_instruction %rax"), "a made up mnemonic was taken");
}

#[test]
fn a_warning_in_the_probe_is_fatal_because_the_probe_says_so() {
    // gas stops on `.warning` under `--fatal-warnings`, and a probe that meets one is asking
    // whether the assembler is quiet about the line, so this has to stop too.
    assert!(!as_instr(".warning \"deprecated\"\nendbr64"), "the warning was not fatal");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args([TARGET, "-c", "-x", "assembler", "-o", "/dev/null", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .and_then(|mut child| {
            let mut stdin = child.stdin.take().expect("a pipe");
            stdin.write_all(b".warning \"deprecated\"\nendbr64\n")?;
            drop(stdin);
            child.wait()
        })
        .expect("the compiler ran");
    assert!(out.success(), "without --fatal-warnings a warning does not stop the file");
}
