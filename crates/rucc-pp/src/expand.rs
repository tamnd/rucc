//! Macro expansion.
//!
//! Design: `spec/05-preprocessor.md` section 5.3.
//!
//! This is Prosser's algorithm with hide sets, not the expansion-depth approximation. The
//! two agree on everything anybody writes on purpose and disagree on mutually recursive
//! macros, which appear in real headers more often than they should and where being wrong is
//! invisible until it is catastrophic.
//!
//! The shape of it: `expand` walks a stream of tokens with pushback, and when it finds a
//! macro invocation it replaces it with `subst` of the replacement list and pushes that back
//! onto the front of the stream to be rescanned. Rescanning from the front rather than
//! recursing is what lets a replacement consume tokens that follow the invocation, which is
//! required and which is the reason a `Vec` used as a stack shows up here instead of an
//! iterator chain.

use rucc_base::{Interner, Symbol};
use rucc_diag::{Diagnostic, Span};
use rucc_lex::{Options, PpToken, PpTokenKind, Punct, TokenFlags, tokenize};

use crate::hide::{HideSet, HideSets};
use crate::macros::{MacroDef, MacroTable};
use crate::token::Tok;

/// A backstop against a replacement list that grows without bound.
///
/// Hide sets guarantee that expansion terminates, but they say nothing about how large the
/// result gets, and a short chain of macros that each mention the next one twice produces a
/// megabyte from four lines. Real code never comes near this; input designed to hang the
/// compiler does, and `spec/19-risks.md` asks for a bound rather than a hang.
const MAX_STEPS: usize = 1 << 24;

/// Macro expansion state that outlives a single expansion.
///
/// Hide sets are interned for the whole translation unit, because the same set is produced
/// over and over by the same nest of headers and re-interning it is free while re-allocating
/// it is not.
#[derive(Debug, Default)]
pub struct Expander {
    hides: HideSets,
    diagnostics: Vec<Diagnostic>,
}

impl Expander {
    /// A fresh expander.
    pub fn new() -> Expander {
        Expander { hides: HideSets::new(), diagnostics: Vec::new() }
    }

    /// Everything reported so far.
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// Takes the diagnostics, leaving the expander empty.
    pub fn take_diagnostics(&mut self) -> Vec<Diagnostic> {
        std::mem::take(&mut self.diagnostics)
    }

    /// How many distinct hide sets have been interned, which is the number to watch when
    /// this starts costing memory.
    pub fn hide_sets(&self) -> usize {
        self.hides.len()
    }

    /// Expands a run of lexed tokens.
    ///
    /// The input is a directive-free stretch of the file. An `Eof` token is ignored rather
    /// than passed through, because the caller decides where the stream ends.
    pub fn expand(
        &mut self,
        tokens: &[PpToken],
        macros: &MacroTable,
        interner: &mut Interner,
    ) -> Vec<Tok> {
        let input: Vec<Tok> =
            tokens.iter().filter(|t| t.kind != PpTokenKind::Eof).map(|&t| Tok::new(t)).collect();
        self.expand_toks(input, macros, interner)
    }

    /// Expands tokens that already carry hide sets, for a caller that is splicing streams
    /// together itself.
    pub fn expand_toks(
        &mut self,
        tokens: Vec<Tok>,
        macros: &MacroTable,
        interner: &mut Interner,
    ) -> Vec<Tok> {
        let mut run = Run {
            hides: &mut self.hides,
            diagnostics: &mut self.diagnostics,
            macros,
            va_opt: interner.intern("__VA_OPT__"),
            interner,
            steps: 0,
        };
        run.expand(tokens)
    }
}

/// One expansion, holding the pieces borrowed for its duration.
struct Run<'a> {
    hides: &'a mut HideSets,
    diagnostics: &'a mut Vec<Diagnostic>,
    interner: &'a mut Interner,
    macros: &'a MacroTable,
    /// `__VA_OPT__`, interned once rather than looked up per body token.
    va_opt: Symbol,
    steps: usize,
}

