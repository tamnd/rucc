//! What a rule with an effect claims, and what has to be true before it may enter the rule set.
//!
//! A rule that reads or writes memory is the first kind whose two halves are not both numbers, so
//! the questions here are about the sort of what a term computes as much as about its value. The
//! one that matters most is the last: a model that gets the byte order backwards has to be caught,
//! because nothing else will catch it on a machine that is little endian all the way down.

use rucc_rules::{Rule, parse};
use rucc_verify::{Model, Solver, Verdict, admit, query, verify};

/// A model of memory, and of two ways of reaching it.
///
/// Deliberately small: two widths rather than four, and both halves of each written out rather
/// than one named in terms of the other, which is the arrangement the shipped model uses and the
/// reason a mistake in one half is a mistake the other half disagrees with.
const MODEL: &str = "\
(semantics (value.i64 v) v)
(semantics (value.i8 v) v)
(semantics (value.i16 v) v)
(semantics (amode_base base) base)
(semantics (load.i8 a) (select (mem) a))
(semantics (load.i16 a) (concat (select (mem) (bvadd a 1)) (select (mem) a)))
(semantics (store.i8 v a) (store (mem) a v))
(semantics (x64.mov_rm_8 a) (select (mem) a))
(semantics (x64.mov_rm_16 a) (concat (select (mem) (bvadd a 1)) (select (mem) a)))
(semantics (x64.mov_mr_8 a v) (store (mem) a v))
(semantics (x64.add_rr_64 l r) (bvadd l r))
(semantics (add.i64 l r) (bvadd l r))";

/// A load of one byte, which is the smallest rule with an effect there is.
const LOAD: &str = "\
(rule (lower (load.i8 (value.i64 a)))
      (x64.mov_rm_8 (amode_base a))
      (spec (= (select (mem) a) (result))))";

/// A store of one byte, whose replacement computes a memory and no value at all.
const STORE: &str = "\
(rule (lower (store.i8 (value.i8 v) (value.i64 a)))
      (x64.mov_mr_8 (amode_base a) v)
      (spec (= (store (mem) a v) (result))))";

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
fn a_rule_that_reaches_memory_is_asked_in_the_theory_that_has_it() {
    let asked = query("t.rules", &rules(LOAD)[0], &model()).expect("the model covers this rule");
    assert!(asked.starts_with("(set-logic QF_ABV)\n"), "{asked}");
    assert!(asked.contains("(declare-const mem (Array (_ BitVec 64) (_ BitVec 8)))"), "{asked}");
}

#[test]
fn a_rule_that_does_not_reach_memory_is_asked_exactly_as_it_was_before() {
    let text = "\
(rule (lower (add.i64 (value.i64 x) (value.i64 y)))
      (x64.add_rr_64 x y)
      (spec (= (bvadd x y) (result))))";
    let asked = query("t.rules", &rules(text)[0], &model()).expect("the model covers this rule");

    // Adding memory to the language cost the rules that have nothing to do with it nothing at
    // all, which is the property that makes it safe to have added.
    assert!(asked.starts_with("(set-logic QF_BV)\n"), "{asked}");
    assert!(!asked.contains("mem"), "{asked}");
}

#[test]
fn nothing_in_a_rule_says_memory_and_the_model_is_what_reaches_it() {
    // The rule text says `load.i8` and never says `mem`. What makes this a question about
    // memory is the model entry for that head, so the check has to expand the model rather than
    // read the surface of the rule, and this is the test that says so.
    let pattern = &rules(LOAD)[0].pattern;
    assert_eq!(pattern.to_string(), "(load.i8 (value.i64 a))");
    assert!(model().touches_memory(pattern));
}

#[test]
fn a_store_computes_a_memory_and_that_is_what_its_specification_reads() {
    let asked = query("t.rules", &rules(STORE)[0], &model()).expect("the model covers this rule");

    // Both halves are stores into the same memory, and the claim is that they are the same
    // memory afterwards. Nothing in the question is a comparison of numbers, which is the
    // whole difference between a rule with an effect and a rule without one.
    assert!(asked.contains("(store mem a v)"), "{asked}");
    assert!(asked.starts_with("(set-logic QF_ABV)\n"), "{asked}");
}

#[test]
fn a_rule_that_replaces_a_memory_with_a_value_is_refused() {
    let text = "\
(rule (lower (store.i8 (value.i8 v) (value.i64 a)))
      (x64.mov_rm_8 (amode_base a))
      (spec (= (store (mem) a v) (result))))";
    let problem =
        query("t.rules", &rules(text)[0], &model()).expect_err("this rule cannot be asked");

    // A store lowered to a load is not a rule with a mistake in its widths. There is no width
    // that would make it right, and saying so is more use than reporting a mismatch of bits.
    assert!(problem.message.contains("computes a memory"), "{}", problem.message);
}

#[test]
fn a_load_and_a_store_are_discharged() {
    let Some(solver) = solver() else {
        return;
    };
    let text = format!("{LOAD}\n{STORE}");
    let report = admit("t.rules", &rules(&text), &model(), &solver).expect("both are provable");
    assert_eq!(report.discharged(), 2, "{report}");
}

#[test]
fn a_model_that_has_the_byte_order_backwards_is_refuted() {
    let Some(solver) = solver() else {
        return;
    };
    // The IR half says the byte at the higher address is the high half, and the machine half
    // here says the opposite. On a load of one byte the two agree and nothing is caught, which
    // is why the rule that catches it has to be a load of two.
    let backwards = MODEL.replace(
        "(semantics (x64.mov_rm_16 a) (concat (select (mem) (bvadd a 1)) (select (mem) a)))",
        "(semantics (x64.mov_rm_16 a) (concat (select (mem) a) (select (mem) (bvadd a 1))))",
    );
    let model = Model::read("t.model", &backwards).expect("the model reads");
    let text = "\
(rule (lower (load.i16 (value.i64 a)))
      (x64.mov_rm_16 (amode_base a))
      (spec (= (concat (select (mem) (bvadd a 1)) (select (mem) a)) (result))))";

    let report = verify("t.rules", &rules(text), &model, &solver).expect("the question is asked");
    assert!(
        matches!(report.verdicts[0], Verdict::Refuted(_)),
        "a backwards byte order got through: {report}"
    );
}
