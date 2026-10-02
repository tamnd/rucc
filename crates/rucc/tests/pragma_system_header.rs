//! `#pragma GCC system_header`, end to end, measured against gcc 13: the rest of the header it
//! is in is a system header, so its warnings go unsaid unless `-Wsystem-headers` asks, while
//! `#warning` is said all the same, and in the main file the pragma is ignored with a warning.
//!
//! Design: `system_header_pragma` in `crates/rucc-pp/src/directive.rs`, which marks the header
//! from the pragma on with `SourceMap::mark_system_from`.

use std::path::PathBuf;
use std::process::Command;

/// The fixture, under a directory of its own so that two runs at once do not share files.
fn fixture() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("rucc-system-header-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let files = [
        (
            "h.h",
            "int before = 1 << 40;\n#pragma GCC system_header\nint after = 1 << 40;\n\
             #warning said\n#include \"inner.h\"\n",
        ),
        ("inner.h", "int inner = 1 << 40;\n"),
        ("m.c", "#include \"h.h\"\n#pragma GCC system_header\nint big = 1 << 40;\n"),
    ];
    for (name, text) in files {
        std::fs::write(dir.join(name), text).expect("the fixture can be written");
    }
    dir
}

/// Each warning as `file:line: message`, without the column, which is not what is being
/// tested, or the code, in the order of file and line.
fn warnings(dir: &PathBuf, flags: &[&str]) -> Vec<String> {
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .env("LC_ALL", "C")
        .current_dir(dir)
        .args(["--target=x86_64-unknown-linux-gnu", "-std=gnu17", "-fsyntax-only"])
        .args(flags)
        .arg("m.c")
        .output()
        .expect("the compiler runs");
    let err = String::from_utf8_lossy(&out.stderr);
    let mut said: Vec<(String, u32, String)> = err
        .lines()
        .filter_map(|line| {
            let (at, message) = line.split_once(": warning: ")?;
            let mut parts = at.split(':');
            let file = parts.next()?.to_owned();
            let row = parts.next()?.parse().ok()?;
            Some((file, row, message.split(" [").next()?.to_owned()))
        })
        .collect();
    said.sort();
    said.dedup();
    said.into_iter().map(|(file, row, message)| format!("{file}:{row}: {message}")).collect()
}

#[test]
fn the_rest_of_the_header_is_quiet_and_the_main_file_is_told_the_pragma_does_nothing() {
    let dir = fixture();
    let shift = "left shift count >= width of type";
    assert_eq!(
        warnings(&dir, &[]),
        [
            format!("h.h:1: {shift}"),
            "h.h:4: said".to_owned(),
            "m.c:2: `#pragma system_header` ignored outside include file".to_owned(),
            format!("m.c:3: {shift}"),
        ]
    );
    assert_eq!(
        warnings(&dir, &["-Wsystem-headers"]),
        [
            format!("h.h:1: {shift}"),
            format!("h.h:3: {shift}"),
            "h.h:4: said".to_owned(),
            format!("inner.h:1: {shift}"),
            "m.c:2: `#pragma system_header` ignored outside include file".to_owned(),
            format!("m.c:3: {shift}"),
        ]
    );
    assert_eq!(warnings(&dir, &["-w"]), Vec::<String>::new());
    std::fs::remove_dir_all(&dir).ok();
}
