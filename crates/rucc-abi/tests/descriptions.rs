//! Properties every description has to have, and the demonstration that a further one is data.
//!
//! Design: `spec/cross-compile/06-abis.md` section 6.7.
//!
//! A description language buys nothing if a wrong description is as easy to write as a right one.
//! The classifier cannot tell the difference between a rule list that ends in a catch-all and one
//! that does not, so these are the invariants the classifier assumes and does not check, checked
//! here once for every description rather than argued about once per review.
//!
//! `an_undescribed_abi_is_a_description_and_no_code` is the one that decides whether section 6.7's
//! proposal was worth making. It adds the s390x ELF ABI in this file, as data, and classifies
//! against it. If that test ever needs a change under `src/` to keep passing, the claim that
//! bringing up an ABI is a data change has stopped being true and the crate needs rethinking
//! rather than extending.
//!
//! The proposal has since been tested the other way too. i386 SysV was added under `src/` as a
//! shipped description and cost no change to the classifier, which is the same result on an ABI
//! that a target dispatches to. The fixture here stays because s390x is the harder case: the
//! module documentation in `src/abis.rs` records why the rest of that ABI cannot be described
//! yet, and a demonstration that only ever ran on the easy ones would be worth less.

use rucc_abi::abis::{DESCRIBED, for_target};
use rucc_abi::{
    AbiDescription, Arg, Banks, Format, Pass, ReturnPointer, Rule, Scalar, Scalars, Short, Slot,
    StackArgs, Test, Travel, Variadic, pieces, record,
};
use rucc_tuple::{TARGETS, TargetEntry};

/// The two rule lists of a description, with a name for the failure message.
fn rule_lists(abi: &'static AbiDescription) -> [(&'static str, &'static [Rule]); 2] {
    [("returns", abi.returns), ("arguments", abi.arguments)]
}

#[test]
fn every_rule_list_ends_in_a_catch_all_and_has_one_nowhere_else() {
    for &abi in DESCRIBED {
        for (which, rules) in rule_lists(abi) {
            let (last, rest) = rules.split_last().unwrap_or_else(|| {
                panic!("{} has an empty {which} list, so it answers nothing", abi.name)
            });
            // Without this the classifier would fall off the end of the list with no answer, and
            // the only sensible thing it could do there is panic on a shape somebody's program
            // contains.
            assert_eq!(
                last.when,
                Test::Anything,
                "{}'s {which} list ends in {:?} rather than a catch-all",
                abi.name,
                last.when
            );
            for rule in rest {
                assert_ne!(
                    rule.when,
                    Test::Anything,
                    "{}'s {which} list has a catch-all before the end, so the rules after it are \
                     unreachable",
                    abi.name
                );
            }
        }
    }
}

#[test]
fn as_found_only_follows_a_test_that_finds_something() {
    for &abi in DESCRIBED {
        for (which, rules) in rule_lists(abi) {
            for rule in rules {
                if rule.then != Travel::AsFound {
                    continue;
                }
                // `AsFound` means the slots the test produced, so pairing it with a test that
                // produces none is a rule that says a value travels in no registers at all.
                assert!(
                    matches!(
                        rule.when,
                        Test::Homogeneous { .. }
                            | Test::FloatPair
                            | Test::X87Stack
                            | Test::Eightbytes { .. }
                    ),
                    "{}'s {which} list travels as found after {:?}, which finds nothing",
                    abi.name,
                    rule.when
                );
            }
        }
    }
}

#[test]
fn ignore_is_only_ever_the_empty_rule() {
    for &abi in DESCRIBED {
        for (which, rules) in rule_lists(abi) {
            for rule in rules {
                assert_eq!(
                    rule.then == Travel::Ignore,
                    rule.when == Test::Empty,
                    "{}'s {which} list pairs {:?} with {:?}, and an aggregate of no size is the \
                     only thing that travels nowhere",
                    abi.name,
                    rule.when,
                    rule.then
                );
            }
        }
    }
}

#[test]
fn a_rule_that_declines_has_somewhere_to_decline_to() {
    for &abi in DESCRIBED {
        for (which, rules) in rule_lists(abi) {
            let last = rules.len().saturating_sub(1);
            for (at, rule) in rules.iter().enumerate() {
                if rule.short == Short::TryNextRule {
                    assert_ne!(
                        at, last,
                        "{}'s last {which} rule tries the next one, and there is no next one",
                        abi.name
                    );
                }
            }
        }
    }
}

#[test]
fn a_return_rule_never_runs_short() {
    for &abi in DESCRIBED {
        for rule in abi.returns {
            // A return value is classified before any argument, so the banks are always full and
            // running short cannot happen. A description that says otherwise is describing
            // something that never occurs, and the reader would be right to wonder which of the
            // two facts is wrong.
            assert_eq!(
                rule.short,
                Short::Unchanged,
                "{}'s return rule {:?} says what happens when the registers run out, and they \
                 cannot have run out yet",
                abi.name,
                rule.when
            );
        }
    }
}

#[test]
fn a_shared_bank_is_counted_in_one_place() {
    for &abi in DESCRIBED {
        if abi.banks.shared {
            // Every spend on a shared bank comes out of the integer count, so a nonzero floating
            // point count would be registers the classifier never looks at.
            assert_eq!(
                abi.banks.float, 0,
                "{} shares argument positions and still counts a separate float bank",
                abi.name
            );
        }
        assert!(abi.banks.integer_width > 0, "{} has registers of no width", abi.name);
        assert!(abi.banks.float_width > 0, "{} has vector registers of no width", abi.name);
    }
}

