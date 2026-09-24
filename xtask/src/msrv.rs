//! Checks the workspace with the oldest Rust it says it builds with.
//!
//! The workspace says `rust-version = "1.85.0"` and the `msrv` job in CI holds it to that, but most
//! merges go in on a local run of `cargo xtask ci`, which used the newest compiler only. A feature
//! stabilised after the floor goes through that without a word, and a let chain has reached main
//! that way four times. See tamnd/rucc#1782. `chains` catches that one feature on any machine, and
//! this catches the rest wherever the old toolchain is installed.
//!
//! The version is read from the workspace manifest rather than written down here, so raising the
//! floor is one line in one file. The check builds into a target directory of its own, because a
//! second compiler cannot share the first one's artefacts and would otherwise rebuild the spine's
//! directory from scratch and then have the spine rebuild it again. That costs a second copy of the
//! dependencies on disk, the same trade the documentation makes.
//!
//! A machine without the toolchain skips the check and says how to get it, since that is a check
//! that did not run rather than one that failed.

use std::process::Command;

use crate::{Error, Result, root};

/// Checks every crate, test and feature with the toolchain the manifest names.
pub(crate) fn msrv() -> Result<()> {
    let version = floor()?;
    let toolchain = format!("+{version}");
    // Asked of rustup's list rather than by running `cargo +version`, because rustup installs a
    // toolchain it is asked to run and does not have, and a gate that downloads a compiler without
    // being asked is not something anybody running it expects.
    let listed = Command::new("rustup")
        .args(["toolchain", "list"])
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
        .unwrap_or_default();
    if !installed(&listed, &version) {
        return Err(Error::Io(format!(
            "Rust {version} is not installed, which is the version the workspace says it builds \
             with. `rustup toolchain install {version} --profile minimal` gets it"
        )));
    }
    let into = root().join("target").join("msrv");
    let status = Command::new("cargo")
        .args([toolchain.as_str(), "check", "--workspace", "--all-targets", "--all-features"])
        .env("CARGO_TARGET_DIR", &into)
        .current_dir(root())
        .status()
        .map_err(|e| Error::Io(format!("could not run cargo: {e}")))?;
    if !status.success() {
        return Err(Error::Failed {
            task: "msrv",
            problems: vec![format!(
                "the workspace does not build with Rust {version}, which it says it does. Either \
                 write it the way {version} takes, or raise `rust-version` on purpose"
            )],
        });
    }
    println!("msrv: the workspace builds with Rust {version}");
    Ok(())
}

/// Whether rustup's list of toolchains has one for this version, which it names with the host
/// after it, as in `1.85.0-x86_64-unknown-linux-gnu`.
fn installed(listed: &str, version: &str) -> bool {
    listed.lines().any(|line| {
        line.strip_prefix(version)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(['-', ' ']))
    })
}

/// The `rust-version` the workspace manifest gives.
fn floor() -> Result<String> {
    let manifest = std::fs::read_to_string(root().join("Cargo.toml"))?;
    version_in(&manifest)
        .ok_or_else(|| Error::Io("the workspace manifest has no rust-version".to_owned()))
}

/// The version on the first `rust-version` line of a manifest.
fn version_in(manifest: &str) -> Option<String> {
    let line = manifest.lines().find(|line| line.trim_start().starts_with("rust-version"))?;
    let (_, value) = line.split_once('=')?;
    let value = value.trim().trim_matches('"');
    (!value.is_empty()).then(|| value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_floor_is_read_off_the_rust_version_line() {
        let manifest = "[workspace.package]\nversion = \"0.11.6\"\nrust-version = \"1.85.0\"\n";
        assert_eq!(version_in(manifest).as_deref(), Some("1.85.0"));
    }

    #[test]
    fn a_manifest_with_no_floor_has_none() {
        assert_eq!(version_in("[workspace.package]\nversion = \"0.11.6\"\n"), None);
    }

    #[test]
    fn a_toolchain_is_found_by_its_version_and_not_by_a_longer_one() {
        let listed = "stable-x86_64-unknown-linux-gnu (default)\n1.85.0-x86_64-unknown-linux-gnu\n";
        assert!(installed(listed, "1.85.0"));
        assert!(!installed(listed, "1.85"));
        assert!(!installed(listed, "1.86.0"));
    }

    #[test]
    fn the_workspace_names_a_floor() {
        assert!(floor().is_ok_and(|version| version.starts_with("1.")));
    }
}
