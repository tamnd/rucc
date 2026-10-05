//! The OSS-Fuzz replay.
//!
//! Design: `spec/safe-memory/12-corpus-and-evidence.md` section 12.7, which calls this the highest
//! expected value activity in the project relative to its cost, and `spec/safe-memory/16-milestones.md`
//! S5, which is where it is due. The tracking issue is tamnd/rucc#1499.
//!
//! # What this is and what `libraries` is
//!
//! [`crate::libraries`] builds seven projects at Tier D and runs a workload this repository wrote.
//! That proves the library survives instrumentation and that the monitor is quiet on code doing
//! nothing wrong, which is the precision axis. It does not go looking for anything, because a
//! workload somebody writes exercises what they thought to write down.
//!
//! This task is the other half. Every OSS-Fuzz project publishes the corpus its fuzzers have
//! accumulated, which is years of inputs selected for reaching new code under a checker that could
//! not see uninitialised reads, type confusion, intra object overflow or a use after free past the
//! quarantine. Those are the classes we add. The inputs exist, they are free, and the only new
//! thing between them and a finding is the monitor.
//!
//! # The harnesses are ours and the inputs are theirs
//!
//! A fuzz target is one function, `LLVMFuzzerTestOneInput`, and of the projects replayed here only
//! zstd and libjpeg-turbo ship their targets in the tarball the libraries table already points at.
//! libjpeg-turbo's is C++ in name only, and the rest live in a git tree or in the OSS-Fuzz
//! repository. So the
//! harnesses under `tests/replay` are written here, and each row names the target it stands in for
//! so that the two can be read side by side. Twenty lines is not what is being borrowed. The corpus
//! is.
//!
//! Written here means transliterated rather than reinvented, and the difference matters more than
//! it sounds. A corpus is a set of inputs selected for reaching new code in one particular harness,
//! so a harness that does something reasonable but different replays those inputs through a door
//! most of them were not chosen for. The brotli target is the case that taught this: the last byte
//! of an input picks whether the input is fed to the decoder whole or a few bytes at a time, and a
//! harness that always feeds it whole never suspends the state machine, which is where a streaming
//! decoder's bugs are. Each harness here says at the top which upstream file it is and what it
//! changed, and the changes are C89 declarations and the output ceiling and nothing else.
//!
//! zstd is the one that could have gone the other way and does not, so the reason is here rather
//! than only in the harness. Its targets are in the tarball, and the one this replays is three
//! files: the target, the thing that reads parameters off the back of the input, and the part of
//! the shared helpers those two call. A row names one harness, and changing that so one row can
//! name three would put a list of one in every other row to buy nothing. So zstd's is written out
//! like the rest, and the transliteration rule applies to it the same way.
//!
//! # libjpeg-turbo rather than libwebp
//!
//! The libraries table has libwebp and the replay does not, and the reason is the corpus rather than
//! the harness. A ClusterFuzz backup bucket answers 403 both for a zip nobody may read and for a zip
//! that is not there, so a wrong target name and a private corpus look the same from outside. libwebp
//! moved its targets to FuzzTest, whose targets OSS-Fuzz names `binary@Suite.Test`, and every name
//! that scheme and the older one give answered 403, while a known good zip in another project's
//! bucket answered 206 to the same probe. With no way to tell those apart there
//! is no corpus to pin.
//!
//! libjpeg-turbo sits in the same row of document 12's tier 1 table as libwebp, for the same reason,
//! which is that image decoders are where memory bugs have always been. Its decoder target's corpus
//! is public, the target is C, and the tarball carries it, so it takes libwebp's place here and the
//! libraries table gained it as a seventh row to have somewhere to build it from. If libwebp's
//! corpus becomes readable, the row it needs is the same few lines as every other.
//!
//! # The corpora are not in the tree
//!
//! Same argument the library sources get, and more so. They are somebody else's bytes, there are
//! tens of thousands of them per project, and putting them in a compiler's history would be
//! permanent. Each row names the zip under the public ClusterFuzz bucket its inputs come from and
//! the variable to point at an unpacked copy, and the task says so when the variable is not set.
//! What each row does keep is the day its zip was taken and how many inputs it had, because the
//! bucket is live, and two runs over two different corpora are two measurements rather than one.
//!
//! An input is a file in the directory the variable points at, and that is the whole rule. Some of
//! these zips carry a subdirectory beside the inputs, the lua and zstd ones each having a
//! `regressions` directory of testcases that once crashed something, and neither the count a row is
//! pinned at nor the walk the driver does goes into it. The reason to say so rather than to quietly
//! include it is that a pin is only worth writing down if two people unpacking the same zip arrive
//! at the same number.
//!
//! # One level, and here rather than in a container
//!
//! `-O0`, because a replay is about the inputs and not about the optimizer, and because `-O0` is
//! the level with the least between what the library wrote and what runs, so a report is about the
//! library. The two level sweep is what the libraries check is for.
//!
//! And on this machine rather than in a container, because the corpus is a directory outside the
//! build and the container only has the build mounted. A replay is run on the machine that has the
//! corpora on it, which is an x86-64 Linux machine, and the task says so on any other.
//!
//! # Objects with a line table, rather than assembly
//!
//! [`crate::libraries`] compiles to assembly so that the link belongs to whoever runs it and a
//! machine that cannot run an x86-64 program can still do everything up to the link. That reason
//! does not apply here, since a replay only happens on the machine the corpus is on and that
//! machine runs what it built, and against it is the one thing this task needs that assembly cannot
//! carry. `-g` writes a line table into an object, and the assembly printer writes no directives
//! for one, so a replay built out of assembly has no line table anywhere in it no matter what is
//! passed.
//!
//! Which matters because of what a report is. The descriptor a report is rendered from carries a
//! judgement, a class, a width and a program counter, and deliberately carries no file and no line,
//! for the reason `spec/safe-memory/06-instrumentation.md` section 6.5 gives: a compiler that ships
//! two line tables ships two answers that can disagree. So the one line table has to be in the
//! program, and until it was, every address in a report had to be taken to a disassembler and read
//! backwards, or the whole replay had to be built again with something that would say. It is built
//! with `-g` now, the linked program is left where it was run from, and the task says where it is
//! and checks that a line table really arrived rather than assuming it. Part of tamnd/rucc#1558.
//!
//! # Triage
//!
//! Every distinct report a corpus produces goes into one of the five buckets of
//! `spec/safe-memory/12-corpus-and-evidence.md` section 12.6, and each row carries that as data in
//! [`Target::triaged`]: the shape, the bucket, and the reason a person gave after reading the
//! source line behind it. A run is held to that list. A shape that is not on it fails the task,
//! since it is a report nobody has read. A shape in bucket 2, 3 or 4 fails it as well even though it
//! is listed, because those are bugs in this compiler and the spec makes them release blocking, so
//! the entry is a label on a failure rather than a way to make one go away. A listed shape that did
//! not arrive is said and is not a failure, because a person replaying a slice of a corpus, or a
//! corpus that has moved since its pin, should expect to miss some.
//!
//! A shape is what the reader can tell apart, which is the judgement and the width of the access.
//! That is coarser than a source line, and a second site with the same judgement and the same width
//! would pass as the first. The descriptor does not carry a site yet, so this is the finest key
//! there is, and the line table every build carries is how a person checks that the inputs behind a
//! listed shape are still at the site its entry was written about.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::libraries::{self, Project};
use crate::runner::TRIPLE;
use crate::safety::BANNER;
use crate::{Error, Result, cost, indent, root};

