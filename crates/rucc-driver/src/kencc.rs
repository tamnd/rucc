//! The kencc driver: rucc with the command line of the Plan 9 compilers `6c`, `7c` and `8c`.
//!
//! The 9front mkfiles call the compiler as `$CC` with kencc's options, such as `6c -FTVw x.c`.
//! rucc-plan9 puts rucc in that slot, and the command line comes through unchanged. This module
//! reads that command line the way `sys/src/cmd/cc/lex.c` reads it, and makes one rucc command
//! line in the GCC style for each source file. [`run`] then compiles each file with
//! [`crate::run`].
//!
//! The driver is used when the program name is `6c`, `7c` or `8c`, or when the first argument
//! is `--driver=kencc`. [`crate::pcc`] puts the command line of APE's `pcc` into this one. Options that start with `--`, such as `--target=` and `--sysroot=`, can
//! come before the kencc options. They go to rucc unchanged. kencc itself never sees such an
//! option, because its option reader stops at `--`.
//!
//! kencc reads options with `ARGBEGIN`. The options stop at the first argument that does not
//! start with `-`, at `-` alone, and after `--`. One argument can hold many option letters, as
//! in `-FTVw`. The letters `o`, `D` and `I` take a value: the rest of the argument, or else the
//! next argument. Every other letter is a debug flag, and kencc only counts it.
//!
//! The rules that this module copies from kencc:
//!
//! - The object of `dir/x.c` is `x.6` in the current directory, with the character of the
//!   machine: `6` for amd64, `7` for arm64 and `8` for 386. `-o` names the object of every file.
//! - With no file, kencc reads standard input, but only when `-o` names the object.
//! - Quoted includes look first in the directory of the source file, or in the current directory
//!   with `-.`, and then in the `-I` directories. Angle includes look in the `-I` directories.
//!   Then both look in `$INCLUDE`, or else in `/$objtype/include` and `/sys/include`.
//! - When no directory has the file, both open the name from the current directory.
//! - `$INCLUDE` and the value of `-I` can hold many directories, with a space between each.
//! - Warnings are printed only with `-w`.
//! - Without `-V`, a pointer to `void` is the same type as any other pointer when two
//!   declarations of one name are compared.
//! - `-p` runs the ANSI C preprocessor on the file, where `#` and `##` are operators. Without
//!   it the preprocessor of kencc reads the file.
//! - With more than one file, kencc prints the name of each file and a colon on standard error
//!   before it compiles the file.
//!
//! kencc has no optimization level. `R9_OPT` in the environment gives rucc options for every
//! compile, so a build can change them with no change to the mkfiles. The level is `-O2` when
//! `R9_OPT` does not give one, and `-N` makes it `-O0`.

use std::fmt;

/// The variable that gives rucc options to each compile in the kencc driver.
pub const OPTIONS_VARIABLE: &str = "R9_OPT";

/// A Plan 9 machine that rucc compiles for in the kencc driver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Machine {
    /// amd64, with `6c` and `.6` objects.
    Amd64,
    /// arm64, with `7c` and `.7` objects.
    Arm64,
    /// 386, with `8c` and `.8` objects.
    I386,
}

impl Machine {
    /// The machine of a compiler name, `6c`, `7c` or `8c`.
    #[must_use]
    pub fn from_compiler(name: &str) -> Option<Machine> {
        match name {
            "6c" => Some(Machine::Amd64),
            "7c" => Some(Machine::Arm64),
            "8c" => Some(Machine::I386),
            _ => None,
        }
    }

    /// The machine of a target such as `x86_64-plan9`.
    #[must_use]
    pub fn from_target(target: &str) -> Option<Machine> {
        let arch = target.split('-').next()?;
        match arch {
            "x86_64" | "amd64" => Some(Machine::Amd64),
            "aarch64" | "arm64" => Some(Machine::Arm64),
            "i386" | "i486" | "i586" | "i686" | "x86" => Some(Machine::I386),
            _ => None,
        }
    }

    /// The character of the machine, which kencc calls `thechar`.
    #[must_use]
    pub fn letter(self) -> char {
        match self {
            Machine::Amd64 => '6',
            Machine::Arm64 => '7',
            Machine::I386 => '8',
        }
    }

    /// The rucc target.
    #[must_use]
    pub fn target(self) -> &'static str {
        match self {
            Machine::Amd64 => "x86_64-plan9",
            Machine::Arm64 => "aarch64-plan9",
            Machine::I386 => "i386-plan9",
        }
    }
}

