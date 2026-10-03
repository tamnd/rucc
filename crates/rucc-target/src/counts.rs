//! The bit counts a machine has one instruction for.
//!
//! Design: `spec/optimizer/20-idioms-and-libcalls.md` section 20.6 and tamnd/rucc#310.
//!
//! Two places ask the same question of a target. The code generator decides which counts it
//! leaves for a rule and which it writes out as shifts, masks and a multiply, and the loop deletion
//! pass decides whether a loop that counts bits is worth turning into one count in front of it. If
//! each read its own table the two could disagree, and the pass would put a count where the code
//! generator then writes out a dozen instructions in place of a loop that went round twice. So the
//! table is a fact about the target, it is here for the reason [`crate::BitInsts`] is, and both
//! read it.
//!
//! It names the counts without the IR, since this crate sits below it, and each reader says which
//! of its instructions is which count.

use crate::isa::{Feature, Isa};

/// Which bits a count counts. All three answer the width for a zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BitCount {
    /// The bits that are set, which is `ctpop`.
    Ones,
    /// The zeros above the highest set bit, which is `ctlz`.
    LeadingZeros,
    /// The zeros below the lowest set bit, which is `cttz`.
    TrailingZeros,
}

/// A bit count that is one instruction on a processor with the extension that has it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CountInst {
    /// The count it answers.
    pub of: BitCount,
    /// The extension, by the name `-m` and `__attribute__((target))` give it.
    pub feature: &'static str,
    /// The widths in bits a rule selects it at.
    pub widths: &'static [u32],
}

impl CountInst {
    /// Whether a processor with those extensions has it.
    #[must_use]
    pub fn on(self, isa: Isa) -> bool {
        Feature::named(self.feature).is_some_and(|it| isa.has(it))
    }

    /// Whether one of `table` is that count at that width on a processor with those extensions.
    #[must_use]
    pub fn in_one(table: &[CountInst], of: BitCount, bits: u32, isa: Isa) -> bool {
        table.iter().any(|count| count.of == of && count.widths.contains(&bits) && count.on(isa))
    }
}
