//! What the rule language accepts and what it refuses.
//!
//! The accepted cases are the two the backend specification writes out, so that the language
//! this crate reads and the language that document describes are checked against each other
//! rather than merely intended to agree.

use rucc_rules::{Rule, RuleKind, parse};

/// The `lea` rule from `spec/10-backend.md` section 10.2.
const LEA: &str = "\
(rule (lower (add.i64 (value x) (mul.i64 (value y) (iconst 4))))
      (x64.lea (amode_base_index_scale x y 4))
      (spec (= (bvadd x (bvmul y 4)) (result))))";

/// The same section's guarded shift.
const SHL: &str = "\
(rule (lower (shl.i64 (value x) (iconst k)))
      (if (and (>= k 0) (< k 64)))
      (x64.shl x k)
      (spec (= (bvshl x k) (result))))";

fn read(text: &str) -> Vec<Rule> {
    match parse("t.rules", text) {
        Ok(rules) => rules,
        Err(errors) => panic!("{}", errors[0]),
    }
}

fn refuse(text: &str) -> Vec<String> {
    match parse("t.rules", text) {
        Ok(_) => panic!("that was supposed to be refused"),
        Err(errors) => errors.iter().map(ToString::to_string).collect(),
    }
}

#[test]
fn the_rule_the_specification_writes_out_is_the_rule_this_reads() {
    let rules = read(LEA);
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].pattern.to_string(), "(add.i64 (value x) (mul.i64 (value y) (iconst 4)))");
    assert_eq!(rules[0].replacement.to_string(), "(x64.lea (amode_base_index_scale x y 4))");
    assert_eq!(rules[0].spec.to_string(), "(= (bvadd x (bvmul y 4)) (result))");
    assert!(rules[0].guard.is_none());
}

#[test]
fn a_guard_is_read_and_is_not_mistaken_for_the_replacement() {
    let rules = read(SHL);
    assert_eq!(
        rules[0].guard.as_ref().map(ToString::to_string).as_deref(),
        Some("(and (>= k 0) (< k 64))")
    );
    assert_eq!(rules[0].replacement.to_string(), "(x64.shl x k)");
}

/// Print, read, print. The second printing has to be the first, which is the property
/// `spec/15-testing.md` section 15.1 asks of every textual form in the project.
#[test]
fn a_rule_survives_being_written_out_and_read_back() {
    for text in [LEA, SHL] {
        let once = read(text)[0].to_string();
        let twice = read(&once)[0].to_string();
        assert_eq!(once, twice);
        assert_eq!(once, text);
    }
}

#[test]
fn several_rules_in_one_file_are_all_read() {
    let text = format!("{LEA}\n\n{SHL}\n");
    assert_eq!(read(&text).len(), 2);
}

#[test]
fn a_comment_is_not_part_of_the_rule() {
    let text = format!("; the address arithmetic x86 does for free\n{LEA}");
    assert_eq!(read(&text).len(), 1);
}

#[test]
fn a_rule_with_no_specification_is_a_syntax_error_rather_than_an_unverified_rule() {
    let text = "(rule (lower (add.i64 (value x) (value y))) (x64.add x y))";
    assert_eq!(refuse(text), ["t.rules:1:58: expected a `(`"]);
}

#[test]
fn a_name_the_pattern_never_bound_is_refused_wherever_it_is_used() {
    let text = "(rule (lower (add.i64 (value x) (value y))) (x64.add x z) (spec (= x (result))))";
    assert_eq!(
        refuse(text),
        ["t.rules:1:56: `z` is used in the replacement and the pattern never bound it"]
    );

    let text = "(rule (lower (neg.i64 (value x))) (x64.neg x) (spec (= (bvneg y) (result))))";
    assert_eq!(
        refuse(text),
        ["t.rules:1:63: `y` is used in the specification and the pattern never bound it"]
    );

    let text = "(rule (lower (shl.i64 (value x) (iconst k))) (if (< n 64)) (x64.shl x k) (spec (= x (result))))";
    assert_eq!(
        refuse(text),
        ["t.rules:1:53: `n` is used in the guard and the pattern never bound it"]
    );
}

/// One name in two places of a pattern is how `spec/optimizer/13-rewrite-rules.md` section 13.4
/// says `x & x`, so it is a rule and not a mistake. The second occurrence is a claim that the
/// two places hold the same thing, and it binds nothing, so the name is bound once.
#[test]
fn one_name_can_stand_for_two_places_in_a_pattern() {
    let text = "\
(rule (simplify (and.i32 (value.i32 x) (value.i32 x)))
      (value.i32 x)
      (spec (= x (result))))";
    let rules = read(text);
    assert_eq!(rules[0].to_string(), text);
}

#[test]
fn the_result_is_only_something_the_specification_can_name() {
    let text =
        "(rule (lower (add.i64 (value x) (value y))) (x64.add (result) y) (spec (= x (result))))";
    assert_eq!(
        refuse(text),
        ["t.rules:1:54: `(result)` belongs in the specification, not in the replacement"]
    );
}

