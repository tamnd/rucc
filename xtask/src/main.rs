//! Build automation.
//!
//! The rule from `spec/18-package-layout.md` section 18.7 is that `cargo build` works with
//! no configuration and no external tools, and everything else is an `xtask`. There is no
//! `configure`, no CMake and no Python in the build, because build system complexity accretes
//! silently and the "clone and build" claim is worth protecting.
//!
//! This crate has no dependencies on purpose. It runs before anything else in CI, including
//! `cargo deny`, so it should not be able to fail because of somebody else's release.

// Not `aux.rs`. `AUX` is a reserved device name on Windows whatever extension follows it, and git
// refuses to write such a path at all, so a file called that fails the checkout on the Windows
// runner before anything is compiled. The task is still spelled `aux` on the command line.
mod aux_plane;
mod bench;
mod bisect;
mod builtins_diff;
mod compress;
mod corpus;
mod cost;
mod differential;
mod disasm;
mod dso;
mod fuzz;
mod implib;
mod pressure;
mod real_libc;
mod runner;
mod safety;
mod size;
mod stubs;
mod unwind;
mod wide;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::{fmt, fs, io};

const USAGE: &str = "\
usage: cargo xtask <task>

tasks:
  layers            check the crate dependency graph against xtask/layers.toml
  style             check documentation and specification prose against the house rules
  thresholds        check that no pass compares against a number it made up
  malformed         check that the written list of malformed IR forms still names real tests
  paths             check that every tracked path can be checked out on Windows
  version           check that every version number in the tree agrees with the workspace's
  targets           regenerate docs/TARGETS.md from the target table, or check it with --check
  abi-corpus        regenerate tests/abi-corpus from the layout engine, or check it with --check
  abi-signatures    regenerate tests/abi-signatures, or check it with --check
  link-lines        regenerate tests/link-lines from rucc-sysroot, or check it with --check
  provenance        regenerate PROVENANCE from the tables, or check it with --check
  abi-differential  compile the signature corpus with both compilers in both directions and run it
  builtins          compile the C runtime support routines into a static library for a target
  builtins-diff     hold the C runtime routines against the Rust reference over the same cases
  bench             time the throughput floor workload against the reference compiler
  size              measure the distribution against the budget in document 13.1
  disasm            check every instruction we encode against an independent decoder
  stubs             write a sysroot's libraries per ELF target and read them back with readelf
  implib            write an import library per Windows target and hold it against llvm-dlltool
  real-libc         hold a stub written from a glibc abilist against this machine's libc.so.6
  dso               build a shared library out of what we emit, link a program against it, run it
  unwind            walk a stack through frames we wrote and count what came back
  wide              compile 128-bit arithmetic with both compilers, run both, compare
  safety            compile, link and run tests/safety, and hold each program to its verdict
  accounting        build tests/safety twice at -O2, with elimination and without, and compare
  fuzz              generate C programs with one memory error each and hold both builds to it
  cost              time bench/safety with the monitor off and on at -O0, or at a level and
                    any -f flags given
  pressure          compile bench/safety both ways at -O2, and with any -f flags given, and
                    report the spill and fill delta
  aux               simulate the two aux plane layouts and compare their cache misses
  compress          sweep what a compressed capability in an aux slot can say exactly
  bisect            halve the optimizer's fuel until one rewrite is left holding the bug
  corpus            run the pinned C corpus against the compiler this tree builds
  bless             rewrite the expectations in tests/golden from what the compiler produces now
  interpose         check the interposition table and the compiler's copy of it agree
  ci                run everything the per-commit CI job runs, in the same order
  help              print this message
";

