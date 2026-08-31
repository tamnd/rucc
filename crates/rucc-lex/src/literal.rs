//! Character constants and string literals: the escapes, the encoding prefixes, and what the
//! elements end up being.
//!
//! Design: `spec/06-lexer-and-parser.md` section 6.1.
//!
//! This is the last piece of phase 7 that is about spellings. A literal arrives here as the
//! bytes the user wrote, prefix and quotes included, and leaves as elements: one value per
//! element of the array a string is, or one number for the character constant. What an element
//! is depends on the prefix and on the target, which is most of the work.
//!
//! The execution character set is UTF-8 and the wide one is UTF-32, or UTF-16 where `wchar_t`
//! is sixteen bits, which is Windows. That is what both compilers default to and it is the only
//! choice that makes a UTF-8 source file mean what it looks like. `-fexec-charset` is not
//! implemented and would change these answers if it ever is.
//!
//! # What an escape is worth
//!
//! There are two kinds of escape and the difference matters more than it looks. `é` names
//! a character, so it is encoded in whatever the literal's encoding is, and in a plain string it
//! becomes the two bytes `c3 a9`. `\xe9` is a value, not a character, so it is that one element
//! and nothing encodes it. gcc 13.3 agrees on both: `"é"` is three bytes long and
//! `"\xe9"` is two.
//!
//! A value escape that does not fit its element is truncated with a warning, and the element is
//! what decides, not the type: `u8'\xff'` is fine and `'\x1ff'` is not, and both are eight bits
//! wide. Octal runs to three digits and stops, so `"\1234"` is `S4` and not one escape, while
//! hexadecimal runs as far as there are hexadecimal digits.
//!
//! An escape whose letter means nothing is that letter, with a warning, so `'\q'` is `'q'`.
//! `\e` is the escape character in both compilers and in no standard. `\N{NAME}` is refused,
//! because gcc 13.3 has it in C++23 alone and inventing our own answer would be worse.
//!
//! # Universal character names
//!
//! A UCN may not name a character in the basic character set, so a UCN that spells out the
//! letter `A` is an error even though `'A'` is a constant. The exception is the three characters
//! `$`, `@` and the backquote, which are below a space in the table and allowed anyway.
//! Surrogates are refused. gcc still reports the basic character case in C23, where the standard
//! relaxed it, so this follows gcc and not the paper. Before C99 a UCN is converted with a remark
//! rather than refused, which is also what gcc does.
//!
//! A code point above `\U0010ffff` is an error here and a warning in gcc, which then encodes
//! the value as though UTF-8 went that far. clang refuses it, this refuses it, and it is the
//! one deliberate difference in this module.
//!
//! # Character constants
//!
//! A plain character constant is an `int` and not a `char`, and its value is the character
//! converted to `int`, so `'\xff'` is minus one where plain `char` is signed and 255 where it
//! is not. More than one character is implementation defined and both compilers shift them
//! together, so `'ab'` is `0x6162`, with a warning. Past the width of the type the ones at the
//! front fall off, so `'abcde'` is `0x62636465`, and gcc says "too long" there instead of
//! "multi-character" rather than as well as it. A wide constant has room for exactly one, so
//! `L'ab'` is `'b'` with the same warning, and a `u8` constant with two characters is an error
//! rather than a warning. All measured on gcc 13.3, x86-64 Linux.
//!
//! # The prefixes and the dialects
//!
//! `L` is C89, `u` and `U` are C11, `u8` on a string is C11 and `u8` on a character constant is
//! C23. In an older dialect gcc lexes `u8'a'` as the identifier `u8` followed by a character
//! constant, which is a different token stream rather than a different constant. The scanner
//! here reads it as one token in every dialect, which is the simpler rule and a divergence in
//! the token stream that no real program can see, so the dialect is checked at this point
//! instead and the constant is refused with a message that names the dialect it needs.

use rucc_session::Std;
use rucc_target::TargetInfo;

use crate::remarks::Remarks;

/// The encoding prefix of a character constant or a string literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    /// No prefix, whose element is a `char`.
    Plain,
    /// `L`, whose element is a `wchar_t` and so is a fact about the target.
    Wide,
    /// `u8`, whose element is a `char8_t`, which is an `unsigned char`.
    Utf8,
    /// `u`, whose element is a `char16_t`, which is a `uint_least16_t`.
    Utf16,
    /// `U`, whose element is a `char32_t`, which is a `uint_least32_t`.
    Utf32,
}

impl Encoding {
    /// The width of one element in bits.
    #[must_use]
    pub fn element_width(self, target: &TargetInfo) -> u32 {
        match self {
            Encoding::Plain | Encoding::Utf8 => 8,
            Encoding::Wide => target.wchar_width,
            Encoding::Utf16 => 16,
            Encoding::Utf32 => 32,
        }
    }

    /// Whether the element type is signed, which only `char` and `wchar_t` can be.
    #[must_use]
    pub fn is_signed(self, target: &TargetInfo) -> bool {
        match self {
            Encoding::Plain => target.char_is_signed,
            Encoding::Wide => target.wchar_is_signed,
            Encoding::Utf8 | Encoding::Utf16 | Encoding::Utf32 => false,
        }
    }

