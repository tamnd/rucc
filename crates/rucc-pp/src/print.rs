//! Printing the token stream back out, which is what `-E` writes.
//!
//! Design: `spec/05-preprocessor.md` section 5.6.
//!
//! Two rules decide everything here, and they pull against each other. The output has to be
//! usable as input, so two tokens that would lex as one token when written next to each other
//! get a space between them. And the output has to be diffable against GCC's, because that
//! diff is the fastest way to find a preprocessor bug, so the line structure, the indentation
//! and the line markers all follow GCC rather than being tidied up.
//!
//! The line marker format is GCC's: `# 42 "file.h" 1` where the number after the name is 1 for
//! entering a file, 2 for returning to one, 3 for a system header and 4 for a header whose
//! contents are implicitly `extern "C"`. A gap of up to eight lines is printed as blank lines
//! rather than as a marker, which is what GCC does and what keeps the output readable.

use rucc_base::Interner;
use rucc_diag::{FileId, SourceMap};
use rucc_lex::{PpTokenKind, TokenFlags};

use crate::include::{quoted, spelling};
use crate::token::Tok;

/// How many blank lines are worth printing before a line marker is cheaper.
///
/// GCC's number. It is not tuned for anything, but matching it is the difference between an
/// empty diff and a diff on every header boundary.
const MAX_BLANKS: u32 = 8;

/// What `-E` was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrintOptions {
    /// Whether to write line markers, which `-P` turns off.
    ///
    /// With them off the blank line padding goes too, because the point of `-P` is output for
    /// something other than a compiler to read.
    pub line_markers: bool,
}

impl PrintOptions {
    /// The default, which is what plain `-E` asks for.
    pub fn new() -> PrintOptions {
        PrintOptions { line_markers: true }
    }
}

impl Default for PrintOptions {
    fn default() -> PrintOptions {
        PrintOptions::new()
    }
}

/// Renders `tokens` the way `-E` prints them.
///
/// `main` is the file named on the command line, which is what the first line marker says even
/// when the first token comes from a header.
pub fn print(
    main: FileId,
    tokens: &[Tok],
    sources: &SourceMap,
    interner: &Interner,
    opts: PrintOptions,
) -> String {
    let mut printer = Printer {
        out: String::new(),
        opts,
        sources,
        interner,
        file: main,
        line: 1,
        printed: false,
        stack: vec![main],
    };
    printer.start();
    let mut previous: Option<Tok> = None;
    for &tok in tokens {
        printer.token(tok, previous);
        previous = Some(tok);
    }
    printer.finish()
}

/// The state of the output: which file and line it is standing on.
struct Printer<'a> {
    out: String,
    opts: PrintOptions,
    sources: &'a SourceMap,
    interner: &'a Interner,
    /// The file the output is currently in.
    file: FileId,
    /// The line of that file the current output line stands for.
    line: u32,
    /// Whether anything has been written on the current output line.
    printed: bool,
    /// The include stack as the output has walked it, which is what decides whether a marker
    /// says entering or returning. It is the output's own stack rather than the
    /// preprocessor's, because by the time this runs the preprocessor's is long gone.
    stack: Vec<FileId>,
}

