//! The `rucc` binary.
//!
//! Deliberately empty. Everything lives in `rucc-driver` so that the whole driver, including
//! argument parsing and the exit code, is reachable from a test without spawning a process.

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::ExitCode::from(u8::try_from(rucc_driver::run(&args)).unwrap_or(1))
}