    /// The prefix this encoding is written with, which is empty for a plain literal.
    #[must_use]
    pub const fn prefix(self) -> &'static str {
        match self {
            Encoding::Plain => "",
            Encoding::Wide => "L",
            Encoding::Utf8 => "u8",
            Encoding::Utf16 => "u",
            Encoding::Utf32 => "U",
        }
    }

    /// The prefix a spelling was written with, for a caller that needs the encoding of a
    /// literal it could not convert.
    #[must_use]
    pub fn read_prefix(text: &str) -> Encoding {
        Encoding::read(text.as_bytes()).0
    }

    /// The prefix at the front of a spelling, and how many bytes it took.
    fn read(bytes: &[u8]) -> (Encoding, usize) {
        match bytes {
            [b'u', b'8', ..] => (Encoding::Utf8, 2),
            [b'u', ..] => (Encoding::Utf16, 1),
            [b'U', ..] => (Encoding::Utf32, 1),
            [b'L', ..] => (Encoding::Wide, 1),
            _ => (Encoding::Plain, 0),
        }
    }

    /// The first dialect that has this prefix, which is not the same for a character constant
    /// as for a string literal.
    fn since(self, character: bool) -> Std {
        match self {
            Encoding::Plain | Encoding::Wide => Std::C89,
            Encoding::Utf8 if character => Std::C23,
            Encoding::Utf8 | Encoding::Utf16 | Encoding::Utf32 => Std::C11,
        }
    }
}

/// A converted character constant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CharConstant {
    /// The value, already converted to the constant's type, which is why it can be negative:
    /// `'\xff'` is minus one where plain `char` is signed.
    pub value: i64,
    /// The prefix it was written with.
    pub encoding: Encoding,
    /// What is worth saying about it, for the caller that holds the span.
    pub remarks: Remarks,
}

/// A converted string literal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StringLiteral {
    /// One value per element of the array, without the terminating zero, since the zero belongs
    /// to the type and not to the spelling. An element is a byte in a plain or `u8` literal, a
    /// UTF-16 code unit in a `u` one, and a code point in a `U` one.
    pub elements: Vec<u32>,
    /// The prefix it was written with.
    pub encoding: Encoding,
    /// What is worth saying about it, for the caller that holds the span.
    pub remarks: Remarks,
}

impl StringLiteral {
    /// The bytes this literal becomes in the object, terminator included, in the target's byte
    /// order.
    #[must_use]
    pub fn bytes(&self, target: &TargetInfo) -> Vec<u8> {
        let width = self.encoding.element_width(target) / 8;
        let mut bytes = Vec::with_capacity((self.elements.len() + 1) * width as usize);
        for element in self.elements.iter().copied().chain([0]) {
            let taken = &element.to_le_bytes()[..width as usize];
            if target.little_endian {
                bytes.extend_from_slice(taken);
            } else {
                bytes.extend(taken.iter().rev());
            }
        }
        bytes
    }
}

/// Why a spelling is not a literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiteralError {
    /// The spelling is not a character constant or a string literal at all, which the caller
    /// cannot reach from a token the scanner produced.
    NotALiteral,
    /// `''`, which has no character in it to have a value.
    Empty,
    /// More characters than the type has room for, in the one encoding where that is an error
    /// rather than a warning.
    TooLong,
    /// `\x` with no hexadecimal digit after it.
    NoHexDigits,
    /// A universal character name that stops before its digits are done.
    IncompleteUcn,
    /// A universal character name that may not name what it names: a character in the basic
    /// character set, a surrogate, or a code point past the end of Unicode.
    InvalidUcn,
    /// `\N{NAME}`, which gcc 13.3 has in C++23 and in no C dialect.
    NamedUcn,
    /// A byte in the source that is not part of a character, in a literal whose encoding has to
    /// know what the characters are.
    InvalidUtf8,
    /// An encoding prefix the dialect does not have.
    PrefixNotInDialect,
    /// A run of adjacent string literals written with two different prefixes, which neither
    /// compiler has an answer for.
    MixedEncodings,
}

impl LiteralError {
    /// What to print, in GCC's words where GCC has any.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            LiteralError::NotALiteral => "not a character constant or a string literal",
            LiteralError::Empty => "empty character constant",
            LiteralError::TooLong => "character constant too long for its type",
            LiteralError::NoHexDigits => "\\x used with no following hex digits",
            LiteralError::IncompleteUcn => "incomplete universal character name",
            LiteralError::InvalidUcn => "not a valid universal character",
            LiteralError::NamedUcn => "named universal character escapes are not supported yet",
            LiteralError::InvalidUtf8 => "failure to convert the source to the execution charset",
            LiteralError::PrefixNotInDialect => {
                "this encoding prefix is not available in this dialect"
            }
            LiteralError::MixedEncodings => {
                "unsupported non-standard concatenation of string literals"
            }
        }
    }
}

