//! The differential ABI harness.
//!
//! Design: `spec/cross-compile/14-testing.md` section 14.3, which is layer 6 of the ladder in
//! section 14.1, and `spec/cross-compile/06-abis.md` section 6.2 items 4 to 6.
//!
//! `tests/abi-corpus` asks whether two compilers agree about where the members of a struct are,
//! and `_Static_assert` can answer that without running anything. This asks the question that has
//! no static answer: whether they agree about which register an argument arrives in, whether an
//! aggregate travels as a copy or as the address of one, and where a return value comes back.
//! None of that is visible in the source, so the only way to check it is to compile one side of a
//! call with one compiler and the other side with the other, link the two together, run the
//! program, and see whether the values arrived.
//!
//! Section 14.3's argument for doing this at all is document 01.8's result: ABI Cafe found GCC,
//! Clang and rustc disagreeing on x86-64 Linux, the most exercised ABI in existence. Implementing
//! the psABI document is not evidence that you implemented it.
//!
//! # The four builds
//!
//! Both directions, and both controls.
//!
//! | caller | callee | what a failure means |
//! | --- | --- | --- |
//! | reference | reference | the corpus is wrong, and nothing else here means anything |
//! | rucc | rucc | rucc disagrees with itself, so one of the two sides has a bug |
//! | rucc | reference | rucc gets an argument wrong on the way out, or a return on the way in |
//! | reference | rucc | rucc gets an argument wrong on the way in, or a return on the way out |
//!
//! The two mixed builds are the point and the two matching ones are what make a failure readable.
//! A corpus that is wrong about C fails all four, and saying so in one line is better than four
//! lines about calling conventions.
//!
//! # Why this is not a `#[test]`
//!
//! For the reason `xtask/src/safety.rs` gives: it needs a machine the compiler emits code for.
//! The only back end is x86-64, so a test in the workspace would fail or skip on an arm mac, and
//! a suite that skips is a suite nobody notices has stopped running. `xtask/src/runner.rs` is
//! where the container comes from on a machine that is not the one this builds for.
//!
//! # What the first run of it found
//!
//! Nothing about rucc, and something about the apparatus. All four builds agree on every value in
//! the corpus. What they also do, on a developer machine where the container runs under qemu, is
//! crash at random about once in every one hundred and thirty runs, including the build where
//! both sides are the reference compiler and there is no disagreement to have. So the script
//! tries a crash again rather than reporting it, the reasoning is written above `SCRIPT`, and the
//! number is in `spec/cross-compile/14-testing.md` section 14.4's list of what qemu does that the
//! hardware does not.
//!
//! # Other targets
//!
//! `--target` names a row other than the machine's own, and today it accepts
//! `x86_64-windows-gnu`, held against MinGW GCC, and `x86_64-windows-msvc`, held against `cl.exe`
//! on a Windows machine. That path does not use the script below, because
//! it has to work on a Windows machine with no `sh` on it as well as on Linux, so the same builds
//! are compiled, linked and run from here one command at a time. [`Reference`] is what differs
//! between the rows: which compiler is the reference, how it is spelled, and what starts one of
//! the programs it links. On Linux that is Wine and on Windows it is nothing, since the program
//! is native there. `cl.exe` is not spelled like gcc, and that is one more way of writing
//! [`Reference::compile`] and [`Reference::link`] and nothing else.
//!
//! rucc is on both sides at `-O0` and at `-O2` on that path, so it is nine builds rather than
//! four: the reference against itself first, as the control, and then every pairing with rucc on
//! one side or both. `-O0` is the question with the fewest other things going on and `-O2` is the
//! one a user's program is built at, and a convention that only holds at one of them is a bug.
//!
//! The first run for `x86_64-windows-gnu`, against MinGW GCC 13 under Wine 9 on Linux, was nine
//! builds and every value arrived in all of them: one hundred and three functions a side, the
//! variadic ones included, with `long double` passed by reference as the Microsoft convention
//! has it and the eight byte and sixteen byte aggregates in the places it puts them. Nothing
//! crashed and nothing was retried.
//!
//! # What this does not cover yet
//!
//! The rest of the table. The corpus is the same C for all forty three rows, and what runs here is
//! x86-64 System V on the machine's own row and the Microsoft x64 convention on the MinGW one.
//! aarch64 Linux is held against gcc under qemu by `tests/qemu/run.sh`, which runs the same
//! corpus. The rows with no back end wait on one.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::runner::{Runner, TRIPLE};
use crate::{Error, Result, indent, root, target_dir};

