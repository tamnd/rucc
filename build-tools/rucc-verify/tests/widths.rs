//! How wide a number in a rule is written, and who decides.
//!
//! Everything else in a term says how wide it is. A register says so because the pattern that
//! bound it says so, an opcode says so in its own name, and a conversion says both of its widths
//! out loud. A number says nothing, so a number is the one thing whose width is decided by where
//! it sits, and the whole of what these tests are about is which "where" that is.
//!
//! The answer is the place it ends up, not the place it was written. A number given to a model
//! head is written where the head's body names it, at the width the body uses it at, which is
//! `tamnd/rucc#915`. Before that it took the width of whatever the rule happened to run in, and
//! the two are the same number for every rule that stays at one width, which is why this went
//! unnoticed until an addressing mode was reached through from a rule of a different width.

use rucc_rules::{Rule, parse};
use rucc_verify::{Model, query};

/// An IR of two widths, and a machine with an addressing mode in it.
///
/// The addressing mode is the shape the issue is about: a head with no width in its name whose
/// body multiplies a sixty four bit index by a scale. A rule that loads four bytes through one
/// runs at thirty two bits and the scale has to be sixty four all the same.
const MODEL: &str = "\
(semantics (value.i32 v) v)
(semantics (value.i64 v) v)
(semantics (iconst.i64 c) c)
(semantics (add.i64 l r) (bvadd l r))
(semantics (mul.i64 l r) (bvmul l r))
(semantics (load.i32 a) (concat (select (mem) (bvadd a 3))
                                (select (mem) (bvadd a 2))
                                (select (mem) (bvadd a 1))
                                (select (mem) a)))
(semantics (amode_base_index_scale base index scale) (bvadd base (bvmul index scale)))
(semantics (x64.mov_rm_32 a) (concat (select (mem) (bvadd a 3))
                                     (select (mem) (bvadd a 2))
                                     (select (mem) (bvadd a 1))
                                     (select (mem) a)))";

/// The rule from the issue, written with the scale as a number rather than as a bound name.
const SCALED: &str = "\
(rule (lower (load.i32 (add.i64 (value.i64 a) (mul.i64 (value.i64 i) (iconst.i64 4)))))
      (x64.mov_rm_32 (amode_base_index_scale a i 4))
      (spec (= (concat (select (mem) (bvadd (bvadd a (bvmul i 4)) 3))
                       (select (mem) (bvadd (bvadd a (bvmul i 4)) 2))
                       (select (mem) (bvadd (bvadd a (bvmul i 4)) 1))
                       (select (mem) (bvadd a (bvmul i 4))))
               (result))))";

fn model(text: &str) -> Model {
    match Model::read("t.model", text) {
        Ok(model) => model,
        Err(errors) => panic!("{}", errors[0]),
    }
}

fn rule(text: &str) -> Rule {
    match parse("t.rules", text) {
        Ok(rules) => rules.into_iter().next().expect("one rule"),
        Err(errors) => panic!("{}", errors[0]),
    }
}

fn refuse(model: &Model, text: &str) -> String {
    match query("t.rules", &rule(text), model) {
        Ok(_) => panic!("that was supposed to be refused"),
        Err(error) => error.to_string(),
    }
}

/// The case the issue is about: a scale of four handed to an addressing mode from a rule that
/// runs at thirty two bits, which has to come out sixty four bits wide because that is the width
/// the body multiplies it at.
#[test]
fn a_number_given_to_a_head_is_written_at_the_width_its_body_uses_it_at() {
    let asked = query("t.rules", &rule(SCALED), &model(MODEL)).expect("the model covers this");
    assert!(asked.contains("(_ bv4 64)"), "{asked}");
    assert!(!asked.contains("(_ bv4 32)"), "{asked}");
}

/// And the width the rule runs at is not it, which is the same thing said from the other side:
/// the rule above runs at thirty two and nothing about the scale is thirty two.
#[test]
fn the_width_the_rule_runs_at_is_not_what_a_number_in_a_head_takes() {
    let text = "\
(rule (lower (load.i32 (add.i64 (value.i64 a) (mul.i64 (value.i64 i) (iconst.i64 8)))))
      (x64.mov_rm_32 (amode_base_index_scale a i 8))
      (spec (= (concat (select (mem) (bvadd (bvadd a (bvmul i 8)) 3))
                       (select (mem) (bvadd (bvadd a (bvmul i 8)) 2))
                       (select (mem) (bvadd (bvadd a (bvmul i 8)) 1))
                       (select (mem) (bvadd a (bvmul i 8))))
               (result))))";
    let asked = query("t.rules", &rule(text), &model(MODEL)).expect("the model covers this");
    assert!(asked.contains("(_ bv8 64)"), "{asked}");
}

/// A number sitting beside something one bit wide is one bit wide, which is the case that says
/// the neighbour in the rule is the wrong answer as surely as the rule's width is.
///
/// `x64.bit_of_8` takes a byte and a bit and its body masks the low bit of the byte with it, so
/// the one is a bit however wide the byte beside it in the rule was written.
#[test]
fn a_number_beside_something_narrower_than_the_rule_takes_the_narrower_width() {
    let text = "\
(semantics (value.i1 v) v)
(semantics (value.i8 v) v)
(semantics (trunc.i8.i1 v) (extract 0 0 v))
(semantics (x64.bit_of_8 l r) (bvand (extract 0 0 l) r))";
    let rule = "\
(rule (lower (trunc.i8.i1 (value.i8 x)))
      (x64.bit_of_8 x 1)
      (spec (= (extract 0 0 x) (result))))";
    let asked = query("t.rules", &self::rule(rule), &model(text)).expect("the model covers this");
    assert!(asked.contains("(_ bv1 1)"), "{asked}");
}

/// A body that uses one parameter at two widths says so rather than letting a number through it
/// twice at two sizes.
///
/// With anything but a number in that place the two sorts would already have disagreed and the
/// solver would never have been asked, so this is the model being held to the same standard for
/// the one kind of argument that has no width of its own.
#[test]
fn a_body_that_uses_one_number_at_two_widths_is_refused() {
    let text = "\
(semantics (value.i8 v) v)
(semantics (value.i32 v) v)
(semantics (x64.two_ways l k) (bvadd (zero_extend 8 32 (bvadd (extract 7 0 l) k)) k))
(semantics (id.i32 v) v)";
    let rule = "\
(rule (lower (id.i32 (value.i32 x)))
      (x64.two_ways x 1)
      (spec (= x (result))))";
    let said = refuse(&model(text), rule);
    assert!(said.contains("`k` is a number"), "{said}");
    assert!(said.contains("8 bits"), "{said}");
    assert!(said.contains("32 bits"), "{said}");
}

/// A complaint about a body is reported in the file the body is in.
///
/// A term carries the line it was written at and not the file it was written in, and the file
/// that used a head is not the file that defined it, so this used to name a line of the rule file
/// that was either about something else or was not there.
#[test]
fn a_complaint_about_a_body_names_the_file_the_body_is_in() {
    let text = "\
(semantics (value.i32 v) v)
(semantics (id.i32 v) v)
(semantics (x64.calls_nothing v) (x64.nothing v))";
    let rule = "\
(rule (lower (id.i32 (value.i32 x)))
      (x64.calls_nothing x)
      (spec (= x (result))))";
    let said = refuse(&model(text), rule);
    assert!(said.contains("t.model:3:"), "{said}");
    assert!(!said.contains("t.rules"), "{said}");
}
