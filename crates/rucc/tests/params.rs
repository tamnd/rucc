//! What `--param` and `--print-params` do, which is tamnd/rucc#2969.
//!
//! Section 40.12 of `spec/optimizer/40-cost-model.md` keeps every number a pass decides by in one
//! table, and `--param` moves one of them for a single run of the compiler with no build. These
//! tests hold the flag to three things. It takes the names `--print-params` lists and refuses the
//! rest. A row set to the value it already has changes nothing. And a row that is moved reaches
//! the pass that reads it, which is one fixture for each row under `tests/params`: a program from
//! tamnd/rucc-corpus whose assembly changes when that row and no other moves. A row with no
//! fixture is in [`UNREAD`] or [`NO_FIXTURE`] with the reason.

use std::path::PathBuf;
use std::process::{Command, Output};

/// The target is written down rather than taken from the host, so the listing is the same on
/// every machine that runs the suite.
const TARGET: &str = "x86_64-unknown-linux-gnu";

/// The rows a fixture shows reaching their pass: the name, the fixture, the flags it is built
/// with, and the value the row is moved to. The value is half the row's own where half changes the
/// fixture, and otherwise the smallest move that does, since some rows only decide anything at the
/// far end of their range on a program small enough to keep here. A flag that sets another row
/// moves that one out of the way, so the one under test is the one that decides.
const MOVED: &[(&str, &str, &[&str], u32)] = &[
    ("phiopt-arm-instructions", "hoist-from-both-arms", &["-O2"], 1),
    ("phiopt-factor-depth", "loop-unswitch", &["-O2"], 0),
    ("phiopt-arm-scan-instructions", "hoist-from-both-arms", &["-O2"], 0),
    ("phiopt-unpredictable-margin-percent", "if-conversion-rate", &["-O2"], 24),
    ("short-circuit-instructions", "both-edges", &["-O2"], 0),
    ("sra-max-bytes", "stack-slots", &["-O2"], 0),
    ("sra-max-pieces", "bit-field", &["-O2"], 0),
    ("sra-max-word-pieces", "loop-idiom-sum", &["-O2"], 8),
    ("loop-header-insns-for-speed", "early-exit", &["-O2"], 0),
    ("loop-header-insns-for-size", "early-exit", &["-Os"], 0),
    ("jump-thread-duplication-insns", "tested-twice", &["-O2"], 0),
    ("jump-thread-paths", "tested-twice", &["-O2"], 0),
    ("jump-thread-path-insns", "tested-twice", &["-O2"], 0),
    ("inline-frequency-clamp", "inline-order", &["-O2", "--param=large-unit-insns=0"], 1),
    ("inline-insns-single", "inline-single", &["-O2"], 10),
    ("inline-early-insns", "inline-early", &["-O2"], 3),
    ("inline-early-insns-o3", "inline-early", &["-O3"], 3),
    ("inline-insns-single-o3", "inline-single", &["-O3"], 0),
    ("inline-insns-auto", "call-time", &["-O2"], 7),
    ("inline-insns-auto-o3", "inline-loops", &["-O3"], 7),
    ("inline-called-once-insns", "dense-hot-case", &["-O2"], 0),
    ("inline-called-once-loop-depth", "dense-hot-case", &["-O2"], 0),
    ("inline-frame-growth", "inline-frame", &["-O2"], 500),
    ("inline-large-frame", "inline-small-frame", &["-O2"], 0),
    ("inline-frame-growth-conserve", "inline-frame", &["-O2", "-fconserve-stack"], 1000),
    ("inline-large-frame-conserve", "inline-small-frame", &["-O2", "-fconserve-stack"], 50),
    ("inline-hint-percent", "inline-loops", &["-O2"], 100),
    ("inline-hint-percent-o3", "inline-loops-o3", &["-O3"], 100),
    ("inline-min-speedup", "inline-loops", &["-O2"], 0),
    ("inline-min-speedup-o3", "inline-loops-o3", &["-O3"], 0),
    ("inline-unit-growth", "inline-order", &["-O2", "--param=large-unit-insns=0"], 20),
    ("large-unit-insns", "inline-loops", &["-O2"], 0),
    ("large-function-insns", "large-function", &["-O2", "--param=large-function-growth=0"], 0),
    ("large-function-growth", "large-function", &["-O2", "--param=large-function-insns=0"], 50),
    ("inline-call-time", "call-time", &["-O2"], 5),
    ("predict-expect", "hot-then", &["-O2"], 45),
    ("predict-never-returns", "never-returns", &["-O2"], 49),
    ("predict-loop-exit-not-taken", "early-exit", &["-O2"], 44),
    ("predict-loop-guard-taken", "masked-four", &["-O2"], 36),
    ("predict-pointer-not-null", "dispatch-threaded", &["-O2"], 35),
    ("predict-negative-return", "returns", &["-O2"], 49),
    ("predict-null-return", "returns", &["-O2"], 35),
    ("predict-call-not-taken", "tail-call-self", &["-O2"], 33),
    ("predict-early-return", "early-return", &["-O3"], 33),
    ("max-predicted-iterations", "dispatch-direct", &["-O2"], 800),
    ("loop-reserved-regs", "shift-xor", &["-O2"], 16),
    ("licm-expensive", "early-exit", &["-O2"], 0),
    ("unroll-max-times", "read-only", &["-O2"], 8),
    ("unroll-max-insns", "less-left", &["-O2"], 0),
    ("split-max-insns", "split", &["-O2", "-fsafety=detect"], 0),
    ("unroll-max-depth", "scope-in-a-loop", &["-O2"], 0),
    ("predict-return-blocks", "returns", &["-O2"], 0),
    ("iv-max-considered-uses", "quotient", &["-O2"], 0),
    ("ivopts-new-variable-bias", "loop-inline", &["-O2"], 1),
    ("switch-conversion-max-growth", "masked-four", &["-O2"], 0),
    ("jump-table-min-targets", "dispatch-table", &["-O2"], 5),
    ("jump-table-min-targets-for-size", "dispatch-table", &["-Os"], 48),
    ("switch-peel-percent", "sparse-hot-case", &["-O2"], 528),
    ("wasm-jump-table-min-targets", "wasm-switch", &["--target=wasm32-unknown-unknown", "-O2"], 40),
    (
        "wasm-jump-table-min-targets-for-size",
        "wasm-switch",
        &["--target=wasm32-unknown-unknown", "-Os"],
        40,
    ),
    ("dse-walk-limit", "store-then-load", &["-O2"], 0),
    ("range-test-bit-intervals", "guard-keeps-the-index", &["-O2"], 1),
    ("range-switch-cases", "range-switch", &["-O2"], 4),
    ("cons-cascade", "inline-loops", &["-O2", "-Zrewriter=consed"], 0),
    ("egraph-nodes", "inline-loops", &["-O2", "-Zrewriter=egraph"], 0),
    ("simplify-rounds", "inline-loops", &["-O2", "-Zrewriter=classical"], 0),
];

