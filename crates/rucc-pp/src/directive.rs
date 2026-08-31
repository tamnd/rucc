//! The directive engine: translation phase 4 over one file's preprocessing tokens.
//!
//! Design: `spec/05-preprocessor.md` section 5.4.
//!
//! A directive is a line whose first token is `#`. That is the whole of the recognition rule,
//! and the two halves of it are both load bearing: `#` has to be first on the line, and the
//! line is what the lexer says it is after splices and comments have been resolved, which is
//! why `x /*\n*/ #define F 1` really does define `F`.
//!
//! The part that is easy to get wrong is skipped regions. Inside `#if 0` a line beginning with
//! `#` still has to be recognised well enough to keep the conditional nesting balanced, and it
//! must not be diagnosed for anything else. Real code puts prose, unbalanced quotes and future
//! syntax inside `#if 0`, and a preprocessor that reports errors from there is unusable. So
//! skipping looks at the directive name and nothing else, and only the seven conditional
//! directives mean anything while it is going on.

use rucc_base::{Interner, Symbol};
use rucc_diag::{Diagnostic, Span};
use rucc_lex::{Options, PpToken, PpTokenKind, Punct, TokenFlags, tokenize};

use crate::cond;
use crate::expand::Expander;
use crate::macros::{MacroTable, parse_define};
use crate::token::Tok;

/// One `#if` and everything hanging off it.
#[derive(Debug)]
struct Cond {
    /// Where the `#if` was written, so an unterminated one can point at it.
    span: Span,
    /// Whether tokens in the branch currently open are kept. Already accounts for whether the
    /// enclosing region was live, so [`Preprocessor::live`] only has to look at the top.
    live: bool,
    /// Whether some branch of this chain has been taken. A later `#elif` is not evaluated once
    /// this is set, which is what makes `#elif 1/0` after a taken branch legal.
    taken: bool,
    /// Whether the enclosing region was live.
    enclosing_live: bool,
    /// Whether `#else` has been seen, so a second one is an error.
    seen_else: bool,
}

/// A `#line` directive, kept for the source map to apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineDirective {
    /// Where the directive is.
    pub span: Span,
    /// The line number the next line is to be called.
    pub line: u32,
    /// The file name the following lines are to be called, if one was given.
    pub file: Option<Symbol>,
}

/// Translation phase 4 over one file.
///
/// Holds the macro table and the conditional stack, so a single instance processes a whole
/// translation unit and the definitions a header makes are visible after it.
#[derive(Debug, Default)]
pub struct Preprocessor {
    macros: MacroTable,
    expander: Expander,
    diagnostics: Vec<Diagnostic>,
    conds: Vec<Cond>,
    lines: Vec<LineDirective>,
}

impl Preprocessor {
    /// A preprocessor with an empty macro table.
    pub fn new() -> Preprocessor {
        Preprocessor::default()
    }

    /// The macros defined so far.
    pub fn macros(&self) -> &MacroTable {
        &self.macros
    }

    /// The macro table, for the driver to seed with `-D` and the predefined set.
    pub fn macros_mut(&mut self) -> &mut MacroTable {
        &mut self.macros
    }

