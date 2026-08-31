//! Phase 3: bytes to preprocessing tokens.
//!
//! Design: `spec/05-preprocessor.md` sections 5.1 and 5.2.
//!
//! The scanner is a loop over the dispatch table in [`crate::class`]. It never copies the
//! input, never allocates for a token whose spelling is contiguous in the file, and interns
//! identifiers as it goes rather than in a second pass, which `spec/06-lexer-and-parser.md`
//! section 6.1 asks for so that no part of the compiler after this one compares identifier
//! text.

use rucc_base::{Interner, Symbol};
use rucc_diag::{BytePos, Diagnostic, Span};

use crate::class::{CLASS, Class, is_ident_continue};
use crate::cursor::Cursor;
use crate::token::{PpToken, PpTokenKind, Punct, TokenFlags};

/// The dialect knobs phase 1 cares about.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Options {
    /// Replace trigraphs, which `-trigraphs` turns on.
    ///
    /// Off by default because C23 removed them, and because leaving them on means `??!`
    /// inside a string literal silently becomes `|`, which has caught out every project that
    /// ever wrote a question mark next to a punctuator.
    pub trigraphs: bool,
}

impl Options {
    /// The defaults, which are what `-std=gnu23` implies.
    #[must_use]
    pub fn new() -> Options {
        Options { trigraphs: false }
    }
}

/// A scanner over one file.
#[derive(Debug)]
pub struct Lexer<'a> {
    cursor: Cursor<'a>,
    /// Where this file begins in the flat coordinate space `rucc-diag` describes, so that a
    /// span is comparable across files without carrying a file id.
    file_start: BytePos,
    at_line_start: bool,
    leading_space: bool,
    /// Where the token currently being scanned began, so that the first interruption in its
    /// spelling can copy everything before itself in one go.
    token_start: u32,
    /// Reused between tokens. Only touched for a token whose spelling is interrupted by a
    /// splice or a trigraph, which is a small fraction of a real file.
    scratch: Vec<u8>,
    /// Whether the token being scanned has needed `scratch`.
    unclean: bool,
    diagnostics: Vec<Diagnostic>,
}

impl<'a> Lexer<'a> {
    /// A scanner over `src`, whose first byte sits at `file_start`.
    #[must_use]
    pub fn new(src: &'a [u8], file_start: BytePos, opts: Options) -> Lexer<'a> {
        Lexer {
            cursor: Cursor::new(src, opts.trigraphs),
            file_start,
            at_line_start: true,
            leading_space: false,
            token_start: 0,
            scratch: Vec::new(),
            unclean: false,
            diagnostics: Vec::new(),
        }
    }

    /// Everything the scan has complained about so far.
    #[must_use]
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// Takes the diagnostics, leaving the scanner able to carry on.
    pub fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }

    /// The next preprocessing token, or an end of file token at the end.
    pub fn next_token(&mut self, interner: &mut Interner) -> PpToken {
        let token = self.scan(interner);
        self.report_loose_splices();
        token
    }

    /// Turns the splices the cursor flagged into warnings.
    ///
    /// GCC warns about a backslash with whitespace before the line ending and splices anyway.
    /// Both halves matter: a good deal of real code has a trailing space after a backslash in
    /// a macro definition and expects it to keep working, and the space is invisible in an
    /// editor, so the one time it does change the meaning nobody can see why.
    fn report_loose_splices(&mut self) {
        for at in self.cursor.take_loose_splices() {
            let span = Span::new(self.file_start + at, self.file_start + at + 1);
            self.diagnostics.push(Diagnostic::warning(
                "backslash and line ending separated by whitespace",
                span,
            ));
        }
    }

