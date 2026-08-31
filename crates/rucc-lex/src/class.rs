//! The first-byte dispatch table.
//!
//! Design: `spec/05-preprocessor.md` section 5.2.
//!
//! "What kind of token starts here" is the single most executed decision in the compiler. A
//! `match` on the byte compiles to a chain of comparisons and range checks, in an order the
//! optimiser picks rather than one that matches how often each class actually occurs. A
//! 256-entry table turns the whole decision into one load and one jump, and the table is
//! built at compile time so it costs nothing at startup.

/// What class of pp-token a byte can begin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub(crate) enum Class {
    /// Space, tab, vertical tab, form feed. Not the newline, which is separate because the
    /// preprocessor is line oriented and needs to see it.
    Space,
    /// A newline.
    Newline,
    /// A letter, an underscore, a dollar sign, or a byte above ASCII.
    IdentStart,
    /// A decimal digit.
    Digit,
    /// A period, which starts a pp-number when a digit follows and is a punctuator otherwise.
    Dot,
    /// A double quote.
    Quote,
    /// A single quote.
    Apostrophe,
    /// A slash, which may start a comment.
    Slash,
    /// A backslash, which may start a universal character name.
    Backslash,
    /// Any other punctuator byte.
    Punct,
    /// A byte that begins nothing, such as a backtick or a null.
    Other,
}

/// Byte to class. One load replaces the comparison chain a `match` would compile to.
pub(crate) static CLASS: [Class; 256] = build();

const fn build() -> [Class; 256] {
    let mut t = [Class::Other; 256];
    let mut i = 0;
    while i < 256 {
        let b = i as u8;
        t[i] = match b {
            b' ' | b'\t' | 0x0B | 0x0C => Class::Space,
            b'\n' | b'\r' => Class::Newline,
            b'a'..=b'z' | b'A'..=b'Z' | b'_' | b'$' => Class::IdentStart,
            // Anything above ASCII is either the start of a UTF-8 identifier character or an
            // error, and telling those apart needs a decoder, so both go down the identifier
            // path and it decides. `spec/05-preprocessor.md` section 5.2 is explicit that
            // UTF-8 is validated only inside identifiers, literals and comments.
            0x80..=0xFF => Class::IdentStart,
            b'0'..=b'9' => Class::Digit,
            b'.' => Class::Dot,
            b'"' => Class::Quote,
            b'\'' => Class::Apostrophe,
            b'/' => Class::Slash,
            b'\\' => Class::Backslash,
            b'[' | b']' | b'(' | b')' | b'{' | b'}' | b'-' | b'+' | b'&' | b'*' | b'~' | b'!'
            | b'%' | b'<' | b'>' | b'=' | b'^' | b'|' | b'?' | b':' | b';' | b',' | b'#' => {
                Class::Punct
            }
            _ => Class::Other,
        };
        i += 1;
    }
    t
}

/// Whether a byte can continue an identifier.
#[inline]
pub(crate) const fn is_ident_continue(b: u8) -> bool {
    matches!(CLASS[b as usize], Class::IdentStart | Class::Digit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_agrees_with_what_c_says_about_each_byte() {
        assert_eq!(CLASS[b'i' as usize], Class::IdentStart);
        assert_eq!(CLASS[b'_' as usize], Class::IdentStart);
        // GNU allows `$` in identifiers and enough real code uses it that rejecting it here
        // would fail on inputs GCC accepts. `spec/13-gnu-extensions.md` carries the flag that
        // turns it off.
        assert_eq!(CLASS[b'$' as usize], Class::IdentStart);
        assert_eq!(CLASS[0xC3], Class::IdentStart);
        assert_eq!(CLASS[b'7' as usize], Class::Digit);
        assert_eq!(CLASS[b' ' as usize], Class::Space);
        assert_eq!(CLASS[b'\n' as usize], Class::Newline);
        assert_eq!(CLASS[b'`' as usize], Class::Other);
        assert_eq!(CLASS[0], Class::Other);
        assert_eq!(CLASS[b'@' as usize], Class::Other);
    }

    #[test]
    fn identifier_continuation_takes_digits_but_no_punctuation() {
        assert!(is_ident_continue(b'x'));
        assert!(is_ident_continue(b'0'));
        assert!(!is_ident_continue(b'-'));
        assert!(!is_ident_continue(b' '));
    }
}
