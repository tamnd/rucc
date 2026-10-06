//! What the driver asks of the machine it runs on.
//!
//! Design: #2862, the first step of the WebAssembly plan. rucc builds for `wasm32-wasip1` and
//! `wasm32-wasip2` with no source change, and the compiler crates run there as they are. Four
//! things in the standard library do not: `std::env::temp_dir` and `std::process::id` panic on
//! WASI, and a process or a thread cannot start. This module is the one place that knows that.
//! The other modules ask it, and no compiler crate has a `cfg` for the host.
//!
//! Threads need nothing here. On WASI `available_parallelism` answers one, so the scheduler runs
//! the jobs in order.
//!
//! The driver also writes to standard output and standard error through [`stdout`] and [`stderr`]
//! here, and not through `std::io`. The reactor build of rucc (#2867) runs the driver more than
//! once in one instance and gives the host what each run wrote, and [`capture`] is how it gets that.

use std::cell::RefCell;
use std::io::{self, StderrLock, StdoutLock, Write};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

/// True when rucc itself is a WebAssembly module.
pub const WASM: bool = cfg!(target_family = "wasm");

/// The words that start every refusal to run another program on a wasm host.
pub const NO_PROCESSES: &str = "rucc is running as WebAssembly and cannot start another program";

/// Where temporary files go.
///
/// On a native host this is `std::env::temp_dir`. On a wasm host it is `TMPDIR` when that is set
/// and not empty, and the working directory when it is not. WASI has no temporary directory of its
/// own, and the working directory is the one directory that an engine gives to a module most often.
#[must_use]
pub fn temp_dir() -> PathBuf {
    if !WASM {
        return std::env::temp_dir();
    }
    match std::env::var("TMPDIR") {
        Ok(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => PathBuf::from("."),
    }
}

/// A number for this process that no other process running now is likely to have.
///
/// On a native host this is the process id. On a wasm host there is no process id, so it is made
/// from the clock the first time it is asked for, and the same number comes back after that. Two
/// modules that start in the same nanosecond get the same number. Every name made from it is also
/// written beside its final name and renamed, so that case is a lost write and not a corrupt file.
#[must_use]
pub fn id() -> u32 {
    if !WASM {
        return std::process::id();
    }
    static ID: OnceLock<u32> = OnceLock::new();
    *ID.get_or_init(|| {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
        // The low bits of the nanoseconds change the most between two starts.
        #[allow(clippy::cast_possible_truncation)]
        let id = (now.as_nanos() as u32) ^ (now.as_secs() as u32).rotate_left(16);
        id
    })
}

/// Why this host cannot start `program`, or nothing when it can.
///
/// The caller puts the answer into its own error, so that the refusal names the step that needed
/// the program.
#[must_use]
pub fn cannot_start(program: &str) -> Option<String> {
    WASM.then(|| format!("{NO_PROCESSES}, so it cannot run `{program}`"))
}

thread_local! {
    /// What this thread wrote to standard output and to standard error while [`capture`] runs.
    static CAPTURED: RefCell<Option<Captured>> = const { RefCell::new(None) };
}

/// What a run of the driver wrote, from [`capture`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Captured {
    /// What went to standard output.
    pub stdout: Vec<u8>,
    /// What went to standard error.
    pub stderr: Vec<u8>,
}

/// Standard output or standard error, locked, or the buffer of a [`capture`] in their place.
#[derive(Debug)]
pub enum Stream {
    Out(StdoutLock<'static>),
    Err(StderrLock<'static>),
    /// The buffer of a capture. True for standard error.
    Captured(bool),
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Stream::Out(out) => out.write(buf),
            Stream::Err(err) => err.write(buf),
            Stream::Captured(err) => {
                CAPTURED.with_borrow_mut(|captured| {
                    if let Some(captured) = captured {
                        let to = if *err { &mut captured.stderr } else { &mut captured.stdout };
                        to.extend_from_slice(buf);
                    }
                });
                Ok(buf.len())
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Stream::Out(out) => out.flush(),
            Stream::Err(err) => err.flush(),
            Stream::Captured(_) => Ok(()),
        }
    }
}

/// True while [`capture`] runs on this thread.
fn capturing() -> bool {
    CAPTURED.with_borrow(Option::is_some)
}

/// Standard output, where the driver writes what a run produces when no file is named.
#[must_use]
pub fn stdout() -> Stream {
    if capturing() { Stream::Captured(false) } else { Stream::Out(io::stdout().lock()) }
}

/// Standard error, where the driver writes its diagnostics and its notes.
#[must_use]
pub fn stderr() -> Stream {
    if capturing() { Stream::Captured(true) } else { Stream::Err(io::stderr().lock()) }
}

/// Runs `f` and gives what it wrote through [`stdout`] and [`stderr`] on this thread, which then
/// goes nowhere else.
///
/// Only this thread is captured. The driver writes its messages on the thread that called it, also
/// when `-j` compiles on other threads, and a wasm host has one thread. A panic in `f` ends the
/// capture as well.
pub fn capture<T>(f: impl FnOnce() -> T) -> (T, Captured) {
    /// Takes the capture back when `f` returns or unwinds.
    struct Done;
    impl Drop for Done {
        fn drop(&mut self) {
            CAPTURED.with_borrow_mut(Option::take);
        }
    }
    CAPTURED.with_borrow_mut(|captured| *captured = Some(Captured::default()));
    let done = Done;
    let value = f();
    let captured = CAPTURED.with_borrow_mut(Option::take).unwrap_or_default();
    drop(done);
    (value, captured)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_capture_takes_what_this_thread_writes_and_ends() {
        let ((), captured) = capture(|| {
            let _ = writeln!(stdout(), "out");
            let _ = write!(stderr(), "err");
        });
        assert_eq!(captured, Captured { stdout: b"out\n".to_vec(), stderr: b"err".to_vec() });
        assert!(!capturing());
        assert!(matches!(stderr(), Stream::Err(_)));
    }

    #[test]
    fn a_native_host_answers_what_the_standard_library_answers() {
        assert_eq!(temp_dir(), std::env::temp_dir());
        assert_eq!(id(), std::process::id());
        assert_eq!(cannot_start("ld"), None);
    }
}