#[test]
fn a_pattern_that_matches_anything_at_all_is_refused() {
    let text = "(rule (lower x) (x64.nop) (spec (= x (result))))";
    assert_eq!(refuse(text), ["t.rules:1:14: a pattern has to name something to match"]);
}

#[test]
fn a_rules_own_keyword_inside_a_term_is_a_missing_parenthesis_and_is_named_as_one() {
    let text = "(rule (lower (add.i64 (value x) (value y))) (spec (= x (result))))";
    assert_eq!(
        refuse(text),
        ["t.rules:1:45: `spec` belongs to a rule's own shape, not inside a term"]
    );
}

#[test]
fn a_rule_that_is_never_closed_says_so_at_the_end_of_the_file() {
    let text = "(rule (lower (add.i64 (value x) (value y))\n";
    assert_eq!(refuse(text), ["t.rules:2:1: `(lower` was never closed"]);
}

/// One bad rule costs one error. Without the resynchronisation the second rule would be read
/// as the tail of the first and every message after the first would be noise.
#[test]
fn a_malformed_rule_does_not_take_the_rest_of_the_file_with_it() {
    let text = format!("(rule (lower @))\n{LEA}");
    let errors = refuse(&text);
    assert_eq!(errors, ["t.rules:1:14: `@` cannot appear in a rule"]);

    let text = format!("(rule (lower (add.i64 (value x))) (x64.add x))\n{SHL}");
    let errors = refuse(&text);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].starts_with("t.rules:1:46: expected"), "{errors:?}");
}

/// The clause a rule carries when there is reason to think the solver will not manage its real
/// width. It says nothing about what the rule does, which is why it is read last and why it
/// prints back where it was written.
#[test]
fn a_rule_can_say_why_a_bounded_proof_would_be_enough_for_it() {
    let text = "\
(rule (lower (mul.i64 (value x) (value y)))
      (x64.imul x y)
      (spec (= (bvmul x y) (result)))
      (bounded \"multiplication of two unknowns at 64 bits is out of reach\"))";
    let rules = read(text);
    assert_eq!(
        rules[0].bounded.as_deref(),
        Some("multiplication of two unknowns at 64 bits is out of reach")
    );
    assert_eq!(rules[0].to_string(), text);
}

#[test]
fn a_rule_without_the_clause_has_no_reason_and_prints_none() {
    let rules = read(LEA);
    assert_eq!(rules[0].bounded, None);
    assert_eq!(rules[0].to_string(), LEA);
}

/// An empty reason is worse than no clause at all, because it looks like somebody signed for
/// something and nobody did.
#[test]
fn a_bounded_proof_with_nothing_written_in_it_is_refused() {
    let text = "(rule (lower (x64.nop)) (x64.nop) (spec (= 0 (result))) (bounded \"  \"))";
    assert_eq!(refuse(text), ["t.rules:1:66: a bounded proof needs a reason somebody signed for"]);
}

#[test]
fn a_bounded_clause_with_no_reason_at_all_is_refused() {
    let text = "(rule (lower (x64.nop)) (x64.nop) (spec (= 0 (result))) (bounded))";
    assert_eq!(refuse(text), ["t.rules:1:65: expected a reason, in quotation marks"]);
}

/// Prose is for a person to read. A rule that puts a string where a term goes is a rule that
/// means nothing, and saying so beats reading it as a name with a space in it.
#[test]
fn a_string_is_not_something_a_term_can_be() {
    let text = "(rule (lower (x64.nop)) (x64.mov \"eax\") (spec (= 0 (result))))";
    assert_eq!(
        refuse(text),
        ["t.rules:1:34: a string is prose for a person and is not something a term can be"]
    );
}

/// The other kind of rule. `spec/optimizer/13-rewrite-rules.md` writes rewrites in the same
/// language as lowerings, so the reader has to take either word and keep track of which it read.
#[test]
fn a_rewrite_rule_reads_the_same_way_a_lowering_does_and_remembers_which_it_is() {
    let text = "\
(rule (simplify (add.i32 (value.i32 x) (iconst.i32 0)))
      (value.i32 x)
      (spec (= x (result))))";
    let rules = read(text);
    assert_eq!(rules[0].kind, RuleKind::Simplify);
    assert_eq!(rules[0].to_string(), text);

    let lowering = read(LEA);
    assert_eq!(lowering[0].kind, RuleKind::Lower);
}

/// The two words are the only two, and a rule that opens with a third is a rule somebody meant
/// something by. The message names both rather than only saying that one of them is missing.
#[test]
fn a_rule_that_opens_with_neither_word_is_told_what_the_two_are() {
    let text = "(rule (rewrite (x64.nop)) (x64.nop) (spec (= 0 (result))))";
    assert_eq!(refuse(text), ["t.rules:1:8: expected `simplify` or `lower`"]);
}
