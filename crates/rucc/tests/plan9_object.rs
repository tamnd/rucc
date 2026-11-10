//! The driver writes a Plan 9 object for `-c` on the x86_64-plan9 and the aarch64-plan9 targets.
//!
//! Design: tamnd/rucc#4156, and 10 of the rucc-plan9 plan.
//!
//! The object is the `.6` file that `6l` reads, or the `.7` file that `7l` reads. The records
//! come from the listing of the assembly, which `rucc_asm::plan9` puts in the instructions of the
//! linker. These tests check the
//! file from the outside. The unit tests of `rucc_asm::plan9` check the records.

use std::path::PathBuf;
use std::process::Command;

const SOURCE: &str = "static int count;\nint table[4] = {1, 2, 3, 4};\nextern int g(int);\nint add(int a, int b) { return a + b; }\nint tail(int x) { return g(x + 1); }\nvoid bump(void) { count++; }\n";

fn fixture(what: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-plan9-object-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    path
}

/// The bytes of the object, or the diagnostics when the compiler refused the source.
fn compile(target: &str, level: &str) -> Result<Vec<u8>, String> {
    let path = fixture(&format!("{target}{level}"));
    let object = path.with_extension("6");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args([&format!("--target={target}"), level, "-c", "-o"])
        .arg(&object)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let bytes = std::fs::read(&object);
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned());
    }
    Ok(bytes.expect("a compiler that succeeds writes the object"))
}

fn contains(bytes: &[u8], what: &[u8]) -> bool {
    bytes.windows(what.len()).any(|window| window == what)
}

#[test]
fn the_object_has_a_name_record_first_and_the_names_of_the_unit() {
    for level in ["-O0", "-O2", "-Os"] {
        let bytes = compile("x86_64-plan9", level).unwrap_or_else(|why| panic!("{level}: {why}"));
        // ANAME is 132 in 6.out.h, and the opcode is two bytes, low byte first.
        assert_eq!(bytes[..2], [132, 0], "{level}");
        for name in ["add", "tail", "bump", "table", "count", "g"] {
            let mut z = name.as_bytes().to_vec();
            z.push(0);
            assert!(contains(&bytes, &z), "{level}: no name {name}");
        }
        assert!(!contains(&bytes, b".text"), "{level}: a section name is in the object");
    }
}

#[test]
fn the_object_for_arm64_has_the_records_of_7l() {
    for level in ["-O0", "-O2", "-Os"] {
        let bytes = compile("aarch64-plan9", level).unwrap_or_else(|why| panic!("{level}: {why}"));
        // ANAME is 313 in 7.out.h.
        assert_eq!(bytes[..2], [57, 1], "{level}");
        for name in ["add", "tail", "bump", "table", "count", "g"] {
            let mut z = name.as_bytes().to_vec();
            z.push(0);
            assert!(contains(&bytes, &z), "{level}: no name {name}");
        }
        assert!(
            !contains(&bytes, b"lo12"),
            "{level}: a relocation of the listing is in the object"
        );
    }
}

/// The assembly that the object is made from, for a source given here.
fn assembly(source: &str, args: &[&str]) -> String {
    // Each call has its own directory, because the tests run at the same time.
    static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let call = CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "rucc-plan9-asm-{}-{call}-{}",
        std::process::id(),
        args.join("")
    ));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("two.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-plan9", "-S", "-o", "-"])
        .args(args)
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn a_name_is_reached_directly_with_no_slot_and_no_table() {
    let source = "extern int f(void);\nextern int v;\nint (*a(void))(void) { return f; }\nint *b(void) { return &v; }\nint c(int x) { switch (x) { case 0: return 4; case 1: return 9; case 2: return 1; case 3: return 7; case 4: return 2; case 5: return 8; } return 0; }\n";
    for level in ["-O0", "-O2"] {
        let text = assembly(source, &[level]);
        assert!(!text.contains("GOTPCREL"), "{level}: {text}");
        assert!(!text.contains(".Ljt"), "{level}: {text}");
        assert!(!text.contains("-.L"), "{level}: {text}");
    }
}

#[test]
fn the_frame_is_made_at_the_entry() {
    // gcc moves the frame down to the one path that needs it. A Plan 9 loader puts it at the
    // entry from the size on the `TEXT` record, so it stays there.
    let source = "extern int g(int);\nint f(int *p) { if (!p) return 0; return g(*p) + 1; }\n";
    let text = assembly(source, &["-O2"]);
    let body: Vec<&str> = text
        .lines()
        .skip_while(|l| *l != "f:")
        .filter(|l| l.starts_with('\t') && !l.starts_with("\t."))
        .collect();
    assert!(body.first().is_some_and(|l| l.starts_with("\tsubq\t")), "{text}");
}

#[test]
fn position_independent_code_is_not_the_default() {
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=x86_64-plan9", "-dM", "-E", "-"])
        .stdin(std::process::Stdio::null())
        .output()
        .expect("the compiler is built before its own tests run");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(!text.contains("__PIE__") && !text.contains("__PIC__"), "{text}");
}