    fn scan(&mut self, interner: &mut Interner) -> PpToken {
        self.skip_trivia();

        let start = self.cursor.pos();
        let flags = self.take_flags();

        if self.cursor.at_end() {
            return PpToken {
                kind: PpTokenKind::Eof,
                flags,
                value: None,
                span: Span::empty_at(self.file_start + start),
            };
        }

        self.token_start = start;
        self.unclean = false;

        let b = self.cursor.first();
        let kind = match CLASS[b as usize] {
            Class::IdentStart => self.ident_or_prefixed_literal(b, start),
            Class::Digit => self.pp_number(),
            Class::Dot if CLASS[self.cursor.nth(1) as usize] == Class::Digit => self.pp_number(),
            Class::Quote => self.literal(b'"', start, PpTokenKind::StringLit),
            Class::Apostrophe => self.literal(b'\'', start, PpTokenKind::CharConst),
            Class::Backslash => {
                // A backslash that survived phase 2 either begins a universal character name,
                // which is a way of spelling an identifier character, or is a stray.
                if matches!(self.cursor.nth(1), b'u' | b'U') {
                    self.identifier()
                } else {
                    self.eat();
                    PpTokenKind::Other
                }
            }
            Class::Dot | Class::Slash | Class::Punct => match self.punctuator(start, flags) {
                Some(token) => return token,
                None => {
                    self.eat();
                    PpTokenKind::Other
                }
            },
            Class::Space | Class::Newline | Class::Other => {
                self.eat();
                PpTokenKind::Other
            }
        };

        let end = self.cursor.pos();
        let value = Some(self.intern_spelling(interner, start, end));
        let mut flags = flags;
        if self.unclean {
            flags = flags.with(TokenFlags::SPLICED);
        }
        let span = Span::new(self.file_start + start, self.file_start + end);
        PpToken { kind, flags, value, span }
    }

    /// Scans a header name, which only a `#include` line can ask for.
    ///
    /// Returns `None` when the line does not begin with `<` or `"`, which is the computed
    /// include case: the directive has to macro expand the line and try again. The scanner
    /// cannot make this call itself, because `<stdio.h>` and a pair of comparisons are the
    /// same bytes and only the directive knows which one is possible here.
    pub fn header_name(&mut self, interner: &mut Interner) -> Option<PpToken> {
        let token = self.scan_header_name(interner);
        self.report_loose_splices();
        token
    }

    fn scan_header_name(&mut self, interner: &mut Interner) -> Option<PpToken> {
        self.skip_horizontal();
        let start = self.cursor.pos();
        let close = match self.cursor.first() {
            b'<' => b'>',
            b'"' => b'"',
            _ => return None,
        };
        let flags = self.take_flags();
        self.token_start = start;
        self.unclean = false;
        self.eat();
        loop {
            if self.cursor.at_end() || self.cursor.first() == b'\n' {
                let span = Span::new(self.file_start + start, self.file_start + self.cursor.pos());
                self.diagnostics
                    .push(Diagnostic::error("missing terminating character in header name", span));
                break;
            }
            if self.eat() == close {
                break;
            }
        }
        let end = self.cursor.pos();
        let value = Some(self.intern_spelling(interner, start, end));
        let mut flags = flags;
        if self.unclean {
            flags = flags.with(TokenFlags::SPLICED);
        }
        let span = Span::new(self.file_start + start, self.file_start + end);
        Some(PpToken { kind: PpTokenKind::HeaderName, flags, value, span })
    }

    /// The flags for the token about to be scanned, and resets them for the next one.
    fn take_flags(&mut self) -> TokenFlags {
        let mut flags = TokenFlags::EMPTY;
        if self.at_line_start {
            flags = flags.with(TokenFlags::START_OF_LINE);
        }
        if self.leading_space {
            flags = flags.with(TokenFlags::LEADING_SPACE);
        }
        self.at_line_start = false;
        self.leading_space = false;
        flags
    }

    /// Consumes one logical byte, keeping the spelling buffer correct.
    fn eat(&mut self) -> u8 {
        let before = self.cursor.pos();
        // Every caller has already established there is a byte here, through `at_end` or
        // through a lookahead that returned a non-zero byte.
        let (b, clean) = self.cursor.bump().expect("eat called at end of file");
        if self.unclean {
            self.scratch.push(b);
        } else if !clean {
            // The first interruption in this token. Everything before it was contiguous, so
            // it copies as one slice, and only from here on does the scan pay per byte.
            let from = self.token_start as usize;
            let bytes = self.cursor.bytes();
            self.scratch.clear();
            self.scratch.extend_from_slice(&bytes[from..before as usize]);
            self.scratch.push(b);
            self.unclean = true;
        }
        b
    }

