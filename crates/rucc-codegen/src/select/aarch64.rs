//! The AArch64 lowering table.
//!
//! Everything below the module comment is generated from `rules/aarch64.rules` by `rucc-rules`
//! when this crate is built, the same way the x86-64 table is, and `crate::pipeline::Machine`
//! hands it to the lowering for any target whose architecture is AArch64.

// The guards are emitted as the comparisons the rules write, for the reason the x86-64 table gives.
#![allow(clippy::manual_range_contains)]

include!(concat!(env!("OUT_DIR"), "/aarch64.rs"));

/// What the lowering asks of AArch64.
pub static SELECTOR: super::Selector = super::Selector {
    table: &TABLE,
    counts: rucc_target::aarch64::COUNTS,
    shapes: &rucc_target::aarch64::MACHINE,
    address: rucc_target::aarch64::address,
    frame: &rucc_target::aarch64::FRAME,
    branch: &rucc_target::aarch64::BRANCH,
    gpr: rucc_target::aarch64::GPR,
    fence: "fence",
    trap: "trap",
    abi: &crate::abi::aarch64::INSTS,
    scratch: &crate::pipeline::AARCH64_SCRATCH,
    symbols: &super::Symbols {
        near: super::Reach::Own("addr_64"),
        far: super::Reach::Own("got_64"),
        slot: super::Reach::Own("slot_64"),
        thread: super::Reach::Own("gottprel_64"),
        pointer: super::Pointer::Own("thread_64"),
        indexed: None,
        teb: Some(super::Teb { index: "ldr_sym_32", array: "teb_64", block: "secrel_64" }),
    },
    jumps: &super::Jumps {
        near: "adr_64",
        cell: "ldrs_32_64",
        add: "add_rr_64",
        two_address: false,
    },
};

#[cfg(test)]
mod tests {
    use rucc_target::aarch64;

    use super::TABLE;
    use crate::select::{Piece, Subject};

    /// The prefix the rule file puts in front of a machine term.
    const PREFIX: &str = "a64.";

    /// The address constructors, which are terms in the rule file and not instructions.
    const AMODES: &[&str] = &["amode_base", "amode_base_offset"];

    /// A term as a flat arena, which is the shape the x86-64 tests use and the shape the IR has.
    #[derive(Debug)]
    enum Node {
        Int(i128),
        App(String, Vec<usize>),
    }

    #[derive(Debug, Default)]
    struct Terms {
        nodes: Vec<Node>,
    }

    impl Terms {
        fn constant(&mut self, head: &str, value: i128) -> usize {
            self.nodes.push(Node::Int(value));
            let at = self.nodes.len() - 1;
            self.app(head, &[at])
        }

        fn app(&mut self, head: &str, args: &[usize]) -> usize {
            self.nodes.push(Node::App(head.to_owned(), args.to_vec()));
            self.nodes.len() - 1
        }

        fn value(&mut self, width: &str, name: &str) -> usize {
            let inner = self.app(name, &[]);
            self.app(&format!("value.{width}"), &[inner])
        }
    }

    impl Subject for Terms {
        type Node = usize;

        fn head(&self, node: usize) -> Option<(&str, usize)> {
            match &self.nodes[node] {
                Node::App(head, args) => Some((head.as_str(), args.len())),
                Node::Int(_) => None,
            }
        }

        fn arg(&self, node: usize, index: usize) -> usize {
            match &self.nodes[node] {
                Node::App(_, args) => args[index],
                Node::Int(_) => unreachable!("a constant has no arguments"),
            }
        }

        fn int(&self, node: usize) -> Option<i128> {
            match self.nodes[node] {
                Node::Int(value) => Some(value),
                Node::App(..) => None,
            }
        }

        fn same(&self, a: usize, b: usize) -> bool {
            a == b
        }
    }