/// The optimization level both compilers are asked for.
///
/// None. A calling convention is not an optimization and the two sides have to agree at every
/// level, but a difference that only shows up at `-O2` is a difference in what got inlined into
/// what, and this corpus cannot tell that apart from a difference in the ABI. Nothing is inlined
/// across the boundary here because neither compiler can see the other's source, so `-O0` asks
/// the question with the fewest other things going on.
const LEVEL: &str = "-O0";

/// The four builds, as the pair of compilers each one uses.
///
/// In this order, so the control is first and a corpus that is simply wrong says so before four
/// paragraphs about registers.
const PAIRS: &[(&str, &str)] = &[("cc", "cc"), ("rucc", "rucc"), ("rucc", "cc"), ("cc", "rucc")];

/// Compiles the signature corpus with both compilers, runs every pairing, and reports what
/// disagreed.
///
/// `args` is what followed the task's name, which is nothing for the machine's own row or
/// `--target` and a triple for another one.
///
/// # Errors
///
/// [`Error::Io`] when the corpus will not compile or there is no way to run the programs, and
/// [`Error::Failed`] with one entry per build whose values did not all arrive.
pub(crate) fn differential(args: &[String]) -> Result<()> {
    match target(args)?.as_deref() {
        None | Some(TRIPLE | "x86_64-linux-gnu") => native(),
        Some(triple) => match Reference::for_target(triple) {
            Some(reference) => foreign(&reference),
            None => Err(Error::Io(format!(
                "abi-differential: there is no reference compiler for {triple} yet, only for \
                 x86_64-linux-gnu, x86_64-windows-gnu and x86_64-windows-msvc"
            ))),
        },
    }
}

/// Reads `--target` out of the arguments, the way `cargo xtask builtins` does.
fn target(args: &[String]) -> Result<Option<String>> {
    let mut target = None;
    let mut at = 0;
    while at < args.len() {
        match args[at].as_str() {
            "--target" => {
                at += 1;
                target = Some(
                    args.get(at)
                        .cloned()
                        .ok_or_else(|| Error::Io("--target wants a triple after it".to_owned()))?,
                );
            }
            other => match other.strip_prefix("--target=") {
                Some(triple) => target = Some(triple.to_owned()),
                None => {
                    return Err(Error::Io(format!("abi-differential: unknown argument `{other}`")));
                }
            },
        }
        at += 1;
    }
    Ok(target)
}

/// The machine's own row, the four builds of the table at the top of this file.
fn native() -> Result<()> {
    let runner = Runner::find("this harness")?;
    let work = build()?;
    let ran = read(&runner.run(&work, "the harness")?);

    let mut problems = Vec::new();
    for (caller, callee) in PAIRS {
        let name = format!("{caller}-{callee}");
        let Some(run) = ran.get(&name) else {
            problems.push(format!("{name}: did not run at all"));
            continue;
        };
        judge(caller, callee, run, &mut problems);
    }

    if problems.is_empty() {
        println!("abi-differential: {} builds, {TRIPLE}, {runner}", PAIRS.len());
        return Ok(());
    }
    Err(Error::Failed { task: "abi-differential", problems })
}

/// Says what one build did, on the terminal when every value arrived and in `problems` when not.
fn judge(caller: &str, callee: &str, run: &Ran, problems: &mut Vec<String>) {
    let name = format!("{caller}-{callee}");
    match run.status {
        Some(0) => {
            println!("abi-differential: {caller} calling {callee}, every value arrived");
            // A retry left its note in the output, and a run that needed one is worth saying out
            // loud rather than swallowing. It is the emulation, but a rate that starts climbing is
            // a thing somebody should get to see.
            for line in run.output.lines() {
                println!("abi-differential:   {line}");
            }
        }
        Some(1) => problems.push(format!(
            "{name}: a {caller} caller and a {callee} callee do not agree\n{}",
            indent(run.output.trim_end())
        )),
        Some(_) => problems
            .push(format!("{name}: crashed on every attempt\n{}", indent(run.output.trim_end()))),
        None => problems
            .push(format!("{name}: did not build or link\n{}", indent(run.output.trim_end()))),
    }
}

