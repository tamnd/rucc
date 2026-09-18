//! Real libraries, built instrumented, linked, run against a real workload, and held to their
//! answers.
//!
//! Design: `spec/safe-memory/14-verification.md` section 14.9 and the seventh box of
//! tamnd/rucc#1307.
//!
//! Everything in `tests/safety` is a few lines of C written to provoke one judgement, which is the
//! right shape for asking whether a check fires. It is the wrong shape for asking whether a real
//! library survives the monitor, and the two are different questions. tamnd/rucc#1307 is what the
//! gap between them costs: every interposed library write recorded the init plane and said nothing
//! on the type plane, which no case in the suite could see, and an instrumented SQLite aborted
//! within a few hundred calls because its lookaside allocator hands the same block out as three
//! different structures. Three more holes of the same shape have been found since, and every one
//! of them was found by running a real library by hand.
//!
//! So it stops being by hand. A library that recycles its own storage, walks its own trees and
//! answers questions whose answers are arithmetic is a thing the suite cannot imitate and does not
//! have to: the sources compile, rucc builds them, and the answers are either right or they are
//! not.
//!
//! # Why there is a table
//!
//! One library is an anecdote. SQLite is a single enormous translation unit that recycles small
//! blocks through a lookaside allocator, and it leans on the type plane harder than anything else
//! we have. zlib is fifteen small ones that take a few large buffers at the start and then index
//! them with pointers held inside the caller's own structure, which leans on the capability
//! instead, and it walks into the init plane the first time it slides its hash table. Lua is
//! neither: every value it moves is a tagged union whose payload is sometimes a pointer and
//! sometimes an integer of the same width, its collector reaches every live object by walking those
//! unions, and its errors leave through longjmp from the middle of a C stack the interpreter built.
//! brotli is the same job as zlib written a different way: a static dictionary of a hundred and
//! twenty thousand words, a ring buffer it grows as it learns how much it needs, and a distance that
//! is a number selecting storage from one of several places rather than a pointer to it. None of the
//! four reaches the others' paths. The rows here are the projects and the code below is the same for
//! all of them, which is what makes adding the next one a paragraph of data.
//!
//! # Why the sources are not in the tree
//!
//! They are somebody else's C and they have versions. Vendoring them would put a copy of SQLite in
//! a compiler's history forever and make every bump of it a commit here, and the checks do not care
//! which version they get: the workloads are ordinary use and the answers are arithmetic the
//! workload does itself. So the sources are looked for and the check says what to fetch when they
//! are not there, which is the same bargain `xtask/src/real_libc.rs` makes about a glibc abilist
//! and for the same reason.
//!
//! # What it is checking
//!
//! That each program links, runs, and prints the answers its own arithmetic says it should, at
//! `-O0` and at `-O2`. Both levels, because the two failures are different: at `-O0` a wrong answer
//! is the instrumentation or the runtime, and at `-O2` it is either of those or a check the
//! optimizer removed that was holding something up.
//!
//! And that the monitor said exactly what the row says it should. A report the row does not list is
//! a failure, because these workloads do nothing wrong, so an unlisted report is a false positive
//! by construction. A listed report that did not arrive is a failure too, because the row is a
//! claim that the library really does do that and the day it stops being true is a day somebody
//! should look.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::runner::{Runner, TRIPLE};
use crate::safety::{self, BANNER};
use crate::{Error, Result, cost, indent, root};

/// What a program has to say before its answers count.
///
/// The workloads check their own arithmetic and print this when all of it came out, which keeps the
/// expected numbers in the C files beside the work that produces them rather than here, where they
/// would be a second copy to keep in step.
const CORRECT: &str = "all answers correct";

/// The levels everything is built and run at.
const LEVELS: [&str; 2] = ["-O0", "-O2"];

/// What the link needs beyond the objects and the runtime archive.
///
/// The runtime's own three, which SQLite's configure script asks for as well and which are a
/// superset of what Lua's makefile asks for, so one list covers every row so far and will cover most
/// of the next ones.
const LIBRARIES: [&str; 3] = ["-lpthread", "-lm", "-ldl"];

