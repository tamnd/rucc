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
        assert_eq!(seen, 12, "the store rules moved and this test did not follow them");
    }

    /// The instructions the calling convention writes rather than a rule.
    ///
    /// Three kinds of them. Naming the register an argument arrived in, where an argument is
    /// depends on its position in the signature and on the classification of every argument before
    /// it, and a rule pattern sees one term and has no way to say any of that, so `crate::abi`
    /// builds these from the convention instead. Calling a name is the same the other way round:
    /// what its operands are is whatever the signature made them, and a call through an address is
    /// the same instruction with one operand more.
    ///
    /// The second half of a value that comes back in two registers is the third. A return of one
    /// value is a rule, because where that value goes depends on nothing but the value, which is
    /// exactly what a rule can say. A return of two is not, because which register the second half
    /// is in depends on the first half: the two register files are counted separately, so a
    /// `double` and a `long` both come back at place zero and two `long`s do not.
    const CONVENTION: &[&str] = &[
        "arg_val_8",
        "arg_val_16",
        "arg_val_32",
        "arg_val_64",
        "arg_val_f32",
        "arg_val_f64",
        "ret_val2_8",
        "ret_val2_16",
        "ret_val2_32",
        "ret_val2_64",
        "ret_val2_f32",
        "ret_val2_f64",
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

    /// The instruction the memory model writes rather than a rule.
    ///
    /// A barrier computes nothing, so there is no equality for the solver to discharge and no
    /// pattern for a rule to be written as. What makes it the right answer is what the machine
    /// promises about the order two other instructions become visible in, which is a claim about
    /// the program around it rather than about any value. `crate::lower` writes it by name, at the
    /// strongest ordering and nowhere else, and `crate::expand` says why the strongest is the only
    /// one that costs anything here.
    const BARRIER: &[&str] = &["mfence"];

    /// The instructions a frame writes rather than a rule.
    ///
    /// A prologue, an epilogue, a copy, a spill and a reload are not in the program. They are what
    /// the allocator's answer costs, so they are written after it, by `crate::finish` reading
    /// `x86_64::FRAME`. Six of the names that describes are already reachable from a rule, since a
    /// prologue taking its frame is a subtraction and a spill is a store, and those are not here:
    /// this is only the ones nothing else can reach.
    const FRAME: &[&str] =
        &["push_64", "pop_64", "ret", "mov_rr_64", "movaps_rr", "movaps_rm", "movaps_mr"];

    /// The two instructions that reach the x87 stack, which no rule selects yet.
    ///
    /// A third kind of exemption, and one that will not last. `fldt` and `fstpt` are the only way
    /// an eighty bit float gets to the only unit that can do arithmetic on it and back again, and
    /// there is no arithmetic yet: `tamnd/rucc#540` has this as its third box and the conversions
    /// and the arithmetic as its fourth and fifth. So this is a description of the machine that
    /// arrived before the rules that use it, which the list below is also full of.
    ///
    /// Whether either of them ever becomes reachable from a rule is the open part. Neither
    /// computes anything on its own, because what one leaves behind and the other picks up is the
    /// top of the stack and that is not a value a rule can name, which is the same reason the
    /// comparisons here are one opcode and not two. The likely answer is that they stay written by
    /// the code generator, the way a frame's instructions are, and this list says the same about
    /// them either way: nothing reaches them today.
    const X87: &[&str] = &[
        "fld_t", "fstp_t", "fld_s", "fld_l", "fild_l", "fild_ll", "fstp_s", "fstp_l", "fistp_l",
        "fistp_ll", "fnstcw", "fldcw",
    ];

    /// The instructions no rule selects yet, because the rules that selected them were taken out.
    ///
    /// A different kind of exemption from the three above. Those say an instruction is written
    /// somewhere a rule cannot reach and always will be. These say nobody reaches one at all right
    /// now, and name the work that puts the rules back.
    ///
    /// The rules went out under `tamnd/rucc#368`. C promotes the operands of an arithmetic
    /// operator to `int`, so a byte add and a two byte compare are things no C program asks the
    /// back end for, and the rules at those widths sat proved and never selected over the whole
    /// torture corpus at every optimization level. The width narrowing pass in `tamnd/rucc#375` is
    /// what asks for them, and the rules come back with it.
    ///
    /// The descriptions stayed. A description says what an x86-64 instruction is, how long it is
    /// and how it encodes, and that is true whether or not anything selects it. Taking them out
    /// would be deleting a correct account of the machine to make a list shorter, and putting them
    /// back is then a second thing to get right rather than a line of a rule file.
    const NARROW: &[&str] = &[
        // Three of the two address forms against an immediate. The `narrow` pass does write the
        // shape, since `char c = a | 1;` narrows to a byte `or` against a byte constant, and no
        // rule selects these yet: the constant goes into a register and the register with
        // register rule takes it. Their `add`, `sub` and `and` siblings do have rules and are
        // reached by the bitfield lowering, so this is six rules missing rather than a shape
        // nothing writes.
        "or_ri_8",
        "or_ri_16",
        "xor_ri_8",
        "xor_ri_16",
        "imul_ri_8",
        "imul_ri_16",
        // The divides, which are four instructions per width because the quotient and the
        // remainder come out of one division in two different registers. `narrow` refuses these
        // on purpose: the most negative byte over minus one is a defined hundred and twenty eight
        // at four bytes and is the overflow that raises at one, so narrowing a division wants a
        // range that rules the pair out and there is no range analysis yet.
        "idiv_quo_8",
        "idiv_quo_16",
        "idiv_rem_8",
        "idiv_rem_16",
        "div_quo_8",
        "div_quo_16",
        "div_rem_8",
        "div_rem_16",
        // The shifts by a value, whose count is in `cl` whatever the width being shifted is. The
        // same refusal for the same kind of reason: a count of twenty is a defined shift to zero
        // at four bytes and is poison at one, so only a count that is a constant below the narrow
        // width narrows, and that one selects the immediate forms which do have rules.
        "shl_rcl_8",
        "shl_rcl_16",
        "shr_rcl_8",
        "shr_rcl_16",
        "sar_rcl_8",
        "sar_rcl_16",
        // A one bit value widened to a byte or to two bytes. The four byte and eight byte forms
        // are what a `_Bool` read turns into, and these two want the one shape `narrow` does not
        // have: a truncation of an extension, where the extension came from something narrower
        // than the truncation goes to, which is what `char c = a < b;` is.
        "bit_to_8",
        "bit_to_16",
    ];

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
        // The second half of a pair at place one, which is the place a rule cannot name. The first
        // half at place zero is `ret_val_*` and is reached by a rule, so it is not on this list.
        let second = |ty| strip(crate::abi::ret_of(ty, 1).expect("every width the pseudos cover"));
        let widths = || {
            [8, 16, 32, 64]
                .into_iter()
                .map(rucc_ir::Type::int)
                .chain([rucc_ir::Float::F32, rucc_ir::Float::F64].map(rucc_ir::Type::float))
        };
        let written: Vec<&str> = widths()
            .map(named)
            .chain(widths().map(second))
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
            if NARROW.contains(&opcode) || BARRIER.contains(&opcode) || X87.contains(&opcode) {
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

    /// The same claim about the barrier as the ones above make about the convention and the frame:
    /// the list holds instructions this target really describes, and holds only the ones that have
    /// no operands, since an instruction with an operand is one a rule could have been written for.
    #[test]
    fn every_instruction_exempt_from_a_rule_is_one_the_memory_model_really_writes() {
        for &opcode in BARRIER {
            let form = x86_64::form(opcode).expect("an instruction this target describes");
            assert!(form.operands().is_empty(), "{opcode} has operands, so a rule could name it");
        }
    }

    /// The staleness rule every list in this project is kept under, on the one list here whose
    /// entries are meant to leave. A rule that starts selecting one of these is `tamnd/rucc#375`
    /// arriving, and the entry goes with it. An entry naming an instruction nothing describes is a
    /// misspelling, and it would sit here exempting nothing.
    #[test]
    fn an_instruction_a_rule_now_selects_is_off_the_list_of_the_ones_left_for_later() {
        let written = heads();
        for &opcode in NARROW {
            let head = format!("{PREFIX}{opcode}");
            assert!(
                !written.contains(&head.as_str()),
                "a rule in {} selects {opcode} now, so it is not waiting on tamnd/rucc#375",
                TABLE.source
            );
            assert!(
                x86_64::INSTS.iter().any(|&(described, _)| described == opcode),
                "{opcode} is not an instruction anything describes"
            );
        }
    }

    /// The same staleness rule on the x87 pair, and one thing more that is particular to them.
    ///
    /// They are a pair. An instruction that pushes onto the x87 stack and nothing that pops off it
    /// again would leave the stack one deeper than the function found it, which is not a mistake
    /// the allocator or the block layout could catch, since neither of them knows the stack is
    /// there. So the two arrive together and leave together, and that is what this says.
    #[test]
    fn the_x87_stack_is_reached_by_a_pair_and_by_nothing_else() {
        let written = heads();
        for &opcode in X87 {
            assert!(
                x86_64::INSTS.iter().any(|&(described, _)| described == opcode),
                "{opcode} is not an instruction anything describes"
            );
            assert!(
                !written.contains(&format!("{PREFIX}{opcode}").as_str()),
                "a rule in {} selects {opcode} now, so the note on tamnd/rucc#540 is stale",
                TABLE.source
            );
        }
        // One way onto the stack per format a value can be read from, one way off it per format a
        // value can be written to, and the control word pair that is neither. The count is here as
        // well as in the target description because this list is what says none of them is
        // reachable, and a name that arrived here without its partner would be a format this
        // target can convert in one direction and not the other.
        assert_eq!(X87.len(), 12, "five pushes, five pops and the control word");
    }
}
