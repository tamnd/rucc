//! How AArch64 Windows code reaches a name that is in a DLL or may be in another object.
//!
//! On COFF such a name is reached through a pointer this image holds: `__imp_` in front of it for
//! a declaration marked `dllimport`, and `.refptr.` in front of it for a variable that is only
//! declared. AArch64 reads the pointer as the page it is on and a load from the low twelve bits,
//! which is what clang for aarch64-w64-mingw32 writes. The call through `__imp_` is the thing
//! every function in a Windows header needs, so before this every program that called one
//! stopped at code generation.

use std::path::PathBuf;
use std::process::Command;

const WINDOWS: &str = "aarch64-windows-gnu";

/// The fixture, under a directory of its own so that two of these running at once do not write
/// the same file.
fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-winarm-imp-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// The instructions of the assembly for one fixture, with every directive and label left out.
fn insts(what: &str, source: &str) -> Vec<String> {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg(format!("--target={WINDOWS}"))
        .args(["-O1", "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    assert!(
        out.status.success(),
        "the compiler refused the fixture:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('.') && !line.ends_with(':'))
        .map(str::to_owned)
        .collect()
}

/// Whether the listing reads the pointer `slot` into some register and then does `then` with it.
fn reads_slot(got: &[String], slot: &str, then: &str) -> bool {
    got.windows(3).any(|three| {
        let Some(reg) = three[0].strip_prefix("adrp ").and_then(|rest| rest.split(',').next())
        else {
            return false;
        };
        three[0] == format!("adrp {reg}, {slot}")
            && three[1] == format!("ldr {reg}, [{reg}, :lo12:{slot}]")
            && three[2].starts_with(then)
            && three[2].contains(reg)
    })
}

#[test]
fn a_call_to_a_dllimport_function_goes_through_its_import_pointer() {
    let got = insts(
        "call",
        "__declspec(dllimport) unsigned GetCurrentProcessId(void);\n\
         unsigned f(void) { return GetCurrentProcessId() + 1; }\n",
    );
    assert!(reads_slot(&got, "__imp_GetCurrentProcessId", "blr "), "{got:#?}");
    assert!(!got.iter().any(|line| line.starts_with("bl ")), "{got:#?}");
}

#[test]
fn the_address_of_a_dllimport_function_is_its_import_pointer() {
    let got = insts(
        "address",
        "__declspec(dllimport) unsigned GetCurrentProcessId(void);\n\
         unsigned (*f(void))(void) { return GetCurrentProcessId; }\n",
    );
    assert_eq!(
        got,
        [
            "adrp x0, __imp_GetCurrentProcessId",
            "ldr x0, [x0, :lo12:__imp_GetCurrentProcessId]",
            "ret"
        ]
    );
}

#[test]
fn dllimport_data_and_a_declared_variable_are_read_through_their_pointers() {
    let got = insts(
        "data",
        "__declspec(dllimport) extern int imported;\n\
         extern int declared;\n\
         int f(void) { return imported + declared; }\n",
    );
    assert!(reads_slot(&got, "__imp_imported", "ldr w"), "{got:#?}");
    assert!(reads_slot(&got, ".refptr.declared", "ldr w"), "{got:#?}");
}

#[test]
fn a_variable_this_file_defines_is_reached_by_its_page_and_offset() {
    let got = insts("local", "static int here = 3;\nint f(void) { return here; }\n");
    assert!(
        got.iter().any(|line| line.starts_with("adrp ") && line.ends_with(", here")),
        "{got:#?}"
    );
    assert!(got.iter().any(|line| line.contains(":lo12:here")), "{got:#?}");
    assert!(
        !got.iter().any(|line| line.contains("__imp_") || line.contains(".refptr")),
        "{got:#?}"
    );
}