/// A report a library earns honestly, so that the check can tell one from a false positive.
///
/// There is no symbol in a report and the address in one is different every run, so what is matched
/// is the judgement and the width of the access. That is coarse, and it is the same coarseness the
/// safety suite's own rows have. What carries the meaning is `why`, which names the site for the
/// person reading this rather than for the code.
struct Known {
    /// The judgement number, as `J1` is written in a report.
    judgement: u32,
    /// The width of the access, in bytes.
    bytes: u32,
    /// The site, and why the library is entitled to it.
    why: &'static str,
}

/// A library, everything needed to build it, and everything the monitor is expected to say.
struct Project {
    /// The name, which is the case name in the report, the directory the build goes in, and the
    /// directory under `tests` the workload lives in.
    name: &'static str,
    /// The variable somebody points at the sources, which may name the directory or the file inside
    /// it that [`Project::marker`] gives.
    variable: &'static str,
    /// The file that says an unpacked directory really is this project.
    marker: &'static str,
    /// Directory names the project's own tarball unpacks to, under the two places a person is
    /// likely to have put it.
    usual: &'static [&'static str],
    /// The C files to build, relative to the source directory.
    sources: &'static [&'static str],
    /// Anything else to put on the include path, relative to the source directory.
    ///
    /// The source directory itself is always searched, which is all the first rows needed. A project
    /// that keeps its public headers apart from its C, which is most of the larger ones, names the
    /// directory here and the workload gets the same path the library does.
    includes: &'static [&'static str],
    /// What the project's configure script would have defined on a Linux machine.
    defines: &'static [&'static str],
    /// Every report the monitor should make, and nothing else.
    known: &'static [Known],
}

/// The projects, in the order they are run.
///
/// SQLite first because it is the one four holes were found with, and because nine megabytes of C
/// compiled twice is most of the time this check takes either way.
const PROJECTS: &[Project] = &[
    Project {
        name: "sqlite",
        variable: "RUCC_SQLITE_AMALGAMATION",
        marker: "sqlite3.c",
        usual: &["sqlite-autoconf", "sqlite"],
        sources: &["sqlite3.c"],
        includes: &[],
        defines: &[],
        known: &[],
    },
    Project {
        name: "zlib",
        variable: "RUCC_ZLIB_SOURCE",
        marker: "zlib.h",
        usual: &["zlib"],
        sources: &[
            "adler32.c",
            "compress.c",
            "crc32.c",
            "deflate.c",
            "gzclose.c",
            "gzlib.c",
            "gzread.c",
            "gzwrite.c",
            "infback.c",
            "inffast.c",
            "inflate.c",
            "inftrees.c",
            "trees.c",
            "uncompr.c",
            "zutil.c",
        ],
        includes: &[],
        // What zlib's configure writes on any Linux machine. Without it the three gzip files do not
        // compile at all, because zlib only reaches for unistd.h when it has been told the header
        // is there.
        defines: &["HAVE_UNISTD_H=1"],
        known: &[Known {
            judgement: 1,
            bytes: 2,
            why: "slide_hash in deflate.c reads every entry of s->prev before anything has \
                  written it. The allocation is a plain malloc, the loop reads each two byte entry \
                  and writes a value derived from it back, and zlib's own comment beside the loop \
                  says the value is garbage for any position not on a hash chain and will never be \
                  used. That is true of what the value is used for and it is still a read of \
                  storage nothing initialised, so the init plane is right to say so. It is one of \
                  the four classes tamnd/rucc#431 is about and it is the first one a second \
                  project produced.",
        }],
    },
    Project {
        name: "lua",
        variable: "RUCC_LUA_SOURCE",
        marker: "lua.h",
        usual: &["lua"],
        sources: &[
            "lapi.c",
            "lauxlib.c",
            "lbaselib.c",
            "lcode.c",
            "lcorolib.c",
            "lctype.c",
            "ldblib.c",
            "ldebug.c",
            "ldo.c",
            "ldump.c",
            "lfunc.c",
            "lgc.c",
            "linit.c",
            "liolib.c",
            "llex.c",
            "lmathlib.c",
            "lmem.c",
            "loadlib.c",
            "lobject.c",
            "lopcodes.c",
            "loslib.c",
            "lparser.c",
            "lstate.c",
            "lstring.c",
            "lstrlib.c",
            "ltable.c",
            "ltablib.c",
            "ltm.c",
            "lundump.c",
            "lutf8lib.c",
            "lvm.c",
            "lzio.c",
        ],
        includes: &[],
        // The one thing Lua's own makefile passes on this platform. It turns on the POSIX bits of
        // the io and os libraries and the dlopen path in loadlib.c, all of which are library code
        // this ought to be running rather than stubs. The two files with a main in them, lua.c and
        // luac.c, are not in the list above, and they are the only ones that would have wanted
        // readline.
        defines: &["LUA_USE_LINUX"],
        known: &[],
    },
    Project {
        name: "brotli",
        variable: "RUCC_BROTLI_SOURCE",
        marker: "c/include/brotli/decode.h",
        usual: &["brotli"],
        sources: &[
            "c/common/constants.c",
            "c/common/context.c",
            "c/common/dictionary.c",
            "c/common/platform.c",
            "c/common/shared_dictionary.c",
            "c/common/transform.c",
            "c/dec/bit_reader.c",
            "c/dec/decode.c",
            "c/dec/huffman.c",
            "c/dec/state.c",
            "c/enc/backward_references.c",
            "c/enc/backward_references_hq.c",
            "c/enc/bit_cost.c",
            "c/enc/block_splitter.c",
            "c/enc/brotli_bit_stream.c",
            "c/enc/cluster.c",
            "c/enc/command.c",
            "c/enc/compound_dictionary.c",
            "c/enc/compress_fragment.c",
            "c/enc/compress_fragment_two_pass.c",
            "c/enc/dictionary_hash.c",
            "c/enc/encode.c",
            "c/enc/encoder_dict.c",
            "c/enc/entropy_encode.c",
            "c/enc/fast_log.c",
            "c/enc/histogram.c",
            "c/enc/literal_cost.c",
            "c/enc/memory.c",
            "c/enc/metablock.c",
            "c/enc/static_dict.c",
            "c/enc/utf8_util.c",
        ],
        // brotli's public headers are the only ones outside its C, and both the library and the
        // workload reach them by the same spelling, so both get the same flag. The one file with a
        // main in it, c/tools/brotli.c, is not in the list above.
        includes: &["c/include"],
        defines: &[],
        known: &[],
    },
];

