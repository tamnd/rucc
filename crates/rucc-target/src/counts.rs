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
//! A count can be in the base architecture rather than in an extension, which is how AArch64 has
//! both of its: `clz` for the leading zeros, and `rbit` then `clz` for the trailing ones, which is two
//! instructions and still a long way short of the arithmetic. Those have no feature to name and are
//! on everywhere.
//!
//! A count can also be guarded, which is how x86-64 has its two zero counts on a processor without
//! `lzcnt` or BMI. `bsr` and `bsf` find the bit, but what they leave for a zero is not the same on
//! every processor, so a rule takes one only where the count is written as a choice between the
//! width, for a zero, and the count, for anything else. A conditional move after the search is that
//! choice, so a guarded count is three or four instructions: much better than the arithmetic, and
//! not one instruction, which is why the loop deletion pass does not count it as one.
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

/// A bit count that is one instruction on a processor with the extension that has it, or a short
/// sequence where it is guarded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CountInst {
    /// The count it answers.
    pub of: BitCount,
    /// The extension, by the name `-m` and `__attribute__((target))` give it, or empty for an
    /// instruction every processor of the architecture has.
    pub feature: &'static str,
    /// The widths in bits a rule selects it at.
    pub widths: &'static [u32],
    /// Whether a rule takes it only under the choice that answers the width for a zero, which the
    /// code generator writes the count as before selection.
    pub guarded: bool,
}

impl CountInst {
    /// Whether a processor with those extensions has it.
    #[must_use]
    pub fn on(self, isa: Isa) -> bool {
        self.feature.is_empty() || Feature::named(self.feature).is_some_and(|it| isa.has(it))
    }

    /// Whether one of `table` is that count at that width on a processor with those extensions,
    /// as one instruction rather than a guarded sequence.
    #[must_use]
    pub fn in_one(table: &[CountInst], of: BitCount, bits: u32, isa: Isa) -> bool {
        table.iter().any(|count| {
            count.of == of && !count.guarded && count.widths.contains(&bits) && count.on(isa)
        })
    }
}