    /// Interns the spelling of the token that ran from `start` to `end`.
    fn intern_spelling(&mut self, interner: &mut Interner, start: u32, end: u32) -> Symbol {
        let lossy = {
            let bytes: &[u8] = if self.unclean {
                &self.scratch
            } else {
                &self.cursor.bytes()[start as usize..end as usize]
            };
            match std::str::from_utf8(bytes) {
                Ok(text) => return interner.intern(text),
                // Only reachable inside an identifier or a literal, because everything else
                // is ASCII by construction. Lossy rather than fatal, so that one bad byte
                // does not stop the run before the errors the user cares about.
                Err(_) => String::from_utf8_lossy(bytes).into_owned(),
            }
        };
        let span = Span::new(self.file_start + start, self.file_start + end);
        self.diagnostics.push(Diagnostic::error("source is not valid UTF-8 here", span));
        interner.intern(&lossy)
    }

    /// Whitespace, newlines and comments, all of which become one space.
    fn skip_trivia(&mut self) {
        while !self.cursor.at_end() {
            // Indentation first, in one jump. This is the single hottest thing the lexer does,
            // because every line of every header starts with some and none of it says anything.
            if self.cursor.skip_blanks() {
                self.leading_space = true;
                continue;
            }
            match CLASS[self.cursor.first() as usize] {
                Class::Space => {
                    self.cursor.bump();
                    self.leading_space = true;
                }
                Class::Newline => {
                    self.cursor.bump();
                    self.at_line_start = true;
                    self.leading_space = false;
                }
                Class::Slash => match self.cursor.nth(1) {
                    b'/' => self.line_comment(),
                    b'*' => self.block_comment(),
                    _ => return,
                },
                _ => return,
            }
        }
    }

    /// Spaces and block comments but not newlines, for scanning inside a directive line.
    fn skip_horizontal(&mut self) {
        while !self.cursor.at_end() {
            if self.cursor.skip_blanks() {
                self.leading_space = true;
                continue;
            }
            let b = self.cursor.first();
            if CLASS[b as usize] == Class::Space {
                self.cursor.bump();
                self.leading_space = true;
            } else if b == b'/' && self.cursor.nth(1) == b'*' {
                self.block_comment();
            } else {
                return;
            }
        }
    }

    fn line_comment(&mut self) {
        while !self.cursor.at_end() && self.cursor.first() != b'\n' {
            // Nothing in the body means anything, so the only bytes worth stopping on are the
            // ones that could end it: the newline, and a backslash or trigraph that splices the
            // comment onto the next line instead. Everything between goes past unread.
            self.cursor.skip_plain(&[]);
            if self.cursor.at_end() || self.cursor.first() == b'\n' {
                break;
            }
            self.cursor.bump();
        }
        // A comment becomes one space. The newline that ends it is left for the trivia loop,
        // so the next token still knows it starts a line.
        self.leading_space = true;
    }

    fn block_comment(&mut self) {
        let start = self.cursor.pos();
        self.cursor.bump();
        self.cursor.bump();
        let mut spans_lines = false;
        loop {
            // Same trade as the line comment, with `*` added because that is what can end this
            // one. A license block is a couple of thousand bytes of nothing, and this walks it
            // in a few dozen steps rather than a few thousand.
            self.cursor.skip_plain(b"*");
            if self.cursor.at_end() {
                let span = Span::new(self.file_start + start, self.file_start + self.cursor.pos());
                self.diagnostics.push(Diagnostic::error("unterminated comment", span));
                break;
            }
            let b = self.cursor.first();
            if b == b'\n' {
                spans_lines = true;
            }
            if b == b'*' && self.cursor.nth(1) == b'/' {
                self.cursor.bump();
                self.cursor.bump();
                break;
            }
            self.cursor.bump();
        }
        self.leading_space = true;
        if spans_lines {
            // A comment is whitespace, so a `#` after a comment that crossed a newline is
            // still the first thing on its line and is still a directive. GCC agrees, and
            // real headers write directives this way.
            self.at_line_start = true;
        }
    }