/// The level the replay builds at.
const LEVEL: &str = "-O0";

/// A corpus, the harness that eats it, and which project in the libraries table it belongs to.
struct Target {
    /// The row in [`crate::libraries::PROJECTS`] this replays, by name.
    ///
    /// By name rather than by a copy of the row, because the file list of a project is a hundred
    /// entries and two copies of it would drift the first time somebody bumped a version.
    project: &'static str,
    /// The harness under `tests/replay`, without its extension.
    harness: &'static str,
    /// The variable somebody points at an unpacked corpus.
    variable: &'static str,
    /// The OSS-Fuzz target this harness stands in for.
    upstream: &'static str,
    /// Where the inputs come from.
    corpus: &'static str,
    /// The day the zip behind [`Target::corpus`] was taken, and how many inputs it had.
    ///
    /// The bucket is live and the project keeps fuzzing, so the zip a person downloads next month
    /// is not the zip this row was written against. That is fine and it is worth saying out loud,
    /// because two runs over two different corpora are not comparable and a count that has moved a
    /// long way is the first sign of it. The task prints the pin beside what it actually found and
    /// carries on, since nothing here controls what Google publishes.
    pinned: (&'static str, usize),
    /// Every shape this corpus is known to produce, triaged.
    ///
    /// An empty list is a claim too, which is that any report at all from this corpus is new.
    triaged: &'static [Triaged],
}

/// One shape a corpus is known to produce, which of document 12.6's buckets it is in, and why.
struct Triaged {
    /// The judgement, so `1` for J1.
    judgement: u32,
    /// The width of the access in bytes, or nothing for a report about a pointer rather than an
    /// access, which is how J2 comes out.
    bytes: Option<u32>,
    /// The bucket a person put it in after reading the source line behind it.
    bucket: Bucket,
    /// What the line does and why that puts it in the bucket, with the issue the argument is on.
    why: &'static str,
}

/// The five buckets of `spec/safe-memory/12-corpus-and-evidence.md` section 12.6, and no sixth.
///
/// All five are here though only some are on any list, because an empty bucket is the state the
/// spec hopes for and not a reason to stop being able to say the name of it. Buckets 2 to 4 in
/// particular are bugs in this compiler, which get fixed rather than listed for long.
#[allow(dead_code, reason = "a bucket nothing is in yet is still one of the five")]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Bucket {
    /// A real bug in the project, with what upstream said about it.
    ///
    /// The spec asks for the answer to be recorded and says a rejection is the more informative of
    /// the two, because it usually means the model is wrong. So the field is the answer as it
    /// stands, including that nobody has asked yet.
    Project { upstream: &'static str },
    /// A false positive because the model in document 04 does not match what C permits.
    Model,
    /// A false positive because the instrumentation or the runtime is wrong.
    Implementation,
    /// A false positive because a boundary in document 10's table is declared wrong.
    Boundary,
    /// A construct C forbids that the project does on purpose, declared and counted.
    Exemption,
}

impl Bucket {
    /// The bucket's number in section 12.6.
    fn number(self) -> usize {
        match self {
            Bucket::Project { .. } => 1,
            Bucket::Model => 2,
            Bucket::Implementation => 3,
            Bucket::Boundary => 4,
            Bucket::Exemption => 5,
        }
    }

