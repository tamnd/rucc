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
        assert_eq!(seen, 14, "the store rules moved and this test did not follow them");
    }

    /// Every comparison can be made against a constant as well as against a register.
    ///
    /// Four comparisons in five in the corpus are against a constant, and without a rule for one
    /// the constant is loaded into a register first, which is an instruction and a register the
    /// machine never needed. A missing width or a missing condition would not fail anything else:
    /// the register rule still matches, the output is still correct, and the only sign is code
    /// that is one instruction longer in a place nobody is looking. So the two lists are counted
    /// against each other here.
    ///
    /// What this cannot check is that the condition on the immediate rule is the right one, since
    /// both halves of a wrong pair would be a consistent pair. That is what the `spec` clause is
    /// for, and `rucc-verify` is what reads it.
    #[test]
    fn a_comparison_against_a_constant_is_written_for_every_one_against_a_register() {
        let mut against_register = Vec::new();
        let mut against_constant = Vec::new();
        for rule in TABLE.rules {
            let Some(rest) = rule.pattern.strip_prefix("(icmp_") else { continue };
            let (condition, operands) = rest.split_once(".i1 ").expect("a comparison takes two");
            let width = operands
                .strip_prefix("(value.")
                .and_then(|rest| rest.split_once(' '))
                .map(|(width, _)| width)
                .expect("a comparison reads a value first");
            let named = format!("{condition}.{width}");
            if operands.contains("(iconst.") {
                // The constant is the second operand and never the first, because a comparison is
                // not symmetric and the same condition on the other side means the opposite.
                assert!(
                    !operands.starts_with("(iconst."),
                    "line {}: {} compares a constant against a value",
                    rule.line,
                    rule.pattern
                );
                against_constant.push(named);
            } else {
                against_register.push(named);
            }
        }
        against_register.sort_unstable();
        against_constant.sort_unstable();
        assert_eq!(against_register, against_constant);
        assert_eq!(against_register.len(), 40, "ten conditions at four widths");
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
    /// there is a second jump after it, so all of these are written where the answer is.
    ///
    /// The comparisons are here for a second reason on top of that one. A branch on a comparison
    /// is a comparison and a jump on the flags it set, and the flags are not a value: no pattern
    /// could bind one and no `spec` clause could say anything about one. So the pair is put
    /// together by the layout, out of a comparison a rule did select and the branch behind it,
    /// which is the same argument `rucc_target::x86_64::Form::CmpSet` is one form rather than two
    /// under.
    const LAYOUT: &[&str] = &[
        "test_rr_8",
        "cmp_rr_8",
        "cmp_rr_16",
        "cmp_rr_32",
        "cmp_rr_64",
        "cmp_ri_8",
        "cmp_ri_16",
        "cmp_ri_32",
        "cmp_ri_64",
        "jcc_e",
        "jcc_ne",
        "jcc_l",
        "jcc_le",
        "jcc_g",
        "jcc_ge",
        "jcc_b",
        "jcc_be",
        "jcc_a",
        "jcc_ae",
        "jmp",
    ];

    /// The instruction the memory model writes rather than a rule.
    ///
    /// A barrier computes nothing, so there is no equality for the solver to discharge and no
    /// pattern for a rule to be written as. What makes it the right answer is what the machine
    /// promises about the order two other instructions become visible in, which is a claim about
    /// the program around it rather than about any value. `crate::lower` writes it by name, at the
    /// strongest ordering and nowhere else, and `crate::expand` says why the strongest is the only
    /// one that costs anything here.
    const BARRIER: &[&str] = &["mfence"];

    /// The instructions that produce two values, which is one more than a rule can name.
    ///
    /// A rule replaces a term with a term, and a term is the value one instruction computes. A
    /// compare and exchange computes two: what it found at the address, and whether what it found
    /// was what the program expected. There is no way to write the second one down in the rule
    /// language, and inventing one would be inventing a language for a single instruction.
    ///
    /// So `crate::lower` writes it by name, the way it writes the barrier by name, and for a reason
    /// that is about the rule language rather than about the machine. What the solver would have
    /// been asked to prove about it is the easy half in any case: the arithmetic is a comparison
    /// and a select, and what is hard is that the whole of it happens at once, which is the same
    /// claim about the program around it that a barrier makes.
    const ATOMIC: &[&str] = &["cmpxchg_8", "cmpxchg_16", "cmpxchg_32", "cmpxchg_64"];

    /// The instructions whose operation is in the payload rather than in the head.
    ///
    /// A different exemption from the one above, on instructions that produce one value each and so
    /// could be named by a rule if the rule had anything to match on. The head a pattern matches is
    /// an opcode and a type, and every read modify write in the IR is the one opcode `atomic_rmw`.
    /// Which of the thirteen operations it performs is carried beside the instruction rather than in
    /// its name, so a pattern written for the exchange would match the add and the nand as well, and
    /// the rule language has no way to look at what a rule matched to tell them apart.
    ///
    /// Giving each operation its own opcode is the other way out and is a worse trade: it is
    /// thirteen opcodes at four widths where the IR wants one, and every pass that treats a read
    /// modify write as one thing would then have a list of fifty two.
    ///
    /// So `crate::lower` writes these by name too. Three operations here, out of the thirteen: the
    /// bitwise ones need a loop around a compare and exchange, which is control flow and so is built
    /// before selection rather than during it, and they are the rest of `tamnd/rucc#311`.
    const PAYLOAD: &[&str] =
        &["xchg_8", "xchg_16", "xchg_32", "xchg_64", "xadd_8", "xadd_16", "xadd_32", "xadd_64"];

    /// The instructions a frame writes rather than a rule.
    ///
    /// A prologue, an epilogue, a copy, a spill and a reload are not in the program. They are what
    /// the allocator's answer costs, so they are written after it, by `crate::finish` reading
    /// `x86_64::FRAME`. Six of the names that describes are already reachable from a rule, since a
    /// prologue taking its frame is a subtraction and a spill is a store, and those are not here:
    /// this is only the ones nothing else can reach.
    const FRAME: &[&str] =
        &["push_64", "pop_64", "ret", "mov_rr_64", "movaps_rr", "movaps_rm", "movaps_mr"];

    /// The instructions that reach the x87 stack, which are selected but not from here.
    ///
    /// A third kind of exemption, and the same reason all the way down the list.
    ///
    /// Every one of these is written by `crate::lower`, as part of a group rather than on its own.
    /// What one of them leaves behind and the next picks up is the top of the x87 stack, which is
    /// not a register anything allocates from and not a value a pattern could bind, so a rule
    /// could neither match the middle of a group nor name what its replacement produced. And an
    /// add here reads two addresses and writes a third, where one machine IR instruction carries
    /// one addressing mode, so the group cannot be folded into a single opcode the way
    /// `ucomisd_set_e` folds a comparison and a `setcc` either.
    ///
    /// So these are exempt for the reason `FRAME` is exempt rather than for the reason the list
    /// below is, and they will stay exempt. Two of them are not reached by anything yet all the
    /// same: `fsub_p` and `fdiv_p` are the other direction of the subtraction and the division,
    /// which a code generator that pushed its operands the other way round would need and this one
    /// does not. `fabs` is a third, since C spells that as a call to a library function.
    const X87: &[&str] = &[
        "fld_t",
        "fstp_t",
        "fld_s",
        "fld_l",
        "fild_l",
        "fild_ll",
        "fstp_s",
        "fstp_l",
        "fistp_l",
        "fistp_ll",
        "fnstcw",
        "fldcw",
        "fadd_p",
        "fsub_p",
        "fsubr_p",
        "fmul_p",
        "fdiv_p",
        "fdivr_p",
        "fchs",
        "fabs",
        "fucomip_set_a",
        "fucomip_set_ae",
        "fucomip_set_b",
        "fucomip_set_be",
        "fucomip_set_e",
        "fucomip_set_ne",
        "fucomip_set_p",
        "fucomip_set_np",
        "fucomip_set_e_and_np",
        "fucomip_set_ne_or_p",
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

    /// The same claim about the block layout's list, which is longer than it looks.
    ///
    /// A name here that the layout does not write is an opcode exempted from needing a rule and
    /// reached by nothing, and a name the layout writes that is not here is a failing test in
    /// `every_described_instruction_is_reachable_from_a_rule` with a misleading message. Both are
    /// avoided by taking the list from `rucc_target::x86_64::BRANCH` rather than believing it.
    #[test]
    fn every_instruction_exempt_from_a_rule_is_one_the_block_layout_really_writes() {
        let branch = &x86_64::BRANCH;
        // Eighty entries name sixteen instructions between them, so this is a set rather than a
        // list and both sides are sorted before they are held against each other. What the order
        // of the list itself is for is reading it.
        let mut written: Vec<&str> = vec![branch.test, branch.jump];
        written.extend(branch.fused.iter().map(|fusion| fusion.cmp));
        written.extend(branch.fused.iter().flat_map(|fusion| [fusion.if_true, fusion.if_false]));
        written.sort_unstable();
        written.dedup();
        let mut exempt = LAYOUT.to_vec();
        exempt.sort_unstable();
        assert_eq!(written, exempt);
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
            if ATOMIC.contains(&opcode) || PAYLOAD.contains(&opcode) {
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

    /// The same claim about the atomic list, read off the thing that put the entry there: an
    /// instruction is exempt for this reason exactly when it writes more than one value, and an
    /// instruction that writes one is one a rule could have been written for.
    #[test]
    fn every_instruction_exempt_from_a_rule_is_one_that_writes_more_than_one_value() {
        let written = heads();
        for &opcode in ATOMIC {
            let form = x86_64::form(opcode).expect("an instruction this target describes");
            let writes = form.operands().iter().filter(|desc| desc.role.is_def()).count();
            assert!(writes > 1, "{opcode} writes one value, so a rule could name it");
            assert!(
                !written.contains(&format!("{PREFIX}{opcode}").as_str()),
                "a rule in {} selects {opcode}, which `crate::lower` also writes by hand",
                TABLE.source
            );
        }
    }

    /// The same claim about the payload list, read off the thing that puts an entry there.
    ///
    /// Two halves. Each of these writes one value, which is what says the reason above is not the
    /// reason here, so a list that grew to cover an instruction the atomic list should have had
    /// fails. And there really is more than one operation behind the one IR opcode, which is the
    /// whole of why a pattern cannot name any of them, and is a fact about the IR that would stop
    /// being true if the operations were ever given opcodes of their own.
    #[test]
    fn every_instruction_exempt_because_its_operation_is_beside_it_writes_one_value() {
        assert!(
            rucc_ir::RmwOp::all().count() > 1,
            "one operation per opcode would be a head a rule could match"
        );
        let written = heads();
        for &opcode in PAYLOAD {
            let form = x86_64::form(opcode).expect("an instruction this target describes");
            let writes = form.operands().iter().filter(|desc| desc.role.is_def()).count();
            assert_eq!(writes, 1, "{opcode} writes more than one value, so it is the other list's");
            assert!(
                !written.contains(&format!("{PREFIX}{opcode}").as_str()),
                "a rule in {} selects {opcode}, which `crate::lower` also writes by hand",
                TABLE.source
            );
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
                "a rule in {} selects {opcode}, which `crate::lower` also writes by hand",
                TABLE.source
            );
        }
        // One way onto the stack per format a value can be read from, one way off it per format a
        // value can be written to, the control word pair that is neither, and the arithmetic. The
        // count is here as well as in the target description because this list is what says none
        // of them is reachable, and a name that arrived here without its partner would be a format
        // this target can convert in one direction and not the other.
        assert_eq!(X87.len(), 30, "twelve that move a value and eighteen that work on one");
    }
}
