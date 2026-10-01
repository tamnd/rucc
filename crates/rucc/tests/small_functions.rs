//! Which calls to a function nobody declared `inline` are copied into the caller at `-O2`.
//!
//! That is gcc's `-finline-small-functions`, which copies a body when the copy grows the caller by
//! less than `max-inline-insns-auto`, and the kernel is written knowing it. A function gcc copies
//! and this does not leaves a call where gcc has none, and one this copies and gcc does not puts
//! its `WARN_ON` into every caller, so the tables of both disagree object by object. How large a
//! body is has to be measured the way gcc measures it for the two to agree.

use std::path::PathBuf;
use std::process::Command;

/// The target is written down rather than taken from the host, so the listing is the same on
/// every machine that runs the suite.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// The fixture, written under a directory of its own so that two of these running at once do not
/// write the same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-small-fns-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// The assembly the compiler writes for that source with those flags.
fn asm(what: &str, flags: &[&str], source: &str) -> String {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args(flags)
        .args(["-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let said = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "the compiler refused the fixture:\n{said}");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    String::from_utf8(out.stdout).expect("what the compiler writes is text")
}

/// How many calls to `name` the listing makes.
fn calls(listing: &str, name: &str) -> usize {
    listing.lines().filter(|line| line.trim() == format!("call\t{name}")).count()
}

const SMALL: &str = "int scale(int x) { return x * 3 + x / 7; }\n\
                     int use(int a, int b) { return scale(a) + scale(b); }\n";

#[test]
fn a_small_function_is_copied_at_o2_and_not_at_o1() {
    assert_eq!(calls(&asm("o2", &["-O2"], SMALL), "scale"), 0);
    assert_eq!(calls(&asm("o1", &["-O1"], SMALL), "scale"), 2);
    assert_eq!(calls(&asm("off", &["-O2", "-fno-inline-small-functions"], SMALL), "scale"), 2);
}

/// A switch is two for each label to gcc and each line of an `asm` is one, so this body is about
/// thirty and stays a call, the way `nl80211_chan_width_to_mhz` does in the kernel.
#[test]
fn a_switch_and_a_long_asm_weigh_what_they_weigh_to_gcc() {
    let source = "int once;\n\
        int width(int w) {\n\
            switch (w) {\n\
            case 0: case 1: return 1;\n\
            case 2: return 2;\n\
            case 3: case 4: return 4;\n\
            case 5: return 8;\n\
            case 8: return 16;\n\
            case 9: return 20;\n\
            case 10: return 40;\n\
            case 11: return 80;\n\
            case 12: return 160;\n\
            case 13: return 320;\n\
            }\n\
            asm volatile(\"1: nop\\n\\t.pushsection .discard.x; .long 1b - .; .popsection\");\n\
            once = 1;\n\
            return -1;\n\
        }\n\
        int use(int a, int b) { return width(a) + width(b) * 3; }\n";
    assert_eq!(calls(&asm("switch", &["-O2"], source), "width"), 2);
}

/// An `asm inline` is one however many lines it has, and a plain one is each of them.
#[test]
fn an_asm_written_inline_weighs_one() {
    let body = "\"1: nop\\n\\t.pushsection .discard.x; .long 1b - .; .popsection\\n\\tnop\\n\\t\
                nop\\n\\tnop\\n\\tnop\\n\\tnop\\n\\tnop\\n\\tnop\\n\\tnop\\n\\tnop\\n\\tnop\\n\\tnop\\n\\t\
                nop\\n\\tnop\\n\\tnop\\n\\tnop\\n\\tnop\\n\\tnop\\n\\tnop\"";
    let source = |qualifier: &str| {
        format!(
            "int mark(int x) {{ asm {qualifier}({body}); return x + 1; }}\n\
             int use(int a) {{ return mark(a) * mark(a + 1); }}\n"
        )
    };
    assert_eq!(calls(&asm("inline", &["-O2"], &source("inline")), "mark"), 0);
    assert_eq!(calls(&asm("plain", &["-O2"], &source("volatile")), "mark"), 2);
}
