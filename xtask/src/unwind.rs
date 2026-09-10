//! Walks a stack through frames this compiler wrote and counts what came back.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.4.
//!
//! tamnd/rucc#767 was that there was no unwind table at all, and what it cost was a backtrace that
//! stopped at the first frame this compiler produced. Two frames where the system compiler's build
//! of the same source gave six. Nothing in the compiler noticed, because nothing in the compiler
//! walks a stack: the table is written for other programs to read and the only way to find out
//! whether it is right is to let one read it.
//!
//! So this runs the program from that issue. The count is compared against the system compiler's
//! for the same source rather than against a number written down here, because how deep a program's
//! own entry is is the C library's business and differs between one and the next.
//!
//! Reading the table back with a reader written beside the writer is the mistake `crates/rucc-stub`
//! made on s390x a version ago: both halves held the same wrong belief and agreed with each other.
//! So the second half of this is `readelf`, which is not ours, and the first half is a program that
//! does not read the table at all and only knows how far it got.
//!
//! Both halves of the compiler's own output are run. The object file is what an ordinary build
//! produces, and the assembly listing assembled here is what a build that goes through `as`
//! produces, and section 11.1 asks that those two cannot disagree.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

use crate::runner::{Runner, TRIPLE};
use crate::{Error, Result, root};

/// The two ways the compiler's own answer reaches the linker.
const OURS: [&str; 2] = ["the object this compiler wrote", "its listing assembled here"];

/// What the system compiler's build of the same source is called in the output.
const THEIRS: &str = "the system compiler";

/// What the table has to say about itself, read back by something that is not ours.
///
/// A table rather than a file beside the fixture, because every row is an assertion with a reason
/// and the reason is the useful half. A row that fails is quoted back with the sentence next to it.
const WANTED: [(&str, &str, &str); 7] = [
    (
        "headers in the table",
        "1",
        "one header per object, holding what every function on the target starts out with, which \
         is what keeps a record down to the handful of bytes that say what changed",
    ),
    (
        "records in the table",
        "2",
        "one per function, including a function with nothing to say, because an unwinder that \
         lands on an address no record covers cannot tell an undescribed function from a described \
         one and has to stop",
    ),
    (
        "the augmentation",
        "\"zR\"",
        "the header says its length so a reader that does not know the rest can skip it, and says \
         that a record spells its function's address as a distance",
    ),
    (
        "the return address column",
        "16",
        "x86-64 has sixteen general registers and the return address is in none of them, so it is \
         given the next column up",
    ),
    (
        "the code alignment factor",
        "1",
        "instructions on this machine are not all one length, so there is no larger number every \
         distance in a record is a multiple of",
    ),
    (
        "the data alignment factor",
        "-8",
        "a slot is eight bytes and every one of them is below the end of the frame, so dividing by \
         a negative is what makes the number written for one positive and a byte shorter",
    ),
    (
        "a record that says a register was saved",
        "yes",
        "the rows survive from the prologue that wrote them to the bytes in the file, which is the \
         half of this that a frame count alone would not catch",
    ),
];

