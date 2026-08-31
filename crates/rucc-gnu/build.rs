//! Turns `features.toml` into the table the compiler reads.
//!
//! Design: `spec/13-gnu-compat.md` section 13.2. The table is generated at build time rather
//! than written out by hand, because a hand written copy of a list is a copy that disagrees
//! with the list, and the whole point of the matrix is that `__has_attribute` and the
//! documentation cannot say different things.
//!
//! The parser understands the part of TOML that `features.toml` uses and nothing else: an
//! array of tables, string values, and single line arrays of strings. Pulling in a TOML crate
//! for this would put a dependency in the build of a compiler that has none, which
//! `spec/18-package-layout.md` section 18.7 rules out. A malformed file fails the build with
//! the line number on it, which is the whole contract a parser this small has to meet.

use std::collections::BTreeSet;
use std::path::Path;
use std::{env, fs};

/// One row of the matrix, as read.
struct Row {
    line: usize,
    name: String,
    kind: String,
    gcc_version: String,
    status: String,
    answer: String,
    value: String,
    used_by: Vec<String>,
    tests: Vec<String>,
    notes: String,
}

const KINDS: &[(&str, &str)] = &[
    ("attribute", "Kind::Attribute"),
    ("c-attribute", "Kind::CAttribute"),
    ("builtin", "Kind::Builtin"),
    ("feature", "Kind::Feature"),
    ("extension", "Kind::Extension"),
];

const STATUSES: &[(&str, &str)] = &[
    ("unimplemented", "Status::Unimplemented"),
    ("partial", "Status::Partial"),
    ("implemented", "Status::Implemented"),
    ("rejected", "Status::Rejected"),
];

const ANSWERS: &[(&str, &str)] = &[("warn", "Answer::Warn"), ("error", "Answer::Error")];

fn main() {
    println!("cargo::rerun-if-changed=features.toml");
    let manifest = env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
    let source = Path::new(&manifest).join("features.toml");
    let text = match fs::read_to_string(&source) {
        Ok(text) => text,
        Err(e) => panic!("could not read {}: {e}", source.display()),
    };
    let rows = parse(&text);
    check(&rows);
    let out = env::var("OUT_DIR").expect("cargo sets OUT_DIR");
    let generated = Path::new(&out).join("features.rs");
    if let Err(e) = fs::write(&generated, render(rows)) {
        panic!("could not write {}: {e}", generated.display());
    }
}

/// Reads the array of tables. Anything the file should not contain is a build failure.
fn parse(text: &str) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();
    for (at, raw) in text.lines().enumerate() {
        let line = at + 1;
        let trimmed = raw.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if trimmed == "[[feature]]" {
            rows.push(Row {
                line,
                name: String::new(),
                kind: String::new(),
                gcc_version: String::new(),
                status: String::new(),
                answer: String::new(),
                value: String::new(),
                used_by: Vec::new(),
                tests: Vec::new(),
                notes: String::new(),
            });
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            panic!("features.toml:{line}: expected `key = value` or `[[feature]]`");
        };
        let Some(row) = rows.last_mut() else {
            panic!("features.toml:{line}: a value before the first `[[feature]]`");
        };
        let key = key.trim();
        let value = value.trim();
        match key {
            "name" => row.name = string(value, line),
            "kind" => row.kind = string(value, line),
            "gcc_version" => row.gcc_version = string(value, line),
            "status" => row.status = string(value, line),
            "answer" => row.answer = string(value, line),
            "value" => row.value = string(value, line),
            "notes" => row.notes = string(value, line),
            "used_by" => row.used_by = array(value, line),
            "tests" => row.tests = array(value, line),
            other => panic!("features.toml:{line}: unknown field `{other}`"),
        }
    }
    rows
}

/// A quoted string, with no escapes because the file needs none.
fn string(value: &str, line: usize) -> String {
    let inner = value.strip_prefix('"').and_then(|v| v.strip_suffix('"'));
    let Some(inner) = inner else {
        panic!("features.toml:{line}: expected a quoted string, found `{value}`");
    };
    if inner.contains('"') {
        panic!("features.toml:{line}: a quote inside a string, which this parser does not take");
    }
    inner.to_owned()
}

/// A single line array of quoted strings.
fn array(value: &str, line: usize) -> Vec<String> {
    let inner = value.strip_prefix('[').and_then(|v| v.strip_suffix(']'));
    let Some(inner) = inner else {
        panic!("features.toml:{line}: expected an array on one line, found `{value}`");
    };
    if inner.trim().is_empty() {
        return Vec::new();
    }
    inner.split(',').map(|item| string(item.trim(), line)).collect()
}

