//! `#pragma GCC target`, `optimize`, `push_options`, `pop_options` and `reset_options`, which
//! give the functions that follow them the attributes of the same names.
//!
//! The lines are read as the parser walks past them, which is how `pack.rs` reads its own, and
//! what is in effect where a declaration starts is written on its specifiers. Sema reads it there
//! ahead of the declaration's own attributes, so `#pragma GCC target("avx2")` over a function is
//! `__attribute__((target("avx2")))` on it, and a function that also says `target("bmi")` is
//! built for both. gcc applies the lines to declarations as well as definitions, and a function
//! declared under one and defined after the `pop_options` keeps what its declaration said.
//!
//! Each `target` or `optimize` line adds to what the ones before it said rather than replacing
//! it, `push_options` saves both lists, `pop_options` puts back the last saved, and
//! `reset_options` empties them. The lines are measured against gcc 13, which says nothing about
//! a line it reads cleanly, warns under `-Wpragmas` about one it cannot read and ignores it, and
//! refuses one with something after the strings.
//!
//! What a string says is not looked at here, since that depends on the target: a name gcc does
//! not know is refused by sema, on the first function the line reaches, with gcc's words.

use rucc_ast::PragmaOptions;
use rucc_diag::Span;
use rucc_lex::{Encoding, Remarks, StringLiteral, Token};

use crate::pack::eat_punct;
use crate::parser::Parser;

/// What the lines have added up to, and what `push_options` saved.
#[derive(Debug, Default)]
pub(crate) struct Options {
    /// What is in effect.
    current: State,
    /// What `push_options` saved, innermost last.
    saved: Vec<State>,
}

/// The options in effect at one point in the file.
#[derive(Debug, Clone, Default)]
struct State {
    /// What the `target` lines said, one entry per string.
    target: Vec<String>,
    /// What the `optimize` lines said, one entry per string or number.
    optimize: Vec<String>,
    /// The two lists as the tree holds them, written again when a line changes one, so that
    /// every declaration under the same lines shares the same strings.
    written: PragmaOptions,
}

impl Parser<'_> {
    /// What `#pragma GCC target` and `optimize` have in effect at the cursor.
    pub(crate) fn options_in_effect(&mut self) -> PragmaOptions {
        self.read_to_cursor();
        self.packs.options.current.written
    }

    /// One `#pragma GCC` line whose second word is one of the five, `GCC` included.
    pub(crate) fn options_line(&mut self, line: &[Token]) {
        // gcc puts every complaint but one about these lines at `GCC`.
        let at = line[0].span;
        let Some(word) = line.get(1).and_then(|token| token.ident()) else { return };
        let word = self.cx.interner.resolve(word);
        let rest = &line[2..];
        let end = Span::empty_at(line[line.len() - 1].span.hi);
        match word {
            "target" | "optimize" => {
                let Some(added) = self.options_list(word, rest, at, end) else { return };
                let state = &mut self.packs.options.current;
                let (list, joined) = if word == "target" {
                    (&mut state.target, &mut state.written.target)
                } else {
                    (&mut state.optimize, &mut state.written.optimize)
                };
                list.extend(added);
                let text = list.join(",");
                if !text.is_empty() {
                    let literal = StringLiteral {
                        elements: text.bytes().map(u32::from).collect(),
                        encoding: Encoding::Plain,
                        remarks: Remarks::default(),
                    };
                    *joined = Some(self.ast.add_string(literal));
                }
            }
            // gcc names these three without the `GCC`, and leaves what is in effect alone.
            _ if !rest.is_empty() => {
                self.warn("E0798", format!("junk at end of `#pragma {word}`"), at);
            }
            "push_options" => {
                let saved = self.packs.options.current.clone();
                self.packs.options.saved.push(saved);
            }
            "pop_options" => match self.packs.options.saved.pop() {
                Some(saved) => self.packs.options.current = saved,
                None => {
                    let what = "`#pragma GCC pop_options` without a corresponding \
                                `#pragma GCC push_options`";
                    self.warn("E0798", what, at);
                }
            },
            _ => self.packs.options.current = State::default(),
        }
    }

    /// The strings of a `target` or `optimize` line, and the numbers of an `optimize` one as the
    /// levels they stand for, or nothing for a line that is not one. An empty string adds nothing.
    ///
    /// `at` is where `GCC` was written and `end` is just past the end of the line.
    fn options_list(
        &mut self,
        word: &str,
        line: &[Token],
        at: Span,
        end: Span,
    ) -> Option<Vec<String>> {
        let mut rest = line;
        let parenthesised = eat_punct(&mut rest, "(");
        let mut added = Vec::new();
        let mut read = false;
        loop {
            if let Some(text) = self.comment_text(&mut rest) {
                if !text.is_empty() {
                    added.push(text);
                }
            } else if let Some(level) = rest
                .first()
                .filter(|_| word == "optimize")
                .and_then(|&token| self.tokens.int(token))
            {
                added.push(format!("O{}", level.value));
                rest = &rest[1..];
            } else {
                break;
            }
            read = true;
            while eat_punct(&mut rest, ",") {}
        }
        if !read {
            if word == "optimize" {
                self.warn("E0798", "`#pragma GCC optimize` is not a string or number", at);
            } else {
                // At what stands where the string should, or just past the end of the line, and
                // naming the pragma by what gcc once called it.
                let there = rest.first().map_or(end, |token| token.span);
                self.warn("E0798", "`#pragma GCC option` is not a string", there);
            }
            return None;
        }
        if parenthesised && !eat_punct(&mut rest, ")") {
            let what =
                format!("`#pragma GCC {word} (string [,string]...)` does not have a final `)`");
            self.warn("E0798", what, at);
            return None;
        }
        if !rest.is_empty() {
            self.error("E0813", format!("`#pragma GCC {word}` string is badly formed"), at);
            return None;
        }
        Some(added)
    }
}
