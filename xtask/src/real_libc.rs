//! Our glibc stub, held against the real `libc.so.6` on this machine.
//!
//! `spec/cross-compile/09-libc-stubs.md` section 9.8 names three correctness properties for the stub
//! writer and calls this one the single highest value test in the document. The other two are
//! `crates/rucc-stub/tests/roundtrip.rs`, which reads the bytes back, and `cargo xtask stubs`, which
//! hands them to readers nobody here wrote. Both of those ask whether the file says what the
//! description said. This one asks whether the description is true, which is a different question and
//! the only one a real library can answer.
//!
//! The rule is section 9.8's: the real library's set must be a superset of ours, with matching
//! versions and sizes on the intersection. Not equality, because a real glibc exports
//! `GLIBC_PRIVATE` names for its own libraries to call and an `abilist` leaves them out on purpose.
//! Ours being a subset is the property a link needs, since every name we promise has to be there when
//! the program runs.
//!
//! Both files are read with the same `readelf`, which is what makes the output comparable. A
//! difference in how two readers spell a version would otherwise come out as a difference between the
//! libraries, and the difference between the libraries is the whole answer here.
//!
//! What this needs, and what it says when it does not have it: a glibc on this machine, which a mac
//! has no version of, and an `abilist` from glibc's own source, which nothing in this repository
//! fetches. `bin/abilist` in tamnd/rucc-cross is the fetch, pinned and hashed the way the toolchains
//! there are, and the reason it lives there rather than here is that a compiler repository should not
//! be downloading a libc to run its tests.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{Error, Result, root, stubs};

/// One definition a reader printed, which is everything the comparison looks at.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Definition {
    /// `FUNC`, `OBJECT`, `TLS`, `IFUNC` or whatever else the reader called it.
    kind: String,
    /// `GLOBAL` or `WEAK`.
    binding: String,
    /// How many bytes, which is the number a copy relocation against a data symbol will move.
    size: u64,
    /// Whether an unversioned reference binds here, which a reader spells `@@`.
    default: bool,
}

/// A name and the version node one of its definitions is at, which is what a linker looks up.
type Key = (String, Option<String>);

