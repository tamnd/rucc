//! The corpus harness.
//!
//! Design: `spec/optimizer/42-measurement.md` section 42.3, and the last open item of M4.0.
//!
//! The corpus itself is a separate repository, tamnd/rucc-corpus. It generates C programs whose
//! answers it already knows, compiles them with every compiler it was given, runs them, compares
//! what they printed against the answer the generator wrote down, and reports sizes and times
//! against a reference compiler. None of that belongs in this tree. What belongs here is the
//! pin and the two lines of setup that stand between a clean checkout and a corpus run.
//!
//! The pin is the point. Section 42.3 asks for the corpus to be checked in as URLs and commit
//! hashes rather than as source, because a corpus that drifts makes historical numbers
//! meaningless. `xtask/corpus.toml` holds the URL and the commit, this task checks that commit
//! out, and two rucc commits measured this way measured the same programs. Moving the pin is a
//! commit of its own, which is where the discontinuity in the numbers is written down.
//!
//! The reference compiler is GCC 16 when there is one. The corpus does not need it to decide
//! whether rucc is correct, because the oracle is the generator and not the other compiler, but
//! without it every size and speed ratio in the report is rucc against itself. On a machine
//! with no GCC 16 the run still happens and still checks every answer, and the report says what
//! it is missing.

use std::path::Path;
use std::process::Command;

use crate::{Error, Result, root};

/// Runs the corpus against the compiler this tree builds.
pub(crate) fn corpus(args: &[String]) -> Result<()> {
    let mut rev: Option<String> = None;
    let mut gcc = "gcc-16".to_owned();
    let mut build = true;
    let mut extra: &[String] = &[];
    let mut at = 0;
    while at < args.len() {
        match args[at].as_str() {
            "--rev" => {
                let value = args.get(at + 1).ok_or_else(|| usage("--rev wants a commit"))?;
                rev = Some(value.clone());
                at += 2;
            }
            "--gcc" => {
                let value = args.get(at + 1).ok_or_else(|| usage("--gcc wants a program"))?;
                gcc = value.clone();
                at += 2;
            }
            "--no-build" => {
                build = false;
                at += 1;
            }
            "--" => {
                extra = &args[at + 1..];
                break;
            }
            other => return Err(usage(&format!("`{other}` is not an option this task has"))),
        }
    }

    let mut pin = read_pin(&std::fs::read_to_string(root().join("xtask/corpus.toml"))?)?;
    if let Some(rev) = rev {
        // For trying a corpus commit before deciding to pin it. The run says which commit it
        // used either way, so a number posted from one of these is still traceable.
        println!("xtask: using {rev} instead of the pinned commit");
        pin.rev = rev;
    }
    let dir = root().join("target/corpus");
    checkout(&pin, &dir)?;

    let rucc = root().join("target/release/rucc");
    if build {
        println!("xtask: building rucc, which is what the corpus is about to run");
        cargo(&root(), &["build", "--release", "-p", "rucc"])?;
    }
    if !rucc.exists() {
        return Err(Error::Io(format!(
            "{} does not exist, so there is nothing for the corpus to test",
            rucc.display()
        )));
    }

    let mut run: Vec<String> = ["run", "--release", "-p", "rucc-corpus", "--", "run"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    run.push("--toolchain".to_owned());
    run.push(format!("rucc={}", rucc.display()));
    if runnable(&gcc) {
        run.push("--toolchain".to_owned());
        run.push(format!("gcc-16={gcc}"));
        run.push("--reference".to_owned());
        run.push("gcc-16".to_owned());
    } else {
        // Correctness is still checked, because the generator knows the answers. What is lost
        // is every ratio in the report, and a ratio of one against yourself is worse than no
        // ratio at all if nobody says so out loud.
        println!("xtask: no {gcc} on this machine, so the sizes and times have nothing to");
        println!("xtask: compare against and only the answers are checked");
        run.push("--reference".to_owned());
        run.push("rucc".to_owned());
    }
    run.extend(extra.iter().cloned());

    println!("xtask: corpus at {}", &pin.rev[..12.min(pin.rev.len())]);
    cargo(&dir, &run.iter().map(String::as_str).collect::<Vec<_>>())?;
    println!(
        "xtask: the report is under {} unless --out said otherwise",
        dir.join("reports").display()
    );
    Ok(())
}

/// Where the corpus is and which commit of it counts.
struct Pin {
    repo: String,
    rev: String,
}

/// Reads `xtask/corpus.toml`.
///
/// The same hand-rolled reading as the layer table, and for the same reason: xtask has no
/// dependencies, so that it cannot fail to build because of somebody else's release.
fn read_pin(text: &str) -> Result<Pin> {
    let mut repo = String::new();
    let mut rev = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        let value = value.trim().trim_matches('"').to_owned();
        match key.trim() {
            "repo" => repo = value,
            "rev" => rev = value,
            _ => {}
        }
    }
    if repo.is_empty() || rev.is_empty() {
        return Err(Error::Io("corpus.toml needs a repo and a rev".to_owned()));
    }
    if rev.len() != 40 || !rev.bytes().all(|b| b.is_ascii_hexdigit()) {
        // A branch name is not a pin. It moves, and a run from six months ago that says it used
        // `main` says nothing at all.
        return Err(Error::Io(format!("corpus.toml: `{rev}` is not a full commit hash")));
    }
    Ok(Pin { repo, rev })
}

