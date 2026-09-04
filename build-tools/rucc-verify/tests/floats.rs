//! What a rule about a float claims, and what has to be true before it may enter the rule set.
//!
//! A float is the second kind of thing a term can compute that is not a number of bits, and it is
//! the more dangerous of the two: a memory is obviously not a bitvector and a float looks exactly
//! like one, right down to occupying the same thirty two or sixty four bits. The questions here
//! are mostly about keeping those apart, because a rule that lowers float arithmetic to an integer
//! instruction is a rule that would otherwise prove itself against a model of the wrong operation.
//!
//! The rounding is the other half. Every arithmetic here is asked with rounding to nearest and
//! ties to even, which is the mode a C program runs in unless it asks for another, and the mode is
//! written in by the verifier rather than by each rule so that no rule can differ from the rest.

use rucc_rules::{Rule, parse};
use rucc_verify::{Model, Solver, Verdict, admit, query, verify};

/// A model of float arithmetic and of the instructions that do it.
///
/// Both halves written out rather than one named in terms of the other, which is the arrangement
/// the shipped model uses and the reason a mistake in one half is a mistake the other disagrees
/// with.
const MODEL: &str = "\
(semantics (value.f32 v) v)
(semantics (value.f64 v) v)
(semantics (value.i32 v) v)
(semantics (fadd.f32 l r) (fp.add l r))
(semantics (fadd.f64 l r) (fp.add l r))
(semantics (fdiv.f32 l r) (fp.div l r))
(semantics (add.i32 l r) (bvadd l r))
(semantics (x64.addss_rr l r) (fp.add l r))
(semantics (x64.addsd_rr l r) (fp.add l r))
(semantics (x64.divss_rr l r) (fp.div l r))
(semantics (x64.add_rr_32 l r) (bvadd l r))";

/// Adding two floats, which is the smallest rule about one there is.
const ADD: &str = "\
(rule (lower (fadd.f32 (value.f32 x) (value.f32 y)))
      (x64.addss_rr x y)
      (spec (= (fp.add x y) (result))))";

fn rules(text: &str) -> Vec<Rule> {
    match parse("t.rules", text) {
        Ok(rules) => rules,
        Err(errors) => panic!("{}", errors[0]),
    }
}

fn model() -> Model {
    match Model::read("t.model", MODEL) {
        Ok(model) => model,
        Err(errors) => panic!("{}", errors[0]),
    }
}

/// The tests that need a solver skip when there is none, so that a machine without one still
/// runs everything else. CI sets the variable and so has to have one.
fn solver() -> Option<Solver> {
    let found = Solver::find();
    assert!(
        !(found.is_none() && std::env::var_os("RUCC_REQUIRE_SOLVER").is_some()),
        "no solver on PATH, and RUCC_REQUIRE_SOLVER says there has to be one"
    );
    found
}

#[test]
fn a_rule_that_reaches_a_float_is_asked_in_the_theory_that_has_it() {
    let asked = query("t.rules", &rules(ADD)[0], &model()).expect("the model covers this rule");
    assert!(asked.starts_with("(set-logic QF_FPBV)\n"), "{asked}");

    // The two names the pattern binds are floats rather than the thirty two bits they sit in,
    // which is the whole of what this file is about.
    assert!(asked.contains("(declare-const x Float32)"), "{asked}");
    assert!(asked.contains("(declare-const y Float32)"), "{asked}");
}

#[test]
fn a_rule_that_has_no_float_is_asked_exactly_as_it_was_before() {
    let text = "\
(rule (lower (add.i32 (value.i32 x) (value.i32 y)))
      (x64.add_rr_32 x y)
      (spec (= (bvadd x y) (result))))";
    let asked = query("t.rules", &rules(text)[0], &model()).expect("the model covers this rule");

    // Adding floats to the language cost the rules that have nothing to do with them nothing at
    // all, which is the property that makes it safe to have added.
    assert!(asked.starts_with("(set-logic QF_BV)\n"), "{asked}");
    assert!(!asked.contains("Float"), "{asked}");
}

#[test]
fn nothing_in_a_rule_says_a_rounding_and_the_verifier_is_what_writes_one() {
    let asked = query("t.rules", &rules(ADD)[0], &model()).expect("the model covers this rule");

    // The rule says `(fp.add x y)` and the question says `(fp.add RNE x y)`. One place decides
    // the rounding for every rule, so no two rules can be proved under different ones.
    assert!(!ADD.contains("RNE"));
    assert!(asked.contains("(fp.add RNE x y)"), "{asked}");
}

