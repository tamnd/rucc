//! The unwind table an AArch64 Windows function carries, as the listing describes it.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.4.
//!
//! A test of the whole compiler for the same reason `windows_unwind.rs` beside it is one: the
//! frame code decides the prologue, the listing writer reads the codes out of it, and the assembler
//! turns them into `.pdata` and `.xdata`, so each part can be right on its own while a function
//! comes out with a table that unwinds from the wrong place. The codes are the instructions, one
//! each, so what is checked is the pairs of instruction and directive the listing writes, which
//! are what clang writes for the same instructions.

use std::path::PathBuf;
use std::process::Command;

/// The tuple, which is mingw-w64's. Microsoft's has the same frame and the same table.
const TARGET: &str = "aarch64-w64-windows-gnu";

/// The fixture, under a directory of its own so that two of these running at once do not write the
/// same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-arm64-unwind-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// What the compiler writes for that source with those flags, and whether it wrote it.
fn run(what: &str, flags: &[&str], source: &str) -> std::process::Output {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={TARGET}"))
        .args(flags)
        .arg(&path)
        .current_dir(path.parent().expect("the fixture is in a directory"))
        .args(if flags.contains(&"-S") { vec!["-o", "-"] } else { vec!["-o", "one.o"] })
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    out
}

/// The listing for that source.
fn asm(what: &str, flags: &[&str], source: &str) -> String {
    let out = run(what, &[&["-S"], flags].concat(), source);
    assert!(
        out.status.success(),
        "the compiler refused the fixture:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The lines of one function from `.seh_proc` to `.seh_endproc`, without labels, each trimmed and
/// with the tab after a directive's name made a space.
fn described(text: &str, name: &str) -> Vec<String> {
    let open = format!(".seh_proc\t{name}");
    text.lines()
        .map(str::trim)
        .skip_while(|line| *line != open)
        .skip(1)
        .take_while(|line| *line != ".seh_endproc")
        .filter(|line| !line.ends_with(':'))
        .map(|line| line.replace('\t', " "))
        .collect()
}

/// Every instruction of the prologue and of the epilogue has its code after it, the prologue ends
/// where the body starts, and the epilogue is marked from its first restore to the `ret`.
#[test]
fn a_frame_is_described_one_instruction_at_a_time() {
    let text = asm("keep", &["-O2"], "void use(void *);\nint keep(int a) { use(&a); return a; }\n");
    let got = described(&text, "keep");
    let prologue =
        ["stp x29, x30, [sp, #-16]!", ".seh_save_fplr_x 16", "mov x29, sp", ".seh_set_fp"];
    assert_eq!(got[..4], prologue, "{text}");
    assert!(got.iter().any(|line| line == ".seh_endprologue"), "{text}");
    let tail = got.iter().rev().take(4).rev().cloned().collect::<Vec<_>>();
    assert_eq!(
        tail,
        ["ldp x29, x30, [sp], #16", ".seh_save_fplr_x 16", ".seh_endepilogue", "ret"],
        "{text}"
    );
    let start = got.iter().position(|line| line == ".seh_startepilogue");
    let end = got.iter().position(|line| line == ".seh_endprologue");
    assert!(start.is_some() && end < start, "{text}");
}

/// A function with no frame has an empty prologue and no epilogue.
#[test]
fn a_leaf_has_an_empty_prologue() {
    let text = asm("leaf", &["-O2"], "int leaf(int a) { return a + 1; }\n");
    assert_eq!(described(&text, "leaf"), [".seh_endprologue", "add w0, w0, #1", "ret"], "{text}");
}

/// A frame over a page is reached through `__chkstk`, which is two `nop`s and the subtraction
/// after it the whole frame, as clang describes the same three instructions.
#[test]
fn a_frame_reached_through_chkstk_is_described_like_clangs() {
    let text =
        asm("page", &["-O2"], "void use(void *);\nvoid page(void) { char b[4100]; use(b); }\n");
    let got = described(&text, "page");
    let want = ["bl __chkstk", ".seh_nop", "sub sp, sp, x15, lsl #4", ".seh_stackalloc 4112"];
    assert!(got.windows(4).any(|four| four == want), "{text}");
}

/// A body that grows the frame is unwound from the frame pointer, so the prologue ends by saying
/// where the frame pointer is, after every register it saved.
#[test]
fn a_growing_frame_ends_its_prologue_at_the_frame_pointer() {
    let source =
        "void use(void *);\nint vla(int n, int m) { char b[n]; use(b); use(&m); return n + m; }\n";
    for level in ["-O0", "-O2"] {
        let text = asm("vla", &[level], source);
        let got = described(&text, "vla");
        let end = got.iter().position(|line| line == ".seh_endprologue").expect("a prologue");
        assert!(got[end - 1].starts_with(".seh_add_fp "), "{level}\n{text}");
        assert!(got[end - 2].starts_with("add x29, sp, #"), "{level}\n{text}");
    }
}

/// The same when the frame is too big for the frame pointer code to reach, where the frame is
/// taken in the body instead, as clang does it.
#[test]
fn a_growing_frame_too_big_for_the_code_takes_the_frame_in_the_body() {
    let source = "void use(void *);\nint big(int n) { char b[5000]; use(b); char c[n]; use(c); return n; }\n";
    let text = asm("big", &["-O2"], source);
    let got = described(&text, "big");
    let end = got.iter().position(|line| line == ".seh_endprologue").expect("a prologue");
    assert!(got[end - 1].starts_with(".seh_add_fp "), "{text}");
    assert!(got[end + 1..].iter().any(|line| line == "bl __chkstk"), "{text}");
}

/// A frame that forces its own alignment says where the frame pointer is before it aligns.
#[test]
fn a_realigned_frame_restates_the_frame_pointer_before_aligning() {
    let source =
        "void use(void *);\nint al(int n) { _Alignas(64) char b[16]; use(b); return n; }\n";
    let text = asm("al", &["-O2"], source);
    let got = described(&text, "al");
    let and = got.iter().position(|line| line.starts_with("and sp, ")).expect("an alignment");
    let fp = got[..and]
        .iter()
        .rposition(|line| line.starts_with(".seh_") && line != ".seh_nop")
        .expect("a code");
    assert!(got[fp].starts_with(".seh_add_fp ") || got[fp] == ".seh_set_fp", "{text}");
}

/// And the object is written, which is the assembler accepting every one of the directives.
#[test]
fn the_object_is_written() {
    let source = "void use(void *);\n\
        int keep(int a) { use(&a); return a; }\n\
        int vla(int n) { char b[n]; use(b); return n; }\n\
        double fl(double a, double b) { use(0); return a * b; }\n";
    for level in ["-O0", "-O2"] {
        let out = run("object", &["-c", level], source);
        assert!(out.status.success(), "{level}: {}", String::from_utf8_lossy(&out.stderr));
    }
}
