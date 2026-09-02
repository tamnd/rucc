//! What a rule has to prove before it is allowed into the rule set.
//!
//! The rules here are the ones `spec/10-backend.md` section 10.2 writes out, which is the point:
//! the claim the project makes is that its lowering is verified, and the first thing to check is
//! that the lowering the design document shows can be.

use rucc_rules::{Rule, parse};
use rucc_verify::{Model, Solver, Verdict, query, verify};

/// What the machine terms in those rules mean. This is the hand written per-target model
/// `spec/10-backend.md` says the machine semantics start as.
const MODEL: &str = "\
(semantics (add.i64 left right) (bvadd left right))
(semantics (mul.i64 left right) (bvmul left right))
(semantics (shl.i64 value amount) (bvshl value amount))
(semantics (value v) v)
(semantics (iconst c) c)
(semantics (amode_base_index_scale base index scale) (bvadd base (bvmul index scale)))
(semantics (x64.lea address) address)
(semantics (x64.shl value amount) (bvshl value amount))
(semantics (x64.add left right) (bvadd left right))";

/// The `lea` rule.
const LEA: &str = "\
(rule (lower (add.i64 (value x) (mul.i64 (value y) (iconst 4))))
      (x64.lea (amode_base_index_scale x y 4))
      (spec (= (bvadd x (bvmul y 4)) (result))))";

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
/// runs everything else. That is the right default and the wrong thing for CI to rely on, so
/// the job that installs a solver sets `RUCC_REQUIRE_SOLVER` and the skip becomes a failure.
fn solver() -> Option<Solver> {
    let found = Solver::find();
    assert!(
        !(found.is_none() && std::env::var_os("RUCC_REQUIRE_SOLVER").is_some()),
        "a solver was required and none was found on PATH"
    );
    found
}

#[test]
fn the_question_a_rule_asks_is_the_negation_of_its_claim() {
    let asked = query("t.rules", &rules(LEA)[0], &model()).expect("the model covers this rule");
    let expected = "\
(set-logic QF_BV)
(declare-const x (_ BitVec 64))
(declare-const y (_ BitVec 64))
(assert (not (and (= (bvadd x (bvmul y (_ bv4 64))) (bvadd x (bvmul y (_ bv4 64)))) (= (bvadd x (bvmul y (_ bv4 64))) (bvadd x (bvmul y (_ bv4 64)))))))
(check-sat)
(get-model)
";
    assert_eq!(asked, expected);
}

/// A guard is an assumption and not part of the claim, which is what makes a rule that is only
/// true for some constants provable at all.
#[test]
fn a_guard_is_asserted_rather_than_claimed() {
    let text = "\
(rule (lower (shl.i64 (value x) (iconst k)))
      (if (and (>= k 0) (< k 64)))
      (x64.shl x k)
      (spec (= (bvshl x k) (result))))";
    let asked = query("t.rules", &rules(text)[0], &model()).expect("the model covers this rule");
    assert!(asked.contains("(assert (and (bvsge k (_ bv0 64)) (bvslt k (_ bv64 64))))"), "{asked}");
    assert!(asked.contains("(= (bvshl x k) (bvshl x k))"), "{asked}");
}

#[test]
fn the_rules_the_design_document_writes_out_are_discharged() {
    let Some(solver) = solver() else {
        return;
    };
    let text = format!(
        "{LEA}\n(rule (lower (add.i64 (value x) (value y))) (x64.add x y) (spec (= (bvadd x y) (result))))"
    );
    let report = verify("t.rules", &rules(&text), &model(), &solver).expect("nothing to report");
    assert!(report.all_discharged(), "{report:?}");
    assert_eq!(report.discharged(), 2);
}

/// The test that says the verification is worth running. A rule that lowers a multiply by four
/// to an address computation that scales by eight is exactly the kind of mistake nobody sees in
/// review, and the solver has to produce the numbers that show it.
#[test]
fn a_rule_that_is_wrong_is_refuted_and_the_counterexample_is_kept() {
    let Some(solver) = solver() else {
        return;
    };
    let text = "\
(rule (lower (add.i64 (value x) (mul.i64 (value y) (iconst 4))))
      (x64.lea (amode_base_index_scale x y 8))
      (spec (= (bvadd x (bvmul y 4)) (result))))";
    let report = verify("t.rules", &rules(text), &model(), &solver).expect("nothing to report");
    assert!(!report.all_discharged());
    let Verdict::Refuted(counterexample) = &report.verdicts[0] else {
        panic!("that rule is wrong: {:?}", report.verdicts[0]);
    };
    assert!(counterexample.contains("define-fun"), "{counterexample}");
}

/// A shift by anything at all is not a shift by less than the width, and without the guard the
/// solver finds that. This is the other half of the guard: it has to be load bearing.
#[test]
fn a_rule_that_needs_its_guard_fails_without_it() {
    let Some(solver) = solver() else {
        return;
    };
    let text = "\
(rule (lower (shl.i64 (value x) (iconst k)))
      (x64.shl x (bvadd k 1))
      (spec (= (bvshl x k) (result))))";
    let report = verify("t.rules", &rules(text), &model(), &solver).expect("nothing to report");
    assert!(matches!(report.verdicts[0], Verdict::Refuted(_)), "{report:?}");
}

/// The hole the pattern's own meaning closes. This rule lowers correctly, in the sense that the
/// machine term it produces is what its `spec` clause describes, and the `spec` clause is still
/// wrong: the pattern is an add and the claim is about a subtract. Checking only the claim would
/// let it through, because it would be checked against itself.
#[test]
fn a_rule_whose_stated_claim_is_not_what_its_pattern_means_is_refuted() {
    let Some(solver) = solver() else {
        return;
    };
    let text = "\
(rule (lower (add.i64 (value x) (value y)))
      (x64.add x y)
      (spec (= (bvsub x y) (result))))";
    let report = verify("t.rules", &rules(text), &model(), &solver).expect("nothing to report");
    assert!(matches!(report.verdicts[0], Verdict::Refuted(_)), "{report:?}");
}

#[test]
fn a_term_nobody_has_said_the_meaning_of_is_refused() {
    let text = "\
(rule (lower (add.i64 (value x) (value y)))
      (x64.frobnicate x y)
      (spec (= (bvadd x y) (result))))";
    let failed = query("t.rules", &rules(text)[0], &model()).expect_err("nothing defines that");
    assert_eq!(
        failed.to_string(),
        "t.rules:2:7: nothing in the model says what `x64.frobnicate` means"
    );
}

#[test]
fn a_head_the_solver_already_knows_cannot_be_given_a_second_meaning() {
    let failed = Model::read("t.model", "(semantics (bvadd a b) (bvsub a b))")
        .expect_err("that is not something to redefine");
    assert_eq!(
        failed[0].to_string(),
        "t.model:1:12: `bvadd` is something the solver already knows"
    );
}

#[test]
fn a_model_that_is_not_a_model_says_so() {
    let failed = Model::read("t.model", "(x64.lea a)").expect_err("that is not a semantics form");
    assert_eq!(failed[0].to_string(), "t.model:1:1: expected a `(semantics ...)` form");
}
