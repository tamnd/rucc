//! The table has to be internally consistent, because everything downstream keys on it.
//!
//! These are the tests that catch the failure mode `spec/cross-compile/04-target-matrix.md` section 4.7 warns
//! about, which is a table that a human edits and nothing checks. A row whose tuple is not
//! canonical produces two cache entries for one target, and a row whose tier was raised without
//! the plan agreeing is a claim nobody made.

use std::collections::BTreeSet;
use std::str::FromStr;

use rucc_tuple::{TARGETS, TargetTuple, Tier, lookup, planned_working_count};

#[test]
fn every_row_parses() {
    for entry in TARGETS {
        assert!(entry.parse().is_ok(), "row `{}` does not parse: {:?}", entry.tuple, entry.parse());
    }
}

#[test]
fn every_row_is_written_canonically() {
    for entry in TARGETS {
        let parsed = entry.parse().expect("row parses");
        assert_eq!(
            parsed.to_canonical_string(),
            entry.tuple,
            "row `{}` is not the canonical spelling of itself",
            entry.tuple
        );
    }
}

#[test]
fn no_row_appears_twice() {
    let mut seen = BTreeSet::new();
    for entry in TARGETS {
        assert!(seen.insert(entry.tuple), "row `{}` appears twice", entry.tuple);
    }
}

#[test]
fn lookup_finds_a_row_from_any_spelling() {
    let pasted = TargetTuple::from_str("x86_64-unknown-linux-gnu").expect("parses");
    let plain = TargetTuple::from_str("x86_64-linux-gnu").expect("parses");
    assert_eq!(pasted, plain);
    assert_eq!(lookup(&pasted).map(|e| e.tuple), Some("x86_64-linux-gnu"));
}

#[test]
fn the_plan_never_promises_less_than_today() {
    for entry in TARGETS {
        assert!(
            entry.planned <= entry.tier,
            "row `{}` plans to get worse, from tier {} to tier {}",
            entry.tuple,
            entry.tier,
            entry.planned
        );
    }
}

#[test]
fn the_reference_target_is_the_best_supported_one() {
    let reference = TARGETS.iter().find(|e| e.tuple == "x86_64-linux-gnu").expect("present");
    let best_today = TARGETS.iter().map(|e| e.tier).min().expect("not empty");
    assert_eq!(reference.tier, best_today);
}

#[test]
fn the_count_claim_1_is_measured_against() {
    // `spec/cross-compile/04-target-matrix.md` section 4.3 puts this in the low thirties. The test is a bound
    // rather than an equality, because rows get added and the point is that the number is
    // computed from the table rather than typed into prose next to it.
    let working = planned_working_count();
    assert!(working >= 30, "the plan only reaches {working} working targets");
    assert!(working <= TARGETS.len());
}

#[test]
fn every_tier_is_reachable() {
    for tier in [Tier::Supported, Tier::SupportedWithEvidence, Tier::InProgress, Tier::Recognized] {
        assert!(
            TARGETS.iter().any(|e| e.planned == tier),
            "no row plans to reach tier {tier}, so the tier is decoration"
        );
    }
}