/// Builds each library at each level, runs its workload, and holds it to its answers and to what
/// the monitor said about it.
///
/// # Errors
///
/// [`Error::Failed`] when a build did not link, a run did not exit cleanly, the answers came back
/// wrong, or the monitor said something other than what the row says it should, with one line per
/// thing that went wrong. [`Error::Io`] when the check could not be run at all, which is the
/// compiler not building or no way to run an x86-64 Linux program.
pub(crate) fn libraries() -> Result<()> {
    let runner = Runner::find("this check")?;
    let mut problems = Vec::new();
    let mut ran_any = false;

    for project in PROJECTS {
        let Some(source) = found(project) else {
            // Not a failure, the same way a mac with no glibc is not one. What is worth printing is
            // where to get the sources, because a person reading this line is a person about to go
            // and look for them.
            println!(
                "{}: no sources on this machine, so nothing was built. Unpack the project's own \
                 tarball and point {} at it.",
                project.name, project.variable
            );
            continue;
        };
        ran_any = true;
        println!(
            "{}: {}, -fsafety=detect at -O0 and -O2, {runner}",
            project.name,
            source.display()
        );
        let work = build(project, &source)?;
        let ran = safety::read(&runner.run(&work, "the workload")?);
        judge(project, &ran, &mut problems);
    }

    if !ran_any {
        return Ok(());
    }
    if problems.is_empty() {
        println!("libraries: every level links, runs and answers correctly");
        return Ok(());
    }
    Err(Error::Failed { task: "libraries", problems })
}

