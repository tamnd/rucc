//! What a rule has to prove before it is allowed into the rule set.
//!
//! The rules here are the ones `spec/10-backend.md` section 10.2 writes out, which is the point:
//! the claim the project makes is that its lowering is verified, and the first thing to check is
//! that the lowering the design document shows can be.

use rucc_rules::{Rule, parse};
use rucc_verify::{Model, Report, Solver, Verdict, Widths, admit, query, query_at, verify};

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
(semantics (x64.add left right) (bvadd left right))
(semantics (udiv.i64 left right) (bvudiv left right))
(semantics (x64.udiv left right) (bvudiv left right))
(semantics (add.i32 left right) (bvadd (extract 31 0 left) (extract 31 0 right)))
(semantics (value.i64 v) v)
(semantics (value.i32 v) v)
(semantics (rv.addw a b) (sign_extend 32 64 (bvadd (extract 31 0 a) (extract 31 0 b))))
(semantics (rv.addwu a b) (zero_extend 32 64 (bvadd (extract 31 0 a) (extract 31 0 b))))";

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

/// The rule a bounded proof exists for.
///
/// Division and multiplication in one claim is the shape a bitvector solver does not settle:
/// this one is answered in hundredths of a second at eight bits and not in five seconds at
/// sixteen, let alone at the sixty four it will run in. It is also true, which is the point of
/// using it here rather than something contrived.
const HARD: &str = "\
(rule (lower (udiv.i64 (value x) (value y)))
      (x64.udiv x y)
      (spec (= (bvmul (result) y) (bvsub x (bvurem x y))))
      (bounded \"division against multiplication is out of reach at sixty four bits\"))";

/// Enough for the narrow widths and not enough for the real one, which is the whole point of
/// the rule above. Ten seconds is what the tests would otherwise wait for a shrug.
fn quick_solver() -> Option<Solver> {
    solver().map(|solver| solver.within(2))
}

#[test]
fn the_same_question_can_be_asked_at_a_narrower_width() {
    let asked = query_at("t.rules", &rules(LEA)[0], &model(), 8).expect("the model covers this");
    assert!(asked.contains("(declare-const x (_ BitVec 8))"), "{asked}");
    assert!(asked.contains("(_ bv4 8)"), "{asked}");
    assert!(!asked.contains("64"), "{asked}");
}

/// The fallback, and the two things that have to be true for it to be taken: the solver gave up
/// at the rule's own width, and the rule says why narrow widths are enough.
#[test]
fn a_rule_the_solver_gives_up_on_gets_a_bounded_proof_if_it_asked_for_one() {
    let Some(solver) = quick_solver() else {
        return;
    };
    let report = verify("t.rules", &rules(HARD), &model(), &solver).expect("nothing to report");
    let Verdict::Bounded { widths, why } = &report.verdicts[0] else {
        panic!("that rule is the one bounded proofs exist for: {:?}", report.verdicts[0]);
    };
    assert_eq!(widths, &[4, 8]);
    assert_eq!(why, "division against multiplication is out of reach at sixty four bits");
    assert_eq!(report.bounded(), 1);
    assert_eq!(report.discharged(), 0);
    assert!(!report.all_discharged());
    assert!(report.accepted());
}

/// The fallback is a judgement somebody makes, so a rule that never asked for one does not get
/// it. Without the clause the same rule is a shrug, and a shrug is not a pass.
#[test]
fn a_rule_that_did_not_ask_for_a_bounded_proof_is_left_unknown() {
    let Some(solver) = quick_solver() else {
        return;
    };
    let text = HARD.replace(
        "\n      (bounded \"division against multiplication is out of reach at sixty four bits\")",
        "",
    );
    let report = verify("t.rules", &rules(&text), &model(), &solver).expect("nothing to report");
    assert_eq!(report.verdicts, vec![Verdict::Unknown]);
    assert_eq!(report.bounded(), 0);
    assert!(!report.accepted());
}

/// The gate. A file with a rule that is not proved is refused whole, at the line the rule
/// starts on, because a compiler built from the rules that happened to pass is a compiler
/// nobody described.
#[test]
fn a_rule_that_is_not_proved_keeps_the_whole_file_out() {
    let Some(solver) = quick_solver() else {
        return;
    };
    let wrong = "\
(rule (lower (add.i64 (value x) (value y)))
      (x64.add x y)
      (spec (= (bvsub x y) (result))))";
    let text = format!("{LEA}\n{wrong}");
    let errors = admit("t.rules", &rules(&text), &model(), &solver).expect_err("one is wrong");
    assert_eq!(errors.len(), 1);
    assert!(errors[0].to_string().starts_with("t.rules:4:1: this rule is not true"), "{errors:?}");
}

#[test]
fn a_file_of_rules_that_are_all_proved_is_admitted() {
    let Some(solver) = quick_solver() else {
        return;
    };
    let text = format!("{LEA}\n{HARD}");
    let report = admit("t.rules", &rules(&text), &model(), &solver).expect("both are proved");
    assert_eq!(report.discharged(), 1);
    assert_eq!(report.bounded(), 1);
    assert_eq!(report.to_string(), "2 rules: 1 discharged, 1 by bounded proof, 0 refused");
}

