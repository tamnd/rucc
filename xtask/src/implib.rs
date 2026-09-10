//! Every import library the writer produces, held against the one `llvm-dlltool` produces.
//!
//! `spec/cross-compile/09-libc-stubs.md` section 9.4 is a file format with no specification worth the
//! name. Microsoft documents the 20 byte record and says nothing about which of the five name types a
//! given `.def` line turns into, nothing about the two linker members of the archive, and nothing about
//! the three objects of boilerplate every import library carries. All of that is in the two tools that
//! write these files, and a writer built from the documentation alone produces something a linker
//! accepts and a loader then cannot resolve.
//!
//! So this is not a round trip and not a reader's opinion. It is byte equality with `llvm-dlltool` on
//! the same `.def` file for the same architecture. That is a harsh test and it is the right one here:
//! the file has no slack in it, every field is either the same as the reference or wrong, and the three
//! rules this writer would otherwise have guessed at were all found by a comparison exactly like this
//! one failing. A library that differs from the reference by one byte is reported with the offset, the
//! member it lands in and both values, because that is what turns a comparison into a diagnosis.
//!
//! `llvm-readobj` is asked as well where it is installed, and only about our file. A comparison that
//! passes says the two writers agree, and it would still pass if both of them were being read by
//! nothing, so something that parses an import library for a living is asked to list the symbols and
//! the records.
//!
//! Neither tool is a build dependency and neither is needed to build or test the compiler. GNU
//! `dlltool` is deliberately not accepted as the reference: it names its members after the output file
//! and defines `_head_<file>` where LLVM defines `__IMPORT_DESCRIPTOR_<dll>`, so its output is a
//! different file by design and `crates/rucc-stub/src/coff.rs` says which of the two it follows and why.

use std::path::PathBuf;
use std::process::Command;

use crate::{Error, Result, root};

/// What the example had to say for itself.
struct Emitted {
    /// One per library it wrote.
    written: Vec<Written>,
    /// One per library it would not write, already worded for a person to read.
    refused: Vec<String>,
}

/// One library the example wrote, and what it would take to write it again.
struct Written {
    /// The case, as `sample`.
    name: String,
    /// The target it was written for.
    tuple: String,
    /// The `.def` the example wrote beside it, which is what the reference is built from.
    source: PathBuf,
    /// Our library.
    path: PathBuf,
    /// The arguments `llvm-dlltool` needs to write the same thing, as the example spelled them.
    machine: Vec<String>,
}

/// Writes every case for every Windows target and compares each one with `llvm-dlltool`.
///
/// # Errors
///
/// [`Error::Io`] when `llvm-dlltool` is not installed, when the example will not run, or when a file
/// it said it wrote is not there, and [`Error::Failed`] with one problem per library that came out
/// different from the reference.
pub(crate) fn implib() -> Result<()> {
    let Some(dlltool) = find("llvm-dlltool") else {
        return Err(Error::Io(
            "no llvm-dlltool on this machine, and this check is byte equality with it, so there is \
             nothing to compare against. It ships with llvm: `apt install llvm` or \
             `brew install llvm`."
                .to_owned(),
        ));
    };

    let into = root().join("target").join("implib");
    let Emitted { written, refused } = emit(&into)?;
    if written.is_empty() {
        return Err(Error::Io("the implib example wrote no libraries at all".to_owned()));
    }
    println!("xtask: {} import libraries, compared with {}", written.len(), dlltool.display());
    for refusal in &refused {
        // Not a failure. Section 9.1 argues for refusing over guessing, and the argument is worth
        // nothing if the refusals are invisible.
        println!("xtask: the writer refused {refusal}");
    }

    let mut problems = Vec::new();
    for one in &written {
        problems.extend(compare(&dlltool, one)?);
    }
    match find("llvm-readobj") {
        Some(readobj) => {
            for one in &written {
                problems.extend(read(&readobj, one)?);
            }
            println!("xtask: and read back by {}", readobj.display());
        }
        // Worth saying rather than worth failing over. The comparison above is the check and it ran.
        None => println!("xtask: no llvm-readobj here, so nothing read our files back"),
    }

    if problems.is_empty() {
        println!("xtask: every import library is the same file llvm-dlltool writes");
        return Ok(());
    }
    Err(Error::Failed { task: "implib", problems })
}