/// Reads one project's two runs and says what was wrong with them.
fn judge(
    project: &Project,
    ran: &std::collections::BTreeMap<String, safety::Ran>,
    out: &mut Vec<String>,
) {
    for level in LEVELS {
        let name = project.name;
        let Some(ran) = ran.get(level) else {
            out.push(format!("{name} {level}: did not run"));
            continue;
        };
        match ran.status {
            None => out
                .push(format!("{name} {level}: did not link.\n{}", indent(ran.output.trim_end()))),
            Some(0) if ran.output.contains(CORRECT) => {}
            _ => out.push(format!(
                "{name} {level}: ran and got the wrong answers, or did not finish.\n{}",
                indent(ran.output.trim_end())
            )),
        }
        said(project, level, &ran.output, out);
    }
}

/// Holds what the monitor said against what the row says it should have said.
///
/// Both directions. A report nobody listed is a false positive, since the workload does nothing
/// wrong. A listed report that did not arrive means the row is describing a library that no longer
/// does what it says, or a check that stopped looking, and either one is worth a person's time.
fn said(project: &Project, level: &str, output: &str, out: &mut Vec<String>) {
    let mut wanted: Vec<&Known> = project.known.iter().collect();
    for (judgement, bytes) in reports(output) {
        match wanted.iter().position(|k| k.judgement == judgement && k.bytes == bytes) {
            Some(at) => {
                wanted.remove(at);
            }
            None => out.push(format!(
                "{} {level}: the monitor made a report nothing here expects, J{judgement} over \
                 {bytes} bytes, so this is a false positive or a finding somebody should write \
                 down.\n{}",
                project.name,
                indent(output.trim_end())
            )),
        }
    }
    for missing in wanted {
        out.push(format!(
            "{} {level}: the monitor did not report J{} over {} bytes, which this row says it \
             should. {}",
            project.name, missing.judgement, missing.bytes, missing.why
        ));
    }
}

/// Every report in a run, as the judgement number and the width of the access.
///
/// The runs are made in the deduplicating posture, so one site says its piece once however many
/// times it is reached, and what comes back out of here is one entry per site rather than one per
/// occurrence.
fn reports(output: &str) -> Vec<(u32, u32)> {
    let mut found = Vec::new();
    for chunk in output.split(BANNER).skip(1) {
        let Some(judgement) = number_after(chunk, "  judgement J") else {
            continue;
        };
        let Some(bytes) = number_before(chunk, " bytes at ") else {
            continue;
        };
        found.push((judgement, bytes));
    }
    found
}

/// The number that starts right after `tag`.
fn number_after(text: &str, tag: &str) -> Option<u32> {
    let at = text.find(tag)? + tag.len();
    let digits: String = text[at..].chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// The number that ends right before `tag`.
fn number_before(text: &str, tag: &str) -> Option<u32> {
    let at = text.find(tag)?;
    let digits: String =
        text[..at].chars().rev().take_while(char::is_ascii_digit).collect::<String>();
    digits.chars().rev().collect::<String>().parse().ok()
}

/// Where a project's sources are, if they are anywhere this knows to look.
fn found(project: &Project) -> Option<PathBuf> {
    if let Some(said) = std::env::var_os(project.variable) {
        if let Some(said) = declared(Path::new(&said), project.marker) {
            return Some(said);
        }
    }
    usual(project)
}

/// What a path somebody set is worth, which is nothing when the sources are not under it.
///
/// Either the directory or the marker file inside it, because a variable that used to name one file
/// is easy to leave naming that file. A variable pointing at neither reads as the variable not
/// being set, since the sentence that prints in that case names the variable and is the right thing
/// to read either way.
fn declared(said: &Path, marker: &str) -> Option<PathBuf> {
    if let Some(holding) = holds(said, marker) {
        return Some(holding);
    }
    if said.is_file() && said.file_name()? == marker {
        return Some(said.parent()?.to_path_buf());
    }
    None
}

/// The directory under `dir` that really holds the sources, which is `dir` itself or the `src` under
/// it.
///
/// Both, because a project is as likely to unpack its C into a `src` directory as to leave it at the
/// top and the person setting the variable should not have to remember which this one does. Only one
/// level, for the reason [`usual`] gives about hunting.
fn holds(dir: &Path, marker: &str) -> Option<PathBuf> {
    if !dir.is_dir() {
        return None;
    }
    if dir.join(marker).is_file() {
        return Some(dir.to_path_buf());
    }
    let inside = dir.join("src");
    inside.join(marker).is_file().then_some(inside)
}

/// Where the sources are when nobody said, which is the directory the project's tarball unpacks to
/// under one of the two places a person is likely to have put it.
///
/// Neither place is searched recursively, because a check that goes hunting through a home
/// directory for somebody else's C is a check that will one day find the wrong copy of it.
fn usual(project: &Project) -> Option<PathBuf> {
    let homes =
        [std::env::var_os("HOME").map(PathBuf::from), Some(PathBuf::from("/usr/local/src"))];
    for home in homes.into_iter().flatten() {
        for stem in project.usual {
            let Ok(entries) = std::fs::read_dir(&home) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name();
                let Some(name) = name.to_str() else {
                    continue;
                };
                if !name.starts_with(stem) {
                    continue;
                }
                if let Some(holding) = holds(&entry.path(), project.marker) {
                    return Some(holding);
                }
            }
        }
    }
    None
}

