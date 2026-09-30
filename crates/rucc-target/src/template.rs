//! A template kept as text, and the holes in it that are filled once registers are known.
//!
//! An `asm` statement whose text is not read back into instructions is carried to the writer as
//! the text, with a hole wherever it names an operand. What goes in a hole is a register, an
//! address or a name, and none of those is known when the text is written down: the register is
//! the allocator's answer and the name is spelled the way the object format wants. The holes are
//! the same on every machine and so is filling them, which is why they are here rather than with
//! either machine's table. What each machine spells into a hole is its writer's business.

/// Where the address a kept template names goes in its text.
///
/// The address is the instruction's memory operand, and where it is depends on registers nothing
/// has chosen yet when the text is written down, so the text holds this in its place and the writer
/// puts the address there. The two bytes around it are ones no template can hold, since a string a
/// program wrote into an `asm` statement is text an assembler reads.
pub const TEMPLATE_MEM: &str = "\u{1}m\u{2}";

/// The address a kept template names, that many bytes further on. See [`TEMPLATE_MEM`].
#[must_use]
pub fn template_mem_at(offset: i64) -> String {
    format!("\u{1}m{offset}\u{2}")
}

/// A name a kept template names, held the same way so the writer can spell it the way the object
/// format wants names spelled. See [`TEMPLATE_MEM`].
#[must_use]
pub fn template_name(name: &str) -> String {
    format!("\u{1}n{name}\u{2}")
}

/// A register a kept template names, held the same way. What it says is the instruction's operand
/// at `at` and the width to spell it at, as the letter gcc's modifier for that width is. On x86-64
/// that is `b`, `w`, `k`, `q`, or `h` for the second byte, and a capital of one of the first four
/// is that width with no `%` in front. On AArch64 it is `w` or `x`. The register is only known once the allocator has run, which is after the text is written down. See
/// [`TEMPLATE_MEM`].
#[must_use]
pub fn template_reg(at: usize, width: char) -> String {
    format!("\u{1}r{at}{width}\u{2}")
}

/// A label an `asm goto` template jumps to, held the same way. What it says is the arm of the
/// instruction's block the label is, counted the way the block's successors are, so the fall
/// through is arm 0 and `%l` of the first label is arm 1. The block that arm goes to only has a
/// name once the layout has numbered the blocks, which is after the text is written down. See
/// [`TEMPLATE_MEM`].
#[must_use]
pub fn template_arm(arm: usize) -> String {
    format!("\u{1}l{arm}\u{2}")
}

/// A kept template's text with the labels in it filled by `arm`, which is handed the arm
/// [`template_arm`] kept and says the name of the block it goes to, and every other hole left for
/// [`template_filled`]. `None` when a label names an arm `arm` has no name for.
#[must_use]
pub fn template_arms(text: &str, arm: impl Fn(usize) -> Option<String>) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("\u{1}l") {
        out.push_str(&rest[..at]);
        let hole = &rest[at + 2..];
        let end = hole.find('\u{2}').unwrap_or(hole.len());
        out.push_str(&arm(hole[..end].parse().ok()?)?);
        rest = hole.get(end + 1..).unwrap_or("");
    }
    out.push_str(rest);
    Some(out)
}

/// Whether a kept template jumps to a label, which is an `asm goto` with instructions in it.
#[must_use]
pub fn template_jumps(text: &str) -> bool {
    text.contains("\u{1}l")
}

/// A kept template's text with its holes filled: the address by `mem`, each name by `name`, and
/// each register by `reg`, which is handed the operand and the width letter [`template_reg`] kept.
#[must_use]
pub fn template_filled(
    text: &str,
    mem: &str,
    name: impl Fn(&str) -> String,
    reg: impl Fn(usize, char) -> String,
) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('\u{1}') {
        out.push_str(&rest[..at]);
        let hole = &rest[at + 1..];
        let end = hole.find('\u{2}').unwrap_or(hole.len());
        match hole[..end].split_at_checked(1) {
            // An address further on is the same address with the distance in front, which the
            // assembler adds to whatever displacement is there already.
            Some(("m", offset)) => match offset.parse::<i64>() {
                Ok(offset) if mem.starts_with('(') => out.push_str(&format!("{offset}{mem}")),
                Ok(offset) => out.push_str(&format!("{offset}+{mem}")),
                Err(_) => out.push_str(mem),
            },
            Some(("n", named)) => out.push_str(&name(named)),
            Some(("r", held)) => {
                let (at, width) = held.split_at(held.len().saturating_sub(1));
                if let (Ok(at), Some(width)) = (at.parse(), width.chars().next()) {
                    out.push_str(&reg(at, width));
                }
            }
            _ => {}
        }
        rest = hole.get(end + 1..).unwrap_or("");
    }
    out.push_str(rest);
    out
}
