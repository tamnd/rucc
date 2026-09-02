//! What a malformed rule file is told.

use std::fmt;

/// A place in a rule file and what is wrong there.
///
/// The compiler's own diagnostics carry a code, a caret and a source map, and none of that is
/// wanted here. A rule file is read by `cargo xtask rules` and its errors are read by whoever
/// is editing the rules, in a build log, so the shape that belongs here is the one every other
/// build tool prints: the file, the position, and one sentence. Spending a number out of the
/// compiler's diagnostic sequence on it would also be wrong, because those numbers are part of
/// what a user of the compiler can look up, and nobody compiling a C program will ever see one
/// of these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    /// The file the rule was read from, as it should be printed.
    pub path: String,
    /// The line, counted from one.
    pub line: u32,
    /// The column, counted from one, in characters.
    pub column: u32,
    /// What is wrong, as a sentence with no trailing full stop.
    pub message: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}: {}", self.path, self.line, self.column, self.message)
    }
}

impl std::error::Error for Error {}