impl<'a> Run<'a> {
    /// The main loop.
    ///
    /// There is deliberately no "already decided not to expand" flag here. Whether a token
    /// may expand is entirely a question of its hide set, and hide sets only ever grow as a
    /// token is carried outwards, so a name that was hidden stays hidden. A function-like
    /// macro name left alone because no parenthesis followed it is a different matter: it may
    /// well be invoked later, once the tokens after it have been expanded and the parenthesis
    /// has appeared. `t(t(g)(0) + t)(1)` in the standard's own example depends on that.
    fn expand(&mut self, input: Vec<Tok>) -> Vec<Tok> {
        let macros = self.macros;
        let mut pending = input;
        pending.reverse();
        let mut out: Vec<Tok> = Vec::with_capacity(pending.len());

        while let Some(tok) = pending.pop() {
            self.steps += 1;
            if self.steps > MAX_STEPS {
                self.diagnostics.push(
                    Diagnostic::error("macro expansion is too large", tok.report_span())
                        .with_code("E0310")
                        .note(
                            "expansion stopped here, the rest of the line is not expanded",
                            tok.span,
                        ),
                );
                out.push(tok);
                pending.reverse();
                out.append(&mut pending);
                return out;
            }

            let Some(name) = tok.ident() else {
                out.push(tok);
                continue;
            };
            if self.hides.contains(tok.hides, name) {
                out.push(tok);
                continue;
            }
            let Some(def) = macros.lookup(name) else {
                out.push(tok);
                continue;
            };

            if !def.function_like {
                let hs = self.hides.add(tok.hides, name);
                let mut args = Args::none();
                let replacement = self.subst(def, &mut args, hs, tok);
                push_front(&mut pending, replacement);
                continue;
            }

            // A function-like macro is only invoked when a parenthesis follows. `#define f(x)`
            // followed by a bare `f` is an ordinary identifier and a great deal of code relies
            // on that, `errno` and `assert` among them.
            if !pending.last().is_some_and(|t| t.is(Punct::LParen)) {
                out.push(tok);
                continue;
            }

            let Some((raw, rparen)) = self.collect_args(def, &mut pending, tok) else {
                out.push(tok);
                continue;
            };
            let shared = self.hides.intersect(tok.hides, rparen.hides);
            let hs = self.hides.add(shared, name);
            let mut args = Args::new(raw);
            let replacement = self.subst(def, &mut args, hs, tok);
            push_front(&mut pending, replacement);
        }
        out
    }

    /// Reads an argument list, `pending` positioned on the opening parenthesis.
    ///
    /// Returns the arguments and the closing parenthesis token, whose hide set the caller
    /// needs. Returns `None` after reporting a problem, in which case the macro name is
    /// emitted unexpanded and the argument tokens are dropped, which is what GCC and Clang
    /// both do: an argument list that does not fit the macro has no useful reading and
    /// putting it back only produces a second error from the parser.
    fn collect_args(
        &mut self,
        def: &MacroDef,
        pending: &mut Vec<Tok>,
        name: Tok,
    ) -> Option<(Vec<Vec<Tok>>, Tok)> {
        let open = pending.pop().expect("the caller checked for an opening parenthesis");
        let mut args: Vec<Vec<Tok>> = Vec::with_capacity(def.arity() + 1);
        let mut current: Vec<Tok> = Vec::new();
        let mut depth = 1usize;
        let rparen = loop {
            let Some(tok) = pending.pop() else {
                self.diagnostics.push(
                    Diagnostic::error("unterminated macro argument list", open.report_span())
                        .with_code("E0311")
                        .note("this macro was invoked here", name.report_span()),
                );
                return None;
            };
            match tok.punct() {
                Some(Punct::LParen) => {
                    depth += 1;
                    current.push(tok);
                }
                Some(Punct::RParen) => {
                    depth -= 1;
                    if depth == 0 {
                        break tok;
                    }
                    current.push(tok);
                }
                // Once the named parameters are filled, a variadic macro's remaining commas
                // are part of the last argument rather than separators.
                Some(Punct::Comma)
                    if depth == 1 && !(def.is_variadic() && args.len() >= def.arity()) =>
                {
                    args.push(std::mem::take(&mut current));
                }
                _ => current.push(tok),
            }
        };

        // `F()` on a macro that takes nothing is no arguments. On a macro that takes one, the
        // same text is one empty argument, which is why this cannot be decided by looking at
        // the tokens alone.
        let empty_invocation = args.is_empty() && current.is_empty();
        if !(empty_invocation && def.arity() == 0 && !def.is_variadic()) {
            args.push(current);
        }
        if def.is_variadic() && args.len() == def.arity() {
            args.push(Vec::new());
        }

        let expected = def.arity() + usize::from(def.is_variadic());
        if args.len() != expected {
            let word = if args.len() < expected { "few" } else { "many" };
            self.diagnostics.push(
                Diagnostic::error(
                    format!(
                        "too {word} arguments to macro `{}`, expected {}{}, got {}",
                        self.interner.resolve(def.name),
                        def.arity(),
                        if def.is_variadic() { " or more" } else { "" },
                        args.len()
                    ),
                    name.report_span(),
                )
                .with_code("E0312")
                .note("defined here", def.span),
            );
            return None;
        }
        Some((args, rparen))
    }