/// Converts the spelling of a character constant into a value.
///
/// # Errors
///
/// [`LiteralError`], for a spelling that is not a character constant or that holds an escape
/// that is not one.
pub fn character(text: &str, std: Std, target: &TargetInfo) -> Result<CharConstant, LiteralError> {
    let (encoding, body) = open(text, b'\'', std, true)?;
    let width = encoding.element_width(target);
    let mut reader = Reader { bytes: body, index: 0, std, remarks: Remarks::NONE };

    // The elements are shifted together into one number, which is what both compilers do with a
    // constant holding more than one, and the ones that fall off the top are the ones the
    // warning is about.
    let mut value: u64 = 0;
    let mut count = 0u32;
    while let Some(piece) = reader.next(width)? {
        for element in piece.elements(width) {
            value = (value << width) | u64::from(element);
            count += 1;
        }
    }
    let mut remarks = reader.remarks;

    // A plain constant is an `int` however many characters it holds, and every other kind is
    // exactly one element wide, so that is how many characters fit.
    let type_width = if encoding == Encoding::Plain { 32 } else { width };
    let capacity = type_width / width;
    match count {
        0 => return Err(LiteralError::Empty),
        1 => {}
        _ if encoding == Encoding::Utf8 => return Err(LiteralError::TooLong),
        _ if count > capacity => remarks = remarks.with(Remarks::TOO_LONG),
        _ => remarks = remarks.with(Remarks::MULTICHARACTER),
    }

    // One character is converted to the constant's type from the element's, which is where the
    // sign of a plain `char` gets in. More than one is already a number of the constant's type
    // and nothing sign extends it from any narrower width.
    let (bits, signed) = if count == 1 {
        (width, encoding.is_signed(target))
    } else {
        (type_width, encoding == Encoding::Plain || encoding.is_signed(target))
    };
    Ok(CharConstant { value: narrow(value, bits, signed), encoding, remarks })
}

/// Converts the spelling of a string literal into its elements.
///
/// # Errors
///
/// [`LiteralError`], for a spelling that is not a string literal or that holds an escape that
/// is not one.
pub fn string(text: &str, std: Std, target: &TargetInfo) -> Result<StringLiteral, LiteralError> {
    strings(std::slice::from_ref(&text), std, target)
}

/// Converts a run of adjacent string literals into the one literal they are.
///
/// The encoding of the result is the prefixed one when any of them is prefixed, so `L"a" "b"`
/// and `"a" L"b"` are both wide, and two different prefixes in one run is an error rather than
/// a choice. That is measured on gcc 13.3, which puts it exactly that way: "unsupported
/// non-standard concatenation of string literals".
///
/// The bodies are read in the encoding of the whole run rather than each in its own, which
/// matters for a character rather than an escape: the accented letter in `L"a" "e-acute"` is
/// one wide element and not the two bytes it would have been on its own.
///
/// # Errors
///
/// [`LiteralError`], for a spelling that is not a string literal, a run mixing two prefixes, or
/// an escape that is not one.
pub fn strings(
    texts: &[&str],
    std: Std,
    target: &TargetInfo,
) -> Result<StringLiteral, LiteralError> {
    let mut bodies = Vec::with_capacity(texts.len());
    let mut encoding = Encoding::Plain;
    for text in texts {
        let (found, body) = open(text, b'"', std, false)?;
        if found != Encoding::Plain {
            if encoding != Encoding::Plain && encoding != found {
                return Err(LiteralError::MixedEncodings);
            }
            encoding = found;
        }
        bodies.push(body);
    }

    let width = encoding.element_width(target);
    let mut elements = Vec::new();
    let mut remarks = Remarks::NONE;
    for body in bodies {
        let mut reader = Reader { bytes: body, index: 0, std, remarks: Remarks::NONE };
        while let Some(piece) = reader.next(width)? {
            elements.extend(piece.elements(width));
        }
        remarks = remarks.with(reader.remarks);
    }
    Ok(StringLiteral { elements, encoding, remarks })
}

/// Reads the prefix and the quotes, and hands back the encoding and what is between them.
fn open(
    text: &str,
    quote: u8,
    std: Std,
    character: bool,
) -> Result<(Encoding, &[u8]), LiteralError> {
    let bytes = text.as_bytes();
    let (encoding, prefix) = Encoding::read(bytes);
    if std < encoding.since(character) {
        return Err(LiteralError::PrefixNotInDialect);
    }
    let rest = &bytes[prefix..];
    match rest {
        [first, .., last] if *first == quote && *last == quote => {
            Ok((encoding, &rest[1..rest.len() - 1]))
        }
        _ => Err(LiteralError::NotALiteral),
    }
}

/// Cuts a value down to `bits` and reads it back as signed or unsigned.
fn narrow(value: u64, bits: u32, signed: bool) -> i64 {
    let masked = if bits >= 64 { value } else { value & ((1u64 << bits) - 1) };
    if signed && bits < 64 && masked >> (bits - 1) & 1 == 1 {
        // The bits above the width are the sign, which is what makes `'\xff'` minus one.
        (masked | !((1u64 << bits) - 1)) as i64
    } else {
        masked as i64
    }
}

/// One piece of a literal, before it is turned into elements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Piece {
    /// A character, which the encoding has to encode: a character from the source or one named
    /// by a universal character name.
    Char(u32),
    /// A value written as a value, with `\x` or with octal digits, which is one element as
    /// written and is not encoded.
    Value(u32),
}