/// The first of these that is there and answers.
///
/// A mac keeps the homebrew llvm out of the way of the system tools and a linux distribution puts it
/// under a directory named for its version, so both places are looked in. This is the same search
/// `stubs` does, with the tool name as the one thing that differs.
fn find(tool: &str) -> Option<PathBuf> {
    let mut versioned: Vec<PathBuf> = std::fs::read_dir("/usr/lib")
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name().is_some_and(|name| name.to_string_lossy().starts_with("llvm-"))
        })
        .map(|path| path.join("bin").join(tool))
        .collect();
    versioned.sort();
    versioned.reverse();

    let named = [
        PathBuf::from(tool),
        PathBuf::from("/opt/homebrew/opt/llvm/bin").join(tool),
        PathBuf::from("/usr/local/opt/llvm/bin").join(tool),
    ];
    named.into_iter().chain(versioned).find(|path| {
        // `llvm-dlltool` has no `--version` that exits zero, so an empty run is what tells us it is
        // there: it prints its usage and fails, which is a different failure from not existing.
        Command::new(path).arg("--version").output().is_ok()
    })
}

/// Runs the example and reads the lines it prints.
///
/// The example is where the cases live, which keeps this file from holding a second copy of what goes
/// in an import library. xtask depends on no crate in the workspace, so what it checks arrives as
/// output rather than as a call.
fn emit(into: &PathBuf) -> Result<Emitted> {
    // A library from an earlier run that the example no longer writes would still be compared, and
    // would pass, so the directory starts empty.
    if into.exists() {
        std::fs::remove_dir_all(into)
            .map_err(|e| Error::Io(format!("could not clear {}: {e}", into.display())))?;
    }
    let out = Command::new("cargo")
        .args(["run", "-q", "-p", "rucc-stub", "--example", "implib"])
        .arg(into)
        .current_dir(root())
        .output()
        .map_err(|e| Error::Io(format!("could not run cargo: {e}")))?;
    if !out.status.success() {
        return Err(Error::Io(format!(
            "the implib example failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }

    let mut written = Vec::new();
    let mut refused = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let fields: Vec<&str> = line.split('|').collect();
        match fields.as_slice() {
            [name, tuple, answer] => match answer.strip_prefix("refused: ") {
                Some(why) => refused.push(format!("{name} for {tuple}: {why}")),
                None => return Err(Error::Io(format!("implib said something unexpected: {line}"))),
            },
            [name, tuple, source, path, machine] => written.push(Written {
                name: (*name).to_owned(),
                tuple: (*tuple).to_owned(),
                source: PathBuf::from(source),
                path: PathBuf::from(path),
                machine: machine.split_whitespace().map(str::to_owned).collect(),
            }),
            _ => return Err(Error::Io(format!("implib said something unexpected: {line}"))),
        }
    }
    Ok(Emitted { written, refused })
}

/// Builds the same library with `llvm-dlltool` and compares the bytes.
fn compare(dlltool: &PathBuf, one: &Written) -> Result<Vec<String>> {
    let reference = one.path.with_extension("reference.a");
    let _ = std::fs::remove_file(&reference);
    let out = Command::new(dlltool)
        .args(&one.machine)
        .arg("-d")
        .arg(&one.source)
        .arg("-l")
        .arg(&reference)
        .output()
        .map_err(|e| Error::Io(format!("could not run llvm-dlltool: {e}")))?;
    if !out.status.success() {
        return Ok(vec![format!(
            "{}: llvm-dlltool would not build the reference for {}: {}",
            one.what(),
            one.source.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )]);
    }

    let theirs = std::fs::read(&reference)
        .map_err(|e| Error::Io(format!("could not read {}: {e}", reference.display())))?;
    let ours = std::fs::read(&one.path)
        .map_err(|e| Error::Io(format!("could not read {}: {e}", one.path.display())))?;
    if ours == theirs {
        return Ok(Vec::new());
    }

    let mut problem = difference(&one.what(), &ours, &theirs);
    problem.push_str(&format!("\n  ours {}\n  theirs {}", one.path.display(), reference.display()));
    Ok(vec![problem])
}

/// What to say about two libraries that are not the same file.
///
/// The two lengths and then the first byte they disagree about, named by the member it falls in. Every
/// difference this check has ever found was read this way, and none of them needed more: a length that
/// is one out is a member that was not padded, a byte that differs by the width of a name is a string
/// table, and a single byte in an object is a field.
fn difference(what: &str, ours: &[u8], theirs: &[u8]) -> String {
    let mut problem =
        format!("{what}: ours is {} bytes and llvm-dlltool's is {}", ours.len(), theirs.len());
    match ours.iter().zip(theirs.iter()).position(|(a, b)| a != b) {
        Some(at) => problem.push_str(&format!(
            ", first difference at {at:#x}{}, ours {:#04x} and theirs {:#04x}",
            member(ours, at),
            ours[at],
            theirs[at]
        )),
        // One is the start of the other, so there is no byte to point at and the lengths are the whole
        // of what there is to say. This is what a missing member looks like.
        None => problem.push_str(", and the shorter one is the start of the longer"),
    }
    problem
}

/// Which member of the archive an offset lands in, as something to say in a problem.
///
/// An offset on its own is not a diagnosis. A member header is 60 bytes of text naming the member and
/// its length, so walking them is cheap and the answer turns "byte 0x1a7" into "the long names
/// member", which is usually the whole of what went wrong.
fn member(archive: &[u8], at: usize) -> String {
    if !archive.starts_with(b"!<arch>\n") {
        return String::new();
    }
    let mut start = 8;
    let mut index = 0;
    while start + 60 <= archive.len() {
        let header = &archive[start..start + 60];
        let name = String::from_utf8_lossy(&header[..16]).trim_end().to_owned();
        let Ok(size) = String::from_utf8_lossy(&header[48..58]).trim_end().parse::<usize>() else {
            return String::new();
        };
        let end = start + 60 + size + size % 2;
        if at < end {
            let where_ = if at < start + 60 { "the header of " } else { "" };
            return format!(" ({where_}member {index}, `{name}`, which starts at {start:#x})");
        }
        start = end;
        index += 1;
    }
    String::new()
}

/// Asks `llvm-readobj` to read our library, and complains if it will not or says nothing.
///
/// Two questions only, because this is the second opinion and not the check. Does something that
/// parses these files for a living accept ours, and does it find the records in it. A file both
/// writers agree on that no reader will look at is still possible, and this is what rules it out.
fn read(readobj: &PathBuf, one: &Written) -> Result<Vec<String>> {
    let out = Command::new(readobj)
        .args(["--coff-imports", "--symbols"])
        .arg(&one.path)
        .output()
        .map_err(|e| Error::Io(format!("could not run llvm-readobj: {e}")))?;
    let said = String::from_utf8_lossy(&out.stdout);
    let complained = String::from_utf8_lossy(&out.stderr);
    if !out.status.success() {
        return Ok(vec![format!(
            "{}: llvm-readobj would not read it: {}",
            one.what(),
            complained.trim()
        )]);
    }
    let mut problems = Vec::new();
    if !complained.trim().is_empty() {
        problems.push(format!("{}: llvm-readobj said {}", one.what(), complained.trim()));
    }
    // Every case has at least one export in it, so a listing with no symbol at all is a file that
    // parsed and holds nothing, which is the failure a byte comparison cannot have.
    if !said.contains("Symbol {") {
        problems.push(format!("{}: llvm-readobj found no symbols in it", one.what()));
    }
    Ok(problems)
}

impl Written {
    /// The case and the target, which is what every problem starts with.
    fn what(&self) -> String {
        format!("{} for {}", self.name, self.tuple)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real archive, cut down to two members, read the way a problem reads it.
    fn archive() -> Vec<u8> {
        let mut out = Vec::from(&b"!<arch>\n"[..]);
        out.extend_from_slice(b"/               0           0     0     0       6         `\n");
        out.extend_from_slice(b"abcdef");
        out.extend_from_slice(b"//              0           0     0     0       3         `\n");
        out.extend_from_slice(b"xy\0");
        out.push(b'\n');
        out
    }

    #[test]
    fn an_offset_is_reported_as_the_member_it_lands_in() {
        let archive = archive();
        // Inside the first member's body.
        assert!(member(&archive, 70).contains("member 0, `/`"));
        // Inside the second member's header, which is worth saying because a length that is one out
        // is a difference in a header and not in anything a linker reads.
        assert!(member(&archive, 80).contains("the header of member 1, `//`"));
        assert!(member(&archive, 135).contains("member 1, `//`"));
    }

    #[test]
    fn a_difference_says_where_it_is_and_what_both_sides_have() {
        let ours = archive();
        let mut theirs = ours.clone();
        theirs[70] = b'Z';
        let said = difference("sample for x86_64-windows-gnu", &ours, &theirs);
        assert!(said.contains("sample for x86_64-windows-gnu"), "{said}");
        assert!(said.contains("ours is 138 bytes and llvm-dlltool's is 138"), "{said}");
        assert!(said.contains("first difference at 0x46"), "{said}");
        assert!(said.contains("member 0, `/`"), "{said}");
        assert!(said.contains("ours 0x63 and theirs 0x5a"), "{said}");
    }

    #[test]
    fn a_truncated_library_is_reported_by_its_length_alone() {
        let ours = archive();
        let said = difference("sample for x86_64-windows-gnu", &ours[..60], &ours);
        assert!(said.contains("ours is 60 bytes and llvm-dlltool's is 138"), "{said}");
        assert!(said.contains("the shorter one is the start of the longer"), "{said}");
    }

    #[test]
    fn something_that_is_not_an_archive_is_not_described() {
        assert_eq!(member(b"not an archive", 3), "");
        // A header that does not parse stops the walk rather than guessing at the next one.
        let mut broken = archive();
        broken[56..58].copy_from_slice(b"xx");
        assert_eq!(member(&broken, 70), "");
    }
}
