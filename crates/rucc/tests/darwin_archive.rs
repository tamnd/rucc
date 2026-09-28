//! A static library built for a Mac, which is an archive in the flavour Apple's `ld` reads.
//!
//! The container is checked here and the linking on a Mac, since this runs anywhere and ld64 does
//! not. What is checked is what ld64 depends on: an index called `__.SYMDEF` that names each
//! function with Apple's underscore in front of it, and every object starting on an eight byte
//! boundary.

use std::process::Command;

#[test]
fn a_darwin_static_library_has_the_index_ld64_reads() {
    let dir = std::env::temp_dir().join(format!("rucc-darwin-archive-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let one = dir.join("one.c");
    let two = dir.join("two.c");
    let library = dir.join("libpair.a");
    std::fs::write(&one, "int work(int n) { return n * 3; }\n").expect("a fixture");
    std::fs::write(&two, "int counter = 7;\nint more(void) { return counter + 1; }\n")
        .expect("a fixture");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(["--target=aarch64-apple-darwin", "-O2", "--emit=archive", "-o"])
        .arg(&library)
        .arg(&one)
        .arg(&two)
        .output()
        .expect("the compiler is built before its own tests run");
    let bytes = std::fs::read(&library).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    assert!(bytes.starts_with(b"!<arch>\n"));
    let mut at = 8;
    let mut names = Vec::new();
    let mut index = Vec::new();
    while at + 60 <= bytes.len() {
        let header = std::str::from_utf8(&bytes[at..at + 60]).expect("a header is text");
        let length: usize = header[3..16].trim_end().parse().expect("a BSD name length");
        let size: usize = header[48..58].trim_end().parse().expect("a size");
        let name = &bytes[at + 60..at + 60 + length];
        let name =
            String::from_utf8(name.split(|byte| *byte == 0).next().expect("a name").to_vec())
                .expect("a name is text");
        let body = &bytes[at + 60 + length..at + 60 + size];
        assert_eq!((at + 60 + length) % 8, 0, "{name} is not aligned");
        if name == "__.SYMDEF" {
            index = body.to_vec();
        } else {
            // A Mach-O arm64 object, whose magic is little-endian.
            assert_eq!(&body[..4], &[0xcf, 0xfa, 0xed, 0xfe], "{name}");
        }
        names.push(name);
        at += 60 + size + size % 2;
    }
    assert_eq!(at, bytes.len());
    assert_eq!(names, ["__.SYMDEF", "one.o", "two.o"]);
    for symbol in ["_work", "_counter", "_more"] {
        let held = format!("{symbol}\0");
        assert!(index.windows(held.len()).any(|at| at == held.as_bytes()), "no {symbol}");
    }
}