    /// Whether a shape in this bucket is a bug in this compiler, which the spec makes release
    /// blocking whether or not somebody has written it down.
    fn ours(self) -> bool {
        matches!(self, Bucket::Model | Bucket::Implementation | Bucket::Boundary)
    }
}

/// The targets, in the order they are run.
const TARGETS: &[Target] = &[
    Target {
        project: "brotli",
        harness: "brotli",
        variable: "RUCC_BROTLI_CORPUS",
        upstream: "brotli_decode_fuzzer",
        corpus: "https://storage.googleapis.com/brotli-backup.clusterfuzz-external.appspot.com/corpus/libFuzzer/brotli_decode_fuzzer/public.zip",
        pinned: ("2026-09-19", 4421),
        triaged: &[],
    },
    Target {
        project: "zlib",
        harness: "zlib",
        variable: "RUCC_ZLIB_CORPUS",
        upstream: "zlib_uncompress_fuzzer",
        corpus: "https://storage.googleapis.com/zlib-backup.clusterfuzz-external.appspot.com/corpus/libFuzzer/zlib_uncompress_fuzzer/public.zip",
        pinned: ("2026-09-21", 1551),
        triaged: &[],
    },
    Target {
        project: "sqlite",
        harness: "sqlite",
        variable: "RUCC_SQLITE_CORPUS",
        upstream: "ossfuzz",
        corpus: "https://storage.googleapis.com/sqlite3-backup.clusterfuzz-external.appspot.com/corpus/libFuzzer/sqlite3_ossfuzz/public.zip",
        pinned: ("2026-10-05", 23270),
        triaged: &[Triaged {
            judgement: 1,
            bytes: Some(8),
            bucket: Bucket::Project { upstream: "not reported yet" },
            why: "OP_Insert at sqlite3.c:102412 copies z and n out of a register nothing wrote. \
                  During VACUUM the transfer optimisation runs OP_RowCell and then OP_Insert with \
                  OPFLAG_PREFORMAT, OP_RowCell fills the btree's preformat buffer rather than the \
                  register, and initMemArray only ever set the register's flags. The value is \
                  dead, since OP_SeekEnd leaves seekResult at -1 and the branch that would use it \
                  is guarded by info.nSize != 0, but it is a read of a value nobody wrote all the \
                  same. tamnd/rucc#1542.",
        }],
    },
    Target {
        project: "lua",
        harness: "lua",
        variable: "RUCC_LUA_CORPUS",
        upstream: "fuzz_lua",
        corpus: "https://storage.googleapis.com/lua-backup.clusterfuzz-external.appspot.com/corpus/libFuzzer/lua_fuzz_lua/public.zip",
        pinned: ("2026-09-21", 18693),
        triaged: &[],
    },
    Target {
        project: "zstd",
        harness: "zstd",
        variable: "RUCC_ZSTD_CORPUS",
        upstream: "simple_decompress",
        corpus: "https://storage.googleapis.com/zstd-backup.clusterfuzz-external.appspot.com/corpus/libFuzzer/zstd_simple_decompress/public.zip",
        pinned: ("2026-10-05", 17088),
        triaged: &[Triaged {
            judgement: 2,
            bytes: None,
            bucket: Bucket::Project { upstream: "not reported yet" },
            why: "pointers zstd's decoder computes outside the buffer they were derived from, at \
                  about ten sites led by the sequence loop of ZSTD_decompressSequences_body, then \
                  BIT_initDStream, ZSTD_overlapCopy8 and HUF_DecompressFastArgs_init. C leaves \
                  that arithmetic undefined, and every input behind it decodes to the right \
                  answer, which makes this the evidence question 12 of \
                  spec/safe-memory/17-open-questions.md asks for. The census of sites is on \
                  tamnd/rucc#1499. The two J1 shapes this corpus also had are bucket 5, and the \
                  zstd row of the libraries table builds them away as a declared exemption.",
        }],
    },
    Target {
        project: "libjpeg-turbo",
        harness: "libjpeg-turbo",
        variable: "RUCC_LIBJPEG_TURBO_CORPUS",
        upstream: "libjpeg_turbo_fuzzer",
        corpus: "https://storage.googleapis.com/libjpeg-turbo-backup.clusterfuzz-external.appspot.com/corpus/libFuzzer/libjpeg-turbo_libjpeg_turbo_fuzzer/public.zip",
        pinned: ("2026-10-05", 7899),
        triaged: &[
            Triaged {
                judgement: 1,
                bytes: Some(1),
                bucket: Bucket::Project { upstream: "not reported yet" },
                why: "the upsamplers in jdsample.c reading rows of downsampled samples the IDCT \
                      never wrote, at six sites in int_upsample, h2v1_upsample, h2v2_upsample and \
                      the three fancy ones. Decoding all 131 inputs behind the two J1 shapes with \
                      the heap as malloc leaves it and then filled with two different patterns \
                      gives byte for byte the same output every time, so the values reach \
                      nothing, and MemorySanitizer reports a use rather than a read. \
                      tamnd/rucc#1499.",
            },
            Triaged {
                judgement: 1,
                bytes: Some(2),
                bucket: Bucket::Project { upstream: "not reported yet" },
                why: "the same upsampler reads as the one byte shape, on images whose samples \
                      are two bytes wide, and the same test over the heap shows the output does \
                      not depend on them either. tamnd/rucc#1499.",
            },
            Triaged {
                judgement: 2,
                bytes: None,
                bucket: Bucket::Project { upstream: "not reported yet" },
                why: "tj3Decompress16 at turbojpeg-mp.c:244 and 246 forming row pointers past \
                      the end of the output buffer. Upstream's harness sizes that buffer for one \
                      byte a sample when a lossless image has a precision below 8 and then calls \
                      tj3Decompress16 anyway, since it only tests for 8 and 12, and \
                      jpeg_read_scanlines rejects the precision before any row is written. So \
                      nothing is written through the pointers, and this target never decodes a \
                      lossless image of precision 2 to 7 or 9 to 11, though the 8 bit ones still \
                      reach decompress_libjpeg_fuzzer through the libjpeg API. tamnd/rucc#1499.",
            },
        ],
    },
];

/// What the driver prints before it hands an input over.
const INPUT: &str = "<<<input ";

/// What the driver prints when the child that took an input is finished.
const ENDED: &str = "<<<ended ";

/// Replays every corpus this machine has against the project it belongs to.
///
/// # Errors
///
/// [`Error::Failed`] when a project did not compile, a replay did not link or did not finish, or an
/// input killed the process that took it, with one line per thing that went wrong. [`Error::Io`]
/// when the replay could not be run at all, which is the compiler not building or this not being a
/// machine that runs x86-64 Linux programs.
pub(crate) fn replay() -> Result<()> {
    let host = crate::host_triple()?;
    if !(host.starts_with("x86_64-") && host.contains("linux")) {
        return Err(Error::Io(format!(
            "the replay reads a corpus from a directory outside the build and runs {TRIPLE} \
             programs over it, so it wants an x86-64 Linux machine with the corpora on it and this \
             is {host}."
        )));
    }

    let mut problems = Vec::new();
    let mut ran_any = false;
    for target in TARGETS {
        let project = row(target)?;
        let Some(source) = libraries::found(project) else {
            println!(
                "{}: no sources on this machine, so nothing was built. The libraries check says \
                 where to get them.",
                target.project
            );
            continue;
        };
        let Some(corpus) = corpus(target) else {
            println!(
                "{}: no corpus on this machine, so nothing was replayed. Unpack {} and point {} at \
                 it.",
                target.project, target.corpus, target.variable
            );
            continue;
        };
        ran_any = true;
        let here = count(&corpus);
        println!(
            "{}: {here} inputs from {}, standing in for {}",
            target.project,
            corpus.display(),
            target.upstream
        );
        let (day, then) = target.pinned;
        if here != then {
            println!(
                "{}: the pin is {then} inputs on {day}, so this corpus has moved by {} and the two \
                 runs are not the same measurement.",
                target.project,
                here.abs_diff(then)
            );
        }
        if let Some(jobs) = jobs() {
            println!(
                "{}: {jobs} drivers at once, each over a run of the corpus in the order one driver \
                 would take it, so the alarm is wall clock time on a busier machine.",
                target.project
            );
        }
        let work = build(target, project, &source)?;
        let out = run(&work, &corpus)?;
        read(target, &out, &mut problems);
        resolvable(target, &mut problems);
    }

    if !ran_any || problems.is_empty() {
        return Ok(());
    }
    Err(Error::Failed { task: "replay", problems })
}

/// The libraries row a target names, or a message saying the two lists have come apart.
fn row(target: &Target) -> Result<&'static Project> {
    libraries::PROJECTS.iter().find(|p| p.name == target.project).ok_or_else(|| {
        Error::Io(format!(
            "the replay has a row for {} and the libraries table does not, so there is nowhere to \
             get its sources from.",
            target.project
        ))
    })
}

