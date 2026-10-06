//! `-fno-zero-initialized-in-bss`, end to end: a variable the program gave an initializer of all
//! zeroes is in `.data`, or `.tdata` when it is thread-local, and one it gave no initializer is
//! still in `.bss`, which is where gcc puts each of them.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The ELF targets the listing is read for, written down rather than taken from the host.
const TARGETS: [&str; 3] =
    ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu", "i686-unknown-linux-gnu"];

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-zero-bss-{}-{what}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir.canonicalize().expect("the directory is there")
}

/// The listing of a source that has to compile without a word.
fn listing(what: &str, target: &str, flags: &[&str], source: &str) -> String {
    let dir = dir(what);
    std::fs::write(dir.join("a.c"), source).expect("the fixture can be written");
    let target_flag = format!("--target={target}");
    let mut args = vec![target_flag.as_str(), "-S", "-o", "-"];
    args.extend_from_slice(flags);
    args.push("a.c");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(&args)
        .current_dir(Path::new(&dir))
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{target} {flags:?}: {err}");
    assert_eq!(err, "", "{target} {flags:?}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The name of the section a label is in, from the last directive in front of it that opened one.
fn section<'a>(text: &'a str, label: &str) -> &'a str {
    let lines: Vec<&str> = text.lines().collect();
    let at = lines
        .iter()
        .position(|line| line.strip_suffix(':').is_some_and(|name| name == label))
        .unwrap_or_else(|| panic!("{label} is in the listing:\n{text}"));
    let line = lines[..at]
        .iter()
        .rev()
        .find(|line| {
            line.starts_with("\t.section\t") || matches!(**line, "\t.data" | "\t.bss" | "\t.text")
        })
        .unwrap_or_else(|| panic!("{label} is in a section:\n{text}"));
    let line = line.trim_start_matches("\t.section\t").trim_start_matches('\t');
    line.split(',').next().unwrap_or(line)
}

/// The label a `static` in a function was given, which has the name the program wrote in it.
fn local<'a>(text: &'a str, name: &str) -> &'a str {
    text.lines()
        .filter_map(|line| line.strip_suffix(':'))
        .find(|label| !label.starts_with('.') && label.starts_with(name))
        .unwrap_or_else(|| panic!("{name} is in the listing:\n{text}"))
}

const SOURCE: &str = "\
struct pair { int x, y; };
struct pair zeroed = {};
int array[4] = {0};
static struct pair hidden = {};
int tentative;
static int quiet;
const struct pair fixed = {};
__thread struct pair each = {};
__thread int each_none;
int touch(void) {
    static struct pair inner = {};
    static int inner_none;
    return zeroed.x + array[1] + hidden.y + tentative + quiet + fixed.x + each.y + each_none
        + inner.x + inner_none;
}
";

#[test]
fn a_zero_the_program_wrote_goes_in_data_under_the_flag() {
    for target in TARGETS {
        let text = listing("off", target, &["-fno-zero-initialized-in-bss"], SOURCE);
        for name in ["zeroed", "array", "hidden"] {
            assert_eq!(section(&text, name), ".data", "{target} {name}:\n{text}");
        }
        assert_eq!(section(&text, local(&text, "inner.")), ".data", "{target}:\n{text}");
        assert_eq!(section(&text, "each"), ".tdata", "{target}:\n{text}");
        // No initializer is no zero the program wrote, so each of these is where it always was.
        for name in ["tentative", "quiet"] {
            assert_eq!(section(&text, name), ".bss", "{target} {name}:\n{text}");
        }
        assert_eq!(section(&text, local(&text, "inner_none.")), ".bss", "{target}:\n{text}");
        assert_eq!(section(&text, "each_none"), ".tbss", "{target}:\n{text}");
        // A constant is read only either way.
        assert_eq!(section(&text, "fixed"), ".rodata", "{target}:\n{text}");
    }
}

#[test]
fn the_default_and_the_positive_form_keep_zeroes_in_bss() {
    for target in TARGETS {
        for flags in [&[][..], &["-fno-zero-initialized-in-bss", "-fzero-initialized-in-bss"]] {
            let text = listing("on", target, flags, SOURCE);
            for name in ["zeroed", "hidden", "tentative", "quiet"] {
                assert_eq!(section(&text, name), ".bss", "{target} {flags:?} {name}:\n{text}");
            }
            assert_eq!(section(&text, "each"), ".tbss", "{target} {flags:?}:\n{text}");
        }
    }
}
