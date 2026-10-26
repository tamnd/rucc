//! The RISC-V lowering table.
//!
//! Everything below the module comment is generated from `rules/riscv64.rules` by `rucc-rules`
//! when this crate is built, the same way the other tables are, and `crate::pipeline::Machine`
//! hands it to the lowering for any target whose architecture is RISC-V.

// The guards are emitted as the comparisons the rules write, for the reason the x86-64 table gives.
#![allow(clippy::manual_range_contains)]

include!(concat!(env!("OUT_DIR"), "/riscv64.rs"));

/// What the lowering asks of RISC-V.
pub static SELECTOR: super::Selector = super::Selector {
    table: &TABLE,
    counts: rucc_target::riscv64::COUNTS,
    fused: Some(""),
    shapes: &rucc_target::riscv64::MACHINE,
    address: rucc_target::riscv64::address,
    frame: &rucc_target::riscv64::FRAME,
    branch: &rucc_target::riscv64::BRANCH,
    gpr: rucc_target::riscv64::GPR,
    fence: "fence",
    trap: "trap",
    abi: &crate::abi::riscv64::INSTS,
    scratch: &crate::pipeline::RISCV64_SCRATCH,
    symbols: &super::Symbols {
        near: super::Reach::Own("addr_64"),
        far: super::Reach::Own("got_64"),
        // Only COFF reaches a name through a pointer of its own, and there is no COFF here.
        slot: super::Reach::Own("got_64"),
        thread: super::Reach::Own("gottprel_64"),
        pointer: super::Pointer::Own("thread_64"),
        indexed: None,
        teb: None,
        dynamic: None,
    },
    // No address scales a register here, so the index is shifted and added to the table first.
    jumps: &super::Jumps {
        near: "addr_64",
        cell: "lds_32",
        add: "add_rr_64",
        two_address: false,
        scale: Some("sll_ri_64"),
    },
};

#[cfg(test)]
mod tests {
    use rucc_target::riscv64;

    use super::TABLE;
    use crate::select::{Piece, Subject};

    /// The prefix the rule file puts in front of a machine term.
    const PREFIX: &str = "rv.";

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
        let text = include_str!("../../rules/riscv64.rules");
        let written = text.lines().filter(|line| line.starts_with("(rule ")).count();
        assert_eq!(TABLE.rules.len(), written, "the table and the rule file disagree");
        assert_eq!(TABLE.source, "rules/riscv64.rules");
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
                    panic!("line {}: {head} is neither a RISC-V term nor an address", rule.line)
                });
                assert!(
                    riscv64::form(opcode).is_some(),
                    "line {}: {head} is selected and `rucc_target::riscv64` does not describe it",
                    rule.line
                );
            }
        }
    }

    /// The narrow widths take the `w` instruction for the operations whose low bits do not depend
    /// on the high ones.
    #[test]
    fn narrow_arithmetic_is_the_word_instruction() {
        let mut terms = Terms::default();
        for width in ["i8", "i16", "i32"] {
            let x = terms.value(width, "v0");
            let y = terms.value(width, "v1");
            let add = terms.app(&format!("add.{width}"), &[x, y]);
            assert_eq!(selects(&terms, add), Some("rv.add_rr_32"));
            let mul = terms.app(&format!("mul.{width}"), &[x, y]);
            assert_eq!(selects(&terms, mul), Some("rv.mul_rr_32"));
        }
        let x = terms.value("i64", "v2");
        let y = terms.value("i64", "v3");
        let add = terms.app("add.i64", &[x, y]);
        assert_eq!(selects(&terms, add), Some("rv.add_rr_64"));
        let rem = terms.app("srem.i64", &[x, y]);
        assert_eq!(selects(&terms, rem), Some("rv.rem_rr_64"));
    }

    /// Every instruction that takes a constant takes twelve bits signed, and taking a constant
    /// away is adding its negation, so the range is one further up for a subtraction.
    #[test]
    fn an_immediate_is_taken_when_twelve_bits_hold_it() {
        let mut terms = Terms::default();
        let x = terms.value("i32", "v0");
        for (op, value, want) in [
            ("add", 2047, Some("rv.add_ri_32")),
            ("add", -2048, Some("rv.add_ri_32")),
            ("add", 2048, None),
            ("sub", 2048, Some("rv.add_ri_32")),
            ("sub", -2048, None),
            ("xor", -1, Some("rv.not_r_32")),
        ] {
            let k = terms.constant("iconst.i32", value);
            let term = terms.app(&format!("{op}.i32"), &[x, k]);
            assert_eq!(selects(&terms, term), want, "{op} {value}");
        }
    }

    /// A word compares as the whole register, since it is kept sign extended there. A byte is
    /// widened first, the way the predicate reads it.
    #[test]
    fn a_comparison_widens_only_what_is_narrower_than_a_word() {
        let mut terms = Terms::default();
        let x = terms.value("i32", "v0");
        let y = terms.value("i32", "v1");
        let less = terms.app("icmp_ult.i1", &[x, y]);
        assert_eq!(selects(&terms, less), Some("rv.cmp_set_lo_32"));
        let x = terms.value("i8", "v2");
        let y = terms.value("i8", "v3");
        let less = terms.app("icmp_slt.i1", &[x, y]);
        assert_eq!(selects(&terms, less), Some("rv.cmp_set_lt_32"));
        let found = TABLE.find(&terms, less).expect("a byte comparison has a rule");
        let widens = TABLE
            .rule(&found)
            .replacement
            .iter()
            .any(|piece| matches!(piece, Piece::App { head, .. } if *head == "rv.sextb_32"));
        assert!(widens, "a signed comparison of bytes compares them sign extended");
    }

    /// Every widening and narrowing the IR has between its integer types has a rule.
    #[test]
    fn every_widening_and_narrowing_has_an_instruction() {
        let mut terms = Terms::default();
        let wanted = [
            ("sext", "i8", "i16", "rv.sextb_16"),
            ("sext", "i32", "i64", "rv.sextw_64"),
            ("zext", "i8", "i64", "rv.zextb_64"),
            ("zext", "i32", "i64", "rv.zextw_64"),
            ("zext", "i1", "i64", "rv.bit_to_64"),
            ("trunc", "i64", "i32", "rv.low_32"),
            ("trunc", "i16", "i8", "rv.low_8"),
            ("trunc", "i64", "i1", "rv.bit_of_64"),
        ];
        for (op, from, to, want) in wanted {
            let x = terms.value(from, "v0");
            let term = terms.app(&format!("{op}.{from}.{to}"), &[x]);
            assert_eq!(selects(&terms, term), Some(want), "{op}.{from}.{to}");
        }
    }

    /// An offset of twelve bits goes into the address, and a larger one leaves the addition to be
    /// a value of its own.
    #[test]
    fn an_offset_of_twelve_bits_is_part_of_the_address() {
        let mut terms = Terms::default();
        let a = terms.value("i64", "v0");
        for (offset, want) in [(2047, Some("rv.ld_32")), (-2048, Some("rv.ld_32")), (2048, None)] {
            let k = terms.constant("iconst.i64", offset);
            let at = terms.app("add.i64", &[a, k]);
            let load = terms.app("load.i32", &[at]);
            assert_eq!(selects(&terms, load), want, "{offset}");
        }
    }
}
