//! Writes a libc stub from a real glibc `abilist`, so that it can be held against a real `libc.so.6`.
//!
//! `cargo xtask real-libc` runs this and then reads both files with `readelf`. The split is the one
//! `examples/emit.rs` uses and for the same reason: xtask depends on no crate in the workspace, so
//! anything it checks has to arrive as output rather than as a call.
//!
//! This is the third of `spec/cross-compile/09-libc-stubs.md` section 9.8's correctness properties,
//! the one it calls the highest value test in the document, and it is the only one that can tell a
//! description that disagrees with reality from one that agrees with itself. The other two are the
//! round trip in `tests/roundtrip.rs` and `cargo xtask stubs`.
//!
//! The path taken is the one a distribution would take, which is why the blob is in it. The text
//! becomes an [`rucc_stub::abilist::Exports`], the exports become a packed blob, the blob is read
//! back, the nodes above the ceiling are dropped, and the result is written as an ELF stub. A bug
//! anywhere along that line shows up here as a symbol the real library does not have or a version it
//! does not agree with.
//!
//! ```text
//! from-abilist <abilist> <tuple> <node|-> <file>
//! ```
//!
//! The node is the ceiling, as `GLIBC_2.39`, and a dash means keep everything the file says. One
//! `key=value` line is printed per thing worth knowing, so the checker can report what it compared
//! without keeping a second copy of it.

use std::path::PathBuf;

use rucc_stub::{Library, blob};
use rucc_tuple::TargetTuple;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [abilist, tuple, node, file] = args.as_slice() else {
        eprintln!("from-abilist: wants an abilist, a tuple, a node or a dash, and a file to write");
        std::process::exit(2);
    };
    let into = PathBuf::from(file);

    let target: TargetTuple = match tuple.parse() {
        Ok(target) => target,
        Err(why) => die(&format!("{tuple} does not parse as a target: {why}")),
    };
    let text = match std::fs::read_to_string(abilist) {
        Ok(text) => text,
        Err(why) => die(&format!("{abilist} cannot be read: {why}")),
    };

    let listed = match rucc_stub::abilist::read(&text) {
        Ok(exports) => exports,
        Err(why) => die(&format!("{abilist} is not an abilist: {why}")),
    };
    println!("listed={}", listed.symbols.len());
    println!("private={}", listed.skipped);

    // Through the blob rather than straight from the text, because the blob is how section 9.2 says
    // the description travels and a test that skips it tests a path nothing will ship.
    let packed = match blob::pack(&[(tuple.as_str(), &listed)]) {
        Ok(bytes) => bytes,
        Err(why) => die(&format!("the description will not pack: {why}")),
    };
    println!("packed={}", packed.len());
    let carried = match blob::Blob::read(&packed) {
        Ok(blob) => blob,
        Err(why) => die(&format!("the packed description will not read back: {why}")),
    };
    // A dash asks for everything the description has, and a node asks for section 9.2's filtering,
    // which is how a ceiling taken from a real library reaches the symbol set.
    let found = if node == "-" { carried.exports(tuple) } else { carried.exports_at(tuple, node) };
    let exports = match found {
        Ok(exports) => exports,
        Err(why) => die(&format!("the description has nothing for {tuple} at {node}: {why}")),
    };
    println!("ceiling={node}");
    println!("exported={}", exports.symbols.len());

    // `libc.so.6` because that is what the real library calls itself, and the comparison is against
    // the real library. The loader the description needs is not in an abilist and is not what this
    // checks, so nothing is invented for it.
    let mut library = Library::new("libc.so.6");
    for symbol in exports.symbols {
        library.export(symbol);
    }
    let bytes = match rucc_stub::write(&library, target) {
        Ok(bytes) => bytes,
        Err(why) => die(&format!("the stub for {tuple} will not write: {why}")),
    };
    if let Err(why) = std::fs::write(&into, &bytes) {
        die(&format!("{} cannot be written: {why}", into.display()));
    }
    println!("stub={}", into.display());
    println!("bytes={}", bytes.len());
}

/// Says what went wrong on the error stream and stops, which is what every failure here is.
///
/// One exit code for all of them, because the caller's question is whether there is a stub to
/// compare and not which step did not produce one. The message is what says that.
fn die(why: &str) -> ! {
    eprintln!("from-abilist: {why}");
    std::process::exit(2);
}
