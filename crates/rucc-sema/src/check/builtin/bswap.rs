//! `__builtin_bswap16`, `__builtin_bswap32` and `__builtin_bswap64`, which reverse the bytes of a
//! value.
//!
//! Design: `spec/13-gnu-compat.md` section 13.5.
//!
//! A program reads a file format or a network packet, the bytes in it are in an order the machine
//! does not use, and it says so. Every C compiler has these because the alternative a program falls
//! back to is a shift and a mask per byte, which is the same instructions written by hand and
//! spread over a line the reader has to check. SQLite writes them for its page headers, glibc's
//! `<endian.h>` defines `htobe32` and its neighbours as exactly these, and the kernel's byte order
//! header is built on them.
//!
//! # Why the name decides this and not the declaration
//!
//! There is no plain name to be confused with. `bswap32` is not a function the C library promises,
//! so unlike the absolute value family next door there is no case where the program means its own
//! thing by the name: the only spellings are the prefixed ones, and the prefix is what says the
//! name belongs to the implementation. A program that declares one itself has redeclared something
//! it does not own.
//!
//! # Why the answer is taken after the call is checked
//!
//! Same reason `check/builtin/expect.rs` gives. Each of the three has a signature in
//! `features.toml`, so the ordinary call checking is what reports the argument count and what
//! converts the argument to the unsigned type of the right width. `__builtin_bswap32((char)1)` is a
//! swap of `1u` and not of a `char`, because the prototype converted it, and writing that
//! conversion again here would gain nothing.
//!
//! # What the width means
//!
//! The bytes are reversed in the width of the type and not in the width of the value. That sounds
//! like the same sentence and is not: `__builtin_bswap16` on a machine with no sixteen bit
//! arithmetic is still the two bytes of a `uint16_t` swapped, and the bits above them are not part
//! of the value at all. Carrying the width on the node is what keeps the walk to the IR from having
//! to decide that, since the type of the node is the type of the answer.

use rucc_base::Symbol;
use rucc_diag::Span;

use crate::check::Checker;
use crate::expr::{Category, Expr, ExprId, ExprKind};

/// The three names, which differ only in how wide the value is.
///
/// Each is a row of `features.toml` with a signature, which is what makes them ordinary calls up to
/// the point this replaces them, and the test at the bottom of this file is what keeps the two
/// lists from drifting apart.
const FAMILY: &[&str] = &["__builtin_bswap16", "__builtin_bswap32", "__builtin_bswap64"];

impl Checker<'_> {
    /// The reversal a call to one of the byte swap builtins is, if the call is one.
    ///
    /// Answers nothing for every other call in the program, which is nearly every call, so the test
    /// that costs a byte goes first.
    pub(in crate::check) fn bswap_builtin_value(
        &mut self,
        function: Option<Symbol>,
        args: &[ExprId],
        span: Span,
    ) -> Option<ExprId> {
        let name = function?;
        let spelled = self.text(name);
        if !spelled.starts_with("__builtin_bswap") || !FAMILY.contains(&spelled) {
            return None;
        }
        // Nothing to reverse, which is the call written with no arguments. The count has been
        // reported by this point, so the ordinary call node is built and the program is refused for
        // the reason it was already going to be refused for.
        let &operand = args.first()?;
        if self.is_poisoned(operand) {
            return Some(self.poison(span));
        }
        // The type of the answer is the type of the operand, which the prototype has already
        // converted to the unsigned type of the width the name says.
        let ty = self.tast[operand].ty;
        Some(self.tast.expr(Expr::new(ExprKind::ByteSwap { operand }, ty, Category::Rvalue), span))
    }
}

#[cfg(test)]
mod tests {
    use rucc_gnu::{Kind, Status};

    use super::*;

    /// Every name here has to be a row of the table carrying a signature, because the signature is
    /// what the call is checked against before this replaces it. A row without one would never be
    /// declared and the call would be to an undeclared name.
    #[test]
    fn every_name_in_the_family_is_a_row_of_the_table_that_carries_a_signature() {
        for &name in FAMILY {
            let Some(feature) = rucc_gnu::lookup(Kind::Builtin, name) else {
                panic!("{name} is answered here and is not in features.toml");
            };
            assert_eq!(feature.status, Status::Implemented, "{name}");
            assert!(!feature.signature.is_empty(), "{name} is checked against its prototype");
            assert!(feature.library.is_empty(), "{name} is not a call to anything");
        }
    }

    /// The parameter and the result are the same unsigned type, and it is the width the name says.
    /// A signature that widened either one would make the swap happen at a width the program did
    /// not ask for.
    #[test]
    fn each_name_takes_and_gives_back_the_unsigned_type_of_the_width_it_names() {
        for &name in FAMILY {
            let bits = name.trim_start_matches(|c: char| !c.is_ascii_digit());
            let written = format!("uint{bits}_t(uint{bits}_t)");
            let feature = rucc_gnu::lookup(Kind::Builtin, name).expect("a row");
            assert_eq!(feature.signature, written, "{name}");
        }
    }
}