fn main() -> ExitCode {
    let task = std::env::args().nth(1);
    let result = match task.as_deref() {
        Some("layers") => layers(),
        Some("style") => style(),
        Some("thresholds") => thresholds(),
        Some("malformed") => malformed(),
        Some("paths") => paths(),
        Some("interpose") => interpose(),
        Some("version") => version(),
        Some("targets") => targets(&std::env::args().skip(2).collect::<Vec<_>>()),
        Some("abi-corpus") => abi_corpus(&std::env::args().skip(2).collect::<Vec<_>>()),
        Some("link-lines") => link_lines(&std::env::args().skip(2).collect::<Vec<_>>()),
        Some("provenance") => provenance(&std::env::args().skip(2).collect::<Vec<_>>()),
        Some("abi-signatures") => abi_signatures(&std::env::args().skip(2).collect::<Vec<_>>()),
        Some("abi-differential") => differential::differential(),
        Some("builtins") => builtins(&std::env::args().skip(2).collect::<Vec<_>>()),
        Some("builtins-diff") => builtins_diff::builtins_diff(),
        Some("bench") => bench::bench(&std::env::args().skip(2).collect::<Vec<_>>()),
        Some("disasm") => disasm::disasm(),
        Some("stubs") => stubs::stubs(),
        Some("implib") => implib::implib(),
        Some("real-libc") => real_libc::real_libc(&std::env::args().skip(2).collect::<Vec<_>>()),
        Some("dso") => dso::dso(),
        Some("unwind") => unwind::unwind(),
        Some("wide") => wide::wide(),
        Some("safety") => safety::safety(),
        Some("size") => size::size(&std::env::args().skip(2).collect::<Vec<_>>()),
        Some("accounting") => safety::accounting(),
        Some("fuzz") => fuzz::fuzz(&std::env::args().skip(2).collect::<Vec<_>>()),
        Some("cost") => cost::cost(&std::env::args().skip(2).collect::<Vec<_>>()),
        Some("pressure") => pressure::pressure(&std::env::args().skip(2).collect::<Vec<_>>()),
        Some("aux") => aux_plane::aux(),
        Some("compress") => compress::compress(),
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

/// Everything indented by two, so that a program's output inside a problem reads as its output.
fn indent(text: &str) -> String {
    text.lines().map(|line| format!("      {line}\n")).collect()
}

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
const INTERPOSE_ROWS: &[&str] = &[
    "runtime/rucc-safe-rt/src/wrap.rs",
    "runtime/rucc-safe-rt/src/syscall.rs",
    "runtime/rucc-safe-rt/src/sync.rs",
];

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

/// The file names Windows will not write, whatever extension follows them.
///
/// They are device names rather than names, so `AUX`, `aux.rs` and `aux.tar.gz` are all the same
/// request to the kernel and all of them fail. `CONIN$` and `CONOUT$` are the two newer ones and
/// they are in the list for the same reason as the rest.
const RESERVED_STEMS: &[&str] = &[
    "con", "prn", "aux", "nul", "conin$", "conout$", "com0", "com1", "com2", "com3", "com4",
    "com5", "com6", "com7", "com8", "com9", "lpt0", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6",
    "lpt7", "lpt8", "lpt9",
];

/// The characters a Windows path component may not contain.
const ILLEGAL_CHARS: &[char] = &['<', '>', ':', '"', '|', '?', '*', '\\'];

/// Checks that every tracked path can be checked out on Windows.
///
/// This is the cheapest check in the tree and it is here because the bug it catches has landed
/// twice. `xtask/src/aux.rs` was renamed to `xtask/src/aux_plane.rs` because `AUX` is a reserved
/// device name on Windows whatever extension follows it, and then the same file appeared again as
/// `runtime/rucc-safe-rt/src/aux.rs` and the Windows job failed the same way a second time.
///
/// What makes it worth a check of its own rather than a note in a review is how it fails. A path
/// Windows will not write does not fail a test, it fails `git clone`, which means no crate is
/// compiled, no test is collected and the job's log has one line in it that is about git. Nothing
/// else in CI can see it, because everything else in CI needs a checkout first. So the guard has to
/// run somewhere that is not Windows, which is every other job, and it is a string comparison over
/// the output of `git ls-files`.
///
/// Four rules, which are the four ways a path that is fine on Linux is not a path on Windows: a
/// reserved device stem, a character that is not allowed in a component, a component that ends in a
/// dot or a space, and two paths that differ only in case. The last one is not a refusal by Windows
/// but a collision on it, where checking out the second file overwrites the first and the working
/// tree is dirty the moment it exists.
fn paths() -> Result<()> {
    let out = Command::new("git")
        .args(["ls-files", "-z"])
        .current_dir(root())
        .output()
        .map_err(|e| Error::Io(format!("could not run git: {e}")))?;
    if !out.status.success() {
        return Err(Error::Failed {
            task: "paths",
            problems: vec!["git ls-files failed, so there is no list of tracked files".to_owned()],
        });
    }
    let listing = String::from_utf8_lossy(&out.stdout);
    let files: Vec<&str> = listing.split('\0').filter(|path| !path.is_empty()).collect();

    let mut problems = Vec::new();
    let mut folded: BTreeMap<String, &str> = BTreeMap::new();
    for path in &files {
        for part in path.split('/') {
            // The stem is everything before the first dot, because that is what Windows matches a
            // device name against. `aux.rs` is the device, not a file with an extension.
            let stem = part.split('.').next().unwrap_or(part).to_ascii_lowercase();
            if RESERVED_STEMS.contains(&stem.as_str()) {
                problems.push(format!(
                    "{path} has the component `{part}`, and `{stem}` is a reserved device name on \
                     Windows whatever extension follows it, so git cannot write the path at all \
                     and the checkout fails before anything is compiled. Rename the file, the way \
                     xtask/src/aux_plane.rs already is."
                ));
            }
            if let Some(bad) = part.chars().find(|c| ILLEGAL_CHARS.contains(c)) {
                problems.push(format!(
                    "{path} has the component `{part}`, which contains `{bad}`, and a Windows path \
                     component may not. Rename the file."
                ));
            }
            if let Some(bad) = part.chars().find(|c| (*c as u32) < 0x20) {
                problems.push(format!(
                    "{path} has a component holding the control character {:#04x}, which no \
                     Windows path may contain. Rename the file.",
                    bad as u32
                ));
            }
            if part.ends_with('.') || part.ends_with(' ') {
                problems.push(format!(
                    "{path} has the component `{part}`, which ends in a dot or a space. Windows \
                     strips both, so the file is written under a different name than the one in \
                     the index and the working tree is dirty as soon as it exists. Rename the file."
                ));
            }
        }
        if let Some(first) = folded.insert(path.to_ascii_lowercase(), path) {
            problems.push(format!(
                "{first} and {path} differ only in case, so on a case insensitive filesystem the \
                 second one checked out overwrites the first. Rename one of them."
            ));
        }
    }

    if problems.is_empty() {
        println!("paths: {} files, every one of them checks out on Windows", files.len());
        Ok(())
    } else {
        Err(Error::Failed { task: "paths", problems })
    }
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

/// Compiles the C in `runtime/builtins` into the static library the driver puts on a link line.
///
/// With rucc itself, which is the whole point of the task rather than a detail of how it is
/// implemented. `spec/12-abi-and-runtime.md` section 12.8 settled in tamnd/rucc#912 that what
/// ships is C compiled by this compiler, and the reason was thirty targets: the Rust path needs a
/// Rust target for every row of `spec/cross-compile/04-target-matrix.md` and that table has rows
/// rustc does not have, and the archive rustc produces brings Rust's own `compiler_builtins` with
/// it, which was 4.5 MB for four routines against a 10 MB budget for every tier 1 and tier 2
/// archive together.
///
/// It also makes each archive evidence. A target whose builtins do not build is a target whose
/// codegen does not work, and that is a better thing to learn here than on somebody's link line.
///
/// The compiler is built first, release, because this is the one task whose tool is the thing
/// under test. Then one command line compiles every `.c` in that directory and writes the archive,
/// which is what `--emit=archive` is for: the objects never reach the file system and the symbol
/// index comes from what the writer just wrote.
///
/// The output lands where `cargo rustc --crate-type staticlib` used to put it,
/// `target/<triple>/release/librucc_builtins.a`, so that `cargo xtask size` and the driver's
/// search still find it under the name they already look for. The path is printed because the
/// thing that wants it next is a link line.
///
/// # Errors
///
/// [`Error::Io`] when `cargo` or the compiler cannot be run, and [`Error::Failed`] when the build
/// of the compiler fails, when there is no C to compile, or when compiling it fails. That last one
/// is the interesting failure and the message says so, because on a target this compiler does not
/// support yet it is the answer rather than an accident.
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

    println!("{}", builtins_archive(&target)?.display());
    Ok(())
}

/// Writes the archive for one target and gives back the path to it.
///
/// Separate from the task above because `cargo xtask builtins-diff` wants the archive
/// rather than a line of output about it, and because the two asking the same function for it is
/// what keeps the differential run about the archive people actually get.
///
/// # Errors
///
/// As [`builtins`], which is the only other caller.
fn builtins_archive(target: &str) -> Result<PathBuf> {
    let sources = builtin_sources()?;
    let archive = root().join("target").join(target).join("release").join("librucc_builtins.a");
    if let Some(dir) = archive.parent() {
        fs::create_dir_all(dir).map_err(|e| Error::Io(format!("{}: {e}", dir.display())))?;
    }

    let rucc = cost::compiler()?;
    let status = Command::new(&rucc)
        .arg(format!("--target={target}"))
        // Freestanding because this is what a program gets instead of a C library, so there is no
        // C library under it to call. No builtins because a loop that copies bytes is a loop a
        // compiler may recognize and replace with a call to memcpy, and in this file that call
        // would be the function calling itself. This is the flag the Rust crate spells
        // `#![no_builtins]`.
        .args(["-ffreestanding", "-fno-builtin", "-O2", "--emit=archive", "-o"])
        .arg(&archive)
        .args(&sources)
        .current_dir(root())
        .status()
        .map_err(|e| Error::Io(format!("could not run {}: {e}", rucc.display())))?;
    if !status.success() {
        return Err(Error::Failed {
            task: "builtins",
            problems: vec![format!(
                "compiling the runtime support routines for {target} failed. The message above is \
                 this compiler's, and on a target it does not support yet that is the answer \
                 rather than an accident"
            )],
        });
    }
    if !archive.is_file() {
        return Err(Error::Failed {
            task: "builtins",
            problems: vec![format!(
                "the compiler reported success but {} is not there",
                archive.display()
            )],
        });
    }
    Ok(archive)
}

/// Every `.c` under `runtime/builtins`, in the order their names sort in.
///
/// Sorted rather than in whatever order the file system hands them back, because the members of an
/// archive come out in the order they went in and `spec/cross-compile/13-distribution.md` section
/// 13.6 asks for the same bytes from the same tree on any machine.
///
/// # Errors
///
/// [`Error::Io`] when the directory cannot be read, and [`Error::Failed`] when there is no C in it
/// at all, which would otherwise write an empty archive and call it a success.
fn builtin_sources() -> Result<Vec<PathBuf>> {
    let dir = root().join("runtime").join("builtins");
    let mut sources = Vec::new();
    for entry in fs::read_dir(&dir).map_err(|e| Error::Io(format!("{}: {e}", dir.display())))? {
        let path = entry.map_err(|e| Error::Io(format!("{}: {e}", dir.display())))?.path();
        if path.extension().is_some_and(|e| e == "c") {
            sources.push(path);
        }
    }
    sources.sort();
    if sources.is_empty() {
        return Err(Error::Failed {
            task: "builtins",
            problems: vec![format!("there is no C to compile in {}", dir.display())],
        });
    }
    Ok(sources)
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
/// Regenerate `docs/TARGETS.md`, or check that the file on disk still matches the table.
///
/// The work is in `rucc-targets` rather than here, because the table lives in `rucc-tuple` and
/// this crate has no dependencies on purpose. What is here is the name people type.
fn targets(args: &[String]) -> Result<()> {
    let mut call = vec!["run", "-q", "-p", "rucc-targets", "--", "docs"];
    if args.iter().any(|a| a == "--check") {
        call.push("--check");
    }
    let status = Command::new("cargo")
        .args(&call)
        .current_dir(root())
        .status()
        .map_err(|e| Error::Io(format!("could not run cargo: {e}")))?;
    if status.success() {
        return Ok(());
    }
    Err(Error::Failed {
        task: "targets",
        problems: vec!["docs/TARGETS.md does not match the table in rucc-tuple".to_owned()],
    })
}

/// Regenerate `tests/abi-corpus`, or check that what is on disk still matches the layout engine.
///
/// The same shape as `targets` and for the same reason: the numbers come from
/// `rucc_types::layout_record` and `xtask` has no dependencies, so the work is in `rucc-targets`
/// and what is here is the name people type. The check is against the generator rather than
/// against a reference compiler, which is a different question and one that needs a toolchain
/// this repository does not carry. That check is the job in tamnd/rucc-cross.
fn abi_corpus(args: &[String]) -> Result<()> {
    let mode = if args.iter().any(|a| a == "--check") { "--check" } else { "--write" };
    let status = Command::new("cargo")
        .args(["run", "-q", "-p", "rucc-targets", "--", "abi-corpus", mode])
        .current_dir(root())
        .status()
        .map_err(|e| Error::Io(format!("could not run cargo: {e}")))?;
    if status.success() {
        return Ok(());
    }
    Err(Error::Failed {
        task: "abi-corpus",
        problems: vec!["tests/abi-corpus does not match what the layout engine says".to_owned()],
    })
}

/// Regenerates the signature corpus, or checks it.
///
/// The same shape as [`abi_corpus`] and for the same reason: the generator is a build tool
/// because it reads `rucc-tuple` and `rucc-abi`, and `xtask` has no dependencies, so the task is
/// a name for a `cargo run`.
fn abi_signatures(args: &[String]) -> Result<()> {
    let mode = if args.iter().any(|a| a == "--check") { "--check" } else { "--write" };
    let status = Command::new("cargo")
        .args(["run", "-q", "-p", "rucc-targets", "--", "abi-signatures", mode])
        .current_dir(root())
        .status()
        .map_err(|e| Error::Io(format!("could not run cargo: {e}")))?;
    if status.success() {
        return Ok(());
    }
    Err(Error::Failed {
        task: "abi-signatures",
        problems: vec!["tests/abi-signatures does not match the grammar".to_owned()],
    })
}

/// Regenerates the recorded link lines, or checks them.
///
/// The third generator of this shape, and the reason is the same one: the lines come from
/// `rucc_sysroot::argv` and `xtask` has nothing on its dependency list, so the work is in
/// `rucc-targets` and this is the name people type.
///
/// What it guards is worth saying. A recorded line is the only check in this repository that a
/// loader path, an emulation name or the ordering of the start files is still what it was, and all
/// three are strings that nothing at link time verifies: a wrong loader produces a binary the kernel
/// refuses to start, and a wrong emulation produces one for the wrong machine. Neither shows up in
/// any other test here, because neither needs a compiler to be wrong.
fn link_lines(args: &[String]) -> Result<()> {
    let mode = if args.iter().any(|a| a == "--check") { "--check" } else { "--write" };
    let status = Command::new("cargo")
        .args(["run", "-q", "-p", "rucc-targets", "--", "link-lines", mode])
        .current_dir(root())
        .status()
        .map_err(|e| Error::Io(format!("could not run cargo: {e}")))?;
    if status.success() {
        return Ok(());
    }
    Err(Error::Failed {
        task: "link-lines",
        problems: vec!["tests/link-lines does not match the line rucc-sysroot builds".to_owned()],
    })
}

/// Regenerates `PROVENANCE`, or checks it.
///
/// The fourth generator of this shape. What it guards is the one file in a release that says what
/// that release will bring onto a machine: a URL and a hash per target where there is one, a licence
/// where section 13.4 says there will never be one, and the word that tells a target nobody has
/// published a tree for yet apart from a target nobody ever will. Those four answers come from three
/// tables, and a file that drifted from them would be a provenance record that reads like an answer
/// and is not one.
///
/// It is checked here rather than only before a release for the same reason the others are. The
/// release workflow runs `cargo xtask ci` on the tag before it packages anything, so a version bump
/// that forgot the file fails there, which is the cheapest place to find out.
fn provenance(args: &[String]) -> Result<()> {
    let mode = if args.iter().any(|a| a == "--check") { "--check" } else { "--write" };
    let status = Command::new("cargo")
        .args(["run", "-q", "-p", "rucc-targets", "--", "provenance", mode])
        .current_dir(root())
        .status()
        .map_err(|e| Error::Io(format!("could not run cargo: {e}")))?;
    if status.success() {
        return Ok(());
    }
    Err(Error::Failed {
        task: "provenance",
        problems: vec!["PROVENANCE does not match the tables it is generated from".to_owned()],
    })
}

/// What to print about the checks that did not run.
///
/// Always a line, including when there is nothing to report. `cargo xtask ci` saying nothing about
/// a check it skipped is how the command came to read as a complete account of the tree when it was
/// not one, so the case where everything ran says so out loud rather than staying quiet and letting
/// the absence of bad news stand in for good news.
fn accounted(skipped: &[(&str, String)]) -> String {
    if skipped.is_empty() {
        return "xtask: ci ran every check it has".to_owned();
    }
    skipped
        .iter()
        .map(|(what, why)| format!("xtask: ci did not run {what}: {why}"))
        .collect::<Vec<_>>()
        .join("\n")
}

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
    // First, because it is the one check about the tree rather than about the code in it, and
    // because what it catches stops the Windows job before that job can report anything.
    paths()?;
    layers()?;
    style()?;
    thresholds()?;
    malformed()?;
    interpose()?;
    version()?;
    targets(&["--check".to_owned()])?;
    abi_corpus(&["--check".to_owned()])?;
    abi_signatures(&["--check".to_owned()])?;
    link_lines(&["--check".to_owned()])?;
    provenance(&["--check".to_owned()])?;
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
    // Last because these are the long ones, and only when there is something to run the programs
    // on. They are x86-64 Linux programs, so an arm mac needs a container for them, and making the
    // standard pre push command fail on a machine with no docker would push people off the command
    // rather than onto docker. What it must not do is skip quietly, which is why the reason the
    // runner gave is printed rather than thrown away. They are in the order they take, cheapest
    // first, and a machine that cannot run one cannot run any of them and says so once per check
    // rather than once.
    let mut skipped: Vec<(&str, String)> = Vec::new();
    match runner::Runner::find("the shared library check") {
        Ok(_) => dso::dso()?,
        Err(why) => skipped.push(("dso", why.to_string())),
    }
    match runner::Runner::find("the unwind table check") {
        Ok(_) => unwind::unwind()?,
        Err(why) => skipped.push(("unwind", why.to_string())),
    }
    // The one check here that runs arithmetic this compiler lowered rather than a program it
    // compiled, and the reason it is not a test in the workspace is the reason the others are not
    // either: it needs a machine the back end emits code for. What it holds is the 128-bit width,
    // whose pass had only its own view of its own output behind it before this, at three
    // optimization levels, because the optimizer is what tamnd/rucc#1054 needed to show up at all.
    match runner::Runner::find("the wide arithmetic differential") {
        Ok(_) => wide::wide()?,
        Err(why) => skipped.push(("wide", why.to_string())),
    }
    match runner::Runner::find("the safety suite") {
        Ok(_) => safety::safety()?,
        Err(why) => skipped.push(("safety", why.to_string())),
    }
    // The same programs at -O2, which is the only thing here that runs an instrumented program
    // through the optimizer. Without it the whole back half of the compiler has no execution
    // coverage in the command people are told to run before pushing, and tamnd/rucc#818 is what
    // that costs: a divide by zero in loop splitting that this catches on a case already in the
    // suite, sat on main until somebody compiled the amalgamation by hand. It was left out on the
    // grounds that it takes too long, which measured is nineteen seconds against the suite's nine.
    match runner::Runner::find("the differential accounting") {
        Ok(_) => safety::accounting()?,
        Err(why) => skipped.push(("accounting", why.to_string())),
    }
    // Programs nobody wrote, which is the only check here whose input is different every time it
    // is asked for. A fixed seed, so the command is the same command twice in a row and a failure
    // on main is a failure anybody can reproduce. The number that finds things is whatever somebody
    // leaves running with `--count`, and what this default is for is a regression obvious enough to
    // show up in two dozen programs.
    match runner::Runner::find("the elimination fuzzer") {
        Ok(_) => fuzz::fuzz(&[])?,
        Err(why) => skipped.push(("fuzz", why.to_string())),
    }
    println!("{}", accounted(&skipped));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{accounted, bare_integer, compared_literals};

    #[test]
    fn a_run_that_checked_everything_says_so() {
        // The point of the line. Silence here is what let the command read as complete.
        assert_eq!(accounted(&[]), "xtask: ci ran every check it has");
    }

    #[test]
    fn a_check_that_did_not_run_is_named_along_with_why() {
        let line = accounted(&[("safety", "docker is not answering".to_owned())]);
        assert_eq!(line, "xtask: ci did not run safety: docker is not answering");
    }

    #[test]
    fn each_check_that_did_not_run_gets_its_own_line() {
        let line =
            accounted(&[("safety", "no runner".to_owned()), ("accounting", "too slow".to_owned())]);
        assert_eq!(line.lines().count(), 2);
        assert!(line.contains("did not run safety"));
        assert!(line.contains("did not run accounting"));
    }

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
