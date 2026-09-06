//! Turns the rewrite rules into the table the simplifier matches with.
//!
//! Design: `spec/optimizer/13-rewrite-rules.md`, which asks for the rewrites to be written as
//! rules that a solver can be asked about rather than as `match` arms nobody can check.
//! Everything this script does is in `rucc-rules`: it reads a rule file, builds the trie over the
//! patterns, and emits the table as Rust. What is here is which files to do it to and where to
//! put the answer, and it is the same script `rucc-codegen` runs over its lowering rules.
//!
//! The rule files live in this crate rather than at the root of the repository, which is what
//! makes a published `rucc-opt` build from its own source archive. `rucc-verify` reads them from
//! here too, so there is still one copy of them and one gate over it.

use std::path::Path;
use std::{env, fs, process};

/// Every rule file this crate compiles.
///
/// One per tier of `spec/optimizer/13-rewrite-rules.md` section 13.4, added here when its file is
/// written, and the list is short on purpose: a rule file nobody compiles is a rule file nobody
/// notices has stopped compiling.
///
/// The order is the order `crate::simplify` tries them in, which is nearly the order the tiers are
/// numbered. Tier one takes an operation away and tier two swaps one for another, so a term both
/// have something to say about is better off losing the operation, and tier four takes one away as
/// well. Tier three is last rather than third because it only rearranges a term so that another
/// rule can be about it, and there is no reason to reach for that while a rule that improves the
/// code still fires. No instruction matches both anyway, since tier three is about a commutative
/// operation with a constant and tier four is about a conversion. Tier five goes between them for
/// no reason at all: it is the only one about a comparison, so there is no term another tier is
/// also about and nothing that depends on whether it is tried first or last.
const SETS: &[&str] = &["simplify", "strength", "width", "compare", "canonical"];

fn main() {
    let manifest = env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
    let out = env::var("OUT_DIR").expect("cargo sets OUT_DIR");
    for set in SETS {
        let name = format!("rules/{set}.rules");
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
        let generated = Path::new(&out).join(format!("{set}.rs"));
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
    eprintln!("rucc-opt: {message}");
    process::exit(1);
}
