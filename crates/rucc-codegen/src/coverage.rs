//! Which IR opcodes have somewhere to go, and which do not.
//!
//! Design: `spec/10-backend.md` section 10.2, under **Coverage**.
//!
//! Every opcode has to be lowered by something or be a hole somebody wrote down. Without this the
//! way a hole is found is that somebody compiles a program containing one and the selector reports
//! that it cannot lower an instruction, which is a fine diagnostic and a bad discovery mechanism:
//! it turns a gap in the rule set into a user's problem rather than a failing build.
//!
//! # The three answers
//!
//! An opcode is lowered by a rule, or somewhere a rule cannot reach, or nowhere.
//!
//! The first is the ordinary answer and the one this can check by itself. [`crate::term`] says
//! every name a rule could be written at, the table says every name one is written at, and an
//! opcode is covered when each of its names is in both. That is what makes this a check about
//! widths rather than about opcodes: an `add` with a rule at four widths and no rule at the fifth
//! is not covered, and would be reported here as the missing name rather than as a covered opcode.
//!
//! The second is [`ELSEWHERE`], which is not a gap. `spec/10-backend.md` names five of them and
//! there are more now, and they are all the same kind of thing: an opcode whose lowering depends on
//! something no pattern can see. Where a call's arguments go depends on the signature, where a
//! local lives depends on the frame, an unconditional jump is an edge and edges live on the block,
//! and a `memcpy` is a run of moves whose length is a constant the pattern would have to count. A
//! rule matches one term and can say none of that.
//!
//! The third is [`GAPS`], which is the number `spec/15-testing.md` section 15.8 says we keep. Each
//! entry names why it is there and the issue that closes it, so that an opcode nobody has written a
//! rule for is a decision somebody wrote down rather than a surprise.
//!
//! # What makes the lists honest
//!
//! An entry that stops being true fails. An opcode on either list that a rule starts covering is a
//! stale entry and the tests below say so by name, which is the same rule the exclusion lists in
//! the compatibility harness are kept under: a list nothing checks is a list that only grows.
//!
//! The direction this cannot check is an opcode moving from [`GAPS`] to [`ELSEWHERE`] without the
//! list following it, because where an opcode is lowered by name is a `match` arm and there is
//! nothing to ask about a `match` arm from here. What that costs is one line of a list going out of
//! date; what it does not cost is a gap going unnoticed, since the opcode is still on a list and
//! still counted.

use core::fmt;

use rucc_ir::Opcode;

use crate::select::{Table, Test};
use crate::term;

/// An opcode no rule is written about, and the place that lowers it instead.
///
/// Not one of these is a gap. Each is an opcode whose lowering depends on something a pattern
/// cannot see, so the answer lives where that something is known.
pub static ELSEWHERE: &[(Opcode, &str)] = &[
    // The convention. What a call's operands are is whatever the signature made them, and which
    // register each one arrives in depends on the classification of every argument before it.
    (Opcode::Call, "`crate::abi`, which builds a call out of the convention"),
    (Opcode::CallIndirect, "`crate::abi`, the same instruction with the callee in a register"),
    // The frame, which is not known until the allocator has finished running out of registers.
    (Opcode::Alloca, "`crate::lower`, as an address into a frame `crate::frame` lays out later"),
    // A relocation, which is right because of what the linker does rather than because of what
    // any bitvector equals.
    (Opcode::GlobalAddr, "`crate::lower`, a `lea` off the instruction pointer with a name on it"),
    // No instruction at all. The IR keeps the width the same and the machine has one register
    // file for both, so the value is already where it needs to be.
    (Opcode::PtrToInt, "`crate::lower`, which renames the value rather than computing anything"),
    (Opcode::IntToPtr, "`crate::lower`, the same rename the other way round"),
    // The edges and the two ways of writing down that control does not arrive.
    (Opcode::Jump, "`crate::layout`, since an edge is on the block and not in the block"),
    (Opcode::Unreachable, "nothing at all, which is the answer for a place control does not reach"),
    (Opcode::UnreachableHint, "nothing at all, for the same reason"),
    // Rewritten into the opcodes above before selection ever sees them.
    (Opcode::Switch, "`crate::expand`, into the compare and branch chain it is"),
    (Opcode::FConst, "`crate::expand`, into a constant in memory and a load of it"),
    (Opcode::FNeg, "`crate::expand`, into the sign bit flip it is"),
    (Opcode::UIToFP, "`crate::expand`, into a widening and a signed conversion"),
    (Opcode::FPToUI, "`crate::expand`, into a signed conversion and a narrowing"),
    (Opcode::Memcpy, "`crate::expand`, into the moves it stands for"),
    (Opcode::Memset, "`crate::expand`, into the fills it stands for"),
    (Opcode::Memmove, "`crate::expand`, into a call, since the two regions may overlap"),
    // The variable argument list, which is four opcodes reading a structure the ABI describes.
    (Opcode::VaStart, "`crate::varargs`, which writes the register save area the ABI describes"),
    (Opcode::VaArg, "`crate::varargs`, into the walk over that structure"),
    (Opcode::VaObject, "`crate::varargs`, the same walk for something that arrived in memory"),
    (Opcode::VaCopy, "`crate::varargs`, into a copy of the structure"),
    (Opcode::VaEnd, "`crate::varargs`, which removes it, since there is nothing to undo"),
];

