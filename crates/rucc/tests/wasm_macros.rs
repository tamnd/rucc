//! The predefined macros of each wasm row, compared with clang 23 from wasi-sdk 34. The files in
//! `tests/predef/wasm` are clang's output, and `approved.txt` there lists each difference and the
//! reason for it. A new difference fails, and so does a line of the list that no difference uses.
//!
//! Design: #2863, and section 11.2 of the WebAssembly notes.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const ROWS: [&str; 4] =
    ["wasm32-wasip1", "wasm32-wasip2", "wasm32-wasip3", "wasm32-unknown-unknown"];

fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("the crate is two levels under the repository root")
        .join("tests/predef/wasm")
}

/// The name and the value of each `#define` line in `text`.
fn defines(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|line| line.strip_prefix("#define "))
        .map(|rest| match rest.split_once(' ') {
            Some((name, value)) => (name.to_owned(), value.trim().to_owned()),
            None => (rest.to_owned(), String::new()),
        })
        .collect()
}

/// What rucc defines for `row` with no option but the target.
fn rucc(row: &str) -> BTreeMap<String, String> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .env("LC_ALL", "C")
        .arg(format!("--target={row}"))
        .args(["-E", "-dM", "-x", "c", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the compiler is built before its own tests run");
    child.stdin.take().expect("a pipe for the input").write_all(b"").expect("an empty input");
    let out = child.wait_with_output().expect("the compiler finished");
    assert!(out.status.success(), "{row}: {}", String::from_utf8_lossy(&out.stderr));
    defines(&String::from_utf8_lossy(&out.stdout))
}

/// An integer literal as its value and its suffix, so that `0x7fffffffL` is `2147483647L`.
fn integer(value: &str) -> Option<String> {
    let digits = value.trim_end_matches(['u', 'U', 'l', 'L']);
    let suffix = value[digits.len()..].to_ascii_uppercase();
    let n = match digits.strip_prefix("0x") {
        Some(hex) => u128::from_str_radix(hex, 16).ok()?,
        None => digits.parse::<u128>().ok()?,
    };
    Some(format!("{n}{suffix}"))
}

/// A floating literal as the number it rounds to in its type. rucc writes a double as
/// `((double)1.5L)` and clang writes it as `1.5`. A `long double` keeps its digits.
fn float(value: &str) -> Option<String> {
    let (cast, rest) = match value.strip_prefix("((double)") {
        Some(rest) => (Some(""), rest.strip_suffix(')')?),
        None => match value.strip_prefix("((float)") {
            Some(rest) => (Some("F"), rest.strip_suffix(')')?),
            None => (None, value),
        },
    };
    let number = rest.trim_end_matches(['F', 'L']);
    if !number.contains(['e', 'E']) || number.parse::<f64>().is_err() {
        return None;
    }
    match cast.unwrap_or(&rest[number.len()..]) {
        "F" => Some(format!("{:e}F", number.parse::<f32>().ok()?)),
        "" => Some(format!("{:e}", number.parse::<f64>().ok()?)),
        suffix => Some(format!("{number}{suffix}")),
    }
}

/// A type as its words in a fixed order, so that `short unsigned int` is `unsigned short`.
fn type_name(value: &str) -> Option<String> {
    let words: Vec<&str> = value.split_whitespace().collect();
    let known = ["char", "short", "int", "long", "signed", "unsigned"];
    if words.is_empty() || !words.iter().all(|w| known.contains(w)) {
        return None;
    }
    let sized = words.contains(&"short") || words.contains(&"long");
    let mut words: Vec<&str> = words.into_iter().filter(|w| !(sized && *w == "int")).collect();
    words.sort_unstable();
    Some(words.join(" "))
}

/// The value of `name` in `set`, in the form that the comparison uses.
fn normal(set: &BTreeMap<String, String>, name: &str) -> String {
    let mut value = set[name].as_str();
    if let Some(other) = set.get(value) {
        value = other;
    }
    integer(value)
        .or_else(|| float(value))
        .or_else(|| type_name(value))
        .unwrap_or_else(|| value.split("##").map(str::trim).collect::<Vec<_>>().join("##"))
}

struct Approval {
    kind: String,
    pattern: String,
}

impl Approval {
    fn covers(&self, kind: &str, name: &str) -> bool {
        if self.kind != kind {
            return false;
        }
        match (self.pattern.strip_prefix('*'), self.pattern.strip_suffix('*')) {
            (Some(end), _) => name.ends_with(end),
            (_, Some(start)) => name.starts_with(start),
            _ => name == self.pattern,
        }
    }
}

fn approvals() -> Vec<Approval> {
    let text = std::fs::read_to_string(dir().join("approved.txt")).expect("the approved list");
    text.lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| {
            let mut words = line.splitn(3, ' ');
            let kind = words.next().unwrap_or_default().to_owned();
            let pattern = words.next().unwrap_or_default().to_owned();
            assert!(
                ["clang", "rucc", "value"].contains(&kind.as_str()) && words.next().is_some(),
                "a line of approved.txt is a kind, a name and a reason: {line}"
            );
            Approval { kind, pattern }
        })
        .collect()
}

