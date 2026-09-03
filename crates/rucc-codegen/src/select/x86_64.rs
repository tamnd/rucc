//! The x86-64 lowering table.
//!
//! Everything below the module comment is generated from `rules/x86-64.rules` by `rucc-rules`
//! when this crate is built, and none of it is in the repository. The rule file is the only
//! place the rules are written, which is what makes the table that is matched with and the
//! table `rucc-verify` proves things about the same table.
//!
//! To read the rules, read the rule file. To read the automaton they compile into, build the
//! crate and read `x86-64.rs` under the build directory, which is a file worth looking at once
//! for the shape of it and never again.

// A guard is emitted as the comparison the rule writes, so a rule saying a shift count is at
// least zero and less than the width comes out as two comparisons rather than as a range. That
// is deliberate: the generated line and the rule it came from should read the same, and the
// suggestion to write it another way is advice for somebody editing code, which nobody here is.
#![allow(clippy::manual_range_contains)]

include!(concat!(env!("OUT_DIR"), "/x86-64.rs"));

#[cfg(test)]
mod tests {
    use rucc_target::x86_64;

    use super::TABLE;
    use crate::select::Piece;

    /// The prefix a rule file puts in front of a machine term, which is how it says which target
    /// the term belongs to. It is not part of the opcode.
    const PREFIX: &str = "x64.";

    /// The two address constructors, which are not instructions. An addressing mode is an
    /// argument to `lea` and to every memory operand after it, so it is written as a term in the
    /// rule file and built by the selector into the instruction that takes it.
    const AMODES: &[&str] = &["amode_base_index_scale", "amode_index_scale"];

    /// Every head this table can write, in and under the replacements.
    fn heads() -> Vec<&'static str> {
        let mut found: Vec<&'static str> = TABLE
            .rules
            .iter()
            .flat_map(|rule| rule.replacement.iter())
            .filter_map(|piece| match piece {
                Piece::App { head, .. } => Some(*head),
                _ => None,
            })
            .collect();
        found.sort_unstable();
        found.dedup();
        found
    }

    #[test]
    fn every_instruction_the_table_writes_is_described() {
        for head in heads() {
            if AMODES.contains(&head) {
                continue;
            }
            let opcode = head.strip_prefix(PREFIX).unwrap_or_else(|| {
                panic!("{head} is neither an x86-64 term nor an addressing mode")
            });
            assert!(
                x86_64::form(opcode).is_some(),
                "{head} is selected by a rule and `rucc_target::x86_64` does not say what it \
                 does with its operands"
            );
        }
    }

    #[test]
    fn every_described_instruction_is_reachable_from_a_rule() {
        let written = heads();
        for &(opcode, _) in x86_64::INSTS {
            let head = format!("{PREFIX}{opcode}");
            assert!(
                written.contains(&head.as_str()),
                "{opcode} is described and no rule in {} selects it",
                TABLE.source
            );
        }
    }
}
