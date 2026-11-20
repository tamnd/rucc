//! Enough JSON to read Microsoft's installer manifest, and deliberately no more.
//!
//! Design: `spec/cross-compile/13-distribution.md` section 13.4, which is the only thing in this
//! compiler that has a JSON document to read. [`crate::msvc`] is the reader that uses this.
//!
//! # Why this is here at all
//!
//! Because section 18.3 of `spec/18-package-layout.md` is a dependency budget and a JSON crate
//! would be a dependency. That budget is the same argument section 13.8 makes about an HTTP client,
//! and it comes out differently here for one reason: a downloader is a program the machine already
//! has and a JSON parser is not a program, it is a few hundred lines that either read the document
//! in front of them or do not. There is nothing to keep current and nothing with a security release
//! schedule, so writing it is cheaper than depending on it.
//!
//! # What kind of reader this is
//!
//! A pull reader, not a document. The manifest is eighteen megabytes of packages and the compiler
//! wants about eight of them, so a reader that builds a tree of the whole thing spends most of its
//! time allocating what the next line throws away. Instead the caller walks the document: it enters
//! an object, asks for keys until there are no more, and for each key either reads the value or
//! skips it. Nothing is kept that the caller did not ask to keep.
//!
//! It borrows where it can. A string with no escape in it is a slice of the text, and only a string
//! that has one is copied, which for this document means the file names with a `\` in them and
//! nothing else.
//!
//! # What it does not do
//!
//! It does not validate. A document this accepts is not thereby well formed JSON: a comma before
//! the first member of an array is read as if it were not there, and a number is read for as far as
//! it looks like one. This reads a file Microsoft generates, and the question it has to answer is
//! what that file says rather than whether some other file would have been legal. What it does
//! refuse is the thing that would hurt: nesting is capped, because the text comes off the network
//! and a thousand open brackets should be an error rather than a stack overflow.
//!
//! Floating point numbers are read and thrown away rather than converted. Every number this needs
//! is a size in bytes.

use std::borrow::Cow;
use std::fmt;

/// How deep the value nesting may go before this gives up.
///
/// The manifest is about eight deep in the places that matter. The cap is not a guess at that, it
/// is a guess at what a hostile document could do to the stack, and sixty four is far above one and
/// far below the other.
const DEPTH: usize = 64;

/// What was wrong, and where.
///
/// The position is a byte offset rather than a line and a column because the manifest is one line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct JsonError {
    /// The byte offset the reader had reached.
    pub(crate) at: usize,
    /// What the reader was looking for there, in the words a person would use.
    pub(crate) wanted: &'static str,
}

impl fmt::Display for JsonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} at byte {} of the document", self.wanted, self.at)
    }
}

impl std::error::Error for JsonError {}

/// The result of every read in this module.
pub(crate) type Json<T> = Result<T, JsonError>;

/// A position in a JSON document and the operations that move it.
#[derive(Debug)]
pub(crate) struct Reader<'a> {
    text: &'a str,
    at: usize,
    depth: usize,
}

impl<'a> Reader<'a> {
    /// Start at the beginning of `text`.
    #[must_use]
    pub(crate) fn new(text: &'a str) -> Self {
        Reader { text, at: 0, depth: 0 }
    }

    /// Step over an object's `{`, so that [`Reader::next_key`] can be called.
    ///
    /// # Errors
    ///
    /// When what is there is not an object.
    pub(crate) fn enter_object(&mut self) -> Json<()> {
        self.take(b'{', "an object")
    }

    /// Step over an array's `[`, so that [`Reader::next_item`] can be called.
    ///
    /// # Errors
    ///
    /// When what is there is not an array.
    pub(crate) fn enter_array(&mut self) -> Json<()> {
        self.take(b'[', "an array")
    }