    /// Everything reported so far.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// Takes the diagnostics, leaving the preprocessor able to carry on.
    pub fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }

    /// The `#line` directives seen, in the order they appeared.
    ///
    /// They are recorded rather than applied because applying one means changing what
    /// `__LINE__` and `__FILE__` say, and neither exists until the source map lands.
    pub fn line_directives(&self) -> &[LineDirective] {
        &self.lines
    }

    /// Runs phase 4 over one file's worth of preprocessing tokens.
    ///
    /// The result is the tokens that survived the conditionals, with macros expanded. Nothing
    /// is thrown away silently: an unterminated `#if` and a stray `#endif` are both reported.
    pub fn run(&mut self, tokens: &[PpToken], interner: &mut Interner) -> Vec<Tok> {
        let names = Names::new(interner);
        let depth_on_entry = self.conds.len();
        let mut out = Vec::new();
        // Consecutive text lines are expanded as one run rather than line by line, because a
        // function-like macro invocation may span lines. It may not span a directive, which is
        // undefined behaviour, so a directive is where the run ends.
        let mut text: Vec<Tok> = Vec::new();
        let mut at = 0;

        while at < tokens.len() {
            let tok = tokens[at];
            if tok.is_eof() {
                break;
            }
            let end = line_end(tokens, at);
            if is_directive(tok) {
                self.flush(&mut text, &mut out, interner, &names);
                self.directive(&tokens[at + 1..end], tok.span, &mut out, interner, &names);
            } else if self.live() {
                text.extend(tokens[at..end].iter().copied().map(Tok::new));
            }
            at = end;
        }
        self.flush(&mut text, &mut out, interner, &names);

        // A file may not close a conditional it did not open. GCC reports this at the `#if`,
        // which is the line the user has to go and look at.
        for cond in self.conds.drain(depth_on_entry..) {
            self.diagnostics
                .push(Diagnostic::error("unterminated `#if`", cond.span).with_code("E0330"));
        }
        out
    }

    /// Whether tokens are currently being kept.
    fn live(&self) -> bool {
        self.conds.last().is_none_or(|c| c.live)
    }

    /// Expands a run of text lines and appends it to the output.
    fn flush(
        &mut self,
        text: &mut Vec<Tok>,
        out: &mut Vec<Tok>,
        interner: &mut Interner,
        names: &Names,
    ) {
        if text.is_empty() {
            return;
        }
        let taken = std::mem::take(text);
        let expanded = self.expander.expand_toks(taken, &self.macros, interner);
        self.diagnostics.append(&mut self.expander.take_diagnostics());
        self.pragma_operator(expanded, out, interner, names);
    }

    /// Dispatches one directive. `body` is the line after the `#`.
    fn directive(
        &mut self,
        body: &[PpToken],
        hash: Span,
        out: &mut Vec<Tok>,
        interner: &mut Interner,
        names: &Names,
    ) {
        // The null directive. A line of just `#` is legal and does nothing, and there is a
        // surprising amount of it in real headers as a visual separator.
        let Some(first) = body.first().copied() else {
            return;
        };
        let name = ident_of(&first);
        let rest = &body[1..];

        // Conditionals are handled whether or not the region is live, because the nesting has
        // to stay balanced through a skipped block.
        if name == Some(names.r#if) {
            let value = self.live() && self.eval(rest, hash, interner, names);
            self.open(hash, value);
            return;
        }
        if name == Some(names.ifdef) || name == Some(names.ifndef) {
            let want = name == Some(names.ifdef);
            let value = self.live() && self.defined_check(rest, hash, want);
            self.open(hash, value);
            return;
        }
        if name == Some(names.elif) || name == Some(names.elifdef) || name == Some(names.elifndef) {
            self.elif(name, rest, hash, interner, names);
            return;
        }
        if name == Some(names.r#else) {
            self.branch_else(rest, hash);
            return;
        }
        if name == Some(names.endif) {
            self.endif(rest, hash);
            return;
        }
        if !self.live() {
            // Everything else inside a skipped region is text, not a directive. `#error` in
            // the branch that was not taken must not fire, and `# 42 "f.c"` from another
            // preprocessor must not be diagnosed.
            return;
        }

        if name == Some(names.define) {
            let (def, diagnostics) = parse_define(rest, interner);
            self.diagnostics.extend(diagnostics);
            if let Some(def) = def {
                if let Some(problem) = self.macros.define(def, interner) {
                    self.diagnostics.push(problem);
                }
            }
        } else if name == Some(names.undef) {
            self.undef(rest, hash, interner);
        } else if name == Some(names.error) || name == Some(names.warning) {
            self.message(rest, hash, name == Some(names.error), interner);
        } else if name == Some(names.line) {
            self.line(rest, hash, interner);
        } else if name == Some(names.pragma) {
            // `#pragma once` needs file identity and lands with include resolution. Everything
            // else is passed through unchanged, which is what `-E` has to print and what a
            // later phase looking for `#pragma pack` will read. Inventing an internal
            // representation now, with no consumer, would only be a thing to migrate later.
            self.pass_through(body, hash, out);
        } else if name == Some(names.include)
            || name == Some(names.include_next)
            || name == Some(names.embed)
        {
            // Recognised so that the line is not reported as an unknown directive, and
            // refused so that nobody mistakes silence for a working include. Resolution, the
            // search path and the source map are the next piece of M1.
            let what = first.value.map_or("include", |v| interner.resolve(v)).to_owned();
            self.diagnostics.push(
                Diagnostic::error(format!("`#{what}` is not implemented yet"), hash)
                    .with_code("E0331")
                    .note(
                        "include resolution lands with the source map, see spec/05-preprocessor.md section 5.5",
                        hash,
                    ),
            );
        } else {
            self.diagnostics.push(
                Diagnostic::error("invalid preprocessing directive", first.span).with_code("E0332"),
            );
        }
    }

    /// Copies a directive line into the output, `#` included.
    fn pass_through(&mut self, body: &[PpToken], hash: Span, out: &mut Vec<Tok>) {
        let _ = self;
        out.push(Tok::synthetic(
            PpTokenKind::Punct(Punct::Hash),
            None,
            TokenFlags::START_OF_LINE,
            hash,
        ));
        out.extend(body.iter().copied().map(Tok::new));
    }

    /// Pushes a conditional whose first branch is or is not taken.
    fn open(&mut self, span: Span, value: bool) {
        let enclosing_live = self.live();
        self.conds.push(Cond {
            span,
            live: enclosing_live && value,
            taken: value,
            enclosing_live,
            seen_else: false,
        });
    }

    fn elif(
        &mut self,
        name: Option<Symbol>,
        rest: &[PpToken],
        hash: Span,
        interner: &mut Interner,
        names: &Names,
    ) {
        let Some(top) = self.conds.last() else {
            self.stray("elif", hash);
            return;
        };
        if top.seen_else {
            self.diagnostics
                .push(Diagnostic::error("`#elif` after `#else`", hash).with_code("E0333"));
            return;
        }
        // Read what is needed before evaluating, because evaluation borrows the whole
        // preprocessor to report into.
        let (enclosing_live, already_taken) = (top.enclosing_live, top.taken);
        let consider = enclosing_live && !already_taken;
        let value = if !consider {
            false
        } else if name == Some(names.elif) {
            self.eval(rest, hash, interner, names)
        } else {
            self.defined_check(rest, hash, name == Some(names.elifdef))
        };
        let top = self.conds.last_mut().expect("checked above and nothing popped");
        top.live = consider && value;
        top.taken = already_taken || value;
    }

    fn branch_else(&mut self, rest: &[PpToken], hash: Span) {
        let Some(top) = self.conds.last_mut() else {
            self.stray("else", hash);
            return;
        };
        if top.seen_else {
            self.diagnostics.push(Diagnostic::error("a second `#else`", hash).with_code("E0333"));
            return;
        }
        top.live = top.enclosing_live && !top.taken;
        top.taken = true;
        top.seen_else = true;
        let enclosing_live = top.enclosing_live;
        if enclosing_live {
            self.extra_tokens(rest, "#else");
        }
    }

    fn endif(&mut self, rest: &[PpToken], hash: Span) {
        if self.conds.pop().is_none() {
            self.stray("endif", hash);
            return;
        }
        if self.live() {
            self.extra_tokens(rest, "#endif");
        }
    }

    fn stray(&mut self, what: &str, hash: Span) {
        self.diagnostics
            .push(Diagnostic::error(format!("`#{what}` without `#if`"), hash).with_code("E0334"));
    }

    /// Warns about tokens after a directive that takes none.
    ///
    /// A warning rather than an error, because `#endif FOO` as a hand written comment is
    /// everywhere in code written before `//` was portable.
    fn extra_tokens(&mut self, rest: &[PpToken], what: &str) {
        if let Some(first) = rest.first() {
            self.diagnostics.push(
                Diagnostic::warning(format!("extra tokens after `{what}`"), first.span)
                    .with_code("W0330"),
            );
        }
    }

    /// Evaluates a `#if` or `#elif` expression.
    fn eval(
        &mut self,
        rest: &[PpToken],
        hash: Span,
        interner: &mut Interner,
        names: &Names,
    ) -> bool {
        let line: Vec<Tok> = rest.iter().copied().map(Tok::new).collect();
        // `defined X` is resolved before expansion, so that `#if defined FOO` does not depend
        // on what `FOO` expands to. It is resolved again afterwards because a macro that
        // expands to `defined(X)` is undefined behaviour that GCC supports and headers use.
        let line = self.resolve_defined(line, interner, names);
        let line = self.expander.expand_toks(line, &self.macros, interner);
        self.diagnostics.append(&mut self.expander.take_diagnostics());
        let line = self.resolve_defined(line, interner, names);
        cond::evaluate(&line, interner, &mut self.diagnostics, hash)
    }

    /// Replaces `defined X` and `defined(X)` with `1` or `0`.
    fn resolve_defined(
        &mut self,
        line: Vec<Tok>,
        interner: &mut Interner,
        names: &Names,
    ) -> Vec<Tok> {
        if !line.iter().any(|t| t.ident() == Some(names.defined)) {
            return line;
        }
        let mut out = Vec::with_capacity(line.len());
        let mut at = 0;
        while at < line.len() {
            let tok = line[at];
            if tok.ident() != Some(names.defined) {
                out.push(tok);
                at += 1;
                continue;
            }
            let parenthesised = line.get(at + 1).is_some_and(|t| t.is(Punct::LParen));
            let name_at = if parenthesised { at + 2 } else { at + 1 };
            let name = line.get(name_at).and_then(|t| t.ident());
            let Some(name) = name else {
                self.diagnostics.push(
                    Diagnostic::error("`defined` without a macro name", tok.report_span())
                        .with_code("E0335"),
                );
                out.push(tok);
                at += 1;
                continue;
            };
            at = name_at + 1;
            if parenthesised {
                if line.get(at).is_some_and(|t| t.is(Punct::RParen)) {
                    at += 1;
                } else {
                    self.diagnostics.push(
                        Diagnostic::error("expected `)` after `defined`", tok.report_span())
                            .with_code("E0335"),
                    );
                }
            }
            let value = self.macros.is_defined(name);
            out.push(number(value, tok.flags, tok.report_span(), interner));
        }
        out
    }

    /// The body of `#ifdef`, `#ifndef`, `#elifdef` and `#elifndef`.
    fn defined_check(&mut self, rest: &[PpToken], hash: Span, want_defined: bool) -> bool {
        let Some(name) = rest.first().and_then(ident_of) else {
            self.diagnostics.push(
                Diagnostic::error("expected a macro name", rest.first().map_or(hash, |t| t.span))
                    .with_code("E0336"),
            );
            return false;
        };
        self.extra_tokens(&rest[1..], if want_defined { "#ifdef" } else { "#ifndef" });
        self.macros.is_defined(name) == want_defined
    }

    fn undef(&mut self, rest: &[PpToken], hash: Span, interner: &Interner) {
        let Some(name) = rest.first().and_then(ident_of) else {
            self.diagnostics.push(
                Diagnostic::error("expected a macro name", rest.first().map_or(hash, |t| t.span))
                    .with_code("E0336"),
            );
            return;
        };
        // The standard reserves these and GCC refuses to let them go, because code that
        // undefines `__FILE__` and then uses it is broken in a way that is very hard to see.
        let text = interner.resolve(name);
        if text == "defined" || text.starts_with("__STDC_") {
            self.diagnostics.push(
                Diagnostic::error(format!("`{text}` cannot be undefined"), rest[0].span)
                    .with_code("E0337"),
            );
            return;
        }
        self.macros.undef(name);
        self.extra_tokens(&rest[1..], "#undef");
    }

    /// `#error` and `#warning`. The message is the rest of the line, spelled back.
    fn message(&mut self, rest: &[PpToken], hash: Span, fatal: bool, interner: &Interner) {
        let text = spell_line(rest, interner);
        let span = rest.first().map_or(hash, |t| t.span.to(last_span(rest)));
        let diag = if fatal {
            Diagnostic::error(text, span).with_code("E0338")
        } else {
            Diagnostic::warning(text, span).with_code("W0331")
        };
        self.diagnostics.push(diag);
    }

    /// `#line 42` and `#line 42 "file.c"`.
    ///
    /// The argument is macro expanded first, which is the one place a directive other than
    /// `#if` does that, and which exists because `#line __LINE__ + 1` is real code.
    fn line(&mut self, rest: &[PpToken], hash: Span, interner: &mut Interner) {
        let line: Vec<Tok> = rest.iter().copied().map(Tok::new).collect();
        let line = self.expander.expand_toks(line, &self.macros, interner);
        self.diagnostics.append(&mut self.expander.take_diagnostics());

        let number_text = line
            .first()
            .filter(|t| t.kind == PpTokenKind::Number)
            .and_then(|t| t.value)
            .map(|v| interner.resolve(v));
        let Some(parsed) = number_text.and_then(|t| t.parse::<u64>().ok()) else {
            self.diagnostics.push(
                Diagnostic::error(
                    "`#line` needs a decimal line number",
                    line.first().map_or(hash, |t| t.report_span()),
                )
                .with_code("E0339"),
            );
            return;
        };
        // 2147483647 is the largest line number the standard requires support for, and it is
        // also where every other compiler stops, so matching that keeps diagnostics comparable.
        if parsed == 0 || parsed > 2_147_483_647 {
            self.diagnostics.push(
                Diagnostic::error("`#line` number is out of range", line[0].report_span())
                    .with_code("E0339"),
            );
            return;
        }

        let mut file = None;
        if let Some(second) = line.get(1) {
            if second.kind == PpTokenKind::StringLit {
                file = second.value;
            } else {
                self.diagnostics.push(
                    Diagnostic::error(
                        "`#line` file name must be a string literal",
                        second.report_span(),
                    )
                    .with_code("E0339"),
                );
                return;
            }
        }
        #[expect(
            clippy::cast_possible_truncation,
            reason = "the range check above keeps this inside i32, let alone u32"
        )]
        self.lines.push(LineDirective { span: hash, line: parsed as u32, file });
    }

    /// Applies the `_Pragma` operator to an expanded run and appends the result.
    ///
    /// `_Pragma("x")` is a pragma written as an expression, which is what makes a pragma
    /// usable from inside a macro. It is handled after expansion because the string it takes
    /// is very often produced by one.
    fn pragma_operator(
        &mut self,
        expanded: Vec<Tok>,
        out: &mut Vec<Tok>,
        interner: &mut Interner,
        names: &Names,
    ) {
        if !expanded.iter().any(|t| t.ident() == Some(names.pragma_op)) {
            out.extend(expanded);
            return;
        }
        let mut at = 0;
        while at < expanded.len() {
            let tok = expanded[at];
            if tok.ident() != Some(names.pragma_op) {
                out.push(tok);
                at += 1;
                continue;
            }
            let open = expanded.get(at + 1).is_some_and(|t| t.is(Punct::LParen));
            let text = expanded.get(at + 2).filter(|t| t.kind == PpTokenKind::StringLit);
            let close = expanded.get(at + 3).is_some_and(|t| t.is(Punct::RParen));
            let (Some(text), true, true) = (text, open, close) else {
                self.diagnostics.push(
                    Diagnostic::error("`_Pragma` takes a single string literal", tok.report_span())
                        .with_code("E0340"),
                );
                out.push(tok);
                at += 1;
                continue;
            };
            let literal = text.value.map(|v| interner.resolve(v)).unwrap_or_default();
            let body = destringize(literal);
            self.emit_pragma(&body, tok, out, interner, names);
            at += 4;
        }
    }

    /// Turns destringized `_Pragma` text into the `# pragma ...` tokens a later phase reads.
    fn emit_pragma(
        &mut self,
        body: &str,
        at: Tok,
        out: &mut Vec<Tok>,
        interner: &mut Interner,
        names: &Names,
    ) {
        let span = at.report_span();
        let (tokens, diagnostics) = tokenize(body.as_bytes(), 0, Options::new(), interner);
        // The text came out of a string literal, so a span into it would point at bytes the
        // user cannot see. Every token reports at the `_Pragma` instead.
        self.diagnostics.extend(
            diagnostics
                .into_iter()
                .map(|d| Diagnostic::new(d.severity, d.message, span).with_code("E0340")),
        );
        out.push(Tok::synthetic(
            PpTokenKind::Punct(Punct::Hash),
            None,
            TokenFlags::START_OF_LINE,
            span,
        ));
        out.push(Tok::synthetic(PpTokenKind::Ident, Some(names.pragma), TokenFlags::EMPTY, span));
        // The tokens keep the spacing they were written with inside the string, so
        // `_Pragma("pack(push)")` prints back as `pack(push)` rather than `pack ( push )`.
        // Only the first one is forced apart, from the `pragma` before it.
        for (at, t) in tokens.into_iter().filter(|t| !t.is_eof()).enumerate() {
            // Start of line has to come off: the line is the `#pragma` we just emitted, not
            // the inside of the string these came from.
            let spaced = at == 0 || t.flags.has(TokenFlags::LEADING_SPACE);
            let flags = if spaced {
                TokenFlags::EMPTY.with(TokenFlags::LEADING_SPACE)
            } else {
                TokenFlags::EMPTY
            };
            out.push(Tok::synthetic(t.kind, t.value, flags, span));
        }
    }
}

