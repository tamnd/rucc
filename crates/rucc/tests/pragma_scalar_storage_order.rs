//! `#pragma scalar_storage_order`, end to end, measured against gcc 13: which records the line
//! reverses the byte order of, and what is said about a line that is not one.
//!
//! Design: `order_line` in `crates/rucc-parse/src/order.rs`, applied where the record is laid out
//! in `crates/rucc-sema/src/check/ty/tag.rs`.

use std::io::Write as _;
use std::process::{Command, Output, Stdio};

fn run(flags: &[&str], input: &str) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .env("LC_ALL", "C")
        .args(["--target=x86_64-unknown-linux-gnu", "-std=gnu17", "-O1"])
        .args(flags)
        .args(["-x", "c", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the compiler is built before its own tests run");
    let mut stdin = child.stdin.take().expect("a pipe to write the input into");
    stdin.write_all(input.as_bytes()).expect("the input can be written");
    drop(stdin);
    child.wait_with_output().expect("the compiler finished")
}

/// Each diagnostic, as `line:column: severity: message`, in line order.
fn said(out: &Output) -> Vec<String> {
    let err = String::from_utf8_lossy(&out.stderr);
    let mut said: Vec<(u32, u32, String)> = err
        .lines()
        .filter_map(|line| line.strip_prefix("<stdin>:"))
        .filter_map(|line| {
            let mut parts = line.splitn(3, ':');
            let row = parts.next()?.parse().ok()?;
            let col = parts.next()?.parse().ok()?;
            let rest = parts.next()?.trim();
            let rest = rest.rsplit_once(" [").map_or(rest, |(said, _)| said).to_owned();
            Some((row, col, rest))
        })
        .filter(|(_, _, rest)| !rest.starts_with("note:"))
        .collect();
    said.sort_by_key(|&(row, col, _)| (row, col));
    said.into_iter().map(|(row, col, rest)| format!("{row}:{col}: {rest}")).collect()
}

/// The functions of the assembly whose body swaps bytes, by name.
fn swapping(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut function = None;
    for line in text.lines() {
        if let Some(name) = line.strip_suffix(':').filter(|name| name.starts_with("f_")) {
            function = Some(name);
        } else if line.contains("bswap") {
            if let Some(name) = function.take() {
                found.push(name.to_owned());
            }
        }
    }
    found
}

const LINES: &str = "\
struct plain { int i; };
#pragma scalar_storage_order big-endian
struct big { int i; };
struct own { int i; } __attribute__((scalar_storage_order(\"little-endian\")));
union ubig { int i; };
struct fwd;
#pragma scalar_storage_order default
struct back { int i; };
struct fwd { int i; };
#pragma scalar_storage_order little-endian
struct little { int i; };
#pragma scalar_storage_order big
struct bare { int i; };
#pragma scalar_storage_order
#pragma scalar_storage_order middle
#pragma scalar_storage_order \"big-endian\"
#pragma scalar_storage_order int
struct still { int i; };
#pragma scalar_storage_order default
struct mid { int i;
#pragma scalar_storage_order big-endian
};
#pragma scalar_storage_order default
int f_plain(struct plain *p) { return p->i; }
int f_big(struct big *p) { return p->i; }
int f_own(struct own *p) { return p->i; }
int f_ubig(union ubig *p) { return p->i; }
int f_back(struct back *p) { return p->i; }
int f_fwd(struct fwd *p) { return p->i; }
int f_little(struct little *p) { return p->i; }
int f_bare(struct bare *p) { return p->i; }
int f_still(struct still *p) { return p->i; }
int f_mid(struct mid *p) { return p->i; }
";

/// A record whose body closes under a `big-endian` line is stored big-endian, a structure and a
/// union alike, unless it writes the attribute itself. `default` and `little-endian` put the
/// target's order back, a tag declared under the line and defined after it is not reversed, and
/// a line in the middle of a body settles the whole record. The four lines gcc does not read are
/// warned about at the word and leave the order before them in effect.
#[test]
fn the_records_after_the_line_are_stored_in_its_order() {
    let out = run(&["-S", "-o", "-"], LINES);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout);
    assert_eq!(swapping(&text), ["f_big", "f_ubig", "f_bare", "f_still", "f_mid"], "{text}");
    assert_eq!(
        said(&out),
        [
            "14:9: warning: missing `big-endian`, `little-endian`, or `default` after \
             `#pragma scalar_storage_order`",
            "15:9: warning: expected `big-endian`, `little-endian`, or `default` after \
             `#pragma scalar_storage_order`",
            "16:9: warning: missing `big-endian`, `little-endian`, or `default` after \
             `#pragma scalar_storage_order`",
            "17:9: warning: expected `big-endian`, `little-endian`, or `default` after \
             `#pragma scalar_storage_order`",
        ]
    );
}

/// An attribute naming neither end is refused under the line as it is without it.
#[test]
fn an_attribute_the_line_cannot_stand_in_for_is_still_refused() {
    let out = run(
        &["-fsyntax-only"],
        "#pragma scalar_storage_order big-endian\n\
         struct s { int i; } __attribute__((scalar_storage_order(\"middle\")));\n",
    );
    assert!(!out.status.success());
    let said = said(&out);
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].contains("'scalar_storage_order' argument must be one of"), "{said:?}");
}