    /// Argument substitution over a replacement list.
    ///
    /// The order of the cases matters and each one of them is a known source of bugs, so
    /// they are written out separately rather than folded together.
    fn subst(&mut self, def: &MacroDef, args: &mut Args, hs: HideSet, invocation: Tok) -> Vec<Tok> {
        let body: Vec<Tok> = def.body.iter().map(|&t| Tok::new(t)).collect();
        let substituted = self.subst_list(def, args, &body, invocation);
        let mut os = drop_placemarkers(substituted);
        for tok in &mut os {
            tok.hides = self.hides.union(tok.hides, hs);
            // The outermost invocation wins, because substitution of the outer macro runs
            // after substitution of the inner ones, and the outer call is the line the user
            // wrote and the line a diagnostic should point at.
            tok.expansion = invocation.report_span();
        }
        if let Some(first) = os.first_mut() {
            first.flags = carried_spacing(invocation.flags);
        }
        os
    }

    /// The recursive half of substitution, which `__VA_OPT__` re-enters for its contents.
    fn subst_list(
        &mut self,
        def: &MacroDef,
        args: &mut Args,
        is: &[Tok],
        invocation: Tok,
    ) -> Vec<Tok> {
        let mut os: Vec<Tok> = Vec::with_capacity(is.len());
        let mut at = 0;
        // Whitespace owed to the output because the thing that carried it substituted to
        // nothing. `#define f(a, ...) [a __VA_ARGS__]` invoked as `f(1)` produces `[1 ]`, not
        // `[1]`, and matching that is part of what makes `-E` output diffable against GCC's,
        // per `spec/05-preprocessor.md` section 5.6.
        let mut owed = false;
        while at < is.len() {
            let tok = is[at];

            // `# parameter`, and the C23 `# __VA_OPT__(...)`.
            if def.function_like && tok.is(Punct::Hash) {
                if let Some(next) = is.get(at + 1) {
                    if let Some(idx) = next.ident().and_then(|s| def.param_index(s)) {
                        let text = self.stringize(args.raw(idx));
                        let string = self.string_token(&text, tok.span.to(next.span));
                        emit(&mut os, &[string], tok, &mut owed);
                        at += 2;
                        continue;
                    }
                    if next.ident() == Some(self.va_opt) {
                        if let Some(inner) = va_opt_group(is, at + 1) {
                            let close = inner.end;
                            let raw = if args.raw(def.arity()).is_empty() {
                                Vec::new()
                            } else {
                                self.subst_raw(def, args, &is[inner])
                            };
                            let text = self.stringize(&raw);
                            let string = self.string_token(&text, tok.span.to(is[close].span));
                            emit(&mut os, &[string], tok, &mut owed);
                            at = close + 1;
                            continue;
                        }
                    }
                }
            }

            // `## operand`. The definition check guarantees there is an operand. A paste
            // clears any owed whitespace, because the point of it is that the two operands
            // become one token with nothing between them.
            if tok.is(Punct::HashHash) {
                let next = is[at + 1];
                owed = false;
                let param = next.ident().and_then(|s| def.param_index(s).map(|idx| (s, idx)));
                if let Some((name, idx)) = param {
                    let raw = args.raw(idx).to_vec();
                    // The GNU extension: in `, ## __VA_ARGS__` the paste is not a paste at
                    // all. It drops the comma when there are no variable arguments and does
                    // nothing at all when there are. An enormous amount of existing code
                    // depends on it and will for another decade.
                    let comma_variadic = def.is_variadic_param(name)
                        && os.last().is_some_and(|l| l.is(Punct::Comma));
                    if comma_variadic {
                        if raw.is_empty() {
                            os.pop();
                        } else {
                            emit(&mut os, &raw, next, &mut owed);
                        }
                    } else {
                        self.glue(&mut os, &raw, next.span);
                    }
                    at += 2;
                    continue;
                }
                if next.ident() == Some(self.va_opt) {
                    if let Some(inner) = va_opt_group(is, at + 1) {
                        let close = inner.end;
                        let rhs = self.va_opt_value(def, args, &is[inner], invocation, next.span);
                        self.glue(&mut os, &rhs, next.span);
                        at = close + 1;
                        continue;
                    }
                }
                self.glue(&mut os, &[next], next.span);
                at += 2;
                continue;
            }

            // `__VA_OPT__(...)` in an ordinary position.
            if tok.ident() == Some(self.va_opt) {
                if let Some(inner) = va_opt_group(is, at) {
                    let close = inner.end;
                    let value = self.va_opt_value(def, args, &is[inner], invocation, tok.span);
                    emit(&mut os, &value, tok, &mut owed);
                    at = close + 1;
                    continue;
                }
            }

            // A parameter. Pasted with what follows it means the raw argument; otherwise the
            // fully expanded one.
            if let Some(idx) = tok.ident().and_then(|s| def.param_index(s)) {
                if is.get(at + 1).is_some_and(|n| n.is(Punct::HashHash)) {
                    let raw = args.raw(idx).to_vec();
                    let placemarker = [Tok::placemarker_at(tok.span)];
                    let value = if raw.is_empty() { &placemarker[..] } else { &raw[..] };
                    emit(&mut os, value, tok, &mut owed);
                } else {
                    let expanded = args.expanded(idx, self).to_vec();
                    emit(&mut os, &expanded, tok, &mut owed);
                }
                at += 1;
                continue;
            }

            emit_plain(&mut os, tok, &mut owed);
            at += 1;
        }
        os
    }