/// An error in a kencc command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One rucc compile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    /// The source file, or `None` for standard input.
    pub file: Option<String>,
    /// The rucc arguments, in the GCC style.
    pub args: Vec<String>,
}

/// The compiles of a kencc command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// The compiles, in the order of the files.
    pub jobs: Vec<Job>,
    /// True when kencc prints the name of each file before it compiles the file.
    pub announce: bool,
    /// Notes to print once on standard error, such as for an option that does nothing.
    pub notes: Vec<String>,
}

/// The options of a kencc command line.
#[derive(Debug)]
struct Options {
    /// The `--` options for rucc that came before the kencc options.
    rucc: Vec<String>,
    /// How many times each option letter was given. kencc calls this `debug`.
    count: [u32; 128],
    /// The value of `-o`.
    out: Option<String>,
    /// The values of `-D`, in order.
    defines: Vec<String>,
    /// The directories of `-I`, in order and with no copies.
    includes: Vec<String>,
    /// The letters that are not ASCII. kencc ignores them.
    other: Vec<char>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            rucc: Vec::new(),
            count: [0; 128],
            out: None,
            defines: Vec::new(),
            includes: Vec::new(),
            other: Vec::new(),
        }
    }
}

impl Options {
    fn has(&self, letter: u8) -> bool {
        self.count[usize::from(letter)] > 0
    }

    /// Add the directories of `value` to the include list, as kencc's `setinclude` does.
    fn include(&mut self, value: &str) {
        for dir in value.split(' ').filter(|d| !d.is_empty()) {
            if !self.includes.iter().any(|d| d == dir) {
                self.includes.push(dir.to_owned());
            }
        }
    }
}

/// True when the program name `program` selects the kencc driver.
#[must_use]
pub fn is_kencc(program: &str) -> bool {
    Machine::from_compiler(base_name(program)).is_some()
}

/// The program name without its directory and without a trailing `.exe`.
fn base_name(program: &str) -> &str {
    let name = program.rsplit(['/', '\\']).next().unwrap_or(program);
    name.strip_suffix(".exe").unwrap_or(name)
}

/// The object name for `file`, as kencc's `compile` makes it: the last element of the path with
/// a trailing `.c` taken off, then a dot and the character of the machine.
#[must_use]
pub fn object_name(file: &str, machine: Machine) -> String {
    let base = file.rsplit('/').next().unwrap_or(file);
    let stem = match base.rfind('.') {
        Some(dot) if &base[dot..] == ".c" => &base[..dot],
        _ => base,
    };
    format!("{stem}.{}", machine.letter())
}

/// Read the kencc options in `args`, and return them with the files after them.
fn read_options(args: &[String]) -> (Options, Vec<String>) {
    let mut o = Options::default();
    let mut i = 0;
    while let Some(arg) = args.get(i) {
        if arg.len() > 2 && arg.starts_with("--") {
            o.rucc.push(arg.clone());
            i += 1;
        } else {
            break;
        }
    }
    while let Some(arg) = args.get(i) {
        if !arg.starts_with('-') || arg == "-" {
            break;
        }
        i += 1;
        if arg == "--" {
            break;
        }
        for (at, c) in arg[1..].char_indices() {
            if !matches!(c, 'o' | 'D' | 'I') {
                match u8::try_from(c).ok().filter(u8::is_ascii) {
                    Some(b) => o.count[usize::from(b)] += 1,
                    None => o.other.push(c),
                }
                continue;
            }
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
                ('o', value) => o.out = value,
                ('D', Some(v)) => o.defines.push(v),
                ('I', Some(v)) => o.include(&v),
                _ => {}
            }
            break;
        }
    }
    (o, args[i..].to_vec())
}

/// The target that a `--target=` option in `rucc` names.
fn target_option(rucc: &[String]) -> Option<&str> {
    rucc.iter().rev().find_map(|a| a.strip_prefix("--target="))
}