    /// The next key of the object being read, with the reader left on its value.
    ///
    /// [`None`] means the object ended, and its `}` has been stepped over, so a loop over this
    /// leaves the reader after the object whichever way it stops.
    ///
    /// # Errors
    ///
    /// When what follows is neither a key nor the end of the object.
    pub(crate) fn next_key(&mut self) -> Json<Option<Cow<'a, str>>> {
        if !self.more(b'}')? {
            return Ok(None);
        }
        let key = self.string()?;
        self.take(b':', "a colon after a key")?;
        Ok(Some(key))
    }

    /// Whether the array being read has another item, with the reader left on it.
    ///
    /// `false` means the array ended and its `]` has been stepped over, the same way
    /// [`Reader::next_key`] ends an object.
    ///
    /// # Errors
    ///
    /// When what follows is neither a value nor the end of the array.
    pub(crate) fn next_item(&mut self) -> Json<bool> {
        self.more(b']')
    }

    /// Read a string.
    ///
    /// Borrowed from the document when it has no escape in it, which is almost all of them, and
    /// copied when it has.
    ///
    /// # Errors
    ///
    /// When what is there is not a string, when it is not closed, and when an escape in it is one
    /// this does not know or a `\u` that is not four hex digits.
    pub(crate) fn string(&mut self) -> Json<Cow<'a, str>> {
        self.take(b'"', "a string")?;
        let from = self.at;
        let bytes = self.text.as_bytes();
        // The common case first and separately: scan to the closing quote, and if nothing in
        // between was a backslash the answer is the slice itself.
        let mut here = self.at;
        while here < bytes.len() && bytes[here] != b'"' && bytes[here] != b'\\' {
            here += 1;
        }
        if here < bytes.len() && bytes[here] == b'"' {
            self.at = here + 1;
            return Ok(Cow::Borrowed(&self.text[from..here]));
        }
        self.at = here;
        self.escaped(from)
    }

    /// Read a number that is expected to be a whole one, such as a size in bytes.
    ///
    /// # Errors
    ///
    /// When what is there is not a number, when it is negative, and when it does not fit in a
    /// [`u64`].
    pub(crate) fn integer(&mut self) -> Json<u64> {
        self.white();
        let from = self.at;
        let bytes = self.text.as_bytes();
        while self.at < bytes.len() && bytes[self.at].is_ascii_digit() {
            self.at += 1;
        }
        if self.at == from {
            return Err(self.wanted("a whole number"));
        }
        self.text[from..self.at].parse().map_err(|_| JsonError {
            at: from,
            wanted: "a number this size fits in sixty four bits",
        })
    }

    /// Step over one value of any kind, which is what a key nothing here cares about gets.
    ///
    /// # Errors
    ///
    /// When the value is malformed, and when the nesting goes past [`DEPTH`].
    pub(crate) fn skip(&mut self) -> Json<()> {
        self.white();
        let Some(byte) = self.peek() else {
            return Err(self.wanted("a value"));
        };
        match byte {
            b'{' => self.skip_nested(b'{'),
            b'[' => self.skip_nested(b'['),
            b'"' => self.string().map(|_| ()),
            // A literal or a number, both of which end where a delimiter starts. Reading them as
            // one case is the tolerance this module's documentation admits to: nothing downstream
            // is told the difference between `true` and `truf`, because nothing downstream reads
            // a value it decided to skip.
            _ => {
                let from = self.at;
                let bytes = self.text.as_bytes();
                while self.at < bytes.len() && !matches!(bytes[self.at], b',' | b'}' | b']') {
                    self.at += 1;
                }
                if self.at == from { Err(self.wanted("a value")) } else { Ok(()) }
            }
        }
    }

    /// Step over an object or an array, whichever `open` is, and everything inside it.
    fn skip_nested(&mut self, open: u8) -> Json<()> {
        if self.depth >= DEPTH {
            return Err(self.wanted("a document nested less deeply than this one"));
        }
        self.depth += 1;
        let close = if open == b'{' { b'}' } else { b']' };
        self.take(open, "a value")?;
        while self.more(close)? {
            if close == b'}' {
                self.string()?;
                self.take(b':', "a colon after a key")?;
            }
            self.skip()?;
        }
        self.depth -= 1;
        Ok(())
    }

    /// The slow half of [`Reader::string`], entered on the first backslash with `from` at the
    /// character after the opening quote.
    fn escaped(&mut self, from: usize) -> Json<Cow<'a, str>> {
        let mut out = String::from(&self.text[from..self.at]);
        let bytes = self.text.as_bytes();
        while self.at < bytes.len() {
            match bytes[self.at] {
                b'"' => {
                    self.at += 1;
                    return Ok(Cow::Owned(out));
                }
                b'\\' => {
                    self.at += 1;
                    let Some(what) = self.peek() else { break };
                    self.at += 1;
                    match what {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => out.push(self.unicode()?),
                        _ => return Err(self.wanted("an escape this reader knows")),
                    }
                }
                _ => {
                    // One character rather than one byte, because a multi byte character in the
                    // middle of a string is not something to cut in half.
                    let rest = &self.text[self.at..];
                    let ch = rest.chars().next().unwrap_or('\0');
                    out.push(ch);
                    self.at += ch.len_utf8();
                }
            }
        }
        Err(self.wanted("a closing quote"))
    }

    /// One `\u` escape, and the second half of a surrogate pair when the first half needs one.
    fn unicode(&mut self) -> Json<char> {
        let first = self.hex()?;
        // A high surrogate is half a character, and the other half is the `\u` that has to follow
        // it. Anything else there is a document that cut a character in two.
        if (0xd800..0xdc00).contains(&first) {
            if self.peek() != Some(b'\\') {
                return Err(self.wanted("the second half of a surrogate pair"));
            }
            self.at += 1;
            if self.peek() != Some(b'u') {
                return Err(self.wanted("the second half of a surrogate pair"));
            }
            self.at += 1;
            let second = self.hex()?;
            if !(0xdc00..0xe000).contains(&second) {
                return Err(self.wanted("the second half of a surrogate pair"));
            }
            let joined = 0x1_0000 + ((first - 0xd800) << 10) + (second - 0xdc00);
            return char::from_u32(joined).ok_or_else(|| self.wanted("a character"));
        }
        char::from_u32(first).ok_or_else(|| self.wanted("a character"))
    }

    /// Four hex digits as a number.
    fn hex(&mut self) -> Json<u32> {
        let from = self.at;
        let bytes = self.text.as_bytes();
        if from + 4 > bytes.len() {
            return Err(self.wanted("four hex digits"));
        }
        let mut value = 0u32;
        for byte in &bytes[from..from + 4] {
            let digit = (*byte as char)
                .to_digit(16)
                .ok_or(JsonError { at: from, wanted: "four hex digits" })?;
            value = value * 16 + digit;
        }
        self.at = from + 4;
        Ok(value)
    }

    /// Whether the container being read has another member, stepping over the separator when
    /// there is one and over `close` when there is not.
    fn more(&mut self, close: u8) -> Json<bool> {
        self.white();
        match self.peek() {
            Some(byte) if byte == close => {
                self.at += 1;
                Ok(false)
            }
            Some(b',') => {
                self.at += 1;
                self.white();
                // A trailing comma before the close, which this reads as the close. Microsoft's
                // generator does not write one, and a reader that refused would be refusing a
                // document it could understand.
                if self.peek() == Some(close) {
                    self.at += 1;
                    return Ok(false);
                }
                Ok(true)
            }
            Some(_) => Ok(true),
            None => Err(self.wanted("the end of the object or array this is inside")),
        }
    }

    /// Step over one expected byte.
    fn take(&mut self, byte: u8, wanted: &'static str) -> Json<()> {
        self.white();
        if self.peek() == Some(byte) {
            self.at += 1;
            return Ok(());
        }
        Err(self.wanted(wanted))
    }

    /// The byte under the reader, without moving it.
    fn peek(&self) -> Option<u8> {
        self.text.as_bytes().get(self.at).copied()
    }

    /// Step over whatever JSON calls whitespace.
    fn white(&mut self) {
        let bytes = self.text.as_bytes();
        while self.at < bytes.len() && matches!(bytes[self.at], b' ' | b'\t' | b'\n' | b'\r') {
            self.at += 1;
        }
    }

    /// An error at the current position.
    fn wanted(&self, wanted: &'static str) -> JsonError {
        JsonError { at: self.at, wanted }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_object_is_walked_key_by_key_and_the_reader_ends_after_it() {
        // Inside an array, so that reading what follows is what says the object ended where it
        // should have.
        let mut reader = Reader::new(r#"[ { "a" : 1 , "b" : "two" } , "tail" ]"#);
        reader.enter_array().expect("an array");
        assert!(reader.next_item().expect("the object"));
        reader.enter_object().expect("an object");
        let mut keys = Vec::new();
        while let Some(key) = reader.next_key().expect("a key or the end") {
            keys.push(key.into_owned());
            reader.skip().expect("a value");
        }
        assert_eq!(keys, ["a", "b"]);
        assert!(reader.next_item().expect("one more"));
        assert_eq!(reader.string().expect("a string"), "tail");
        assert!(!reader.next_item().expect("the end"));
    }

    #[test]
    fn an_array_is_walked_item_by_item_and_an_empty_one_has_none() {
        let mut reader = Reader::new(r#"[10,20,30]"#);
        reader.enter_array().expect("an array");
        let mut sizes = Vec::new();
        while reader.next_item().expect("an item or the end") {
            sizes.push(reader.integer().expect("a number"));
        }
        assert_eq!(sizes, [10, 20, 30]);

        let mut reader = Reader::new("[ ]");
        reader.enter_array().expect("an array");
        assert!(!reader.next_item().expect("the end"));
    }

    #[test]
    fn a_string_with_no_escape_in_it_is_a_slice_of_the_document() {
        let mut reader = Reader::new(r#""Microsoft.VC.14.44.17.14.CRT.Headers.base""#);
        let read = reader.string().expect("a string");
        assert!(matches!(read, Cow::Borrowed(_)), "it was copied");
        assert_eq!(read, "Microsoft.VC.14.44.17.14.CRT.Headers.base");
    }

    #[test]
    fn the_escapes_the_manifest_uses_come_out_as_the_characters_they_name() {
        // The one that matters is the backslash, because every SDK payload name has a Windows
        // path in it and `Installers\` is how that arrives.
        let mut reader =
            Reader::new(r#""Installers\\Windows SDK Desktop Headers x86-x86_en-us.msi""#);
        let read = reader.string().expect("a string");
        assert!(matches!(read, Cow::Owned(_)), "it was borrowed and it has an escape in it");
        assert_eq!(read, r"Installers\Windows SDK Desktop Headers x86-x86_en-us.msi");

        let mut reader = Reader::new(r#""\" \/ \b \f \n \r \t é 😀""#);
        assert_eq!(reader.string().expect("a string"), "\" / \u{8} \u{c} \n \r \t é 😀");
    }

    #[test]
    fn a_nesting_deeper_than_the_cap_is_an_error_rather_than_a_stack_overflow() {
        // The document comes off the network, which is the whole reason there is a cap.
        let deep = format!("{}{}", "[".repeat(DEPTH + 2), "]".repeat(DEPTH + 2));
        let mut reader = Reader::new(&deep);
        let why = reader.skip().expect_err("a refusal");
        assert!(why.wanted.contains("nested less deeply"), "{why}");
    }

    #[test]
    fn each_thing_that_is_not_there_says_what_was_wanted_and_where() {
        let mut reader = Reader::new("  [1]");
        let why = reader.enter_object().expect_err("not an object");
        assert_eq!(why.wanted, "an object");
        assert_eq!(why.at, 2);

        let mut reader = Reader::new(r#"{"a" 1}"#);
        reader.enter_object().expect("an object");
        let why = reader.next_key().expect_err("no colon");
        assert_eq!(why.wanted, "a colon after a key");

        let mut reader = Reader::new(r#""unterminated"#);
        assert_eq!(reader.string().expect_err("no closing quote").wanted, "a closing quote");

        let mut reader = Reader::new("-3");
        assert_eq!(reader.integer().expect_err("not a whole number").wanted, "a whole number");
    }
}