#[test]
fn every_difference_from_clang_23_on_a_wasm_row_is_approved() {
    let approvals = approvals();
    let mut used = BTreeSet::new();
    let mut unapproved = Vec::new();
    for row in ROWS {
        let path = dir().join(format!("{row}.txt"));
        let clang = defines(&std::fs::read_to_string(&path).expect("the clang dump of the row"));
        let rucc = rucc(row);
        let names: BTreeSet<&String> = clang.keys().chain(rucc.keys()).collect();
        for name in names {
            let kind = match (clang.contains_key(name), rucc.contains_key(name)) {
                (true, false) => "clang",
                (false, true) => "rucc",
                _ if normal(&clang, name) == normal(&rucc, name) => continue,
                _ => "value",
            };
            match approvals.iter().position(|a| a.covers(kind, name)) {
                Some(n) => {
                    used.insert(n);
                }
                None => unapproved.push(format!(
                    "{row}: {kind} {name}  clang: {:?}  rucc: {:?}",
                    clang.get(name),
                    rucc.get(name)
                )),
            }
        }
    }
    assert!(
        unapproved.is_empty(),
        "differences with no line in approved.txt:\n{}",
        unapproved.join("\n")
    );
    let stale: Vec<String> = approvals
        .iter()
        .enumerate()
        .filter(|(n, _)| !used.contains(n))
        .map(|(_, a)| format!("{} {}", a.kind, a.pattern))
        .collect();
    assert!(
        stale.is_empty(),
        "lines of approved.txt that no difference uses:\n{}",
        stale.join("\n")
    );
}

#[test]
fn the_comparison_reads_two_spellings_of_one_value_as_one_value() {
    assert_eq!(integer("0x7fffffffL"), integer("2147483647L"));
    assert_ne!(integer("0x7fffffffL"), integer("2147483647"));
    assert_eq!(
        float("((double)2.22044604925031308084726333618164062e-16L)"),
        float("2.2204460492503131e-16")
    );
    assert_eq!(float("1.19209289550781250000000000000000000e-7F"), float("1.19209290e-7F"));
    assert_ne!(float("1.0e-7F"), float("1.0e-7"));
    assert_eq!(type_name("short unsigned int"), type_name("unsigned short"));
    assert_ne!(type_name("long unsigned int"), type_name("unsigned int"));
}

/// `sizeof` and a pointer difference have the types that the macros name. On wasm32 that is
/// `long` and not `int`, which is the one place where wasm32 is not i686.
#[test]
fn the_type_of_sizeof_is_the_type_that_size_type_names() {
    let program = "\
static_assert(_Generic(sizeof(int), __SIZE_TYPE__: 1, default: 0));
static_assert(_Generic(sizeof(int), unsigned long: 1, default: 0));
static_assert(_Generic((char *)0 - (char *)0, __PTRDIFF_TYPE__: 1, default: 0));
static_assert(_Generic((char *)0 - (char *)0, long: 1, default: 0));
static_assert(sizeof(long) == 4 && sizeof(void *) == 4);
";
    for row in ROWS {
        let mut child = Command::new(env!("CARGO_BIN_EXE_rucc"))
            .env("LC_ALL", "C")
            .arg(format!("--target={row}"))
            .args(["-std=c23", "-fsyntax-only", "-x", "c", "-"])
            .stdin(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("the compiler is built before its own tests run");
        child
            .stdin
            .take()
            .expect("a pipe for the input")
            .write_all(program.as_bytes())
            .expect("the input");
        let out = child.wait_with_output().expect("the compiler finished");
        assert!(out.status.success(), "{row}: {}", String::from_utf8_lossy(&out.stderr));
        assert!(out.stderr.is_empty(), "{row}: {}", String::from_utf8_lossy(&out.stderr));
    }
}