/// The rows that are written down for a pass that has not been written yet, so nothing reads them.
/// The flag takes each of them and the output does not change.
const UNREAD: &[(&str, &str)] = &[
    (
        "branch-cost-for-size",
        "The cost table holds it, and nothing outside the tests asks the table for it yet.",
    ),
    (
        "branch-cost-predictable",
        "The cost table holds it, and nothing outside the tests asks the table for it yet.",
    ),
    (
        "predictable-branch-percent",
        "Only a measured profile can call a branch predictable, and nothing outside the tests asks yet.",
    ),
    (
        "if-conversion-budget-predictable",
        "Written down for gcc's RTL if-conversion, which rucc does not have yet. phiopt is the one it has, and has rows of its own.",
    ),
    (
        "if-conversion-budget-unpredictable",
        "Written down for gcc's RTL if-conversion, which rucc does not have yet. phiopt is the one it has, and has rows of its own.",
    ),
    (
        "if-conversion-block-limit",
        "Written down for gcc's RTL if-conversion, which rucc does not have yet. phiopt is the one it has, and has rows of its own.",
    ),
    (
        "block-copy-moves-for-speed",
        "The cost table holds it as the move ratio, and the block copy lowering does not ask the table for that yet.",
    ),
    (
        "block-copy-moves-for-size",
        "The cost table holds it as the move ratio, and the block copy lowering does not ask the table for that yet.",
    ),
    (
        "reassoc-width-untuned",
        "The cost table holds it, and there is no reassociation pass to ask for it yet.",
    ),
    (
        "hot-block-fraction",
        "Only the question of whether a block is hot in its function reads it, and no pass asks that yet.",
    ),
    (
        "predict-cold-call",
        "Every caller hands the predictor an empty list of cold functions, so a call is never one.",
    ),
    (
        "profile-sum-tolerance-percent",
        "Only the check that a profile adds up reads it, and only the tests run that check.",
    ),
    (
        "align-frequency-fraction",
        "Written down for aligning hot blocks, which rucc does not do yet.",
    ),
    ("loop-align-min-iterations", "Written down for aligning loops, which rucc does not do yet."),
    ("iv-consider-all-candidates-bound", "Written down for ivopts, which does not read it yet."),
    ("scheduler-ready-list-bound", "Written down for the scheduler, which does not read it yet."),
    (
        "allocator-degradation-percent",
        "Written down for the register allocator, which does not read it yet.",
    ),
];

