//! How much stack each function uses, for `-fstack-usage`.
//!
//! The number is [`crate::frame::Frame::usage`] and what it counts is said there. This file is
//! only somewhere to keep one row per function until the driver, which is the part that knows
//! what a file is, writes them out beside the object the way gcc does.
//!
//! A row keeps the span of the function's name rather than a line and a column, because the
//! source map that turns one into the other belongs to the driver's session and nothing in this
//! crate reads it.

use rucc_diag::Span;

/// One function's stack use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Usage {
    /// What the source calls it, which is the symbol unless an assembler name renamed it.
    pub name: String,
    /// Where its name is written, which is [`rucc_ir::Func::named`].
    pub named: Span,
    /// Where its body opens, which is [`rucc_ir::Func::declared`] and what is said instead when a
    /// function has no name span, as one built by something other than the C lowering has not.
    pub declared: Span,
    /// The bytes, as [`crate::frame::Frame::usage`] defines them.
    pub bytes: u32,
    /// Whether the function moves the stack pointer while it runs, which a variable length array
    /// and an `alloca` both do, and which makes `bytes` a floor rather than the whole answer.
    pub dynamic: bool,
}

impl Usage {
    /// What gcc writes in the third column, which is `static` when the number is the whole answer
    /// and `dynamic` when the function takes more while it runs.
    ///
    /// gcc has a third answer, `dynamic,bounded`, for a function that moves the stack pointer by
    /// an amount it can put a bound on, and on x86-64 the usual one is a function that pushes the
    /// arguments of a call that passes some on the stack. Such a function here has the room for
    /// them in its frame from the prologue on, which [`crate::frame::Frame::usage`] counts
    /// already, so the same bytes come out as `static`. Nothing else here puts a bound on how far
    /// a frame grows, so this answer is never given.
    #[must_use]
    pub fn qualifier(&self) -> &'static str {
        if self.dynamic { "dynamic" } else { "static" }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn row(dynamic: bool, named: Span) -> Usage {
        Usage { name: "f".to_owned(), named, declared: Span::new(10, 11), bytes: 16, dynamic }
    }

    #[test]
    fn a_function_that_grows_is_dynamic_and_every_other_one_is_static() {
        assert_eq!(row(false, Span::new(1, 2)).qualifier(), "static");
        assert_eq!(row(true, Span::new(1, 2)).qualifier(), "dynamic");
    }

    #[test]
    fn a_line_is_the_place_the_name_the_bytes_and_the_qualifier_split_by_tabs() {
        let line = row(false, Span::new(1, 2)).line("src/a.c", 12, 5);
        assert_eq!(line, "src/a.c:12:5:f\t16\tstatic\n");
        let line = row(true, Span::new(1, 2)).line("inc/h.h", 1, 19);
        assert_eq!(line, "inc/h.h:1:19:f\t16\tdynamic\n");
    }

    #[test]
    fn the_report_points_at_the_name_and_falls_back_to_the_brace() {
        assert_eq!(row(false, Span::new(1, 2)).span(), Span::new(1, 2));
        assert_eq!(row(false, Span::DUMMY).span(), Span::new(10, 11));
    }
}