/// Builds the compiler, compiles the corpus with it, and lays out the directory the runner runs.
///
/// rucc produces assembly here and the reference compiler produces objects inside the runner,
/// which is what `xtask/src/safety.rs` does and for the same reason: rucc cannot assemble or
/// link, and the machine that can is the one the programs run on.
fn build() -> Result<PathBuf> {
    let source = root().join("tests").join("abi-signatures");
    let work = corpus()?;
    let rucc = compiler()?;

    for name in ["caller", "callee"] {
        let out = Command::new(&rucc)
            .args(["-S", &format!("--target={TRIPLE}"), LEVEL])
            .arg("-o")
            .arg(work.join(format!("rucc-{name}.s")))
            .arg(source.join(format!("{name}.c")))
            .current_dir(&source)
            .output()
            .map_err(|e| Error::Io(format!("could not run the compiler: {e}")))?;
        if !out.status.success() {
            return Err(Error::Io(format!(
                "rucc would not compile {name}.c:\n{}",
                indent(String::from_utf8_lossy(&out.stderr).trim_end())
            )));
        }
    }

    std::fs::write(work.join("run.sh"), SCRIPT)
        .map_err(|e| Error::Io(format!("could not write the script: {e}")))?;
    Ok(work)
}

/// Clears the work directory and copies the corpus into it.
fn corpus() -> Result<PathBuf> {
    let source = root().join("tests").join("abi-signatures");
    let work = target_dir().join("abi-differential");
    if work.exists() {
        std::fs::remove_dir_all(&work)
            .map_err(|e| Error::Io(format!("could not clear {}: {e}", work.display())))?;
    }
    std::fs::create_dir_all(&work)
        .map_err(|e| Error::Io(format!("could not make {}: {e}", work.display())))?;

    for name in ["abi.h", "report.c", "caller.c", "callee.c"] {
        std::fs::copy(source.join(name), work.join(name)).map_err(|e| {
            Error::Io(format!(
                "could not copy {name}: {e}. Run `cargo xtask abi-signatures` to write the corpus."
            ))
        })?;
    }
    Ok(work)
}

/// Builds the compiler and says where it is.
fn compiler() -> Result<PathBuf> {
    let status = Command::new("cargo")
        .args(["build", "-q", "--release", "-p", "rucc"])
        .current_dir(root())
        .status()
        .map_err(|e| Error::Io(format!("could not run cargo: {e}")))?;
    if !status.success() {
        return Err(Error::Io("the compiler did not build".to_owned()));
    }
    Ok(target_dir().join("release").join(format!("rucc{}", std::env::consts::EXE_SUFFIX)))
}

/// The script that assembles each side, links the four combinations and runs them.
///
/// Every combination links the same `report.c`, built here by the reference compiler, because it
/// is the one file that includes a libc header and the two files under test deliberately do not.
///
/// A build that will not link prints its log and says `nolink` rather than a status, which is a
/// different failure from a program that ran and found a value in the wrong place, and the two
/// should not be reported the same way.
///
/// # Why a crash is tried again and a wrong value is not
///
/// Because on this project's developer machines the container runs under qemu, and qemu drops a
/// program on the floor every so often for reasons that have nothing to do with the program. The
/// measured rate is seven crashes in nine hundred runs of the build where both sides are the
/// reference compiler, which is the build that cannot have an ABI disagreement in it at all. A
/// harness that reported that as a finding would be a harness people learn to rerun until it goes
/// green, and then it is not a harness.
///
/// So a program that exits 0 or 1 is believed the first time, because those are the two statuses
/// the corpus chooses for itself and neither of them is a crash. Anything else is tried again, up
/// to three times, and only a build that crashes every time is reported. At the measured rate
/// three crashes in a row is about one run in a hundred million, and a program that really does
/// crash still crashes all three times.
const SCRIPT: &str = "\
#!/bin/sh
exec 2>/dev/null
out=/tmp/differential
mkdir -p \"$out\"
cc -std=c17 -O0 -c report.c -o \"$out/report.o\" >\"$out/report.log\" 2>&1
for name in caller callee; do
    cc -std=c17 -O0 -c \"$name.c\" -o \"$out/cc-$name.o\" >\"$out/cc-$name.log\" 2>&1
    cc -c \"rucc-$name.s\" -o \"$out/rucc-$name.o\" >\"$out/rucc-$name.log\" 2>&1
