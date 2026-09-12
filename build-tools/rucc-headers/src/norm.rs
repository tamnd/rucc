//! What counts as a change to a header, and what a header is cut into before it is merged.
//!
//! Design: `spec/cross-compile/08-sysroots.md` section 8.3, the paragraph that says a change is a
//! change to the code and not to the bytes. 420 of glibc's 498 installed headers differ in bytes
//! somewhere between 2.28 and 2.44 and only 210 of them differ in code, because glibc moves the
//! copyright line in every file every January and changed the licence URL in every file from
//! `http` to `https` in 2019. A merge that believed the bytes would write a conditional into twice
//! as many files as need one, and every one of those conditionals would be about a year.
//!
//! So two texts are compared by their code: comments removed, blank lines dropped, continued lines
//! joined, runs of spaces and tabs collapsed. The text that gets written out is still real header
//! text, never a normalized form of one, and where releases agree on the code it is the newest
//! release's text that is written.
//!
//! # Why a file is cut into logical lines and not into lines
//!
//! Because the merge writes `#if` between two pieces and there are places that cannot have an
//! `#if` put in the middle of them. One is a macro definition continued with a backslash, where a
//! directive between the continuation lines is not a directive at all. Another is a directive
//! continued the same way. The third is a block comment that opens after some code and closes on a
//! later line, which glibc writes in every table of constants:
//!
//! ```text
//! #define IN_EXCL_UNLINK  0x04000000      /* Exclude events on unlinked
//!                                            objects.  */
//! #define IN_MASK_ADD     0x20000000      /* Add to the mask.  */
//! ```
//!
//! A cut between those two lines puts the end of a comment at the top of a piece, and the release
//! that does not take that piece is left with a comment that never closes and a file whose next
//! several declarations are inside it. So a logical line runs until the line ends with no comment
//! open and no backslash on it, and a conditional lands before it or after it and never inside it.
//!
//! Comment-only and blank lines are not pieces of their own either. They attach to the code line
//! below them, which is where the comment about it lives, so moving a declaration between releases
//! moves its comment with it instead of leaving it stranded above a conditional.

/// One piece of a header: a logical line of code, with whatever comments and blank lines came
/// immediately above it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// The text exactly as the file had it, newlines included, which is what gets written out.
    pub text: String,
    /// The code of the logical line, normalized, which is what two releases are compared by.
    pub key: String,
    /// Where in `text` the logical line starts, which is after the comments that came with it.
    ///
    /// Here so that a patch to the code can leave the comment above it where it was, which is what
    /// the replacement of glibc's own definition of `__GLIBC_MINOR__` does.
    pub code_at: usize,
}

/// A header cut into pieces, with the comments after the last piece kept separately.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pieces {
    /// The pieces, in order.
    pub items: Vec<Item>,
    /// The trailing comments and blank lines, which belong to no code line.
    pub tail: String,
}

impl Pieces {
    /// The comparison keys, for the alignment.
    pub fn keys(&self) -> Vec<&str> {
        self.items.iter().map(|i| i.key.as_str()).collect()
    }
}

/// Cuts a header into pieces.
///
/// Every byte of the input is in exactly one piece's `text` or in `tail`, in order, so
/// concatenating them all reproduces the file. That is the property the merge relies on: a piece
/// is a place the text can be cut, not a summary of what is there.
pub fn pieces(text: &str) -> Pieces {
    let mut scanner = Scanner::default();
    let mut items: Vec<Item> = Vec::new();
    let mut pending = String::new();
    let mut logical = String::new();
    let mut key = String::new();

    for line in text.split_inclusive('\n') {
        let code = scanner.line(line);
        let continues = continued(line);
        if logical.is_empty() && code.is_empty() && !continues {
            // A comment or a blank line, which belongs to the next piece of code.
            pending.push_str(line);
            continue;
        }
        logical.push_str(line);
        if !code.is_empty() {
            if !key.is_empty() {
                key.push(' ');
            }
            key.push_str(&code);
        }
        if continues || scanner.in_comment {
            continue;
        }
        let mut piece = std::mem::take(&mut pending);
        let code_at = piece.len();
        piece.push_str(&logical);
        logical.clear();
        items.push(Item { text: piece, key: std::mem::take(&mut key), code_at });
    }

    // A file whose last line is continued, or whose last line has no newline on it, still has to
    // come back out whole.
    if !logical.is_empty() {
        let mut piece = std::mem::take(&mut pending);
        let code_at = piece.len();
        piece.push_str(&logical);
        items.push(Item { text: piece, key: std::mem::take(&mut key), code_at });
    }
    Pieces { items, tail: pending }
}

