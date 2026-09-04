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
(semantics (value.i64 v) v)
(semantics (fadd.f32 l r) (fp.add l r))
(semantics (fadd.f64 l r) (fp.add l r))
(semantics (fdiv.f32 l r) (fp.div l r))
(semantics (add.i32 l r) (bvadd l r))
(semantics (x64.addss_rr l r) (fp.add l r))
(semantics (x64.addsd_rr l r) (fp.add l r))
(semantics (x64.divss_rr l r) (fp.div l r))
(semantics (x64.add_rr_32 l r) (bvadd l r))
(semantics (load.f32 a)
           (float_from_bits 32
                   (concat (select (mem) (bvadd a 3)) (select (mem) (bvadd a 2))
                           (select (mem) (bvadd a 1)) (select (mem) a))))
(semantics (x64.movss_rm a)
           (float_from_bits 32
                   (concat (select (mem) (bvadd a 3)) (select (mem) (bvadd a 2))
                           (select (mem) (bvadd a 1)) (select (mem) a))))
(semantics (x64.mov_rm_32 a)
           (concat (select (mem) (bvadd a 3)) (select (mem) (bvadd a 2))
                   (select (mem) (bvadd a 1)) (select (mem) a)))
(semantics (store.f32 v a)
           (store (store (store (store (mem)
                                a (extract 7 0 (bits_from_float 32 v)))
                                (bvadd a 1) (extract 15 8 (bits_from_float 32 v)))
                                (bvadd a 2) (extract 23 16 (bits_from_float 32 v)))
                                (bvadd a 3) (extract 31 24 (bits_from_float 32 v))))
(semantics (x64.movss_mr a v)
           (store (store (store (store (mem)
                                a (extract 7 0 (bits_from_float 32 v)))
                                (bvadd a 1) (extract 15 8 (bits_from_float 32 v)))
                                (bvadd a 2) (extract 23 16 (bits_from_float 32 v)))
                                (bvadd a 3) (extract 31 24 (bits_from_float 32 v))))
(semantics (fpext.f32.f64 v) (float_from_float 32 64 v))
(semantics (fptosi.f64.i32 v) (signed_from_float 64 32 v))
(semantics (sitofp.i32.f32 v) (float_from_signed 32 32 v))
(semantics (bitcast.i32.f32 v) (float_from_bits 32 v))
(semantics (x64.cvtss2sd v) (float_from_float 32 64 v))
(semantics (x64.cvttsd2si_32 v) (signed_from_float 64 32 v))
(semantics (x64.cvtsi2ss_32 v) (float_from_signed 32 32 v))
(semantics (x64.movd_to_xmm v) (float_from_bits 32 v))
(semantics (amode_base base) base)";

/// Reading a float out of memory, which is the rule that has both kinds of thing in it at once.
const LOAD: &str = "\
(rule (lower (load.f32 (value.i64 a)))
      (x64.movss_rm (amode_base a))
      (spec (= (float_from_bits 32
                       (concat (select (mem) (bvadd a 3)) (select (mem) (bvadd a 2))
                               (select (mem) (bvadd a 1)) (select (mem) a)))
               (result))))";

/// Adding two floats, which is the smallest rule about one there is.
const ADD: &str = "\
(rule (lower (fadd.f32 (value.f32 x) (value.f32 y)))
      (x64.addss_rr x y)
      (spec (= (fp.add x y) (result))))";

/// A float turned into an integer, which is the conversion whose rounding is not the one every
/// other rule here is proved under.
const TO_INT: &str = "\
(rule (lower (fptosi.f64.i32 (value.f64 x)))
      (x64.cvttsd2si_32 x)
      (spec (= (signed_from_float 64 32 x) (result))))";

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

#[test]
fn a_load_asks_about_arrays_and_floats_at_once() {
    let asked = query("t.rules", &rules(LOAD)[0], &model()).expect("the model covers this rule");

    // Four logics rather than two, because a rule may reach memory, floats, both or neither and
    // a solver told about a theory nothing in the question uses is slower for no reason.
    assert!(asked.starts_with("(set-logic QF_ABVFP)\n"), "{asked}");

    // The address is a bitvector and the value is not, which is the point: what comes out of
    // memory is bytes, and reading them as a float is the thing the rule says out loud.
    assert!(asked.contains("(declare-const a (_ BitVec 64))"), "{asked}");
    assert!(asked.contains("((_ to_fp 8 24) (concat"), "{asked}");
}