done
for pair in cc:cc rucc:rucc rucc:cc cc:rucc; do
    caller=${pair%:*}
    callee=${pair#*:}
    name=\"$caller-$callee\"
    printf '<<<pair %s>>>\\n' \"$name\"
    if cc \"$out/$caller-caller.o\" \"$out/$callee-callee.o\" \"$out/report.o\" \\
        -o \"$out/$name\" >\"$out/$name.log\" 2>&1; then
        for attempt in 1 2 3; do
            \"$out/$name\" >\"$out/$name.out\" 2>&1
            status=$?
            [ \"$status\" -lt 2 ] && break
            printf 'exit %s, which is not a status this program chooses. Trying again.\\n' \\
                \"$status\"
        done
        cat \"$out/$name.out\"
        printf '<<<status %s>>>\\n' \"$status\"
    else
        cat \"$out/report.log\" \"$out/cc-caller.log\" \"$out/rucc-caller.log\" \"$out/$name.log\"
        printf '<<<status nolink>>>\\n'
    fi
done
";

/// The reference compiler for a row that is not the machine's own, and how its programs start.
struct Reference {
    /// The row, as rucc spells it after `--target=`.
    triple: &'static str,
    /// The command that compiles and links for that row, which is a program on `PATH` or a path.
    cc: String,
    /// What starts a program built for the row, empty when this machine runs it directly.
    runner: Vec<String>,
    /// Whether `cc` reads its options the way `cl.exe` does rather than the way gcc does.
    msvc: bool,
    /// What rucc is told on top of `--target`, on both sides and for the link on the MSVC rows.
    rucc: Vec<String>,
}

impl Reference {
    /// The reference for `triple` on this machine, or [`None`] when there is not one yet.
    ///
    /// `RUCC_ABI_CC` names a different compiler and `RUCC_ABI_RUNNER` a different way of starting
    /// the programs, split at spaces, for a machine where the defaults are somewhere else. The
    /// defaults are MinGW GCC by its cross name and Wine on Linux, and plain `gcc` and nothing on
    /// Windows, which is what MSYS2's MinGW environment puts on `PATH`.
    ///
    /// On `x86_64-windows-msvc` the reference is `cl.exe`, which a Developer Command Prompt or
    /// `ilammy/msvc-dev-cmd` puts on `PATH`. rucc links every build there, against the CRT and
    /// SDK that `RUCC_ABI_SYSROOT` names, which is the directory `rucc --fetch` said to use. So
    /// the link is the same for all nine builds and only the compilers differ. cl.exe has no
    /// `__int128` and no `_Float128`, and the corpus leaves both out when their `__SIZEOF_`
    /// macros are not defined, so rucc is told to forget both or its side would call functions
    /// the cl.exe side never wrote.
    fn for_target(triple: &str) -> Option<Self> {
        let (triple, msvc) = match triple {
            "x86_64-windows-gnu" | "x86_64-w64-mingw32" | "x86_64-pc-windows-gnu" => {
                ("x86_64-windows-gnu", false)
            }
            "x86_64-windows-msvc" | "x86_64-pc-windows-msvc" => ("x86_64-windows-msvc", true),
            _ => return None,
        };
        let windows = cfg!(windows);
        let cc = std::env::var("RUCC_ABI_CC").unwrap_or_else(|_| {
            match (msvc, windows) {
                (true, _) => "cl",
                (false, true) => "gcc",
                (false, false) => "x86_64-w64-mingw32-gcc",
            }
            .to_owned()
        });
        let runner = match std::env::var("RUCC_ABI_RUNNER") {
            Ok(said) => said.split_whitespace().map(str::to_owned).collect(),
            Err(_) if windows => Vec::new(),
            Err(_) => vec![wine()],
        };
        let mut rucc = Vec::new();
        if msvc {
            rucc.extend(["-U__SIZEOF_INT128__", "-U__SIZEOF_FLOAT128__"].map(str::to_owned));
        }
        if let Ok(sysroot) = std::env::var("RUCC_ABI_SYSROOT") {
            rucc.push(format!("--sysroot={sysroot}"));
        }
        Some(Self { triple, cc, runner, msvc, rucc })
    }

    /// The name the reference's side goes by in what this prints.
    fn side(&self) -> &'static str {
        if self.msvc { "cl" } else { "gcc" }
    }

    /// Compiles one file of the corpus to an object.
    fn compile(&self, source: &Path, object: &Path) -> Command {
        let mut command = Command::new(&self.cc);
        if self.msvc {
            // cl.exe wants the object glued to its option, and `-Od` is its `-O0`.
            let mut fo = std::ffi::OsString::from("-Fo");
            fo.push(object);
            command.args(["-nologo", "-c", "-std:c11", "-Od"]).arg(source).arg(fo);
        } else {
            command.args(["-std=c17", LEVEL, "-c"]).arg(source).arg("-o").arg(object);
        }
        command
    }

    /// Links a caller, a callee and the report into a program, with `rucc` on the MSVC rows.
    fn link(&self, rucc: &Path, objects: &[PathBuf], program: &Path) -> Command {
        let mut command;
        if self.msvc {
            command = Command::new(rucc);
            command.arg(format!("--target={}", self.triple)).args(&self.rucc);
        } else {
            command = Command::new(&self.cc);
        }
        command.args(objects).arg("-o").arg(program);
        command
    }

    /// Starts a program this reference linked.
    fn start(&self, program: &Path) -> Command {
        match self.runner.split_first() {
            Some((first, rest)) => {
                let mut command = Command::new(first);
                command.args(rest).arg(program);
                command
            }
            None => Command::new(program),
        }
    }

    /// How the programs were run, for the summary line.
    fn how(&self) -> String {
        match self.runner.first() {
            Some(first) => format!("run under {first}"),
            None => "run here".to_owned(),
        }
    }
}

/// Wine, by the first of its names that is on this machine.
///
/// `wine64` is what Debian and Ubuntu put in `/usr/lib/wine` and not on `PATH`, and `wine` is what
/// the Wine project's own packages and most other distributions call it.
fn wine() -> String {
    for name in ["wine64", "wine"] {
        if Command::new(name).arg("--version").output().is_ok_and(|out| out.status.success()) {
            return name.to_owned();
        }
    }
    if Path::new("/usr/lib/wine/wine64").exists() {
        return "/usr/lib/wine/wine64".to_owned();
    }
    "wine".to_owned()
}

/// The levels rucc builds each side at on a row that is not the machine's own.
const FOREIGN_LEVELS: &[&str] = &["-O0", "-O2"];

/// A row that is not the machine's own, compiled, linked and run from here one command at a time.
fn foreign(reference: &Reference) -> Result<()> {
    let work = corpus()?;
    let rucc = compiler()?;
    let triple = reference.triple;
    let mut problems = Vec::new();

    // The reference's objects. A corpus the reference will not compile is not a finding about
    // rucc, so it stops the run rather than turning into nine failures.
    let mut sides = vec![reference.side().to_owned()];
    for name in ["report", "caller", "callee"] {
        let object = work.join(format!("{}-{name}.o", reference.side()));
        let out = reference
            .compile(&work.join(format!("{name}.c")), &object)
            .current_dir(&work)
            .output()
            .map_err(|e| Error::Io(format!("could not run {}: {e}", reference.cc)))?;
        if !out.status.success() {
            return Err(Error::Io(format!(
                "{} would not compile {name}.c:\n{}",
                reference.cc,
                indent(String::from_utf8_lossy(&out.stderr).trim_end())
            )));
        }
    }

    // rucc's, at each level. One that will not compile is a finding and its builds are reported
    // as never having linked, which is what they are.
    let mut refused = BTreeMap::new();
    for level in FOREIGN_LEVELS {
        let side = format!("rucc{level}");
        for name in ["caller", "callee"] {
            let out = Command::new(&rucc)
                .args(["-c", &format!("--target={triple}"), level])
                .args(&reference.rucc)
                .arg(work.join(format!("{name}.c")))
                .arg("-o")
                .arg(work.join(format!("{side}-{name}.o")))
                .current_dir(&work)
                .output()
                .map_err(|e| Error::Io(format!("could not run the compiler: {e}")))?;
            if !out.status.success() {
                refused.insert(
                    side.clone(),
                    format!(
                        "rucc {level} would not compile {name}.c:\n{}",
                        String::from_utf8_lossy(&out.stderr).trim_end()
                    ),
                );
            }
        }
        sides.push(side);
    }

    let mut builds = 0;
    for caller in &sides {
        for callee in &sides {
            builds += 1;
            let run = match refused.get(caller).or_else(|| refused.get(callee)) {
                Some(why) => Ran { output: why.clone(), status: None },
                None => pairing(reference, &rucc, &work, caller, callee)?,
            };
            judge(caller, callee, &run, &mut problems);
        }
    }

    if problems.is_empty() {
        println!(
            "abi-differential: {builds} builds, {triple} against {}, {}",
            reference.cc,
            reference.how()
        );
        return Ok(());
    }
    Err(Error::Failed { task: "abi-differential", problems })
}

/// Links one caller to one callee and runs it, by the rule [`SCRIPT`] follows: a status of 0 or
/// 1 is believed the first time, and anything else is tried again up to three times.
fn pairing(
    reference: &Reference,
    rucc: &Path,
    work: &Path,
    caller: &str,
    callee: &str,
) -> Result<Ran> {
    let program = work.join(format!("{caller}-{callee}.exe"));
    let objects = [
        work.join(format!("{caller}-caller.o")),
        work.join(format!("{callee}-callee.o")),
        work.join(format!("{}-report.o", reference.side())),
    ];
    let out = reference
        .link(rucc, &objects, &program)
        .current_dir(work)
        .output()
        .map_err(|e| Error::Io(format!("could not run {}: {e}", reference.cc)))?;
    if !out.status.success() {
        return Ok(Ran { output: String::from_utf8_lossy(&out.stderr).into_owned(), status: None });
    }

    let mut output = String::new();
    for attempt in 1..=3 {
        let out = reference
            .start(&program)
            .current_dir(work)
            .output()
            .map_err(|e| Error::Io(format!("could not start {}: {e}", program.display())))?;
        // A Windows program writes CRLF to a text stream, and the line it prints is the same line
        // either way.
        let said = String::from_utf8_lossy(&out.stderr).replace('\r', "")
            + &String::from_utf8_lossy(&out.stdout).replace('\r', "");
        let status = out.status.code();
        if matches!(status, Some(0 | 1)) || attempt == 3 {
            output.push_str(&said);
            // A program killed by a signal has no code, and it is a crash like any other status
            // the corpus does not choose.
            return Ok(Ran { output, status: Some(status.unwrap_or(-1)) });
        }
        let _ = writeln!(
            output,
            "exit {}, which is not a status this program chooses. Trying again.",
            status.map_or_else(|| "by a signal".to_owned(), |code| code.to_string())
        );
    }
    unreachable!("the third attempt returns")
}

/// What one build did.
struct Ran {
    /// Everything it printed, which for a failing run is one line per value that did not arrive.
    output: String,
    /// Its exit status, and [`None`] when it never got as far as running.
    status: Option<i32>,
}

/// Splits what the script printed back into one entry per build.
fn read(text: &str) -> BTreeMap<String, Ran> {
    let mut runs = BTreeMap::new();
    let mut name = String::new();
    let mut output = String::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("<<<pair ").and_then(|l| l.strip_suffix(">>>")) {
            name = rest.to_owned();
            output.clear();
            continue;
        }
        if let Some(rest) = line.strip_prefix("<<<status ").and_then(|l| l.strip_suffix(">>>")) {
            runs.insert(
                std::mem::take(&mut name),
                Ran { output: std::mem::take(&mut output), status: rest.parse().ok() },
            );
            continue;
        }
        let _ = writeln!(output, "{line}");
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_build_that_never_linked_is_not_a_build_that_passed() {
        // `nolink` does not parse as a status, and reading it as a zero would turn a corpus that
        // will not compile into four green ticks.
        let runs = read("<<<pair rucc-cc>>>\nundefined reference\n<<<status nolink>>>\n");
        assert_eq!(runs["rucc-cc"].status, None);
    }

    #[test]
    fn the_lines_a_build_printed_stay_with_that_build() {
        let runs = read(
            "<<<pair cc-cc>>>\n<<<status 0>>>\n<<<pair rucc-cc>>>\ng00: a1 did not arrive\n\
             <<<status 1>>>\n",
        );
        assert_eq!(runs["cc-cc"].output, "");
        assert_eq!(runs["rucc-cc"].status, Some(1));
        assert!(runs["rucc-cc"].output.contains("g00: a1"));
    }
}
