//! Build automation.
//!
//! The rule from `spec/18-package-layout.md` section 18.7 is that `cargo build` works with
//! no configuration and no external tools, and everything else is an `xtask`. There is no
//! `configure`, no CMake and no Python in the build, because build system complexity accretes
//! silently and the "clone and build" claim is worth protecting.
//!
//! This crate has no dependencies on purpose. It runs before anything else in CI, including
//! `cargo deny`, so it should not be able to fail because of somebody else's release.

mod aux;
mod bench;
mod bisect;
mod corpus;
mod cost;
mod disasm;
mod safety;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::{fmt, fs, io};

const USAGE: &str = "\
usage: cargo xtask <task>

tasks:
  layers      check the crate dependency graph against xtask/layers.toml
  style       check documentation and specification prose against the house rules
  thresholds  check that no pass compares against a number it made up
  malformed   check that the written list of malformed IR forms still names real tests
  version     check that every version number in the tree agrees with the workspace's
  builtins    build rucc-builtins as a static library for a target
  bench       time the throughput floor workload against the reference compiler
  disasm      check every instruction we encode against an independent decoder
  safety      compile, link and run tests/safety, and hold each program to its verdict
  cost        time bench/safety with the monitor off and on, and report the ratio
  aux         simulate the two aux plane layouts and compare their cache misses
  bisect      halve the optimizer's fuel until one rewrite is left holding the bug
  corpus      run the pinned C corpus against the compiler this tree builds
  bless       rewrite the expectations in tests/golden from what the compiler produces now
  interpose   check the interposition table and the compiler's copy of it agree
  ci          run everything the per-commit CI job runs, in the same order
  help        print this message
";

fn main() -> ExitCode {
    let task = std::env::args().nth(1);
    let result = match task.as_deref() {
        Some("layers") => layers(),
        Some("style") => style(),
        Some("thresholds") => thresholds(),
        Some("malformed") => malformed(),
        Some("interpose") => interpose(),
        Some("version") => version(),
        Some("builtins") => builtins(&std::env::args().skip(2).collect::<Vec<_>>()),
        Some("bench") => bench::bench(&std::env::args().skip(2).collect::<Vec<_>>()),
        Some("disasm") => disasm::disasm(),
        Some("safety") => safety::safety(),
        Some("cost") => cost::cost(),
        Some("aux") => aux::aux(),
        Some("bisect") => bisect::bisect(&std::env::args().skip(2).collect::<Vec<_>>()),
        Some("corpus") => corpus::corpus(&std::env::args().skip(2).collect::<Vec<_>>()),
        Some("bless") => bless(),
        Some("ci") => ci(),
        Some("help") | Some("--help") | Some("-h") | None => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Some(other) => {
            eprintln!("xtask: unknown task `{other}`");
            print!("{USAGE}");
            return ExitCode::FAILURE;
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("xtask: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Anything that stops a task finishing.
#[derive(Debug)]
enum Error {
    /// The task ran and found problems. Each string is one problem, already formatted.
    Failed { task: &'static str, problems: Vec<String> },
    /// The task could not run.
    Io(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Failed { task, problems } => {
                writeln!(f, "{task}: {} problem(s)", problems.len())?;
                for p in problems {
                    writeln!(f, "  {p}")?;
                }
                Ok(())
            }
            Error::Io(m) => f.write_str(m),
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e.to_string())
    }
}

type Result<T> = std::result::Result<T, Error>;

/// The workspace root, which is the parent of the directory holding this crate.
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("xtask is not at the root").to_path_buf()
}

// The layer check.

/// One crate in the workspace, as read off disk.
struct Crate {
    name: String,
    manifest: PathBuf,
    deps: Vec<Dep>,
}

/// One dependency, and whether it is one the compiler links against.
struct Dep {
    name: String,
    /// Whether it was read from `[build-dependencies]`. A build tool may be depended on that
    /// way and no other, per spec/18-package-layout.md section 18.2, because what runs during
    /// a build is not what ships in the binary.
    at_build: bool,
}

/// Reads every workspace member's manifest.
///
/// A real TOML parser would be more robust and would be a dependency. The manifests in this
/// workspace are written by us in one style, so a line reader is enough, and it keeps xtask
/// buildable with an empty lockfile.
fn read_crates(root: &Path) -> Result<Vec<Crate>> {
    let mut out = Vec::new();
    for dir in ["crates", "build-tools", "runtime"] {
        let d = root.join(dir);
        if !d.is_dir() {
            continue;
        }
        let mut entries: Vec<_> = fs::read_dir(&d)?.collect::<io::Result<Vec<_>>>()?;
        entries.sort_by_key(fs::DirEntry::file_name);
        for e in entries {
            let manifest = e.path().join("Cargo.toml");
            if manifest.is_file() {
                out.push(read_crate(&manifest)?);
            }
        }
    }
    out.push(read_crate(&root.join("xtask/Cargo.toml"))?);
    Ok(out)
}

fn read_crate(manifest: &Path) -> Result<Crate> {
    let text = fs::read_to_string(manifest)?;
    let mut name = None;
    let mut deps = Vec::new();
    let mut in_deps = false;
    let mut at_build = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            // Dev and build dependencies count too. A dev-dependency that inverts the stack
            // still makes the two crates impossible to build separately.
            in_deps = line.contains("dependencies]");
            at_build = line.contains("build-dependencies]");
            continue;
        }
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if name.is_none() {
            if let Some(v) = line.strip_prefix("name = ") {
                name = Some(v.trim_matches('"').to_owned());
            }
        }
        if in_deps {
            if let Some(dep) = line.split(['.', ' ', '=']).next() {
                let dep = dep.trim();
                if dep.starts_with("rucc") {
                    deps.push(Dep { name: dep.to_owned(), at_build });
                }
            }
        }
    }
    let name =
        name.ok_or_else(|| Error::Io(format!("{} has no package name", manifest.display())))?;
    Ok(Crate { name, manifest: manifest.to_path_buf(), deps })
}

/// Reads the rank table and the list of crates outside the stack.
fn read_layers(root: &Path) -> Result<(BTreeMap<String, u32>, Vec<String>)> {
    let path = root.join("xtask/layers.toml");
    let text = fs::read_to_string(&path)?;
    let mut ranks = BTreeMap::new();
    let mut outside = Vec::new();
    let mut section = "";
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            section = if line == "[ranks]" {
                "ranks"
            } else if line == "[outside]" {
                "outside"
            } else {
                ""
            };
            continue;
        }
        match section {
            "ranks" => {
                let Some((k, v)) = line.split_once('=') else { continue };
                let rank: u32 = v
                    .trim()
                    .parse()
                    .map_err(|_| Error::Io(format!("layers.toml: `{line}` is not a rank")))?;
                ranks.insert(k.trim().to_owned(), rank);
            }
            "outside" => {
                if let Some((_, v)) = line.split_once('=') {
                    for name in v.split(['[', ']', ',']) {
                        let name = name.trim().trim_matches('"');
                        if !name.is_empty() {
                            outside.push(name.to_owned());
                        }
                    }
                }
            }
            _ => {}
        }
    }
    if ranks.is_empty() {
        return Err(Error::Io(format!("{} has no ranks", path.display())));
    }
    Ok((ranks, outside))
}