/// Compares our generated glibc stub against the real `libc.so.6` on this machine.
pub(crate) fn real_libc(args: &[String]) -> Result<()> {
    let Some(reader) = stubs::find().into_iter().next() else {
        return Err(Error::Io(
            "no readelf and no llvm-readelf on this machine, and both sides of this comparison are \
             read with one. GNU readelf ships with binutils: `apt install binutils` or `brew \
             install binutils`."
                .to_owned(),
        ));
    };

    let Some(real) = real(args) else {
        // A mac is the ordinary case and it is not a failure. The comparison is per architecture and
        // per libc, and a machine with no glibc has nothing to say about glibc.
        println!(
            "xtask: no glibc libc.so.6 on this machine, so there is nothing to compare against"
        );
        return Ok(());
    };
    let arch = std::env::consts::ARCH;
    let tuple = format!("{arch}-linux-gnu");
    let Some(abilist) = abilist(args, arch) else {
        return Err(Error::Io(format!(
            "no abilist for {arch}. glibc checks one in per architecture and it is where the \
             description comes from, so this comparison needs the file rather than a list written \
             here. `bin/abilist` in tamnd/rucc-cross fetches a pinned glibc and extracts them, and \
             the directory it writes goes in RUCC_ABILISTS or as the first argument."
        )));
    };

    // The ceiling is the real library's own newest node, because what it exports is what it was
    // built to export and an abilist from a later glibc names symbols this machine does not have.
    // That is not a finding about either one, it is two versions, and section 9.2's node filtering is
    // exactly the answer to it.
    let real_symbols = symbols(&reader, &real)?;
    let ceiling = newest(&real_symbols);
    println!(
        "xtask: {} has {} definitions, the newest node being {}",
        real.display(),
        real_symbols.len(),
        ceiling.as_deref().unwrap_or("none at all")
    );

    let stub = root().join("target").join("real-libc").join(format!("{tuple}-libc.so.6"));
    if let Some(parent) = stub.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| Error::Io(format!("could not make {}: {e}", parent.display())))?;
    }
    write(&abilist, &tuple, ceiling.as_deref(), &stub)?;
    let ours = symbols(&reader, &stub)?;
    println!("xtask: our stub from {} has {} definitions", abilist.display(), ours.len());

    let mut problems = Vec::new();
    let mut matched = 0usize;
    let mut obsolete = 0usize;
    for (key, ours) in &ours {
        let (name, node) = key;
        let at = match node {
            Some(node) => format!("{name}@{node}"),
            None => name.clone(),
        };
        let Some(real) = real_symbols.get(key) else {
            // The node is named separately when the name is there and the node is not, because the
            // two say different things: one is a symbol glibc does not have and the other is a
            // version it does not have it at, and only the second is a node filtering bug.
            let elsewhere: Vec<String> = real_symbols
                .keys()
                .filter(|(other, _)| other == name)
                .map(|(_, node)| node.clone().unwrap_or_else(|| "no node".to_owned()))
                .collect();
            problems.push(if elsewhere.is_empty() {
                format!("{at} is ours alone, and the real library does not export that name")
            } else {
                format!(
                    "{at} is ours alone, and the real library has it at {}",
                    elsewhere.join(", ")
                )
            });
            continue;
        };
        matched += 1;

        // A real glibc resolves several of these at load time, so the real side says IFUNC where a
        // stub has to say FUNC: a stub with a resolver in it would have to have the resolver. Any
        // other disagreement is a description that would give a program the wrong relocation form.
        let theirs =
            if real.kind == "IFUNC" || real.kind == "GNU_IFUNC" { "FUNC" } else { &real.kind };
        if theirs != ours.kind {
            problems.push(format!("{at} is {} here and {} there", ours.kind, real.kind));
        }
        // The two directions of this say different things. Ours default and theirs not is a symbol
        // glibc keeps only for the programs already linked against it, and the abilist cannot say so:
        // the line for a compat symbol is the same three fields as the line for any other, and which
        // ones they are lives in glibc's C sources rather than in the file we read. So it is counted,
        // like the binding below. Ours superseded and theirs default is the description being wrong
        // about the definition a link will bind to, and that is a problem.
        if ours.default && !real.default {
            obsolete += 1;
        } else if !ours.default && real.default {
            problems.push(format!("{at} is superseded here and the default definition there"));
        }
        // Only where there is storage to copy. A function's size is how long the function is, which
        // an abilist does not record and a stub has no code to have.
        if ours.kind == "OBJECT" && ours.size != real.size {
            problems.push(format!("{at} is {} bytes here and {} there", ours.size, real.size));
        }
    }

    // Counted rather than failed on. glibc's own generator keeps no letter for a weak symbol, so every
    // name read from an abilist comes back global, which is the gap section 9.1's last row names and
    // the reason a binding is a number here and not a complaint.
    let weak = ours
        .iter()
        .filter(|(key, ours)| {
            real_symbols.get(*key).is_some_and(|real| real.binding != ours.binding)
        })
        .count();
    println!(
        "xtask: {matched} of our {} definitions are in the real library, which has {} of its own \
         besides",
        ours.len(),
        real_symbols.len().saturating_sub(matched)
    );
    if weak > 0 {
        println!(
            "xtask: {weak} of them differ in binding, which is the abilist keeping no letter for a \
             weak symbol and is section 9.1's last row"
        );
    }
    if obsolete > 0 {
        // What this costs is worth stating, because it is not nothing and it is not a broken program
        // either. A link against our stub binds one of these to the node the description names, the
        // loader finds the compat definition there and the program runs. A real toolchain would have
        // refused the link instead, so we are the more permissive of the two, and the way to stop
        // being is a recorded list of which names a real library keeps only for old binaries.
        println!(
            "xtask: {obsolete} of them the real library keeps only for programs already linked \
             against it, which an abilist has no way to say"
        );
    }

    if problems.is_empty() {
        println!(
            "xtask: every name we promise is in the real library, at the version we promise it"
        );
        return Ok(());
    }
    Err(Error::Failed { task: "real-libc", problems })
}

/// The real `libc.so.6` to compare against, or [`None`] on a machine that has no glibc.
///
/// `RUCC_REAL_LIBC` names one, for a machine with several or with one somewhere unusual. Otherwise
/// the loader is asked where the one it would use is, which is a better answer than a list of paths
/// because a distribution puts it where it likes, and the list is the fallback for a machine whose
/// `ldd` is not GNU's.
fn real(args: &[String]) -> Option<PathBuf> {
    if let Some(named) = args.iter().find_map(|a| a.strip_prefix("--libc=")) {
        return Some(PathBuf::from(named));
    }
    if let Ok(named) = std::env::var("RUCC_REAL_LIBC") {
        return Some(PathBuf::from(named));
    }
    if let Ok(out) = Command::new("ldd").arg("/bin/sh").output() {
        let said = String::from_utf8_lossy(&out.stdout);
        for line in said.lines() {
            let line = line.trim();
            if !line.starts_with("libc.so.6") {
                continue;
            }
            if let Some((_, path)) = line.split_once("=> ") {
                let path = path.split_whitespace().next().unwrap_or_default();
                if !path.is_empty() && Path::new(path).exists() {
                    return Some(PathBuf::from(path));
                }
            }
        }
    }
    [
        format!("/lib/{}-linux-gnu/libc.so.6", std::env::consts::ARCH),
        "/lib64/libc.so.6".to_owned(),
        "/lib/libc.so.6".to_owned(),
        "/usr/lib/libc.so.6".to_owned(),
    ]
    .into_iter()
    .map(PathBuf::from)
    .find(|path| path.exists())
}