/// Puts the checkout at the pinned commit, cloning it first if this is the first run.
fn checkout(pin: &Pin, dir: &Path) -> Result<()> {
    if !dir.join(".git").is_dir() {
        if dir.exists() {
            return Err(Error::Io(format!(
                "{} exists and is not a checkout, move it out of the way",
                dir.display()
            )));
        }
        println!("xtask: cloning {} into {}", pin.repo, dir.display());
        let parent = dir.parent().expect("the checkout is under target");
        std::fs::create_dir_all(parent)?;
        git(parent, &["clone", "--quiet", &pin.repo, &dir.display().to_string()])?;
    }
    if git_says(dir, &["rev-parse", "HEAD"])? == pin.rev {
        return Ok(());
    }
    if !git_says(dir, &["status", "--porcelain"])?.is_empty() {
        // Somebody is working in there. Checking out over it would throw that away, and this is
        // a measurement task, so the right answer is to stop and let them decide.
        return Err(Error::Io(format!(
            "{} has changes of its own and is not at the pinned commit, sort that out first",
            dir.display()
        )));
    }
    if git(dir, &["cat-file", "-e", &format!("{}^{{commit}}", pin.rev)]).is_err() {
        println!("xtask: fetching, the pinned commit is not in the checkout yet");
        git(dir, &["fetch", "--quiet", "origin"])?;
    }
    git(dir, &["checkout", "--quiet", "--detach", &pin.rev])
}

/// Whether a program is there and will answer.
fn runnable(program: &str) -> bool {
    Command::new(program).arg("--version").output().is_ok_and(|out| out.status.success())
}

/// Runs git in a directory, and fails with what git said.
fn git(dir: &Path, args: &[&str]) -> Result<()> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| Error::Io(format!("could not run git: {e}")))?;
    if out.status.success() {
        return Ok(());
    }
    Err(Error::Io(format!(
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr).trim()
    )))
}

/// Runs git in a directory and hands back what it printed.
fn git_says(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| Error::Io(format!("could not run git: {e}")))?;
    if !out.status.success() {
        return Err(Error::Io(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// Runs cargo in a directory, with its output left where the person watching can see it.
fn cargo(dir: &Path, args: &[&str]) -> Result<()> {
    let status = Command::new("cargo")
        .args(args)
        .current_dir(dir)
        .status()
        .map_err(|e| Error::Io(format!("could not run cargo: {e}")))?;
    if status.success() {
        return Ok(());
    }
    Err(Error::Io(format!("cargo {} failed", args.join(" "))))
}

/// A mistake in how the task was asked for, with the shape of the answer attached.
fn usage(why: &str) -> Error {
    Error::Io(format!(
        "{why}\n\
         \n\
         usage: cargo xtask corpus [--rev <commit>] [--gcc <program>] [--no-build]\n\
         \x20                         [-- <arguments for rucc-corpus run>]\n\
         \n\
         Checks out the corpus commit pinned in xtask/corpus.toml, builds rucc in release, and\n\
         runs the whole corpus against it with GCC 16 as the reference. The arguments after --\n\
         go to the corpus runner as they are, which is where --facet, --level, --limit and\n\
         --jobs live:\n\
         \n\
         \x20 cargo xtask corpus -- --facet loops.reduction --level O2\n\
         \n\
         --rev runs a corpus commit other than the pinned one, for deciding whether to move the\n\
         pin. --no-build uses the release binary that is already there."
    ))
}

#[cfg(test)]
mod tests {
    use super::read_pin;

    #[test]
    fn the_pin_in_the_tree_is_a_url_and_a_full_commit() {
        let text = std::fs::read_to_string(crate::root().join("xtask/corpus.toml"))
            .expect("the manifest is checked in");
        let pin = read_pin(&text).expect("the manifest parses");
        assert!(pin.repo.contains("rucc-corpus"), "{}", pin.repo);
        assert_eq!(pin.rev.len(), 40);
    }

    #[test]
    fn a_branch_name_is_refused_because_a_branch_moves() {
        let text = "[corpus]\nrepo = \"https://example.invalid/c.git\"\nrev = \"main\"\n";
        let Err(error) = read_pin(text) else { panic!("a branch name is not a pin") };
        assert!(format!("{error}").contains("is not a full commit hash"), "{error}");
    }

    #[test]
    fn a_manifest_missing_half_of_what_it_needs_says_so() {
        let text = "[corpus]\nrepo = \"https://example.invalid/c.git\"\n";
        let Err(error) = read_pin(text) else { panic!("there is no rev to pin to") };
        assert!(format!("{error}").contains("needs a repo and a rev"), "{error}");
    }
}
