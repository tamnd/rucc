//! `#pragma GCC target` as the preprocessor sees it, which is the extension macros following it.
//!
//! gcc defines `__AVX2__` from a `#pragma GCC target("avx2")` line on, and takes it away again at
//! the `pop_options` or `reset_options` that ends the line's reach, so code under the line may ask
//! `#ifdef __AVX2__` and get the answer the functions around it are built for. That is how gcc's
//! own intrinsic headers are written, and how a program that builds one file for several
//! processors picks its code. The lines still pass through to the parser, which gives the
//! functions after them the attribute, and only the macros are this phase's business.
//!
//! Only a line gcc reads cleanly moves the macros, since gcc ignores the others, and so does one
//! naming an extension it does not know. Only x86-64 has macros to move: AArch64's strings name
//! extensions with macros of their own and change nothing here, as they change nothing in the
//! function either.

use rucc_target::{Isa, Target};

/// What one `#pragma GCC` line about options asks of the macros.
#[derive(Debug)]
pub(crate) enum OptionsLine {
    /// `push_options`.
    Push,
    /// `pop_options`.
    Pop,
    /// `reset_options`.
    Reset,
    /// `target(...)`, with its strings.
    Target(Vec<String>),
}

/// The `target` lines in effect, and what `push_options` saved.
#[derive(Debug, Default)]
pub(crate) struct TargetLines {
    /// What the unit is built for, on x86-64, and nothing on a target the lines move no macro on.
    unit: Option<Isa>,
    /// The strings of the lines in effect, in the order they were written.
    lines: Vec<String>,
    /// What `push_options` saved, innermost last.
    saved: Vec<Vec<String>>,
}

impl TargetLines {
    /// No lines yet, over what the unit is built for.
    pub(crate) fn new(unit: Option<Isa>) -> TargetLines {
        TargetLines { unit, lines: Vec::new(), saved: Vec::new() }
    }

    /// Applies one line, and answers what the unit's functions were built for before it and are
    /// built for after it, for the caller to move the macros by, when there is a unit to say it of.
    pub(crate) fn apply(&mut self, line: OptionsLine) -> Option<(Isa, Isa)> {
        let unit = self.unit?;
        let before = over(unit, &self.lines)?;
        match line {
            OptionsLine::Push => self.saved.push(self.lines.clone()),
            OptionsLine::Pop => self.lines = self.saved.pop()?,
            OptionsLine::Reset => self.lines.clear(),
            OptionsLine::Target(strings) => {
                let mut lines = self.lines.clone();
                lines.extend(strings);
                over(unit, &lines)?;
                self.lines = lines;
            }
        }
        Some((before, over(unit, &self.lines)?))
    }
}

/// What `lines` build a function for over the unit's extensions, or nothing when one of them
/// names something gcc does not know.
fn over(unit: Isa, lines: &[String]) -> Option<Isa> {
    let mut target = Target::new();
    for line in lines {
        target.read(line).ok()?;
    }
    Some(target.over(unit))
}
