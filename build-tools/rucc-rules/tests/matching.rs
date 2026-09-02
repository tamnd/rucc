//! What the compiled rule set matches, and in what order.

use rucc_rules::{Matcher, Rule, Term, parse};

/// Two of the rules `spec/10-backend.md` section 10.2 writes out, including the one that is not
/// x86-64, because the point of the trie is what happens when rules share a prefix. The other
/// two are the general forms the first two are special cases of, which is what makes the order
/// the rules are tried in observable.
const RULES: &str = "\
(rule (lower (add.i64 (value x) (mul.i64 (value y) (iconst 4))))
      (x64.lea (amode_base_index_scale x y 4))
      (spec (= (bvadd x (bvmul y 4)) (result))))
(rule (lower (add.i64 (value x) (shl.i64 (value y) (iconst k))))
      (if (and (>= k 0) (< k 64)))
      (a64.add_shifted x y (lsl k))
      (spec (= (bvadd x (bvshl y k)) (result))))
(rule (lower (add.i64 (value x) (mul.i64 (value y) (iconst m))))
      (x64.imul_then_add x y m)
      (spec (= (bvadd x (bvmul y m)) (result))))
(rule (lower (add.i64 (value x) (value y)))
      (x64.add x y)
      (spec (= (bvadd x y) (result))))";

fn rules(text: &str) -> Vec<Rule> {
    match parse("t.rules", text) {
        Ok(rules) => rules,
        Err(errors) => panic!("{}", errors[0]),
    }
}

fn matcher(text: &str) -> Matcher {
    match Matcher::build("t.rules", &rules(text)) {
        Ok(matcher) => matcher,
        Err(errors) => panic!("{}", errors[0]),
    }
}

/// Subjects are written in the rule language too, since a pattern with no variables in it is
/// exactly a ground term.
fn term(text: &str) -> Term {
    // The replacement and the specification are the smallest well formed things that mention
    // nothing, since only the pattern is wanted here.
    let text = format!("(rule (lower {text}) (nothing) (spec (= (result) (result))))");
    match parse("t.rules", &text) {
        Ok(rules) => rules.into_iter().next().expect("one rule").pattern,
        Err(errors) => panic!("{}", errors[0]),
    }
}

#[test]
fn the_rule_that_names_the_most_is_the_rule_that_fires() {
    let matcher = matcher(RULES);
    let subject = term("(add.i64 (value a) (mul.i64 (value b) (iconst 4)))");
    let found = matcher.find(&subject).expect("something has to match");
    assert_eq!(found.rule, 0);
    assert_eq!(found.get("x").map(ToString::to_string).as_deref(), Some("a"));
    assert_eq!(found.get("y").map(ToString::to_string).as_deref(), Some("b"));
}

/// The scale in the first rule is written out as `4`, so a subject with any other scale has to
/// fall past it. Falling past a literal test and into a wildcard is the case a matcher built as
/// a chain of conditionals gets right by accident and a trie has to get right on purpose.
#[test]
fn a_literal_that_does_not_match_falls_through_to_the_more_general_rule() {
    let matcher = matcher(RULES);
    let subject = term("(add.i64 (value a) (mul.i64 (value b) (iconst 8)))");
    let found = matcher.find(&subject).expect("the general rule has to catch it");
    assert_eq!(found.rule, 2);
    assert_eq!(found.get("m").map(ToString::to_string).as_deref(), Some("8"));
}

#[test]
fn a_shift_by_a_constant_reaches_the_rule_that_asked_for_one() {
    let matcher = matcher(RULES);
    let subject = term("(add.i64 (value a) (shl.i64 (value b) (iconst 3)))");
    let found = matcher.find(&subject).expect("something has to match");
    assert_eq!(found.rule, 1);
    assert_eq!(found.get("k").map(ToString::to_string).as_deref(), Some("3"));
}

#[test]
fn nothing_matches_when_nothing_matches() {
    let matcher = matcher(RULES);
    assert!(matcher.find(&term("(sub.i64 (value a) (value b))")).is_none());
    assert!(matcher.find(&term("(add.i64 (value a))")).is_none());
    assert_eq!(matcher.find(&term("(add.i64 (value a) (value b))")).map(|m| m.rule), Some(3));
}

/// The prefix every rule shares is one branch, not three. This is the whole reason the matcher
/// is a trie, so it is worth asserting rather than trusting.
#[test]
fn rules_that_start_the_same_way_share_the_steps_they_agree_on() {
    let shown = matcher(RULES).to_string();
    assert_eq!(shown.matches("add.i64/2").count(), 1, "{shown}");
    // Four, not five: the one all four rules share, one inside each of the two second operands
    // that are shifts or multiplies, and one for the plain add. The two multiply rules agree on
    // everything up to the scale, so they share the `value/1` inside the multiply.
    assert_eq!(shown.matches("value/1").count(), 4, "{shown}");
}

#[test]
fn the_wildcard_is_the_last_thing_tried_at_every_node() {
    let shown = matcher(RULES).to_string();
    let expected = "\
add.i64/2
  value/1
    bind x
      mul.i64/2
        value/1
          bind y
            iconst/1
              4
                => rule 0
              bind m
                => rule 2
      shl.i64/2
        value/1
          bind y
            iconst/1
              bind k
                => rule 1
      value/1
        bind y
          => rule 3
";
    assert_eq!(shown, expected);
}

#[test]
fn a_rule_that_can_never_fire_is_refused_rather_than_dropped() {
    let text = "\
(rule (lower (add.i64 (value x) (value y))) (x64.add x y) (spec (= (bvadd x y) (result))))
(rule (lower (add.i64 (value a) (value b))) (x64.lea a b) (spec (= (bvadd a b) (result))))";
    let Err(errors) = Matcher::build("t.rules", &rules(text)) else {
        panic!("that was supposed to be refused");
    };
    assert_eq!(
        errors[0].to_string(),
        "t.rules:2:1: this rule can never fire, because the rule on line 1 matches everything it does"
    );
}

#[test]
fn an_empty_rule_set_matches_nothing_and_says_so() {
    let matcher = Matcher::build("t.rules", &[]).expect("nothing to refuse");
    assert!(matcher.is_empty());
    assert!(matcher.find(&term("(add.i64 (value a) (value b))")).is_none());
}