/// Compiles a project and its workload at each level and leaves a directory the runner can take.
///
/// Assembly rather than objects, so that the link is the runner's and a machine that cannot run an
/// x86-64 program can still do everything up to it. That is the same arrangement the safety suite
/// has, and it is what lets the container do one link and one run rather than being handed a binary
/// built somewhere it cannot read.
fn build(project: &Project, source: &Path) -> Result<PathBuf> {
    let work = root().join("target").join("libraries").join(project.name);
    if work.exists() {
        std::fs::remove_dir_all(&work)
            .map_err(|e| Error::Io(format!("could not clear {}: {e}", work.display())))?;
    }
    std::fs::create_dir_all(&work)
        .map_err(|e| Error::Io(format!("could not make {}: {e}", work.display())))?;

    let rucc = cost::compiler()?;
    let archive = crate::staticlib("rucc-safe-rt", TRIPLE)?;
    std::fs::copy(&archive, work.join("safe-rt.a"))
        .map_err(|e| Error::Io(format!("could not copy {}: {e}", archive.display())))?;

    let driver = root().join("tests").join(project.name).join("a-real-workload.c");
    let mut problems = Vec::new();
    for level in LEVELS {
        for (file, stem) in files(project, source, &driver) {
            let mut command = Command::new(&rucc);
            command
                .args(["-S", &format!("--target={TRIPLE}"), "-fsafety=detect", level])
                .arg("-I")
                .arg(source)
                .args(
                    project
                        .includes
                        .iter()
                        .flat_map(|inside| [Path::new("-I").to_path_buf(), source.join(inside)]),
                )
                .args(project.defines.iter().map(|define| format!("-D{define}")))
                .arg("-o")
                .arg(work.join(format!("{stem}{level}.s")))
                .arg(&file);
            let out = command
                .current_dir(root())
                .output()
                .map_err(|e| Error::Io(format!("could not run the compiler: {e}")))?;
            if !out.status.success() {
                problems.push(format!(
                    "{} {level}: {} did not compile\n{}",
                    project.name,
                    file.display(),
                    indent(String::from_utf8_lossy(&out.stderr).trim_end())
                ));
            }
        }
    }
    if !problems.is_empty() {
        return Err(Error::Failed { task: "libraries", problems });
    }

    std::fs::write(work.join("run.sh"), script(project))
        .map_err(|e| Error::Io(format!("could not write the script: {e}")))?;
    Ok(work)
}

/// Every file to compile and the name its assembly goes under.
///
/// The workload is called `driver` rather than what it is called in the tree, so that the script
/// below can name it without knowing which project it is running.
///
/// A project that keeps its C in subdirectories gets one flat directory of assembly out, with the
/// path written into the name, because two files called `state.c` in two directories are two files
/// and a name that dropped the directory would quietly be one.
fn files(project: &Project, source: &Path, driver: &Path) -> Vec<(PathBuf, String)> {
    let mut all: Vec<(PathBuf, String)> = project
        .sources
        .iter()
        .map(|name| (source.join(name), name.trim_end_matches(".c").replace('/', "-")))
        .collect();
    all.push((driver.to_path_buf(), "driver".to_owned()));
    all
}

