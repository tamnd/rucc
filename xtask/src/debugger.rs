//! Stops in a function under `gdb` and prints its locals, both compilers, the same program.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.4. See tamnd/rucc#1645.
//!
//! Everything this compiler says about a local is written for a debugger to read, and until
//! something reads it the only thing that has been checked is that the bytes are the bytes the
//! writer meant to write. A reader written beside the writer is the mistake `crates/rucc-stub`
//! made on s390x a version ago, where both halves held the same wrong belief and agreed with each
//! other, so the reader here is `gdb`, which is not ours and has no idea which compiler produced
//! the object it is reading.
//!
//! The answers are compared against the system compiler's for the same source rather than against
//! numbers written down here. A number written down is a number this file can be wrong about, and
//! a value that comes out of gcc at `-O0` is what a person sitting in front of the debugger
//! expects to see, which is the actual obligation.
//!
//! One program, built twice, differing only in which compiler produced the object holding the
//! function whose locals are being read. Everything around it is the system compiler's in both,
//! which is what keeps the difference down to the thing under test.
//!
//! There is one place the two are meant to differ, and it is checked rather than skipped. A local
//! nothing reads after a certain point has no value anywhere after it, and this compiler says the
//! variable is unavailable while gcc at `-O0` still prints the frame slot it gives every local.
//! [`DEAD`] holds that question the other way up: gcc has to print a value, which says the name is
//! a real one, and this compiler has to refuse, which says a dead variable comes out marked rather
//! than stale.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

use crate::runner::{Runner, TRIPLE};
use crate::{Error, Result, root};

/// What the two builds are called in the output and in the report.
const OURS: &str = "rucc";

/// The other one.
const THEIRS: &str = "gcc";

/// What the debugger is asked and why the answer is worth having.
///
/// A table rather than a list, because a question that comes back wrong is only useful next to the
/// reason it was asked, and the reason is the half that says which part of the compiler just broke.
const ASKED: [(&str, &str); 5] = [
    (
        "count",
        "a parameter, which is the one case where the entry the signature already wrote and the \
         location have to end up on the same entry rather than on two of that name, and which the \
         return still wants, so it has to survive a call to be printed at one",
    ),
    (
        "total",
        "a local the back end kept in a register and which is still wanted after the call, so \
         where it is at the breakpoint is somewhere the call did not clobber rather than the \
         register it was computed into",
    ),
    (
        "inner",
        "the one declared in the block the breakpoint is in, which is the whole of what a lexical \
         block buys: there is another local of that name in the block after this one and only the \
         scopes say which of them is being asked for",
    ),
    (
        "buf",
        "an array, which is in the frame whatever the lowering does with it, printed by value so \
         that the answer is the bytes rather than an address the two builds would never agree on",
    ),
    (
        "through the pointer",
        "a read through a pointer parameter, which is the location and the type together, since a \
         wrong type reads the right address wrongly and still prints a number",
    ),
];

/// What the debugger is asked where the two compilers are meant to differ, and why.
///
/// A local nothing reads after a certain point has no value anywhere after it, and this compiler
/// says so rather than pointing at the register the value used to be in. gcc at `-O0` gives every
/// local a frame slot it never leaves, so it can still print one, and that is the difference rather
/// than a defect on either side: `spec/11-asm-objects-debug.md` section 11.4 says why this compiler
/// will not put a local in a slot it would not otherwise have had just because `-g` was passed.
///
/// What is checked is both halves of that. gcc has to print a real value, which is what says the
/// name is a name and the question is a fair one, and this compiler has to say the value is
/// unavailable, which is what says a dead variable comes out marked rather than stale. An answer
/// here that agreed with gcc would be a stale register read that happened to still hold the number.
const DEAD: [(&str, &str); 1] = [(
    "early",
    "a local nothing reads after the line that adds it in, so at the breakpoint it is nowhere and \
     a debugger should say so rather than print whatever is in the register it was last in",
)];