/// Make the plan for the kencc command line `args`. `program` is the name that rucc was run
/// as, and `env` reads the environment.
pub fn plan(
    program: &str,
    args: &[String],
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<Plan, Error> {
    let (o, files) = read_options(args);
    let compiler = base_name(program);
    let machine = match target_option(&o.rucc) {
        Some(target) => Machine::from_target(target).ok_or_else(|| {
            Error(format!("the kencc driver compiles for amd64, arm64 and 386 only, not {target}"))
        })?,
        None => Machine::from_compiler(compiler).ok_or_else(|| {
            Error("the kencc driver needs a --target= or the name 6c, 7c or 8c".into())
        })?,
    };
    let name = format!("{}c", machine.letter());
    if o.has(b'l') {
        return Err(Error("can only use -l with vc".into()));
    }
    if files.is_empty() && o.out.is_none() {
        return Err(Error(format!("usage: {name} [-options] files")));
    }
    if o.has(b'a') || o.has(b'Z') {
        return Err(Error(format!(
            "{name} -a and -Z write acid and pickle declarations, which rucc does not write yet"
        )));
    }
    let mut notes = Vec::new();
    for (letter, what) in [(b'd', "print declarations"), (b'i', "print initializations")] {
        if o.has(letter) {
            notes.push(format!("{name}: -{} ({what}) does nothing in rucc", char::from(letter)));
        }
    }

    let mut common = Vec::new();
    if target_option(&o.rucc).is_none() {
        common.push(format!("--target={}", machine.target()));
    }
    common.extend(o.rucc.iter().cloned());
    common.push("-c".to_owned());
    common.push("-fkencc".to_owned());
    // `-p` runs the ANSI C preprocessor, `/bin/cpp`, on the file first.
    if o.has(b'p') {
        common.push("-fno-kencc-preprocessor".to_owned());
    }
    if !o.has(b'w') {
        common.push("-w".to_owned());
    }
    // `-V` compares `void *` with other pointers as ANSI C does.
    if o.has(b'V') {
        common.push("-fkencc-strict-void".to_owned());
    }
    for d in &o.defines {
        common.push(format!("-D{d}"));
    }
    if o.has(b'.') {
        common.push("-I.".to_owned());
        common.push("-I-".to_owned());
    }
    for dir in &o.includes {
        common.push(format!("-I{dir}"));
    }
    if let Some(list) = env("INCLUDE") {
        common.push("-nostdinc".to_owned());
        for dir in list.split(' ').filter(|d| !d.is_empty()) {
            if !o.includes.iter().any(|d| d == dir) {
                common.push(format!("-I{dir}"));
            }
        }
    }
    // When no directory has the file, `macinc` opens the name as it is, from the current
    // directory. libndb has `#include <ndbhf.h>` for a header that is only in its own directory.
    common.push("-idirafter".to_owned());
    common.push(".".to_owned());
    let extra: Vec<String> =
        env(OPTIONS_VARIABLE).unwrap_or_default().split_whitespace().map(str::to_owned).collect();
    if !extra.iter().any(|a| a.starts_with("-O")) {
        common.push("-O2".to_owned());
    }
    common.extend(extra);
    if o.has(b'N') {
        common.push("-O0".to_owned());
    }

    let sources: Vec<Option<String>> =
        if files.is_empty() { vec![None] } else { files.into_iter().map(Some).collect() };
    let mut jobs = Vec::new();
    for file in &sources {
        let out = match (&o.out, file) {
            (Some(out), _) => out.clone(),
            (None, Some(file)) => object_name(file, machine),
            (None, None) => unreachable!("standard input needs -o"),
        };
        let input = file.clone().unwrap_or_else(|| "-".to_owned());
        let mut args = common.clone();
        if file.is_none() {
            args.extend(["-x".to_owned(), "c".to_owned()]);
        }
        args.extend(["-o".to_owned(), out, input.clone()]);
        jobs.push(Job { file: file.clone(), args });
        if o.has(b'S') && file.is_some() {
            let mut args = common.clone();
            args.retain(|a| a != "-c");
            args.extend(["-S".to_owned(), "-o".to_owned(), "-".to_owned(), input]);
            jobs.push(Job { file: file.clone(), args });
        }
    }
    Ok(Plan { jobs, announce: sources.len() > 1, notes })
}

/// Run rucc as kencc. `program` is `argv[0]` and `args` is the rest. Return the exit code.
#[must_use]
pub fn run(program: &str, args: &[String]) -> i32 {
    use std::io::Write as _;
    let plan = match plan(program, args, &|name| std::env::var(name).ok()) {
        Ok(plan) => plan,
        Err(e) => {
            let _ = writeln!(crate::host::stderr(), "{e}");
            return 1;
        }
    };
    execute(&plan)
}

/// Run the compiles of `plan`, and return the exit code.
#[must_use]
pub fn execute(plan: &Plan) -> i32 {
    use std::io::Write as _;
    for note in &plan.notes {
        let _ = writeln!(crate::host::stderr(), "{note}");
    }
    let mut failed = false;
    let mut last: Option<&str> = None;
    for job in &plan.jobs {
        let file = job.file.as_deref();
        if plan.announce && file != last {
            let _ = writeln!(crate::host::stderr(), "{}:", file.unwrap_or("stdin"));
        }
        last = file;
        if crate::run(&job.args) != 0 {
            failed = true;
        }
    }
    i32::from(failed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|&a| a.to_owned()).collect()
    }

    fn plan_with(program: &str, list: &[&str], vars: &[(&str, &str)]) -> Result<Plan, Error> {
        let vars: Vec<(String, String)> =
            vars.iter().map(|&(k, v)| (k.to_owned(), v.to_owned())).collect();
        plan(program, &args(list), &|name| {
            vars.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone())
        })
    }

    fn one(program: &str, list: &[&str]) -> Vec<String> {
        let p = plan_with(program, list, &[]).unwrap();
        assert_eq!(p.jobs.len(), 1);
        p.jobs[0].args.clone()
    }

    #[test]
    fn the_names_of_the_compilers_select_the_driver() {
        assert!(is_kencc("6c"));
        assert!(is_kencc("/usr/local/bin/7c"));
        assert!(is_kencc("C:\\r9\\8c.exe"));
        assert!(!is_kencc("5c"));
        assert!(!is_kencc("cc"));
        assert!(!is_kencc("x86_64-plan9-rucc"));
    }

    #[test]
    fn the_object_is_named_as_kencc_names_it() {
        assert_eq!(object_name("x.c", Machine::Amd64), "x.6");
        assert_eq!(object_name("../port/proc.c", Machine::Arm64), "proc.7");
        assert_eq!(object_name("y.tab.c", Machine::I386), "y.tab.8");
        assert_eq!(object_name("a.cc", Machine::Amd64), "a.cc.6");
        assert_eq!(object_name("noext", Machine::Amd64), "noext.6");
    }

    #[test]
    fn a_plain_compile_is_quiet_and_optimized() {
        assert_eq!(
            one("6c", &["-FTVw", "acme.c"]),
            [
                "--target=x86_64-plan9",
                "-c",
                "-fkencc",
                "-fkencc-strict-void",
                "-idirafter",
                ".",
                "-O2",
                "-o",
                "acme.6",
                "acme.c"
            ]
        );
        assert_eq!(
            one("7c", &["../port/proc.c"]),
            [
                "--target=aarch64-plan9",
                "-c",
                "-fkencc",
                "-w",
                "-idirafter",
                ".",
                "-O2",
                "-o",
                "proc.7",
                "../port/proc.c"
            ]
        );
    }

    #[test]
    fn options_with_a_value_take_the_rest_or_the_next_argument() {
        let a = one("6c", &["-wDX=1", "-D", "Y", "-I../common", "-Ia b", "-o", "out.6", "x.c"]);
        assert_eq!(
            a,
            [
                "--target=x86_64-plan9",
                "-c",
                "-fkencc",
                "-DX=1",
                "-DY",
                "-I../common",
                "-Ia",
                "-Ib",
                "-idirafter",
                ".",
                "-O2",
                "-o",
                "out.6",
                "x.c"
            ]
        );
    }

    #[test]
    fn the_options_stop_at_the_first_file() {
        let p = plan_with("6c", &["x.c", "-w"], &[]).unwrap();
        assert_eq!(p.jobs.len(), 2);
        assert_eq!(p.jobs[1].file.as_deref(), Some("-w"));
        assert!(p.announce);
        let p = plan_with("6c", &["--", "-w"], &[]).unwrap();
        assert_eq!(p.jobs[0].file.as_deref(), Some("-w"));
        assert!(p.jobs[0].args.contains(&"-w".to_owned()));
    }

    #[test]
    fn several_files_give_one_object_each() {
        let p = plan_with("6c", &["-w", "a.c", "dir/b.c"], &[]).unwrap();
        let outs: Vec<&str> = p.jobs.iter().map(|j| j.args[j.args.len() - 2].as_str()).collect();
        assert_eq!(outs, ["a.6", "b.6"]);
        assert!(p.announce);
    }

    #[test]
    fn standard_input_needs_an_output_name() {
        assert_eq!(plan_with("6c", &["-w"], &[]).unwrap_err().0, "usage: 6c [-options] files");
        assert_eq!(
            one("6c", &["-o", "p.6"]),
            [
                "--target=x86_64-plan9",
                "-c",
                "-fkencc",
                "-w",
                "-idirafter",
                ".",
                "-O2",
                "-x",
                "c",
                "-o",
                "p.6",
                "-"
            ]
        );
    }

    #[test]
    fn dot_searches_the_current_directory_for_quoted_includes() {
        let a = one("6c", &["-.", "-Iinc", "sub/x.c"]);
        assert_eq!(a[3..7], ["-w", "-I.", "-I-", "-Iinc"]);
    }

    #[test]
    fn include_replaces_the_system_directories() {
        let p = plan_with(
            "6c",
            &["-w", "-I/sys/include", "x.c"],
            &[("INCLUDE", "/sys/include /amd64/include")],
        )
        .unwrap();
        assert_eq!(
            p.jobs[0].args[3..9],
            ["-I/sys/include", "-nostdinc", "-I/amd64/include", "-idirafter", ".", "-O2"]
        );
    }

    #[test]
    fn r9_opt_sets_the_options_and_n_turns_off_the_optimizer() {
        let a = plan_with("6c", &["-w", "x.c"], &[("R9_OPT", "-O1 -g")]).unwrap();
        assert_eq!(a.jobs[0].args[5..7], ["-O1", "-g"]);
        let a = plan_with("6c", &["-wN", "x.c"], &[("R9_OPT", "-O3")]).unwrap();
        assert_eq!(a.jobs[0].args[5..7], ["-O3", "-O0"]);
        let a = one("6c", &["-N", "x.c"]);
        assert_eq!(a[6..8], ["-O2", "-O0"]);
    }

    #[test]
    fn p_runs_the_ansi_preprocessor() {
        let a = one("6c", &["-FVpw", "x.c"]);
        assert_eq!(a[2..4], ["-fkencc", "-fno-kencc-preprocessor"]);
        assert!(!one("6c", &["-w", "x.c"]).iter().any(|a| a == "-fno-kencc-preprocessor"));
    }

    #[test]
    fn s_prints_the_assembly_and_still_writes_the_object() {
        let p = plan_with("6c", &["-Sw", "x.c"], &[]).unwrap();
        assert_eq!(p.jobs.len(), 2);
        assert_eq!(p.jobs[0].args.last().unwrap(), "x.c");
        assert_eq!(
            p.jobs[1].args,
            ["--target=x86_64-plan9", "-fkencc", "-idirafter", ".", "-O2", "-S", "-o", "-", "x.c"]
        );
        assert!(!p.announce);
    }

    #[test]
    fn rucc_options_come_first_and_a_target_selects_the_machine() {
        let a = one(
            "rucc",
            &["--target=aarch64-plan9", "--sysroot=/n/guest", "-FTVw", "-I../common", "acme.c"],
        );
        assert_eq!(
            a,
            [
                "--target=aarch64-plan9",
                "--sysroot=/n/guest",
                "-c",
                "-fkencc",
                "-fkencc-strict-void",
                "-I../common",
                "-idirafter",
                ".",
                "-O2",
                "-o",
                "acme.7",
                "acme.c"
            ]
        );
        assert!(plan_with("rucc", &["x.c"], &[]).is_err());
        assert!(plan_with("rucc", &["--target=riscv64-plan9", "x.c"], &[]).is_err());
    }

    #[test]
    fn options_that_rucc_cannot_honour_are_refused_or_noted() {
        assert_eq!(plan_with("6c", &["-l", "x.c"], &[]).unwrap_err().0, "can only use -l with vc");
        assert!(plan_with("6c", &["-aa", "x.c"], &[]).unwrap_err().0.contains("acid"));
        assert!(plan_with("7c", &["-an", "x.c"], &[]).is_err());
        let p = plan_with("6c", &["-dw", "x.c"], &[]).unwrap();
        assert_eq!(p.notes, ["6c: -d (print declarations) does nothing in rucc"]);
        assert!(plan_with("6c", &["-FTVBpcmsw", "x.c"], &[]).unwrap().notes.is_empty());
    }
}