    fn selects(terms: &Terms, term: usize) -> Option<&'static str> {
        let found = TABLE.find(terms, term)?;
        TABLE.rule(&found).head()
    }

    #[test]
    fn the_table_holds_every_rule_the_file_writes() {
        let text = include_str!("../../rules/aarch64.rules");
        let written = text.lines().filter(|line| line.starts_with("(rule ")).count();
        assert_eq!(TABLE.rules.len(), written, "the table and the rule file disagree");
        assert_eq!(TABLE.source, "rules/aarch64.rules");
    }

    #[test]
    fn every_instruction_the_table_writes_is_described() {
        for rule in TABLE.rules {
            for piece in rule.replacement {
                let Piece::App { head, .. } = piece else { continue };
                if AMODES.contains(head) {
                    continue;
                }
                let opcode = head.strip_prefix(PREFIX).unwrap_or_else(|| {
                    panic!("line {}: {head} is neither an AArch64 term nor an address", rule.line)
                });
                assert!(
                    aarch64::form(opcode).is_some(),
                    "line {}: {head} is selected and `rucc_target::aarch64` does not describe it",
                    rule.line
                );
            }
        }
    }

    /// The narrow widths take the thirty two bit instruction for the operations whose low bits do
    /// not depend on the high ones, and nothing else reaches them.
    #[test]
    fn narrow_arithmetic_is_the_thirty_two_bit_instruction() {
        let mut terms = Terms::default();
        for width in ["i8", "i16", "i32"] {
            let x = terms.value(width, "v0");
            let y = terms.value(width, "v1");
            let add = terms.app(&format!("add.{width}"), &[x, y]);
            assert_eq!(selects(&terms, add), Some("a64.add_rr_32"));
            let mul = terms.app(&format!("mul.{width}"), &[x, y]);
            assert_eq!(selects(&terms, mul), Some("a64.mul_rr_32"));
        }
        let x = terms.value("i8", "v0");
        let y = terms.value("i8", "v1");
        let shift = terms.app("lshr.i8", &[x, y]);
        assert_eq!(selects(&terms, shift), None);
        let x = terms.value("i64", "v2");
        let y = terms.value("i64", "v3");
        let add = terms.app("add.i64", &[x, y]);
        assert_eq!(selects(&terms, add), Some("a64.add_rr_64"));
    }

    /// A narrow right shift by a constant is the instruction that widens first, since the bits
    /// above a byte or a half in its register are not known.
    #[test]
    fn a_narrow_right_shift_by_a_constant_is_the_instruction_that_widens_first() {
        let mut terms = Terms::default();
        for (width, bits) in [("i8", 7), ("i16", 15)] {
            let x = terms.value(width, "v0");
            let k = terms.constant(&format!("iconst.{width}"), 3);
            let right = terms.app(&format!("lshr.{width}"), &[x, k]);
            assert_eq!(selects(&terms, right), Some(&*format!("a64.lsr_ri_{}", &width[1..])));
            let right = terms.app(&format!("ashr.{width}"), &[x, k]);
            assert_eq!(selects(&terms, right), Some(&*format!("a64.asr_ri_{}", &width[1..])));
            let far = terms.constant(&format!("iconst.{width}"), bits + 1);
            let right = terms.app(&format!("lshr.{width}"), &[x, far]);
            assert_eq!(selects(&terms, right), None);
        }
    }

    /// A constant is one `mov` when it or its complement fits in sixteen bits, or when every
    /// sixteen bit piece but one is zero or every one but one is all ones. Anything else is built a
    /// piece at a time, and the outermost instruction says how many pieces: one `movk` under two
    /// to the thirty two, two when the top piece is zero, and three otherwise.
    #[test]
    fn a_constant_is_one_mov_only_when_one_mov_can_build_it() {
        let mut terms = Terms::default();
        let wanted = [
            (0, "a64.mov_ri_64"),
            (65535, "a64.mov_ri_64"),
            (-65536, "a64.mov_ri_64"),
            (65536, "a64.mov_ri_64"),
            (0x1_0000_0000, "a64.mov_ri_64"),
            (0x4008_0000_0000_0000, "a64.mov_ri_64"),
            (-0x1234_0000_0001, "a64.mov_ri_64"),
            (-0x1_0001, "a64.mov_ri_64"),
            (0x1_0001, "a64.movk_ri_16_64"),
            (0xffff_ffff, "a64.movk_ri_16_64"),
            (0x1_2345_6789, "a64.movk_ri_32_64"),
            (0x1_0000_0001, "a64.movk_ri_32_64"),
            (-0x1_0002, "a64.movk_ri_48_64"),
            (0x1_0000_0000_0001, "a64.movk_ri_48_64"),
        ];
        for (value, want) in wanted {
            let k = terms.constant("iconst.i64", value);
            assert_eq!(selects(&terms, k), Some(want), "{value}");
        }
        let wanted = [
            (0x1234_5678, "a64.movk_ri_16_32"),
            (0x4040_0000, "a64.mov_ri_32"),
            (0x1234_ffff, "a64.mov_ri_32"),
            (i128::from(i32::MIN), "a64.mov_ri_32"),
        ];
        for (value, want) in wanted {
            let k = terms.constant("iconst.i32", value);
            assert_eq!(selects(&terms, k), Some(want), "{value}");
        }
    }

    /// Every widening and narrowing the IR has between its integer types has a rule.
    #[test]
    fn every_widening_and_narrowing_has_an_instruction() {
        let mut terms = Terms::default();
        let wanted = [
            ("sext", "i8", "i16", "a64.sxtb_16"),
            ("zext", "i8", "i16", "a64.uxtb_16"),
            ("zext", "i8", "i64", "a64.uxtb_64"),
            ("zext", "i16", "i64", "a64.uxth_64"),
            ("zext", "i1", "i8", "a64.bit_to_8"),
            ("zext", "i1", "i64", "a64.bit_to_64"),
            ("trunc", "i64", "i32", "a64.low_32"),
            ("trunc", "i32", "i16", "a64.low_16"),
            ("trunc", "i16", "i8", "a64.low_8"),
            ("trunc", "i8", "i1", "a64.bit_of_32"),
            ("trunc", "i64", "i1", "a64.bit_of_64"),
        ];
        for (op, from, to, want) in wanted {
            let x = terms.value(from, "v0");
            let term = terms.app(&format!("{op}.{from}.{to}"), &[x]);
            assert_eq!(selects(&terms, term), Some(want), "{op}.{from}.{to}");
        }
    }

    #[test]
    fn an_immediate_is_taken_when_twelve_bits_hold_it() {
        let mut terms = Terms::default();
        let x = terms.value("i32", "v0");
        let k = terms.constant("iconst.i32", 4095);
        let add = terms.app("add.i32", &[x, k]);
        assert_eq!(selects(&terms, add), Some("a64.add_ri_32"));
        let k = terms.constant("iconst.i32", 4096);
        let add = terms.app("add.i32", &[x, k]);
        assert_eq!(selects(&terms, add), None);
    }

    /// The constants a logical instruction holds, built the way the architecture describes them:
    /// a run of ones in a piece, turned round within it, the piece repeated to fill the register.
    fn patterns(bits: u32) -> std::collections::BTreeSet<u64> {
        let mut out = std::collections::BTreeSet::new();
        let mut size = 2;
        while size <= bits {
            let piece_mask = if size == 64 { u64::MAX } else { (1u64 << size) - 1 };
            for ones in 1..size {
                let run = (1u64 << ones) - 1;
                for by in 0..size {
                    let piece =
                        if by == 0 { run } else { (run >> by | run << (size - by)) & piece_mask };
                    let mut value = 0u64;
                    for at in (0..bits).step_by(size as usize) {
                        value |= piece << at;
                    }
                    out.insert(value);
                }
            }
            size *= 2;
        }
        out
    }

    /// The guard on the logical immediates says yes to exactly the patterns above. Every pattern
    /// is asked about, and so is every number one bit away from one, which is where a mistake in
    /// the test for a run would show.
    #[test]
    fn the_logical_immediate_guard_takes_exactly_the_patterns() {
        for bits in [32u32, 64] {
            let all = patterns(bits);
            assert_eq!(all.len(), if bits == 64 { 5334 } else { 1302 });
            let width_mask = if bits == 64 { u64::MAX } else { u64::from(u32::MAX) };
            for &value in &all {
                assert!(super::logical_immediate(bits, i128::from(value)), "{bits} {value:#x}");
                for bit in 0..bits {
                    let near = value ^ (1u64 << bit);
                    let want = all.contains(&near);
                    assert_eq!(super::logical_immediate(bits, i128::from(near)), want, "{near:#x}");
                }
            }
            // The IR holds a thirty two bit constant sign extended, and the answer is about the
            // bits the instruction gets.
            let sign = |value: u64| i128::from(value as u32 as i32);
            if bits == 32 {
                assert!(super::logical_immediate(32, sign(0xffff_fff0)));
                assert!(super::logical_immediate(32, sign(0x8000_0000)));
            }
            for value in [0, width_mask, 0x12345, 0xff01] {
                assert!(!super::logical_immediate(bits, i128::from(value)), "{value:#x}");
            }
        }
    }

    #[test]
    fn a_logical_operation_takes_a_pattern_and_leaves_the_rest_to_a_register() {
        let mut terms = Terms::default();
        let x = terms.value("i32", "v0");
        let k = terms.constant("iconst.i32", -16);
        let and = terms.app("and.i32", &[x, k]);
        assert_eq!(selects(&terms, and), Some("a64.and_ri_32"));
        let y = terms.value("i64", "v1");
        let k = terms.constant("iconst.i64", 0x5555_5555_5555_5555);
        let xor = terms.app("xor.i64", &[y, k]);
        assert_eq!(selects(&terms, xor), Some("a64.eor_ri_64"));
        let k = terms.constant("iconst.i32", 0x8000_0000_u32.cast_signed().into());
        let or = terms.app("or.i32", &[x, k]);
        assert_eq!(selects(&terms, or), Some("a64.orr_ri_32"));
        for odd in [0, -1, 0x12345] {
            let k = terms.constant("iconst.i32", odd);
            let and = terms.app("and.i32", &[x, k]);
            assert_eq!(selects(&terms, and), None, "{odd:#x}");
        }
    }

    /// The IR's unsigned predicates are under the architecture's names for them.
    #[test]
    fn an_unsigned_comparison_is_the_condition_the_architecture_names() {
        let mut terms = Terms::default();
        let x = terms.value("i64", "v0");
        let y = terms.value("i64", "v1");
        for (ir, cc) in [("ult", "lo"), ("ule", "ls"), ("ugt", "hi"), ("uge", "hs")] {
            let term = terms.app(&format!("icmp_{ir}.i1"), &[x, y]);
            let want = format!("a64.cmp_set_{cc}_64");
            assert_eq!(selects(&terms, term), Some(want.as_str()));
        }
    }

    /// A byte swap is `rev`, both zero counts end in `clz`, the trailing one counting the value
    /// turned round by `rbit`, and the set bits are counted in a vector register.
    #[test]
    fn a_byte_swap_and_the_zero_counts_are_the_instructions_the_architecture_has() {
        let mut terms = Terms::default();
        for width in ["i32", "i64"] {
            let x = terms.value(width, "v0");
            let bits = &width[1..];
            let pairs =
                [("bswap", "rev_r"), ("ctlz", "clz_r"), ("cttz", "clz_r"), ("ctpop", "cnt")];
            for (ir, inst) in pairs {
                let term = terms.app(&format!("{ir}.{width}"), &[x]);
                let want = format!("a64.{inst}_{bits}");
                assert_eq!(selects(&terms, term), Some(want.as_str()), "{ir}.{width}");
            }
        }
    }

    /// Every comparison against a register has one against a constant, for the reason the x86-64
    /// table counts them.
    #[test]
    fn a_comparison_against_a_constant_is_written_for_every_one_against_a_register() {
        let mut against_register = Vec::new();
        let mut against_constant = Vec::new();
        let mut narrow = 0;
        for rule in TABLE.rules {
            let Some(rest) = rule.pattern.strip_prefix("(icmp_") else { continue };
            let (condition, operands) = rest.split_once(".i1 ").expect("a comparison takes two");
            // A test of the bits under a mask compares an `and` with zero, which is `tst` and has
            // no form against a register to pair with.
            if operands.starts_with("(and.") {
                continue;
            }
            let width = operands
                .strip_prefix("(value.")
                .and_then(|rest| rest.split_once(' '))
                .map(|(width, _)| width)
                .expect("a comparison reads a value first");
            // The narrow widths compare two registers, each widened first, and have no form
            // against a constant.
            if matches!(width, "i8" | "i16") {
                narrow += 1;
                continue;
            }
            let named = format!("{condition}.{width}");
            if operands.contains("(iconst.") {
                against_constant.push(named);
            } else {
                against_register.push(named);
            }
        }
        against_register.sort_unstable();
        against_constant.sort_unstable();
        assert_eq!(against_register, against_constant);
        assert_eq!(against_register.len(), 20, "ten conditions at two widths");
        assert_eq!(narrow, 20, "ten conditions at the two narrow widths");
    }

    /// The value comes first in a store, which is where the IR keeps it.
    #[test]
    fn a_store_is_written_with_the_value_first() {
        let mut seen = 0;
        for rule in TABLE.rules {
            let Some(rest) = rule.pattern.strip_prefix("(store.") else { continue };
            let (width, operands) = rest.split_once(' ').expect("a store takes operands");
            assert!(
                operands.starts_with(&format!("(value.{width} ")),
                "line {}: {} binds something other than the value first",
                rule.line,
                rule.pattern
            );
            seen += 1;
        }
        assert_eq!(seen, 16, "the store rules moved and this test did not follow them");
    }

    /// An offset the unscaled form holds goes into the address, and one it does not leaves the
    /// addition where it is.
    #[test]
    fn a_small_offset_is_part_of_the_address() {
        let mut terms = Terms::default();
        let a = terms.value("i64", "v0");
        let k = terms.constant("iconst.i64", -8);
        let at = terms.app("add.i64", &[a, k]);
        let load = terms.app("load.i32", &[at]);
        let found = TABLE.find(&terms, load).expect("a rule fires");
        assert_eq!(TABLE.rule(&found).head(), Some("a64.ldr_32"));
        assert!(
            TABLE
                .rule(&found)
                .replacement
                .iter()
                .any(|piece| matches!(piece, Piece::App { head: "amode_base_offset", .. }))
        );
        let k = terms.constant("iconst.i64", 256);
        let at = terms.app("add.i64", &[a, k]);
        let load = terms.app("load.i32", &[at]);
        assert_eq!(selects(&terms, load), None);
    }
}
