//! Builds a shared library out of what this compiler wrote, links a program against it, and runs
//! it.
//!
//! Design: `spec/11-asm-objects-debug.md` section 11.3.
//!
//! There is one argument for this and it is that two bugs in a row were things only a shared
//! library could show. tamnd/rucc#733 was every global coming out `STV_HIDDEN`, so a library built
//! here exported nothing and `dlsym` could not find a function the source plainly defines, and a
//! static link never reads the field. tamnd/rucc#756 was `-fPIC` being accepted and dropped, so an
//! object could not go into a shared library at all once the code touched a global, and every link
//! the suite does produces an executable. Both were found by somebody remembering to look, which is
//! not a check. This is the check.
//!
//! tamnd/rucc#1004 was the third, and it got past this one. The library here was built out of
//! listings, `as` turned those into objects, and the bug was in the objects this compiler writes
//! itself, which nothing here ever linked. So the library is built twice now, once each way, and
//! the fixture is the same fixture: what a check of this shape is worth depends entirely on the
//! output under it being the output a real build gets.
//!
//! Not one question but four, because the ones that pass separately are the ones that hid these.
//! A link that succeeds says nothing about `st_other`. A dynamic symbol table with the right names
//! in it says nothing about which copy of a variable the library reads. So the library is linked,
//! its table is read, the program that uses it is run, and it is run again with something loaded in
//! front of it.
//!
//! The library is this compiler's and everything around it is the system compiler's. That is the
//! point: a fixture where both halves are wrong in the same direction agrees with itself, which is
//! how the s390x hash width in `crates/rucc-stub` got through its own round trip.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

use crate::runner::{Runner, TRIPLE};
use crate::{Error, Result, root};

/// The sources this compiler builds, which are the library and nothing else.
const OURS: [&str; 2] = ["one", "two"];

/// What the run has to print, in the order the fixture prints it.
///
/// A table rather than a file beside the fixture, because every row is an assertion with a reason
/// and the reason is the useful half. A row that fails is quoted back with the sentence next to it.
const WANTED: [(&str, &str, &str); 16] = [
    (
        "object link",
        "ok",
        "the same two files, compiled to objects by this compiler rather than to listings, link \
         into a shared library as well. That is #1004: every function carries an unwind record, \
         the record measured to the function's name, and a name is answered at load time, so the \
         linker refused the distance and no object this compiler wrote could go into a library. \
         The listing path was right the whole time, which is why the eleven rows below never said \
         a word about it",
    ),
    (
        "dynamic symbol exported",
        "DEFAULT",
        "an exported function is in the table and nothing narrowed it, which is #733: the object \
         writer used to infer a visibility from a scope and infer hidden, so this table was empty",
    ),
    (
        "dynamic symbol shared_var",
        "DEFAULT",
        "and so is an exported variable, which travels a different path through the writer",
    ),
    (
        "dynamic symbol hidden_helper",
        "absent",
        "a hidden name is not in the table at all, which is what hidden means and is the half of \
         #733 that was accidentally right",
    ),
    (
        "dynamic symbol protected_answer",
        "PROTECTED",
        "all three visibilities survive, because ELF records all three",
    ),
    (
        "exported through dlsym",
        "7",
        "the name is not only in the table but is the function it says it is",
    ),
    (
        "hidden_helper through dlsym",
        "absent",
        "and a hidden one cannot be reached from outside however the caller asks",
    ),
    (
        "shared variable the library reads",
        "42",
        "the executable took its own copy of an exported variable, so the library has to read that \
         copy. This is the case #756 got wrong in a way that was an answer rather than an error",
    ),
    (
        "other library's variable",
        "5",
        "a variable another library defines, whose address only the loader knows",
    ),
    (
        "hidden helper called inside the library",
        "9",
        "a hidden name is still global to the static link, so the other object of the same library \
         reaches it",
    ),
    ("interposed", "1", "with nothing in front of it the library calls its own definition"),
    (
        "interposed under LD_PRELOAD",
        "99",
        "and with something in front of it the library calls that one instead, which is what an \
         exported name means and is what -fno-semantic-interposition will be about",
    ),
    (
        "dynamic symbol exported from the object path",
        "DEFAULT",
        "and the library built from objects says the same four things about its names as the one \
         built from listings. The two paths are separate code, so a visibility is written twice \
         and could come out twice differently",
    ),
    (
        "dynamic symbol shared_var from the object path",
        "DEFAULT",
        "the same for an exported variable",
    ),
    ("dynamic symbol hidden_helper from the object path", "absent", "the same for a hidden one"),
    (
        "dynamic symbol protected_answer from the object path",
        "PROTECTED",
        "and the same for the third visibility",
    ),
];