/// The rows a pass reads but no program tried shows moving, with why. Each was set to nothing, to
/// one, to half and eight times its own value and to the largest it takes, over the programs under
/// `tests/params`, a quarter of the facets of tamnd/rucc-corpus and every facet named for its pass.
const NO_FIXTURE: &[(&str, &str)] = &[
    (
        "jump-thread-back-edge-scale",
        "It only prices a thread across a loop's back edge, and no program tried has one that the other bounds do not already settle.",
    ),
    (
        "inline-growth-squaring-bound",
        "It only reorders the heap of calls the inliner may copy, and no program tried has two calls whose order it swaps.",
    ),
    (
        "inline-summary-clauses",
        "It only decides which bodies the inliner weighs by their summary and which it walks for each call, and the two give the same answer.",
    ),
    (
        "predict-continue-taken",
        "No program tried has a branch to a loop's next round whose layout turns on this guess rather than another one.",
    ),
    (
        "split-remade-insns",
        "The split pass runs under -fsafety=detect alone, and no program tried rebuilds enough code there to reach the bound.",
    ),
    (
        "iv-always-prune-cand-set-bound",
        "No loop tried has a candidate set big enough that pruning it finds a cheaper one.",
    ),
    (
        "ivopts-set-penalty",
        "No loop tried has candidate sets close enough in cost that the penalty for one more register picks a different one.",
    ),
    (
        "constant-p-load-depth",
        "No program tried asks __builtin_constant_p about a value behind more than one load of constant data.",
    ),
    (
        "select-forms",
        "On every program tried the matcher finds the same instruction with fewer forms of an operand, since the folds before it already shaped the addresses.",
    ),
];

/// The compiler run with those arguments.
fn rucc(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rucc"))
        .args(args)
        .output()
        .expect("the compiler is built before its own tests run")
}

/// Every row `--print-params` lists after those arguments, with its value.
fn listed(args: &[&str]) -> Vec<(String, u32)> {
    let out = rucc(&[args, &["--print-params"]].concat());
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout)
        .expect("the listing is text")
        .lines()
        .map(|line| {
            let (name, rest) = line.split_once(" = ").expect("each line is a name and a value");
            let value = rest.split(' ').next().and_then(|value| value.parse().ok());
            (name.to_owned(), value.expect("each value is a number"))
        })
        .collect()
}

/// The fixture of that name.
fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/params").join(format!("{name}.c"))
}

/// The assembly the compiler writes for that fixture with those flags. The target comes first so
/// that a fixture for another one can name it among its flags, the last `--target` being the one
/// that counts.
fn asm(name: &str, flags: &[&str]) -> Vec<u8> {
    let path = fixture(name);
    let path = path.to_str().expect("the path is text");
    let target = format!("--target={TARGET}");
    let out = rucc(&[&[target.as_str()][..], flags, &["-S", "-o", "-", path]].concat());
    assert!(out.status.success(), "{name}: {}", String::from_utf8_lossy(&out.stderr));
    out.stdout
}