#[test]
fn the_described_abis_have_distinct_names() {
    let mut names: Vec<&str> = DESCRIBED.iter().map(|abi| abi.name).collect();
    names.sort_unstable();
    let count = names.len();
    names.dedup();
    // The name is what a diagnostic and the generated report say, so two descriptions sharing one
    // would make a report that says the compiler classified for the right ABI when it did not.
    assert_eq!(names.len(), count, "two descriptions answer to the same name");
}

#[test]
fn every_target_with_an_abi_gets_one_of_the_described_ones() {
    let mut answered = 0;
    for entry in TARGETS {
        let target =
            TargetEntry::parse(entry).expect("the table parses, which its own tests check");
        let Some(abi) = for_target(target) else {
            continue;
        };
        assert!(
            DESCRIBED.iter().any(|described| std::ptr::eq(*described, abi)),
            "{} was given an ABI that is not in the list the report iterates",
            entry.tuple
        );
        answered += 1;
    }
    // A floor rather than an exact count, so that adding a target row does not fail this test,
    // but deleting the dispatch does.
    assert!(answered >= 10, "only {answered} of the target table's rows have an ABI");
}

#[test]
fn windows_on_aarch64_gets_no_answer_rather_than_the_almost_right_one() {
    let target = "aarch64-pc-windows-msvc".parse().expect("a row in the target table");
    // AAPCS64 with different varargs and x18 reserved, per spec/cross-compile/06-abis.md section 6.1. Answering
    // AAPCS64 here would be right for most programs and wrong for the ones that call `printf`,
    // which is exactly the failure the crate exists to avoid.
    assert!(for_target(target).is_none());
}

/// The s390x ELF ABI, written here rather than in `src/`.
///
/// Five general purpose argument registers, r2 to r6, and four floating point ones. An aggregate
/// of one, two, four or eight bytes travels in a general purpose register and anything else
/// travels as the address of a copy, which is Windows x64's rule with a different bank size. A
/// structure return value is always in memory through a hidden first argument, so the return list
/// has no size rule in it at all.
///
/// It reuses [`Test::SizeOneOf`], which Windows x64 already needed, so the mechanism count does
/// not move. That is the claim: a new ABI costs a description, and a new *idea* costs a
/// mechanism, and there are fewer ideas than there are targets.
///
/// It is a fixture rather than a shipped description, and the reason is one scalar. A 128-bit
/// `long double` on s390x travels in an even and odd pair of floating point registers, and the
/// description language has no way to say that: a floating point value here either fits one
/// vector register or moves to the general purpose bank. Everything asserted below is the
/// aggregate half, which is describable and is what the demonstration is about. The scalar half
/// is why `abis::for_target` still answers [`None`] for the s390x rows, and `src/abis.rs`
/// carries the longer version of that.
static S390X_ELF: AbiDescription = AbiDescription {
    name: "s390x ELF",
    banks: Banks { integer: 5, float: 4, shared: false, integer_width: 8, float_width: 8 },
    scalars: Scalars { in_memory: None, wide_integer_is_all_or_nothing: false },
    returns: &[
        Rule::new(Test::Empty, Travel::Ignore),
        Rule::new(Test::Anything, Travel::ByReference),
    ],
    arguments: &[
        Rule::new(Test::Empty, Travel::Ignore),
        Rule::new(Test::SizeOneOf(&[1, 2, 4, 8]), Travel::AsOneInteger),
        Rule::new(Test::Anything, Travel::ByReference),
    ],
    return_pointer: ReturnPointer::FirstArgument,
    variadic: Variadic::SameAsFixed,
    stack_args: StackArgs::RegisterSized,
};

#[test]
fn an_undescribed_abi_is_a_description_and_no_code() {
    let mut call = S390X_ELF.call();

    // A structure return value is in memory here whatever its size, and its address is the first
    // argument, so one of the five registers is gone before the call has any arguments.
    let small = pieces(&[Scalar::integer(4)]);
    assert_eq!(call.returns(&Arg::Aggregate(record(&small))), Pass::Reference);
    assert_eq!(call.integer_left(), 4);

    // Eight bytes of two floats in a general purpose register, the same as Windows x64 and for
    // the same reason, which is that the rule reads the size and nothing else.
    let two_floats = pieces(&[Scalar::float(Format::Single, 4); 2]);
    assert_eq!(
        call.argument(&Arg::Aggregate(record(&two_floats))),
        Pass::Pieces(vec![Slot::Integer { offset: 0, size: 8 }])
    );

    // Three bytes is not a size a register holds, so it travels as an address.
    let three = pieces(&[Scalar::integer(1); 3]);
    assert_eq!(call.argument(&Arg::Aggregate(record(&three))), Pass::Reference);

    // The two banks are counted apart, unlike Windows x64, which is one field of difference.
    assert_eq!(call.float_left(), 4);
    assert_eq!(call.argument(&Arg::Scalar(Scalar::float(Format::Double, 8))), Pass::Direct);
    assert_eq!(call.float_left(), 3);
    assert_eq!(call.integer_left(), 2);
}

#[test]
fn the_fixture_satisfies_the_same_properties_as_the_shipped_descriptions() {
    // The properties above iterate `DESCRIBED`, which this one is deliberately not in, so it
    // would have escaped every check in this file. Running the two that catch real mistakes
    // against it keeps the demonstration honest.
    for (which, rules) in rule_lists(&S390X_ELF) {
        let (last, rest) = rules.split_last().expect("a non-empty list");
        assert_eq!(last.when, Test::Anything, "the {which} list has no catch-all");
        assert!(rest.iter().all(|rule| rule.when != Test::Anything));
        assert!(
            rules.iter().all(|rule| (rule.then == Travel::Ignore) == (rule.when == Test::Empty))
        );
    }
}
