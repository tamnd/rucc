//! Translation phases 1 and 2, done lazily.
//!
//! Design: `spec/05-preprocessor.md` section 5.1.
//!
//! Phase 1 is the byte order mark, the line ending normalisation, and trigraphs when they are
//! asked for. Phase 2 splices a line ending in a backslash onto the next one.
//!
//! Neither rewrites the buffer. Rewriting is the obvious implementation and it is wrong twice
//! over: it copies a 60 KB header to delete three backslashes from it, and it destroys the
//! only thing a diagnostic can point at, which is a byte offset in the file the user actually
//! wrote. So the cursor resolves both phases as it walks, and a span is always a range of
//! real file bytes even when the token's spelling is not those bytes read in order.
//!
//! The cost of that choice is one branch per byte. It is paid on a fast path that rejects the
//! two bytes that can begin a splice or a trigraph, so the common case is a compare against a
//! constant and everything else is out of line.
//!
//! Where a whole run of bytes is known to be uninteresting before it is read, which is what
//! whitespace and comment bodies are, [`Cursor::skip_blanks`] and [`Cursor::skip_plain`] move
//! the head over the run in one go using the word at a time scans in [`crate::swar`]. They are
//! allowed to do that only because neither can pass a byte that phases 1 and 2 would have
//! rewritten, so nothing goes unrecorded and the head still lands on a real file offset.

use crate::swar;

/// One logical byte, and what it cost to get it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Step {
    /// The byte as phases 1 and 2 leave it.
    pub(crate) byte: u8,
    /// Where the next logical byte starts, as a file offset.
    pub(crate) next: u32,
    /// Whether `byte` is literally the byte at the position this step started from. False
    /// when a splice was crossed, a trigraph was replaced, or a `\r\n` was folded, which is
    /// what [`TokenFlags::SPLICED`](crate::TokenFlags::SPLICED) reports.
    pub(crate) clean: bool,
}

/// A read head over one file's bytes, presenting the file as phase 2 leaves it.
#[derive(Debug, Clone)]
pub(crate) struct Cursor<'a> {
    src: &'a [u8],
    pos: u32,
    trigraphs: bool,
    /// Offsets of backslashes that spliced a line but had whitespace after them. Collected
    /// rather than reported here, because the cursor has no diagnostics and because the
    /// lexer is the thing that knows a span is worth attaching to a token.
    loose_splices: Vec<u32>,
}

impl<'a> Cursor<'a> {
    /// A cursor over `src`, starting after any byte order mark.
    pub(crate) fn new(src: &'a [u8], trigraphs: bool) -> Cursor<'a> {
        // Phase 1. A UTF-8 byte order mark at the very start is not part of the program, and
        // a surprising number of headers shipped by vendors on Windows have one.
        let pos = if src.starts_with(&[0xEF, 0xBB, 0xBF]) { 3 } else { 0 };
        Cursor { src, pos, trigraphs, loose_splices: Vec::new() }
    }

    /// The current file offset.
    #[inline]
    pub(crate) fn pos(&self) -> u32 {
        self.pos
    }

    /// The raw bytes, for taking a token's spelling as a slice when it has no splices in it.
    #[inline]
    pub(crate) fn bytes(&self) -> &'a [u8] {
        self.src
    }

    /// Whether the head is past the last byte.
    #[inline]
    pub(crate) fn at_end(&self) -> bool {
        self.pos as usize >= self.src.len()
    }

    /// The logical byte at `at`, or `None` at end of file.
    pub(crate) fn step_at(&self, at: u32) -> Option<Step> {
        let mut p = at as usize;
        let mut clean = true;
        loop {
            let b = *self.src.get(p)?;
            match b {
                // Fast path. Everything that is not one of the four interesting bytes is
                // itself, and this is the branch that runs for almost every byte of a file.
                _ if b != b'\\' && b != b'?' && b != b'\r' => {
                    return Some(Step { byte: b, next: p as u32 + 1, clean });
                }
                b'\r' => {
                    // Phase 1. A file with CRLF endings must lex the same as one without,
                    // and a lone CR is a line ending on nothing anybody still uses but costs
                    // one comparison to accept.
                    let next = if self.src.get(p + 1) == Some(&b'\n') { p + 2 } else { p + 1 };
                    return Some(Step { byte: b'\n', next: next as u32, clean: false });
                }
                b'?' if self.trigraphs => {
                    let Some(mapped) = self.trigraph_at(p) else {
                        return Some(Step { byte: b'?', next: p as u32 + 1, clean });
                    };
                    if mapped == b'\\' {
                        // A trigraph that spells a backslash can still splice, because phase
                        // 1 runs before phase 2. This is the one case where the order of the
                        // phases is observable, and it is in the standard's examples. The
                        // scan for the line ending starts after all three bytes of the
                        // trigraph rather than after one.
                        if let Some(after) = self.splice_from(p + 3) {
                            p = after;
                            clean = false;
                            continue;
                        }
                    }
                    return Some(Step { byte: mapped, next: p as u32 + 3, clean: false });
                }
                b'?' => return Some(Step { byte: b'?', next: p as u32 + 1, clean }),
                _ => {
                    // A backslash. Phase 2 if a line ending follows, an ordinary backslash
                    // otherwise, and an ordinary backslash is how a universal character name
                    // and an escape sequence both start.
                    match self.splice_at(p) {
                        Some(after) => {
                            p = after;
                            clean = false;
                        }
                        None => return Some(Step { byte: b'\\', next: p as u32 + 1, clean }),
                    }
                }
            }
        }
    }