/// The rules that make the table worth trusting.
fn check(rows: &[Row]) {
    let mut seen = BTreeSet::new();
    for row in rows {
        let line = row.line;
        if row.name.is_empty() {
            panic!("features.toml:{line}: this feature has no name");
        }
        let name = &row.name;
        if pick(KINDS, &row.kind).is_none() {
            panic!(
                "features.toml:{line}: `{name}` has kind `{}`, which is not one of {}",
                row.kind,
                names(KINDS)
            );
        }
        if pick(STATUSES, &row.status).is_none() {
            panic!(
                "features.toml:{line}: `{name}` has status `{}`, which is not one of {}",
                row.status,
                names(STATUSES)
            );
        }
        if !row.answer.is_empty() && pick(ANSWERS, &row.answer).is_none() {
            panic!(
                "features.toml:{line}: `{name}` has answer `{}`, which is not one of {}",
                row.answer,
                names(ANSWERS)
            );
        }
        if !row.value.is_empty() {
            if row.kind != "c-attribute" {
                panic!("features.toml:{line}: `{name}` has a value, which only a c-attribute has");
            }
            if row.value.parse::<u32>().is_err() {
                panic!(
                    "features.toml:{line}: `{name}` has value `{}`, which is not a number",
                    row.value
                );
            }
        }
        // The rule from section 13.2: a feature claimed as implemented with nothing proving
        // it is a build failure, because the claim is what `__has_attribute` answers with.
        if row.status == "implemented" && row.tests.is_empty() {
            panic!("features.toml:{line}: `{name}` is implemented and has no tests");
        }
        if !seen.insert((row.kind.clone(), row.name.clone())) {
            panic!("features.toml:{line}: `{name}` appears twice as a {}", row.kind);
        }
    }
}

/// The Rust spelling of a value from one of the tables above.
fn pick(table: &[(&'static str, &'static str)], name: &str) -> Option<&'static str> {
    table.iter().find(|(spelling, _)| *spelling == name).map(|(_, rust)| *rust)
}

/// The accepted spellings, for a diagnostic.
fn names(table: &[(&'static str, &'static str)]) -> String {
    let list: Vec<&str> = table.iter().map(|(spelling, _)| *spelling).collect();
    list.join(", ")
}

/// Writes the table out, sorted, so that the lookup can be a binary search and the build is
/// reproducible whatever order the file is in.
fn render(mut rows: Vec<Row>) -> String {
    rows.sort_by(|a, b| {
        let rank = |row: &Row| KINDS.iter().position(|(k, _)| *k == row.kind).unwrap_or(0);
        rank(a).cmp(&rank(b)).then_with(|| a.name.cmp(&b.name))
    });
    let mut out = String::from(
        "// Generated from features.toml by build.rs. Do not edit: edit the table instead.\n\n\
         /// Every row of the matrix, sorted by kind and then by name.\n\
         pub static FEATURES: &[Feature] = &[\n",
    );
    for row in &rows {
        let kind = pick(KINDS, &row.kind).unwrap_or("Kind::Feature");
        let status = pick(STATUSES, &row.status).unwrap_or("Status::Unimplemented");
        let answer = pick(ANSWERS, &row.answer).unwrap_or("Answer::Warn");
        let value = if row.value.is_empty() { "1".to_owned() } else { row.value.clone() };
        out.push_str("    Feature {\n");
        out.push_str(&format!("        name: {},\n", quote(&row.name)));
        out.push_str(&format!("        kind: {kind},\n"));
        out.push_str(&format!("        gcc_version: {},\n", quote(&row.gcc_version)));
        out.push_str(&format!("        status: {status},\n"));
        out.push_str(&format!("        answer: {answer},\n"));
        out.push_str(&format!("        value: {value},\n"));
        out.push_str(&format!("        used_by: &{},\n", list(&row.used_by)));
        out.push_str(&format!("        tests: &{},\n", list(&row.tests)));
        out.push_str(&format!("        notes: {},\n", quote(&row.notes)));
        out.push_str("    },\n");
    }
    out.push_str("];\n");
    out
}

fn list(items: &[String]) -> String {
    let quoted: Vec<String> = items.iter().map(|i| quote(i)).collect();
    format!("[{}]", quoted.join(", "))
}

fn quote(text: &str) -> String {
    let escaped = text.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}
