//! Every stub the writer produces, read by a reader nobody here wrote.
//!
//! `spec/cross-compile/09-libc-stubs.md` section 9.8 names three correctness properties for the stub
//! writer, and the round trip test in `crates/rucc-stub/tests/roundtrip.rs` is one of them. Its limit
//! is that the reader and the writer were written by the same person on the same afternoon, so it
//! checks the encoding and not the belief behind it. The first version of both used four byte
//! `.hash` entries on every architecture, they agreed with each other, and 64-bit s390 uses eight.
//! `llvm-readelf` is what noticed.
//!
//! So this check hands the output to readers from outside the workspace. It is much cheaper than the
//! comparison against a real distribution `libc.so` that section 9.8 calls the highest value test,
//! and it catches a different class of thing: not a description that disagrees with reality, but a
//! container that only one program in the world can parse.
//!
//! Two readers are asked, because they disagree about exactly the field that started this. GNU
//! `readelf` carries the hash entry width per architecture and gets s390 right, and `llvm-readelf`
//! declines to parse an s390 hash table at all rather than assume the common width, which is the one
//! complaint below that is accepted rather than reported. Either reader alone is worth running and
//! neither is required, because a machine with no llvm and no binutils should be told what is
//! missing rather than told everything passed.
//!
//! Neither reader is a build dependency and neither is needed to build or test the compiler. This
//! task is the only thing that wants them.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{Error, Result, root};

/// The one complaint a reader is allowed to make, and the only file it is allowed to make it about.
///
/// `llvm-readelf` refuses to parse any `EM_S390` hash table whatever is in it, so the message says
/// nothing about our bytes. GNU `readelf` does parse it, which is why both readers are run: the
/// table llvm will not look at is checked by the reader that knows its width.
const ALLOWED: (&str, &str) = ("s390x", "non-standard 8 byte entries");

/// Things a reader prints into its output rather than onto its error stream.
///
/// A string table index that points nowhere is not an error to a reader, it is a string it could not
/// read, and it says so inline and carries on with a zero exit status. Those are the quiet failures
/// worth grepping for, because the loud ones are already covered by the exit status.
const TELLTALES: &[&str] = &["<corrupt", "<unknown", "warning:", "error:"];

/// A stub the example wrote, and what went into it.
struct Stub {
    /// The target it was written for, or `empty` for the library that exports nothing.
    name: String,
    /// Where the example put it.
    path: PathBuf,
    /// The `SONAME` the description asked for, which a reader has to be able to find and print.
    soname: String,
    /// Every symbol name the description asked for, which a reader has to list.
    symbols: Vec<String>,
}

/// Everything the example had to say: the stubs it wrote, and the targets the writer would not write.
struct Written {
    /// One per file on disk.
    stubs: Vec<Stub>,
    /// A target and the reason the writer gave for not producing a stub for it.
    refused: Vec<(String, String)>,
}

/// Writes one stub per ELF target and reads every one of them back with an outside reader.
pub(crate) fn stubs() -> Result<()> {
    let readers = find();
    if readers.is_empty() {
        return Err(Error::Io(
            "no readelf and no llvm-readelf on this machine, and this check is nothing without \
             one. GNU readelf ships with binutils: `apt install binutils` or \
             `brew install binutils`. llvm-readelf ships with llvm: `apt install llvm` or \
             `brew install llvm`."
                .to_owned(),
        ));
    }

    let into = root().join("target").join("stubs");
    let Written { stubs, refused } = emit(&into)?;
    if stubs.is_empty() {
        return Err(Error::Io("the emit example wrote no stubs at all".to_owned()));
    }
    let names: Vec<String> =
        readers.iter().map(|path| path.display().to_string()).collect::<Vec<_>>();
    println!("xtask: {} stubs, read back by {}", stubs.len(), names.join(" and "));
    for (target, why) in &refused {
        // Not a failure. Section 9.1 argues for refusing over guessing, and an argument for refusing
        // is worth nothing if the refusals are invisible.
        println!("xtask: the writer refused {target}: {why}");
    }

    let mut problems = Vec::new();
    for reader in &readers {
        for stub in &stubs {
            problems.extend(read(reader, stub)?);
        }
    }
    if problems.is_empty() {
        println!("xtask: every stub reads back with its soname and all of its symbols");
        return Ok(());
    }
    Err(Error::Failed { task: "stubs", problems })
}