/// Where an unpacked corpus is, if somebody said.
fn corpus(target: &Target) -> Option<PathBuf> {
    let said = std::env::var_os(target.variable)?;
    let path = PathBuf::from(said);
    path.is_dir().then_some(path)
}

/// How many drivers the script runs at once, when somebody asked for more than one.
///
/// The script reads the same variable and is what acts on it. This only says so, because a run
/// whose timeouts went up with its parallelism should say why on the line above the count.
fn jobs() -> Option<usize> {
    let said = std::env::var("RUCC_REPLAY_JOBS").ok()?;
    said.trim().parse::<usize>().ok().filter(|&jobs| jobs > 1)
}

/// How many files are in a corpus, for the line that says what is about to be run.
fn count(corpus: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(corpus) else { return 0 };
    entries.filter(|entry| entry.as_ref().is_ok_and(|e| e.path().is_file())).count()
}

/// Compiles the project, its harness and the driver, and leaves a directory the script can link.
fn build(target: &Target, project: &Project, source: &Path) -> Result<PathBuf> {
    let work = root().join("target").join("replay").join(target.project);
    if work.exists() {
        std::fs::remove_dir_all(&work)
            .map_err(|e| Error::Io(format!("could not clear {}: {e}", work.display())))?;
    }
    std::fs::create_dir_all(&work)
        .map_err(|e| Error::Io(format!("could not make {}: {e}", work.display())))?;

    let configured = libraries::configure(project, source, &work)?;
    let rucc = cost::compiler()?;
    let archive = crate::staticlib("rucc-safe-rt", TRIPLE)?;
    std::fs::copy(&archive, work.join("safe-rt.a"))
        .map_err(|e| Error::Io(format!("could not copy {}: {e}", archive.display())))?;

    let mut problems = Vec::new();
    for (file, stem) in files(target, project, source) {
        let mut command = Command::new(&rucc);
        command
            .args([
                "-c",
                "-g",
                &format!("--target={TRIPLE}"),
                "-fsafety=detect",
                LEVEL,
                crate::VERIFY,
            ])
            .arg("-I")
            .arg(source)
            .args(configured.iter().flat_map(|dir| [PathBuf::from("-I"), dir.clone()]))
            .args(
                project
                    .includes
                    .iter()
                    .flat_map(|inside| [Path::new("-I").to_path_buf(), source.join(inside)]),
            )
            .args(project.defines.iter().map(|define| format!("-D{define}")))
            .arg("-o")
            .arg(work.join(format!("{stem}.o")))
            .arg(&file);
        let out = command
            .current_dir(root())
            .output()
            .map_err(|e| Error::Io(format!("could not run the compiler: {e}")))?;
        if !out.status.success() {
            problems.push(format!(
                "{}: {} did not compile\n{}",
                target.project,
                file.display(),
                indent(String::from_utf8_lossy(&out.stderr).trim_end())
            ));
        }
    }
    if !problems.is_empty() {
        return Err(Error::Failed { task: "replay", problems });
    }

    std::fs::write(work.join("run.sh"), script(target, project))
        .map_err(|e| Error::Io(format!("could not write the script: {e}")))?;
    Ok(work)
}

/// Every file to compile and the name its object goes under.
///
/// The library's own files, then the harness under the name `harness` and the driver under the name
/// `driver`, so that the script can name the last two without knowing which project it is running.
fn files(target: &Target, project: &Project, source: &Path) -> Vec<(PathBuf, String)> {
    let here = root().join("tests").join("replay");
    let mut all: Vec<(PathBuf, String)> = project
        .sources
        .iter()
        .map(|name| (source.join(name), name.trim_end_matches(".c").replace('/', "-")))
        .collect();
    all.push((here.join(format!("{}.c", target.harness)), "harness".to_owned()));
    all.push((here.join("a-replay-driver.c"), "driver".to_owned()));
    all
}

