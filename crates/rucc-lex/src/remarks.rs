//! What a constant or a literal did that somebody might want to hear about.
//!
//! Design: `spec/06-lexer-and-parser.md` section 6.1.
//!
//! The conversions in [`crate::number`] and [`crate::literal`] never decide that something is a
//! warning. They convert what they were given, in the dialect they were told, and report what
//! the constant did along with the value, because the caller is the one holding the span and
//! the flags that say whether `-pedantic` is on and whether warnings are errors. A conversion
//! that decided for itself would have to be told about every warning flag in the driver.

/// What a constant does that the dialect being compiled has an opinion about, or that happened
/// to it on the way to a value.
///
/// A bitmask rather than a list, because a constant may earn several and a `Vec` per constant
/// on a file full of them is a cost with nothing to show for it. Every one of these is legal
/// in the dialect this compiler defaults to, so none of them is an error here: the caller
/// decides what `-pedantic`, `-Woverflow` and `-Werror` make of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Remarks(u32);

impl Remarks {
    /// Nothing to say.
    pub const NONE: Remarks = Remarks(0);
    /// A `0b` constant before C23, where it is a GNU extension both compilers accept.
    pub const BINARY: Remarks = Remarks(1);
    /// A digit separator before C23, which neither compiler accepts there.
    pub const SEPARATORS: Remarks = Remarks(2);
    /// A `wb` suffix before C23.
    pub const BIT_INT: Remarks = Remarks(4);
    /// An `ll` suffix under `-std=c89`, where GCC says "use of C99 long long integer constant".
    pub const LONG_LONG: Remarks = Remarks(8);
    /// A decimal constant with no `u` suffix that fits no signed type, so it became an unsigned
    /// one. GCC says "integer constant is so large that it is unsigned", and it is worth saying
    /// because the constant's arithmetic is now unsigned and its negation is not negative.
    pub const UNSIGNED: Remarks = Remarks(16);
    /// A hexadecimal floating constant before C99, where GCC says "use of C99 hexadecimal
    /// floating constant".
    pub const HEX_FLOAT: Remarks = Remarks(32);
    /// A suffix that names a type ISO C does not have: `q`, `w`, or one of the `_FloatN` and
    /// `_FloatNx` ones. GCC says "non-standard suffix on floating constant", and separately
    /// that ISO C does not support the type.
    pub const EXTENDED_SUFFIX: Remarks = Remarks(64);
    /// A `d` suffix, which is a `double` written the long way. GCC gives this one its own
    /// wording, "suffix for double constant is a GCC extension", and gives it in every dialect
    /// rather than only under `-pedantic`.
    pub const DOUBLE_SUFFIX: Remarks = Remarks(128);
    /// An `i` or `j` suffix. GCC says "imaginary constants are a GCC extension", in every
    /// dialect, because no version of C has a spelling for one.
    pub const IMAGINARY: Remarks = Remarks(256);
    /// A value too large for its type, which became an infinity. GCC says "floating constant
    /// exceeds range of 'double'" and names the type.
    pub const OUT_OF_RANGE: Remarks = Remarks(512);
    /// A nonzero value too small for its type, which became a zero. GCC says "floating constant
    /// truncated to zero", and it is worth saying because the program now divides by zero where
    /// it meant to divide by something very small.
    pub const TRUNCATED: Remarks = Remarks(1024);
    /// A character constant holding more than one character, whose value GCC builds by shifting
    /// them together and which the standard leaves implementation defined. GCC says
    /// "multi-character character constant".
    pub const MULTICHARACTER: Remarks = Remarks(2048);
    /// A character constant holding more characters than its type has room for, so the ones at
    /// the front are gone. GCC says "character constant too long for its type", and says it
    /// instead of the multi-character remark rather than as well as it.
    pub const TOO_LONG: Remarks = Remarks(4096);
    /// An escape whose letter means nothing, which is the letter itself and a warning in both
    /// compilers. GCC says "unknown escape sequence".
    pub const UNKNOWN_ESCAPE: Remarks = Remarks(8192);
    /// `\e`, the escape character, which both compilers have and no standard does. GCC says
    /// "non-ISO-standard escape sequence".
    pub const NON_ISO_ESCAPE: Remarks = Remarks(16384);
    /// A `\x` escape whose value does not fit the element it is written in, so it was truncated.
    /// GCC says "hex escape sequence out of range".
    pub const HEX_ESCAPE_OUT_OF_RANGE: Remarks = Remarks(32768);
    /// An octal escape whose value does not fit the element it is written in. GCC gives this its
    /// own wording, "octal escape sequence out of range", which is why it is its own flag.
    pub const OCTAL_ESCAPE_OUT_OF_RANGE: Remarks = Remarks(65536);
    /// A universal character name before C99, where GCC says "universal character names are
    /// only valid in C++ and C99" and converts it anyway.
    pub const UCN: Remarks = Remarks(131_072);

    /// Whether every remark in `other` is set here.
    #[inline]
    #[must_use]
    pub const fn has(self, other: Remarks) -> bool {
        self.0 & other.0 == other.0
    }

    /// This set with `other` added.
    #[inline]
    #[must_use]
    pub const fn with(self, other: Remarks) -> Remarks {
        Remarks(self.0 | other.0)
    }

    /// Whether there is nothing to say.
    #[inline]
    #[must_use]
    pub const fn is_none(self) -> bool {
        self.0 == 0
    }
}