/// Builds the library, runs the program, and holds every line to [`WANTED`].
///
/// # Errors
///
/// [`Error::Io`] when the compiler will not build or the script will not run, and
/// [`Error::Failed`] with one problem per line that came back wrong or did not come back.
pub(crate) fn dso() -> Result<()> {
    let work = build()?;
    let runner = Runner::find("the shared library check")?;
    let printed = runner.run(&work, "the shared library check")?;
    let said = read(&printed);

    let mut problems = Vec::new();
    for (what, want, why) in WANTED {
        match said.get(what) {
            Some(got) if got == want => {}
            Some(got) => problems.push(format!("{what}: {got}, wanted {want}. {why}")),
            None => problems.push(format!("{what}: nothing said it. {why}")),
        }
    }
    if !problems.is_empty() {
        // The whole output, because a link that failed is one message explaining every line at
        // once and reporting eleven of them separately would bury it.
        problems.push(format!("what the run printed:\n{}", crate::indent(printed.trim_end())));
        return Err(Error::Failed { task: "dso", problems });
    }
    println!("dso: {} checks, two libraries, {runner}", WANTED.len());
    Ok(())
}

/// Compiles the library with this compiler and lays out the directory the runner is pointed at.
///
/// `-fPIC`, which is the whole of what this is about. Both ways of writing it, because the two are
/// separate code and only one of them was ever checked here. The listing is what the eleven
/// answers are taken from, so that a failure is something a person can read and so that the
/// assembler in the container is the one that has to agree with it rather than one more thing this
/// compiler is trusted about. The objects are what a real build gets, since nothing passes `-S` to
/// a compiler and then runs `as` by hand, and the one bug the listing path could not have shown is
/// the one in tamnd/rucc#1004.
fn build() -> Result<PathBuf> {
    let work = root().join("target").join("dso");
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

    let fixtures = root().join("tests").join("dso");
    for name in OURS {
        for (how, extension) in [("-S", "s"), ("-c", "o")] {
            let out = Command::new(&rucc)
                .args([how, &format!("--target={TRIPLE}"), "-fPIC", "-O1"])
                .arg("-o")
                .arg(work.join(format!("{name}.{extension}")))
                .arg(fixtures.join(format!("{name}.c")))
                .current_dir(root())
                .output()
                .map_err(|e| Error::Io(format!("could not run the compiler: {e}")))?;
            if !out.status.success() {
                return Err(Error::Failed {
                    task: "dso",
                    problems: vec![format!(
                        "{name}.c did not compile with {how}\n{}",
                        crate::indent(String::from_utf8_lossy(&out.stderr).trim_end())
                    )],
                });
            }
        }
    }
    // Copied rather than mounted from the tree, because the runner is given one directory and the
    // container mounts it read only.
    for name in ["main", "other", "preload"] {
        let source = fixtures.join(format!("{name}.c"));
        std::fs::copy(&source, work.join(format!("{name}.c")))
            .map_err(|e| Error::Io(format!("could not copy {}: {e}", source.display())))?;
    }
    std::fs::write(work.join("run.sh"), SCRIPT)
        .map_err(|e| Error::Io(format!("could not write the script: {e}")))?;
    Ok(work)
}

/// What the runner runs.
///
/// The order matters in one place: the library under test is linked against the other one, so that
/// one is built first. Everything is written under `/tmp` because the directory this reads from is
/// mounted read only.
///
/// Both the library and the program get an rpath, which looks like one too many until the link
/// fails without it. `ld` writes `DT_RUNPATH` rather than `DT_RPATH` now, and a runpath is used for
/// what that object names and not for what those objects name in turn, so the program's copy does
/// not help the library find the one it depends on.
///
/// The second library, the one built from objects, is the only step that does not end the run when
/// it fails. A link that stops is one message explaining itself, and exiting there would take the
/// other fifteen answers with it and leave a report saying nothing came back rather than a report
/// saying what the linker refused.
const SCRIPT: &str = "\
#!/bin/sh
out=/tmp/dso
mkdir -p \"$out\"
gcc -fPIC -shared other.c -o \"$out/libother.so\" || exit 1
gcc -fPIC -shared preload.c -o \"$out/libpreload.so\" || exit 1
gcc -shared one.s two.s -L\"$out\" -lother -Wl,-rpath,\"$out\" -o \"$out/libdso.so\" || exit 1
gcc main.c -L\"$out\" -ldso -lother -Wl,-rpath,\"$out\" -o \"$out/prog\" || exit 1
readelf --dyn-syms \"$out/libdso.so\" | sed 's/^/dynsym /'
if gcc -shared one.o two.o -L\"$out\" -lother -Wl,-rpath,\"$out\" -o \"$out/libobj.so\" \
        2> \"$out/said\"
