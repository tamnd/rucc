//! The pcc driver: rucc with the command line of `pcc`, the compiler of APE on Plan 9.
//!
//! APE is the ANSI and POSIX environment of Plan 9. Its mkfiles set `CC=pcc`, and
//! `sys/src/cmd/pcc.c` runs `/bin/cpp` with `-D__STDC__=1` and `-N` and pipes the result into
//! `6c`, `7c` or `8c`. `-N` turns off the standard include directories, and pcc adds
//! `/$objtype/include/ape` and `/sys/include/ape` after the `-I` directories. This module reads
//! the command line of pcc and makes a command line of the kencc driver, with `-p` for the ANSI
//! C preprocessor. [`crate::kencc::plan`] then makes the rucc compiles.
//!
//! The driver is used when the program name is `pcc`, or when the first argument is
//! `--driver=pcc`. As in the kencc driver, options that start with `--` can come first, and
//! they go to rucc unchanged.
//!
//! The rules that this module copies from pcc:
//!
//! - The machine is the value of `$objtype`, unless a `--target=` option names one.
//! - `-w`, `-B`, `-F`, `-N`, `-S`, `-T` and `-V` go to the compiler.
//! - `-D`, `-I` and `-U` go to the preprocessor. They take a value: the rest of the argument, or
//!   else the next argument.
//! - `-o` names the object, and `-c` compiles with no link.
//! - A file that ends in `.c` is compiled. pcc gives other files to the loader.
//!
//! rucc does not link Plan 9 programs, so pcc in rucc needs `-c`. `-E`, `-P`, `-a` and `-l` are
//! refused, and `-v`, `-p`, `-s` and `-+` do nothing. pcc prints a note for a letter that it
//! does not know and goes on, and so does this driver.

use crate::kencc::{self, Error, Machine, Plan};

/// True when the program name `program` selects the pcc driver.
#[must_use]
pub fn is_pcc(program: &str) -> bool {
    let name = program.rsplit(['/', '\\']).next().unwrap_or(program);
    name.strip_suffix(".exe").unwrap_or(name) == "pcc"
}

/// The machine of the value of `$objtype`.
fn from_objtype(objtype: &str) -> Option<Machine> {
    match objtype {
        "amd64" => Some(Machine::Amd64),
        "arm64" => Some(Machine::Arm64),
        "386" => Some(Machine::I386),
        _ => None,
    }
}

/// The name of the include directory for `machine` under `/`.
fn objtype(machine: Machine) -> &'static str {
    match machine {
        Machine::Amd64 => "amd64",
        Machine::Arm64 => "arm64",
        Machine::I386 => "386",
    }
}

/// Make the plan for the pcc command line `args`, with `env` to read the environment.
pub fn plan(args: &[String], env: &dyn Fn(&str) -> Option<String>) -> Result<Plan, Error> {
    let mut rucc = Vec::new();
    let mut i = 0;
    while let Some(arg) = args.get(i).filter(|a| a.len() > 2 && a.starts_with("--")) {
        rucc.push(arg.clone());
        i += 1;
    }
    let target = rucc.iter().rev().find_map(|a| a.strip_prefix("--target=")).map(str::to_owned);
    let machine = match &target {
        Some(target) => Machine::from_target(target).ok_or_else(|| {
            Error(format!("pcc compiles for amd64, arm64 and 386 only, not {target}"))
        })?,
        None => {
            let value =
                env("objtype").ok_or_else(|| Error("pcc: no $objtype in environment".into()))?;
            from_objtype(&value).ok_or_else(|| Error("pcc: unknown $objtype".into()))?
        }
    };
    let mut letters = String::new();
    let mut cpp = Vec::new();
    let mut undefs = Vec::new();
    let mut out = None;
    let mut compile_only = false;
    let mut files = Vec::new();
    let mut ignored = Vec::new();
    while let Some(arg) = args.get(i) {
        i += 1;
        if !arg.starts_with('-') || arg == "-" {
            match arg.rsplit_once('.') {
                Some((_, "c")) => files.push(arg.clone()),
                _ if compile_only => {}
                _ => files.push(arg.clone()),
            }
            continue;
        }
        for (at, c) in arg[1..].char_indices() {
            match c {
                'c' => compile_only = true,
                'w' | 'B' | 'F' | 'N' | 'S' | 'T' | 'V' => letters.push(c),
                'v' | 'p' | '+' => {}
                'E' | 'P' | 'a' => {
                    return Err(Error(format!("pcc: rucc does not take -{c} yet")));
                }
                'o' | 'D' | 'I' | 'U' | 's' | 'l' => {
                    let rest = &arg[1 + at + c.len_utf8()..];
                    let value = if rest.is_empty() {
                        let next = args.get(i).cloned();
                        if next.is_some() {
                            i += 1;
                        }
                        next
                    } else {
                        Some(rest.to_owned())
                    };
                    match (c, value) {
                        ('o', None) => return Err(Error("pcc: no -o argument".into())),
                        ('o', value) => out = value,
                        ('D', Some(v)) => cpp.push(format!("-D{v}")),
                        ('I', Some(v)) => cpp.push(format!("-I{v}")),
                        ('U', Some(v)) => undefs.push(format!("-U{v}")),
                        ('l', _) => {
                            return Err(Error("pcc: rucc does not link, so -l is refused".into()));
                        }
                        _ => {}
                    }
                    break;
                }
                c => ignored.push(format!("pcc: flag -{c} ignored")),
            }
        }
    }
    if files.is_empty() {
        return Err(Error("pcc: no files to compile or load".into()));
    }
    if !compile_only {
        return Err(Error("pcc: rucc does not link Plan 9 programs, so pcc needs -c".into()));
    }
    if let Some(file) = files.iter().find(|f| !f.ends_with(".c")) {
        return Err(Error(format!("pcc: {file} is not a C file")));
    }

    let mut line = rucc;
    if target.is_none() {
        line.push(format!("--target={}", machine.target()));
    }
    line.push(format!("-p{letters}"));
    line.push("-D__STDC__=1".to_owned());
    line.extend(cpp);
    if let Some(out) = out {
        line.push("-o".to_owned());
        line.push(out);
    }
    line.extend(files);
    // `-N` of cpp turns off the standard directories. pcc then adds the two of APE.
    let dirs = format!("/{}/include/ape /sys/include/ape", objtype(machine));
    let mut plan = kencc::plan("6c", &line, &|name| match name {
        "INCLUDE" => Some(dirs.clone()),
        _ => env(name),
    })?;
    plan.notes.splice(0..0, ignored);
    for job in &mut plan.jobs {
        let at = job.args.iter().position(|a| a == "-o").unwrap_or(job.args.len());
        job.args.splice(at..at, undefs.iter().cloned());
    }
    Ok(plan)
}

