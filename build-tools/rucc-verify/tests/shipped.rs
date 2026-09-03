//! The shipped rule sets, checked as far as a machine without a solver can check them.
//!
//! The gate itself is `cargo run -p rucc-verify -- crates/rucc-codegen/rules` and it needs z3.
//! Most of what it does does not: reading every file, building the matcher a rule set compiles
//! into, and turning every rule into the question that would be asked all happen before any
//! solver is started, and all three are things a change to the model or to the width rules can
//! break. So they are a test, and what a machine without a solver loses is the answers rather
//! than the questions.

use std::fs;
use std::path::{Path, PathBuf};

use rucc_rules::{Matcher, parse};
use rucc_verify::{Model, query};

/// The rule directory, found from this crate rather than from the working directory. The
/// rules live in the crate that compiles them, per spec/18-package-layout.md.
fn rules_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/rucc-codegen/rules")
        .canonicalize()
        .expect("the rule directory")
}

fn files() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = fs::read_dir(rules_dir())
        .expect("the rule directory is readable")
        .map(|entry| entry.expect("an entry").path())
        .filter(|path| path.extension().is_some_and(|kind| kind == "rules"))
        .collect();
    out.sort();
    out
}

#[test]
fn there_is_a_rule_set_to_check() {
    assert!(!files().is_empty(), "there are no rule files, and this test is about them");
}

#[test]
fn every_shipped_rule_has_a_question_to_ask() {
    for file in files() {
        let shown = file.display().to_string();
        let text = fs::read_to_string(&file).expect("the rule file is readable");
        let rules = match parse(&shown, &text) {
            Ok(rules) => rules,
            Err(errors) => panic!("{}", errors[0]),
        };

        // A rule an earlier rule already covers can never fire, which is a mistake whatever a
        // solver would have said about it.
        if let Err(errors) = Matcher::build(&shown, &rules) {
            panic!("{}", errors[0]);
        }

        let model_path = file.with_extension("model");
        let model_text = fs::read_to_string(&model_path).expect("a model beside the rules");
        let model = match Model::read(&model_path.display().to_string(), &model_text) {
            Ok(model) => model,
            Err(errors) => panic!("{}", errors[0]),
        };

        // This is where a head with no entry in the model, a replacement narrower than what it
        // replaces, and two operands of one instruction disagreeing about their width all come
        // out, none of which needs anybody to be asked anything.
        for rule in &rules {
            if let Err(problem) = query(&shown, rule, &model) {
                panic!("{problem}");
            }
        }
    }
}
