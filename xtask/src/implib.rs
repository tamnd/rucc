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
//! The reference has to be llvm 19 or newer and an older one is refused rather than compared against,
//! which [`dlltool`] explains. That is a real cost of a check written this way: the thing being
//! compared against has versions, and two of them disagree.
//!
//! Neither tool is a build dependency and neither is needed to build or test the compiler. GNU
//! `dlltool` is deliberately not accepted as the reference: it names its members after the output file
//! and defines `_head_<file>` where LLVM defines `__IMPORT_DESCRIPTOR_<dll>`, so its output is a
//! different file by design and `crates/rucc-stub/src/coff.rs` says which of the two it follows and why.

use std::path::{Path, PathBuf};
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
    let dlltool = dlltool()?;

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

/// The reference writer, and only one new enough to be the reference.
///
/// llvm 18 and earlier wrote a different file from the one llvm 19 onwards writes, because the
/// handling of a `.def` rename was rewritten for ARM64EC and that rewrite is where the weak alias
/// answer to a `== name` came from. So an older tool is not a tool that happens to disagree, it is a
/// tool with behaviour llvm itself has since replaced, and holding this writer to it would mean
/// writing libraries no current toolchain writes. Ubuntu 24.04 ships llvm 18, which is how this was
/// found.
///
/// What says which is which is the usage message, because `llvm-dlltool` has no `--version` at all.
/// It ends with the machines it accepts, and `arm64ec` is in that line from 19 onwards and not before,
/// which is the same release boundary for the same reason. A version number parsed out of a sibling
/// tool would be guessing that the two came out of one package.
fn dlltool() -> Result<PathBuf> {
    let mut old = Vec::new();
    for path in candidates("llvm-dlltool") {
        // No arguments, so it prints its usage and exits non-zero. That is how it is asked what it
        // can do, and a tool that is not installed fails a different way.
        let Ok(out) = Command::new(&path).output() else { continue };
        let said = String::from_utf8_lossy(&out.stdout);
        if said.contains("arm64ec") {
            return Ok(path);
        }
        if said.contains("TARGETS:") {
            old.push(path);
        }
    }
    match old.split_first() {
        Some((first, _)) => Err(Error::Io(format!(
            "{} is llvm 18 or older, which writes a different import library from every llvm since, \
             so it cannot be the reference. Install a newer one: `apt install llvm-19` or later, or \
             `brew install llvm`.",
            first.display()
        ))),
        None => Err(Error::Io(
            "no llvm-dlltool on this machine, and this check is byte equality with it, so there is \
             nothing to compare against. It ships with llvm: `apt install llvm` or \
             `brew install llvm`."
                .to_owned(),
        )),
    }
}

/// The first of these that is there and answers.
///
/// A mac keeps the homebrew llvm out of the way of the system tools and a linux distribution puts it
/// under a directory named for its version, so both places are looked in. This is the same search
/// `stubs` does, with the tool name as the one thing that differs.
fn find(tool: &str) -> Option<PathBuf> {
    candidates(tool).into_iter().find(|path| {
        Command::new(path).arg("--version").output().is_ok_and(|out| out.status.success())
    })
}

/// Everywhere a tool might be, best first.
///
/// The versioned directories come last and in reverse, so a machine carrying several llvm versions
/// side by side is asked about the newest rather than about `llvm-16` sorting before `llvm-9`.
fn candidates(tool: &str) -> Vec<PathBuf> {
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
    // By the number rather than by the text, so llvm-9 does not sort above llvm-21.
    versioned.sort_by_key(|path| std::cmp::Reverse(number(path)));

    let named = [
        PathBuf::from(tool),
        PathBuf::from("/opt/homebrew/opt/llvm/bin").join(tool),
        PathBuf::from("/usr/local/opt/llvm/bin").join(tool),
    ];
    named.into_iter().chain(versioned).collect()
}

/// The version out of an `/usr/lib/llvm-21/bin/llvm-dlltool`, or zero if there is not one.
fn number(path: &Path) -> u32 {
    path.components()
        .filter_map(|part| part.as_os_str().to_string_lossy().strip_prefix("llvm-")?.parse().ok())
        .next_back()
        .unwrap_or(0)
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
    fn a_versioned_directory_is_read_as_a_number_rather_than_as_text() {
        assert_eq!(number(Path::new("/usr/lib/llvm-21/bin/llvm-dlltool")), 21);
        // The thing this is for. Ubuntu 24.04 carries llvm 16, 17 and 18 at once, and the whole point
        // of looking at several is to end up at the newest, which sorting the paths as text does not.
        let mut several: Vec<&Path> = vec![
            Path::new("/usr/lib/llvm-18/bin/llvm-dlltool"),
            Path::new("/usr/lib/llvm-9/bin/llvm-dlltool"),
            Path::new("/usr/lib/llvm-21/bin/llvm-dlltool"),
        ];
        several.sort_by_key(|path| std::cmp::Reverse(number(path)));
        assert_eq!(several[0], Path::new("/usr/lib/llvm-21/bin/llvm-dlltool"));
        // The tool's own name starts with the same letters and is not a version.
        assert_eq!(number(Path::new("llvm-dlltool")), 0);
        assert_eq!(number(Path::new("/opt/homebrew/opt/llvm/bin/llvm-dlltool")), 0);
    }

    #[test]
    fn the_tool_on_the_path_is_asked_before_anywhere_a_version_is_spelled_out() {
        let order = candidates("llvm-dlltool");
        assert_eq!(order[0], Path::new("llvm-dlltool"));
        // Whatever a machine has, a directory naming a version never comes first, because the one on
        // the path is the one somebody chose.
        assert!(order.iter().take(3).all(|path| number(path) == 0), "{order:?}");
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
