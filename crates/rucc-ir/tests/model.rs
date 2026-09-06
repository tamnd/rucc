//! The IR model against the names the IR actually gives its instructions.
//!
//! `rules/ir.model` says what the terms of the IR mean, and `term::heads` says what the IR calls
//! its instructions. Those are two lists of the same names written by hand in two places, and a
//! name spelled one way here and another way there is a rule that never fires, which is the
//! quietest kind of mistake a rule set has. So they are checked against each other.
//!
//! The solver never notices any of this. A model entry for a head no instruction has is an entry
//! nothing asks about, and a head with no entry only shows up on the day somebody writes a rule
//! about it, which may be a year after the typo.

use std::collections::BTreeSet;

use rucc_ir::term::heads;

/// The model, read at compile time out of the crate it belongs to.
const MODEL: &str = include_str!("../rules/ir.model");

/// Every head the model gives a meaning to.
///
/// Scanned rather than parsed, because the parser is in a build tool this crate cannot see and
/// what is wanted is the name in head position of each entry. An entry always starts a line and
/// always opens with its head, which the model's own layout guarantees and which the model is
/// checked against below.
fn defined() -> BTreeSet<&'static str> {
    MODEL
        .lines()
        .filter_map(|line| line.strip_prefix("(semantics ("))
        .map(|rest| rest.split([' ', ')', '\n']).next().unwrap_or(rest))
        .collect()
}

/// Every name the IR gives an instruction.
fn named() -> BTreeSet<&'static str> {
    heads().into_iter().map(|(_, name)| name).collect()
}

/// The layout the scan above relies on: an entry starts a line and nothing else in the file
/// opens with `(semantics`. Checked from the other side, by counting, so that a continuation
/// line indented to look like an entry would be caught.
#[test]
fn every_entry_starts_a_line_of_its_own() {
    let written = MODEL.matches("(semantics ").count();
    assert_eq!(defined().len(), written, "an entry is not where the scan looks for it");
}

/// A head with no meaning is a rule nobody can write, so the two lists are compared both ways
/// and the difference is named rather than counted.
///
/// The heads the IR has and the model does not are memory, and they are the list below rather
/// than an allowance for anything missing. What a load of four bytes means depends on which end
/// of the value sits at the lowest address, which is a fact about the target, so those entries
/// live in the target's model and the two that exist there today are written out.
#[test]
fn a_head_with_no_meaning_here_is_one_whose_meaning_belongs_to_the_target() {
    let missing: Vec<&str> = named().difference(&defined()).copied().collect();
    assert_eq!(
        missing,
        [
            "load.f32",
            "load.f64",
            "load.i16",
            "load.i32",
            "load.i64",
            "load.i8",
            "store.f32",
            "store.f64",
            "store.i16",
            "store.i32",
            "store.i64",
            "store.i8",
        ]
    );
}

/// And nothing here is a meaning for a head no instruction has.
///
/// `value.iN` is the exception, and it is the only one. It is not an instruction: it is how a
/// rule writes an operand that is already computed and sitting in a register, so no opcode is
/// ever called that and the sweep has no reason to produce the name.
#[test]
fn nothing_here_gives_a_meaning_to_a_head_no_instruction_has() {
    let named = named();
    let extra: Vec<&str> = defined()
        .into_iter()
        .filter(|head| !named.contains(head) && !head.starts_with("value."))
        .collect();
    assert!(extra.is_empty(), "the model names something the IR does not: {extra:?}");
}

/// One bit is the width the rewrite rules of `rucc-opt` are about as much as any other, so its
/// meanings are here rather than in a target's model. They were in one, back when a target's
/// model was the only model there was.
#[test]
fn one_bit_has_its_meanings_here() {
    let defined = defined();
    for head in ["value.i1", "iconst.i1", "and.i1", "or.i1", "xor.i1", "brif.i1", "zext.i1.i32"] {
        assert!(defined.contains(head), "{head} has no meaning in the IR model");
    }
}