#[test]
fn a_store_writes_the_bits_of_the_float_and_the_verifier_is_what_names_the_operation() {
    let text = "\
(rule (lower (store.f32 (value.f32 v) (value.i64 a)))
      (x64.movss_mr (amode_base a) v)
      (spec (= (store (store (store (store (mem)
                             a (extract 7 0 (bits_from_float 32 v)))
                             (bvadd a 1) (extract 15 8 (bits_from_float 32 v)))
                             (bvadd a 2) (extract 23 16 (bits_from_float 32 v)))
                             (bvadd a 3) (extract 31 24 (bits_from_float 32 v)))
               (result))))";
    let asked = query("t.rules", &rules(text)[0], &model()).expect("the model covers this rule");

    // The rule writes `bits_from_float` and the query writes the solver's name for it, which is
    // the same arrangement a rule writing `<` and a query writing `bvslt` has. Nothing in the
    // rule language is spelled the way one solver happens to spell it.
    assert!(!text.contains("ieee"));
    assert!(asked.contains("(fp.to_ieee_bv v)"), "{asked}");
    assert!(asked.contains("(declare-const v Float32)"), "{asked}");
}

#[test]
fn a_float_load_lowered_to_an_integer_one_is_refused() {
    // The two instructions move the same four bytes and neither looks at them, so this is a
    // rule that is right about memory and wrong about where the value ends up. Nothing in the
    // bytes says which it is, so the sorts are what has to catch it, and they do.
    let text = "\
(rule (lower (load.f32 (value.i64 a)))
      (x64.mov_rm_32 (amode_base a))
      (spec (= (float_from_bits 32
                       (concat (select (mem) (bvadd a 3)) (select (mem) (bvadd a 2))
                               (select (mem) (bvadd a 1)) (select (mem) a)))
               (result))))";
    let problem =
        query("t.rules", &rules(text)[0], &model()).expect_err("this rule cannot be asked");
    assert!(
        problem.message.contains("replaces something 32 bits of float with something 32 bits"),
        "{}",
        problem.message
    );
}

#[test]
fn a_reinterpretation_at_a_format_the_standard_has_no_name_for_is_refused() {
    let text = "\
(rule (lower (load.f32 (value.i64 a)))
      (x64.movss_rm (amode_base a))
      (spec (= (float_from_bits 80
                       (concat (select (mem) (bvadd a 3)) (select (mem) (bvadd a 2))
                               (select (mem) (bvadd a 1)) (select (mem) a)))
               (result))))";
    let problem =
        query("t.rules", &rules(text)[0], &model()).expect_err("this rule cannot be asked");
    assert!(problem.message.contains("which is not a float format"), "{}", problem.message);
}

#[test]
fn reading_a_float_as_bits_of_the_wrong_width_is_refused() {
    let text = "\
(rule (lower (load.f32 (value.i64 a)))
      (x64.movss_rm (amode_base a))
      (spec (= (float_from_bits 64
                       (concat (select (mem) (bvadd a 3)) (select (mem) (bvadd a 2))
                               (select (mem) (bvadd a 1)) (select (mem) a)))
               (result))))";
    let problem =
        query("t.rules", &rules(text)[0], &model()).expect_err("this rule cannot be asked");
    assert!(
        problem.message.contains("takes something 64 bits wide and this is 32 bits wide"),
        "{}",
        problem.message
    );
}

#[test]
fn the_bytes_of_a_load_go_back_in_the_order_they_came_out() {
    let Some(solver) = solver() else {
        return;
    };
    let report = admit("t.rules", &rules(LOAD), &model(), &solver).expect("this is provable");
    assert_eq!(report.discharged(), 1, "{report}");

    // The claim is worth something because the byte order is written out on both sides rather
    // than shared, so a load that counted up on one side is a load the other disagrees with.
    let wrong = LOAD.replace("(bvadd a 3)) (select (mem) (bvadd a 2))", "a) (select (mem) a)");
    let report = verify("t.rules", &rules(&wrong), &model(), &solver).expect("it is asked");
    assert!(matches!(report.verdicts[0], Verdict::Refuted(_)), "{report}");
}