/// What a debugger says when it has the name and cannot answer.
///
/// Used both ways. On the system compiler's answers it means the question was not a fair one, since
/// a question gcc cannot answer either is a question about the fixture. On this compiler's it is
/// the answer [`DEAD`] wants.
const NO_ANSWER: [&str; 4] =
    ["optimized out", "No symbol", "Cannot access", "value has been optimized"];

/// Builds the program both ways, runs the debugger over each, and holds the answers to each other.
///
/// # Errors
///
/// [`Error::Io`] when the compiler will not build, when the script will not run, or when there is
/// no `gdb` on the machine, which is a check that did not run rather than one that failed.
/// [`Error::Failed`] with one problem per answer that came back wrong or did not come back.
pub(crate) fn debugger() -> Result<()> {
    let work = build()?;
    let runner = Runner::find("the debugger differential")?;
    let printed = runner.run(&work, "the debugger differential")?;
    if printed.contains("gdb: missing") {
        return Err(Error::Io(format!(
            "the debugger differential needs gdb to read the locals back with and there is none \
             where the programs run, which is {runner}. Install gdb, or run this on a machine \
             that has one"
        )));
    }
    let said = read(&printed);
    let (ours, theirs) = (said.get(OURS).cloned().unwrap_or_default(), said.get(THEIRS).cloned());
    let Some(theirs) = theirs else {
        return Err(Error::Failed {
            task: "debugger",
            problems: vec![
                format!("{THEIRS} answered nothing, so there is nothing to compare against"),
                format!("what the run printed:\n{}", crate::indent(printed.trim_end())),
            ],
        });
    };

    let mut problems = Vec::new();
    for (what, why) in ASKED {
        let Some(want) = theirs.answers.get(what) else {
            problems.push(format!(
                "{what}: {THEIRS} said nothing, so there is nothing to hold {OURS} to. {why}"
            ));
            continue;
        };
        if NO_ANSWER.iter().any(|said| want.contains(said)) {
            problems.push(format!(
                "{what}: {THEIRS} answered {want}, which is the fixture's problem rather than this \
                 compiler's. {why}"
            ));
            continue;
        }
        match ours.answers.get(what) {
            Some(got) if got == want => {}
            Some(got) => {
                problems.push(format!("{what}: {OURS} said {got} and {THEIRS} said {want}. {why}"))
            }
            None => problems.push(format!("{what}: {OURS} said nothing. {why}")),
        }
    }

    for (what, why) in DEAD {
        match theirs.answers.get(what) {
            Some(want) if !NO_ANSWER.iter().any(|said| want.contains(said)) => {}
            Some(want) => problems.push(format!(
                "{what}: {THEIRS} answered {want}, so there is nothing saying the name is a name \
                 and the question a fair one. {why}"
            )),
            None => problems.push(format!("{what}: {THEIRS} said nothing at all. {why}")),
        }
        match ours.answers.get(what) {
            Some(got) if NO_ANSWER.iter().any(|said| got.contains(said)) => {}
            Some(got) => problems.push(format!(
                "{what}: {OURS} answered {got} where the value is dead, which is a place that is \
                 stale rather than one that is right. {why}"
            )),
            None => problems.push(format!("{what}: {OURS} said nothing. {why}")),
        }
    }

    // And which names are in scope there, which is the other half of what a scope is for. A name
    // this compiler puts in scope that gcc does not is a block that covers addresses it should not,
    // and the same name twice is two entries of one name under one parent, which is what the
    // lexical blocks were written to stop.
    for name in &ours.names {
        if !theirs.names.contains(name) {
            problems.push(format!(
                "{name} is in scope at the breakpoint for {OURS} and is not for {THEIRS}, so a \
                 scope covers addresses the name was not declared over"
            ));
        }
        if ours.names.iter().filter(|seen| *seen == name).count() > 1 {
            problems.push(format!(
                "{name} is in scope twice at once for {OURS}, so two locals of one name are two \
                 entries under one parent and a reader asked for the name picks one of them"
            ));
        }
    }

    if !problems.is_empty() {
        problems.push(format!("what the run printed:\n{}", crate::indent(printed.trim_end())));
        return Err(Error::Failed { task: "debugger", problems });
    }
    println!(
        "debugger: {} locals agree with {THEIRS}, {} marked dead rather than stale, {} names in \
         scope, {runner}",
        ASKED.len(),
        DEAD.len(),
        ours.names.len()
    );
    Ok(())
}