    /// Where the line continues, if a splice starts at `p`.
    ///
    /// GCC accepts whitespace between the backslash and the newline and warns about it. A
    /// good deal of real code has a trailing space after a backslash in a macro definition
    /// and expects it to keep working, so this accepts it too. The caller reports it.
    fn splice_at(&self, p: usize) -> Option<usize> {
        self.splice_from(p + 1)
    }

    /// The same, given the position just past the backslash however it was spelled.
    fn splice_from(&self, from: usize) -> Option<usize> {
        let mut q = from;
        while matches!(self.src.get(q), Some(b' ' | b'\t' | 0x0B | 0x0C)) {
            q += 1;
        }
        match self.src.get(q) {
            Some(b'\n') => Some(q + 1),
            Some(b'\r') => {
                if self.src.get(q + 1) == Some(&b'\n') {
                    Some(q + 2)
                } else {
                    Some(q + 1)
                }
            }
            _ => None,
        }
    }

    /// Whether a splice starts at `p` and has whitespace between the backslash and the line
    /// ending, which is the case worth warning about.
    fn splice_has_trailing_space(&self, p: usize) -> bool {
        matches!(self.src.get(p + 1), Some(b' ' | b'\t' | 0x0B | 0x0C))
            && self.splice_at(p).is_some()
    }