#[test]
fn the_widest_float_the_machine_has_is_not_one_of_these() {
    // A `long double` is eighty bits on the x87 stack. That is not one of the interchange
    // formats and there is no name for it to be written under, so a rule about one is a rule
    // nobody has said the meaning of. `crates/rucc-codegen/src/abi.rs` refuses one for the same
    // reason at the other end of the compiler.
    let text = "\
(rule (lower (fadd.f80 (value.f80 x) (value.f80 y)))
      (x64.addss_rr x y)
      (spec (= (fp.add x y) (result))))";
    let problem =
        query("t.rules", &rules(text)[0], &model()).expect_err("this rule cannot be asked");
    assert!(problem.message.contains("`value.f80`"), "{}", problem.message);
}

#[test]
fn a_float_lowered_to_an_integer_instruction_is_refused() {
    let text = "\
(rule (lower (fadd.f32 (value.f32 x) (value.f32 y)))
      (x64.add_rr_32 x y)
      (spec (= (fp.add x y) (result))))";
    let problem =
        query("t.rules", &rules(text)[0], &model()).expect_err("this rule cannot be asked");

    // Two floats and the bits of two floats are the same size and are not the same thing. The
    // instruction that adds the bits computes something else, and no width makes it right.
    assert!(problem.message.contains("`bvadd` works on bitvectors"), "{}", problem.message);
    assert!(problem.message.contains("32 bits of float"), "{}", problem.message);
}

#[test]
fn an_integer_lowered_to_a_float_instruction_is_refused() {
    let text = "\
(rule (lower (add.i32 (value.i32 x) (value.i32 y)))
      (x64.addss_rr x y)
      (spec (= (bvadd x y) (result))))";
    let problem =
        query("t.rules", &rules(text)[0], &model()).expect_err("this rule cannot be asked");
    assert!(problem.message.contains("`fp.add` works on floats"), "{}", problem.message);
}

#[test]
fn two_formats_in_one_operation_is_refused() {
    let text = "\
(rule (lower (fadd.f32 (value.f32 x) (value.f64 y)))
      (x64.addss_rr x y)
      (spec (= (fp.add x y) (result))))";
    let problem =
        query("t.rules", &rules(text)[0], &model()).expect_err("this rule cannot be asked");
    assert!(
        problem.message.contains("32 bits of float and something 64 bits of float"),
        "{}",
        problem.message
    );
}

#[test]
fn the_arithmetic_is_discharged_at_both_formats() {
    let Some(solver) = solver() else {
        return;
    };
    let text = format!(
        "{ADD}
(rule (lower (fadd.f64 (value.f64 x) (value.f64 y)))
      (x64.addsd_rr x y)
      (spec (= (fp.add x y) (result))))"
    );
    let report = admit("t.rules", &rules(&text), &model(), &solver).expect("both are provable");

    // At the format each is written in, and not at a narrower one. There is no narrower float to
    // fall back to, so a float rule is either proved where it runs or not proved.
    assert_eq!(report.discharged(), 2, "{report}");
    assert_eq!(report.bounded(), 0, "{report}");
}

#[test]
fn a_model_that_has_the_operation_wrong_is_refuted() {
    let Some(solver) = solver() else {
        return;
    };
    // Division rather than addition on the machine half. The two agree on nothing, but a solver
    // that had been told floats are bitvectors would have been comparing two things it did not
    // understand and could have agreed with either.
    let wrong = MODEL.replace(
        "(semantics (x64.addss_rr l r) (fp.add l r))",
        "(semantics (x64.addss_rr l r) (fp.div l r))",
    );
    let model = Model::read("t.model", &wrong).expect("the model reads");
    let report = verify("t.rules", &rules(ADD), &model, &solver).expect("the question is asked");
    assert!(
        matches!(report.verdicts[0], Verdict::Refuted(_)),
        "the wrong operation got through: {report}"
    );
}

#[test]
fn a_division_by_zero_is_a_value_here_and_the_rule_covers_it() {
    let Some(solver) = solver() else {
        return;
    };
    // The integer division rules carry a guard because the IR only ever holds a division the
    // language says has a divisor. A float division has no guard and needs none: every pair of
    // floats has an answer, zero and the not a number included, and the rule claims the machine
    // computes it for all of them.
    let text = "\
(rule (lower (fdiv.f32 (value.f32 x) (value.f32 y)))
      (x64.divss_rr x y)
      (spec (= (fp.div x y) (result))))";
    let report = admit("t.rules", &rules(text), &model(), &solver).expect("this is provable");
    assert_eq!(report.discharged(), 1, "{report}");
    assert!(!text.contains("(if "), "a float division needs no guard");
}