/// Checks the dependency graph against the ranks.
fn layers() -> Result<()> {
    let root = root();
    let (ranks, outside) = read_layers(&root)?;
    let crates = read_crates(&root)?;
    let mut problems = Vec::new();

    for c in &crates {
        if outside.contains(&c.name) {
            continue;
        }
        let Some(&rank) = ranks.get(&c.name) else {
            problems.push(format!(
                "{} has no rank; add it to xtask/layers.toml or list it under [outside]",
                c.name
            ));
            continue;
        };
        for dep in &c.deps {
            if outside.contains(&dep.name) {
                // A build tool is allowed to be a build dependency and nothing else. What runs
                // during a build is not what ships in the binary, and generating a table from a
                // data file is the whole reason the build tools exist.
                if dep.at_build {
                    continue;
                }
                problems.push(format!(
                    "{} depends on {}, which is outside the layer stack and must not be \
                     linked into the compiler. A build tool may be a build dependency and \
                     nothing else",
                    c.name, dep.name
                ));
                continue;
            }
            let Some(&dep_rank) = ranks.get(&dep.name) else {
                problems.push(format!("{} depends on {}, which has no rank", c.name, dep.name));
                continue;
            };
            if dep_rank >= rank {
                problems.push(format!(
                    "{} (rank {rank}) depends on {} (rank {dep_rank}); a crate may depend \
                     only on strictly lower ranks. See {}",
                    c.name,
                    dep.name,
                    c.manifest.strip_prefix(&root).unwrap_or(&c.manifest).display()
                ));
            }
        }
    }

    // A rank in the table with no crate on disk is a rename nobody finished.
    let names: Vec<&str> = crates.iter().map(|c| c.name.as_str()).collect();
    for name in ranks.keys() {
        if !names.contains(&name.as_str()) {
            problems.push(format!("xtask/layers.toml ranks {name}, which is not in the workspace"));
        }
    }

    if problems.is_empty() {
        println!(
            "layers: {} crates, {} ranked, graph is acyclic by construction",
            crates.len(),
            ranks.len()
        );
        Ok(())
    } else {
        Err(Error::Failed { task: "layers", problems })
    }
}

// The prose style check.