/// The script that links each level and runs it.
///
/// `gcc` rather than rucc's own driver for the link, because what is being checked is the code rucc
/// generated and not the link line it writes. `-no-pie` for the same reason the safety suite uses
/// it: the runtime's planes are addressed absolutely.
///
/// The runs are made in the deduplicating posture rather than the aborting one, because what this
/// check wants is every distinct site the monitor has something to say about and not the first one,
/// and because a library with a report the row expects still has to finish its workload and get the
/// answers right.
///
/// The case names are the levels, so that what comes back reads the way the safety suite's does and
/// can be split by the same reader.
fn script(project: &Project) -> String {
    let stems: Vec<String> = files(project, Path::new(""), Path::new(""))
        .into_iter()
        .map(|(_, stem)| format!("\"{stem}$level.s\""))
        .collect();
    let objects = stems.join(" ");
    let libraries = LIBRARIES.join(" ");
    format!(
        "\
#!/bin/sh
exec 2>/dev/null
here=$(pwd)
out=/tmp/rucc-${{here##*/}}
mkdir -p \"$out\"
RUCC_SAFETY_ON_ERROR=continue
export RUCC_SAFETY_ON_ERROR
for level in -O0 -O2; do
    printf '<<<case %s>>>\\n' \"$level\"
    if gcc -no-pie {objects} safe-rt.a {libraries} \\
        -o \"$out/run$level\" >\"$out/$level.log\" 2>&1; then
        (cd \"$out\" && \"$out/run$level\") >\"$out/$level.out\" 2>&1
        status=$?
        cat \"$out/$level.out\"
        printf '<<<status %s>>>\\n' \"$status\"
    else
        cat \"$out/$level.log\"
        printf '<<<status nolink>>>\\n'
    fi
done
"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The script and the compile loop have to agree about what the files are called, and they are
    /// written out in two places because one is shell and the other is Rust.
    #[test]
    fn the_script_links_the_files_the_build_writes() {
        for project in PROJECTS {
            let text = script(project);
            for level in LEVELS {
                assert!(text.contains(level), "{text}");
            }
            for (_, stem) in files(project, Path::new(""), Path::new("")) {
                assert!(text.contains(&format!("\"{stem}$level.s\"")), "{text}");
            }
        }
    }

    /// Four runs share `/tmp` under the gate and a fixed name would be four of them in one
    /// directory, which is the hazard the safety suite's own test is about.
    #[test]
    fn the_programs_go_somewhere_named_after_the_run() {
        for project in PROJECTS {
            assert!(script(project).contains("out=/tmp/rucc-${here##*/}"), "{}", project.name);
        }
    }

    /// The row is a claim that the monitor will say something, which is only worth making in the
    /// posture that lets the program reach every site rather than stopping at the first.
    #[test]
    fn the_runs_are_made_in_the_posture_that_sees_every_site() {
        for project in PROJECTS {
            assert!(script(project).contains("RUCC_SAFETY_ON_ERROR=continue"), "{}", project.name);
        }
    }

    /// The sentence a workload prints when its arithmetic came out is the thing this check reads,
    /// so it has to be the sentence the workload actually prints, in every project.
    #[test]
    fn every_workload_prints_the_words_this_looks_for() {
        for project in PROJECTS {
            let path = root().join("tests").join(project.name).join("a-real-workload.c");
            let source = std::fs::read_to_string(&path)
                .unwrap_or_else(|_| panic!("{} is in the tree", path.display()));
            assert!(source.contains(CORRECT), "{CORRECT} is not what {} prints", project.name);
        }
    }

    /// A row that expects a report has to say which site and why, because the judgement and the
    /// width on their own do not tell a later reader anything they could check.
    #[test]
    fn every_expected_report_says_what_it_is() {
        for project in PROJECTS {
            for known in project.known {
                assert!(
                    known.why.len() > 80,
                    "{} says too little about J{}",
                    project.name,
                    known.judgement
                );
            }
        }
    }

    /// Both directions of the expectation, since the whole value of the row is that it fails when
    /// the monitor says more than it should and when it says less.
    #[test]
    fn a_report_is_held_against_the_row_in_both_directions() {
        let quiet = Project {
            name: "quiet",
            variable: "",
            marker: "",
            usual: &[],
            sources: &[],
            includes: &[],
            defines: &[],
            known: &[],
        };
        let expecting = Project {
            known: &[Known { judgement: 1, bytes: 2, why: "because the test says so" }],
            ..quiet
        };
        let one = format!("{BANNER}\n  judgement J1, whatever\n  2 bytes at 0x1\n");

        let mut out = Vec::new();
        said(&quiet, "-O0", &one, &mut out);
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(out[0].contains("a report nothing here expects"), "{out:?}");

        let mut out = Vec::new();
        said(&expecting, "-O0", &one, &mut out);
        assert!(out.is_empty(), "{out:?}");

        let mut out = Vec::new();
        said(&expecting, "-O0", "a run that said nothing", &mut out);
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(out[0].contains("did not report J1 over 2 bytes"), "{out:?}");

        // Two sites that look alike are two reports and the row only excuses one of them, which is
        // the coarseness of matching on the judgement and the width made visible.
        let mut out = Vec::new();
        said(&expecting, "-O0", &format!("{one}{one}"), &mut out);
        assert_eq!(out.len(), 1, "{out:?}");
    }

    /// An unset variable and a variable pointing at nothing are the same answer, because the
    /// sentence that prints names the variable either way.
    #[test]
    fn a_path_that_is_not_there_reads_as_nothing_being_there() {
        assert_eq!(declared(Path::new("/nowhere/at/all"), "sqlite3.c"), None);
        // A directory with nothing of the project in it is not the project.
        assert_eq!(declared(&root(), "sqlite3.c"), None);
        // The directory and the file inside it are both accepted, because a variable that used to
        // name the file is easy to leave naming the file.
        let tests = root().join("tests").join("zlib");
        assert_eq!(declared(&tests, "a-real-workload.c"), Some(tests.clone()));
        assert_eq!(declared(&tests.join("a-real-workload.c"), "a-real-workload.c"), Some(tests));
    }

    /// The assembly all goes in one directory, so two files with the same name in two of a
    /// project's own directories have to come out with two names.
    #[test]
    fn a_file_in_a_subdirectory_keeps_the_subdirectory_in_its_name() {
        for project in PROJECTS {
            let mut stems: Vec<String> =
                files(project, Path::new(""), Path::new("")).into_iter().map(|(_, s)| s).collect();
            let all = stems.len();
            stems.sort();
            stems.dedup();
            assert_eq!(stems.len(), all, "{} has two files under one name", project.name);
            for stem in &stems {
                assert!(!stem.contains('/'), "{stem} would want a directory that is not made");
            }
        }
    }

    /// Half the projects worth running unpack their C into a `src` directory and half leave it at
    /// the top, and the person setting the variable should not have to know which this one is.
    #[test]
    fn the_sources_are_taken_from_a_src_directory_as_well_as_from_the_top() {
        let xtask = root().join("xtask");
        assert_eq!(holds(&xtask, "libraries.rs"), Some(xtask.join("src")));
        assert_eq!(holds(&xtask, "Cargo.toml"), Some(xtask.clone()));
        assert_eq!(holds(&xtask, "nothing-of-the-sort.c"), None);
        assert_eq!(declared(&xtask, "libraries.rs"), Some(xtask.join("src")));
    }

    /// The reader has to come back with one entry per site and with the numbers the report wrote,
    /// because everything this check says about false positives rests on it.
    #[test]
    fn the_reader_finds_the_judgement_and_the_width() {
        let text = format!(
            "some output\n{BANNER}\n  judgement J1, an access the capability, the planes or the \
             alignment did not permit\n  2 bytes at 0x00007f655b86485e\n  in instance 3, which is \
             live\nmore output\n{BANNER}\n  judgement J10, whatever that one says\n  16 bytes at \
             0x0000000000001000\n"
        );
        assert_eq!(reports(&text), vec![(1, 2), (10, 16)]);
        assert_eq!(reports("a run that said nothing"), Vec::new());
    }
}
