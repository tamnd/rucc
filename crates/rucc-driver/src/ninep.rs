//! The file system of a build whose sysroot is on a 9P server.
//!
//! Design: tamnd/rucc#4153, and 07 of the rucc-plan9 plan.
//!
//! `--sysroot=9p:tcp!host!port` makes the sysroot the text `9p:tcp!host!port`. The include
//! directories of the target are under it as under any sysroot, for example
//! `9p:tcp!host!port/sys/include`, and so is a source file named that way on the command line. A
//! path under that root is read from the server with [`rucc_9p::Client`], and each other path is
//! read from the disk as before. A file that `#include "x.h"` finds next to a file from the
//! server is under the same root, so it comes from the server too.
//!
//! The client sends one request at a time, so one lock around it is enough. The cache of the
//! client keeps each file that it read, and the cache of [`OsFileSystem`] is not used for the
//! server.

use std::io::{self, Write};
use std::net::TcpStream;
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, PoisonError};

use rucc_9p::Client;
use rucc_diag::SourceBytes;
use rucc_session::{FileSystem, Options, path_key};

use crate::preprocess::OsFileSystem;

/// The disk, and the 9P server of the sysroot when there is one.
#[derive(Debug)]
pub struct Sources {
    os: OsFileSystem,
    remote: Option<Remote>,
}

/// A 9P server and the root that its paths are under.
#[derive(Debug)]
struct Remote {
    addr: String,
    root: PathBuf,
    client: Mutex<Client<TcpStream>>,
}

/// The user that the client attaches as: `$USER`, or `none`, the user that a Plan 9 server
/// gives to a client it does not know.
fn user() -> String {
    std::env::var("USER").ok().filter(|u| !u.is_empty()).unwrap_or_else(|| "none".to_owned())
}

impl Sources {
    /// The file system for `opts`. With a sysroot on a 9P server, this connects to it.
    ///
    /// # Errors
    ///
    /// The message to show when the server cannot be reached or refuses the attach.
    pub fn new(opts: &Options) -> Result<Sources, String> {
        let remote = match &opts.nine_p {
            None => None,
            Some(addr) => {
                let fail = |e: io::Error| {
                    format!("cannot read the sysroot from the 9P server {addr}: {e}")
                };
                let stream = rucc_9p::dial(addr).map_err(fail)?;
                let client = Client::connect(stream, &user(), "").map_err(fail)?;
                let root = PathBuf::from(format!("9p:{addr}"));
                Some(Remote { addr: addr.clone(), root, client: Mutex::new(client) })
            }
        };
        Ok(Sources { os: OsFileSystem::new(), remote })
    }

    /// The path on the server of `path`, when `path` is under the root of the server.
    fn remote_path(&self, path: &Path) -> Option<(&Remote, String)> {
        let remote = self.remote.as_ref()?;
        let rest = path.strip_prefix(&remote.root).ok()?;
        let mut names = Vec::new();
        for part in rest.components() {
            match part {
                Component::Normal(name) => names.push(name.to_str()?),
                Component::ParentDir => names.push(".."),
                _ => {}
            }
        }
        Some((remote, names.join("/")))
    }

    /// Under `-time`, write what the reads from the server cost, in the form of the other lines
    /// of `-time`.
    pub fn report(&self, stderr: &mut impl Write) {
        let Some(remote) = &self.remote else { return };
        let stats = remote.client.lock().unwrap_or_else(PoisonError::into_inner).stats();
        let _ = writeln!(
            stderr,
            "# 9p {} {:.2} {} round trips, {} bytes, {} from the cache",
            remote.addr,
            stats.wait.as_secs_f64(),
            stats.round_trips,
            stats.bytes,
            stats.cache_hits,
        );
    }
}

impl FileSystem for Sources {
    fn read(&self, path: &Path) -> io::Result<SourceBytes> {
        match self.remote_path(path) {
            Some((remote, name)) => {
                let mut client = remote.client.lock().unwrap_or_else(PoisonError::into_inner);
                client.read_file(&name).map(SourceBytes::new)
            }
            None => self.os.read(path),
        }
    }

    fn identity(&self, path: &Path) -> PathBuf {
        // The server has no links that the client can see, so the path with its `.` taken out
        // names the file.
        match self.remote_path(path) {
            Some(_) => path_key(path),
            None => self.os.identity(path),
        }
    }
}