/// Checks prose against the house rules.
///
/// Two rules, both mechanical, both chosen because they are the ones that quietly drift:
/// no em or en dashes, and no horizontal rules. Everything else about writing is a review
/// comment rather than a check.
fn style() -> Result<()> {
    let root = root();
    let mut problems = Vec::new();
    let mut files = Vec::new();
    collect_markdown(&root, &mut files)?;
    files.sort();

    for path in &files {
        let text = fs::read_to_string(path)?;
        let shown = path.strip_prefix(&root).unwrap_or(path).display();
        let mut fenced = false;
        for (n, line) in text.lines().enumerate() {
            let n = n + 1;
            if line.trim_start().starts_with("```") {
                fenced = !fenced;
                continue;
            }
            if fenced {
                continue;
            }
            if let Some(col) = line.find(['\u{2014}', '\u{2013}']) {
                problems.push(format!("{shown}:{n}:{col}: em or en dash; use a comma, a colon, a period or the word `to`"));
            }
            if line.trim() == "---" && n != 1 {
                problems.push(format!("{shown}:{n}: horizontal rule; use a heading instead"));
            }
        }
    }

    if problems.is_empty() {
        println!("style: {} markdown files, clean", files.len());
        Ok(())
    } else {
        Err(Error::Failed { task: "style", problems })
    }
}

// The bare threshold check.

/// Which directories a pass lives in, for the threshold check below.
///
/// One entry today. Section 40.12 names `rucc-opt` and that is where the passes are, and the list
/// is a list rather than a string so that the back end joins it by being added here, on the day
/// somebody moves its numbers into the heuristics file rather than on the day this is written.
const PASS_DIRS: &[&str] = &["crates/rucc-opt/src"];

/// The literals a comparison may name without being a threshold.
///
/// Zero, one and two. They are not tuning constants, they are structure: whether a value has any
/// users, whether it has more than one, whether a block has more than a pair of predecessors. A
/// number in that range is never something anybody would want to tune, and treating it as one
/// would mean a heuristics file full of entries called `ONE`.
const STRUCTURAL: &[&str] = &["0", "1", "2"];

/// Checks that no pass compares against a number it made up, per section 40.12.
///
/// The rule the document states is "A pass may not contain a bare numeric threshold; the coding
/// standard test greps for one", and this is that grep. The failure it exists to stop is not a
/// wrong number, it is a number nobody can find: an inlining limit written in the inliner and an
/// unrolling limit written in the unroller are two constants that will never be tuned together,
/// and neither of them will ever be measured, because measuring them means finding them first.
///
/// What counts as a threshold here is a bare integer literal on either side of a comparison. That
/// catches `if size > 40` and does not catch `size > limit`, which is the whole distinction. Three
/// things are allowed through:
///
/// - The literals in [`STRUCTURAL`], which are counts rather than thresholds.
/// - A line that mentions a width, since `ty.bits() >= 8` is a fact about the machine and there is
///   no version of the compiler where 8 is the wrong answer.
/// - A line carrying `// not a threshold:` and a reason, which is the escape hatch. It is a
///   comment rather than an attribute because the point is that somebody had to write the reason
///   down, and a reviewer reading the diff sees it.
///
/// Test modules are not checked. A test that asserts a pass fired eleven times is a test with the
/// number eleven in it, and there is nowhere else for that number to live.
fn thresholds() -> Result<()> {
    let root = root();
    let mut problems = Vec::new();
    let mut checked = 0;

    for dir in PASS_DIRS {
        let mut files = Vec::new();
        collect_rust(&root.join(dir), &mut files)?;
        files.sort();
        for path in &files {
            let text = fs::read_to_string(path)?;
            let shown = path.strip_prefix(&root).unwrap_or(path).display();
            checked += 1;
            for (n, line) in text.lines().enumerate() {
                // Everything from the test module on is somebody's expected value, and expected
                // values are literals by definition. Tests go last by convention in this tree, so
                // the first `#[cfg(test)]` ends the part of the file that is compiler.
                if line.trim_start().starts_with("#[cfg(test)]") {
                    break;
                }
                let code = line.split_once("//").map_or(line, |(before, _)| before);
                if code.contains("bits()") || code.contains("width") {
                    continue;
                }
                if line.contains("not a threshold:") {
                    continue;
                }
                for literal in compared_literals(code) {
                    if STRUCTURAL.contains(&literal.as_str()) {
                        continue;
                    }
                    problems.push(format!(
                        "{shown}:{}: compares against {literal}, which is a threshold nobody \
                         can find. Move it to rucc_cost::heuristics with the document that \
                         justifies it, or say `// not a threshold: <reason>` on this line.",
                        n + 1
                    ));
                }
            }
        }
    }

    if problems.is_empty() {
        println!("thresholds: {checked} pass files, no bare numbers");
        Ok(())
    } else {
        Err(Error::Failed { task: "thresholds", problems })
    }
}

