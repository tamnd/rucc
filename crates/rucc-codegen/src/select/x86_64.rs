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
    const AMODES: &[&str] =
        &["amode_base_index_scale", "amode_index_scale", "amode_base", "amode_base_offset"];

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

    /// The order the operands of a store are written in, which is the IR's and not a choice this
    /// file makes.
    ///
    /// A pattern is matched against an instruction's operand list by position, so a rule that
    /// names the address where the IR holds the value is a rule that stores to the value and
    /// writes the address into memory. Nothing in a proof would catch it, because a proof is
    /// about the rule file agreeing with itself, and both halves would be wrong in the same way.
    /// `rucc_ir::Builder::store` takes the value first and the machine instruction takes it last,
    /// which is why the two halves of one of these rules read in opposite orders.
    #[test]
    fn a_store_is_written_with_the_value_first_because_that_is_where_the_ir_keeps_it() {
        let mut seen = 0;
        for rule in TABLE.rules {
            let Some(rest) = rule.pattern.strip_prefix("(store.") else { continue };
            let (width, operands) = rest.split_once(' ').expect("a store takes operands");
            assert!(
                operands.starts_with(&format!("(value.{width} ")),
                "line {}: {} binds something other than the value it is storing first",
                rule.line,
                rule.pattern
            );
            assert!(
                operands.contains("(value.i64 "),
                "line {}: {} reaches no address",
                rule.line,
                rule.pattern
            );
            seen += 1;
        }
        assert_eq!(seen, 8, "the store rules moved and this test did not follow them");
    }

    /// The instructions the calling convention writes rather than a rule.
    ///
    /// Two kinds of them. Naming the register an argument arrived in, where an argument is depends
    /// on its position in the signature and on the classification of every argument before it,
    /// and a rule pattern sees one term and has no way to say any of that, so `crate::abi` builds
    /// these from the convention instead. Calling a name is the same the other way round: what its
    /// operands are is whatever the signature made them. The return is not here, because where a
    /// return value goes depends on nothing but the value, which is exactly what a rule can say.
    const CONVENTION: &[&str] = &["arg_val_8", "arg_val_16", "arg_val_32", "arg_val_64", "call"];

    #[test]
    fn every_instruction_exempt_from_a_rule_is_one_the_convention_really_writes() {
        // An exemption list that nothing checks is a hole, since an opcode dropped into it stops
        // being covered by either direction of the pinning. These are the five `crate::abi` can
        // name, at the four widths it has names for an argument, and no others.
        let strip = |head: &'static str| head.strip_prefix(PREFIX).expect("an x86-64 term");
        let written: Vec<&str> = [8, 16, 32, 64]
            .into_iter()
            .map(|bits| {
                strip(
                    crate::abi::head_of(rucc_ir::Type::int(bits))
                        .expect("every width the pseudos cover"),
                )
            })
            .chain([strip(crate::abi::CALL)])
            .collect();
        assert_eq!(written, CONVENTION);
    }

    #[test]
    fn every_described_instruction_is_reachable_from_a_rule() {
        let written = heads();
        for &(opcode, _) in x86_64::INSTS {
            if CONVENTION.contains(&opcode) {
                continue;
            }
            let head = format!("{PREFIX}{opcode}");
            assert!(
                written.contains(&head.as_str()),
                "{opcode} is described and no rule in {} selects it",
                TABLE.source
            );
        }
    }
}