    fn ident_or_prefixed_literal(&mut self, b: u8, start: u32) -> PpTokenKind {
        // `L"x"` is one token rather than an identifier followed by a string, so the prefixes
        // are checked before the identifier scan rather than unwound afterwards.
        let (n1, n2) = (self.cursor.nth(1), self.cursor.nth(2));
        match b {
            b'L' | b'u' | b'U' if n1 == b'"' => {
                self.eat();
                self.literal(b'"', start, PpTokenKind::StringLit)
            }
            b'L' | b'u' | b'U' if n1 == b'\'' => {
                self.eat();
                self.literal(b'\'', start, PpTokenKind::CharConst)
            }
            // `u8"s"` is C11. `u8'c'` is C23.
            b'u' if n1 == b'8' && (n2 == b'"' || n2 == b'\'') => {
                self.eat();
                self.eat();
                let kind = if n2 == b'"' { PpTokenKind::StringLit } else { PpTokenKind::CharConst };
                self.literal(n2, start, kind)
            }
            _ => self.identifier(),
        }
    }

    fn identifier(&mut self) -> PpTokenKind {
        while !self.cursor.at_end() {
            let b = self.cursor.first();
            if is_ident_continue(b) {
                self.eat();
            } else if b == b'\\' && matches!(self.cursor.nth(1), b'u' | b'U') {
                // A universal character name spells an identifier character. Whether the
                // character it names is allowed in an identifier is a phase 7 question,
                // because the answer depends on `-std=`.
                self.eat();
                self.eat();
            } else {
                break;
            }
        }
        PpTokenKind::Ident
    }

    fn pp_number(&mut self) -> PpTokenKind {
        // The pp-number grammar is deliberately looser than the constant grammar, so `1.2.3`
        // and `0x1p+3` are both one token here. Rejecting the first belongs to phase 7, and
        // doing it here would break `##` pasting that builds a number out of pieces.
        self.eat();
        while !self.cursor.at_end() {
            let b = self.cursor.first();
            let n1 = self.cursor.nth(1);
            if matches!(b, b'e' | b'E' | b'p' | b'P') && matches!(n1, b'+' | b'-') {
                self.eat();
                self.eat();
            } else if is_ident_continue(b) || b == b'.' {
                self.eat();
            } else if b == b'\'' && is_ident_continue(n1) {
                // C23 digit separators. `1'000'000` is one pp-number, and the apostrophe only
                // separates when an identifier character follows, so `1'a'` still ends the
                // number where a character constant begins.
                self.eat();
                self.eat();
            } else if b == b'\\' && matches!(n1, b'u' | b'U') {
                self.eat();
                self.eat();
            } else {
                break;
            }
        }
        PpTokenKind::Number
    }

    fn literal(&mut self, quote: u8, start: u32, kind: PpTokenKind) -> PpTokenKind {
        self.eat();
        loop {
            if self.cursor.at_end() || self.cursor.first() == b'\n' {
                // A literal does not cross a line. Reporting it here and stopping at the
                // newline is what keeps one missing quote from swallowing the rest of the
                // file and turning into a hundred nonsense errors.
                let span = Span::new(self.file_start + start, self.file_start + self.cursor.pos());
                let what = if quote == b'"' { "string literal" } else { "character constant" };
                self.diagnostics
                    .push(Diagnostic::error(format!("missing terminating quote in {what}"), span));
                break;
            }
            let b = self.eat();
            if b == quote {
                break;
            }
            if b == b'\\' && !self.cursor.at_end() && self.cursor.first() != b'\n' {
                // What the escape means is phase 5's problem. All this needs to know is that
                // the next byte cannot end the literal.
                self.eat();
            }
        }
        kind
    }

    /// Scans a punctuator, or returns `None` without consuming anything when the byte begins
    /// no punctuator at all.
    fn punctuator(&mut self, start: u32, flags: TokenFlags) -> Option<PpToken> {
        let (punct, len, digraph) = self.punctuator_kind()?;
        for _ in 0..len {
            self.eat();
        }
        let end = self.cursor.pos();
        let mut flags = flags;
        if digraph {
            flags = flags.with(TokenFlags::DIGRAPH);
        }
        if self.unclean {
            flags = flags.with(TokenFlags::SPLICED);
        }
        let span = Span::new(self.file_start + start, self.file_start + end);
        Some(PpToken { kind: PpTokenKind::Punct(punct), flags, value: None, span })
    }