    /// What a `__VA_OPT__(...)` group stands for: its substituted contents when the variadic
    /// argument has tokens, and a placemarker when it does not.
    fn va_opt_value(
        &mut self,
        def: &MacroDef,
        args: &mut Args,
        inner: &[Tok],
        invocation: Tok,
        span: Span,
    ) -> Vec<Tok> {
        if args.raw(def.arity()).is_empty() {
            return vec![Tok::placemarker_at(span)];
        }
        let value = self.subst_list(def, args, inner, invocation);
        if value.is_empty() { vec![Tok::placemarker_at(span)] } else { value }
    }

    /// Substitution with parameters replaced by their unexpanded arguments, which is what
    /// stringizing a `__VA_OPT__` group needs.
    fn subst_raw(&mut self, def: &MacroDef, args: &mut Args, inner: &[Tok]) -> Vec<Tok> {
        let mut out = Vec::with_capacity(inner.len());
        for &tok in inner {
            match tok.ident().and_then(|s| def.param_index(s)) {
                Some(idx) => out.extend_from_slice(args.raw(idx)),
                None => out.push(tok),
            }
        }
        out
    }

    /// Pastes `rhs` onto the last token of `os`.
    ///
    /// An empty `rhs` is a placemarker, and pasting anything onto a placemarker or a
    /// placemarker onto anything leaves the anything, which is what makes `a ## b` with an
    /// empty `b` produce `a` instead of an error.
    fn glue(&mut self, os: &mut Vec<Tok>, rhs: &[Tok], span: Span) {
        let placemarker = [Tok::placemarker_at(span)];
        let rhs = if rhs.is_empty() { &placemarker[..] } else { rhs };
        let Some(lhs) = os.pop() else {
            os.extend_from_slice(rhs);
            return;
        };
        let first = rhs[0];
        if lhs.is_placemarker() {
            os.extend_from_slice(rhs);
            return;
        }
        if first.is_placemarker() {
            os.push(lhs);
            os.extend_from_slice(&rhs[1..]);
            return;
        }
        match self.paste(lhs, first) {
            Some(joined) => os.push(joined),
            None => {
                // The two were meant to be one token, so they are printed with nothing
                // between them even though the paste failed. GCC and Clang both do this.
                let mut first = first;
                first.flags = TokenFlags::EMPTY;
                os.push(lhs);
                os.push(first);
            }
        }
        os.extend_from_slice(&rhs[1..]);
    }

