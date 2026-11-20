//! A division by a field read twice is one division at `-Os` and `-Oz`. The two reads are two
//! values until the loads are merged, so value numbering has to run again after that to see the two
//! divisions as one. FreeBSD's `boot2` works out the cylinder group of an inode that way in
//! `fsread`, and it has to fit in 7680 bytes.

use std::process::Command;

const SOURCE: &str = "\
struct fs { int magic; unsigned ipg; int fpg; int off; int mask; } fs;
unsigned twice(unsigned ino) { return ino / fs.ipg + (ino / fs.ipg) * 3; }
long long group(unsigned ino) {
  return (long long)fs.fpg * (ino / fs.ipg) + (long long)fs.off * ((ino / fs.ipg) & ~fs.mask);
}
";

fn assembly(name: &str, target: &str, level: &str) -> String {
    let dir =
        std::env::temp_dir().join(format!("rucc-field-division-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temporary directory can be created");
    let path = dir.join("one.c");
    std::fs::write(&path, SOURCE).expect("the fixture can be written");
    let out = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args([&format!("--target={target}"), "-fno-pic", level, "-S", "-o", "-"])
        .arg(&path)
        .output()
        .expect("the compiler is built before its own tests run");
    let _ = std::fs::remove_dir_all(&dir);
    assert!(out.status.success(), "{name}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("assembly is text")
}

/// The body of one function in the assembly.
fn body<'a>(text: &'a str, name: &str) -> &'a str {
    let start = text.find(&format!("\n{name}:")).expect("the function is in the assembly");
    let rest = &text[start + 1..];
    &rest[..rest.find("\n\t.size").unwrap_or(rest.len())]
}

fn divisions(body: &str) -> usize {
    let division = |line: &&str| line.starts_with("div") || line.starts_with("idiv");
    body.lines().map(str::trim_start).filter(division).count()
}

#[test]
fn a_division_by_a_field_read_twice_is_one_division_for_size() {
    for target in ["i686-unknown-linux-gnu", "x86_64-unknown-linux-gnu"] {
        for level in ["-Os", "-Oz"] {
            let text = assembly(&format!("{level}-{target}"), target, level);
            let body = body(&text, "twice");
            assert_eq!(divisions(body), 1, "{target} {level}\n{body}");
        }
    }
}

#[test]
fn the_ufs1_start_of_a_group_divides_once() {
    // The base of the group and its UFS1 offset, in one block as in `fsread`. A second division on
    // the other side of a branch is not merged at `-Os`: the loads there are merged across blocks
    // by `redundant-load`, which only `-O2` and up run.
    let text = assembly("group", "i686-unknown-linux-gnu", "-Os");
    let body = body(&text, "group");
    assert_eq!(divisions(body), 1, "{body}");
}