    /// Takes the offsets of splices that had whitespace after the backslash.
    ///
    /// The lexer drains this after every token and turns each one into a warning. The trailing
    /// space is invisible in an editor, so a macro definition that quietly lost its last line
    /// looks correct right up until it does not compile.
    pub(crate) fn take_loose_splices(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.loose_splices)
    }

    /// The character a trigraph at `p` stands for.
    fn trigraph_at(&self, p: usize) -> Option<u8> {
        if self.src.get(p + 1) != Some(&b'?') {
            return None;
        }
        match self.src.get(p + 2)? {
            b'=' => Some(b'#'),
            b'(' => Some(b'['),
            b'/' => Some(b'\\'),
            b')' => Some(b']'),
            b'\'' => Some(b'^'),
            b'<' => Some(b'{'),
            b'!' => Some(b'|'),
            b'>' => Some(b'}'),
            b'-' => Some(b'~'),
            _ => None,
        }
    }

    /// The logical byte under the head, or zero at end of file.
    ///
    /// Zero is safe as the end marker because a null byte in a source file is not part of any
    /// pp-token: it is whitespace to the standard and an error worth reporting to us, so
    /// nothing downstream can confuse the two.
    #[inline]
    pub(crate) fn first(&self) -> u8 {
        self.step_at(self.pos).map_or(0, |s| s.byte)
    }

    /// The logical byte `n` positions ahead, or zero.
    pub(crate) fn nth(&self, n: usize) -> u8 {
        let mut at = self.pos;
        for _ in 0..n {
            match self.step_at(at) {
                Some(s) => at = s.next,
                None => return 0,
            }
        }
        self.step_at(at).map_or(0, |s| s.byte)
    }

    /// Consumes one logical byte and returns it, with whether it was clean.
    #[inline]
    pub(crate) fn bump(&mut self) -> Option<(u8, bool)> {
        let from = self.pos;
        let s = self.step_at(self.pos)?;
        self.pos = s.next;
        if !s.clean {
            self.note_loose_splices(from, s.next);
        }
        Some((s.byte, s.clean))
    }

    /// Advances over spaces and tabs, eight at a time, and says whether it moved.
    ///
    /// Safe to do in one jump because neither byte is one phases 1 and 2 have anything to say
    /// about: a space is a space in every phase, it cannot begin a splice or a trigraph, and no
    /// step that starts on one is ever unclean. So the head lands on a real file offset with
    /// nothing left unrecorded behind it, which is the property the whole lazy design rests on.
    ///
    /// Vertical tab and form feed are left to the caller. They are whitespace too, and they
    /// occur about as often as they deserve to.
    #[inline]
    pub(crate) fn skip_blanks(&mut self) -> bool {
        let from = self.pos as usize;
        let to = swar::run_of_blanks(self.src, from);
        self.pos = to as u32;
        to > from
    }

    /// Advances to the next byte the caller has to look at, over the ones it certainly does not.
    ///
    /// `stops` is what the caller wants to stop at. Added to it are the bytes phases 1 and 2
    /// might rewrite, so the head can never end up past a splice, a trigraph or a carriage
    /// return that nothing looked at. A newline is always a stop, because every caller either
    /// ends at one or has to notice it went by.
    ///
    /// This is the comment body scan. A license block at the top of a header is two thousand
    /// bytes that mean nothing to the compiler, and reading them a byte at a time through
    /// [`step_at`](Self::step_at) asks the splice question two thousand times to hear no.
    ///
    /// May stop on a byte that is in neither set, since the underlying scan is allowed to be
    /// conservative. The caller looks at whatever it lands on anyway, so the worst case is a
    /// little less progress and never a wrong answer.
    pub(crate) fn skip_plain(&mut self, stops: &[u8]) {
        // The bytes that are never safe to jump over, then the caller's.
        let mut set = [b'\n', b'\r', b'\\', b'?', 0, 0, 0, 0];
        let mut n = if self.trigraphs { 4 } else { 3 };
        debug_assert!(n + stops.len() <= set.len());
        for &s in stops {
            set[n] = s;
            n += 1;
        }
        self.pos = swar::first_of(self.src, self.pos as usize, &set[..n]) as u32;
    }

    /// Records any splice in `from .. to` that had whitespace after its backslash.
    ///
    /// Off the fast path by construction: it only runs for a step that was not clean, which
    /// is a handful of positions in a real file.
    #[cold]
    fn note_loose_splices(&mut self, from: u32, to: u32) {
        for p in from as usize..(to as usize).min(self.src.len()) {
            if self.src[p] == b'\\' && self.splice_has_trailing_space(p) {
                self.loose_splices.push(p as u32);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn logical(src: &str, trigraphs: bool) -> String {
        let mut c = Cursor::new(src.as_bytes(), trigraphs);
        let mut out = String::new();
        while let Some((b, _)) = c.bump() {
            out.push(b as char);
        }
        out
    }

    #[test]
    fn a_byte_order_mark_is_not_part_of_the_program() {
        assert_eq!(logical("\u{feff}int x;", false), "int x;");
    }

    #[test]
    fn crlf_reads_the_same_as_lf() {
        assert_eq!(logical("a\r\nb\rc\nd", false), "a\nb\nc\nd");
    }

    #[test]
    fn a_backslash_at_the_end_of_a_line_joins_it_to_the_next() {
        assert_eq!(logical("in\\\nt x;", false), "int x;");
        assert_eq!(logical("in\\\r\nt x;", false), "int x;");
    }

    #[test]
    fn a_backslash_with_trailing_space_still_splices() {
        // GCC warns and splices. A great deal of existing code depends on the splicing half.
        let src = "in\\  \nt";
        assert_eq!(logical(src, false), "int");
        let mut c = Cursor::new(src.as_bytes(), false);
        while c.bump().is_some() {}
        assert_eq!(c.take_loose_splices(), vec![2]);
    }

    #[test]
    fn an_ordinary_splice_is_not_reported() {
        let mut c = Cursor::new(b"in\\\nt", false);
        while c.bump().is_some() {}
        assert!(c.take_loose_splices().is_empty());
    }

    #[test]
    fn a_lone_backslash_is_just_a_backslash() {
        assert_eq!(logical("\\u00e9", false), "\\u00e9");
    }

    #[test]
    fn trigraphs_are_off_unless_asked_for() {
        assert_eq!(logical("??=define", false), "??=define");
        assert_eq!(logical("??=define", true), "#define");
    }

    #[test]
    fn a_trigraph_backslash_can_still_splice() {
        // Phase 1 runs before phase 2, so `??/` at the end of a line is a line splice. The
        // standard has this as an example and it is the only place the phase order shows.
        assert_eq!(logical("in??/\nt", true), "int");
    }

    #[test]
    fn spans_still_point_at_real_bytes_across_a_splice() {
        let mut c = Cursor::new(b"a\\\nb", false);
        assert_eq!(c.bump(), Some((b'a', true)));
        assert_eq!(c.pos(), 1);
        assert_eq!(c.bump(), Some((b'b', false)));
        // Four bytes consumed for two logical ones, and the head is at the real end of file.
        assert_eq!(c.pos(), 4);
    }

    #[test]
    fn lookahead_crosses_splices_too() {
        let c = Cursor::new(b"+\\\n+", false);
        assert_eq!(c.first(), b'+');
        assert_eq!(c.nth(1), b'+');
        assert_eq!(c.nth(2), 0);
    }
}