    /// Concatenates two spellings and re-lexes the result.
    ///
    /// A result that is not exactly one preprocessing token is a constraint violation. GCC
    /// diagnoses it and keeps both tokens, and we do the same, because rejecting the
    /// translation unit here would stop a build over something that in practice never
    /// reaches the parser.
    fn paste(&mut self, lhs: Tok, rhs: Tok) -> Option<Tok> {
        let mut text = String::new();
        self.spell(lhs, &mut text);
        let split = text.len();
        self.spell(rhs, &mut text);

        let (tokens, _) = tokenize(text.as_bytes(), 0, Options::new(), self.interner);
        let single = tokens.len() == 2
            && tokens[0].kind != PpTokenKind::Eof
            && tokens[1].kind == PpTokenKind::Eof
            && tokens[0].span.lo == 0
            && tokens[0].span.hi as usize == text.len();
        if !single {
            self.diagnostics.push(
                Diagnostic::error(
                    format!(
                        "pasting `{}` and `{}` does not give a valid preprocessing token",
                        &text[..split],
                        &text[split..]
                    ),
                    lhs.report_span().to(rhs.report_span()),
                )
                .with_code("E0313")
                .note("the left operand is here", lhs.span)
                .note("the right operand is here", rhs.span),
            );
            return None;
        }
        Some(Tok {
            kind: tokens[0].kind,
            flags: lhs.flags,
            value: tokens[0].value,
            span: lhs.span.to(rhs.span),
            expansion: lhs.expansion,
            hides: self.hides.union(lhs.hides, rhs.hides),
            placemarker: false,
        })
    }

    /// Builds the string literal `#` produces.
    ///
    /// Internal whitespace runs collapse to one space, leading and trailing space is
    /// dropped, and a backslash or double quote inside a string or character literal is
    /// escaped, per `spec/05-preprocessor.md` section 5.3.
    fn stringize(&self, toks: &[Tok]) -> String {
        let mut out = String::from("\"");
        let mut first = true;
        for &tok in toks.iter().filter(|t| !t.is_placemarker()) {
            if !first && tok.flags.has(TokenFlags::LEADING_SPACE) {
                out.push(' ');
            }
            first = false;
            let mut spelled = String::new();
            self.spell(tok, &mut spelled);
            if matches!(tok.kind, PpTokenKind::StringLit | PpTokenKind::CharConst) {
                for ch in spelled.chars() {
                    if ch == '\\' || ch == '"' {
                        out.push('\\');
                    }
                    out.push(ch);
                }
            } else {
                out.push_str(&spelled);
            }
        }
        out.push('"');
        out
    }

    /// Wraps stringized text as a token.
    fn string_token(&mut self, text: &str, span: Span) -> Tok {
        Tok {
            kind: PpTokenKind::StringLit,
            flags: TokenFlags::EMPTY,
            value: Some(self.interner.intern(text)),
            span,
            expansion: Span::DUMMY,
            hides: HideSet::EMPTY,
            placemarker: false,
        }
    }

    /// Appends a token's spelling.
    fn spell(&self, tok: Tok, out: &mut String) {
        if tok.is_placemarker() {
            return;
        }
        match (tok.kind, tok.value) {
            (PpTokenKind::Punct(p), _) => out.push_str(p.as_str()),
            (_, Some(sym)) => out.push_str(self.interner.resolve(sym)),
            (_, None) => {}
        }
    }
}

/// Appends what a body token substituted to.
///
/// The first token of the result takes the spacing of the token it replaced, so that
/// `#define f(x) (x + x)` prints as `(1 + 1)` rather than `(1 +1)`. A group that substituted
/// to nothing leaves its spacing owed to whatever comes next.
fn emit(os: &mut Vec<Tok>, value: &[Tok], source: Tok, owed: &mut bool) {
    let Some((&first, rest)) = value.split_first() else {
        *owed = *owed || source.flags.has(TokenFlags::LEADING_SPACE);
        return;
    };
    let mut first = first;
    first.flags = carried_spacing(source.flags);
    if *owed {
        first.flags = first.flags.with(TokenFlags::LEADING_SPACE);
        *owed = false;
    }
    os.push(first);
    os.extend_from_slice(rest);
}