impl Printer<'_> {
    /// The marker that says which file the output starts in.
    fn start(&mut self) {
        if self.opts.line_markers {
            self.out.push_str(&format!("# 1 {}\n", quoted(&self.sources.file(self.file).name)));
        }
    }

    /// Writes one token, with whatever whitespace has to come before it.
    fn token(&mut self, tok: Tok, previous: Option<Tok>) {
        let at = tok.report_span().lo;
        // A token the preprocessor made up rather than read has no position to move to, so it
        // stays on whatever line the output is already on. `_Pragma` produces these.
        if let Some(loc) = self.sources.lookup(at) {
            self.move_to(loc.file, loc.line, loc.column);
        }
        let text = spelling(tok, self.interner);
        if self.space_before(tok, text, previous) {
            self.out.push(' ');
        }
        self.out.push_str(text);
        self.printed = true;
    }

    /// Whether a space goes between the previous token and this one.
    ///
    /// A run of spaces in the input is one space here, which is what GCC does. The indentation
    /// of a line is the exception and it is rebuilt from the column instead, so the space this
    /// returns for the first token of a line is the last of the ones `indent` wrote.
    fn space_before(&self, tok: Tok, text: &str, previous: Option<Tok>) -> bool {
        if tok.flags.has(TokenFlags::LEADING_SPACE) {
            return true;
        }
        match previous {
            Some(prev) if self.printed => {
                avoid_paste(prev, spelling(prev, self.interner), tok, text)
            }
            _ => false,
        }
    }

    /// Moves the output to a file and a line, printing whatever that takes.
    fn move_to(&mut self, file: FileId, line: u32, column: u32) {
        if file == self.file && line == self.line && self.printed {
            return;
        }
        self.end_line();
        if file != self.file {
            self.marker(file, line);
        } else if line > self.line && line - self.line <= MAX_BLANKS {
            // Close enough to walk to. Under `-P` the walk is skipped and the lines simply
            // follow each other, which is what makes `-P` output compact.
            if self.opts.line_markers {
                for _ in self.line..line {
                    self.out.push('\n');
                }
            }
            self.line = line;
        } else if line != self.line {
            // Too far to walk, or backwards, which happens when a macro invocation spans lines
            // and the tokens after it are reported at the line it started on.
            self.jump(file, line);
        }
        self.indent(column);
    }

    /// Ends the current output line, if anything is on it.
    fn end_line(&mut self) {
        if self.printed {
            self.out.push('\n');
            self.line += 1;
            self.printed = false;
        }
    }

    /// A marker that says the output has changed file.
    fn marker(&mut self, file: FileId, line: u32) {
        // Entering or returning is decided by whether the file is already on the stack. A file
        // that is not is one the output has not been in, which is an entry however it was
        // reached.
        let flag = match self.stack.iter().position(|&f| f == file) {
            Some(at) => {
                self.stack.truncate(at + 1);
                2
            }
            None => {
                self.stack.push(file);
                1
            }
        };
        if self.opts.line_markers {
            let name = quoted(&self.sources.file(file).name);
            self.out.push_str(&format!("# {line} {name} {flag}\n"));
        }
        self.file = file;
        self.line = line;
    }

    /// A marker that says the output has moved within the same file.
    fn jump(&mut self, file: FileId, line: u32) {
        if self.opts.line_markers {
            let name = quoted(&self.sources.file(file).name);
            self.out.push_str(&format!("# {line} {name}\n"));
        }
        self.line = line;
    }

    /// Indents the first token of a line to the column it was written at.
    ///
    /// One space short of the column, because the token's own leading space flag supplies the
    /// last one. GCC does exactly this, and the reason to copy it rather than to print the
    /// tokens flush left is that indentation is most of what makes preprocessed output
    /// readable when something has gone wrong in it.
    fn indent(&mut self, column: u32) {
        if self.printed {
            return;
        }
        for _ in 2..column {
            self.out.push(' ');
        }
    }

    /// The finished text, which always ends in a newline.
    fn finish(mut self) -> String {
        if self.printed {
            self.out.push('\n');
        }
        self.out
    }
}

