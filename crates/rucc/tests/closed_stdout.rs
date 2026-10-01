//! A reader that stops early is not a failure of the compiler. kbuild reads the first line of
//! `$(CC) --version` through `head -n 1`, which closes the pipe while the rest is being written.

use std::io::Read as _;
use std::process::{Command, Stdio};

#[test]
fn help_into_a_closed_pipe_exits_quietly() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rucc"))
        .arg("--help")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let mut stderr = String::new();
    child.stderr.take().unwrap().read_to_string(&mut stderr).unwrap();
    let status = child.wait().unwrap();
    assert!(!stderr.contains("panicked"), "{stderr}");
    assert_eq!(status.code(), Some(0), "{stderr}");
}
