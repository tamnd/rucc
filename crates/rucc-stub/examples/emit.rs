//! Writes a sysroot's worth of libraries per ELF target, for a reader nobody here wrote to read back.
//!
//! `cargo xtask stubs` runs this and then runs `readelf` over everything it produced. The split is
//! the same one `cargo run -p rucc-target --example listing` and `cargo xtask disasm` use, and for
//! the same reason: xtask depends on no crate in the workspace, so the data it checks has to arrive
//! as output rather than as a call.
//!
//! Takes one argument, a directory to write into, and makes one subdirectory per target holding a
//! `libc` stub and every empty compatibility library section 9.9 says that target's link lines will
//! ask for. One line is printed per file, bars between the fields, so that the checker can hold a
//! reader's output to what went in without keeping its own copy of the description:
//!
//! ```text
//! x86_64-linux-gnu/libc.so|shared|<path>|libc.so.6|printf,malloc,...
//! x86_64-linux-musl/libm.a|archive|<path>
//! loongarch64-linux-gnu|refused: what belongs in e_flags for loongarch64 is not decided yet
//! ```
//!
//! A refusal is reported once for the target rather than once per file, since the reason is always a
//! fact about the target. It is an answer rather than a failure, and
//! `spec/cross-compile/09-libc-stubs.md` section 9.1's argument for refusing over guessing depends on
//! the refusals staying visible.

use std::path::{Path, PathBuf};

use rucc_stub::{Compat, Form, Library, Symbol};
use rucc_tuple::{ObjectFormat, TARGETS, TargetTuple};

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(into) = args.next().map(PathBuf::from) else {
        eprintln!("emit: wants one argument, the directory to write the libraries into");
        std::process::exit(2);
    };
    if let Err(why) = std::fs::create_dir_all(&into) {
        eprintln!("emit: cannot make {}: {why}", into.display());
        std::process::exit(2);
    }

    for entry in TARGETS {
        let target: TargetTuple = match entry.tuple.parse() {
            Ok(target) => target,
            Err(why) => {
                println!("{}|refused: the tuple does not parse: {why}", entry.tuple);
                continue;
            }
        };
        // The Darwin, Windows and WebAssembly rows want a different container, and section 9.7 says
        // the first of them wants nothing from this writer at all, so they are not a gap to report.
        if target.object_format() != ObjectFormat::Elf {
            continue;
        }
        sysroot(&into, entry.tuple, target);
    }
}

/// Everything one target's sysroot needs from this crate, written into a directory of its own.
///
/// The libc stub comes first and the empty compatibility libraries after it, which is the order
/// somebody reading the output would expect and also the order in which a failure is informative: if
/// the libc cannot be written then nothing about that target can be, and saying so once is enough.
fn sysroot(into: &Path, tuple: &str, target: TargetTuple) {
    let dir = into.join(tuple);
    if let Err(why) = std::fs::create_dir_all(&dir) {
        println!("{tuple}|refused: cannot make a directory for it: {why}");
        return;
    }

    let libc = libc(tuple);
    let bytes = match rucc_stub::write(&libc, target) {
        Ok(bytes) => bytes,
        Err(why) => {
            // One line for the target, not one per file. Every file here would fail for the same
            // reason and four copies of it read as four problems.
            println!("{tuple}|refused: {why}");
            return;
        }
    };
    let symbols: Vec<String> = libc.symbols.iter().map(spelling).collect();
    // `libc.so` rather than `libc.so.6`, because the file name is what `-lc` opens and the `SONAME`
    // inside it is what the loader is told to find. A distribution has both and one is a link to the
    // other, and a sysroot that only has the second leaves the linker with nothing to open.
    write(&dir, tuple, "libc.so", &bytes, Some((&libc.soname, &symbols.join(","))));

    // The empty libraries of section 9.9. These are the case most likely to be got wrong, because
    // every count in the file is at its smallest and a table of nothing still has to be walkable by
    // something that does not know yet that it is empty.
    for one in rucc_stub::compat(target) {
        let Compat { file, form } = &one;
        match one.bytes(target) {
            Ok(bytes) => {
                let soname = match form {
                    Form::Shared(soname) => Some((soname.as_str(), "")),
                    Form::Archive => None,
                };
                write(&dir, tuple, file, &bytes, soname);
            }
            Err(why) => println!("{tuple}/{file}|refused: {why}"),
        }
    }
}

/// Writes one file and prints the line describing it.
///
/// A shared object carries a `SONAME` and a list of symbols for the reader to be held to. An archive
/// carries neither, and saying so in the line is what keeps the checker from handing eight bytes of
/// archive magic to a program that reads ELF.
fn write(dir: &Path, tuple: &str, file: &str, bytes: &[u8], shared: Option<(&str, &str)>) {
    let path = dir.join(file);
    if let Err(why) = std::fs::write(&path, bytes) {
        println!("{tuple}/{file}|refused: the file could not be written: {why}");
        return;
    }
    match shared {
        Some((soname, symbols)) => {
            println!("{tuple}/{file}|shared|{}|{soname}|{symbols}", path.display());
        }
        None => println!("{tuple}/{file}|archive|{}", path.display()),
    }
}

/// How a reader will print one symbol, which is the name and the version node if it has one.
///
/// Both readers spell a default definition `name@@NODE` and a superseded one `name@NODE`, and they
/// take the node from `.gnu.version_d` through the index in `.gnu.version`. So holding a reader to
/// this one string holds it to both version tables and to the agreement between them, which is most of
/// what section 9.2 asks the writer to get right.
fn spelling(symbol: &Symbol) -> String {
    match &symbol.version {
        None => symbol.name.clone(),
        Some(version) => {
            let at = if version.default { "@@" } else { "@" };
            format!("{}{at}{}", symbol.name, version.node)
        }
    }
}

/// A libc with one of everything that can vary, spelled the way the target's libc spells it.
///
/// The contents barely matter, because what is being checked is the container. What does matter is
/// that a weak symbol, an object with a size and a function with none are all present, since those
/// are three of the rows in section 9.1's table of the things a stub must get exactly right.
///
/// The glibc one carries version nodes and the musl one does not, because that is the difference
/// between the two libcs and a writer that produced version tables for musl would be describing a
/// library musl does not ship. `memcpy` is at two nodes, which is the shape a reader has the most
/// ways to get wrong: two definitions of one name, one of them the one an unversioned reference takes.
fn libc(tuple: &str) -> Library {
    let musl = tuple.contains("musl");
    let mut library = Library::new(if musl { "libc.so" } else { "libc.so.6" });
    if !musl {
        library.needs("ld-linux.so.2");
    }
    library
        .function("printf")
        .function("malloc")
        .function("free")
        .object("environ", 8)
        .object("stdout", 8)
        .export(Symbol::function("pthread_cancel").weak())
        .export(Symbol::object("__progname", 8).weak());
    if !musl {
        let node = "GLIBC_2.2.5";
        for symbol in &mut library.symbols {
            symbol.version = Some(rucc_stub::Version { node: node.to_owned(), default: true });
        }
        library
            .export(Symbol::function("memcpy").at("GLIBC_2.14"))
            .export(Symbol::function("memcpy").behind(node))
            // Not everything a real glibc exports is versioned, so one here is not either.
            .function("__libc_start_main");
    }
    library
}