/// The `abilist` for an architecture, from the first of the three places that has one.
///
/// A file is taken as it is, and a directory is looked in for `<arch>/libc.abilist`, which is the
/// layout `bin/abilist` in tamnd/rucc-cross writes and the layout glibc's own tree suggests.
fn abilist(args: &[String], arch: &str) -> Option<PathBuf> {
    let named = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .cloned()
        .or_else(|| std::env::var("RUCC_ABILISTS").ok())?;
    let path = PathBuf::from(named);
    if path.is_file() {
        return Some(path);
    }
    [path.join(arch).join("libc.abilist"), path.join("libc.abilist")]
        .into_iter()
        .find(|candidate| candidate.is_file())
}

/// Runs the example that turns an `abilist` into a stub, and says what it printed.
///
/// The description, the blob and the writer are all in the example, because xtask depends on no crate
/// in the workspace. The same split `cargo xtask stubs` uses.
fn write(abilist: &Path, tuple: &str, ceiling: Option<&str>, into: &Path) -> Result<()> {
    let out = Command::new("cargo")
        .args(["run", "-q", "-p", "rucc-stub", "--example", "from-abilist", "--"])
        .arg(abilist)
        .arg(tuple)
        .arg(ceiling.unwrap_or("-"))
        .arg(into)
        .current_dir(root())
        .output()
        .map_err(|e| Error::Io(format!("could not run cargo: {e}")))?;
    if !out.status.success() {
        return Err(Error::Io(format!(
            "the from-abilist example failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        // The example's own account of what it read and what it dropped, which is most of what a
        // person looking at a failure here wants to know before they look at the failure.
        println!("xtask: from-abilist {line}");
    }
    Ok(())
}

/// Every definition in a file's dynamic symbol table, keyed by the name and the node.
///
/// References are left out. A dynamic symbol table has both, the undefined entries being what the
/// file itself needs, and a comparison of what two libraries export has no business with them.
fn symbols(reader: &Path, file: &Path) -> Result<BTreeMap<Key, Definition>> {
    // `--wide` for the same reason `cargo xtask stubs` passes it: GNU readelf spends twenty one
    // columns on a name and a version node and truncates the rest into an ellipsis.
    let out = Command::new(reader)
        .arg("--wide")
        .arg("--dyn-syms")
        .arg(file)
        .output()
        .map_err(|e| Error::Io(format!("could not run {}: {e}", reader.display())))?;
    if !out.status.success() {
        return Err(Error::Io(format!(
            "{} could not read {}: {}",
            reader.display(),
            file.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }

    let mut found = BTreeMap::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        if let Some((key, definition)) = definition(line) {
            found.insert(key, definition);
        }
    }
    if found.is_empty() {
        return Err(Error::Io(format!(
            "{} found no definitions in {}, which is not a library this comparison can use",
            reader.display(),
            file.display()
        )));
    }
    Ok(found)
}

/// One line of a symbol table as a definition, or [`None`] if it is not one.
///
/// The columns are `Num: Value Size Type Bind Vis Ndx Name`, and a row is told from a heading, a blank
/// line or the table's own title by the first column being a number with a colon after it. The colon
/// alone is not enough, because the heading's first column is the word `Num:` and the rest of it lines
/// up with a row's columns well enough to be read as one. A row with no name has seven columns rather
/// than eight and there is nothing in it to compare.
fn definition(line: &str) -> Option<(Key, Definition)> {
    let fields: Vec<&str> = line.split_whitespace().collect();
    if fields.len() < 8 || !fields[0].strip_suffix(':').is_some_and(|n| n.parse::<u64>().is_ok()) {
        return None;
    }
    let (size, kind, binding, ndx, name) = (fields[2], fields[3], fields[4], fields[6], fields[7]);
    // An undefined entry is a reference rather than a definition, and the two are the same eight
    // columns apart from this one.
    if ndx == "UND" {
        return None;
    }
    // A reader prints a size in hexadecimal once it is large enough, which no symbol here is, and a
    // number nobody can read is better treated as no number than as a reason to stop reading a table.
    let size = size.parse::<u64>().unwrap_or(0);
    let (name, node, default) = match name.split_once("@@") {
        Some((name, node)) => (name, Some(node.to_owned()), true),
        None => match name.split_once('@') {
            Some((name, node)) => (name, Some(node.to_owned()), false),
            // An unversioned definition is the one an unversioned reference binds to, which is the
            // same thing being the default means.
            None => (name, None, true),
        },
    };
    let definition =
        Definition { kind: kind.to_owned(), binding: binding.to_owned(), size, default };
    Some(((name.to_owned(), node), definition))
}

/// The newest `GLIBC_` node anything in the table is at, which is the version the library is.
///
/// Compared by number and not as text, because `GLIBC_2.9` is older than `GLIBC_2.10` and sorting
/// the strings says the opposite. Only the glibc family is considered: `GCC_3.0` is a node on two of
/// our architectures and it says nothing about which glibc this is.
fn newest(symbols: &BTreeMap<Key, Definition>) -> Option<String> {
    symbols
        .keys()
        .filter_map(|(_, node)| node.as_deref())
        .filter(|node| node.starts_with("GLIBC_") && node != &"GLIBC_PRIVATE")
        .map(|node| (parts(node), node.to_owned()))
        .max()
        .map(|(_, node)| node)
}

/// A node's version as numbers, so that two of them compare the way a person reads them.
fn parts(node: &str) -> Vec<u64> {
    node.trim_start_matches("GLIBC_").split('.').map(|p| p.parse::<u64>().unwrap_or(0)).collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{Definition, definition, newest};

    /// A table out of the names and nodes alone, which is all [`newest`] looks at.
    fn table(at: &[(&str, &str)]) -> BTreeMap<(String, Option<String>), Definition> {
        at.iter()
            .map(|(name, node)| {
                let definition = Definition {
                    kind: "FUNC".to_owned(),
                    binding: "GLOBAL".to_owned(),
                    size: 0,
                    default: true,
                };
                ((name.to_string(), Some(node.to_string())), definition)
            })
            .collect()
    }

    #[test]
    fn a_default_definition_and_a_superseded_one_are_told_apart() {
        let line = "  2117: 0000000000110e30   181 FUNC    GLOBAL DEFAULT   16 fcntl@@GLIBC_2.28";
        let (key, found) = definition(line).expect("that is a definition");
        assert_eq!(key, ("fcntl".to_owned(), Some("GLIBC_2.28".to_owned())));
        assert!(found.default);
        assert_eq!(found.size, 181);

        let line = "  1286: 000000000010e7a0   157 FUNC    GLOBAL DEFAULT   16 fcntl@GLIBC_2.2.5";
        let (key, found) = definition(line).expect("that is a definition too");
        assert_eq!(key, ("fcntl".to_owned(), Some("GLIBC_2.2.5".to_owned())));
        assert!(!found.default);
    }

    #[test]
    fn a_reference_is_not_a_definition() {
        // What the table of a real library's own needs looks like. Counting these as exports would
        // make the superset rule pass on names nobody defines.
        let line = "     1: 0000000000000000     0 FUNC    GLOBAL DEFAULT  UND __tls_get_addr";
        assert!(definition(line).is_none());
    }

    #[test]
    fn a_heading_is_not_a_definition() {
        assert!(definition("Symbol table '.dynsym' contains 2364 entries:").is_none());
        assert!(
            definition("   Num:    Value          Size Type    Bind   Vis      Ndx Name").is_none()
        );
        assert!(definition("").is_none());
    }

    #[test]
    fn a_nameless_row_is_passed_over() {
        // The first entry of every symbol table, which has no name and nothing to compare.
        let line = "     0: 0000000000000000     0 NOTYPE  LOCAL  DEFAULT  UND";
        assert!(definition(line).is_none());
    }

    #[test]
    fn the_newest_node_is_the_highest_number_and_not_the_last_word() {
        // The case that makes this worth a function. Sorted as text, GLIBC_2.9 is the winner.
        let found =
            newest(&table(&[("a", "GLIBC_2.9"), ("b", "GLIBC_2.34"), ("c", "GLIBC_2.2.5")]));
        assert_eq!(found.as_deref(), Some("GLIBC_2.34"));
    }

    #[test]
    fn the_private_node_and_another_familys_are_not_versions_of_glibc() {
        // GLIBC_PRIVATE is at no version at all and GCC_3.0 is somebody else's node. Either one as a
        // ceiling would drop every symbol in the library.
        let found = newest(&table(&[
            ("a", "GLIBC_2.17"),
            ("b", "GLIBC_PRIVATE"),
            ("c", "GCC_3.0"),
            ("d", "GLIBC_2.4"),
        ]));
        assert_eq!(found.as_deref(), Some("GLIBC_2.17"));
    }
}