/// Builds the fixture, runs the three programs, and holds what came back to [`WANTED`].
///
/// # Errors
///
/// [`Error::Io`] when the compiler will not build or the script will not run, and
/// [`Error::Failed`] with one problem per answer that came back wrong or did not come back.
pub(crate) fn unwind() -> Result<()> {
    let work = build()?;
    let runner = Runner::find("the unwind table check")?;
    let printed = runner.run(&work, "the unwind table check")?;
    let said = read(&printed);

    let mut problems = Vec::new();
    let theirs = said.get(&format!("frames through {THEIRS}")).cloned();
    match &theirs {
        // A count of one would mean the system compiler's own build stopped at the first frame,
        // which would be a broken container rather than a result to compare against.
        Some(count) if count != "1" => {
            for ours in OURS {
                let what = format!("frames through {ours}");
                match said.get(&what) {
                    Some(got) if got == count => {}
                    Some(got) => problems.push(format!(
                        "{what}: {got}, and {THEIRS} got {count} for the same source. The walk \
                         stops at the first frame nothing describes"
                    )),
                    None => problems.push(format!("{what}: nothing said it")),
                }
            }
        }
        _ => problems.push(format!(
            "{THEIRS} walked {} frames, so there is nothing to compare against",
            theirs.as_deref().unwrap_or("no")
        )),
    }
    for (what, want, why) in WANTED {
        match said.get(what) {
            Some(got) if got == want => {}
            Some(got) => problems.push(format!("{what}: {got}, wanted {want}. {why}")),
            None => problems.push(format!("{what}: nothing said it. {why}")),
        }
    }
    if !problems.is_empty() {
        // The whole output, because a link that failed is one message explaining every line at
        // once and reporting each of them separately would bury it.
        problems.push(format!("what the run printed:\n{}", crate::indent(printed.trim_end())));
        return Err(Error::Failed { task: "unwind", problems });
    }
    let frames = theirs.unwrap_or_default();
    println!("unwind: {} frames both ways, {} checks on the table, {runner}", frames, WANTED.len());
    Ok(())
}

