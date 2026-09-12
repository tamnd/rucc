//! Generating `PROVENANCE` from the target table, the pinned artifacts and the licence walls.
//!
//! Design: `spec/cross-compile/13-distribution.md` section 13.5, whose last line asks for the
//! provenance of every target to ship beside the binary so it can be audited without running the
//! compiler.
//!
//! The text is `rucc_sysroot::distribution::render` and what is here is the file and the two modes,
//! which is the shape `docs`, `abi-corpus` and `link-lines` already have. A generated file that is
//! committed and checked is the only form that works for this one: the release builds a binary per
//! host, two of those hosts cannot run what they built, and a file written by running the compiler
//! would therefore be written on three machines and missing on two.
//!
//! The release version is in the file, so a version bump regenerates it. That is deliberate rather
//! than tolerated. A provenance record that did not name the release it describes would be a file
//! somebody extracted from one archive and read beside another, and the check below is what makes
//! the bump and the file move together.

use std::path::Path;
use std::process::ExitCode;

/// Write the file or check the one on disk, which is what the two flags are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Write it.
    Write,
    /// Check it, and fail saying which command writes it.
    Check,
}

/// The file's name, at the top of the repository beside the readme and the licence.
///
/// Uppercase and without an extension because of where it ends up: `.github/package.sh` copies it
/// into the archive next to `README.md`, `LICENSE-APACHE` and `CHANGELOG.md`, and somebody who has
/// unpacked a release and is looking for what is in it reads the names in that column.
const NAME: &str = "PROVENANCE";

/// Write the file, or check that the one on disk matches what the tables say.
pub(crate) fn run(root: &Path, mode: Mode) -> ExitCode {
    let path = root.join(NAME);
    let wanted = rucc_sysroot::distribution::render(env!("CARGO_PKG_VERSION"));

    if mode == Mode::Check {
        let found = std::fs::read_to_string(&path).unwrap_or_default();
        if found == wanted {
            println!("provenance: {NAME} is up to date");
            return ExitCode::SUCCESS;
        }
        println!("{NAME} is out of date, run `cargo xtask provenance`");
        return ExitCode::FAILURE;
    }

    match std::fs::write(&path, wanted) {
        Ok(()) => {
            println!("provenance: wrote {NAME}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}
