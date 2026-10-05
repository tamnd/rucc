//! Turns `rules/wasm32.rules` into the table that the selector matches with.
//!
//! Design: the WebAssembly notes, section 6.5 (decision D14). Everything this script does is in
//! `rucc-rules`: it reads the rule file, builds the trie over the patterns, and emits the table as
//! Rust. It is the script that `rucc-codegen` runs over the native rules, for one file. The rules
//! live in this crate, so a published `rucc-wasm` builds from its own source archive, and
//! `rucc-verify` reads them from here too.

use std::path::Path;
use std::{env, fs, process};

fn main() {
    let manifest = env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
    let out = env::var("OUT_DIR").expect("cargo sets OUT_DIR");
    let name = "rules/wasm32.rules";
    println!("cargo::rerun-if-changed={name}");
    let source = Path::new(&manifest).join(name);
    let text = match fs::read_to_string(&source) {
        Ok(text) => text,
        Err(e) => fail(&format!("could not read {}: {e}", source.display())),
    };
    let rules = match rucc_rules::parse(name, &text) {
        Ok(rules) => rules,
        Err(errors) => fail(&errors[0].to_string()),
    };
    let matcher = match rucc_rules::Matcher::build(name, &rules) {
        Ok(matcher) => matcher,
        Err(errors) => fail(&errors[0].to_string()),
    };
    let table = match rucc_rules::emit(name, &rules, &matcher) {
        Ok(table) => table,
        Err(errors) => fail(&errors[0].to_string()),
    };
    let generated = Path::new(&out).join("wasm32.rs");
    if let Err(e) = fs::write(&generated, table) {
        fail(&format!("could not write {}: {e}", generated.display()));
    }
}

/// A rule file that does not compile stops the build with the message and nothing else.
fn fail(message: &str) -> ! {
    eprintln!("rucc-wasm: {message}");
    process::exit(1);
}