/// Compiles the fixture with this compiler, both ways, and lays out the directory the runner is
/// pointed at.
fn build() -> Result<PathBuf> {
    let work = root().join("target").join("unwind");
    if work.exists() {
        std::fs::remove_dir_all(&work)
            .map_err(|e| Error::Io(format!("could not clear {}: {e}", work.display())))?;
    }
    std::fs::create_dir_all(&work)
        .map_err(|e| Error::Io(format!("could not make {}: {e}", work.display())))?;

    let status = Command::new("cargo")
        .args(["build", "-q", "--release", "-p", "rucc"])
        .current_dir(root())
        .status()
        .map_err(|e| Error::Io(format!("could not run cargo: {e}")))?;
    if !status.success() {
        return Err(Error::Io("the compiler did not build".to_owned()));
    }
    let rucc = root().join("target").join("release").join("rucc");

    let fixtures = root().join("tests").join("unwind");
    let source = fixtures.join("deep.c");
    for (flag, name) in [("-c", "deep.o"), ("-S", "deep.s")] {
        let out = Command::new(&rucc)
            .args([flag, &format!("--target={TRIPLE}"), "-O1"])
            .arg("-o")
            .arg(work.join(name))
            .arg(&source)
            .current_dir(root())
            .output()
            .map_err(|e| Error::Io(format!("could not run the compiler: {e}")))?;
        if !out.status.success() {
            return Err(Error::Failed {
                task: "unwind",
                problems: vec![format!(
                    "deep.c did not compile to {name}\n{}",
                    crate::indent(String::from_utf8_lossy(&out.stderr).trim_end())
                )],
            });
        }
    }
    // Copied rather than mounted from the tree, because the runner is given one directory and the
    // container mounts it read only.
    for name in ["deep.c", "top.c"] {
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
/// Three programs out of one source, differing only in what produced the object the walk goes
/// through. Everything is written under `/tmp` because the directory this reads from is mounted
/// read only.
const SCRIPT: &str = "\
#!/bin/sh
out=/tmp/unwind
mkdir -p \"$out\"
as --64 -o \"$out/asm.o\" deep.s || exit 1
gcc -O1 -c deep.c -o \"$out/gcc.o\" || exit 1
gcc -O1 top.c deep.o -o \"$out/prog-obj\" -rdynamic || exit 1
gcc -O1 top.c \"$out/asm.o\" -o \"$out/prog-asm\" -rdynamic || exit 1
gcc -O1 top.c \"$out/gcc.o\" -o \"$out/prog-gcc\" -rdynamic || exit 1
\"$out/prog-obj\" | sed 's/^frames: /frames through the object this compiler wrote: /'
\"$out/prog-asm\" | sed 's/^frames: /frames through its listing assembled here: /'
\"$out/prog-gcc\" | sed 's/^frames: /frames through the system compiler: /'
readelf --debug-dump=frames deep.o | sed 's/^/table /'
";

/// Turns what the script printed into one answer per question.
///
/// The `readelf` lines are folded in here rather than compared where they are, so that the table
/// above is one list and a fact the dump does not carry reads the same way as one that came back
/// wrong. What is counted is counted rather than matched line by line, because the ops inside a
/// record are the code generator's business and this is about the shape of the file.
fn read(printed: &str) -> BTreeMap<String, String> {
    let mut said = BTreeMap::new();
    let mut headers = 0;
    let mut records = 0;
    let mut saved = false;
    for line in printed.lines() {
        let Some(rest) = line.strip_prefix("table ") else {
            if let Some((what, answer)) = line.split_once(": ") {
                said.insert(what.to_owned(), answer.trim().to_owned());
            }
            continue;
        };
        let rest = rest.trim();
        if rest.ends_with(" CIE") {
            headers += 1;
        }
        if rest.contains(" FDE cie=") {
            records += 1;
        }
        // Not in the header, where the return address is described the same way and would be a
        // register nothing saved.
        if records > 0 && rest.starts_with("DW_CFA_offset:") {
            saved = true;
        }
        for (heading, what) in [
            ("Augmentation:", "the augmentation"),
            ("Return address column:", "the return address column"),
            ("Code alignment factor:", "the code alignment factor"),
            ("Data alignment factor:", "the data alignment factor"),
        ] {
            if let Some(value) = rest.strip_prefix(heading) {
                said.insert(what.to_owned(), value.trim().to_owned());
            }
        }
    }
    said.insert("headers in the table".to_owned(), headers.to_string());
    said.insert("records in the table".to_owned(), records.to_string());
    let saved = if saved { "yes" } else { "no" };
    said.insert("a record that says a register was saved".to_owned(), saved.to_owned());
    said
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The dump and the printed lines both land in the same map, and the counts are counts.
    #[test]
    fn a_dump_and_a_printed_line_both_land_in_the_same_map() {
        let printed = "\
frames through the system compiler: 7
table Contents of the .eh_frame section:
table 00000000 0000000000000014 00000000 CIE
table   Version:               1
table   Augmentation:          \"zR\"
table   Code alignment factor: 1
table   Data alignment factor: -8
table   Return address column: 16
table   DW_CFA_offset: r16 (rip) at cfa-8
table 00000018 0000000000000024 0000001c FDE cie=00000000 pc=0000000000000000..0000000000000040
table   DW_CFA_def_cfa_offset: 16
table   DW_CFA_offset: r3 (rbx) at cfa-16
table 00000040 0000000000000014 00000044 FDE cie=00000000 pc=0000000000000040..0000000000000050
";
        let said = read(printed);
        assert_eq!(said["frames through the system compiler"], "7");
        assert_eq!(said["headers in the table"], "1");
        assert_eq!(said["records in the table"], "2");
        assert_eq!(said["the augmentation"], "\"zR\"");
        assert_eq!(said["the return address column"], "16");
        assert_eq!(said["the data alignment factor"], "-8");
        assert_eq!(said["a record that says a register was saved"], "yes");
    }

    /// The saved register the header describes is the return address, which every function on the
    /// target has and no prologue wrote down. Counting it would make the check pass on an object
    /// whose records are all empty.
    #[test]
    fn the_return_address_in_the_header_is_not_a_saved_register() {
        let printed = "\
table 00000000 0000000000000014 00000000 CIE
table   DW_CFA_offset: r16 (rip) at cfa-8
table 00000018 0000000000000014 0000001c FDE cie=00000000 pc=0000000000000000..0000000000000010
";
        let said = read(printed);
        assert_eq!(said["a record that says a register was saved"], "no");
        assert_eq!(said["records in the table"], "1");
    }
}
