//! A command with the arguments of `6a`, for the comparison with the C program.
//!
//! Usage: `p9a 6 [-o out] [-Dname] [-Idir] file.s`. An absolute name is read under `$P9ROOT`, and
//! `$P9CWD` is the directory of the history.

use std::process::ExitCode;

use rucc_asm::plan9::syntax::assemble6;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let _machine = args.next();
    let (mut out, mut defines, mut includes, mut file) = (None, Vec::new(), Vec::new(), None);
    while let Some(a) = args.next() {
        if a == "-o" {
            out = args.next();
        } else if let Some(d) = a.strip_prefix("-D") {
            defines.push(if d.is_empty() {
                args.next().unwrap_or_default()
            } else {
                d.to_string()
            });
        } else if let Some(i) = a.strip_prefix("-I") {
            includes.push(if i.is_empty() {
                args.next().unwrap_or_default()
            } else {
                i.to_string()
            });
        } else {
            file = Some(a);
        }
    }
    let Some(file) = file else {
        eprintln!("usage: p9a 6 [-o out] file.s");
        return ExitCode::FAILURE;
    };
    let root = std::env::var("P9ROOT").unwrap_or_default();
    let cwd = std::env::var("P9CWD").unwrap_or_else(|_| "/".to_string());
    let read = |n: &str| {
        let p = if n.starts_with('/') { format!("{root}{n}") } else { n.to_string() };
        std::fs::read(p).ok()
    };
    match assemble6(&file, &cwd, &defines, &includes, &read) {
        Ok(o) => {
            let out = out.unwrap_or_else(|| "6.out".to_string());
            if std::fs::write(&out, o).is_err() {
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            print!("{e}");
            ExitCode::FAILURE
        }
    }
}
