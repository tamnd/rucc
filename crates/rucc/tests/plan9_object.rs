//! The driver writes a Plan 9 object for `-c` on the x86_64-plan9 target.
//!
//! Design: tamnd/rucc#4156, and 10 of the rucc-plan9 plan.
//!
//! The object is the `.6` file that `6l` reads. The records come from the listing of the
//! assembly, which `rucc_asm::plan9` puts in the instructions of `6l`. These tests check the
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
fn an_object_for_arm64_is_refused_until_it_is_written() {
    let why = compile("aarch64-plan9", "-O2").expect_err("the arm64 object is not written yet");
    assert!(why.contains("Plan 9 object"), "{why}");
}