/// Compiles the function under test with this compiler and lays out the directory the runner is
/// pointed at.
fn build() -> Result<PathBuf> {
    let work = root().join("target").join("debugger");
    if work.exists() {
        std::fs::remove_dir_all(&work)
            .map_err(|e| Error::Io(format!("could not clear {}: {e}", work.display())))?;
    }
    std::fs::create_dir_all(&work)
        .map_err(|e| Error::Io(format!("could not make {}: {e}", work.display())))?;

    let rucc = crate::cost::compiler()?;
    let fixtures = root().join("tests").join("debugger");
    let source = fixtures.join("locals.c");
    let out = Command::new(&rucc)
        .args(["-c", &format!("--target={TRIPLE}"), "-O0", "-g", crate::VERIFY])
        .arg("-o")
        .arg(work.join("locals.o"))
        .arg(&source)
        .current_dir(root())
        .output()
        .map_err(|e| Error::Io(format!("could not run the compiler: {e}")))?;
    if !out.status.success() {
        return Err(Error::Failed {
            task: "debugger",
            problems: vec![format!(
                "locals.c did not compile\n{}",
                crate::indent(String::from_utf8_lossy(&out.stderr).trim_end())
            )],
        });
    }
    // Copied rather than read from the tree, because the runner is given one directory and the
    // container mounts it read only.
    for name in ["locals.c", "main.c", "script.gdb"] {
        let from = fixtures.join(name);
        std::fs::copy(&from, work.join(name))
            .map_err(|e| Error::Io(format!("could not copy {}: {e}", from.display())))?;
    }
    std::fs::write(work.join("run.sh"), SCRIPT)
        .map_err(|e| Error::Io(format!("could not write the script: {e}")))?;
    Ok(work)
}

/// What the runner runs.
///
/// Two programs out of one source, differing only in which compiler produced the object holding
/// `examine`. The system compiler builds `main.c` and does the linking in both, so the C library,
/// the entry and the `stop` the breakpoint is on are the same bytes either way.
///
/// A machine with no `gdb` says so and exits zero, because a check that could not run is a
/// different thing from a check that failed and the caller is the one that decides which to
/// report. Everything is written under `/tmp` because the directory this reads from is mounted
/// read only.
const SCRIPT: &str = "\
#!/bin/sh
out=/tmp/debugger
mkdir -p \"$out\"
command -v gdb > /dev/null 2>&1 || { echo 'gdb: missing'; exit 0; }
gcc -O0 -g -c locals.c -o \"$out/gcc.o\" || exit 1
gcc -O0 -g -o \"$out/prog-gcc\" \"$out/gcc.o\" main.c || exit 1
gcc -O0 -g -o \"$out/prog-rucc\" locals.o main.c || exit 1
for which in rucc gcc; do
  gdb -batch -nx -x script.gdb \"$out/prog-$which\" 2>&1 | sed \"s/^/$which /\"
done
";

/// What one build's debugger session said.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Said {
    /// One answer per question in [`ASKED`], by the name the script announced it under.
    answers: BTreeMap<String, String>,
    /// The names the debugger reported in scope at the breakpoint, in the order it reported them,
    /// which is the order that says whether one of them arrived twice.
    names: Vec<String>,
}