/// An opcode nothing lowers, why it is here, and the issue that closes it.
///
/// This is the count `spec/15-testing.md` section 15.8 asks for. It is not zero yet and the
/// spec says it should be, which is the honest reading of where the back end is: every one of
/// these is a feature nobody has written, and all but three of them are opcodes the front end
/// cannot produce either, so a program that reaches one of these is a program that reaches an
/// unimplemented builtin first.
pub static GAPS: &[(Opcode, &str, &str)] = &[
    (Opcode::Splat, "a vector, and no rule is written about a lane count", "tamnd/rucc#200"),
    (
        Opcode::TargetIntrinsic,
        "the same, since what needs one is a vector builtin",
        "tamnd/rucc#200",
    ),
    (Opcode::BlockAddr, "the address of a label", "tamnd/rucc#353"),
    (Opcode::IndirectBr, "the branch a computed goto turns into", "tamnd/rucc#353"),
    (
        Opcode::FRem,
        "a call to `fmod`, so a link line question as much as a lowering one",
        "tamnd/rucc#226",
    ),
    (
        Opcode::Fma,
        "a call or one instruction, depending on what the machine is told it has",
        "tamnd/rucc#226",
    ),
    (Opcode::AtomicLoad, "an ordering, which the IR cannot say yet", "tamnd/rucc#311"),
    (Opcode::AtomicStore, "the same", "tamnd/rucc#311"),
    (Opcode::AtomicRmw, "the same, and a `lock` prefix per operation", "tamnd/rucc#311"),
    (Opcode::Cmpxchg, "the same, and a result that is a pair", "tamnd/rucc#311"),
    (
        Opcode::Fence,
        "the same, and nothing at all on this machine for most orderings",
        "tamnd/rucc#311",
    ),
    (
        Opcode::Ctlz,
        "one instruction on a machine that has it and several on one that does not",
        "tamnd/rucc#310",
    ),
    (Opcode::Cttz, "the same", "tamnd/rucc#310"),
    (Opcode::Ctpop, "the same", "tamnd/rucc#310"),
    (Opcode::Bswap, "three instructions and no rule", "tamnd/rucc#307"),
    (Opcode::Bitreverse, "a node nothing writes and nothing lowers", "tamnd/rucc#363"),
    (
        Opcode::SAddOverflow,
        "a result and a flag together, which no rule can write",
        "tamnd/rucc#309",
    ),
    (Opcode::UAddOverflow, "the same", "tamnd/rucc#309"),
    (Opcode::SSubOverflow, "the same", "tamnd/rucc#309"),
    (Opcode::USubOverflow, "the same", "tamnd/rucc#309"),
    (Opcode::SMulOverflow, "the same", "tamnd/rucc#309"),
    (Opcode::UMulOverflow, "the same", "tamnd/rucc#309"),
    (Opcode::Expect, "a branch weight nothing reads yet", "tamnd/rucc#364"),
    (Opcode::Prefetch, "one instruction, once the hints have somewhere to go", "tamnd/rucc#313"),
    (Opcode::FrameAddress, "a walk up the frame pointers", "tamnd/rucc#312"),
    (Opcode::ReturnAddress, "the same walk, one word further along", "tamnd/rucc#312"),
    (
        Opcode::StackSave,
        "a frame that can grow, as a variable length array needs",
        "tamnd/rucc#291",
    ),
    (Opcode::StackRestore, "the same", "tamnd/rucc#291"),
    (
        Opcode::SetjmpMarker,
        "a call that returns twice, which the allocator has to be told about",
        "tamnd/rucc#223",
    ),
    (Opcode::LongjmpMarker, "the same", "tamnd/rucc#223"),
    (Opcode::TailCall, "a terminator nothing writes and nothing lowers", "tamnd/rucc#365"),
    (
        Opcode::InlineAsm,
        "a template, its constraints, and sixty eight torture programs",
        "tamnd/rucc#349",
    ),
];