/// The rule `spec/10-backend.md` writes for RISC-V, where a thirty two bit add of two sixty four
/// bit registers leaves its result sign extended across the whole register. Two widths in one
/// rule, which is what every `sext`, `zext` and `trunc` in a lowering needs.
const ADDW: &str = "\
(rule (lower (add.i32 (value.i64 x) (value.i64 y)))
      (rv.addw x y)
      (spec (= (sign_extend 32 64 (bvadd (extract 31 0 x) (extract 31 0 y))) (result))))";

#[test]
fn a_rule_that_changes_width_is_discharged() {
    let Some(solver) = solver() else {
        return;
    };
    let report = verify("t.rules", &rules(ADDW), &model(), &solver).expect("nothing to report");
    assert!(report.all_discharged(), "{report:?}");
}

/// A name is as wide as the place that bound it and not as wide as the rule, and what the rule
/// computes is narrower than what the machine instruction leaves in the register.
#[test]
fn a_name_is_as_wide_as_the_pattern_binds_it() {
    let asked = query("t.rules", &rules(ADDW)[0], &model()).expect("the model covers this rule");
    assert!(asked.contains("(declare-const x (_ BitVec 64))"), "{asked}");
    assert!(asked.contains("((_ sign_extend 32)"), "{asked}");
    assert!(
        asked.contains("(= (bvadd ((_ extract 31 0) x) ((_ extract 31 0) y)) ((_ extract 31 0)")
    );
}

/// What the machine instruction does with the rest of the register is claimed by the `spec`
/// clause and nowhere else, so a rule that gets it wrong has to be refuted by that clause. This
/// one lowers to an instruction that zero extends and says it sign extends.
#[test]
fn what_the_wider_register_holds_is_claimed_by_the_specification() {
    let Some(solver) = solver() else {
        return;
    };
    let text = ADDW.replace("rv.addw", "rv.addwu");
    let report = verify("t.rules", &rules(&text), &model(), &solver).expect("nothing to report");
    assert!(matches!(report.verdicts[0], Verdict::Refuted(_)), "{report:?}");
}

/// A bounded proof scales every width in the rule by the one ratio rather than flattening them
/// onto a single number, because a rule that converts between widths that have been flattened
/// together is no longer the rule anybody wrote.
#[test]
fn a_narrower_question_keeps_the_widths_apart() {
    let asked = query_at("t.rules", &rules(ADDW)[0], &model(), 8).expect("the model covers this");
    assert!(asked.contains("(declare-const x (_ BitVec 16))"), "{asked}");
    assert!(asked.contains("((_ sign_extend 8)"), "{asked}");
    assert!(asked.contains("((_ extract 7 0) x)"), "{asked}");
}

/// A lowering may not lose bits, and a replacement narrower than what it replaces is not a claim
/// that needs a solver to settle.
#[test]
fn a_replacement_narrower_than_what_it_replaces_is_refused() {
    let text = "\
(rule (lower (add.i64 (value x) (value y)))
      (x64.add (extract 31 0 x) (extract 31 0 y))
      (spec (= (bvadd x y) (result))))";
    let failed = query("t.rules", &rules(text)[0], &model()).expect_err("that loses bits");
    assert_eq!(
        failed.to_string(),
        "t.rules:2:7: what this replaces is 64 bits wide and this is 32, so it cannot compute it"
    );
}

/// The model is held to what the rules say about it. An opcode that names a width and means
/// something of another width is a mistake in the model, and it is worth catching there rather
/// than in a proof that quietly asks about the wrong thing.
#[test]
fn an_opcode_that_means_something_of_another_width_is_refused() {
    let text = "\
(semantics (add.i32 left right) (bvadd left right))
(semantics (value.i64 v) v)";
    let model = Model::read("t.model", text).expect("that is a model");
    let rule = &rules(
        "(rule (lower (add.i32 (value.i64 x) (value.i64 y))) (x64.add x y) (spec (= x (result))))",
    )[0];
    let widths = Widths::of(&rule.pattern);
    let failed = model.write("t.rules", &rule.pattern, &widths).expect_err("that is 64 bits wide");
    assert_eq!(
        failed.to_string(),
        "t.rules:1:14: `add.i32` is written for 32 bits and means something 64 bits wide"
    );
}

/// Two widths in one operation is a mistake the solver would report in its own words, at a place
/// in generated text nobody wants to read.
#[test]
fn adding_two_things_of_different_widths_is_refused() {
    let rule = &rules(
        "(rule (lower (add.i64 (value x) (value.i32 y))) (x64.add x y) (spec (= x (result))))",
    )[0];
    let model = model();
    let widths = Widths::of(&rule.pattern);
    let failed =
        model.write("t.rules", &rule.pattern, &widths).expect_err("those are different widths");
    assert!(
        failed.to_string().ends_with(
            "`bvadd` is given something 64 bits wide and something 32 bits wide, and those are \
             not the same kind of thing"
        ),
        "{failed}"
    );
}

/// The count is the metric `spec/15-testing.md` section 15.5 asks to be reported, so the line
/// it is reported on is worth pinning.
#[test]
fn a_report_says_what_became_of_every_rule() {
    let report = Report {
        verdicts: vec![
            Verdict::Discharged,
            Verdict::Bounded { widths: vec![4, 8], why: "wide multiplication".to_owned() },
            Verdict::Unknown,
            Verdict::Refuted("(define-fun x () (_ BitVec 64) #x0)".to_owned()),
        ],
    };
    assert_eq!(report.to_string(), "4 rules: 1 discharged, 1 by bounded proof, 2 refused");
    assert!(!report.accepted());
    assert!(!report.all_discharged());
}