/// Whether this token opens a directive line.
fn is_directive(tok: PpToken) -> bool {
    tok.flags.has(TokenFlags::START_OF_LINE) && tok.punct() == Some(Punct::Hash)
}

/// The index one past the end of the line starting at `at`.
fn line_end(tokens: &[PpToken], at: usize) -> usize {
    let mut end = at + 1;
    while end < tokens.len()
        && !tokens[end].is_eof()
        && !tokens[end].flags.has(TokenFlags::START_OF_LINE)
    {
        end += 1;
    }
    end
}

fn ident_of(tok: &PpToken) -> Option<Symbol> {
    match tok.kind {
        PpTokenKind::Ident => tok.value,
        _ => None,
    }
}

fn last_span(tokens: &[PpToken]) -> Span {
    tokens.last().map_or(Span::DUMMY, |t| t.span)
}

/// A synthetic `1` or `0`.
fn number(value: bool, flags: TokenFlags, span: Span, interner: &mut Interner) -> Tok {
    let sym = interner.intern(if value { "1" } else { "0" });
    Tok::synthetic(PpTokenKind::Number, Some(sym), flags, span)
}

/// Spells a directive's tokens back for an `#error` message.
fn spell_line(tokens: &[PpToken], interner: &Interner) -> String {
    let mut out = String::new();
    for (index, tok) in tokens.iter().enumerate() {
        if index > 0 && tok.flags.has(TokenFlags::LEADING_SPACE) {
            out.push(' ');
        }
        match tok.value {
            Some(sym) => out.push_str(interner.resolve(sym)),
            None => {
                if let Some(p) = tok.punct() {
                    out.push_str(p.as_str());
                }
            }
        }
    }
    out
}