impl Piece {
    /// The elements this piece becomes in an encoding of the given element width.
    fn elements(self, width: u32) -> Vec<u32> {
        let code = match self {
            Piece::Value(value) => return vec![value],
            Piece::Char(code) => code,
        };
        match width {
            8 => {
                let mut buffer = [0u8; 4];
                let text = char::from_u32(code)
                    .map(|character| character.encode_utf8(&mut buffer).len())
                    .unwrap_or(0);
                buffer[..text].iter().map(|&byte| u32::from(byte)).collect()
            }
            // UTF-16 is the one encoding where a character can take two elements, which is why
            // a wide string is not the same length on Windows as it is anywhere else.
            16 if code > 0xffff => {
                let value = code - 0x1_0000;
                vec![0xd800 + (value >> 10), 0xdc00 + (value & 0x3ff)]
            }
            _ => vec![code],
        }
    }
}

/// Reads the body of a literal one character at a time.
struct Reader<'a> {
    /// What is between the quotes.
    bytes: &'a [u8],
    /// How far in the reader is.
    index: usize,
    /// The dialect, which decides what is worth a remark.
    std: Std,
    /// What the literal has earned so far.
    remarks: Remarks,
}

impl Reader<'_> {
    /// The next piece, or [`None`] at the end of the literal.
    fn next(&mut self, width: u32) -> Result<Option<Piece>, LiteralError> {
        let Some(&byte) = self.bytes.get(self.index) else {
            return Ok(None);
        };
        self.index += 1;
        if byte == b'\\' {
            return self.escape(width).map(Some);
        }
        if byte < 0x80 {
            return Ok(Some(Piece::Char(u32::from(byte))));
        }
        // A byte above ASCII begins a character in the source, which is UTF-8. A narrow literal
        // is UTF-8 too, so its bytes go through untouched and nothing has to be able to decode
        // them; a wide one has to know which character this is before it can encode it again.
        if width == 8 {
            return Ok(Some(Piece::Value(u32::from(byte))));
        }
        // Only this one character is decoded, and not the rest of the literal, because what
        // comes after it may be an escape holding a byte that no character begins with.
        let length = utf8_length(byte).ok_or(LiteralError::InvalidUtf8)?;
        let end = self.index - 1 + length;
        let text = self
            .bytes
            .get(self.index - 1..end)
            .and_then(|slice| std::str::from_utf8(slice).ok())
            .ok_or(LiteralError::InvalidUtf8)?;
        let character = text.chars().next().ok_or(LiteralError::InvalidUtf8)?;
        self.index = end;
        Ok(Some(Piece::Char(character as u32)))
    }

    /// The piece an escape sequence is worth, with the backslash already read.
    fn escape(&mut self, width: u32) -> Result<Piece, LiteralError> {
        let Some(&byte) = self.bytes.get(self.index) else {
            // The scanner reports the missing quote, and there is nothing here to convert.
            return Err(LiteralError::NotALiteral);
        };
        self.index += 1;
        let simple = match byte {
            b'n' => Some(0x0a),
            b't' => Some(0x09),
            b'r' => Some(0x0d),
            b'a' => Some(0x07),
            b'b' => Some(0x08),
            b'f' => Some(0x0c),
            b'v' => Some(0x0b),
            b'\\' | b'\'' | b'"' | b'?' => Some(u32::from(byte)),
            _ => None,
        };
        if let Some(value) = simple {
            return Ok(Piece::Value(value));
        }
        match byte {
            // The escape character, which both compilers have and no standard does.
            b'e' | b'E' => {
                self.remarks = self.remarks.with(Remarks::NON_ISO_ESCAPE);
                Ok(Piece::Value(0x1b))
            }
            b'0'..=b'7' => Ok(Piece::Value(self.octal(byte, width))),
            b'x' => self.hex(width).map(Piece::Value),
            b'u' | b'U' => self.ucn(byte).map(Piece::Char),
            b'N' => Err(LiteralError::NamedUcn),
            // An escape that means nothing is the character itself, which both compilers do
            // after a warning rather than refusing the program.
            _ => {
                self.remarks = self.remarks.with(Remarks::UNKNOWN_ESCAPE);
                Ok(Piece::Value(u32::from(byte)))
            }
        }
    }

    /// An octal escape, which is at most three digits however many follow, so that `"\1234"` is
    /// two characters.
    fn octal(&mut self, first: u8, width: u32) -> u32 {
        let mut value = u32::from(first - b'0');
        for _ in 0..2 {
            match self.bytes.get(self.index) {
                Some(&byte @ b'0'..=b'7') => {
                    value = value * 8 + u32::from(byte - b'0');
                    self.index += 1;
                }
                _ => break,
            }
        }
        self.fit(value, width, Remarks::OCTAL_ESCAPE_OUT_OF_RANGE)
    }

    /// A hexadecimal escape, which runs as far as there are hexadecimal digits.
    fn hex(&mut self, width: u32) -> Result<u32, LiteralError> {
        let mut value: u64 = 0;
        let mut digits = 0;
        while let Some(digit) = self.bytes.get(self.index).and_then(|&byte| hex_digit(byte)) {
            // A value far past the width is truncated anyway, so the accumulator stops growing
            // rather than overflowing, and the remark still gets made.
            value = value.saturating_mul(16).saturating_add(u64::from(digit));
            digits += 1;
            self.index += 1;
        }
        if digits == 0 {
            return Err(LiteralError::NoHexDigits);
        }
        Ok(self.fit(
            u32::try_from(value).unwrap_or(u32::MAX),
            width,
            Remarks::HEX_ESCAPE_OUT_OF_RANGE,
        ))
    }

    /// A universal character name, with its `u` or `U` already read.
    fn ucn(&mut self, marker: u8) -> Result<u32, LiteralError> {
        if self.bytes.get(self.index) == Some(&b'{') {
            return Err(LiteralError::NamedUcn);
        }
        let digits = if marker == b'u' { 4 } else { 8 };
        let mut value: u32 = 0;
        for _ in 0..digits {
            let Some(digit) = self.bytes.get(self.index).and_then(|&byte| hex_digit(byte)) else {
                return Err(LiteralError::IncompleteUcn);
            };
            value = value * 16 + digit;
            self.index += 1;
        }
        // The basic character set is off limits, and so is everything below ` ` except the
        // three characters the standard lets through. gcc still says so in C23, where the
        // wording was relaxed, so this follows the compiler rather than the paper.
        let allowed_low = matches!(value, 0x24 | 0x40 | 0x60);
        if (value < 0xa0 && !allowed_low) || (0xd800..=0xdfff).contains(&value) || value > 0x10ffff
        {
            return Err(LiteralError::InvalidUcn);
        }
        if self.std < Std::C99 {
            self.remarks = self.remarks.with(Remarks::UCN);
        }
        Ok(value)
    }

    /// Cuts an escape down to the element it is written in, and says so when that loses
    /// something. Which remark that is depends on how the escape was written, because GCC words
    /// the hexadecimal case and the octal one differently.
    fn fit(&mut self, value: u32, width: u32, out_of_range: Remarks) -> u32 {
        if width >= 32 {
            return value;
        }
        let mask = (1u32 << width) - 1;
        if value & !mask != 0 {
            self.remarks = self.remarks.with(out_of_range);
        }
        value & mask
    }
}

