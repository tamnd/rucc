//! How an x86-64 Linux program gets run from whatever machine you are sitting at.
//!
//! Three tasks need this and they all need the same thing. The only back end is x86-64 and the
//! only libc the tasks link against is a Linux one, so a developer on an arm mac cannot run what
//! the compiler produced, and CI can. Rather than each task deciding separately what to do about
//! that, they all ask here and get one of two answers: run it, or run it in a container.
//!
//! The alternative to a container is skipping, and `xtask/src/safety.rs` says why that is worse:
//! a suite that skips is a suite nobody notices has stopped running. So the failure here is a
//! sentence saying what to start, not a green tick.
//!
//! Everything a task wants run goes in one directory with a `run.sh` in it, and the directory is
//! mounted read only, which is what keeps a container from leaving files in the tree owned by
//! somebody else.

use std::path::Path;
use std::process::Command;

use crate::{Error, Result};

/// The machine the programs are built for, which is the only one there is a back end for.
pub(crate) const TRIPLE: &str = "x86_64-unknown-linux-gnu";

/// The image the programs run in on a machine that is not an x86-64 Linux one.
///
/// A compiler image rather than a bare distribution, because what the container has to do is
/// assemble and link, and pinning the major version keeps a program that starts failing from
/// being a question about which `gcc` the machine pulled this morning.
const IMAGE: &str = "gcc:13";

/// How the programs get run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Runner {
    /// Straight, because this machine is the machine they are compiled for.
    Here,
    /// In a container, because it is not.
    Container,
}

impl std::fmt::Display for Runner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Here => f.write_str("run here"),
            Self::Container => f.write_str("run in a container"),
        }
    }
}

impl Runner {
    /// Picks one, or says why there is not one.
    ///
    /// `what` names what wanted to run, so the sentence reads as the task's own rather than as
    /// this module's.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when this is not an x86-64 Linux machine and nothing here can start the image.
    pub(crate) fn find(what: &str) -> Result<Self> {
        let host = crate::host_triple()?;
        if host.starts_with("x86_64-") && host.contains("linux") {
            return Ok(Self::Here);
        }
        // Start something, rather than asking the daemon whether it is up. A daemon that answers
        // and an image that runs are two different facts, and the gap between them is where a task
        // that announced it was about to run a suite stops halfway through with docker's own
        // complaint instead. On a machine that has the image this costs a second, and on one that
        // does not it costs the pull the real run was going to do anyway.
        let out = Command::new("docker")
            .args(["run", "--rm", "--platform", "linux/amd64", IMAGE, "true"])
            .output();
        let said = match out {
            Ok(out) if out.status.success() => return Ok(Self::Container),
            Ok(out) => String::from_utf8_lossy(&out.stderr).trim().to_owned(),
            Err(e) => e.to_string(),
        };
        Err(Error::Io(format!(
            "{what} is {TRIPLE} programs and this machine is {host}, so it needs a container to \
             run them in and {IMAGE} did not start. Fix docker, or run this on an x86-64 Linux \
             machine. What it said: {said}"
        )))
    }

    /// Runs `run.sh` over the directory and hands back what it printed.
    ///
    /// `what` names what was being run, for the message when the script itself fails, which is a
    /// different thing from a program inside it failing. A program that exits non zero is a
    /// result the caller reads out of the output; a script that cannot start is an error.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the script could not be started or did not exit zero.
    pub(crate) fn run(&self, work: &Path, what: &str) -> Result<String> {
        let out = match self {
            Self::Here => Command::new("sh").arg("run.sh").current_dir(work).output(),
            Self::Container => Command::new("docker")
                .args(["run", "--rm", "--platform", "linux/amd64", "-v"])
                .arg(format!("{}:/w:ro", work.display()))
                .args(["-w", "/w", IMAGE, "sh", "run.sh"])
                .output(),
        }
        .map_err(|e| Error::Io(format!("could not run {what}: {e}")))?;
        if !out.status.success() {
            return Err(Error::Io(format!(
                "{what} did not run: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}