/// Undoes what `#` would have done, per C23 6.10.10.
///
/// The `L` or `u8` prefix and the quotes come off, then `\"` becomes `"` and `\\` becomes `\`.
/// No other escape is touched, because no other escape was introduced.
fn destringize(literal: &str) -> String {
    let body = literal
        .trim_start_matches(['L', 'u', 'U', '8'])
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(literal);
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// The directive names and the two operators, interned once per file.
///
/// Comparing symbols rather than strings is the point: a directive line is recognised with
/// integer comparisons, and the identifiers were interned during the scan, so there is no
/// string work in the hot path.
struct Names {
    define: Symbol,
    undef: Symbol,
    r#if: Symbol,
    ifdef: Symbol,
    ifndef: Symbol,
    elif: Symbol,
    elifdef: Symbol,
    elifndef: Symbol,
    r#else: Symbol,
    endif: Symbol,
    line: Symbol,
    error: Symbol,
    warning: Symbol,
    pragma: Symbol,
    include: Symbol,
    include_next: Symbol,
    embed: Symbol,
    defined: Symbol,
    pragma_op: Symbol,
}

impl Names {
    fn new(interner: &mut Interner) -> Names {
        Names {
            define: interner.intern("define"),
            undef: interner.intern("undef"),
            r#if: interner.intern("if"),
            ifdef: interner.intern("ifdef"),
            ifndef: interner.intern("ifndef"),
            elif: interner.intern("elif"),
            elifdef: interner.intern("elifdef"),
            elifndef: interner.intern("elifndef"),
            r#else: interner.intern("else"),
            endif: interner.intern("endif"),
            line: interner.intern("line"),
            error: interner.intern("error"),
            warning: interner.intern("warning"),
            pragma: interner.intern("pragma"),
            include: interner.intern("include"),
            include_next: interner.intern("include_next"),
            embed: interner.intern("embed"),
            defined: interner.intern("defined"),
            pragma_op: interner.intern("_Pragma"),
        }
    }
}

#[cfg(test)]
mod tests {
    use rucc_diag::Severity;

    use super::*;

    /// A whole file through phase 4, which is what almost every test here wants.
    struct Run {
        interner: Interner,
        pp: Preprocessor,
    }

    impl Run {
        fn new() -> Run {
            Run { interner: Interner::new(), pp: Preprocessor::new() }
        }

        /// The surviving tokens, spelled with one space wherever they were separated.
        fn go(&mut self, src: &str) -> String {
            let (tokens, errors) = tokenize(src.as_bytes(), 0, Options::new(), &mut self.interner);
            assert!(errors.is_empty(), "test input should lex cleanly: {errors:?}");
            let out = self.pp.run(&tokens, &mut self.interner);
            let mut text = String::new();
            for (at, tok) in out.iter().enumerate() {
                let spaced = tok.flags.has(TokenFlags::LEADING_SPACE)
                    || tok.flags.has(TokenFlags::START_OF_LINE);
                if at > 0 && spaced {
                    text.push(' ');
                }
                match tok.kind {
                    PpTokenKind::Punct(p) => text.push_str(p.as_str()),
                    _ => text.push_str(
                        self.interner.resolve(tok.value.expect("every non-punctuator interns")),
                    ),
                }
            }
            text
        }

        fn messages(&mut self) -> Vec<String> {
            self.pp.take_diagnostics().into_iter().map(|d| d.message).collect()
        }

        fn severities(&mut self) -> Vec<Severity> {
            self.pp.diagnostics().iter().map(|d| d.severity).collect()
        }
    }

    fn clean(src: &str) -> String {
        let mut run = Run::new();
        let text = run.go(src);
        assert!(run.messages().is_empty(), "expected no diagnostics from {src:?}");
        text
    }

    #[test]
    fn a_taken_branch_is_kept_and_the_other_is_not() {
        assert_eq!(clean("#if 1\nyes\n#else\nno\n#endif\n"), "yes");
        assert_eq!(clean("#if 0\nyes\n#else\nno\n#endif\n"), "no");
    }

    #[test]
    fn ifdef_and_ifndef_ask_the_macro_table() {
        assert_eq!(clean("#define F 1\n#ifdef F\nyes\n#endif\n"), "yes");
        assert_eq!(clean("#ifdef F\nyes\n#endif\n"), "");
        assert_eq!(clean("#ifndef F\nyes\n#endif\n"), "yes");
        // C23 spells the two of them as `#elifdef` and `#elifndef` as well.
        assert_eq!(clean("#define F 1\n#if 0\na\n#elifdef F\nb\n#endif\n"), "b");
        assert_eq!(clean("#if 0\na\n#elifndef F\nb\n#endif\n"), "b");
    }

    #[test]
    fn only_the_first_true_branch_of_a_chain_is_taken() {
        assert_eq!(clean("#if 0\na\n#elif 1\nb\n#elif 1\nc\n#else\nd\n#endif\n"), "b");
        assert_eq!(clean("#if 0\na\n#elif 0\nb\n#else\nc\n#endif\n"), "c");
    }

    #[test]
    fn a_branch_after_one_that_was_taken_is_not_evaluated() {
        // `1/0` in a branch that cannot be reached is legal, and headers rely on it: the
        // guard that made the branch dead is often the thing that made the expression safe.
        assert_eq!(clean("#if 1\na\n#elif 1/0\nb\n#endif\n"), "a");
    }

    #[test]
    fn a_skipped_region_is_not_read_for_anything_but_nesting() {
        // Prose, an unknown directive and a broken `#define` all have to pass silently.
        let src = "#if 0\nthis is not C at all\n#frobnicate\n#define\n#if 1\ninner\n#endif\n#endif\nafter\n";
        assert_eq!(clean(src), "after");
    }

    #[test]
    fn nesting_inside_a_dead_branch_stays_balanced() {
        let src = "#if 0\n#ifdef X\na\n#else\nb\n#endif\n#else\nc\n#endif\n";
        assert_eq!(clean(src), "c");
    }

    #[test]
    fn defined_works_in_both_spellings_and_before_expansion() {
        assert_eq!(clean("#define F 0\n#if defined F\nyes\n#endif\n"), "yes");
        assert_eq!(clean("#define F 0\n#if defined(F)\nyes\n#endif\n"), "yes");
        assert_eq!(clean("#if defined(F)\nyes\n#endif\n"), "");
        // `F` expands to 0, but `defined F` is answered before that happens, which is the
        // whole reason `defined` is resolved in a pass of its own.
        assert_eq!(clean("#define F 0\n#if defined F && !F\nyes\n#endif\n"), "yes");
    }

    #[test]
    fn an_identifier_that_survived_expansion_is_zero() {
        assert_eq!(clean("#if NOT_DEFINED_ANYWHERE\nyes\n#else\nno\n#endif\n"), "no");
        assert_eq!(clean("#if !NOT_DEFINED_ANYWHERE\nyes\n#endif\n"), "yes");
    }

    #[test]
    fn short_circuiting_keeps_a_guarded_expression_safe() {
        // The reason `&&` has to short circuit rather than merely produce the right answer:
        // the right hand side divides by zero when the guard is false.
        assert_eq!(clean("#if defined(F) && 1/F\nyes\n#else\nno\n#endif\n"), "no");
        assert_eq!(clean("#if 1 ? 2 : 1/0\nyes\n#endif\n"), "yes");
    }

    #[test]
    fn the_operators_have_the_precedence_they_do_in_c() {
        assert_eq!(clean("#if 1 + 2 * 3 == 7\nyes\n#endif\n"), "yes");
        assert_eq!(clean("#if (1 + 2) * 3 == 9\nyes\n#endif\n"), "yes");
        assert_eq!(clean("#if 1 << 4 == 16\nyes\n#endif\n"), "yes");
        assert_eq!(clean("#if -8 / 3 == -2\nyes\n#endif\n"), "yes");
        assert_eq!(clean("#if (0xff & 0x0f) == 15\nyes\n#endif\n"), "yes");
    }

    #[test]
    fn an_unsigned_operand_makes_the_whole_comparison_unsigned() {
        // The rule that catches everyone out in C catches them out here too, and a
        // preprocessor that quietly disagreed with the compiler would be worse than one that
        // is merely surprising.
        assert_eq!(clean("#if -1 < 0u\nyes\n#else\nno\n#endif\n"), "no");
        assert_eq!(clean("#if -1 < 0\nyes\n#else\nno\n#endif\n"), "yes");
    }

    #[test]
    fn character_constants_evaluate() {
        assert_eq!(clean("#if 'A' == 65\nyes\n#endif\n"), "yes");
        assert_eq!(clean("#if '\\n' == 10\nyes\n#endif\n"), "yes");
    }

    #[test]
    fn a_macro_is_expanded_before_the_expression_is_evaluated() {
        assert_eq!(clean("#define V 3\n#if V > 2\nyes\n#endif\n"), "yes");
        assert_eq!(clean("#define M(a) ((a) * 2)\n#if M(3) == 6\nyes\n#endif\n"), "yes");
    }

    #[test]
    fn an_invocation_may_span_lines_within_a_run_of_text() {
        assert_eq!(clean("#define M(a, b) a + b\nM(1,\n2)\n"), "1 + 2");
    }

    #[test]
    fn undef_removes_a_definition() {
        assert_eq!(clean("#define F 1\n#undef F\n#ifdef F\nyes\n#else\nno\n#endif\n"), "no");
        // Undefining something that was never defined is not an error, and configure scripts
        // emit it constantly.
        assert_eq!(clean("#undef NEVER_DEFINED\nok\n"), "ok");
    }

    #[test]
    fn some_names_cannot_be_undefined() {
        let mut run = Run::new();
        run.go("#undef defined\n");
        assert_eq!(run.messages(), vec!["`defined` cannot be undefined".to_owned()]);
    }

    #[test]
    fn error_reports_the_rest_of_the_line() {
        let mut run = Run::new();
        run.go("#if 0\n#error not this one\n#else\n#error unsupported target\n#endif\n");
        assert_eq!(run.messages(), vec!["unsupported target".to_owned()]);
    }

    #[test]
    fn warning_is_a_warning() {
        let mut run = Run::new();
        run.go("#warning this is fine\n");
        assert_eq!(run.severities(), vec![Severity::Warning]);
        assert_eq!(run.messages(), vec!["this is fine".to_owned()]);
    }

    #[test]
    fn an_unterminated_conditional_is_reported() {
        let mut run = Run::new();
        assert_eq!(run.go("#if 1\nyes\n"), "yes");
        assert_eq!(run.messages(), vec!["unterminated `#if`".to_owned()]);
    }

    #[test]
    fn a_conditional_without_an_if_is_reported() {
        let mut run = Run::new();
        run.go("#endif\n");
        assert_eq!(run.messages(), vec!["`#endif` without `#if`".to_owned()]);

        let mut run = Run::new();
        run.go("#if 1\n#else\n#else\n#endif\n");
        assert_eq!(run.messages(), vec!["a second `#else`".to_owned()]);

        let mut run = Run::new();
        run.go("#if 1\n#else\n#elif 1\n#endif\n");
        assert_eq!(run.messages(), vec!["`#elif` after `#else`".to_owned()]);
    }

    #[test]
    fn tokens_after_endif_are_a_warning_rather_than_an_error() {
        // `#endif FOO` as a hand written comment predates `//` being portable and there is a
        // great deal of it about. Refusing to compile it would be correct and useless.
        let mut run = Run::new();
        assert_eq!(run.go("#if 1\nyes\n#endif FOO\n"), "yes");
        assert_eq!(run.severities(), vec![Severity::Warning]);
        assert_eq!(run.messages(), vec!["extra tokens after `#endif`".to_owned()]);
    }

    #[test]
    fn the_null_directive_does_nothing() {
        assert_eq!(clean("#\na\n#\nb\n"), "a b");
    }

    #[test]
    fn an_unknown_directive_is_an_error_when_the_region_is_live() {
        let mut run = Run::new();
        run.go("#frobnicate\n");
        assert_eq!(run.messages(), vec!["invalid preprocessing directive".to_owned()]);
    }

    #[test]
    fn line_is_recorded_for_the_source_map() {
        let mut run = Run::new();
        run.go("#line 42 \"other.c\"\n");
        assert!(run.messages().is_empty());
        let recorded = run.pp.line_directives();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].line, 42);
        let file = recorded[0].file.expect("a file name was given");
        assert_eq!(run.interner.resolve(file), "\"other.c\"");
    }

    #[test]
    fn a_line_number_out_of_range_is_refused() {
        let mut run = Run::new();
        run.go("#line 0\n");
        assert_eq!(run.messages(), vec!["`#line` number is out of range".to_owned()]);

        let mut run = Run::new();
        run.go("#line notanumber\n");
        assert_eq!(run.messages(), vec!["`#line` needs a decimal line number".to_owned()]);
    }

    #[test]
    fn a_pragma_passes_through_unchanged() {
        assert_eq!(clean("#pragma pack(1)\nint x;\n"), "#pragma pack(1) int x;");
    }

    #[test]
    fn the_pragma_operator_becomes_a_pragma() {
        assert_eq!(
            clean("_Pragma(\"GCC visibility push(default)\")\nint x;\n"),
            "#pragma GCC visibility push(default) int x;"
        );
    }

    #[test]
    fn the_pragma_operator_works_from_inside_a_macro() {
        // This is the entire reason `_Pragma` exists: a `#pragma` cannot be written in a macro
        // body, so a header that wants to wrap one has no other option.
        let src = "#define PUSH _Pragma(\"pack(push)\")\nPUSH\nint x;\n";
        assert_eq!(clean(src), "#pragma pack(push) int x;");
    }

    #[test]
    fn a_pragma_operator_that_is_not_given_a_string_is_reported() {
        let mut run = Run::new();
        run.go("_Pragma(x)\n");
        assert_eq!(run.messages(), vec!["`_Pragma` takes a single string literal".to_owned()]);
    }

    #[test]
    fn include_is_refused_rather_than_ignored() {
        let mut run = Run::new();
        run.go("#include <stdio.h>\n");
        assert_eq!(run.messages(), vec!["`#include` is not implemented yet".to_owned()]);
    }

    #[test]
    fn a_directive_may_have_space_before_the_hash_and_after_it() {
        assert_eq!(clean("  #  define F 1\n#ifdef F\nyes\n#endif\n"), "yes");
    }

    #[test]
    fn a_definition_survives_across_a_conditional() {
        assert_eq!(clean("#if 1\n#define F 7\n#endif\nF\n"), "7");
    }

    #[test]
    fn an_empty_if_expression_is_reported() {
        let mut run = Run::new();
        run.go("#if\n#endif\n");
        assert_eq!(run.messages(), vec!["`#if` with no expression".to_owned()]);
    }

    #[test]
    fn destringizing_undoes_what_stringizing_did() {
        assert_eq!(destringize(r#""a \"b\" c""#), r#"a "b" c"#);
        assert_eq!(destringize(r#""a \\ b""#), r"a \ b");
        assert_eq!(destringize(r#"L"wide""#), "wide");
    }
}
