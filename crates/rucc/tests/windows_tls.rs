//! What a thread-local variable on Windows reaches the assembler as.
//!
//! A Windows thread keeps an array of pointers to its copies of each image's `.tls` section, and
//! `_tls_index` says which slot is this image's. So the variable goes in `.tls$`, and reading it is
//! four instructions: the index, the array out of the thread block at `%gs:88`, this image's copy
//! out of the array, and the variable's offset in the section added to that. On i386 the array is
//! at `%fs:44` and a pointer in it is four bytes.

use std::path::PathBuf;
use std::process::Command;

fn fixture(what: &str, source: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-wintls-{}-{what}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, source).expect("the fixture can be written");
    path
}

/// Whether the compiler agreed, what it wrote to standard output, and what it said.
fn run(what: &str, args: &[&str], source: &str) -> (bool, Vec<u8>, String) {
    let path = fixture(what, source);
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(args)
        .args(["-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(path.parent().expect("the fixture is in a directory"));
    (out.status.success(), out.stdout, String::from_utf8_lossy(&out.stderr).into_owned())
}

const SOURCE: &str = "\
_Thread_local int counter;
_Thread_local int start = 7;

int bump(void) { return ++counter + start; }
";

#[test]
fn a_thread_local_is_reached_through_the_tls_index() {
    let (ok, out, said) = run("asm", &["--target=x86_64-windows-gnu", "-S"], SOURCE);
    assert!(ok, "{said}");
    let text = String::from_utf8(out).expect("a listing is text");
    assert!(text.contains("\t.section\t.tls$,\"dw\"\n"), "{text}");
    assert!(text.contains("_tls_index(%rip)"), "{text}");
    assert!(text.contains("%gs:88"), "{text}");
    assert!(text.contains("counter@SECREL32("), "{text}");
    assert!(text.contains("start@SECREL32("), "{text}");
    // Zeroed or not, it is in the one section every thread copies, since there is no zeroed half.
    assert!(!text.contains(".tbss"), "{text}");
}

/// How many `SECREL` relocations an object has. Read straight off the section headers, which start
/// after the twenty byte file header and whatever optional header it says it has. The relocation
/// is eleven on both machines, `IMAGE_REL_AMD64_SECREL` and `IMAGE_REL_I386_SECREL`.
fn secrels(out: &[u8]) -> usize {
    let u16_at = |at: usize| usize::from(u16::from_le_bytes([out[at], out[at + 1]]));
    let u32_at = |at: usize| u32::from_le_bytes(out[at..at + 4].try_into().unwrap()) as usize;
    let sections = u16_at(2);
    let headers = 20 + u16_at(16);
    let mut secrel = 0;
    for section in 0..sections {
        let header = headers + section * 40;
        let (relocs, count) = (u32_at(header + 24), u16_at(header + 32));
        secrel += (0..count).filter(|at| u16_at(relocs + at * 10 + 8) == 0x000b).count();
    }
    secrel
}

#[test]
fn a_thread_local_is_written_into_an_object() {
    let (ok, out, said) = run("obj", &["--target=x86_64-windows-gnu", "-c"], SOURCE);
    assert!(ok, "{said}");
    let secrel = secrels(&out);
    assert!(secrel >= 2, "at least one for each of the two variables, and {secrel} in all");
}

/// i386 is the same walk at four bytes a pointer, from `%fs:44`, and the names have cdecl's
/// underscore in front. These are the instructions gcc writes for `i686-w64-mingw32`.
#[test]
fn an_i386_thread_local_is_reached_through_fs_44() {
    for target in ["i686-windows-gnu", "i686-windows-msvc"] {
        let (ok, out, said) = run("asm32", &[&format!("--target={target}"), "-S"], SOURCE);
        assert!(ok, "{target}: {said}");
        let text = String::from_utf8(out).expect("a listing is text");
        assert!(text.contains("\t.section\t.tls$,\"dw\"\n"), "{target}: {text}");
        assert!(text.contains("__tls_index,"), "{target}: {text}");
        assert!(text.contains("%fs:44,"), "{target}: {text}");
        assert!(text.contains(",4), %"), "{target}: four bytes a pointer: {text}");
        assert!(text.contains("_counter@SECREL32("), "{target}: {text}");
        assert!(text.contains("_start@SECREL32("), "{target}: {text}");
        assert!(!text.contains("%gs"), "{target}: {text}");
    }
}

#[test]
fn an_i386_thread_local_is_written_into_an_object() {
    for target in ["i686-windows-gnu", "i686-windows-msvc"] {
        let (ok, out, said) = run("obj32", &[&format!("--target={target}"), "-c"], SOURCE);
        assert!(ok, "{target}: {said}");
        assert_eq!(u16::from_le_bytes([out[0], out[1]]), 0x014c, "{target}: an i386 object");
        let secrel = secrels(&out);
        assert!(secrel >= 2, "{target}: one for each of the two variables, and {secrel} in all");
    }
}
