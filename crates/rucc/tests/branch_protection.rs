//! `-mbranch-protection=` and `-msign-return-address=` on AArch64, end to end: a function that
//! saves its return address signs it on the way in and checks it on the way out, every address an
//! indirect branch may arrive at opens with a `bti`, and the note says which of the two the file
//! was built for. The shape is clang's and gcc's.

use std::path::{Path, PathBuf};
use std::process::Command;

const TARGET: &str = "--target=aarch64-unknown-linux-gnu";

/// A directory of this test's own, empty.
fn dir(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-branch-{}-{what}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    dir.canonicalize().expect("the directory is there")
}

/// What the compiler said, and whether it succeeded, for that source and those flags.
fn run(what: &str, flags: &[&str], source: &str) -> (bool, String, String) {
    let dir = dir(what);
    std::fs::write(dir.join("a.c"), source).expect("the fixture can be written");
    let mut args = vec!["-S", "-O2", "-o", "-"];
    args.extend_from_slice(flags);
    args.push("a.c");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(&args)
        .current_dir(Path::new(&dir))
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    (out.status.success(), text, String::from_utf8_lossy(&out.stderr).into_owned())
}

/// The listing of a source that has to compile without a word.
fn listing(what: &str, flags: &[&str], source: &str) -> String {
    let (ok, text, err) = run(what, flags, source);
    assert!(ok && err.is_empty(), "{flags:?}: {err}");
    text
}

/// The instructions of one function, without its labels and directives.
fn body(text: &str, name: &str) -> Vec<String> {
    text.lines()
        .skip_while(|line| *line != format!("{name}:"))
        .skip(1)
        .take_while(|line| !line.starts_with("\t.size"))
        .filter(|line| line.starts_with('\t') && !line.starts_with("\t."))
        .map(|line| line.trim().to_string())
        .collect()
}

/// The words of the AArch64 feature note, if the listing has one.
fn feature(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let at = lines.iter().position(|line| *line == "\t.long\t0xc0000000")?;
    Some(lines[at + 2].trim_start_matches("\t.long\t").to_string())
}

const SOURCE: &str = "\
int g(int);
int leaf(int x) { return x + 1; }
int caller(int x) { return g(x) * 2; }
void *ra(void) { return __builtin_return_address(0); }
";

#[test]
fn every_function_signs_and_checks_under_leaf() {
    let text = listing("leaf", &[TARGET, "-mbranch-protection=pac-ret+leaf+bti"], SOURCE);
    let leaf = body(&text, "leaf");
    assert_eq!(leaf.first().map(String::as_str), Some("hint #25"), "{text}");
    assert_eq!(leaf[leaf.len() - 2..], ["hint #29", "ret"], "{text}");
    // Signed before the frame record is written, so the record holds the signed address.
    let caller = body(&text, "caller");
    assert_eq!(caller[..2], ["hint #25", "stp x29, x30, [sp, #-16]!"], "{text}");
    assert_eq!(caller[caller.len() - 3..], ["ldp x29, x30, [sp], #16", "hint #29", "ret"]);
    // An unwinder is told where the link register starts being signed and where it stops.
    let rows = text.lines().filter(|line| *line == "\t.cfi_negate_ra_state").count();
    assert_eq!(rows, 6, "{text}");
    // No `bti c` in front of the signing instruction, which lets the same calls in.
    assert!(!text.contains("hint #34"), "{text}");
    assert_eq!(feature(&text).as_deref(), Some("0x3"), "{text}");
}

#[test]
fn only_a_function_that_saves_the_return_address_signs_it_by_default() {
    for flag in ["-mbranch-protection=pac-ret", "-msign-return-address=non-leaf"] {
        let text = listing("nonleaf", &[TARGET, flag], SOURCE);
        assert_eq!(body(&text, "leaf"), ["add w0, w0, #1", "ret"], "{flag}:\n{text}");
        assert_eq!(body(&text, "caller")[0], "hint #25", "{flag}:\n{text}");
        assert_eq!(feature(&text).as_deref(), Some("0x2"), "{flag}:\n{text}");
    }
}

#[test]
fn bti_opens_every_function_and_every_label_an_indirect_jump_reaches() {
    let source = "\
int setjmp(long *) __attribute__((returns_twice));
int g(int);
int go(int x) { static void *t[] = { &&a, &&b }; goto *t[x & 1]; a: return 1; b: return 2; }
long jb[40];
int sj(void) { if (setjmp(jb)) return 1; return g(0); }
int (*fp)(int);
int indirect(int x) { return fp(x); }
";
    let text = listing("bti", &[TARGET, "-mbranch-protection=bti"], source);
    for name in ["go", "sj", "indirect"] {
        assert_eq!(body(&text, name)[0], "hint #34", "{name}:\n{text}");
    }
    assert_eq!(body(&text, "go").iter().filter(|line| *line == "hint #36").count(), 2, "{text}");
    let sj = body(&text, "sj");
    let call = sj.iter().position(|line| line == "bl setjmp").expect("the call");
    assert_eq!(sj[call + 1], "hint #36", "{text}");
    // A jump through a register would arrive at the callee's `bti c` from somewhere it does not
    // let in, so the call through a pointer in tail position stays a call.
    assert!(body(&text, "indirect").iter().any(|line| line.starts_with("blr ")), "{text}");
    assert!(!text.contains("hint #25"), "{text}");
    assert_eq!(feature(&text).as_deref(), Some("0x1"), "{text}");
}

#[test]
fn the_pad_stays_in_front_of_the_room_for_a_patcher() {
    let flags = [TARGET, "-mbranch-protection=standard", "-fpatchable-function-entry=2"];
    let text = listing("patch", &flags, SOURCE);
    assert_eq!(body(&text, "caller")[..4], ["hint #34", "nop", "nop", "hint #25"], "{text}");
}

#[test]
fn the_return_address_is_the_saved_link_register_with_its_signature_stripped() {
    for flags in [&[TARGET][..], &[TARGET, "-mbranch-protection=standard"]] {
        let text = listing("ra", flags, SOURCE);
        let ra = body(&text, "ra");
        let at = ra.iter().position(|line| line == "ldr x0, [x29, #8]").expect("the load");
        let strip = ["mov x30, x0", "hint #7", "mov x0, x30"];
        assert!(ra[at..].windows(3).any(|three| three == strip), "{flags:?}:\n{text}");
    }
}

#[test]
fn nothing_is_signed_and_no_note_is_written_without_the_flag() {
    for flags in [&[TARGET][..], &[TARGET, "-mbranch-protection=none"]] {
        let text = listing("none", flags, SOURCE);
        assert!(!text.contains("hint #25") && !text.contains("hint #34"), "{flags:?}:\n{text}");
        assert_eq!(feature(&text), None, "{flags:?}:\n{text}");
    }
}

#[test]
fn what_is_not_written_is_refused() {
    let (ok, _, err) = run("bkey", &[TARGET, "-mbranch-protection=pac-ret+b-key"], SOURCE);
    assert!(!ok && err.contains("b-key"), "{err}");
    let (ok, _, err) =
        run("mac", &["--target=aarch64-apple-darwin", "-mbranch-protection=bti"], SOURCE);
    assert!(!ok && err.contains("not supported"), "{err}");
    let (ok, _, err) =
        run("x86", &["--target=x86_64-unknown-linux-gnu", "-mbranch-protection=bti"], SOURCE);
    assert!(!ok && err.contains("unknown option"), "{err}");
}
