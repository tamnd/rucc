//! The `rucc` binary.
//!
//! Deliberately empty. Everything lives in `rucc-driver` so that the whole driver, including
//! argument parsing and the exit code, is reachable from a test without spawning a process.

fn main() -> std::process::ExitCode {
    let mut args = std::env::args();
    let program = args.next().unwrap_or_default();
    let args: Vec<String> = args.collect();
    std::process::ExitCode::from(u8::try_from(rucc_driver::run_as(&program, &args)).unwrap_or(1))
}
