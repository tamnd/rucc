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

use crate::directive::LineDirective;
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
/// when the first token comes from a header. `lines` is the `#line` directives the run read,
/// in the order it read them, because each one is a marker in the output at the point it was
/// written rather than at the point its effect is first visible.
pub fn print(
    main: FileId,
    tokens: &[Tok],
    lines: &[LineDirective],
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
        name: sources.file(main).name.clone(),
        line: 1,
        printed: false,
        stack: vec![main],
        lines,
        next: 0,
    };
    printer.start();
    let mut previous: Option<Tok> = None;
    for (at, &tok) in tokens.iter().enumerate() {
        printer.line_directives(at);
        printer.token(tok, previous);
        previous = Some(tok);
    }
    printer.line_directives(tokens.len());
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
    /// The name that file is going under, which a `#line` can change without the output
    /// leaving the file. It is held rather than looked up because it is what the next marker
    /// is compared against, and the comparison is per token.
    name: String,
    /// The line of that file the current output line stands for, presented rather than real,
    /// since a marker is what tells the next compiler along where it is.
    line: u32,
    /// Whether anything has been written on the current output line.
    printed: bool,
    /// The include stack as the output has walked it, which is what decides whether a marker
    /// says entering or returning. It is the output's own stack rather than the
    /// preprocessor's, because by the time this runs the preprocessor's is long gone.
    stack: Vec<FileId>,
    /// The `#line` directives, in the order they were read.
    lines: &'a [LineDirective],
    /// How many of them have been written out.
    next: usize,
}

