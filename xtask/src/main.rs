//! Build automation.
//!
//! The rule from `spec/18-package-layout.md` section 18.7 is that `cargo build` works with
//! no configuration and no external tools, and everything else is an `xtask`. There is no
//! `configure`, no CMake and no Python in the build, because build system complexity accretes
//! silently and the "clone and build" claim is worth protecting.
//!
//! This crate has no dependencies on purpose. It runs before anything else in CI, including
//! `cargo deny`, so it should not be able to fail because of somebody else's release.

mod bench;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::{fmt, fs, io};

const USAGE: &str = "\
usage: cargo xtask <task>

tasks:
  layers      check the crate dependency graph against xtask/layers.toml
  style       check documentation and specification prose against the house rules
  version     check that every version number in the tree agrees with the workspace's
  bench       time the throughput floor workload against the reference compiler
  bless       rewrite the expectations in tests/golden from what the compiler produces now
  ci          run everything the per-commit CI job runs, in the same order
  help        print this message
";

fn main() -> ExitCode {
    let task = std::env::args().nth(1);
    let result = match task.as_deref() {
        Some("layers") => layers(),
        Some("style") => style(),
        Some("version") => version(),
        Some("bench") => bench::bench(&std::env::args().skip(2).collect::<Vec<_>>()),
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
    deps: Vec<String>,
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
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            // Dev and build dependencies count too. A dev-dependency that inverts the stack
            // still makes the two crates impossible to build separately.
            in_deps = line.contains("dependencies]");
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
                    deps.push(dep.to_owned());
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
            if outside.contains(dep) {
                problems.push(format!(
                    "{} depends on {}, which is outside the layer stack and must not be \
                     linked into the compiler",
                    c.name, dep
                ));
                continue;
            }
            let Some(&dep_rank) = ranks.get(dep) else {
                problems.push(format!("{} depends on {}, which has no rank", c.name, dep));
                continue;
            };
            if dep_rank >= rank {
                problems.push(format!(
                    "{} (rank {rank}) depends on {dep} (rank {dep_rank}); a crate may depend \
                     only on strictly lower ranks. See {}",
                    c.name,
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