/// The value of a hexadecimal digit, and [`None`] when the byte is not one.
fn hex_digit(byte: u8) -> Option<u32> {
    char::from(byte).to_digit(16)
}

/// How many bytes the character starting with this one takes, and [`None`] when no character
/// starts with it.
fn utf8_length(byte: u8) -> Option<usize> {
    match byte {
        0x00..=0x7f => Some(1),
        0xc2..=0xdf => Some(2),
        0xe0..=0xef => Some(3),
        0xf0..=0xf4 => Some(4),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use rucc_target::Triple;

    use super::*;

    fn linux() -> TargetInfo {
        TargetInfo::new("x86_64-unknown-linux-gnu".parse::<Triple>().expect("a known triple"))
    }

    fn windows() -> TargetInfo {
        TargetInfo::new("x86_64-pc-windows-msvc".parse::<Triple>().expect("a known triple"))
    }

    fn arm() -> TargetInfo {
        TargetInfo::new("aarch64-unknown-linux-gnu".parse::<Triple>().expect("a known triple"))
    }

    /// The value of a character constant on x86-64 Linux, in C23.
    fn ch(text: &str) -> i64 {
        character(text, Std::C23, &linux()).expect("a character constant").value
    }

    /// The remarks a character constant earns on x86-64 Linux, in C23.
    fn ch_remarks(text: &str) -> Remarks {
        character(text, Std::C23, &linux()).expect("a character constant").remarks
    }

    /// What a character constant goes wrong with.
    fn ch_error(text: &str) -> LiteralError {
        character(text, Std::C23, &linux()).expect_err("not a character constant")
    }

    /// The elements of a string literal on x86-64 Linux, in C23.
    fn str_elements(text: &str) -> Vec<u32> {
        string(text, Std::C23, &linux()).expect("a string literal").elements
    }

    /// The bytes a string literal becomes on x86-64 Linux, terminator included.
    fn str_bytes(text: &str) -> Vec<u8> {
        string(text, Std::C23, &linux()).expect("a string literal").bytes(&linux())
    }

    #[test]
    fn the_ordinary_cases_are_the_characters_they_look_like() {
        assert_eq!(ch("'a'"), 0x61);
        assert_eq!(ch(r"'\n'"), 0x0a);
        assert_eq!(ch(r"'\0'"), 0);
        assert_eq!(ch(r"'\\'"), 0x5c);
        assert_eq!(ch(r"'\''"), 0x27);
        assert_eq!(ch(r#"'\"'"#), 0x22);
        assert_eq!(ch(r"'\?'"), 0x3f);
        assert_eq!(str_elements(r#""hi""#), vec![0x68, 0x69]);
    }

    /// A single character in a plain constant goes through plain `char` on the way to `int`,
    /// which is the whole reason `'\xff'` is a negative number on one target and a positive
    /// one on another.
    #[test]
    fn a_high_character_takes_the_sign_of_plain_char() {
        assert_eq!(ch(r"'\xff'"), -1);
        assert_eq!(ch(r"'\377'"), -1);
        assert_eq!(character(r"'\xff'", Std::C23, &arm()).expect("a constant").value, 255);
        // Not a `char`, so nothing sign extends it.
        assert_eq!(ch(r"u8'\xff'"), 255);
    }

    /// Measured on GCC 13.3, x86-64 Linux. A value escape is truncated to its element and the
    /// warning is about the truncation, not about the type. The two spellings get two remarks
    /// because GCC gives them two wordings.
    #[test]
    fn an_escape_too_big_for_its_element_is_truncated_and_says_so() {
        let out = character(r"'\x1ff'", Std::C23, &linux()).expect("a constant");
        assert_eq!(out.value, -1);
        assert!(out.remarks.has(Remarks::HEX_ESCAPE_OUT_OF_RANGE));
        let out = character(r"'\400'", Std::C23, &linux()).expect("a constant");
        assert_eq!(out.value, 0);
        assert!(out.remarks.has(Remarks::OCTAL_ESCAPE_OUT_OF_RANGE));
        assert!(!out.remarks.has(Remarks::HEX_ESCAPE_OUT_OF_RANGE));
        // Wide enough to hold it, so there is nothing to say.
        assert!(!ch_remarks(r"L'\x1ff'").has(Remarks::HEX_ESCAPE_OUT_OF_RANGE));
        assert_eq!(ch(r"L'\x1ff'"), 0x1ff);
    }

    /// Measured on GCC 13.3: a plain literal in a run takes the prefix of its neighbour, two
    /// different prefixes are an error, and the bodies are read in the encoding of the run, so
    /// a character in the plain part is one wide element rather than its UTF-8 bytes.
    #[test]
    fn adjacent_literals_agree_on_one_encoding_or_none_at_all() {
        let target = linux();
        let wide = strings(&[r#"L"a""#, r#""b""#], Std::C23, &target).expect("a string");
        assert_eq!(wide.encoding, Encoding::Wide);
        assert_eq!(wide.elements, vec![0x61, 0x62]);
        assert_eq!(wide.bytes(&target).len(), 12);
        let other_way = strings(&[r#""a""#, r#"L"b""#], Std::C23, &target).expect("a string");
        assert_eq!(other_way.encoding, Encoding::Wide);
        assert_eq!(other_way.bytes(&target).len(), 12);

        let u8_run = strings(&[r#"u8"a""#, r#""b""#], Std::C23, &target).expect("a string");
        assert_eq!(u8_run.encoding, Encoding::Utf8);
        assert_eq!(u8_run.bytes(&target).len(), 3);

        // The plain part is read as wide, so the accented letter is one element and not two.
        let mixed = strings(&[r#"L"a""#, r#""é""#], Std::C23, &target).expect("a string");
        assert_eq!(mixed.elements, vec![0x61, 0xe9]);

        for run in [[r#"u8"a""#, r#"u"b""#], [r#"u8"a""#, r#"L"b""#], [r#"u"a""#, r#"L"b""#]] {
            assert_eq!(
                strings(&run, Std::C23, &target).expect_err("two prefixes in one run"),
                LiteralError::MixedEncodings
            );
        }

        // A run of one is the same thing as the literal on its own.
        assert_eq!(
            strings(&[r#""hi""#], Std::C23, &target).expect("a string").elements,
            vec![0x68, 0x69]
        );
    }

    /// Also measured on GCC 13.3. The characters are shifted together, the ones past the width
    /// of the type fall off the front, and past that point GCC says "too long" instead of
    /// "multi-character" rather than as well as it.
    #[test]
    fn more_than_one_character_shifts_them_together() {
        assert_eq!(ch("'ab'"), 0x6162);
        assert_eq!(ch("'abc'"), 0x616263);
        assert_eq!(ch("'abcd'"), 0x61626364);
        assert_eq!(ch("'abcde'"), 0x62636465);
        assert_eq!(ch(r"'\xff\xfe'"), 0xfffe);
        assert_eq!(ch(r"'\xff\xff\xff\xff'"), -1);
        assert_eq!(ch(r"'\x80\x00'"), 0x8000);

        assert!(ch_remarks("'ab'").has(Remarks::MULTICHARACTER));
        assert!(ch_remarks("'abcd'").has(Remarks::MULTICHARACTER));
        assert!(ch_remarks("'abcde'").has(Remarks::TOO_LONG));
        assert!(!ch_remarks("'abcde'").has(Remarks::MULTICHARACTER));
        assert!(!ch_remarks("'a'").has(Remarks::MULTICHARACTER));
    }

    /// A wide constant has room for exactly one character, so two is already too many and the
    /// last one is what survives. `u8` is the one encoding where GCC makes this an error.
    #[test]
    fn a_prefixed_constant_holds_one_character_and_keeps_the_last() {
        for text in [r"L'ab'", r"u'ab'", r"U'ab'"] {
            let out = character(text, Std::C23, &linux()).expect("a constant");
            assert_eq!(out.value, 0x62, "{text}");
            assert!(out.remarks.has(Remarks::TOO_LONG), "{text}");
        }
        assert_eq!(ch_error("u8'ab'"), LiteralError::TooLong);
        assert_eq!(ch_error("u8'é'"), LiteralError::TooLong);
    }

    #[test]
    fn the_empty_constant_has_no_value_to_have() {
        assert_eq!(ch_error("''"), LiteralError::Empty);
        assert_eq!(ch_error("L''"), LiteralError::Empty);
        // The empty string is fine, and is one element long once the terminator is there.
        assert_eq!(str_elements(r#""""#), Vec::<u32>::new());
        assert_eq!(str_bytes(r#""""#), vec![0]);
    }

    /// A character from the source is encoded in the literal's encoding, so the same `é` is
    /// two bytes in one constant and one code point in another.
    #[test]
    fn a_source_character_is_encoded_and_an_escape_is_not() {
        assert_eq!(ch("'é'"), 0xc3a9);
        assert_eq!(ch("L'é'"), 0xe9);
        assert_eq!(ch("u'€'"), 0x20ac);
        assert_eq!(ch(r"U'\U0001F600'"), 0x1f600);
        // The same character in a plain constant is its UTF-8 bytes, which makes it a
        // multi-character constant and a negative number.
        assert_eq!(ch(r"'\U0001F600'"), i64::from(0xf09f_9880u32 as i32));
        assert!(ch_remarks(r"'\U0001F600'").has(Remarks::MULTICHARACTER));
    }

    /// Both compilers have `\e` and neither standard does, and an escape that means nothing is
    /// the letter itself after a warning rather than an error.
    #[test]
    fn the_escapes_outside_the_standard_still_have_values() {
        assert_eq!(ch(r"'\e'"), 0x1b);
        assert!(ch_remarks(r"'\e'").has(Remarks::NON_ISO_ESCAPE));
        assert_eq!(ch(r"'\q'"), 0x71);
        assert!(ch_remarks(r"'\q'").has(Remarks::UNKNOWN_ESCAPE));
        assert_eq!(ch_error(r"'\x'"), LiteralError::NoHexDigits);
        assert_eq!(ch_error(r"'\N{LATIN SMALL LETTER A}'"), LiteralError::NamedUcn);
    }

    /// A universal character name may not name a character in the basic character set, which
    /// GCC still enforces in C23 where the wording was relaxed, and the three characters below
    /// a space that are allowed anyway are allowed here too.
    #[test]
    fn a_universal_character_name_may_not_name_just_anything() {
        assert_eq!(ch("'\\u0024'"), 0x24);
        assert_eq!(ch("'\\u00e9'"), 0xc3a9);
        assert_eq!(ch_error("'\\u0041'"), LiteralError::InvalidUcn);
        assert_eq!(ch_error(r"'\ud800'"), LiteralError::InvalidUcn);
        assert_eq!(ch_error(r"'\u00'"), LiteralError::IncompleteUcn);
        // GCC warns here and encodes the value anyway. clang refuses it and so does this.
        assert_eq!(ch_error(r"'\U00110000'"), LiteralError::InvalidUcn);
    }

    #[test]
    fn a_universal_character_name_before_c99_is_worth_a_remark() {
        let out = character("'\\u00e9'", Std::C89, &linux()).expect("a constant");
        assert!(out.remarks.has(Remarks::UCN));
        let out = character("'\\u00e9'", Std::C99, &linux()).expect("a constant");
        assert!(!out.remarks.has(Remarks::UCN));
    }

    /// Octal runs to three digits and stops, and hexadecimal runs as far as the digits go, so
    /// `"\1234"` is two characters and `"\x41z"` is two as well.
    #[test]
    fn an_octal_escape_ends_and_a_hex_escape_does_not() {
        assert_eq!(str_elements(r#""\1234""#), vec![0x53, 0x34]);
        assert_eq!(str_elements(r#""\x41z""#), vec![0x41, 0x7a]);
        assert_eq!(str_elements(r#""\x41""#), vec![0x41]);
    }

    /// The sizes GCC reports for these, which is the elements plus the terminator times the
    /// width of one.
    #[test]
    fn a_string_is_as_many_bytes_as_its_encoding_makes_it() {
        assert_eq!(str_bytes(r#""abc""#).len(), 4);
        assert_eq!(str_bytes(r#"L"abc""#).len(), 16);
        assert_eq!(str_bytes(r#"u"abc""#).len(), 8);
        assert_eq!(str_bytes(r#"U"abc""#).len(), 16);
        assert_eq!(str_bytes(r#"u8"abc""#).len(), 4);
        // A zero in the middle is an element like any other, and the terminator is still added.
        assert_eq!(str_bytes(r#""a\0b""#), vec![0x61, 0x00, 0x62, 0x00]);
        assert_eq!(str_bytes(r#""é""#), vec![0xc3, 0xa9, 0x00]);
    }

    /// The one encoding where a character can take two elements, which is why a wide string is
    /// not the same length on Windows as it is anywhere else.
    #[test]
    fn utf16_splits_the_characters_that_do_not_fit_into_a_surrogate_pair() {
        assert_eq!(
            str_elements(r#"u8"é€😀""#),
            vec![0xc3, 0xa9, 0xe2, 0x82, 0xac, 0xf0, 0x9f, 0x98, 0x80]
        );
        assert_eq!(str_elements(r#"u"€😀""#), vec![0x20ac, 0xd83d, 0xde00]);
        assert_eq!(str_elements(r#"U"€😀""#), vec![0x20ac, 0x1f600]);
    }

    /// A wide literal is UTF-16 on Windows and UTF-32 everywhere else, so the same three
    /// characters are four elements on one target and three on the other.
    #[test]
    fn a_wide_literal_is_whatever_the_target_makes_wchar_t() {
        let text = r#"L"a😀""#;
        let here = string(text, Std::C23, &linux()).expect("a string");
        assert_eq!(here.elements, vec![0x61, 0x1f600]);
        assert_eq!(here.bytes(&linux()).len(), 12);
        let there = string(text, Std::C23, &windows()).expect("a string");
        assert_eq!(there.elements, vec![0x61, 0xd83d, 0xde00]);
        assert_eq!(there.bytes(&windows()).len(), 8);
        // And a wide character constant takes the sign of `wchar_t`, which is not the same on
        // every target either.
        assert_eq!(character(r"L'\xffffffff'", Std::C23, &linux()).expect("a constant").value, -1);
        assert_eq!(
            character(r"L'\xffffffff'", Std::C23, &arm()).expect("a constant").value,
            0xffff_ffff
        );
    }

    /// Every target the compiler has is little-endian, so the other order is checked by
    /// flipping the field rather than by naming a target, and this is the test that fails on
    /// the day a big-endian one arrives with the layout still assuming otherwise.
    #[test]
    fn the_bytes_come_out_in_the_targets_order() {
        let mut big = linux();
        big.little_endian = false;
        let literal = string(r#"u"ab""#, Std::C23, &big).expect("a string");
        assert_eq!(literal.bytes(&big), vec![0x00, 0x61, 0x00, 0x62, 0x00, 0x00]);
        assert_eq!(literal.bytes(&linux()), vec![0x61, 0x00, 0x62, 0x00, 0x00, 0x00]);
    }

    /// `L` is C89, `u` and `U` are C11, and `u8` is C11 on a string and C23 on a character
    /// constant, which is the one place the two differ.
    #[test]
    fn a_prefix_is_only_available_in_the_dialect_that_has_it() {
        assert!(character("L'a'", Std::C89, &linux()).is_ok());
        assert_eq!(
            character("u'a'", Std::C99, &linux()).expect_err("not in C99"),
            LiteralError::PrefixNotInDialect
        );
        assert!(character("u'a'", Std::C11, &linux()).is_ok());
        assert!(string(r#"u8"a""#, Std::C11, &linux()).is_ok());
        assert_eq!(
            character("u8'a'", Std::C11, &linux()).expect_err("not in C11"),
            LiteralError::PrefixNotInDialect
        );
        assert!(character("u8'a'", Std::C23, &linux()).is_ok());
    }

    /// The widths and signs the elements have, which is what the parser will turn into the
    /// type of the literal.
    #[test]
    fn an_element_is_as_wide_as_the_encoding_and_the_target_agree() {
        let target = linux();
        assert_eq!(Encoding::Plain.element_width(&target), 8);
        assert_eq!(Encoding::Utf8.element_width(&target), 8);
        assert_eq!(Encoding::Utf16.element_width(&target), 16);
        assert_eq!(Encoding::Utf32.element_width(&target), 32);
        assert_eq!(Encoding::Wide.element_width(&target), 32);
        assert_eq!(Encoding::Wide.element_width(&windows()), 16);

        assert!(Encoding::Plain.is_signed(&target));
        assert!(!Encoding::Plain.is_signed(&arm()));
        assert!(Encoding::Wide.is_signed(&target));
        assert!(!Encoding::Wide.is_signed(&arm()));
        assert!(!Encoding::Utf8.is_signed(&target));
        assert!(!Encoding::Utf16.is_signed(&target));
        assert!(!Encoding::Utf32.is_signed(&target));
    }

    #[test]
    fn a_spelling_that_is_not_a_literal_is_refused_rather_than_guessed_at() {
        assert_eq!(ch_error("a"), LiteralError::NotALiteral);
        assert_eq!(ch_error("'a"), LiteralError::NotALiteral);
        assert_eq!(
            string("'a'", Std::C23, &linux()).expect_err("not a string"),
            LiteralError::NotALiteral
        );
        assert_eq!(ch_error("'"), LiteralError::NotALiteral);
    }

    #[test]
    fn every_error_has_something_to_print() {
        for error in [
            LiteralError::NotALiteral,
            LiteralError::Empty,
            LiteralError::TooLong,
            LiteralError::NoHexDigits,
            LiteralError::IncompleteUcn,
            LiteralError::InvalidUcn,
            LiteralError::NamedUcn,
            LiteralError::InvalidUtf8,
            LiteralError::PrefixNotInDialect,
            LiteralError::MixedEncodings,
        ] {
            assert!(!error.message().is_empty());
        }
    }
}