/// Every integer literal that sits on one side of a comparison in this line.
///
/// Deliberately simple. It reads `<`, `>`, `<=`, `>=`, `==` and `!=`, and looks at the token on
/// each side, and a token counts only if it is digits and nothing else. That rules out `u32`,
/// `i128::from`, `x2` and every other place digits appear inside a name, which is where a
/// character by character reading has to be careful and a regular expression would not have been.
fn compared_literals(code: &str) -> Vec<String> {
    let bytes = code.as_bytes();
    let mut found = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let op = match bytes[i] {
            b'<' | b'>' => 1,
            b'=' | b'!' if i + 1 < bytes.len() && bytes[i + 1] == b'=' => 2,
            _ => {
                i += 1;
                continue;
            }
        };
        // `<` and `>` are also generics and shifts, `=>` is a match arm and `->` is a return type.
        // None of those are a comparison, and all of them sit next to numbers often enough that
        // not excluding them makes the check useless. A shift is checked on both sides because
        // the second `>` of a `>>` looks exactly like a comparison from where it stands.
        let doubled = matches!(bytes[i], b'<' | b'>')
            && (i + 1 < bytes.len() && bytes[i + 1] == bytes[i]
                || i > 0 && bytes[i - 1] == bytes[i]
                || i > 0 && matches!(bytes[i - 1], b'=' | b'-'));
        let end = i + if op == 1 && i + 1 < bytes.len() && bytes[i + 1] == b'=' { 2 } else { op };
        if !doubled {
            if let Some(literal) = token_before(code, i) {
                found.push(literal);
            }
            if let Some(literal) = token_after(code, end) {
                found.push(literal);
            }
        }
        i = end.max(i + 1);
    }
    found
}

/// The token ending just before `at`, if it is a bare integer literal.
fn token_before(code: &str, at: usize) -> Option<String> {
    let head = code[..at].trim_end();
    let start = head.rfind(|c: char| !c.is_ascii_alphanumeric() && c != '_').map_or(0, |i| i + 1);
    bare_integer(&head[start..])
}

/// The token starting just after `at`, if it is a bare integer literal.
fn token_after(code: &str, at: usize) -> Option<String> {
    let tail = code.get(at..)?.trim_start();
    let end = tail.find(|c: char| !c.is_ascii_alphanumeric() && c != '_').unwrap_or(tail.len());
    bare_integer(&tail[..end])
}

/// The token as a number, if that is all it is.
///
/// `40` yes. `u32`, `x40`, `40u32` and `0x40` no. A suffixed literal is a number somebody wrote
/// deliberately for a type reason and is almost always a mask or a limit of the type rather than a
/// tuning constant, and an underscore separated one is caught by the digits check anyway.
fn bare_integer(token: &str) -> Option<String> {
    if !token.is_empty() && token.bytes().all(|b| b.is_ascii_digit()) {
        return Some(token.to_owned());
    }
    None
}