then
    echo 'object link: ok'
    readelf --dyn-syms \"$out/libobj.so\" | sed 's/^/objsym /'
else
    echo 'object link: failed'
    sed 's/^/the linker said: /' \"$out/said\"
fi
\"$out/prog\" \"$out/libdso.so\" || exit 1
LD_PRELOAD=\"$out/libpreload.so\" \"$out/prog\" \"$out/libdso.so\" |
    sed -n 's/^interposed: /interposed under LD_PRELOAD: /p'
";

/// The names that get a row of their own, since a table has a hundred entries in it and four of
/// them are the question.
const WATCHED: [&str; 4] = ["exported", "shared_var", "hidden_helper", "protected_answer"];

/// Turns what the script printed into one answer per question.
///
/// The `readelf` lines are folded in here rather than compared where they are, so that the table
/// above is one list and a symbol that is not in the output at all reads the same way as an answer
/// that came back wrong.
fn read(printed: &str) -> BTreeMap<String, String> {
    let mut said = BTreeMap::new();
    for name in WATCHED {
        said.insert(format!("dynamic symbol {name}"), "absent".to_owned());
    }
    for name in WATCHED {
        said.insert(format!("dynamic symbol {name} from the object path"), "absent".to_owned());
    }
    for line in printed.lines() {
        // The same table read from each of the two libraries, which is why the prefix decides
        // which set of rows the answer lands in rather than the line deciding anything.
        let table = [("dynsym ", ""), ("objsym ", " from the object path")]
            .into_iter()
            .find_map(|(prefix, whose)| line.strip_prefix(prefix).map(|rest| (rest, whose)));
        if let Some((rest, whose)) = table {
            // `num: value size type bind vis ndx name`, and a line that is not a symbol has fewer
            // fields than that, which is how the heading and the blank lines fall out.
            let fields: Vec<&str> = rest.split_whitespace().collect();
            let [.., visibility, _, name] = fields[..] else { continue };
            if fields.len() >= 8 && WATCHED.contains(&name) {
                said.insert(format!("dynamic symbol {name}{whose}"), visibility.to_owned());
            }
            continue;
        }
        if let Some((what, answer)) = line.split_once(": ") {
            said.insert(what.to_owned(), answer.trim().to_owned());
        }
    }
    said
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two shapes of line the script produces, read the way the checks read them.
    #[test]
    fn a_symbol_table_line_and_a_printed_line_both_land_in_the_same_map() {
        let printed = "\
dynsym Symbol table '.dynsym' contains 12 entries:
dynsym    Num:    Value          Size Type    Bind   Vis      Ndx Name
dynsym      7: 0000000000001119    11 FUNC    GLOBAL DEFAULT   12 exported
dynsym      8: 0000000000004010     4 OBJECT  GLOBAL PROTECTED 21 protected_answer
exported through dlsym: 7
shared variable the library reads: 42
";
        let said = read(printed);
        assert_eq!(said["dynamic symbol exported"], "DEFAULT");
        assert_eq!(said["dynamic symbol protected_answer"], "PROTECTED");
        assert_eq!(said["exported through dlsym"], "7");
        assert_eq!(said["shared variable the library reads"], "42");
    }

    /// A name the table does not carry is absent rather than missing, so the row that wants it
    /// says what was wrong instead of saying nothing said anything.
    #[test]
    fn a_name_the_table_does_not_carry_reads_as_absent() {
        let said = read("dynsym    Num:    Value          Size Type    Bind   Vis      Ndx Name\n");
        assert_eq!(said["dynamic symbol hidden_helper"], "absent");
        assert_eq!(said["dynamic symbol exported"], "absent");
    }
}
