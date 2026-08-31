//! Build automation.
//!
//! The rule from `spec/18-package-layout.md` section 18.7 is that `cargo build` works with
//! no configuration and no external tools, and everything else is an `xtask`. There is no
//! `configure`, no CMake and no Python in the build, because build system complexity accretes
//! silently and the "clone and build" claim is worth protecting.
//!
//! This crate has no dependencies on purpose. It runs before anything else in CI, including
//! `cargo deny`, so it should not be able to fail because of somebody else's release.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::{fmt, fs, io};

const USAGE: &str = "\
usage: cargo xtask <task>

tasks:
  layers      check the crate dependency graph against xtask/layers.toml
  style       check documentation and specification prose against the house rules
  ci          run everything the per-commit CI job runs, in the same order
  help        print this message
";

fn main() -> ExitCode {
    let task = std::env::args().nth(1);
    let result = match task.as_deref() {
        Some("layers") => layers(),
        Some("style") => style(),
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
