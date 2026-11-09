//! How much stack each function uses, for `-fstack-usage`.
//!
//! The number is [`crate::frame::Frame::usage`] and what it counts is said there. This file is
//! only somewhere to keep one row per function until the driver, which is the part that knows
//! what a file is, writes them out beside the object the way gcc does.
//!
//! A row keeps the span of the function's name rather than a line and a column, because the
//! source map that turns one into the other belongs to the driver's session and nothing in this
//! crate reads it.

use rucc_base::hash::Set;
use rucc_diag::Span;
use rucc_ir::{Block, Func, Inst, Opcode};

/// One function's stack use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Usage {
    /// What the source calls it, which is the symbol unless an assembler name renamed it.
    pub name: String,
    /// Where its name is written, which is [`rucc_ir::Func::named`].
    pub named: Span,
    /// Where its body is, which is [`rucc_ir::Func::declared`] and what is said instead when a
    /// function has no name span, as one built by something other than the C lowering has not.
    pub declared: Span,
    /// The bytes, as [`crate::frame::Frame::usage`] defines them, and with the most that the
    /// function takes while it runs added when that has a bound.
    pub bytes: u32,
    /// Whether the function moves the stack pointer while it runs, and if so whether by an amount
    /// there is a bound on.
    pub kind: Kind,
    /// The frame alone, as [`crate::frame::Frame::frame`] defines it, which is what
    /// `-Wframe-larger-than=` reads.
    pub frame: u32,
}

/// gcc's three answers to how much stack a function takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The frame is the whole answer.
    Static,
    /// The function takes more while it runs, and [`Usage::bytes`] counts the most it can take.
    ///
    /// A variable length array or an `alloca` whose size is a constant and which cannot add to
    /// itself, because no path from it back to itself misses the place its scope gives the stack
    /// back. gcc says the same of a variable length array whose size it knows where it expands
    /// it, and on x86-64 also of a function that pushes the arguments of a call. Such a function
    /// here has the room for them in its frame from the prologue on, so it is `Static`. See
    /// [`bound`] for the `alloca` gcc says less about.
    Bounded,
    /// The function takes more while it runs and nothing says how much, so [`Usage::bytes`] is a
    /// floor.
    Unbounded,
}

impl Usage {
    /// What gcc writes in the third column: `static`, `dynamic,bounded` or `dynamic`.
    #[must_use]
    pub fn qualifier(&self) -> &'static str {
        match self.kind {
            Kind::Static => "static",
            Kind::Bounded => "dynamic,bounded",
            Kind::Unbounded => "dynamic",
        }
    }

    /// This function's line of a `.su` file, with the newline that ends it.
    ///
    /// `file:line:column:function`, a tab, the bytes, a tab and the qualifier, which is what gcc 16
    /// writes and what a tool reading gcc's files splits on. The line and the column are where
    /// [`Usage::span`] is, counting from one, and the file is the name the source was opened
    /// under, so a function in a header is reported against the header, the same as gcc.
    #[must_use]
    pub fn line(&self, file: &str, line: u32, column: u32) -> String {
        format!("{file}:{line}:{column}:{}\t{}\t{}\n", self.name, self.bytes, self.qualifier())
    }

    /// The span the report points at, which is the name where there is one and the brace where
    /// there is not.
    #[must_use]
    pub fn span(&self) -> Span {
        if self.named.is_dummy() { self.declared } else { self.named }
    }
}

/// Every function a compilation gave a frame, in the order they came through.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct StackUsage {
    rows: Vec<Usage>,
}

impl StackUsage {
    /// Nothing recorded yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Writes down one function.
    pub fn record(&mut self, usage: Usage) {
        self.rows.push(usage);
    }

    /// The functions, in the order they were compiled, which is the order they are in the file.
    #[must_use]
    pub fn rows(&self) -> &[Usage] {
        &self.rows
    }
}

/// The most the function's variable length arrays and `alloca`s take while it runs, or nothing
/// when that has no bound.
///
/// Read off the IR once [`crate::expand::rounds`] has rounded each size up to what comes off the
/// stack pointer, so the number is the bytes the function takes and not the bytes it asked for.
/// An allocation has a bound when its size is a constant and it cannot add to itself: every path
/// from it back to itself passes a `stackrestore`, which is the end of the scope a variable length
/// array is declared in. An `alloca` in a loop is not given back until the function returns, so
/// it has no bound whatever its size. gcc gives up on an `alloca` that is not in a loop as well,
/// and here that has its size as the bound, which is the true answer and the one that warns less.
#[must_use]
pub fn bound(func: &Func) -> Option<u32> {
    let mut bytes: u32 = 0;
    for block in func.blocks() {
        for inst in func.insts(block) {
            if func[inst].opcode != Opcode::Alloca {
                continue;
            }
            let Some(&size) = func[func[inst].args].first() else { continue };
            let size = crate::divide::constant(func, size)?;
            if again(func, block, inst) {
                return None;
            }
            bytes = bytes.saturating_add(u32::try_from(size.unsigned()).ok()?);
        }
    }
    Some(bytes)
}

/// Whether control can come back to `inst` without passing a `stackrestore` on the way.
fn again(func: &Func, block: Block, inst: Inst) -> bool {
    // The rest of the block it is in, and then every block reached from there, each from its top.
    let mut seen = Set::default();
    let mut work = vec![(block, true)];
    while let Some((at, part)) = work.pop() {
        let mut reached = !part;
        let mut given_back = false;
        for next in func.insts(at) {
            if !reached {
                reached = next == inst;
                continue;
            }
            if next == inst {
                return true;
            }
            if func[next].opcode == Opcode::StackRestore {
                given_back = true;
                break;
            }
        }
        if given_back {
            continue;
        }
        let Some(term) = func.terminator(at) else { continue };
        for call in func.successors(term) {
            if seen.insert(call.block) {
                work.push((call.block, false));
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(kind: Kind, named: Span) -> Usage {
        Usage {
            name: "f".to_owned(),
            named,
            declared: Span::new(10, 11),
            bytes: 16,
            kind,
            frame: 0,
        }
    }

    #[test]
    fn the_qualifier_is_gcc_s_word_for_each_kind() {
        assert_eq!(row(Kind::Static, Span::new(1, 2)).qualifier(), "static");
        assert_eq!(row(Kind::Bounded, Span::new(1, 2)).qualifier(), "dynamic,bounded");
        assert_eq!(row(Kind::Unbounded, Span::new(1, 2)).qualifier(), "dynamic");
    }

    #[test]
    fn a_line_is_the_place_the_name_the_bytes_and_the_qualifier_split_by_tabs() {
        let line = row(Kind::Static, Span::new(1, 2)).line("src/a.c", 12, 5);
        assert_eq!(line, "src/a.c:12:5:f\t16\tstatic\n");
        let line = row(Kind::Unbounded, Span::new(1, 2)).line("inc/h.h", 1, 19);
        assert_eq!(line, "inc/h.h:1:19:f\t16\tdynamic\n");
    }

    #[test]
    fn the_report_points_at_the_name_and_falls_back_to_the_brace() {
        assert_eq!(row(Kind::Static, Span::new(1, 2)).span(), Span::new(1, 2));
        assert_eq!(row(Kind::Static, Span::DUMMY).span(), Span::new(10, 11));
    }
}
