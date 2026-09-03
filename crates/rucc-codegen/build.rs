//! Turns the rule files into the tables the selector matches with.
//!
//! Design: `spec/10-backend.md` section 10.2, which asks for a generated automaton rather than a
//! chain of conditionals and for lowering to be written as rules rather than as `match` arms.
//! Everything this script does is in `rucc-rules`: it reads a rule file, builds the trie over
//! the patterns, and emits the table as Rust. What is here is which files to do it to and where
//! to put the answer.
//!
//! The rule files live in this crate rather than at the root of the repository, which is what
//! makes a published `rucc-codegen` build from its own source archive. `rucc-verify` reads them
//! from here too, so there is still one copy of them and one gate over it.

use std::path::Path;
use std::{env, fs, process};

/// Every rule file this crate compiles, and the module that includes what it turns into.
///
/// A target is added here when its rule file is written. The list is short on purpose: a rule
/// file nobody compiles is a rule file nobody notices has stopped compiling.
const TARGETS: &[&str] = &["x86-64"];

fn main() {
    let manifest = env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
    let out = env::var("OUT_DIR").expect("cargo sets OUT_DIR");
    for target in TARGETS {
        let name = format!("rules/{target}.rules");
        println!("cargo::rerun-if-changed={name}");
        let source = Path::new(&manifest).join(&name);
        let text = match fs::read_to_string(&source) {
            Ok(text) => text,
            Err(e) => fail(&format!("could not read {}: {e}", source.display())),
        };
        // The name in the generated file is the one a person could open, rather than wherever
        // this build happened to run from, because it is what a report about a rule will print.
        let rules = match rucc_rules::parse(&name, &text) {
            Ok(rules) => rules,
            Err(errors) => fail(&errors[0].to_string()),
        };
        let matcher = match rucc_rules::Matcher::build(&name, &rules) {
            Ok(matcher) => matcher,
            Err(errors) => fail(&errors[0].to_string()),
        };
        let table = match rucc_rules::emit(&name, &rules, &matcher) {
            Ok(table) => table,
            Err(errors) => fail(&errors[0].to_string()),
        };
        let generated = Path::new(&out).join(format!("{target}.rs"));
        if let Err(e) = fs::write(&generated, table) {
            fail(&format!("could not write {}: {e}", generated.display()));
        }
    }
}

/// A rule file that will not compile stops the build with the message and nothing else.
///
/// A panic here would bury the line and column under a backtrace and the words "build script
/// panicked", and what has gone wrong is a rule somebody is editing.
fn fail(message: &str) -> ! {
    eprintln!("rucc-codegen: {message}");
    process::exit(1);
}