#[test]
fn a_conversion_to_an_integer_cuts_towards_zero_rather_than_rounding() {
    let asked = query("t.rules", &rules(TO_INT)[0], &model()).expect("the model covers this rule");

    // C keeps the part before the point and discards the rest, whatever the rounding mode is set
    // to, which is why the instruction is the one with two `t`s in its name. Every other float
    // question here is asked at the default rounding and this one is not, so the two modes are
    // both written in one place rather than in each rule.
    assert!(asked.contains("((_ fp.to_sbv 32) RTZ x)"), "{asked}");
    assert!(!asked.contains("RNE"), "{asked}");
    assert!(!TO_INT.contains("RTZ"));

    // A float on one side and bits on the other, in a rule that touches no memory.
    assert!(asked.starts_with("(set-logic QF_FPBV)\n"), "{asked}");
    assert!(asked.contains("(declare-const x Float64)"), "{asked}");
}

#[test]
fn a_conversion_to_a_float_rounds_the_way_the_arithmetic_does() {
    let text = "\
(rule (lower (sitofp.i32.f32 (value.i32 x)))
      (x64.cvtsi2ss_32 x)
      (spec (= (float_from_signed 32 32 x) (result))))";
    let asked = query("t.rules", &rules(text)[0], &model()).expect("the model covers this rule");

    // The other direction, where there is a nearest float and rounding to it is what the
    // instruction does. The name the rule writes says the number it reads is signed, which is a
    // thing SMT-LIB spells by which of its operators is used rather than in the term.
    assert!(asked.contains("((_ to_fp 8 24) RNE x)"), "{asked}");
    assert!(asked.contains("(declare-const x (_ BitVec 32))"), "{asked}");
}

#[test]
fn a_conversion_from_a_number_that_is_given_a_float_is_refused() {
    let text = "\
(rule (lower (sitofp.i32.f32 (value.f32 x)))
      (x64.cvtsi2ss_32 x)
      (spec (= (float_from_signed 32 32 x) (result))))";
    let problem =
        query("t.rules", &rules(text)[0], &model()).expect_err("this rule cannot be asked");

    // A conversion whose source is already the kind of thing it converts into is a conversion
    // somebody has left out, and the two are the same size, so nothing but the sorts would say.
    assert!(
        problem.message.contains("takes something 32 bits wide and this is 32 bits of float"),
        "{}",
        problem.message
    );
}

#[test]
fn a_reinterpretation_lowered_to_a_conversion_is_refuted() {
    let Some(solver) = solver() else {
        return;
    };
    // Nothing about the shape of this rule is wrong. `movd` and `cvtsi2ss` both read a general
    // purpose register and write a vector one, at the same two widths, so the sorts fit and the
    // question is asked. What differs is the whole of what they mean: one keeps every bit and no
    // value and the other keeps the value and no bit.
    let text = "\
(rule (lower (bitcast.i32.f32 (value.i32 x)))
      (x64.cvtsi2ss_32 x)
      (spec (= (float_from_bits 32 x) (result))))";
    let report = verify("t.rules", &rules(text), &model(), &solver).expect("the question is asked");
    assert!(matches!(report.verdicts[0], Verdict::Refuted(_)), "{report}");
}

#[test]
fn the_conversions_are_proved_wherever_they_have_an_answer() {
    let Some(solver) = solver() else {
        return;
    };
    // A float too big for the integer it is asked for has no answer, and both halves of the rule
    // reach the same unspecified one, so the proof covers every float the conversion is defined
    // for and claims nothing about the rest. C leaves that case undefined and the machine writes
    // a value of its own, so there is nothing there to claim.
    let text = format!(
        "{TO_INT}
(rule (lower (fpext.f32.f64 (value.f32 x)))
      (x64.cvtss2sd x)
      (spec (= (float_from_float 32 64 x) (result))))"
    );
    let report = admit("t.rules", &rules(&text), &model(), &solver).expect("both are provable");
    assert_eq!(report.discharged(), 2, "{report}");
    assert_eq!(report.bounded(), 0, "{report}");
}