/// The readers on this machine, at most one of each family.
///
/// One of each rather than all of them, because two copies of llvm read a file the same way and the
/// value here is in the second opinion rather than in the second run. A mac keeps the homebrew ones
/// out of the way of the system tools and a linux distribution puts llvm under a directory named for
/// its version, so both places are looked in.
fn find() -> Vec<PathBuf> {
    let mut versioned: Vec<PathBuf> = std::fs::read_dir("/usr/lib")
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name().is_some_and(|name| name.to_string_lossy().starts_with("llvm-"))
        })
        .map(|path| path.join("bin").join("llvm-readelf"))
        .collect();
    versioned.sort();
    versioned.reverse();

    let gnu = [
        PathBuf::from("readelf"),
        PathBuf::from("/opt/homebrew/opt/binutils/bin/readelf"),
        PathBuf::from("/usr/local/opt/binutils/bin/readelf"),
    ];
    let llvm = [
        PathBuf::from("llvm-readelf"),
        PathBuf::from("/opt/homebrew/opt/llvm/bin/llvm-readelf"),
        PathBuf::from("/usr/local/opt/llvm/bin/llvm-readelf"),
    ];
    let mut found = Vec::new();
    found.extend(first(gnu.into_iter()));
    found.extend(first(llvm.into_iter().chain(versioned)));
    found
}

/// The first of these that is there and answers.
fn first(mut candidates: impl Iterator<Item = PathBuf>) -> Option<PathBuf> {
    candidates.find(|path| {
        Command::new(path).arg("--version").output().is_ok_and(|out| out.status.success())
    })
}

