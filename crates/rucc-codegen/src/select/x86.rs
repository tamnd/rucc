//! The i386 lowering table.
//!
//! Generated from `rules/i386.rules` when this crate is built, the way the x86-64 table is from
//! its own file. The heads are x86-64's, since the instructions are, and what keeps a sixty four
//! bit one out of an i386 function is that no rule in the file names one and
//! [`rucc_target::x86::MACHINE`] refuses any that something else proposes.
//!
//! A compile for `i686-linux-gnu` reaches it through `crate::Machine::for_target`, which also sets
//! `esi` and `edi` aside as the two scratch registers the way `r10` and `r11` are on x86-64. The
//! table is checked by the tests below and by `rucc-verify` as well.

// For the reason `super::x86_64` gives.
#![allow(clippy::manual_range_contains)]

include!(concat!(env!("OUT_DIR"), "/i386.rs"));

/// What the lowering asks of i386.
///
/// The x86-64 selector at thirty two bits: an address is computed with `leal` and read with
/// `movl`, the thread pointer is the word at `%gs:0`, and a jump table's cell is already as wide
/// as an address, so reading one is a plain load. The scratch registers are `esi` and `edi`, for
/// the reason `crate::pipeline::X86_SCRATCH` gives.
pub static SELECTOR: super::Selector = super::Selector {
    table: &TABLE,
    shapes: &rucc_target::x86::MACHINE,
    address: rucc_target::x86_64::address,
    frame: &rucc_target::x86::FRAME,
    branch: &rucc_target::x86::BRANCH,
    gpr: rucc_target::x86::GPR,
    fence: "mfence",
    trap: "ud2",
    abi: &crate::abi::X86,
    scratch: &crate::pipeline::X86_SCRATCH,
    symbols: &super::Symbols {
        near: super::Reach::Mode("lea_32"),
        far: super::Reach::Mode("mov_rm_32"),
        slot: super::Reach::Mode("mov_rm_32"),
        thread: super::Reach::Mode("mov_rm_32"),
        pointer: super::Pointer::Segment("mov_rm_32", rucc_target::Segment::Gs),
        // The thread block is in `%fs` on 32-bit Windows, and the array of `.tls` copies is 44
        // bytes into it. Only a COFF file reaches it. See `crate::elsewhere::Elsewhere::indexed`.
        indexed: Some(super::Indexed {
            index: "mov_rm_32",
            load: "mov_rm_32",
            segment: rucc_target::Segment::Fs,
            at: 0x2c,
            scale: 4,
            add: "lea_32",
        }),
        teb: None,
    },
    jumps: &super::Jumps { near: "lea_32", cell: "mov_rm_32", add: "add_rr_32", two_address: true },
};

#[cfg(test)]
mod tests {
    use rucc_target::x86;

    use super::{SELECTOR, TABLE};
    use crate::select::Piece;

    /// The two address constructors, which are not instructions. See `super::super::x86_64`.
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

    /// Every instruction a rule selects is one the machine has, which is a stronger claim than
    /// the x86-64 table's: the description has to know the opcode, and the encoder has to be able
    /// to write it with no REX byte.
    #[test]
    fn every_instruction_the_table_writes_is_one_i386_has() {
        for head in heads() {
            if AMODES.contains(&head) {
                continue;
            }
            let opcode = head
                .strip_prefix(SELECTOR.prefix())
                .unwrap_or_else(|| panic!("{head} is neither an x86 term nor an addressing mode"));
            assert!(
                x86::form_here(opcode).is_some(),
                "{head} is selected and i386 has no {opcode}"
            );
        }
    }

    /// No pattern names a sixty four bit integer, because `crate::wide` has split every one of
    /// them into two registers before selection sees it, and an address is thirty two bits.
    #[test]
    fn no_rule_matches_a_sixty_four_bit_integer() {
        for rule in TABLE.rules {
            assert!(!rule.pattern.contains(".i64"), "line {}: {}", rule.line, rule.pattern);
        }
    }

    /// A store binds the value first and the address second, and the address is thirty two bits.
    /// See the test of the same name beside the x86-64 table for why the order matters.
    #[test]
    fn a_store_is_written_with_the_value_first_and_a_thirty_two_bit_address() {
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
                operands.contains("(value.i32 a)"),
                "line {}: {} reaches no address",
                rule.line,
                rule.pattern
            );
            seen += 1;
        }
        // x86-64's sixteen less the two at sixty four bits.
        assert_eq!(seen, 14, "the store rules moved and this test did not follow them");
    }

    /// Every comparison against a register has its twin against a constant, at the three widths.
    #[test]
    fn a_comparison_against_a_constant_is_written_for_every_one_against_a_register() {
        let mut against_register = Vec::new();
        let mut against_constant = Vec::new();
        for rule in TABLE.rules {
            let Some(rest) = rule.pattern.strip_prefix("(icmp_") else { continue };
            if rest.contains("(iconst.") {
                against_constant.push(rest.replace("(iconst.", "(value."));
            } else {
                against_register.push(rest.replace(" y)", " k)"));
            }
        }
        against_register.sort_unstable();
        against_constant.sort_unstable();
        assert_eq!(against_register, against_constant);
        assert_eq!(against_register.len(), 30, "ten conditions at three widths");
    }

    /// Everything the frame, the branches and the lowering name by hand is on this machine too.
    #[test]
    fn every_instruction_the_selector_names_is_one_i386_has() {
        let symbols = SELECTOR.symbols;
        let mut names = vec![SELECTOR.fence, SELECTOR.trap];
        for reach in [symbols.near, symbols.far, symbols.slot, symbols.thread] {
            match reach {
                crate::select::Reach::Mode(name) | crate::select::Reach::Own(name) => {
                    names.push(name);
                }
            }
        }
        let (crate::select::Pointer::Segment(name, _) | crate::select::Pointer::Own(name)) =
            symbols.pointer;
        names.push(name);
        names.extend([SELECTOR.jumps.near, SELECTOR.jumps.cell, SELECTOR.jumps.add]);
        for name in [SELECTOR.abi.call, SELECTOR.abi.call_reg, SELECTOR.abi.lea, SELECTOR.abi.small]
        {
            names.push(name.strip_prefix(SELECTOR.prefix()).expect("an x86 opcode"));
        }
        for name in names {
            assert!(x86::form_here(name).is_some(), "i386 has no {name}");
        }
    }
}
