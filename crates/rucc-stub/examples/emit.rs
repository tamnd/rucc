//! Writes one stub per ELF target, for a reader nobody here wrote to read back.
//!
//! `cargo xtask stubs` runs this and then runs `readelf` over everything it produced. The split is
//! the same one `cargo run -p rucc-target --example listing` and `cargo xtask disasm` use, and for
//! the same reason: xtask depends on no crate in the workspace, so the data it checks has to arrive
//! as output rather than as a call.
//!
//! Takes one argument, a directory to write into, and prints one line per target. A line is bars
//! between the name, the file, the `SONAME` and the symbols, so that the checker can hold a reader's
//! output to what went in without keeping its own copy of the description. A target whose stub the
//! writer refuses prints `refused: ` and what it said, and that is an answer rather than a failure,
//! because `spec/cross-compile/09-libc-stubs.md` section 9.1's argument for refusing over guessing
//! depends on the refusals staying visible.

use std::path::{Path, PathBuf};

use rucc_stub::{Library, Symbol};
use rucc_tuple::{ObjectFormat, TARGETS, TargetTuple};

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(into) = args.next().map(PathBuf::from) else {
        eprintln!("emit: wants one argument, the directory to write the stubs into");
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
        emit(&into, entry.tuple, &libc(entry.tuple), target);
    }

    // The empty library of section 9.9, which is a real case and the one most likely to be got
    // wrong, because every count in the file is at its smallest and a table of nothing still has to
    // be walkable by something that does not know yet that it is empty.
    let target: TargetTuple = "x86_64-linux-gnu".parse().expect("a row in the table");
    emit(&into, "empty", &Library::new("libm.so.6"), target);
}

/// Writes one stub and prints the line describing it.
fn emit(into: &Path, name: &str, library: &Library, target: TargetTuple) {
    let bytes = match rucc_stub::write(library, target) {
        Ok(bytes) => bytes,
        Err(why) => {
            println!("{name}|refused: {why}");
            return;
        }
    };
    let path = into.join(format!("{name}.so"));
    if let Err(why) = std::fs::write(&path, bytes) {
        println!("{name}|refused: the file could not be written: {why}");
        return;
    }
    let symbols: Vec<&str> = library.symbols.iter().map(|symbol| symbol.name.as_str()).collect();
    println!("{name}|{}|{}|{}", path.display(), library.soname, symbols.join(","));
}

/// A libc with one of everything that can vary, spelled the way the target's libc spells it.
///
/// The contents barely matter, because what is being checked is the container. What does matter is
/// that a weak symbol, an object with a size and a function with none are all present, since those
/// are three of the rows in section 9.1's table of the things a stub must get exactly right.
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
    library
}