/// The script that links the replay and runs it over the corpus.
///
/// The deduplicating posture, for the reason the libraries check uses it, and here it means one
/// report per site per input rather than per site: the driver gives every input a process of its
/// own, so the posture's memory of what it has already said goes with that process.
///
/// The corpus is the script's one argument. Writing it into the script rather than passing it
/// through the environment keeps the directory that ran with the build that ran it.
///
/// Everything the replay prints goes through `tee` as well as back here, because a few thousand
/// inputs is the better part of an hour and a task that shows nothing until it is finished is a
/// task a person kills. The copy sits beside the linked program and can be watched while it fills.
///
/// Where it links to is [`linked`] rather than a path written here, because the task reads the
/// program back afterwards to see whether a line table arrived in it and the two have to be the
/// same program.
///
/// `RUCC_REPLAY_JOBS` above one cuts the corpus into that many parts and runs one driver on each at
/// the same time, because the driver takes one input at a time and a corpus of eight thousand
/// images at ten a minute is the better part of a day on a machine with cores to spare. The parts
/// are runs of the corpus in the order the driver sorts it, which is `strcmp` order and so
/// `LC_ALL=C sort`, so the logs put back together part after part are the log one driver would
/// have written, and [`read`] cannot tell the two apart. The only thing that differs is a
/// `<<<replayed>>>` line per part rather than one, and nothing reads that line. Each part's log
/// sits in `parts` beside the program and can be watched while it fills, in place of the `tee`.
/// The list of inputs is the driver's rule written in shell: regular files, through a link if
/// there is one, and nothing whose name starts with a dot. The driver's alarm is wall clock time,
/// so a machine running more parts than it has idle cores will see more inputs run out of time,
/// which is why the default is one.
fn script(target: &Target, project: &Project) -> String {
    let stems: Vec<String> =
        files(target, project, Path::new("")).into_iter().map(|(_, stem)| stem).collect();
    let objects = stems.iter().map(|stem| format!("\"{stem}.o\"")).collect::<Vec<_>>().join(" ");
    let libraries = libraries::LIBRARIES.join(" ");
    format!(
        "\
#!/bin/sh
exec 2>/dev/null
out={out}
mkdir -p \"$out\"
RUCC_SAFETY_ON_ERROR=continue
export RUCC_SAFETY_ON_ERROR
jobs=${{RUCC_REPLAY_JOBS:-1}}
if gcc -no-pie {objects} safe-rt.a {libraries} -o \"$out/run\" >\"$out/link.log\" 2>&1; then
    if [ \"$jobs\" -gt 1 ] 2>/dev/null; then
        corpus=$(cd \"$1\" && pwd)
        rm -rf \"$out/parts\"
        mkdir -p \"$out/parts\"
        (cd \"$corpus\" && find -L . -maxdepth 1 -type f ! -name '.*') | sed 's|^\\./||' \\
            | LC_ALL=C sort >\"$out/parts/all\"
        split -n l/\"$jobs\" -d -a 3 \"$out/parts/all\" \"$out/parts/\"
        for part in \"$out\"/parts/[0-9][0-9][0-9]; do
            [ -s \"$part\" ] || continue
            mkdir \"$part.in\"
            while IFS= read -r name; do ln -s \"$corpus/$name\" \"$part.in/$name\"; done <\"$part\"
            {{ \"$out/run\" \"$part.in\" >\"$part.log\" 2>&1; echo \"$?\" >\"$part.status\"; }} &
        done
        wait
        status=0
        : >\"$out/out.log\"
        for part in \"$out\"/parts/[0-9][0-9][0-9]; do
            [ -s \"$part\" ] || continue
            cat \"$part.log\" >>\"$out/out.log\"
            [ \"$(cat \"$part.status\")\" = 0 ] || status=$(cat \"$part.status\")
        done
        printf '<<<status %s>>>\\n' \"$status\" >>\"$out/out.log\"
        cat \"$out/out.log\"
    else
        {{ \"$out/run\" \"$1\" 2>&1; printf '<<<status %s>>>\\n' \"$?\"; }} | tee \"$out/out.log\"
    fi
else
    cat \"$out/link.log\"
    printf '<<<status nolink>>>\\n'
fi
",
        out = linked(target).display()
    )
}

/// Where the replay is linked and run from.
///
/// Outside the build tree, because the build tree is what a container mounts and this is a program
/// built for the machine the corpus is on. It stays there after the task is finished, on purpose:
/// it is built with `-g` now, so an address out of a report is an `addr2line` away from a file and
/// a line for as long as nobody deletes it, which is the whole point of passing the flag.
fn linked(target: &Target) -> PathBuf {
    PathBuf::from(format!("/tmp/rucc-replay-{}", target.project))
}

/// How many line table rows the linked program carries.
///
/// Read with `readelf` rather than with anything of ours, for the reason [`crate::lines`] reads it
/// that way: a check on our own output that goes through our own reader is a check against
/// ourselves. Counted rather than looked for, because a line program with a header and no rows in
/// it is a section that exists and answers nothing, and the question here is whether an address can
/// be resolved.
///
/// None when `readelf` is not on the machine, which is not a failure. A replay that ran is worth
/// more than a check on the build that ran it, and the task says which of the two happened.
fn carried(program: &Path) -> Option<usize> {
    let out = Command::new("readelf")
        .arg("--debug-dump=decodedline")
        .arg(program)
        .output()
        .ok()
        .filter(|out| out.status.success())?;
    Some(count_rows(&String::from_utf8_lossy(&out.stdout)))
}

/// The rows in a decoded line table, which are the lines with a file, a number and an address on
/// them, and not the headings or the end of a sequence.
fn count_rows(text: &str) -> usize {
    text.lines()
        .filter(|line| {
            let mut parts = line.split_whitespace();
            let (Some(_name), Some(which), Some(at)) = (parts.next(), parts.next(), parts.next())
            else {
                return false;
            };
            let address = match at.strip_prefix("0x") {
                Some(rest) => u64::from_str_radix(rest, 16).is_ok(),
                None => at.parse::<u64>().is_ok(),
            };
            which.parse::<u32>().is_ok() && address
        })
        .count()
}

/// Runs the script over the corpus and hands back everything the replay printed.
fn run(work: &Path, corpus: &Path) -> Result<String> {
    let out = Command::new("sh")
        .arg("run.sh")
        .arg(corpus)
        .current_dir(work)
        .output()
        .map_err(|e| Error::Io(format!("could not run the replay: {e}")))?;
    if !out.status.success() {
        return Err(Error::Io(format!(
            "the replay did not run: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// What one input did.
struct Took {
    /// The input's name in the corpus.
    name: String,
    /// How the process that took it ended, which is `0` for a return, a signal number for a death,
    /// and nothing for an input that could not be read.
    ended: Option<i32>,
    /// Whether the process was killed rather than returning.
    killed: bool,
    /// Whether what killed it was the driver's own alarm, which is an input that ran out of time
    /// rather than an input that went wrong.
    timed: bool,
    /// Every distinct report it made, as the judgement and the width.
    said: Vec<(u32, Option<u32>)>,
}

/// Reads a replay's output and says what was wrong with it.
///
/// A link that did not happen and a run that did not finish are failures. An input that killed the
/// process that took it is a failure too, and it is the unambiguous one: whatever the monitor did
/// or did not say, a library that dies on an input from its own corpus is either a bug in the
/// library or a bug in this compiler, and both are worth stopping for.
///
/// The reports are counted, printed and held to the row's triage list by [`held`].
fn read(target: &Target, out: &str, problems: &mut Vec<String>) {
    if out.contains("<<<status nolink>>>") {
        problems.push(format!("{}: did not link\n{}", target.project, indent(out.trim_end())));
        return;
    }
    if !out.contains("<<<status 0>>>") {
        problems.push(format!(
            "{}: the replay did not finish\n{}",
            target.project,
            indent(out.trim_end())
        ));
        return;
    }

    let took = split(out);
    let died: Vec<&Took> = took.iter().filter(|one| one.killed).collect();
    let timed = took.iter().filter(|one| one.timed).count();
    let noisy = took.iter().filter(|one| !one.said.is_empty()).count();
    let mut shapes: Vec<(u32, Option<u32>, String, usize)> = Vec::new();
    for one in &took {
        for &(judgement, bytes) in &one.said {
            match shapes.iter_mut().find(|(j, b, _, _)| *j == judgement && *b == bytes) {
                Some((_, _, _, seen)) => *seen += 1,
                None => shapes.push((judgement, bytes, one.name.clone(), 1)),
            }
        }
    }

    println!(
        "{}: {} replayed, {} died, {timed} ran out of time, {noisy} reported, {} distinct shapes",
        target.project,
        took.len(),
        died.len(),
        shapes.len()
    );
    for (judgement, bytes, first, seen) in &shapes {
        let width = match bytes {
            Some(bytes) => format!("over {bytes} bytes"),
            None => "with no access width, so it is about a pointer rather than a read or a write"
                .to_owned(),
        };
        let bucket = match listed(target, *judgement, *bytes).map(|entry| entry.bucket) {
            Some(Bucket::Project { upstream }) => format!("bucket 1, upstream: {upstream}"),
            Some(bucket) => format!("bucket {}", bucket.number()),
            None => "not triaged".to_owned(),
        };
        println!(
            "{}: J{judgement} {width}, {seen} inputs, first at {first}, {bucket}",
            target.project
        );
    }
    held(target, &shapes, problems);

    for one in died {
        problems.push(format!(
            "{}: {} killed the process that took it, with signal {}. An input from a project's own \
             corpus that kills it is a bug in the library or a bug in this compiler.",
            target.project,
            one.name,
            one.ended.unwrap_or(0)
        ));
    }
}

/// The entry on a row's triage list for one shape, if there is one.
fn listed(target: &Target, judgement: u32, bytes: Option<u32>) -> Option<&'static Triaged> {
    target.triaged.iter().find(|entry| entry.judgement == judgement && entry.bytes == bytes)
}

/// A shape in a sentence, short enough to sit in the middle of one.
fn shape(judgement: u32, bytes: Option<u32>) -> String {
    match bytes {
        Some(bytes) => format!("J{judgement} over {bytes} bytes"),
        None => format!("J{judgement} with no access width"),
    }
}

/// Holds a run's shapes to the row's triage list and prints how many fell in each bucket.
///
/// A shape on no list is a problem, because it is a report nobody has read. A listed shape in
/// bucket 2, 3 or 4 is a problem as well, because those are bugs in this compiler. A listed shape
/// that did not arrive is said and nothing more, since a slice of a corpus or a corpus that has
/// moved since its pin can explain it, and the count beside the pin already says which.
fn held(target: &Target, shapes: &[(u32, Option<u32>, String, usize)], problems: &mut Vec<String>) {
    let mut buckets = [0_usize; 5];
    for (judgement, bytes, first, seen) in shapes {
        let what = shape(*judgement, *bytes);
        match listed(target, *judgement, *bytes) {
            None => problems.push(format!(
                "{}: {what} on {seen} inputs, first at {first}, is on no list. It is a report \
                 nobody has read yet, so it goes into one of the five buckets of section 12.6 of \
                 spec/safe-memory/12-corpus-and-evidence.md and onto the row before this passes.",
                target.project
            )),
            Some(entry) => {
                buckets[entry.bucket.number() - 1] += 1;
                if entry.bucket.ours() {
                    problems.push(format!(
                        "{}: {what} on {seen} inputs is bucket {}, which is a bug in this \
                         compiler and release blocking. {}",
                        target.project,
                        entry.bucket.number(),
                        entry.why
                    ));
                }
            }
        }
    }
    for entry in target.triaged {
        if !shapes.iter().any(|(j, b, _, _)| *j == entry.judgement && *b == entry.bytes) {
            println!(
                "{}: {} is on the list and did not arrive this time.",
                target.project,
                shape(entry.judgement, entry.bytes)
            );
        }
    }
    if !shapes.is_empty() {
        let counts: Vec<String> = buckets
            .iter()
            .enumerate()
            .map(|(at, count)| format!("{count} in {}", at + 1))
            .collect();
        println!("{}: shapes by bucket, {}", target.project, counts.join(", "));
    }
}

/// Says whether an address out of one of this replay's reports can be turned into a source line.
///
/// A descriptor carries a program counter and no file and no line, for the reason
/// `spec/safe-memory/06-instrumentation.md` section 6.5 gives, so the only way from one to the
/// other is the line table in the program. The build passes `-g` and the program is left where it
/// ran, and this is the part that checks the flag did something rather than trusting that it did.
/// A program with no rows in it is a failure and not a remark, because a corpus run whose findings
/// cannot be read is most of an hour spent to learn a number.
///
/// Nothing is said about a build that did not link, since [`read`] has already said it and a
/// missing program is that same news a second time.
fn resolvable(target: &Target, problems: &mut Vec<String>) {
    let program = linked(target).join("run");
    if !program.is_file() {
        return;
    }
    match carried(&program) {
        Some(0) => problems.push(format!(
            "{}: {} carries no line table, so an address out of a report cannot be turned into a \
             file and a line. The build passes -g, so this is a compiler bug rather than a missing \
             flag.",
            target.project,
            program.display()
        )),
        Some(rows) => println!(
            "{}: {rows} line table rows in {}, so addr2line resolves an address out of a report \
             without building any of this again.",
            target.project,
            program.display()
        ),
        None => println!(
            "{}: no readelf on this machine, so whether {} carries a line table was not checked.",
            target.project,
            program.display()
        ),
    }
}

/// Splits a replay's output into one entry per input.
fn split(out: &str) -> Vec<Took> {
    let mut all = Vec::new();
    for chunk in out.split(INPUT).skip(1) {
        let Some((name, rest)) = chunk.split_once(">>>\n") else { continue };
        let Some(end) = rest.find(ENDED) else { continue };
        let (body, tail) = rest.split_at(end);
        let tail = &tail[ENDED.len()..];
        let timed = tail.starts_with("timeout");
        let killed = tail.starts_with("signal ");
        let digits: String =
            tail.trim_start_matches("signal ").chars().take_while(|c| c.is_ascii_digit()).collect();
        all.push(Took {
            name: name.to_owned(),
            ended: digits.parse().ok(),
            killed,
            timed,
            said: shapes(body),
        });
    }
    all
}

/// Every report in one input's output, as the judgement number and the width of the access.
///
/// The width is optional because not every report is about an access. J2 is a pointer that left the
/// object it was derived from, and nothing has been read or written at the point it is refused, so
/// the report says where the pointer is and how far out it went and never says a number of bytes.
/// This used to require the width and skip a report that had none, which threw away every J2 there
/// was: on the zstd corpus that was 739 of the 946 inputs that reported anything, counted as zero,
/// and the summary said 223 inputs and two shapes where the truth was 946 and three.
fn shapes(body: &str) -> Vec<(u32, Option<u32>)> {
    let mut found = Vec::new();
    for chunk in body.split(BANNER).skip(1) {
        let Some(judgement) = libraries::number_after(chunk, "  judgement J") else {
            continue;
        };
        let bytes = libraries::number_before(chunk, " bytes at ");
        if !found.contains(&(judgement, bytes)) {
            found.push((judgement, bytes));
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every row has to name a project the libraries table has, because that is where its sources
    /// come from, and a harness file that exists, because a row naming one that does not is a row
    /// that fails at the first machine with the corpus on it rather than here.
    #[test]
    fn every_row_names_a_project_and_a_harness_that_are_there() {
        for target in TARGETS {
            assert!(row(target).is_ok(), "{}", target.project);
            let harness = root().join("tests").join("replay").join(format!("{}.c", target.harness));
            assert!(harness.is_file(), "{}", harness.display());
        }
    }

    /// The script names the objects the compile loop writes, and the two are written out in two
    /// places because one is shell and the other is Rust.
    #[test]
    fn the_script_names_every_file_the_compile_loop_writes() {
        for target in TARGETS {
            let project = row(target).expect("a row");
            let script = script(target, project);
            for (_, stem) in files(target, project, Path::new("")) {
                assert!(script.contains(&format!("\"{stem}.o\"")), "{stem}");
            }
        }
    }

    /// The script links to the directory the line table check reads back from. They are one path in
    /// two languages, and a change to either alone would have the task looking at a program that
    /// was never written.
    #[test]
    fn the_script_links_where_the_line_table_check_looks() {
        for target in TARGETS {
            let project = row(target).expect("a row");
            let script = script(target, project);
            let out = format!("out={}\n", linked(target).display());
            assert!(script.contains(&out), "{out}");
        }
    }

    /// A decoded line table is rows, headings and ends of sequences, and only the rows are rows.
    ///
    /// The first address of a program is printed with no `0x` in front of it, so a count that only
    /// took the prefixed form would miss the one row the front of a function is about. A row whose
    /// line column is a dash ends a sequence and is a position rather than a line.
    #[test]
    fn a_decoded_table_is_counted_by_its_rows_and_nothing_else() {
        let text = "CU: ./g.c:\n\
                    File name    Line number    Starting address    View    Stmt\n\
                    g.c                    3                   0               x\n\
                    g.c                    4                 0x5               x\n\
                    g.c                    -                0x1a\n";
        assert_eq!(count_rows(text), 2);
        assert_eq!(count_rows(""), 0);
    }

    /// The driver's two markers are what the reader splits on, so a change to either without a
    /// change here would read every input as no inputs.
    #[test]
    fn the_reader_and_the_driver_agree_about_the_markers() {
        let driver =
            std::fs::read_to_string(root().join("tests").join("replay").join("a-replay-driver.c"))
                .expect("the driver");
        assert!(driver.contains(&format!("{INPUT}%s>>>")), "{INPUT}");
        assert!(driver.contains(&format!("{ENDED}%d>>>")), "{ENDED}");
        assert!(driver.contains(&format!("{ENDED}signal %d>>>")), "{ENDED}");
        assert!(driver.contains(&format!("{ENDED}timeout>>>")), "{ENDED}");
    }

    /// One input that reported and one that died, read back out of what the driver would print.
    #[test]
    fn an_input_that_reported_and_one_that_died_are_told_apart() {
        let out = format!(
            "<<<input aaa>>>\n{BANNER}\n  judgement J1, an access\n  8 bytes at 0x1\n<<<ended \
             0>>>\n<<<input bbb>>>\n<<<ended signal 11>>>\n<<<status 0>>>\n"
        );
        let took = split(&out);
        assert_eq!(took.len(), 2);
        assert_eq!(took[0].name, "aaa");
        assert_eq!(took[0].said, vec![(1, Some(8))]);
        assert!(!took[0].killed);
        assert_eq!(took[1].name, "bbb");
        assert!(took[1].killed);
        assert_eq!(took[1].ended, Some(11));
    }

    /// A report that names no number of bytes is still a report.
    ///
    /// J2 is a pointer that went outside the object it came from and it is refused before anything
    /// is read or written, so there is no access to give a width to, and the only number in it is
    /// how far out the pointer went. Requiring a width threw all of those away, which on the zstd
    /// corpus was three quarters of the inputs that said anything.
    #[test]
    fn a_report_with_no_access_width_is_counted_rather_than_dropped() {
        let out = format!(
            "<<<input aaa>>>\n{BANNER}\n  judgement J2, a pointer derived from another\n  at \
             0x1\n  which no instance owns\n  which is 11 bytes before the start of it\n<<<ended \
             0>>>\n<<<status 0>>>\n"
        );
        let took = split(&out);
        assert_eq!(took.len(), 1);
        assert_eq!(took[0].said, vec![(2, None)]);
    }

    /// Two parts' logs put back together read as one driver's log over the same inputs.
    ///
    /// What sharding changes in the output is a `<<<replayed>>>` line at the end of every part
    /// rather than one at the end of the whole, so the line between two parts is the one place the
    /// reader could go wrong, by folding it into the input before it or by losing the input after.
    #[test]
    fn logs_from_parts_read_as_one_log() {
        let first = format!(
            "<<<input aaa>>>\n{BANNER}\n  judgement J1, an access\n  2 bytes at 0x1\n<<<ended \
             0>>>\n<<<input bbb>>>\n<<<ended timeout>>>\n<<<replayed 2>>>\n"
        );
        let second = "<<<input ccc>>>\n<<<ended 0>>>\n<<<replayed 1>>>\n<<<status 0>>>\n";
        let took = split(&format!("{first}{second}"));
        let names: Vec<&str> = took.iter().map(|one| one.name.as_str()).collect();
        assert_eq!(names, ["aaa", "bbb", "ccc"]);
        assert_eq!(took[0].said, vec![(1, Some(2))]);
        assert!(took[1].timed);
        assert!(took[2].said.is_empty() && !took[2].killed && !took[2].timed);
    }

    /// The script lists a corpus the way the driver does, so that the parts are runs of the order
    /// one driver would have taken and the log put back together is that driver's log.
    #[test]
    fn the_script_cuts_the_corpus_in_the_drivers_order() {
        let target = &TARGETS[0];
        let script = script(target, row(target).expect("a row"));
        assert!(script.contains("jobs=${RUCC_REPLAY_JOBS:-1}"));
        assert!(script.contains("find -L . -maxdepth 1 -type f ! -name '.*'"));
        assert!(script.contains("LC_ALL=C sort"));
        assert!(script.contains("split -n l/"));
        let driver =
            std::fs::read_to_string(root().join("tests").join("replay").join("a-replay-driver.c"))
                .expect("the driver");
        assert!(driver.contains("strcmp(*(const char *const *)a"));
        assert!(driver.contains("entry->d_name[0] == '.'"));
        assert!(driver.contains("S_ISREG"));
    }

    /// Every entry on a triage list is one shape once, says why, and names the issue the argument
    /// is on, since a reason nobody can follow back to its evidence is a suppression with a comment.
    #[test]
    fn every_triaged_shape_is_listed_once_with_a_reason() {
        for target in TARGETS {
            for (at, entry) in target.triaged.iter().enumerate() {
                let what = shape(entry.judgement, entry.bytes);
                assert!(entry.why.contains("tamnd/rucc#"), "{} {what}", target.project);
                let again = target.triaged[at + 1..]
                    .iter()
                    .any(|other| other.judgement == entry.judgement && other.bytes == entry.bytes);
                assert!(!again, "{} {what} twice", target.project);
            }
        }
    }

    /// A shape on no list fails, a listed shape in a bucket that is ours fails, a listed bucket 1
    /// shape passes, and a listed shape that did not arrive is not a failure.
    #[test]
    fn a_run_is_held_to_its_triage_list() {
        const ROW: Target = Target {
            project: "row",
            harness: "",
            variable: "",
            upstream: "",
            corpus: "",
            pinned: ("", 0),
            triaged: &[
                Triaged {
                    judgement: 1,
                    bytes: Some(8),
                    bucket: Bucket::Project { upstream: "not reported yet" },
                    why: "a dead read. tamnd/rucc#1.",
                },
                Triaged {
                    judgement: 1,
                    bytes: Some(4),
                    bucket: Bucket::Implementation,
                    why: "a wrong check. tamnd/rucc#2.",
                },
                Triaged {
                    judgement: 2,
                    bytes: None,
                    bucket: Bucket::Exemption,
                    why: "declared. tamnd/rucc#3.",
                },
            ],
        };
        let mut problems = Vec::new();
        held(&ROW, &[(1, Some(8), "aaa".to_owned(), 3)], &mut problems);
        assert!(problems.is_empty(), "{problems:?}");
        held(&ROW, &[(1, Some(2), "bbb".to_owned(), 1)], &mut problems);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("J1 over 2 bytes on 1 inputs, first at bbb, is on no list"));
        held(&ROW, &[(1, Some(4), "ccc".to_owned(), 1)], &mut problems);
        assert_eq!(problems.len(), 2);
        assert!(problems[1].contains("bucket 3"), "{}", problems[1]);
    }
}