fn collect_rust(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for e in fs::read_dir(dir)? {
        let path = e?.path();
        if path.is_dir() {
            collect_rust(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
    Ok(())
}

/// Where the written list of malformed forms lives, and where the tests that check them do.
///
/// One document and two files, because the list is about the IR extension and the IR extension
/// has one parser and one verifier. If it grows a third place to be rejected, this is the line
/// that changes.
const MALFORMED_LIST: &str = "spec/safe-memory/06-instrumentation.md";

/// The files whose tests the list is allowed to name.
const MALFORMED_TESTS: &[&str] = &["crates/rucc-ir/src/verify.rs", "crates/rucc-ir/src/parse.rs"];

/// Checks that every test the malformed-forms list names is a test that exists.
///
/// Document 06 section 6.6 is a written list of the safety forms the IR is not allowed to
/// express, and the whole reason it is written down is that a rule living only in the verifier's
/// source is a rule nobody can check the verifier against. Each row names the test that pins it.
///
/// A named test that has been renamed or deleted turns the list into a document that describes a
/// compiler we used to have, which is worse than no list, because a reader would believe it. This
/// is the grep that stops that. It does not check the other direction: a test that is not in the
/// list is fine, since plenty of verifier tests are about things that are not safety forms.
fn malformed() -> Result<()> {
    let root = root();
    let doc = fs::read_to_string(root.join(MALFORMED_LIST))?;
    let Some((_, list)) = doc.split_once("## 6.6 ") else {
        return Err(Error::Failed {
            task: "malformed",
            problems: vec![format!("{MALFORMED_LIST} has no section 6.6 to read")],
        });
    };

    let mut sources = String::new();
    for path in MALFORMED_TESTS {
        sources.push_str(&fs::read_to_string(root.join(path))?);
    }

    let mut problems = Vec::new();
    let mut named = 0;
    for name in backticked(list) {
        // A row names a test and also mentions instructions, section numbers and node kinds. A
        // test name is the only one of those that is a Rust identifier of some length with no
        // spaces and no dots in it.
        if name.len() < 15
            || !name.chars().all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit())
        {
            continue;
        }
        named += 1;
        if !sources.contains(&format!("fn {name}(")) {
            problems.push(format!(
                "{MALFORMED_LIST} section 6.6 names `{name}` and there is no such test in {}. \
                 Either the test was renamed and the row should follow it, or the form is no \
                 longer rejected and the row is a claim we cannot make.",
                MALFORMED_TESTS.join(" or ")
            ));
        }
    }

    if problems.is_empty() {
        println!("malformed: {named} forms listed, every one has its test");
        Ok(())
    } else {
        Err(Error::Failed { task: "malformed", problems })
    }
}

/// Where the interposition table's rows are written, one file per group, in the order the
/// compiler's list has to spell them.
const INTERPOSE_ROWS: &[&str] =
    &["runtime/rucc-safe-rt/src/wrap.rs", "runtime/rucc-safe-rt/src/syscall.rs"];

/// Where the compiler's copy of the same names is written.
const INTERPOSE_NAMES: &str = "crates/rucc-safety/src/wrap.rs";

/// Checks that the interposition table and the compiler's copy of its names say the same thing.
///
/// There are two lists because there have to be. The table lives in `rucc-safe-rt`, which is
/// compiled for the target, and the redirection lives in `rucc-safety`, which runs on the host, so
/// the compiler cannot read the runtime's table without building the runtime twice.
///
/// Two lists that are supposed to agree will not, and the two ways they fail are not equally loud.
/// A name the compiler knows with no row behind it redirects a call to a symbol that does not
/// exist, which is a link error and gets noticed. A row with no name in front of it is a wrapper
/// nothing calls, which is a hole in the monitor that looks exactly like a program with no bugs in
/// it. This is the grep that catches the quiet one.
///
/// The order has to match as well as the contents, which is stricter than anything depends on and
/// is worth it: two lists in different orders are two lists a person has to sort before they can
/// compare them, and the next hundred rows are going in by hand.
fn interpose() -> Result<()> {
    let root = root();
    let names = fs::read_to_string(root.join(INTERPOSE_NAMES))?;

    let mut written: Vec<String> = Vec::new();
    for group in INTERPOSE_ROWS {
        let rows = fs::read_to_string(root.join(group))?;
        let Some((_, table)) = rows.split_once("interpose! {") else {
            return Err(Error::Failed {
                task: "interpose",
                problems: vec![format!("{group} has no interpose! table to read")],
            });
        };
        // The rows stop where the invocation does, which is the first closing brace in column one.
        // Everything after it is the test module, whose functions are indented the same way a row
        // is.
        let table = table.split_once("\n}\n").map_or(table, |(inside, _)| inside);
        written.extend(
            table
                .lines()
                .filter_map(|line| line.strip_prefix("    fn "))
                .filter_map(|rest| rest.split_once('('))
                .map(|(name, _)| name.to_owned()),
        );
    }
    let rows = INTERPOSE_ROWS.join(" and ");

    let Some((_, list)) = names.split_once("INTERPOSED: &[&str] = &[") else {
        return Err(Error::Failed {
            task: "interpose",
            problems: vec![format!("{INTERPOSE_NAMES} has no INTERPOSED list to read")],
        });
    };
    let Some((list, _)) = list.split_once("];") else {
        return Err(Error::Failed {
            task: "interpose",
            problems: vec![format!("{INTERPOSE_NAMES} has an INTERPOSED list that never ends")],
        });
    };
    let known: Vec<String> = quoted(list);

    let mut problems = Vec::new();
    for name in &written {
        if !known.contains(name) {
            problems.push(format!(
                "{rows} has a row for `{name}` and {INTERPOSE_NAMES} does not name it, so the \
                 wrapper is generated and nothing is redirected to it. That is a hole in the \
                 monitor that looks exactly like a program with no bugs in it."
            ));
        }
    }
    for name in &known {
        if !written.contains(name) {
            problems.push(format!(
                "{INTERPOSE_NAMES} names `{name}` and {rows} has no row for it, so every call to \
                 it is redirected to a symbol that does not exist."
            ));
        }
    }
    if problems.is_empty() && written != known {
        problems.push(format!(
            "{rows} and {INTERPOSE_NAMES} hold the same names in different orders. Two lists a \
             person has to sort before they can compare them is how the next hundred rows go \
             wrong."
        ));
    }

    if problems.is_empty() {
        println!("interpose: {} rows, the compiler and the runtime agree", written.len());
        Ok(())
    } else {
        Err(Error::Failed { task: "interpose", problems })
    }
}

/// Every run of text between double quotes in this fragment.
fn quoted(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('"') {
        rest = &rest[open + 1..];
        let Some(close) = rest.find('"') else { break };
        out.push(rest[..close].to_string());
        rest = &rest[close + 1..];
    }
    out
}

/// Every run of text between backticks in this fragment.
fn backticked(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('`') {
        rest = &rest[open + 1..];
        let Some(close) = rest.find('`') else { break };
        out.push(rest[..close].to_string());
        rest = &rest[close + 1..];
    }
    out
}

/// Checks that every version number in the tree agrees with the workspace manifest.
///
/// The workspace manifest is the one that gets edited when the version goes up, and it is the
/// one the release workflow checks the tag against. Two other places repeat it and neither of
/// them breaks anything by being wrong: the exact pins between our own crates, which a partial
/// publish would trip over, and the `html_root_url` of every published crate, which sends a
/// reader of the docs to the wrong version and says nothing at all while doing it. Both of them
/// drifted between 0.1.0 and 0.1.1, which is why this task exists.
fn version() -> Result<()> {
    let root = root();
    let manifest = fs::read_to_string(root.join("Cargo.toml"))?;
    let want = manifest
        .lines()
        .find_map(|l| l.strip_prefix("version = "))
        .map(|v| v.trim().trim_matches('"').to_owned())
        .ok_or_else(|| Error::Io("the workspace manifest has no version".to_owned()))?;
    let mut problems = Vec::new();

    let pin = format!("version = \"={want}\"");
    for (n, line) in manifest.lines().enumerate() {
        if line.starts_with("rucc-") && line.contains("version = \"=") && !line.contains(&pin) {
            problems.push(format!("Cargo.toml:{}: not pinned to {want}: {line}", n + 1));
        }
    }

    let mut checked = 0;
    for c in read_crates(&root)? {
        let dir = c.manifest.parent().unwrap_or(&root);
        // A member that spells a version out instead of inheriting one from the workspace
        // table keeps its own copy of the number, and that copy is what went stale.
        let member = fs::read_to_string(&c.manifest)?;
        let shown_manifest = c.manifest.strip_prefix(&root).unwrap_or(&c.manifest).display();
        for (n, line) in member.lines().enumerate() {
            if line.starts_with("rucc-") && line.contains("version = ") {
                problems.push(format!(
                    "{shown_manifest}:{}: names a version instead of inheriting one: {line}",
                    n + 1
                ));
            }
        }
        // Only the published compiler crates. `xtask` and the build tools have no docs.rs page
        // to send anyone to.
        if !dir.starts_with(root.join("crates")) {
            continue;
        }
        let lib = dir.join("src/lib.rs");
        let Ok(text) = fs::read_to_string(&lib) else {
            continue;
        };
        let shown = lib.strip_prefix(&root).unwrap_or(&lib).display();
        let want_url = format!("https://docs.rs/{}/{want}", c.name);
        match text.lines().enumerate().find(|(_, l)| l.contains("html_root_url")) {
            Some((n, line)) if !line.contains(&want_url) => {
                problems.push(format!("{shown}:{}: html_root_url is not {want_url}", n + 1));
            }
            Some(_) => checked += 1,
            None => problems.push(format!("{shown}: no html_root_url")),
        }
    }

    if problems.is_empty() {
        println!("version: {want}, {checked} crates agree");
        Ok(())
    } else {
        Err(Error::Failed { task: "version", problems })
    }
}

fn collect_markdown(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for e in fs::read_dir(dir)? {
        let path = e?.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        if name.starts_with('.') || name == "target" {
            continue;
        }
        if path.is_dir() {
            collect_markdown(&path, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
            out.push(path);
        }
    }
    Ok(())
}

// The golden suite.

/// The triple every golden case is compiled for, whatever the host is.
///
/// This and the dialect below have to be what `crates/rucc/tests/golden.rs` uses, since that is
/// what reads back what this writes. They are two constants rather than one shared one because
/// `xtask` has no dependencies, which is the rule `spec/18-package-layout.md` section 18.7 sets
/// so that the build cannot break because of somebody else's release.
const GOLDEN_TARGET: &str = "x86_64-unknown-linux-gnu";

/// The dialect every golden case is compiled under, which is the default one.
const GOLDEN_STD: &str = "gnu23";

/// The comment a case names its own dialect with, which the suite in `crates/rucc/tests` reads
/// the same way. A case that does not name one is compiled under [`GOLDEN_STD`].
const GOLDEN_STD_DIRECTIVE: &str = "// std: ";

/// Rewrites the expectation beside every case in `tests/golden` from what the compiler produces
/// now.
///
/// This is the only way those files are meant to be edited, and running it is half of the job.
/// The other half is reading the diff: a golden file that gets blessed without anybody looking
/// at what changed is a test that has stopped testing. What the diff is for is the change
/// nobody meant, which is the kind no unit test was going to be written for in advance.
///
/// A case that produces a diagnostic is refused rather than blessed, because the expectations
/// hold the tree and not the messages, and a case that warns is one whose expectation would
/// silently be the tree of a program the compiler had complained about.
///
/// The one refusal that is not a problem is a construct the walk to the IR has not been written
/// for yet. Such a case keeps its `.tast` and has no `.ir` beside it, and the suite checks that
/// it still cannot be lowered, so the day it can is the day the suite asks for the expectation.
fn bless() -> Result<()> {
    let dir = root().join("tests").join("golden");
    let mut cases: Vec<PathBuf> = fs::read_dir(&dir)
        .map_err(|e| Error::Io(format!("{}: {e}", dir.display())))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "c"))
        .collect();
    cases.sort();
    if cases.is_empty() {
        return Err(Error::Io(format!("{}: no cases", dir.display())));
    }

    let mut problems = Vec::new();
    let mut changed = 0;
    for case in &cases {
        let name = case.file_name().unwrap_or(case.as_os_str()).to_string_lossy().into_owned();
        match produce(&name, "tast")? {
            Ok(stdout) => bless_file(&case.with_extension("tast"), &stdout, &mut changed)?,
            Err(said) => {
                problems.push(format!("{name}: {said}"));
                continue;
            }
        }
        let expected = case.with_extension("ir");
        match produce(&name, "ir")? {
            Ok(stdout) => bless_file(&expected, &stdout, &mut changed)?,
            Err(said) if said.contains("[E0519]") => {
                if expected.exists() {
                    fs::remove_file(&expected)
                        .map_err(|e| Error::Io(format!("{}: {e}", expected.display())))?;
                    println!("dropped {}", file_name(&expected));
                    changed += 1;
                }
            }
            Err(said) => problems.push(format!("{name}: {said}")),
        }
    }
    if !problems.is_empty() {
        return Err(Error::Failed { task: "bless", problems });
    }
    println!("xtask: {changed} expectation(s) rewritten, {} case(s)", cases.len());
    Ok(())
}

/// Runs the compiler over one golden case, and gives back what it wrote to standard output or
/// what it said when it refused.
///
/// Through `cargo run` rather than a path under `target`, so that the binary is up to date and
/// so that this keeps working wherever `CARGO_TARGET_DIR` points. The case is named relative to
/// the repository root, because the name of the input is printed in the IR module header and an
/// absolute path would bless the layout of one person's disk into a file everybody has to match.
/// With forward slashes, since Windows opens it either way and only one spelling can be blessed.
/// The dialect one case asks to be compiled under, which is the default one unless it says.
///
/// A case that is about a rule which changed cannot be written in the default dialect: `int f();`
/// means a function taking anything before C23 and a function taking nothing from C23 on.
fn golden_dialect(name: &str) -> String {
    let path = root().join("tests").join("golden").join(name);
    let text = fs::read_to_string(path).unwrap_or_default();
    for line in text.lines() {
        if let Some(named) = line.strip_prefix(GOLDEN_STD_DIRECTIVE) {
            return named.trim().to_owned();
        }
    }
    GOLDEN_STD.to_owned()
}

fn produce(name: &str, kind: &str) -> Result<std::result::Result<Vec<u8>, String>> {
    let out = Command::new("cargo")
        .args(["run", "-q", "-p", "rucc", "--"])
        .arg(format!("--target={GOLDEN_TARGET}"))
        .arg(format!("-std={}", golden_dialect(name)))
        .arg(format!("--emit={kind}"))
        .arg(format!("tests/golden/{name}"))
        .args(["-o", "-"])
        .current_dir(root())
        .output()
        .map_err(|e| Error::Io(format!("could not run cargo: {e}")))?;
    if !out.status.success() || !out.stderr.is_empty() {
        return Ok(Err(String::from_utf8_lossy(&out.stderr).trim().replace('\n', "; ")));
    }
    Ok(Ok(out.stdout))
}

/// Writes one expectation, and says so, when what the compiler produces is not what is there.
fn bless_file(path: &Path, produced: &[u8], changed: &mut usize) -> Result<()> {
    if fs::read(path).unwrap_or_default() == produced {
        return Ok(());
    }
    fs::write(path, produced).map_err(|e| Error::Io(format!("{}: {e}", path.display())))?;
    println!("blessed {}", file_name(path));
    *changed += 1;
    Ok(())
}

/// The last component of a path, for a message about it.
fn file_name(path: &Path) -> String {
    path.file_name().unwrap_or(path.as_os_str()).to_string_lossy().into_owned()
}

// The target-side runtime.

/// Builds `rucc-builtins` into the static library the driver puts on a link line.
///
/// This is an `xtask` rather than part of `cargo build` because it is the one crate in the
/// workspace compiled *for the target* rather than for the host, and a `cargo build` of the
/// workspace on a machine that has no standard library for the target should still work. So the
/// normal build compiles this crate for the host as an ordinary library, which is what makes its
/// tests run, and this task compiles the same source again as a `staticlib` for wherever the
/// generated code is going.
///
/// The output lands where `cargo` puts it, `target/<triple>/release/librucc_builtins.a`, and the
/// path is printed because the thing that wants it next is a link line.
///
/// # Errors
///
/// [`Error::Io`] when `cargo` cannot be run or when the target has no `core` installed, which is
/// the usual reason this fails and is worth saying out loud rather than passing on a linker
/// message about a missing crate.
fn builtins(args: &[String]) -> Result<()> {
    let mut target = None;
    let mut at = 0;
    while at < args.len() {
        match args[at].as_str() {
            "--target" => {
                at += 1;
                target = Some(
                    args.get(at)
                        .cloned()
                        .ok_or_else(|| Error::Io("--target wants a triple after it".to_owned()))?,
                );
            }
            other => match other.strip_prefix("--target=") {
                Some(triple) => target = Some(triple.to_owned()),
                None => return Err(Error::Io(format!("builtins: unknown argument `{other}`"))),
            },
        }
        at += 1;
    }
    let target = match target {
        Some(triple) => triple,
        // The host, because building for the machine you are on is the case that always works
        // and is what somebody typing this with no arguments is asking for.
        None => host_triple()?,
    };

    println!("{}", staticlib("rucc-builtins", &target)?.display());
    Ok(())
}

/// Builds one of the target-side crates as a static library for `target`, and says where it is.
///
/// `cargo rustc` rather than `cargo build`, because the crate type is a property of this build and
/// not of the crate. Written in `Cargo.toml` instead it would make every ordinary `cargo build`
/// produce an archive full of `no_mangle` C names, and that archive would then be sitting in
/// `target/` waiting for something to link it by accident.
///
/// # Errors
///
/// [`Error::Failed`] when the build fails or produces nothing, which on a fresh machine is almost
/// always the standard library for that target not being installed.
fn staticlib(package: &str, target: &str) -> Result<PathBuf> {
    let status = Command::new("cargo")
        .args(["rustc", "-q", "-p", package, "--release", "--crate-type", "staticlib"])
        .arg("--target")
        .arg(target)
        .current_dir(root())
        .status()
        .map_err(|e| Error::Io(format!("could not run cargo: {e}")))?;
    if !status.success() {
        return Err(Error::Failed {
            task: "staticlib",
            problems: vec![format!(
                "building {package} for {target} failed. If the message above is about `core`, \
                 the standard library for that target is not installed: `rustup target add \
                 {target}`"
            )],
        });
    }

    let file = format!("lib{}.a", package.replace('-', "_"));
    let archive = root().join("target").join(target).join("release").join(file);
    if !archive.is_file() {
        return Err(Error::Failed {
            task: "staticlib",
            problems: vec![format!(
                "cargo reported success but {} is not there",
                archive.display()
            )],
        });
    }
    Ok(archive)
}

/// The triple of the machine this is running on, as `rustc` names it.
fn host_triple() -> Result<String> {
    let out = Command::new("rustc")
        .arg("-vV")
        .output()
        .map_err(|e| Error::Io(format!("could not run rustc: {e}")))?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .find_map(|line| line.strip_prefix("host: "))
        .map(str::to_owned)
        .ok_or_else(|| Error::Io("rustc -vV did not say what host it is for".to_owned()))
}

// The local mirror of CI.

/// Runs what CI runs, in the order CI runs it.
///
/// The order is the point: the cheap checks come first, so a formatting mistake costs
/// seconds rather than a full test run.
fn ci() -> Result<()> {
    let steps: &[(&str, &[&str])] = &[
        ("cargo", &["fmt", "--all", "--check"]),
        (
            "cargo",
            &["clippy", "--workspace", "--all-targets", "--all-features", "--", "-D", "warnings"],
        ),
        ("cargo", &["test", "--workspace", "--all-features"]),
        ("cargo", &["doc", "--workspace", "--no-deps"]),
    ];
    layers()?;
    style()?;
    thresholds()?;
    malformed()?;
    interpose()?;
    version()?;
    for (bin, args) in steps {
        println!("xtask: running {bin} {}", args.join(" "));
        let status = Command::new(bin)
            .args(*args)
            .current_dir(root())
            .status()
            .map_err(|e| Error::Io(format!("could not run {bin}: {e}")))?;
        if !status.success() {
            return Err(Error::Failed {
                task: "ci",
                problems: vec![format!("{bin} {} failed", args.join(" "))],
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{bare_integer, compared_literals};

    #[test]
    fn a_comparison_against_a_number_is_found() {
        assert_eq!(compared_literals("if size > 40 {"), ["40"]);
        assert_eq!(compared_literals("if growth >= 256 {"), ["256"]);
        assert_eq!(compared_literals("if n == 7 {"), ["7"]);
        assert_eq!(compared_literals("if n != 7 {"), ["7"]);
        assert_eq!(compared_literals("if 40 < size {"), ["40"]);
    }

    #[test]
    fn a_comparison_against_something_with_a_name_is_not() {
        // The whole distinction. A number that came from somewhere is fine and a number that came
        // from nowhere is the thing being looked for.
        assert!(compared_literals("if size > limit {").is_empty());
        assert!(
            compared_literals("if size > heuristics::INLINE_GROWTH_SQUARING_BOUND {").is_empty()
        );
    }

    #[test]
    fn the_things_that_look_like_comparisons_and_are_not() {
        // Every one of these appeared in the tree while this was being written, and every one of
        // them would have been reported by a check that only looked for an angle bracket next to
        // a digit.
        assert!(compared_literals("fn shift(n: u32) -> u32 { n >> 3 }").is_empty());
        assert!(compared_literals("let x = n << 3;").is_empty());
        assert!(compared_literals("match n { 4 => 5, _ => 6 }").is_empty());
        assert!(compared_literals("fn f() -> Option<u32> { None }").is_empty());
        assert!(compared_literals("let v: Vec<[u8; 4]> = Vec::new();").is_empty());
    }

    #[test]
    fn a_number_inside_a_name_is_not_a_number() {
        assert_eq!(bare_integer("40"), Some("40".to_owned()));
        assert_eq!(bare_integer("u32"), None);
        assert_eq!(bare_integer("x40"), None);
        assert_eq!(bare_integer("40u32"), None);
        assert_eq!(bare_integer(""), None);
    }

    #[test]
    fn the_pass_directories_exist() {
        // The check silently passes if it is pointed at a directory that is not there, and a
        // silently passing coding standard is worse than none, because somebody is relying on it.
        for dir in super::PASS_DIRS {
            let path = super::root().join(dir);
            assert!(path.is_dir(), "{} does not exist", path.display());
        }
    }
}