#[test]
fn every_row_is_listed_once() {
    let rows = listed(&[]);
    let mut names: Vec<&str> = rows.iter().map(|(name, _)| name.as_str()).collect();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), rows.len(), "a name is listed twice");
    let written = MOVED.len() + UNREAD.len() + NO_FIXTURE.len();
    assert_eq!(written, rows.len(), "a row is missing from the tables here, or is in two of them");
    for (name, _) in &rows {
        let here = MOVED.iter().any(|row| row.0 == name)
            || UNREAD.iter().any(|row| row.0 == name)
            || NO_FIXTURE.iter().any(|row| row.0 == name);
        assert!(here, "{name} has no fixture and no reason why not");
    }
}

#[test]
fn a_name_that_is_not_a_row_is_refused_with_the_names_that_are() {
    let out = rucc(&["--param", "inline-insn-single=10", "--print-params"]);
    assert!(!out.status.success());
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(said.contains("inline-insn-single"), "{said}");
    assert!(said.contains("inline-insns-single"), "{said}");
    assert!(said.contains("max-inline-insns-single"), "{said}");
}

#[test]
fn a_value_that_is_not_a_number_is_refused() {
    for spec in [
        "inline-insns-single",
        "inline-insns-single=",
        "inline-insns-single=-1",
        "inline-insns-single=ten",
    ] {
        let out = rucc(&["--param", spec, "--print-params"]);
        assert!(!out.status.success(), "{spec} was taken");
    }
}

#[test]
fn both_spellings_set_the_row_and_no_other() {
    let before = listed(&[]);
    for args in [&["--param", "inline-insns-single=35"][..], &["--param=inline-insns-single=35"]] {
        let after = listed(args);
        for ((name, was), (_, is)) in before.iter().zip(&after) {
            let expect = if name == "inline-insns-single" { 35 } else { *was };
            assert_eq!(*is, expect, "{name} after {args:?}");
        }
    }
}

#[test]
fn gcc_s_name_sets_every_row_it_names() {
    let after = listed(&["--param", "inline-heuristics-hint-percent=300"]);
    for name in ["inline-hint-percent", "inline-hint-percent-o3"] {
        assert!(after.contains(&(name.to_owned(), 300)), "{name}: {after:?}");
    }
}

/// Every fixture with the flags it is built with, once each.
fn fixtures() -> Vec<(&'static str, &'static [&'static str])> {
    let mut all: Vec<_> = MOVED.iter().map(|&(_, fixture, flags, _)| (fixture, flags)).collect();
    all.sort_unstable();
    all.dedup();
    all
}

/// Every row set to the value it has comes before the fixture's own flags, since some of those
/// move a row out of the way and the last setting of a row is the one that counts.
#[test]
fn every_row_set_to_its_own_value_changes_nothing() {
    let all: Vec<String> =
        listed(&[]).iter().map(|(name, value)| format!("--param={name}={value}")).collect();
    let all: Vec<&str> = all.iter().map(String::as_str).collect();
    for (fixture, flags) in fixtures() {
        let set = asm(fixture, &[&all, flags].concat());
        assert!(set == asm(fixture, flags), "{fixture} {flags:?}: setting every row moved it");
    }
}

#[test]
fn a_row_that_is_moved_reaches_its_pass() {
    let mut was = Vec::new();
    for &(name, fixture, flags, value) in MOVED {
        let at = match was.iter().position(|(key, _)| *key == (fixture, flags)) {
            Some(at) => at,
            None => {
                was.push(((fixture, flags), asm(fixture, flags)));
                was.len() - 1
            }
        };
        let moved = format!("--param={name}={value}");
        let changed = asm(fixture, &[flags, &[moved.as_str()]].concat());
        assert!(changed != was[at].1, "{name}={value} changed nothing in {fixture} {flags:?}");
    }
}