/// A width no rule is written at, why, and the issue that closes it.
///
/// The other half of coverage, and the half an opcode list cannot say. An opcode is covered when
/// every name it has is a name a rule is written at, and a width with no name has no names to
/// check: an `add` of two `__int128`s is not a missing rule for `add`, it is a width the rule
/// language cannot spell. So the widths are written down here for the same reason the opcodes are
/// written down above.
pub static WIDTHS: &[(&str, &str, &str)] = &[
    (
        "one bit",
        "everything but and, or, xor, a constant, and the widening out of one",
        "tamnd/rucc#352",
    ),
    (
        "a hundred and twenty eight bits",
        "no register pair, so nothing at that width has a name",
        "tamnd/rucc#351",
    ),
    (
        "eighty bits",
        "a long double is on the x87 stack and no rule is about that stack",
        "tamnd/rucc#326",
    ),
    (
        "a vector of any lane count",
        "a rule at a width says nothing about how many lanes",
        "tamnd/rucc#200",
    ),
];

/// What a target's rules cover, and what they do not.
#[derive(Debug)]
pub struct Report {
    /// The rule file this is about, so that anything said about it names a file to open.
    pub source: &'static str,
    /// How many opcodes the IR has.
    pub opcodes: usize,
    /// The opcodes every name of which a rule is written at.
    pub by_rule: Vec<Opcode>,
    /// How many names those are, which is one per opcode and width.
    pub names: usize,
    /// A name a rule could be written at and none is, which is what a missing rule looks like.
    pub uncovered: Vec<(Opcode, &'static str)>,
    /// A name a rule is written at that nothing can ever be called, which is a dead rule.
    pub unreachable: Vec<&'static str>,
    /// The opcodes lowered somewhere a rule cannot reach.
    pub elsewhere: Vec<Opcode>,
    /// The opcodes nothing lowers.
    pub gaps: Vec<Opcode>,
    /// The opcodes on none of the three lists, which is what a new opcode is until somebody says
    /// where it goes.
    pub unaccounted: Vec<Opcode>,
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "rucc-codegen: {} lowers {} of the {} IR opcodes by rule at {} names, {} are lowered \
             where no rule reaches and {} have no lowering yet",
            self.source,
            self.by_rule.len(),
            self.opcodes,
            self.names,
            self.elsewhere.len(),
            self.gaps.len()
        )
    }
}

/// What a table covers.
///
/// Nothing is executed and nothing is compiled. The rule set and the naming of instructions are
/// both data, and the answer is a comparison of two lists.
#[must_use]
pub fn report(table: &Table) -> Report {
    let named = term::heads();
    let patterns = pattern_heads(table);

    let mut by_rule = Vec::new();
    let mut uncovered = Vec::new();
    for &(opcode, name) in &named {
        if patterns.contains(&name) {
            by_rule.push(opcode);
        } else {
            uncovered.push((opcode, name));
        }
    }
    // An opcode is covered when every name it has is covered, so one missing width takes the
    // whole opcode off the list however many of its other widths are there.
    for &(opcode, _) in &uncovered {
        by_rule.retain(|&covered| covered != opcode);
    }
    by_rule.sort_unstable();
    by_rule.dedup();

    let names = named.len() - uncovered.len();
    let unreachable: Vec<&'static str> = patterns
        .iter()
        .filter(|head| !named.iter().any(|(_, name)| name == *head))
        .copied()
        .collect();

    let elsewhere: Vec<Opcode> = ELSEWHERE.iter().map(|&(opcode, _)| opcode).collect();
    let gaps: Vec<Opcode> = GAPS.iter().map(|&(opcode, ..)| opcode).collect();
    let unaccounted: Vec<Opcode> = Opcode::all()
        .filter(|opcode| {
            !by_rule.contains(opcode) && !elsewhere.contains(opcode) && !gaps.contains(opcode)
        })
        .collect();

    Report {
        source: table.source,
        opcodes: Opcode::all().count(),
        by_rule,
        names,
        uncovered,
        unreachable,
        elsewhere,
        gaps,
        unaccounted,
    }
}