/// Runs the example that writes the stubs, and reads the lines it prints.
///
/// The example is where the descriptions live, which keeps this file from holding a second copy of
/// what a stub is supposed to contain. xtask depends on no crate in the workspace, so the data it
/// checks arrives as output rather than as a call, the same split `cargo xtask disasm` uses.
fn emit(into: &Path) -> Result<Written> {
    // A stale file from an earlier run that the example no longer writes would still be read, and
    // would pass, so the directory starts empty.
    if into.exists() {
        std::fs::remove_dir_all(into)
            .map_err(|e| Error::Io(format!("could not clear {}: {e}", into.display())))?;
    }
    let out = Command::new("cargo")
        .args(["run", "-q", "-p", "rucc-stub", "--example", "emit"])
        .arg(into)
        .current_dir(root())
        .output()
        .map_err(|e| Error::Io(format!("could not run cargo: {e}")))?;
    if !out.status.success() {
        return Err(Error::Io(format!(
            "the emit example failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }

    let listing = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut stubs = Vec::new();
    let mut refused = Vec::new();
    for line in listing.lines() {
        let fields: Vec<&str> = line.split('|').collect();
        match fields.as_slice() {
            [name, answer] => match answer.strip_prefix("refused: ") {
                Some(why) => refused.push(((*name).to_owned(), why.to_owned())),
                None => {
                    return Err(Error::Io(format!("emit said something unexpected: {line}")));
                }
            },
            [name, path, soname, symbols] => stubs.push(Stub {
                name: (*name).to_owned(),
                path: PathBuf::from(path),
                soname: (*soname).to_owned(),
                // An empty library has no symbols, and splitting nothing on a comma gives one
                // empty name rather than none.
                symbols: symbols.split(',').filter(|s| !s.is_empty()).map(str::to_owned).collect(),
            }),
            _ => return Err(Error::Io(format!("emit line has the wrong number of bars: {line}"))),
        }
    }
    Ok(Written { stubs, refused })
}

/// What one reader makes of one stub, as a list of problems, empty when there are none.
fn read(reader: &Path, stub: &Stub) -> Result<Vec<String>> {
    let out = Command::new(reader)
        .arg("--all")
        .arg(&stub.path)
        .output()
        .map_err(|e| Error::Io(format!("could not run {}: {e}", reader.display())))?;
    let said = String::from_utf8_lossy(&out.stdout);
    let complained = String::from_utf8_lossy(&out.stderr);
    let who = reader
        .file_name()
        .map_or_else(|| reader.display().to_string(), |n| n.to_string_lossy().into_owned());
    let at = format!("{who} on {}", stub.name);

    let mut problems = Vec::new();
    if !out.status.success() {
        problems.push(format!("{at}: exited {}", out.status));
    }
    // Anything at all on the error stream is a complaint, and a complaint on the output stream is
    // one of the quiet spellings. Both go through the same exception, so that the one message this
    // check accepts is accepted wherever the reader chose to print it.
    let lines = complained
        .lines()
        .filter(|line| !line.trim().is_empty())
        .chain(said.lines().filter(|line| quiet(line)));
    for line in lines {
        if !allowed(&stub.name, line) {
            problems.push(format!("{at}: {}", line.trim()));
        }
    }

    // Proof that the reader got as far as the header rather than printing nothing and succeeding.
    // Both families print the magic bytes, so this holds whichever one is asking.
    if !said.contains("Magic:") {
        problems.push(format!("{at}: printed no ELF header"));
        return Ok(problems);
    }
    if !said.contains(&stub.soname) {
        problems.push(format!("{at}: does not mention the soname {}", stub.soname));
    }
    for symbol in &stub.symbols {
        if !said.contains(symbol.as_str()) {
            problems.push(format!("{at}: does not list {symbol}"));
        }
    }
    Ok(problems)
}

/// Whether a line a reader printed among its output is a complaint rather than a finding.
///
/// Binutils capitalizes its warnings and llvm does not, so the comparison is folded. The words are
/// not ones either reader uses for anything it found in a well formed file.
fn quiet(line: &str) -> bool {
    let line = line.to_ascii_lowercase();
    TELLTALES.iter().any(|telltale| line.contains(telltale))
}

/// Whether a line a reader printed is the one known exception.
fn allowed(stub: &str, line: &str) -> bool {
    let (machine, message) = ALLOWED;
    stub.contains(machine) && line.contains(message)
}

#[cfg(test)]
mod tests {
    use super::{allowed, quiet};

    #[test]
    fn a_complaint_among_the_output_is_noticed_however_it_is_spelled() {
        // binutils capitalizes and llvm does not, and a reader that says a string table index points
        // nowhere says it inline with a zero exit status, which is the case this is here for.
        assert!(quiet("readelf: Warning: the dynamic symbol table is truncated"));
        assert!(quiet("llvm-readelf: warning: invalid sh_type for string table"));
        assert!(quiet("  [ 3] .dynstr   STRTAB   <corrupt: 4>"));
    }

    #[test]
    fn an_ordinary_line_of_a_readers_output_is_not_a_complaint() {
        assert!(!quiet("  Magic:   7f 45 4c 46 02 01 01 00 00 00 00 00 00 00 00 00"));
        assert!(!quiet("     3: 0000000000000000     8 OBJECT  GLOBAL DEFAULT     1 environ"));
        assert!(!quiet(" 0x000000000000000e (SONAME)  Library soname: [libc.so.6]"));
    }

    #[test]
    fn the_s390_hash_refusal_is_accepted_because_it_is_about_the_machine() {
        // llvm prints this on `e_machine` alone, before looking at the table, so it says nothing
        // about our bytes. It is the only complaint this check swallows.
        let line = "llvm-readelf: warning: 's390x-linux-gnu.so': the hash table at 0xb0 is not \
                    supported: it contains non-standard 8 byte entries on IBM S/390 platform";
        assert!(allowed("s390x-linux-gnu", line));
    }

    #[test]
    fn the_same_refusal_about_another_machine_is_news() {
        // The point of keeping the exception tied to a file name. If a reader ever says this about
        // x86-64, the writer has put an s390 hash table in an x86-64 file.
        let line = "llvm-readelf: warning: the hash table contains non-standard 8 byte entries";
        assert!(!allowed("x86_64-linux-gnu", line));
    }

    #[test]
    fn anything_else_about_s390x_is_still_reported() {
        let line = "llvm-readelf: warning: 's390x-linux-gnu.so': the dynamic table is truncated";
        assert!(!allowed("s390x-linux-gnu", line));
    }
}