    /// Longest match over the punctuator set, measured in logical bytes.
    fn punctuator_kind(&self) -> Option<(Punct, usize, bool)> {
        let one = self.cursor.first();
        let two = self.cursor.nth(1);
        let three = self.cursor.nth(2);
        let four = self.cursor.nth(3);
        let found = match one {
            b'[' => (Punct::LBracket, 1, false),
            b']' => (Punct::RBracket, 1, false),
            b'(' => (Punct::LParen, 1, false),
            b')' => (Punct::RParen, 1, false),
            b'{' => (Punct::LBrace, 1, false),
            b'}' => (Punct::RBrace, 1, false),
            b'~' => (Punct::Tilde, 1, false),
            b'?' => (Punct::Question, 1, false),
            b';' => (Punct::Semi, 1, false),
            b',' => (Punct::Comma, 1, false),
            b'.' if two == b'.' && three == b'.' => (Punct::Ellipsis, 3, false),
            b'.' => (Punct::Dot, 1, false),
            b'-' => match two {
                b'>' => (Punct::Arrow, 2, false),
                b'-' => (Punct::MinusMinus, 2, false),
                b'=' => (Punct::MinusEq, 2, false),
                _ => (Punct::Minus, 1, false),
            },
            b'+' => match two {
                b'+' => (Punct::PlusPlus, 2, false),
                b'=' => (Punct::PlusEq, 2, false),
                _ => (Punct::Plus, 1, false),
            },
            b'&' => match two {
                b'&' => (Punct::AmpAmp, 2, false),
                b'=' => (Punct::AmpEq, 2, false),
                _ => (Punct::Amp, 1, false),
            },
            b'|' => match two {
                b'|' => (Punct::PipePipe, 2, false),
                b'=' => (Punct::PipeEq, 2, false),
                _ => (Punct::Pipe, 1, false),
            },
            b'*' if two == b'=' => (Punct::StarEq, 2, false),
            b'*' => (Punct::Star, 1, false),
            b'/' if two == b'=' => (Punct::SlashEq, 2, false),
            b'/' => (Punct::Slash, 1, false),
            b'!' if two == b'=' => (Punct::Ne, 2, false),
            b'!' => (Punct::Bang, 1, false),
            b'^' if two == b'=' => (Punct::CaretEq, 2, false),
            b'^' => (Punct::Caret, 1, false),
            b'=' if two == b'=' => (Punct::EqEq, 2, false),
            b'=' => (Punct::Eq, 1, false),
            b':' => match two {
                b'>' => (Punct::RBracket, 2, true),
                b':' => (Punct::ColonColon, 2, false),
                _ => (Punct::Colon, 1, false),
            },
            b'<' => match two {
                b'<' if three == b'=' => (Punct::ShlEq, 3, false),
                b'<' => (Punct::Shl, 2, false),
                b'=' => (Punct::Le, 2, false),
                b':' => (Punct::LBracket, 2, true),
                b'%' => (Punct::LBrace, 2, true),
                _ => (Punct::Lt, 1, false),
            },
            b'>' => match two {
                b'>' if three == b'=' => (Punct::ShrEq, 3, false),
                b'>' => (Punct::Shr, 2, false),
                b'=' => (Punct::Ge, 2, false),
                _ => (Punct::Gt, 1, false),
            },
            b'%' => match two {
                b'=' => (Punct::PercentEq, 2, false),
                b'>' => (Punct::RBrace, 2, true),
                b':' if three == b'%' && four == b':' => (Punct::HashHash, 4, true),
                b':' => (Punct::Hash, 2, true),
                _ => (Punct::Percent, 1, false),
            },
            b'#' if two == b'#' => (Punct::HashHash, 2, false),
            b'#' => (Punct::Hash, 1, false),
            _ => return None,
        };
        Some(found)
    }
}

/// Scans `src` to the end and returns every preprocessing token and every complaint.
///
/// The end of file token is included, because everything downstream wants somewhere to point
/// when a construct runs off the end of the file.
pub fn tokenize(
    src: &[u8],
    file_start: BytePos,
    opts: Options,
    interner: &mut Interner,
) -> (Vec<PpToken>, Vec<Diagnostic>) {
    let mut lexer = Lexer::new(src, file_start, opts);
    let mut out = Vec::new();
    loop {
        let token = lexer.next_token(interner);
        let done = token.is_eof();
        out.push(token);
        if done {
            break;
        }
    }
    let diagnostics = lexer.take_diagnostics();
    (out, diagnostics)
}