/// Appends a token that stands for itself, which is every token of a replacement list that
/// is not a parameter or an operator.
fn emit_plain(os: &mut Vec<Tok>, tok: Tok, owed: &mut bool) {
    let mut tok = tok;
    if *owed {
        tok.flags = tok.flags.with(TokenFlags::LEADING_SPACE);
        *owed = false;
    }
    os.push(tok);
}

/// Removes placemarkers, handing any whitespace they carried to the next real token.
fn drop_placemarkers(toks: Vec<Tok>) -> Vec<Tok> {
    let mut out = Vec::with_capacity(toks.len());
    let mut owed = false;
    for tok in toks {
        if tok.is_placemarker() {
            owed = owed || tok.flags.has(TokenFlags::LEADING_SPACE);
            continue;
        }
        emit_plain(&mut out, tok, &mut owed);
    }
    out
}

/// Finds the parenthesised group belonging to a `__VA_OPT__` at `at`.
///
/// Returns the range of the contents. The closing parenthesis is at `range.end`, so the
/// group ends at `range.end + 1`, which is what every caller wants next.
fn va_opt_group(is: &[Tok], at: usize) -> Option<std::ops::Range<usize>> {
    if !is.get(at + 1).is_some_and(|t| t.is(Punct::LParen)) {
        return None;
    }
    let start = at + 2;
    let mut depth = 1usize;
    let mut end = start;
    while end < is.len() {
        match is[end].punct() {
            Some(Punct::LParen) => depth += 1,
            Some(Punct::RParen) => {
                depth -= 1;
                if depth == 0 {
                    return Some(start..end);
                }
            }
            _ => {}
        }
        end += 1;
    }
    None
}

/// Pushes a replacement onto the front of the pushback stack, preserving its order.
fn push_front(pending: &mut Vec<Tok>, mut replacement: Vec<Tok>) {
    replacement.reverse();
    pending.append(&mut replacement);
}

/// The flags a replacement's first token inherits from the invocation.
///
/// Only spacing carries over. A macro that expanded from a spliced or digraph token did not
/// itself come from one, and saying it did would put the wrong thing in `-E` output.
fn carried_spacing(flags: TokenFlags) -> TokenFlags {
    let mut carried = TokenFlags::EMPTY;
    if flags.has(TokenFlags::START_OF_LINE) {
        carried = carried.with(TokenFlags::START_OF_LINE);
    }
    if flags.has(TokenFlags::LEADING_SPACE) {
        carried = carried.with(TokenFlags::LEADING_SPACE);
    }
    carried
}

/// The arguments of one invocation, raw and expanded.
///
/// An argument used twice in a replacement list is expanded once. That is not just a saving:
/// `spec/02-the-goal.md` wants the same input to produce the same diagnostics, and expanding
/// an argument twice would report anything wrong inside it twice.
struct Args {
    raw: Vec<Vec<Tok>>,
    expanded: Vec<Option<Vec<Tok>>>,
}

impl Args {
    fn new(raw: Vec<Vec<Tok>>) -> Args {
        let count = raw.len();
        Args { raw, expanded: vec![None; count] }
    }

    /// The argument list of an object-like macro, which has none.
    fn none() -> Args {
        Args { raw: Vec::new(), expanded: Vec::new() }
    }

    fn raw(&self, idx: usize) -> &[Tok] {
        self.raw.get(idx).map_or(&[][..], |a| a.as_slice())
    }

    fn expanded(&mut self, idx: usize, run: &mut Run<'_>) -> &[Tok] {
        let Some(slot) = self.expanded.get(idx) else {
            return &[];
        };
        if slot.is_none() {
            let expanded = run.expand(self.raw[idx].clone());
            self.expanded[idx] = Some(expanded);
        }
        self.expanded[idx].as_deref().expect("just filled in")
    }
}