/// Every name a rule in a table is written about, which is the first test the trie makes.
///
/// Node zero is the root of the trie over the patterns and the first thing any walk asks is what
/// the term in hand is called, so its tests are exactly the set of pattern heads. There is no
/// wildcard there to worry about: a rule matching any term at all is one nobody has written and
/// one that would be an error to write, since a lowering has to know what it is lowering.
fn pattern_heads(table: &Table) -> Vec<&'static str> {
    let Some(root) = table.nodes.first() else { return Vec::new() };
    let mut found: Vec<&'static str> = root
        .tests
        .iter()
        .filter_map(|(test, _)| match test {
            Test::App { head, .. } => Some(*head),
            Test::Int(_) => None,
        })
        .collect();
    found.sort_unstable();
    found.dedup();
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::select::x86_64::TABLE;

    /// The claim the whole module is for, in the direction that matters: a name an instruction
    /// can be called by is a name a rule is written at. This is the width check as much as the
    /// opcode check, since a name is an opcode and a width together.
    #[test]
    fn every_name_an_instruction_can_have_is_one_a_rule_is_written_at() {
        let report = report(&TABLE);
        assert!(
            report.uncovered.is_empty(),
            "nothing in {} lowers these, and each is an opcode at a width the rule language can \
             spell: {:?}",
            report.source,
            report.uncovered
        );
    }

    /// And the other direction, which costs nothing to ask and finds a rule that can never fire.
    /// A pattern head no instruction is ever called by is a rule written against a name that was
    /// renamed or misspelled, and it would sit there proved and unreachable.
    #[test]
    fn every_name_a_rule_is_written_at_is_one_an_instruction_can_have() {
        let report = report(&TABLE);
        assert!(
            report.unreachable.is_empty(),
            "{} has rules for these and no instruction is ever called one: {:?}",
            report.source,
            report.unreachable
        );
    }

    /// Every opcode is one of the three things, so a new opcode in the IR fails this until
    /// somebody says where it goes. That is the whole point: the answer for a new opcode should
    /// be written down when it is added rather than discovered by a user compiling a program.
    #[test]
    fn every_opcode_is_lowered_or_is_a_gap_somebody_wrote_down() {
        let report = report(&TABLE);
        assert!(
            report.unaccounted.is_empty(),
            "no rule lowers these, `ELSEWHERE` does not say where they are lowered and `GAPS` \
             does not say why they are not: {:?}",
            report.unaccounted
        );
        assert_eq!(
            report.by_rule.len() + report.elsewhere.len() + report.gaps.len(),
            report.opcodes,
            "the three lists overlap, so an opcode is counted twice"
        );
    }

    /// An entry that starts being covered fails, which is the rule every list in this project is
    /// kept under. An opcode a rule now lowers is one that should be off both lists, and a list
    /// that keeps claiming otherwise is a list nobody can read.
    #[test]
    fn an_entry_a_rule_now_covers_is_a_stale_entry() {
        let report = report(&TABLE);
        for &(opcode, where_) in ELSEWHERE {
            assert!(
                !report.by_rule.contains(&opcode),
                "`{}` is lowered by a rule now, so the `ELSEWHERE` entry saying it is lowered by \
                 {where_} is stale",
                opcode.name()
            );
        }
        for &(opcode, why, issue) in GAPS {
            assert!(
                !report.by_rule.contains(&opcode),
                "`{}` is lowered by a rule now, so the `GAPS` entry saying it is {why} is stale \
                 and {issue} may be closed",
                opcode.name()
            );
            assert!(
                !report.elsewhere.contains(&opcode),
                "`{}` is on both lists, so it is both lowered and not lowered",
                opcode.name()
            );
        }
    }

    /// Every gap names an issue, since a gap with no issue behind it is a gap nobody has decided
    /// anything about, which is the thing this module exists to stop.
    #[test]
    fn every_gap_names_the_issue_that_closes_it() {
        let issues = GAPS
            .iter()
            .map(|&(_, _, issue)| issue)
            .chain(WIDTHS.iter().map(|&(_, _, issue)| issue));
        for issue in issues {
            let number = issue
                .strip_prefix("tamnd/rucc#")
                .unwrap_or_else(|| panic!("{issue} is not an issue in this project's tracker"));
            assert!(number.parse::<u32>().is_ok(), "{issue} does not name an issue number");
        }
    }

    /// The count, which `spec/15-testing.md` section 15.8 says we keep about ourselves. CI runs
    /// this test with the output shown, so the number lands in a log next to the rule proof
    /// rather than in a file somebody has to go and read.
    #[test]
    fn the_count_is_reported() {
        let report = report(&TABLE);
        println!("{report}");
        for &(opcode, why, issue) in GAPS {
            println!("rucc-codegen: no lowering for `{}`, which is {why}: {issue}", opcode.name());
        }
        for &(width, why, issue) in WIDTHS {
            println!("rucc-codegen: no rule at {width}, which is {why}: {issue}");
        }
        assert_eq!(report.gaps.len(), GAPS.len());
    }

    /// What the root of the trie is, which is the assumption [`pattern_heads`] rests on. If the
    /// rule compiler ever built the trie some other way this would say so, rather than the
    /// coverage numbers quietly becoming a report about an empty list.
    #[test]
    fn the_root_of_the_trie_is_the_head_of_every_pattern() {
        let heads = pattern_heads(&TABLE);
        assert!(!heads.is_empty(), "the table has rules and the root of the trie tests nothing");
        for rule in TABLE.rules {
            let head = rule
                .pattern
                .strip_prefix('(')
                .and_then(|rest| rest.split([' ', ')']).next())
                .expect("a pattern is an application");
            assert!(
                heads.contains(&head),
                "line {}: {} is a pattern whose head the root of the trie does not test",
                rule.line,
                rule.pattern
            );
        }
    }
}
