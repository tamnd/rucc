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
        assert_eq!(seen, 16, "the store rules moved and this test did not follow them");
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
        "arg_val_f16",
        "arg_val_f32",
        "arg_val_f64",
        "arg_val_f128",
        "ret_val2_8",
        "ret_val2_16",
        "ret_val2_32",
        "ret_val2_64",
        "ret_val2_f16",
        "ret_val2_f32",
        "ret_val2_f64",
        "ret_val2_f128",
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
    /// The instructions the size directed peephole writes rather than a rule.
    ///
    /// [`crate::shorten`] turns a comparison of a register against zero into a test of the register
    /// against itself, which asks the machine the same thing in one byte less, and an addition of
    /// one into the instruction that adds one and says so in its opcode, which is another byte less.
    /// No rule could select either. Whether the first says the same thing depends on the constant
    /// the comparison carries and a pattern binds a value rather than reads a number out of one, and
    /// whether the second does depends on what reads the carry behind it, which is not something a
    /// pattern sees at all. The eight bit test is not here because the layout writes that one as
    /// well and it is on the list below.
    const PEEPHOLE: &[&str] = &[
        "test_rr_16",
        "test_rr_32",
        "test_rr_64",
        "inc_r_8",
        "inc_r_16",
        "inc_r_32",
        "inc_r_64",
        "dec_r_8",
        "dec_r_16",
        "dec_r_32",
        "dec_r_64",
    ];

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
        "cmp_rm_8",
        "cmp_rm_16",
        "cmp_rm_32",
        "cmp_rm_64",
        "cmp_mi_8",
        "cmp_mi_16",
        "cmp_mi_32",
        "cmp_mi_64",
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

    /// The instructions the compare pass writes rather than a rule.
    ///
    /// The other half of the argument the comparisons above are here under. A rule selects a
    /// comparison that keeps its answer in a byte, because that is the shape a value has. What is
    /// left of one when the machine has already made the comparison is the byte with no comparison
    /// in front of it, and there is no pattern for that: the term it would compute is the same term
    /// the full comparison computes, and what makes the short one right is the instruction three
    /// places back rather than anything about the value. So `crate::compare` writes them by name,
    /// in place of a comparison it found was already made.
    const COMPARE: &[&str] = &[
        "set_e", "set_ne", "set_l", "set_le", "set_g", "set_ge", "set_b", "set_be", "set_a",
        "set_ae",
    ];

    /// The instruction a computed `goto` is written as rather than a rule.
    ///
    /// The one branch `crate::lower` writes by name, and the one the block layout does not write
    /// either. What it reads is the address, which a pattern could have bound, so it is not
    /// exempt for the reason the branches above are. What no pattern can say is the rest of it:
    /// how many arms the block has, which is every label of the function the program took the
    /// address of, and a rule says what an instruction reads rather than where a block goes.
    const LABELS: &[&str] = &["jmp_reg"];

    /// The instruction the memory model writes rather than a rule.
    ///
    /// A barrier computes nothing, so there is no equality for the solver to discharge and no
    /// pattern for a rule to be written as. What makes it the right answer is what the machine
    /// promises about the order two other instructions become visible in, which is a claim about
    /// the program around it rather than about any value. `crate::lower` writes it by name, at the
    /// strongest ordering and nowhere else, and `crate::expand` says why the strongest is the only
    /// one that costs anything here.
    const BARRIER: &[&str] = &["mfence"];

    /// The instruction a program stops on, which `crate::lower` writes rather than a rule.
    ///
    /// The first half of the barrier's reason and not the second. It computes nothing, so there is
    /// no equality for the solver and no pattern for a rule. What makes it right is not a claim
    /// about the order anything becomes visible in either: it is what the operating system does
    /// with the fault, which is a fact about neither the values nor the program around it.
    const STOP: &[&str] = &["ud2"];

    /// The instructions that are a hint rather than a computation.
    ///
    /// The same shape of exemption the barrier gets and for a reason one step further out. A
    /// barrier computes nothing and still has to be where it is, so there is at least a claim about
    /// the program around it. A prefetch does not even have that: a machine that drops the whole
    /// instruction runs the program correctly, because the only thing it can change is how long the
    /// program takes.
    ///
    /// So there is no equality for the solver and no pattern for a rule, and which of the four a
    /// program gets is decided by a number in the builtin's own arguments rather than by anything
    /// about the value being prefetched. `crate::lower` writes them by name, out of the hint the IR
    /// carries beside the instruction.
    const HINT: &[&str] = &["prefetch_nta", "prefetch_t0", "prefetch_t1", "prefetch_t2"];

    /// The instructions nothing but an `asm` statement asks for.
    ///
    /// One step further out again. A prefetch is a hint and is still something the compiler decides
    /// to write, out of a builtin the program called. These are instructions the program wrote down
    /// itself, by name, in a template, and nothing else in the language reaches them: there is no
    /// builtin for either, no rule could match a term that produces one, and `crate::lower` writes
    /// them only because [`rucc_target::x86_64::read`] found the name in a template and said which
    /// opcode that is.
    ///
    /// `pause` is the hint a spin lock writes between two tries at the lock. `cpuid` is how a
    /// program asks the processor what it can do, which there is no other way to ask, so every
    /// program that takes a faster path on some machines than on others has one of these in it.
    ///
    /// The alignment is the third, and it is on this list rather than one of its own because it
    /// meets the claim below outright: an instruction is exempt for this reason exactly when there
    /// is nothing about it for a rule to name, and an opcode with no operands and no addressing mode
    /// has nothing. It is not an instruction at all, which is more than the test asks and is the
    /// reason no rule could have been written for it however the rule language grew.
    ///
    /// A byte out of a template is the fourth and is there for the same reason as the alignment,
    /// one step further still: it is not an instruction, and what it holds is a byte the program
    /// wrote out itself because its assembler was older than the instruction it wanted. There is
    /// nothing for a rule to have said about a number a program handed the processor directly.
    const TEMPLATE: &[&str] = &["cpuid", "pause", "align", "byte"];

    /// The rotates and the test against a constant, which a template writes and nothing else does.
    ///
    /// A rotate is a term the IR could have, and does not yet: C spells one as two shifts and an or,
    /// and nothing puts those back together. A test against a constant is an and whose answer is
    /// thrown away, and the layout writes a comparison for that rather than this. A store of a
    /// constant goes through a register when the compiler writes it. So what reaches one of these
    /// is a program that wrote the name, which is what tcc's byte swap, its copy of `memcpy` and
    /// its test of `"m"` operands do.
    const TEMPLATED: &[&str] = &[
        "rol_ri_8",
        "rol_ri_16",
        "rol_ri_32",
        "rol_ri_64",
        "rol_rcl_8",
        "rol_rcl_16",
        "rol_rcl_32",
        "rol_rcl_64",
        "ror_ri_8",
        "ror_ri_16",
        "ror_ri_32",
        "ror_ri_64",
        "ror_rcl_8",
        "ror_rcl_16",
        "ror_rcl_32",
        "ror_rcl_64",
        "test_ri_8",
        "test_ri_16",
        "test_ri_32",
        "test_ri_64",
        "mov_mi_8",
        "mov_mi_16",
        "mov_mi_32",
        "mov_mi_64",
    ];

    /// The instructions a template asks for that are right because of the line above them.
    ///
    /// These are exempt for the reason the ten bytes in [`COMPARE`] are, one step further out. A
    /// rule selects a conditional move with its comparison in front of it, because that pair is the
    /// shape a select has. The move on its own computes the same term and what makes it right is the
    /// comparison somewhere behind it rather than anything about its own operands, so no pattern
    /// could say what it means. The compare pass does not write one either, because it replaces a
    /// comparison it found was already made and there is no earlier move here to replace: what
    /// writes one is a program that put the comparison on one line of a template and the move on the
    /// next, which is what zstd does to keep a bounds check from becoming a branch.
    ///
    /// So these have operands a rule could have named, unlike everything in [`TEMPLATE`], and they
    /// are still not instructions a rule could have been written for.
    ///
    /// The jumps on the sign, the overflow and the parity are here for the same reason. The layout
    /// writes the other ten behind a comparison it chose, and nothing chooses one of these: a C
    /// condition never asks about one bit on its own, so the only line above one is a line in a
    /// template, which is what a loop in tcc's tests that counts down with `dec` and stops on `js`
    /// is.
    /// The add with carry and the subtract with borrow, which read a bit off the instruction in
    /// front of them.
    ///
    /// Exempt one step further out again than [`CONDITIONAL`]. A conditional move reads the
    /// condition state and leaves it alone, so what is missing from a rule that named one is the
    /// comparison. These read it and write it both, and what is missing is worse than a comparison:
    /// the bit they read is the carry out of an addition, and an addition in the IR is an addition
    /// of a width with no carry out at all, so there is no term a rule could match that the bit is
    /// a part of. A program gets one by writing both halves itself in a template, which is what
    /// `add_ssaaaa` and `sub_ddmmss` in libgmp's `longlong.h` are. The form against a constant is
    /// here for the same reason and is the same instruction with a zero where the second source is,
    /// which `add_sssaaaa` writes for the top word of a number three words wide.
    ///
    /// What keeps the two halves together once they are two instructions in a block is not here. It
    /// is `rucc_target::FlagInsts`, which the scheduler reads for exactly this, and the test below
    /// checks the entry is there rather than trusting that somebody remembered.
    const CARRY: &[&str] = &[
        "adc_rr_8",
        "adc_rr_16",
        "adc_rr_32",
        "adc_rr_64",
        "sbb_rr_8",
        "sbb_rr_16",
        "sbb_rr_32",
        "sbb_rr_64",
        "adc_ri_8",
        "adc_ri_16",
        "adc_ri_32",
        "adc_ri_64",
        "sbb_ri_8",
        "sbb_ri_16",
        "sbb_ri_32",
        "sbb_ri_64",
    ];

    const CONDITIONAL: &[&str] = &[
        "cmov_e_16",
        "cmov_e_32",
        "cmov_e_64",
        "cmov_ne_16",
        "cmov_ne_32",
        "cmov_ne_64",
        "cmov_l_16",
        "cmov_l_32",
        "cmov_l_64",
        "cmov_le_16",
        "cmov_le_32",
        "cmov_le_64",
        "cmov_g_16",
        "cmov_g_32",
        "cmov_g_64",
        "cmov_ge_16",
        "cmov_ge_32",
        "cmov_ge_64",
        "cmov_b_16",
        "cmov_b_32",
        "cmov_b_64",
        "cmov_be_16",
        "cmov_be_32",
        "cmov_be_64",
        "cmov_a_16",
        "cmov_a_32",
        "cmov_a_64",
        "cmov_ae_16",
        "cmov_ae_32",
        "cmov_ae_64",
        "jcc_s",
        "jcc_ns",
        "jcc_o",
        "jcc_no",
        "jcc_p",
        "jcc_np",
    ];

    /// The instructions that look for a set bit, which a template asks for and nothing else does.
    ///
    /// These have a source and a destination a rule could have named, the way the conditional moves
    /// above do, and the reason no rule names them is a different one again. It is not that their
    /// meaning comes from the line in front of them: each of these says on its own exactly what it
    /// computes. It is that [`crate::expand`] already answers the question they answer, out of
    /// arithmetic every machine has, and it does that because what these do when the source is zero
    /// is four different things on four families of processor. A rule that selected one would be a
    /// rule whose answer depends on which machine ran it.
    ///
    /// So the only thing that reaches one is a program that wrote the name in a template, which is
    /// what the libraries that were counting bits before there was a builtin for it all do.
    /// `crate::lower` writes them for the reason it writes the three in [`TEMPLATE`], and they are
    /// not on that list because they are not bare: a rule could have named these operands and the
    /// claim that list makes would be false of them.
    const SEARCH: &[&str] = &[
        "bsf_16", "bsf_32", "bsf_64", "bsr_16", "bsr_32", "bsr_64", "lzcnt_32", "lzcnt_64",
        "tzcnt_32", "tzcnt_64",
    ];

    /// The instruction that turns a register round, which a template asks for and nothing else does.
    ///
    /// The list above, one step simpler. A search is unselected because what it does with a source
    /// of zero is not the same on every processor, so a rule that chose one would depend on what ran
    /// it. A byte reversal has no such case: it means exactly one thing everywhere. What keeps it
    /// off the rule set is a choice made once, in [`crate::expand`], which builds a reversal out of
    /// shifts and masks so that the answer is the same on every target this compiler has rather than
    /// good on the one that happens to have the instruction. tamnd/rucc#310 is where that trade is
    /// written down, and the day a target grows its own reversal is the day to reopen it.
    ///
    /// So the only thing that reaches one is a program that wrote the name in a template, which is
    /// what libgmp does in `gmp-impl.h` to put a limb the other way round.
    ///
    /// The third is the same thing at a width `bswap` does not reach. Turning a sixteen bit number
    /// round is exchanging its two bytes with each other, and this machine says that by naming the
    /// high byte of a register, which only the first four registers have. femtolisp writes one in
    /// `llt/utils.h`, which is how a C library older than `__builtin_bswap16` said it, and that
    /// header is the one every other file of the library includes.
    const SWAP: &[&str] = &["bswap_32", "bswap_64", "xchg_high_16"];

    /// The jump out of the function a template may end with, which a template asks for and nothing
    /// else could.
    ///
    /// Unselected for a reason none of the lists above give, and the plainest reason of the lot:
    /// there is no term in the IR for it to be the answer to. A tail jump is not a computation and
    /// it is not a branch between this function's blocks either, it is the function ending
    /// somewhere other than at its own `ret`, and the only thing that says a function ends that way
    /// is a program writing `jmp` at the end of a template in a function that is `naked`. See
    /// [`rucc_target::x86_64::Step::Away`].
    const AWAY: &[&str] = &["jmp_away"];

    /// The instructions that change an object where it lives, which a template asks for and
    /// nothing else does.
    ///
    /// Each of these is a load, one operation and a store in one line. The rules select the three
    /// on their own and never the one that is all of them, because what a rule sees is a value in a
    /// register and the store is a separate term further on. What asks for one is a program that
    /// gave an `asm` operand the constraint `m` and then named it in an instruction, which is how a
    /// C library sets a bit in a `sigset_t` and how tcc's `tests/tcctest.c` counts a static local up.
    const MEMORY: &[&str] = &[
        "neg_m_8",
        "neg_m_16",
        "neg_m_32",
        "neg_m_64",
        "not_m_8",
        "not_m_16",
        "not_m_32",
        "not_m_64",
        "inc_m_8",
        "inc_m_16",
        "inc_m_32",
        "inc_m_64",
        "dec_m_8",
        "dec_m_16",
        "dec_m_32",
        "dec_m_64",
        "bts_mr_16",
        "bts_mr_32",
        "bts_mr_64",
        "btr_mr_16",
        "btr_mr_32",
        "btr_mr_64",
        "btc_mr_16",
        "btc_mr_32",
        "btc_mr_64",
        "bts_mi_16",
        "bts_mi_32",
        "bts_mi_64",
        "btr_mi_16",
        "btr_mi_32",
        "btr_mi_64",
        "btc_mi_16",
        "btc_mi_32",
        "btc_mi_64",
    ];

    /// The multiply that keeps both halves of its product and the division that reads both halves
    /// of its dividend, which a template asks for and nothing else does.
    ///
    /// A third reason again, and the plainest of the three. A search is unselected because its
    /// answer depends on the processor and a reversal because a choice was made to build one out of
    /// arithmetic. This one is unselected because there is nothing in the IR to select it from: a
    /// multiply in C takes two values of a type and produces a value of that type, so the term a
    /// rule would match on is the narrow product, and the wide product is not a term at all. A rule
    /// that fired on the narrow one and wrote this would be writing an instruction that computes
    /// twice as much as was asked for and leaves the rest in a register nobody asked about.
    ///
    /// So the only thing that reaches one is a program that wrote the name in a template, which is
    /// what `umul_ppmm` in libgmp's `longlong.h` does, and what every library that is building
    /// arithmetic out of limbs does somewhere.
    ///
    /// The division is the same claim upside down and is on this list because the reason is the same
    /// one. A division in C divides a number by a number of its own width, so the term a rule would
    /// match is the narrow one, and this compiler already has two opcodes for that: each of them
    /// fills the high half of the dividend itself and then throws one of the two answers away. A
    /// dividend the program filled both halves of is not a term the IR has, and `udiv_qrnnd` beside
    /// the multiply in the same header is how long division a limb at a time is written.
    const WIDE: &[&str] = &[
        "mul_wide_16",
        "mul_wide_32",
        "mul_wide_64",
        "imul_wide_16",
        "imul_wide_32",
        "imul_wide_64",
        "div_wide_16",
        "div_wide_32",
        "div_wide_64",
        "idiv_wide_16",
        "idiv_wide_32",
        "idiv_wide_64",
    ];

    /// The string instructions, which a template writes and nothing else does.
    ///
    /// Exempt for the reason `cpuid` is in [`TEMPLATE`]: every register one of them reaches is one
    /// the instruction names for itself, so there is nothing about one for a rule to name. A copy
    /// or a fill the compiler writes is a loop it can schedule or a call to the library, and never
    /// one of these.
    const STRING: &[&str] = &[
        "movs_8",
        "movs_16",
        "movs_32",
        "movs_64",
        "rep_movs_8",
        "rep_movs_16",
        "rep_movs_32",
        "rep_movs_64",
        "stos_8",
        "stos_16",
        "stos_32",
        "stos_64",
        "rep_stos_8",
        "rep_stos_16",
        "rep_stos_32",
        "rep_stos_64",
        "lods_8",
        "lods_16",
        "lods_32",
        "lods_64",
        "scas_8",
        "scas_16",
        "scas_32",
        "scas_64",
        "repe_scas_8",
        "repe_scas_16",
        "repe_scas_32",
        "repe_scas_64",
        "repne_scas_8",
        "repne_scas_16",
        "repne_scas_32",
        "repne_scas_64",
        "cmps_8",
        "cmps_16",
        "cmps_32",
        "cmps_64",
        "repe_cmps_8",
        "repe_cmps_16",
        "repe_cmps_32",
        "repe_cmps_64",
        "repne_cmps_8",
        "repne_cmps_16",
        "repne_cmps_32",
        "repne_cmps_64",
    ];

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
    const FRAME: &[&str] = &[
        "push_64",
        "pop_64",
        "ret",
        "mov_rr_64",
        "movaps_rr",
        // The touch a probing prologue puts on each page as it reaches it, the landing pad a
        // prologue opens with, and the byte that does nothing which one reserves room with. All
        // three are written by a frame and none on a command line that did not ask for it.
        "or_mi_8",
        "endbr64",
        "nop",
    ];

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
        // The signed divides, which are two instructions per width because the quotient and the
        // remainder come out of one division in two different registers. `narrow` writes the
        // unsigned ones for a division of zero extensions and refuses these on purpose: the most
        // negative byte over minus one is a defined hundred and twenty eight at four bytes and is
        // the overflow that raises at one, so narrowing a signed division wants a range that rules
        // the pair out and there is no range analysis yet.
        "idiv_quo_8",
        "idiv_quo_16",
        "idiv_rem_8",
        "idiv_rem_16",
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

    /// The arithmetic that reaches memory, which [`crate::combine`] writes: the forms that read a
    /// source out of it and the forms that leave the answer in it.
    ///
    /// A function rather than a list, for the reason the compare pass's exemption is taken from the
    /// flag description rather than typed out: the pass already writes down which instructions it
    /// can produce, and a second copy of that here would be a second opinion about one pass.
    ///
    /// No rule selects one of these because a rule matches a term and one of these is two terms, a
    /// load and an arithmetic operation, put together, or three where the answer goes back to
    /// memory. Whether they may be put together depends on what is written between them and on
    /// whether anything else wants what the load read, and neither is a fact about any of the
    /// terms. That is the whole reason the pass exists and the module documentation there says it
    /// at length.
    fn combine() -> Vec<&'static str> {
        let loads = crate::combine::FOLDS.iter().map(|fold| fold.into);
        // And the instruction a load on the other side comes to, which for most rows is the one
        // above and for a comparison is the condition the other way round.
        let swapped = crate::combine::FOLDS.iter().filter_map(|fold| fold.swapped);
        let stores = crate::combine::UPDATES.iter().map(|update| update.into);
        let constants = crate::combine::BUMPS.iter().map(|bump| bump.into);
        loads.chain(swapped).chain(stores).chain(constants).collect()
    }

    #[test]
    fn every_instruction_exempt_from_a_rule_is_one_a_frame_really_writes() {
        // The same claim as the one about the convention, so that this list cannot grow an opcode
        // that no frame asks for. In the order `x86_64::FRAME` names them, the copies after the
        // return because there is one set of them per class the allocator may spill.
        let frame = &x86_64::FRAME;
        let mut written = vec![frame.push, frame.pop, frame.ret];
        for class in frame.classes {
            written.extend([class.mov, class.load, class.store]);
        }
        // And the touch a probing prologue puts on a page, which the target names as an option
        // because a target with no instruction that writes an address without changing it takes
        // every frame in one subtraction and has nothing to exempt.
        written.extend(frame.probe.map(|probe| probe.inst));
        // And the landing pad and the byte that does nothing, which are options for the same
        // reason.
        written.extend(frame.landing);
        written.extend(frame.pad);
        // What is left after the ones a rule already reaches, which are the loads and the stores of
        // both register files, since those are the same instructions a program's own reads and
        // writes of memory are. The vector pair joined them with the rules for a quad float, and a
        // spill of one is now the same instruction as a program reading a `_Float128` variable.
        written.retain(|opcode| !heads().contains(&format!("{PREFIX}{opcode}").as_str()));
        assert_eq!(written, FRAME);
    }

    #[test]
    fn every_instruction_exempt_from_a_rule_is_one_the_convention_really_writes() {
        // An exemption list that nothing checks is a hole, since an opcode dropped into it stops
        // being covered by either direction of the pinning. These are the ones `crate::abi` can
        // name, at the four integer widths and the four float formats it has names for an
        // argument in, and no others.
        let strip = |head: &'static str| head.strip_prefix(PREFIX).expect("an x86-64 term");
        let named = |ty| strip(crate::abi::head_of(ty).expect("every width the pseudos cover"));
        // The second half of a pair at place one, which is the place a rule cannot name. The first
        // half at place zero is `ret_val_*` and is reached by a rule, so it is not on this list.
        let second = |ty| strip(crate::abi::ret_of(ty, 1).expect("every width the pseudos cover"));
        let widths = || {
            [8, 16, 32, 64].into_iter().map(rucc_ir::Type::int).chain(
                [
                    rucc_ir::Float::F16,
                    rucc_ir::Float::F32,
                    rucc_ir::Float::F64,
                    rucc_ir::Float::F128,
                ]
                .map(rucc_ir::Type::float),
            )
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

    /// The same claim about the compare pass. What it writes is what the flag description says is
    /// left of a comparison, so the exemption is taken from that rather than typed out twice, and
    /// an entry added there without a rule to go with it shows up here rather than in a build that
    /// fails somewhere else.
    #[test]
    fn every_instruction_exempt_from_a_rule_is_one_the_compare_pass_really_writes() {
        let mut written: Vec<&str> =
            x86_64::FLAGS.compares.iter().filter_map(|entry| entry.kept).collect();
        written.sort_unstable();
        written.dedup();
        let mut exempt = COMPARE.to_vec();
        exempt.sort_unstable();
        assert_eq!(written, exempt);
    }

    /// And the same claim about the one the lowering writes, held against the name the target gave
    /// it rather than against the spelling written above.
    #[test]
    fn the_instruction_a_computed_goto_is_exempt_for_is_the_one_the_target_names() {
        assert_eq!(LABELS, [x86_64::BRANCH.indirect]);
    }

    /// The rows of the constant table that take nothing yet are exactly the narrow ones waiting on
    /// the width narrowing, so the day `NARROW` shrinks is the day this says so.
    ///
    /// `crate::combine::BUMPS` has a row per instruction this machine has, which is the whole five
    /// operations at the whole four widths. Four of those instructions arrive out of a rule that is
    /// not written yet, so four of the rows sit there taking nothing. That is a fact worth holding
    /// rather than a thing to notice again later.
    #[test]
    fn the_constant_runs_that_take_nothing_are_the_ones_no_rule_selects_yet() {
        let written = heads();
        let mut waiting = Vec::new();
        for bump in crate::combine::BUMPS {
            if !written.contains(&format!("{PREFIX}{}", bump.from).as_str()) {
                waiting.push(bump.from);
            }
        }
        assert_eq!(waiting, ["or_ri_8", "or_ri_16", "xor_ri_8", "xor_ri_16"]);
        for from in waiting {
            assert!(NARROW.contains(&from), "{from} is unselected and is not on the list");
        }
    }

    #[test]
    fn every_described_instruction_is_reachable_from_a_rule() {
        let written = heads();
        let combine = combine();
        for &(opcode, _) in x86_64::INSTS {
            if combine.contains(&opcode) {
                continue;
            }
            if CONVENTION.contains(&opcode) || LAYOUT.contains(&opcode) || FRAME.contains(&opcode) {
                continue;
            }
            if PEEPHOLE.contains(&opcode) {
                continue;
            }
            if NARROW.contains(&opcode) || BARRIER.contains(&opcode) || X87.contains(&opcode) {
                continue;
            }
            if ATOMIC.contains(&opcode) || PAYLOAD.contains(&opcode) || HINT.contains(&opcode) {
                continue;
            }
            if CONDITIONAL.contains(&opcode) {
                continue;
            }
            if CARRY.contains(&opcode) {
                continue;
            }
            if COMPARE.contains(&opcode) || TEMPLATE.contains(&opcode) {
                continue;
            }
            if SEARCH.contains(&opcode) || SWAP.contains(&opcode) || WIDE.contains(&opcode) {
                continue;
            }
            if AWAY.contains(&opcode) || MEMORY.contains(&opcode) || STRING.contains(&opcode) {
                continue;
            }
            if TEMPLATED.contains(&opcode) {
                continue;
            }
            if LABELS.contains(&opcode) || STOP.contains(&opcode) {
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

    /// The same claim about the peephole's list, which is a claim about the target's description
    /// rather than about this crate: every name on it is one the target really has, and every one
    /// of them is a shorter spelling the description names, which is what says the peephole is
    /// where it comes from. A name on the list that the peephole could never write would be an
    /// instruction nothing writes at all, and this test is what stops that sitting there unnoticed.
    #[test]
    fn every_instruction_exempt_from_a_rule_is_one_the_peephole_really_writes() {
        let tests = x86_64::SHORT.testing.iter().map(|entry| entry.into);
        let steps = x86_64::SHORT.stepping.iter().map(|entry| entry.into);
        let shorter: Vec<&str> = tests.chain(steps).collect();
        for &opcode in PEEPHOLE {
            assert!(
                x86_64::form(opcode).is_some(),
                "{opcode} is not an instruction this describes"
            );
            assert!(shorter.contains(&opcode), "{opcode} is not one the peephole writes");
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

    /// The same claim about the instruction a program stops on, which is the barrier's shape
    /// exactly: no operands, because an instruction with one is an instruction a rule could have
    /// been written for, and no addressing mode either, because it is given nothing at all.
    #[test]
    fn the_instruction_exempt_from_a_rule_because_it_stops_the_program_is_bare() {
        for &opcode in STOP {
            let form = x86_64::form(opcode).expect("an instruction this target describes");
            assert!(form.operands().is_empty(), "{opcode} has operands, so a rule could name it");
            assert!(!form.takes_mem(), "{opcode} is given an address and stopping needs none");
        }
    }

    /// The same claim about the hints, with the one difference between them written down. A hint is
    /// given an address and nothing else, so it has no operands for the reason a barrier has none
    /// and it does carry an addressing mode, which is what a rule would have had to match on.
    #[test]
    fn every_instruction_exempt_from_a_rule_because_it_is_a_hint_is_given_only_an_address() {
        for &opcode in HINT {
            let form = x86_64::form(opcode).expect("an instruction this target describes");
            assert!(form.operands().is_empty(), "{opcode} has operands, so a rule could name it");
            assert!(form.takes_mem(), "{opcode} is a hint about an address and is given none");
        }
    }

    /// The same claim about the template list. An instruction is exempt for this reason exactly
    /// when there is nothing about it for a rule to name, and there are two ways to have nothing.
    /// No operands and no address, which is the hint. Or every operand fixed to one register by the
    /// description, which is the question put to the processor: a rule names the operands of a term
    /// and binds them to the values underneath it, and an operand that can be nothing but `rax` is
    /// not a place a value goes. Either way the whole of the claim holds, which is that there was
    /// nowhere else for the instruction to come from.
    #[test]
    fn every_instruction_exempt_from_a_rule_because_only_a_template_asks_for_it_is_bare() {
        for &opcode in TEMPLATE {
            let form = x86_64::form(opcode).expect("an instruction this target describes");
            let fixed = form
                .operands()
                .iter()
                .all(|desc| matches!(desc.constraint, rucc_target::Constraint::Fixed(_)));
            assert!(fixed, "{opcode} has an operand a rule could name");
            assert!(!form.takes_mem(), "{opcode} is given an address, so a rule could name it");
        }
    }

    /// The same claim about the bit searches, read off the description that put them there and read
    /// both ways round. An instruction is exempt for this reason exactly when the machine describes
    /// it as a search, so the list cannot grow an opcode that is something else, and a search this
    /// target grows later cannot be left off the list and quietly go unselected with nobody saying
    /// why. Nothing in the rule set selects one, which is the other half of the reason and is what
    /// the check above would have caught in any case.
    #[test]
    fn every_instruction_exempt_from_a_rule_because_only_a_template_searches_for_a_bit_is_one() {
        let written = heads();
        for &opcode in SEARCH {
            let form = x86_64::form(opcode).expect("an instruction this target describes");
            assert_eq!(form, x86_64::Form::Search, "{opcode} is not a search");
            assert!(
                !written.contains(&format!("{PREFIX}{opcode}").as_str()),
                "a rule in {} selects {opcode}, which only a template asks for",
                TABLE.source
            );
        }
        for &(opcode, form) in x86_64::INSTS {
            if form == x86_64::Form::Search {
                assert!(SEARCH.contains(&opcode), "{opcode} is a search and is not on the list");
            }
        }
    }

    /// The same claim about the byte reversal, read both ways round the way the searches are, and
    /// with the one thing that is different about it checked as well: this is the instruction of its
    /// shape that leaves the condition state alone, which is the whole reason it has a form rather
    /// than being a unary operation, so a description that stopped saying that would stop being the
    /// reason this list exists.
    #[test]
    fn every_instruction_exempt_from_a_rule_because_only_a_template_turns_a_register_round_is_one()
    {
        let written = heads();
        for &opcode in SWAP {
            let form = x86_64::form(opcode).expect("an instruction this target describes");
            // Two forms and one job. The wide reversals are one shape and the sixteen bit one is
            // another, because the narrow one is an exchange between the halves of a register and
            // has to say which register, so what they share is the answer they compute rather than
            // the operands they compute it from.
            assert!(
                matches!(form, x86_64::Form::Swap | x86_64::Form::SwapHalves),
                "{opcode} is not a byte reversal"
            );
            assert!(
                !(x86_64::FLAGS.writes)(opcode),
                "{opcode} writes the condition state, so it is a unary operation after all"
            );
            assert!(
                !written.contains(&format!("{PREFIX}{opcode}").as_str()),
                "a rule in {} selects {opcode}, which only a template asks for",
                TABLE.source
            );
        }
        for &(opcode, form) in x86_64::INSTS {
            if matches!(form, x86_64::Form::Swap | x86_64::Form::SwapHalves) {
                assert!(SWAP.contains(&opcode), "{opcode} is a reversal and is not on the list");
            }
        }
    }

    /// The same claim about the two that work on a pair of registers, read both ways round and with
    /// the thing that puts them out of reach of a rule checked rather than asserted in prose: each
    /// writes two registers, and a rule replaces a term with a term, so there is no way to say the
    /// second answer in the rule language at all. That is the same bar the compare and exchange is
    /// exempt at, and this list is separate from that one because the reason it is nobody's to select
    /// is different: an atomic is written by name where it is needed, and nothing in this compiler
    /// needs one of these.
    #[test]
    fn every_instruction_exempt_from_a_rule_because_only_a_template_wants_both_halves_writes_two() {
        let written = heads();
        let both = [x86_64::Form::MulWide, x86_64::Form::DivWide];
        for &opcode in WIDE {
            let form = x86_64::form(opcode).expect("an instruction this target describes");
            assert!(both.contains(&form), "{opcode} works on one register rather than on a pair");
            let defs = form.operands().iter().filter(|desc| desc.role.is_def()).count();
            assert_eq!(defs, 2, "{opcode} writes {defs} registers and a pair takes two");
            assert!(
                !written.contains(&format!("{PREFIX}{opcode}").as_str()),
                "a rule in {} selects {opcode}, which only a template asks for",
                TABLE.source
            );
        }
        for &(opcode, form) in x86_64::INSTS {
            if both.contains(&form) {
                assert!(WIDE.contains(&opcode), "{opcode} works on a pair and is not on the list");
            }
        }
    }

    /// The same claim about the carry pair, and the one thing that has to be true of them that is
    /// not true of anything else on any of these lists. An instruction here reads the condition
    /// state and writes it, which is what makes it half of a pair and not a rewrite of its own, and
    /// the scheduler will only keep it behind the instruction that set the bit if the target says
    /// it reads one.
    #[test]
    fn every_instruction_exempt_from_a_rule_because_it_reads_a_carry_says_it_reads_the_state() {
        let written = heads();
        for &opcode in CARRY {
            let form = x86_64::form(opcode).expect("an instruction this target describes");
            let pair = matches!(form, x86_64::Form::AluCarry | x86_64::Form::AluCarryI);
            assert!(pair, "{opcode} is not one of the pair");
            assert_eq!(
                x86_64::FLAGS.reads(opcode),
                Some(rucc_target::Reads::Carry),
                "{opcode} does not say it reads the carry, so the scheduler may move it"
            );
            assert!(
                (x86_64::FLAGS.writes)(opcode),
                "{opcode} is said to leave the condition state alone"
            );
            assert!(
                !written.contains(&format!("{PREFIX}{opcode}").as_str()),
                "a rule in {} selects {opcode}, which only a template asks for",
                TABLE.source
            );
        }
        for &(opcode, form) in x86_64::INSTS {
            if matches!(form, x86_64::Form::AluCarry | x86_64::Form::AluCarryI) {
                assert!(CARRY.contains(&opcode), "{opcode} reads a carry and is not on the list");
            }
        }
    }

    /// The same claim about the conditional moves, read off the flag description the way the compare
    /// pass's list is taken from it rather than typed out twice. An instruction is exempt for this
    /// reason exactly when it reads the condition state and leaves it as it found it, which is what
    /// says the instruction in front of it is where its meaning comes from. One that wrote the state
    /// as well would be one a pattern could match on its own.
    #[test]
    fn every_instruction_exempt_from_a_rule_because_a_comparison_gives_it_its_meaning_reads_one() {
        for &opcode in CONDITIONAL {
            x86_64::form(opcode).expect("an instruction this target describes");
            assert!(
                x86_64::FLAGS.reads(opcode).is_some(),
                "{opcode} reads no comparison, so a rule could name it"
            );
            assert!(
                !(x86_64::FLAGS.writes)(opcode),
                "{opcode} writes the condition state, so a rule could name it"
            );
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
