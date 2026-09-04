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
    /// operands are is whatever the signature made them, and a call through an address is the same
    /// instruction with one operand more. The return is not here, because where a return value
    /// goes depends on nothing but the value, which is exactly what a rule can say.
    const CONVENTION: &[&str] = &[
        "arg_val_8",
        "arg_val_16",
        "arg_val_32",
        "arg_val_64",
        "arg_val_f32",
        "arg_val_f64",
        "call",
        "call_reg",
    ];

    /// The instructions the block layout writes rather than a rule.
    ///
    /// A rule sees one branch and the layout is about the order of every block in the function, so
    /// which arm falls through is not something any pattern could say. That answer is what decides
    /// whether the jump goes to the arm the condition is true for or the other one, and whether
    /// there is a second jump after it, so all four of these are written where the answer is.
    const LAYOUT: &[&str] = &["test_rr_8", "jcc_e", "jcc_ne", "jmp"];

    /// The instructions a frame writes rather than a rule.
    ///
    /// A prologue, an epilogue, a copy, a spill and a reload are not in the program. They are what
    /// the allocator's answer costs, so they are written after it, by `crate::finish` reading
    /// `x86_64::FRAME`. Six of the names that describes are already reachable from a rule, since a
    /// prologue taking its frame is a subtraction and a spill is a store, and those are not here:
    /// this is only the ones nothing else can reach.
    const FRAME: &[&str] =
        &["push_64", "pop_64", "ret", "mov_rr_64", "movaps_rr", "movaps_rm", "movaps_mr"];

    #[test]
    fn every_instruction_exempt_from_a_rule_is_one_a_frame_really_writes() {
        // The same claim as the one about the convention, so that this list cannot grow an opcode
        // that no frame asks for. In the order `x86_64::FRAME` names them, the moves last because
        // there is one set of them per class the allocator may spill.
        let frame = &x86_64::FRAME;
        let mut written = vec![frame.push, frame.pop, frame.ret];
        for class in frame.classes {
            written.extend([class.mov, class.load, class.store]);
        }
        // What is left after the ones a rule already reaches, which are the loads and the stores
        // of a general purpose register, since those are the same instructions a program's own
        // reads and writes of memory are.
        written.retain(|opcode| !heads().contains(&format!("{PREFIX}{opcode}").as_str()));
        assert_eq!(written, FRAME);
    }

    #[test]
    fn every_instruction_exempt_from_a_rule_is_one_the_convention_really_writes() {
        // An exemption list that nothing checks is a hole, since an opcode dropped into it stops
        // being covered by either direction of the pinning. These are the ones `crate::abi` can
        // name, at the four integer widths and the two float formats it has names for an
        // argument in, and no others.
        let strip = |head: &'static str| head.strip_prefix(PREFIX).expect("an x86-64 term");
        let named = |ty| strip(crate::abi::head_of(ty).expect("every width the pseudos cover"));
        let written: Vec<&str> = [8, 16, 32, 64]
            .into_iter()
            .map(|bits| named(rucc_ir::Type::int(bits)))
            .chain(
                [rucc_ir::Float::F32, rucc_ir::Float::F64]
                    .map(|at| named(rucc_ir::Type::float(at))),
            )
            .chain([strip(crate::abi::CALL), strip(crate::abi::CALL_REG)])
            .collect();
        assert_eq!(written, CONVENTION);
    }

    #[test]
    fn every_described_instruction_is_reachable_from_a_rule() {
        let written = heads();
        for &(opcode, _) in x86_64::INSTS {
            if CONVENTION.contains(&opcode) || LAYOUT.contains(&opcode) || FRAME.contains(&opcode) {
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