/// Run rucc as pcc. `args` is the command line after the program name. Return the exit code.
#[must_use]
pub fn run(args: &[String]) -> i32 {
    match plan(args, &|name| std::env::var(name).ok()) {
        Ok(plan) => kencc::execute(&plan),
        Err(e) => {
            use std::io::Write as _;
            let _ = writeln!(crate::host::stderr(), "{e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan_with(list: &[&str], vars: &[(&str, &str)]) -> Result<Plan, Error> {
        let args: Vec<String> = list.iter().map(|&a| a.to_owned()).collect();
        let vars: Vec<(String, String)> =
            vars.iter().map(|&(k, v)| (k.to_owned(), v.to_owned())).collect();
        plan(&args, &|name| vars.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()))
    }

    #[test]
    fn the_name_pcc_selects_the_driver() {
        assert!(is_pcc("pcc"));
        assert!(is_pcc("/bin/pcc"));
        assert!(!is_pcc("6c"));
    }

    #[test]
    fn pcc_runs_the_ansi_preprocessor_with_the_ape_headers() {
        let p =
            plan_with(&["-c", "-Fw", "-DX=1", "-I", "inc", "-UY", "x.c"], &[("objtype", "arm64")])
                .unwrap();
        assert_eq!(p.jobs.len(), 1);
        let a = &p.jobs[0].args;
        assert_eq!(a[0], "--target=aarch64-plan9");
        assert!(a.iter().any(|x| x == "-fno-kencc-preprocessor"));
        let pos = |s: &str| a.iter().position(|x| x == s).unwrap_or_else(|| panic!("{s}: {a:?}"));
        assert!(pos("-D__STDC__=1") < pos("-DX=1"));
        assert!(pos("-Iinc") < pos("-nostdinc"));
        assert!(pos("-nostdinc") < pos("-I/arm64/include/ape"));
        assert!(pos("-I/arm64/include/ape") < pos("-I/sys/include/ape"));
        assert!(pos("-UY") < pos("-o"));
        assert_eq!(a[pos("-o") + 1], "x.7");
        assert!(!a.iter().any(|x| x == "-w"));
    }

    #[test]
    fn pcc_needs_the_machine_and_c() {
        assert!(plan_with(&["-c", "x.c"], &[]).is_err());
        assert!(plan_with(&["x.c"], &[("objtype", "amd64")]).is_err());
        assert!(plan_with(&["-c", "x.c"], &[("objtype", "mips")]).is_err());
        let p = plan_with(&["--target=x86_64-plan9", "-c", "-o", "y.6", "x.c"], &[]).unwrap();
        assert_eq!(p.jobs[0].args.last().unwrap(), "x.c");
        assert!(p.jobs[0].args.iter().any(|x| x == "y.6"));
    }
}