/// The code of one text, which is what "the same header" means here.
///
/// One logical line per line, which is the same cut [`pieces`] makes and for the same reason: the
/// preprocessor joins a continued line before anything looks at it, so a macro whose body had its
/// line break moved is the same macro. There is one definition of the code and both the alignment
/// and the check the merge does afterwards use it, because two definitions that disagree anywhere
/// would have the merge writing a file it then says is wrong.
pub fn code(text: &str) -> String {
    let mut out = String::new();
    for item in &pieces(text).items {
        if item.key.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&item.key);
    }
    out
}

/// Whether this physical line is continued on the next one.
fn continued(line: &str) -> bool {
    line.trim_end_matches(['\n', '\r']).ends_with('\\')
}

/// The comment state carried from one line to the next.
#[derive(Default)]
struct Scanner {
    in_comment: bool,
}

impl Scanner {
    /// The code on one physical line, with the comments taken out and the spacing collapsed.
    fn line(&mut self, text: &str) -> String {
        let bytes = text.as_bytes();
        let mut out = String::with_capacity(text.len());
        let mut i = 0;
        while i < bytes.len() {
            if self.in_comment {
                if bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/') {
                    self.in_comment = false;
                    i += 2;
                } else {
                    i += 1;
                }
                continue;
            }
            match bytes[i] {
                b'/' if bytes.get(i + 1) == Some(&b'*') => {
                    self.in_comment = true;
                    i += 2;
                    // A comment between two tokens is a space between them, not nothing.
                    push_space(&mut out);
                }
                b'/' if bytes.get(i + 1) == Some(&b'/') => break,
                b'"' | b'\'' => {
                    let quote = bytes[i];
                    match literal(bytes, i) {
                        // A string or a character constant is code, and a `/*` inside one is not
                        // the start of a comment.
                        Some(end) => {
                            out.push_str(&text[i..end]);
                            i = end;
                        }
                        // An apostrophe in prose and an unterminated string are the same thing
                        // here: a byte, taken as it stands. glibc's headers have both, in
                        // `#error` text and in what a comment scanner would otherwise swallow.
                        None => {
                            out.push(quote as char);
                            i += 1;
                        }
                    }
                }
                b' ' | b'\t' | b'\r' | b'\n' => {
                    push_space(&mut out);
                    i += 1;
                }
                b'\\' if i + 1 == bytes.len() || bytes[i + 1] == b'\n' || bytes[i + 1] == b'\r' => {
                    // The backslash of a continued line is spacing rather than code, so that a
                    // macro whose body moved to one line reads the same as one that did not.
                    push_space(&mut out);
                    i += 1;
                }
                byte => {
                    out.push(byte as char);
                    i += 1;
                }
            }
        }
        out.trim().to_owned()
    }
}

/// One space, and never two in a row or one at the start.
fn push_space(out: &mut String) {
    if !out.is_empty() && !out.ends_with(' ') {
        out.push(' ');
    }
}