/// Turns what the script printed into one of those per build.
///
/// The session announces each question before asking it, so a line naming a question puts every
/// answer after it under that name until the next one. An answer that never came back is a name
/// with nothing under it rather than the next answer along, which is what would happen if the
/// answers were read by the order they arrived in.
fn read(printed: &str) -> BTreeMap<String, Said> {
    let mut out: BTreeMap<String, Said> = BTreeMap::new();
    let mut asking: BTreeMap<String, Option<String>> = BTreeMap::new();
    let mut naming: BTreeMap<String, bool> = BTreeMap::new();
    for line in printed.lines() {
        let Some((which, rest)) = line.split_once(' ') else { continue };
        let said = out.entry(which.to_owned()).or_default();
        let rest = rest.trim_end();
        if let Some(what) = rest.strip_prefix("ask ") {
            asking.insert(which.to_owned(), Some(what.to_owned()));
            naming.insert(which.to_owned(), false);
            continue;
        }
        if rest == "names" {
            asking.insert(which.to_owned(), None);
            naming.insert(which.to_owned(), true);
            continue;
        }
        if rest == "done" {
            asking.insert(which.to_owned(), None);
            naming.insert(which.to_owned(), false);
            continue;
        }
        if naming.get(which).copied().unwrap_or(false) {
            // `name = value`, and anything else there is the debugger saying it has nothing to
            // report, which is a fact about the frame rather than a name in it.
            if let Some((name, _)) = rest.split_once(" = ") {
                let name = name.trim();
                if !name.is_empty() && !name.contains(' ') {
                    said.names.push(name.to_owned());
                }
            }
            continue;
        }
        // A printed value is `$1 = ...`, and the number is the debugger's own counter, which says
        // nothing about which question was asked.
        let Some(what) = asking.get(which).and_then(Clone::clone) else { continue };
        let Some(value) = rest.strip_prefix('$') else { continue };
        if let Some((_, value)) = value.split_once(" = ") {
            said.answers.insert(what, value.trim().to_owned());
            asking.insert(which.to_owned(), None);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A whole session, read the way the check reads it.
    #[test]
    fn each_answer_is_read_by_the_name_the_question_was_announced_under() {
        let printed = "\
rucc Breakpoint 1 at 0x1139: file main.c, line 13.
rucc #1  0x0000000000401146 in examine (count=7, label=0x402010) at locals.c:22
rucc ask count
rucc $1 = 7
rucc ask total
rucc $2 = 38
rucc ask buf
rucc $3 = {7, 8, 9, 10}
rucc names
rucc inner = 24
rucc total = 38
rucc buf = {7, 8, 9, 10}
rucc done
";
        let said = read(printed);
        let ours = &said["rucc"];
        assert_eq!(ours.answers["count"], "7");
        assert_eq!(ours.answers["total"], "38");
        assert_eq!(ours.answers["buf"], "{7, 8, 9, 10}");
        assert_eq!(ours.names, vec!["inner", "total", "buf"]);
    }

    /// A question the debugger could not answer leaves that question empty rather than taking the
    /// answer to the question after it.
    #[test]
    fn a_question_with_no_answer_does_not_take_the_next_ones() {
        let printed = "\
gcc ask count
gcc No symbol \"count\" in current context.
gcc ask total
gcc $1 = 38
gcc done
";
        let said = read(printed);
        let theirs = &said["gcc"];
        assert_eq!(theirs.answers.get("count"), None);
        assert_eq!(theirs.answers["total"], "38");
    }

    /// Two builds in one stream, each kept to itself.
    #[test]
    fn the_two_builds_do_not_read_each_others_answers() {
        let printed = "\
rucc ask total
gcc ask total
gcc $1 = 38
rucc $1 = 99
rucc done
gcc done
";
        let said = read(printed);
        assert_eq!(said["rucc"].answers["total"], "99");
        assert_eq!(said["gcc"].answers["total"], "38");
    }

    /// What the debugger says when a frame has nothing in it is not a name.
    #[test]
    fn a_frame_with_nothing_in_it_reports_no_names() {
        let printed = "\
rucc names
rucc No symbol table info available.
rucc done
";
        assert_eq!(read(printed)["rucc"].names, Vec::<String>::new());
    }
}