impl Printer<'_> {
    /// Writes the marker for every `#line` that was read before the token at `at`.
    ///
    /// GCC prints one of these per directive, where the directive was written, and so does
    /// this. Letting the effect show up on its own instead would put the same information in
    /// the output in a different place: `#line 5` followed by three blank lines and a
    /// statement comes out as a marker and three blank lines here, and as eight blank lines
    /// if the printer only ever reacts to the line a token claims to be on.
    fn line_directives(&mut self, at: usize) {
        let (lines, sources) = (self.lines, self.sources);
        while let Some(directive) = lines.get(self.next).filter(|d| d.at <= at) {
            self.next += 1;
            let Some(loc) = sources.presumed_after(directive.span.lo) else { continue };
            self.end_line();
            self.jump(loc.name, loc.line);
        }
    }

    /// The marker that says which file the output starts in.
    fn start(&mut self) {
        if self.opts.line_markers {
            self.out.push_str(&format!("# 1 {}\n", quoted(&self.name)));
        }
    }

    /// Writes one token, with whatever whitespace has to come before it.
    fn token(&mut self, tok: Tok, previous: Option<Tok>) {
        let at = tok.report_span().lo;
        // A token the preprocessor made up rather than read has no position to move to, so it
        // stays on whatever line the output is already on. `_Pragma` produces these.
        // Which file it is in is the real one, since that is what the include stack is kept
        // in, and where it says it is is the presented one, since that is what a marker says.
        // `sources` is copied out of `self` so that the borrow of the name outlives the call
        // that needs `self` mutably. It is a shared reference the printer does not own.
        let sources = self.sources;
        if let Some(file) = sources.lookup_file(at) {
            if let Some(loc) = sources.presumed(at) {
                self.move_to(file, loc.name, loc.line, loc.column);
            }
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
    ///
    /// The paste test is asked only where the two tokens did not arrive together. Two tokens
    /// the user wrote next to each other read back as themselves by construction, because they
    /// came out of the lexer that way, so `[52-2*sizeof(x)]` in a header prints as it was
    /// written. It is a macro that can put two tokens next to each other that were never next
    /// to each other, and that is where the question is worth asking. GCC arrives at the same
    /// place from the other end: it inserts padding around each expansion and consults
    /// `cpp_avoid_paste` only where one sits.
    ///
    /// "Arrived together" is the trace rather than the outermost invocation, because the
    /// outermost is the same for every token of a nest and the boundaries inside it are real.
    /// lz4 writes `#define LZ4_HASHLOG (LZ4_MEMORY_USAGE-2)` over a `LZ4_MEMORY_USAGE` of 14,
    /// and the `14` and the `-` are two steps apart, so gcc prints `(14 -2)` and so does this.
    fn space_before(&self, tok: Tok, text: &str, previous: Option<Tok>) -> bool {
        if tok.flags.has(TokenFlags::LEADING_SPACE) {
            return true;
        }
        match previous {
            Some(prev) if self.printed && prev.trace != tok.trace => {
                avoid_paste(prev, spelling(prev, self.interner), tok, text)
            }
            _ => false,
        }
    }

    /// Moves the output to a file and a line, printing whatever that takes.
    fn move_to(&mut self, file: FileId, name: &str, line: u32, column: u32) {
        if file == self.file && line == self.line && self.printed && name == self.name {
            return;
        }
        self.end_line();
        if file != self.file {
            self.marker(file, name, line);
        } else if name != self.name {
            // Same file, different name, which is a `#line` that renamed it. GCC prints that
            // as a plain marker with no flag on it, the same as a jump within a file, because
            // as far as the output is concerned that is what it is.
            self.jump(name, line);
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
            self.jump(name, line);
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
    fn marker(&mut self, file: FileId, name: &str, line: u32) {
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
            self.out.push_str(&format!("# {line} {} {flag}\n", quoted(name)));
        }
        self.file = file;
        self.set_name(name);
        self.line = line;
    }

    /// A marker that says the output has moved within the same file.
    fn jump(&mut self, name: &str, line: u32) {
        if self.opts.line_markers {
            self.out.push_str(&format!("# {line} {}\n", quoted(name)));
        }
        self.set_name(name);
        self.line = line;
    }

    /// Records the name the output is now going under, without allocating when it has not
    /// changed, which is every token of every file that has no `#line` in it.
    fn set_name(&mut self, name: &str) {
        if self.name != name {
            self.name.clear();
            self.name.push_str(name);
        }
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
            print(main, &out, self.pp.line_directives(), &self.sources, &self.interner, opts)
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

    /// The paste test is for tokens a macro put next to each other. Two the user wrote next to
    /// each other came out of the lexer that way and read back as themselves, so nothing is
    /// inserted between them: the kernel's `sound/asound.h` writes an array bound as
    /// `[52-2*sizeof(x)]` and GCC prints it back unchanged.
    #[test]
    fn a_paste_is_only_avoided_where_a_macro_put_the_tokens_together() {
        let mut run = Run::new();
        assert_eq!(
            run.go("char a[52-2*sizeof(int)];\n"),
            "# 1 \"/main.c\"\nchar a[52-2*sizeof(int)];\n"
        );

        // The number comes out of `N` and the sign does not, so they did not arrive together
        // and `52-2` would read back as a different pp-number than the two tokens it is.
        let mut run = Run::new();
        assert_eq!(run.go("#define N 52\nN-2;\n"), "# 1 \"/main.c\"\n\n52 -2;\n");

        // Both out of the same expansion, so the body's own spacing is what is printed.
        let mut run = Run::new();
        assert_eq!(run.go("#define S 41+1\nS;\n"), "# 1 \"/main.c\"\n\n41+1;\n");

        // A nest, which is lz4's `#define LZ4_HASHLOG (LZ4_MEMORY_USAGE-2)` cut down. The two
        // tokens share an outermost invocation and are still a step apart, and gcc prints the
        // space, so the question is asked of the trace rather than of the outermost.
        let mut run = Run::new();
        assert_eq!(
            run.go("#define A 14\n#define B (A-2)\nint t[1 << B];\n"),
            "# 1 \"/main.c\"\n\n\nint t[1 << (14 -2)];\n"
        );
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

    #[test]
    fn a_macro_that_expands_to_nothing_leaves_its_space_behind() {
        let mut run = Run::new();
        // GCC and clang both print `int a ;` here, and the space is not decoration. The glibc
        // headers hang `__THROW` and its relatives off the end of several hundred prototypes
        // per file, and on a dialect where those expand to nothing this one space is the whole
        // difference between agreeing with the reference compiler and not.
        let text = run.print("#define E\nint a E;\n", PrintOptions { line_markers: false });
        assert_eq!(text, "int a ;\n");
    }

    #[test]
    fn the_space_is_only_left_where_there_was_one() {
        let mut run = Run::new();
        // No space before the macro means no space after it. `a1(E);` is `a1();` and not
        // `a1( );`, which is the case that stops this rule from turning into "always insert".
        let text = run.print("#define E\na1(E);\n", PrintOptions { line_markers: false });
        assert_eq!(text, "a1();\n");
    }

    #[test]
    fn a_space_owed_by_one_empty_macro_is_not_paid_twice() {
        let mut run = Run::new();
        // Three vanishing macros in a row owe one space between them, not three. The debt is
        // handed along until a token that survives takes it.
        let text = run.print("#define E\nd1 E E E d2;\n", PrintOptions { line_markers: false });
        assert_eq!(text, "d1 d2;\n");
    }

    #[test]
    fn the_space_crosses_out_of_the_expansion_that_owed_it() {
        let mut run = Run::new();
        // `J(4)` expands to `4 E`, and the `E` vanishes at the end of the replacement list. The
        // token that takes the space is the `;` from the source, which the expansion never saw.
        let text = run
            .print("#define E\n#define J(x) x E\np6 J(4);\n", PrintOptions { line_markers: false });
        assert_eq!(text, "p6 4 ;\n");
    }

    #[test]
    fn a_function_like_macro_with_an_empty_body_leaves_a_space_too() {
        let mut run = Run::new();
        // The rule is about the invocation vanishing, not about which kind of macro it was.
        let text = run
            .print("#define F(x)\nint d(int F(9), int);\n", PrintOptions { line_markers: false });
        assert_eq!(text, "int d(int , int);\n");
    }

    #[test]
    fn a_line_directive_is_a_marker_where_it_was_written() {
        let mut run = Run::new();
        // GCC writes the marker at the directive and then walks the three blank lines from
        // there. Reacting to the line the statement claims to be on instead would put the same
        // information in the output as eight blank lines and no marker.
        let text = run.go("#line 5\n\n\n\nint a;\n");
        assert_eq!(text, "# 1 \"/main.c\"\n# 5 \"/main.c\"\n\n\n\nint a;\n");
    }

    #[test]
    fn two_directives_in_a_row_are_two_markers() {
        let mut run = Run::new();
        assert_eq!(
            run.go("#line 5\n#line 9\nint a;\n"),
            "# 1 \"/main.c\"\n# 5 \"/main.c\"\n# 9 \"/main.c\"\nint a;\n"
        );
    }

    #[test]
    fn a_directive_with_nothing_after_it_still_writes_its_marker() {
        let mut run = Run::new();
        assert_eq!(run.go("int a;\n#line 5\n"), "# 1 \"/main.c\"\nint a;\n# 5 \"/main.c\"\n");
    }

    #[test]
    fn the_marker_goes_where_the_directive_is_and_not_where_its_bytes_are() {
        let mut run = Run::new();
        // The header is added to the source map after the file that includes it, so its bytes
        // come after every byte of this file, including the ones after the `#include`. A
        // printer that ordered the markers by position would write the rename after the
        // header rather than before it.
        run.file("/one.h", "int in_header;\n");
        let text = run.go("#line 900 \"outer\"\n#include \"one.h\"\nint after;\n");
        assert_eq!(
            text,
            "# 1 \"/main.c\"\n\
             # 900 \"outer\"\n\
             # 1 \"/one.h\" 1\n\
             int in_header;\n\
             # 901 \"outer\" 2\n\
             int after;\n"
        );
    }

    #[test]
    fn dash_p_drops_the_markers_a_directive_makes_like_every_other_one() {
        let mut run = Run::new();
        let text = run.print("#line 900 \"outer\"\nint a;\n", PrintOptions { line_markers: false });
        assert_eq!(text, "int a;\n");
    }
}