/// The end of the literal that starts at `at`, if it ends on this line.
fn literal(bytes: &[u8], at: usize) -> Option<usize> {
    let quote = bytes[at];
    let mut i = at + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'\n' => return None,
            byte if byte == quote => return Some(i + 1),
            _ => i += 1,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every byte of the input is in one piece or in the tail, which is what lets the merge cut
    /// a file up and write the pieces back out.
    fn rejoined(text: &str) -> String {
        let cut = pieces(text);
        let mut out: String = cut.items.iter().map(|i| i.text.as_str()).collect();
        out.push_str(&cut.tail);
        out
    }

    #[test]
    fn a_file_comes_back_out_of_its_pieces() {
        for text in [
            "#define A 1\n",
            "/* one */\n#define A 1\n/* two */\n",
            "#define A \\\n  1\n",
            "no newline at the end",
            "\n\n",
            "",
        ] {
            assert_eq!(rejoined(text), text, "{text:?}");
        }
    }

    #[test]
    fn a_comment_above_a_declaration_belongs_to_it() {
        let cut = pieces("/* what it does\n   and why */\nint f (void);\n#define A 1\n");
        assert_eq!(cut.items.len(), 2);
        assert!(cut.items[0].text.starts_with("/* what it does"));
        assert_eq!(&cut.items[0].text[cut.items[0].code_at..], "int f (void);\n");
        assert_eq!(cut.items[0].key, "int f (void);");
        assert_eq!(cut.items[1].key, "#define A 1");
        assert_eq!(cut.tail, "");
    }

    #[test]
    fn the_comments_at_the_end_belong_to_nothing() {
        let cut = pieces("int f (void);\n/* the end */\n");
        assert_eq!(cut.items.len(), 1);
        assert_eq!(cut.tail, "/* the end */\n");
    }

    /// The hazard this cutting exists for: a conditional cannot go between these two lines.
    #[test]
    fn a_continued_macro_is_one_piece() {
        let cut = pieces("#define TWO(a, b) \\\n  do { a; b; } while (0)\n#define A 1\n");
        assert_eq!(cut.items.len(), 2);
        assert_eq!(cut.items[0].key, "#define TWO(a, b) do { a; b; } while (0)");
        assert_eq!(cut.items[1].key, "#define A 1");
    }

    #[test]
    fn the_year_in_the_copyright_line_is_not_code() {
        let old = "/* Copyright (C) 1991-2018 Free Software Foundation, Inc.\n   http://x */\n\
                   #define A 1\n";
        let new = "/* Copyright (C) 1991-2024 Free Software Foundation, Inc.\n   https://x */\n\
                   #define A 1\n";
        assert_ne!(old, new);
        assert_eq!(code(old), code(new));
        assert_eq!(code(old), "#define A 1");
    }

    #[test]
    fn a_comment_between_two_tokens_is_a_space() {
        assert_eq!(code("int/* and */f (void);\n"), "int f (void);");
        assert_eq!(code("int   \tf (void);\n"), "int f (void);");
    }

    #[test]
    fn a_comment_start_inside_a_string_is_not_one() {
        assert_eq!(code("#define S \"/*\"\n#define A 1\n"), "#define S \"/*\"\n#define A 1");
    }

    /// glibc's `features.h` has apostrophes in its prose, and a scanner that took one for a
    /// character constant would eat the rest of the line and then agree with a release that
    /// changed it.
    #[test]
    fn an_apostrophe_in_a_comment_does_not_eat_the_file() {
        let text = "/* The macros `__GLIBC__' and `__GLIBC_MINOR__' are defined. */\n#define A 1\n";
        assert_eq!(code(text), "#define A 1");
    }

    #[test]
    fn a_line_comment_ends_the_code_on_its_line() {
        assert_eq!(
            code("#define A 1 // and nothing after\n#define B 2\n"),
            "#define A 1\n#define B 2"
        );
    }

    #[test]
    fn a_comment_that_spans_lines_is_gone_from_all_of_them() {
        assert_eq!(
            code("#define A 1\n/* one\n   two\n   three */\n#define B 2\n"),
            "#define A 1\n#define B 2"
        );
    }

    /// The hazard the module documentation opens with. A cut between these two definitions would
    /// leave the release that does not take the second one with a comment that never closes.
    #[test]
    fn a_comment_that_opens_after_code_keeps_its_piece_open() {
        let text = "#define A 1\t/* one\n\t\t\t   two.  */\n#define B 2\n";
        let cut = pieces(text);
        assert_eq!(cut.items.len(), 2);
        assert_eq!(cut.items[0].text, "#define A 1\t/* one\n\t\t\t   two.  */\n");
        assert_eq!(cut.items[0].key, "#define A 1");
        assert_eq!(cut.items[1].text, "#define B 2\n");
        assert_eq!(rejoined(text), text);
    }

    /// Moving where a continued line breaks is not a change, because the preprocessor joins the
    /// line before anything sees it. glibc does this in `a.out.h`, `dirent.h` and `ip_icmp.h`
    /// between 2.28 and 2.44, always to put an operator at the start of a line instead of the end.
    #[test]
    fn moving_where_a_continued_line_breaks_is_not_a_change() {
        let old = "#define F(x) \\\n  (a (x) ? b (x) : \\\n   c (x))\n";
        let new = "#define F(x) \\\n  (a (x) ? b (x) \\\n   : c (x))\n";
        assert_eq!(code(old), code(new));
        assert_eq!(code(old), "#define F(x) (a (x) ? b (x) : c (x))");
    }
}