/// Whether writing these two tokens next to each other would change what they say.
///
/// This is GCC's `cpp_avoid_paste` with the same answers, written over spellings rather than
/// over token codes. The word case is deliberately wider than GCC's: an identifier followed by
/// a number gets a space here, because `x` and `1` written together are the single identifier
/// `x1`, and output that does not read back as itself is not output.
fn avoid_paste(prev: Tok, prev_text: &str, next: Tok, next_text: &str) -> bool {
    let Some(first) = next_text.chars().next() else {
        return false;
    };
    // Anything that ends in a word character followed by anything that starts as one. This
    // covers name and name, name and number, number and number, and the prefixed forms of a
    // character constant and a string literal, which are a name followed by a quote.
    let word = matches!(prev.kind, PpTokenKind::Ident | PpTokenKind::Number | PpTokenKind::Other);
    if word {
        let joins = matches!(
            next.kind,
            PpTokenKind::Ident
                | PpTokenKind::Number
                | PpTokenKind::CharConst
                | PpTokenKind::StringLit
        );
        if joins {
            return true;
        }
        // A pp-number swallows a following sign after an exponent, and a `.` either side of
        // one is part of the number rather than a separate token.
        if prev.kind == PpTokenKind::Number {
            return matches!(first, '.' | '+' | '-');
        }
        return false;
    }

    // An `=` glues onto every operator that has a compound assignment form, and onto the
    // comparisons, which is most of them, so it is asked first.
    if first == '=' {
        return matches!(
            prev_text,
            "=" | "!" | "<" | ">" | "+" | "-" | "*" | "/" | "%" | "&" | "|" | "^" | "<<" | ">>"
        );
    }
    match prev_text {
        ">" => first == '>',
        "<" => matches!(first, '<' | '%' | ':'),
        "+" => first == '+',
        "-" => matches!(first, '-' | '>'),
        // Not an operator that pastes: `/` and `*` written together open a comment, and `//`
        // swallows the rest of the line.
        "/" => matches!(first, '/' | '*'),
        "%" => matches!(first, ':' | '%' | '>'),
        "&" => first == '&',
        "|" => first == '|',
        ":" => matches!(first, ':' | '>'),
        "." => first == '.' || next.kind == PpTokenKind::Number,
        "#" => matches!(first, '#' | '%'),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use rucc_diag::SourceMap;
    use rucc_session::{MemoryFileSystem, SearchPath};

    use super::*;
    use crate::directive::Preprocessor;
    use crate::include::Context;

    /// A translation unit through phase 4 and back out as text.
    struct Run {
        interner: Interner,
        sources: SourceMap,
        fs: MemoryFileSystem,
        search: SearchPath,
        pp: Preprocessor,
    }

    impl Run {
        fn new() -> Run {
            Run {
                interner: Interner::new(),
                sources: SourceMap::new(),
                fs: MemoryFileSystem::new(),
                search: SearchPath::new(),
                pp: Preprocessor::new(),
            }
        }

        fn file(&mut self, path: &str, contents: &str) {
            self.fs.insert(path, contents.as_bytes().to_vec());
        }

        fn go(&mut self, src: &str) -> String {
            self.print(src, PrintOptions::new())
        }

        fn print(&mut self, src: &str, opts: PrintOptions) -> String {
            let main =
                self.sources.add("/main.c", src.as_bytes().to_vec()).expect("the map has room");
            let out = {
                let mut cx =
                    Context::new(&mut self.interner, &mut self.sources, &self.fs, &self.search);
                self.pp.run(main, &mut cx)
            };
            assert!(self.pp.diagnostics().is_empty(), "{:?}", self.pp.diagnostics());
            print(main, &out, &self.sources, &self.interner, opts)
        }
    }

    #[test]
    fn the_first_line_says_which_file_this_is() {
        let mut run = Run::new();
        assert_eq!(run.go("int x;\n"), "# 1 \"/main.c\"\nint x;\n");
    }

    #[test]
    fn a_line_the_preprocessor_ate_comes_back_as_a_blank_one() {
        let mut run = Run::new();
        // The definition produced no tokens, so line 2 is blank and `x` is still on line 3.
        // Keeping it there is what lets a diagnostic from a later phase name the right line.
        assert_eq!(run.go("#define N 1\nint x;\n"), "# 1 \"/main.c\"\n\nint x;\n");
    }

    #[test]
    fn a_long_gap_is_a_marker_rather_than_a_page_of_blank_lines() {
        let mut run = Run::new();
        let src = format!("a;{}b;\n", "\n".repeat(20));
        let text = run.go(&src);
        assert!(text.contains("# 21 \"/main.c\"\nb;\n"), "{text}");
        assert!(!text.contains("\n\n\n"), "a gap that big is a marker, not blank lines: {text}");
    }

    #[test]
    fn entering_and_leaving_a_header_are_both_marked() {
        let mut run = Run::new();
        run.file("/one.h", "int from_the_header;\n");
        let text = run.go("#include \"one.h\"\nint after;\n");
        assert_eq!(
            text,
            "# 1 \"/main.c\"\n\
             # 1 \"/one.h\" 1\n\
             int from_the_header;\n\
             # 2 \"/main.c\" 2\n\
             int after;\n"
        );
    }

    #[test]
    fn dash_p_prints_the_tokens_and_nothing_else() {
        let mut run = Run::new();
        run.file("/one.h", "int from_the_header;\n");
        let src = "#include \"one.h\"\n\n\n\nint after;\n";
        let text = run.print(src, PrintOptions { line_markers: false });
        assert_eq!(text, "int from_the_header;\nint after;\n");
    }

    #[test]
    fn indentation_survives() {
        let mut run = Run::new();
        assert_eq!(run.go("    int x;\n"), "# 1 \"/main.c\"\n    int x;\n");
    }

    #[test]
    fn a_space_goes_in_where_the_tokens_would_otherwise_paste() {
        let mut run = Run::new();
        // `+ +` rather than `++`, and `- -` rather than `--`, because those are different
        // operators and the output has to say what the input said.
        let src = "#define P +\n#define M -\nP+x;\nM-x;\n";
        assert_eq!(run.go(src), "# 1 \"/main.c\"\n\n\n+ +x;\n- -x;\n");
    }

    #[test]
    fn a_name_and_a_number_do_not_run_together() {
        let mut run = Run::new();
        // `x1` would read back as one identifier, so the space is not optional.
        assert_eq!(run.go("#define J(a,b) a b\nJ(x,1)J(2,y)\n"), "# 1 \"/main.c\"\n\nx 1 2 y\n");
    }

    #[test]
    fn a_slash_and_a_star_do_not_open_a_comment() {
        let mut run = Run::new();
        assert_eq!(run.go("#define D /\nD*p;\n"), "# 1 \"/main.c\"\n\n/ *p;\n");
    }

    #[test]
    fn a_run_of_spaces_is_one_space_and_the_indent_is_the_real_one() {
        let mut run = Run::new();
        // GCC collapses whitespace between tokens to one space and rebuilds the indentation
        // from the column, so a line that was indented by two still is.
        assert_eq!(run.go("  int   x = a+b;\n"), "# 1 \"/main.c\"\n  int x = a+b;\n");
    }

    #[test]
    fn a_macro_that_spans_lines_leaves_the_output_where_the_call_was() {
        let mut run = Run::new();
        let text = run.go("#define ADD(a, b) a + b\nADD(1,\n    2)\nlast;\n");
        assert_eq!(text, "# 1 \"/main.c\"\n\n1 + 2\n\nlast;\n");
    }
}
