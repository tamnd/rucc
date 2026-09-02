//! Macro definitions and the table they live in.
//!
//! Design: `spec/05-preprocessor.md` sections 5.3 and 5.4.
//!
//! Parsing a definition and expanding one are separate concerns and the constraint checks
//! belong here, at definition time, because that is where the user's `#define` line is and
//! where the error is worth reading. By the time the expander runs, a definition is known
//! good and it can concentrate on the substitution rules.

use std::collections::HashMap;

use rucc_base::{Interner, Symbol};
use rucc_diag::{Diagnostic, Span};
use rucc_lex::{PpToken, PpTokenKind, Punct, TokenFlags};

/// A predefined macro whose value is a question rather than a replacement list.
///
/// `__FILE__` and its relatives cannot be written as a body, because what they stand for
/// depends on where they are used rather than on what the target is. GCC calls these builtin
/// macros and answers them while expanding, and this is the same arrangement: the table holds
/// the name and which question it is, and `crate::expand` asks the source map when it meets
/// one. Everything else about them is ordinary, so `#ifdef __FILE__` is true, `#undef
/// __FILE__` works, and redefining one warns the way redefining anything else does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Builtin {
    /// `__FILE__`, the file the use is in, spelled as a string literal.
    File,
    /// `__FILE_NAME__`, the same file with the directories taken off.
    FileName,
    /// `__BASE_FILE__`, the file at the bottom of the include stack.
    BaseFile,
    /// `__LINE__`, the line the use is on.
    Line,
    /// `__INCLUDE_LEVEL__`, how many `#include` directives deep the use is.
    IncludeLevel,
    /// `__COUNTER__`, a number that is different every time it is expanded.
    Counter,
}

impl Builtin {
    /// Every builtin macro and its spelling.
    ///
    /// One list, so that the set the preprocessor defines and the set `-dM` prints cannot
    /// drift apart.
    pub const ALL: [(&'static str, Builtin); 6] = [
        ("__FILE__", Builtin::File),
        ("__FILE_NAME__", Builtin::FileName),
        ("__BASE_FILE__", Builtin::BaseFile),
        ("__LINE__", Builtin::Line),
        ("__INCLUDE_LEVEL__", Builtin::IncludeLevel),
        ("__COUNTER__", Builtin::Counter),
    ];
}

/// A `#define`.
#[derive(Debug, Clone)]
pub struct MacroDef {
    /// The macro's name.
    pub name: Symbol,
    /// Whether the macro takes arguments. A function-like macro with no parameters is not
    /// the same thing as an object-like macro, so this cannot be inferred from `params`.
    pub function_like: bool,
    /// The named parameters, in order, not including the variadic one.
    pub params: Vec<Symbol>,
    /// The variadic parameter: `__VA_ARGS__` for the standard `...` spelling, or the given
    /// name for the GNU `args...` form. `None` for a macro that is not variadic.
    pub variadic: Option<Symbol>,
    /// The replacement list.
    pub body: Vec<PpToken>,
    /// The `#define` line, for the note attached to a redefinition or an arity error.
    pub span: Span,
    /// Which question this macro asks, for the handful that ask one instead of having a body.
    pub builtin: Option<Builtin>,
}

impl MacroDef {
    /// Whether the macro takes a variable number of arguments.
    #[inline]
    pub fn is_variadic(&self) -> bool {
        self.variadic.is_some()
    }

    /// How many arguments an invocation must supply at a minimum.
    #[inline]
    pub fn arity(&self) -> usize {
        self.params.len()
    }

    /// The parameter position `name` refers to, with the variadic parameter counting as one
    /// past the named ones.
    pub fn param_index(&self, name: Symbol) -> Option<usize> {
        if let Some(at) = self.params.iter().position(|&p| p == name) {
            return Some(at);
        }
        if self.variadic == Some(name) { Some(self.params.len()) } else { None }
    }

    /// Whether `name` is this macro's variadic parameter.
    #[inline]
    pub fn is_variadic_param(&self, name: Symbol) -> bool {
        self.variadic == Some(name)
    }

    /// Whether two definitions are the same one, which is what decides whether a
    /// redefinition is silently allowed.
    ///
    /// The standard's rule is spelling equivalence including whitespace separation, not just
    /// the same tokens, which is why the leading space flag is part of the comparison.
    pub fn same_definition_as(&self, other: &MacroDef) -> bool {
        if self.function_like != other.function_like
            || self.params != other.params
            || self.variadic != other.variadic
            || self.builtin != other.builtin
            || self.body.len() != other.body.len()
        {
            return false;
        }
        self.body.iter().zip(&other.body).enumerate().all(|(at, (a, b))| {
            a.kind == b.kind
                && a.value == b.value
                // The first token of a replacement list has whitespace before it whether or
                // not the user typed any, so only interior separation is compared.
                && (at == 0
                    || a.flags.has(TokenFlags::LEADING_SPACE)
                        == b.flags.has(TokenFlags::LEADING_SPACE))
        })
    }
}

/// Every macro currently defined.
#[derive(Debug, Default)]
pub struct MacroTable {
    by_name: HashMap<Symbol, MacroDef>,
    /// What `#pragma push_macro` put aside, innermost last, one stack per name.
    ///
    /// A name with nothing saved has no entry, so the common case of a file that never uses the
    /// pragma pays for one empty map. `None` in a stack is a real value and not an absence: it
    /// records that the name had no definition when it was pushed, which is what a matching pop
    /// has to restore.
    saved: HashMap<Symbol, Vec<Option<MacroDef>>>,
}

impl MacroTable {
    /// An empty table.
    pub fn new() -> MacroTable {
        MacroTable::default()
    }

    /// The definition of `name`, if it has one.
    #[inline]
    pub fn lookup(&self, name: Symbol) -> Option<&MacroDef> {
        self.by_name.get(&name)
    }

    /// Whether `name` is defined, which is what `#ifdef` and `defined` ask.
    #[inline]
    pub fn is_defined(&self, name: Symbol) -> bool {
        self.by_name.contains_key(&name)
    }

    /// How many macros are defined.
    pub fn len(&self) -> usize {
        self.by_name.len()
    }

    /// Whether no macros are defined.
    pub fn is_empty(&self) -> bool {
        self.by_name.is_empty()
    }

    /// Adds a definition, returning a warning if it replaces a different one.
    ///
    /// Redefining a macro to the same thing is legal and extremely common, because a header
    /// included twice through two paths does it. Redefining it to something else is a
    /// constraint violation, which GCC reports as a warning and accepts, and we match that
    /// because rejecting it would break real builds.
    pub fn define(&mut self, def: MacroDef, interner: &Interner) -> Option<Diagnostic> {
        let complaint =
            self.by_name.get(&def.name).filter(|old| !old.same_definition_as(&def)).map(|old| {
                Diagnostic::warning(format!("`{}` redefined", interner.resolve(def.name)), def.span)
                    .with_code("W0301")
                    .note("previous definition was here", old.span)
            });
        self.by_name.insert(def.name, def);
        complaint
    }

    /// Defines one of the macros whose value is a question.
    ///
    /// `span` is where to say the macro came from, which is the start of `<built-in>`, so that
    /// a warning about redefining `__FILE__` has somewhere to point.
    pub fn define_builtin(&mut self, name: Symbol, builtin: Builtin, span: Span) {
        let def = MacroDef {
            name,
            function_like: false,
            params: Vec::new(),
            variadic: None,
            body: Vec::new(),
            span,
            builtin: Some(builtin),
        };
        self.by_name.insert(name, def);
    }

    /// Removes a definition. Undefining a macro that is not defined is legal and silent.
    pub fn undef(&mut self, name: Symbol) -> Option<MacroDef> {
        self.by_name.remove(&name)
    }

    /// Puts the current definition of `name` aside, per `#pragma push_macro`.
    ///
    /// A name with no definition pushes the absence, because the pragma is about restoring the
    /// state and "not defined" is a state. This is what lets the idiom work at all: a header
    /// pushes a name, defines it for its own use, and pops it, and the caller gets back exactly
    /// what it had whether that was a definition or nothing.
    pub fn push_macro(&mut self, name: Symbol) {
        let current = self.by_name.get(&name).cloned();
        self.saved.entry(name).or_default().push(current);
    }

    /// Restores what the last `#pragma push_macro` on `name` put aside.
    ///
    /// A pop with nothing pushed does nothing and says nothing, which is what gcc does. The
    /// pragma is written in pairs across headers that do not know about each other, so a
    /// diagnostic here would fire on code that is not wrong.
    pub fn pop_macro(&mut self, name: Symbol) {
        let Some(stack) = self.saved.get_mut(&name) else {
            return;
        };
        let Some(was) = stack.pop() else {
            return;
        };
        if stack.is_empty() {
            self.saved.remove(&name);
        }
        match was {
            Some(def) => {
                self.by_name.insert(name, def);
            }
            None => {
                self.by_name.remove(&name);
            }
        }
    }

    /// Every defined macro, sorted by symbol.
    ///
    /// Sorted because `-dM` output has to be byte identical across runs and hash order is
    /// not, per `spec/02-the-goal.md`.
    pub fn sorted(&self) -> Vec<&MacroDef> {
        let mut all: Vec<&MacroDef> = self.by_name.values().collect();
        all.sort_by_key(|m| m.name);
        all
    }
}

/// Parses the tokens after `#define` into a definition.
///
/// `tokens` is the rest of the directive line with no end marker, exactly as the lexer
/// produced it. Diagnostics are returned alongside the definition where the definition is
/// still usable, and alone where it is not.
///
/// # Panics
///
/// Panics if `tokens` did not come from `rucc_lex`, which interns the spelling of every
/// identifier it produces. There is no other source of preprocessing tokens.
pub fn parse_define(
    tokens: &[PpToken],
    interner: &mut Interner,
) -> (Option<MacroDef>, Vec<Diagnostic>) {
    let mut diagnostics = Vec::new();
    let Some(&first) = tokens.first() else {
        return (None, vec![Diagnostic::error("no macro name given in `#define`", Span::DUMMY)]);
    };
    if first.kind != PpTokenKind::Ident {
        diagnostics.push(
            Diagnostic::error("macro name must be an identifier", first.span).with_code("E0300"),
        );
        return (None, diagnostics);
    }
    let name = first.value.expect("the lexer interns every identifier");
    let span = first.span;
    let rest = &tokens[1..];

    // A parenthesis touching the name introduces parameters. The same parenthesis with a
    // space before it is the first token of the replacement list, which is the difference
    // between `#define A (x)` and `#define A(x)` and the reason the flag exists.
    let opens_params = rest.first().is_some_and(|t| {
        t.punct() == Some(Punct::LParen) && !t.flags.has(TokenFlags::LEADING_SPACE)
    });

    let (function_like, params, variadic, body) = if opens_params {
        match parse_params(&rest[1..], interner, &mut diagnostics) {
            Some((params, variadic, consumed)) => (true, params, variadic, &rest[1 + consumed..]),
            None => return (None, diagnostics),
        }
    } else {
        (false, Vec::new(), None, rest)
    };

    let def = MacroDef {
        name,
        function_like,
        params,
        variadic,
        body: body.to_vec(),
        span,
        builtin: None,
    };
    check_body(&def, interner, &mut diagnostics);
    (Some(def), diagnostics)
}

/// Parses a parameter list, `tokens` starting just after the opening parenthesis.
///
/// Returns the parameters, the variadic parameter if there is one, and how many tokens were
/// consumed including the closing parenthesis.
fn parse_params(
    tokens: &[PpToken],
    interner: &mut Interner,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<(Vec<Symbol>, Option<Symbol>, usize)> {
    let va_args = interner.intern("__VA_ARGS__");
    let mut params: Vec<Symbol> = Vec::new();
    let mut variadic = None;
    let mut at = 0;

    if tokens.first().is_some_and(|t| t.punct() == Some(Punct::RParen)) {
        return Some((params, None, 1));
    }

    loop {
        let Some(&tok) = tokens.get(at) else {
            diagnostics.push(
                Diagnostic::error("missing `)` in macro parameter list", last_span(tokens))
                    .with_code("E0301"),
            );
            return None;
        };
        at += 1;

        if tok.punct() == Some(Punct::Ellipsis) {
            variadic = Some(va_args);
        } else if tok.kind == PpTokenKind::Ident {
            let sym = tok.value.expect("the lexer interns every identifier");
            // The GNU named variadic form, `args...`, which the kernel uses everywhere.
            if tokens.get(at).is_some_and(|t| t.punct() == Some(Punct::Ellipsis)) {
                at += 1;
                variadic = Some(sym);
            } else if sym == va_args {
                diagnostics.push(
                    Diagnostic::error("`__VA_ARGS__` cannot be used as a parameter name", tok.span)
                        .with_code("E0302"),
                );
                return None;
            } else if params.contains(&sym) {
                diagnostics.push(
                    Diagnostic::error(
                        format!("duplicate macro parameter `{}`", interner.resolve(sym)),
                        tok.span,
                    )
                    .with_code("E0303"),
                );
                return None;
            } else {
                params.push(sym);
            }
        } else {
            diagnostics.push(
                Diagnostic::error("macro parameter must be an identifier", tok.span)
                    .with_code("E0301"),
            );
            return None;
        }

        match tokens.get(at).and_then(|t| t.punct()) {
            Some(Punct::RParen) => return Some((params, variadic, at + 1)),
            Some(Punct::Comma) if variadic.is_none() => at += 1,
            Some(Punct::Comma) => {
                diagnostics.push(
                    Diagnostic::error("`...` must be the last macro parameter", tokens[at].span)
                        .with_code("E0301"),
                );
                return None;
            }
            _ => {
                diagnostics.push(
                    Diagnostic::error("missing `)` in macro parameter list", last_span(tokens))
                        .with_code("E0301"),
                );
                return None;
            }
        }
    }
}

/// The constraint checks on a replacement list that do not need the expander to run.
fn check_body(def: &MacroDef, interner: &mut Interner, diagnostics: &mut Vec<Diagnostic>) {
    let va_opt = interner.intern("__VA_OPT__");
    let va_args = interner.intern("__VA_ARGS__");

    if let Some(first) = def.body.first().filter(|t| t.punct() == Some(Punct::HashHash)) {
        diagnostics.push(
            Diagnostic::error("`##` cannot appear at the start of a replacement list", first.span)
                .with_code("E0304"),
        );
    }
    // Guarded on length so that a body of exactly `##` is reported once rather than twice.
    let trailing =
        def.body.last().filter(|t| def.body.len() > 1 && t.punct() == Some(Punct::HashHash));
    if let Some(last) = trailing {
        diagnostics.push(
            Diagnostic::error("`##` cannot appear at the end of a replacement list", last.span)
                .with_code("E0304"),
        );
    }

    for (at, tok) in def.body.iter().enumerate() {
        // `#` in a function-like macro must stringify a parameter. In an object-like macro
        // it is just a token, which is how `#define HASH #` works.
        if def.function_like && tok.punct() == Some(Punct::Hash) {
            let operand = def.body.get(at + 1);
            let names_param = operand.is_some_and(|t| {
                t.value.is_some_and(|v| def.param_index(v).is_some())
                    || (def.is_variadic() && t.value == Some(va_opt))
            });
            if !names_param {
                diagnostics.push(
                    Diagnostic::error("`#` must be followed by a macro parameter", tok.span)
                        .with_code("E0305"),
                );
            }
        }

        if tok.kind != PpTokenKind::Ident {
            continue;
        }
        if tok.value == Some(va_args) && !def.is_variadic() {
            diagnostics.push(
                Diagnostic::error("`__VA_ARGS__` can only appear in a variadic macro", tok.span)
                    .with_code("E0306"),
            );
        }
        if tok.value == Some(va_opt) {
            if !def.is_variadic() {
                diagnostics.push(
                    Diagnostic::error("`__VA_OPT__` can only appear in a variadic macro", tok.span)
                        .with_code("E0306"),
                );
            } else if !def.body.get(at + 1).is_some_and(|t| t.punct() == Some(Punct::LParen)) {
                diagnostics.push(
                    Diagnostic::error("`__VA_OPT__` must be followed by `(`", tok.span)
                        .with_code("E0307"),
                );
            }
        }
    }
}

/// A span to hang an unterminated-construct error on when there is no token left to point at.
fn last_span(tokens: &[PpToken]) -> Span {
    tokens.last().map_or(Span::DUMMY, |t| t.span)
}

#[cfg(test)]
mod tests {
    use rucc_diag::Severity;
    use rucc_lex::{Options, tokenize};

    use super::*;

    fn define(src: &str, interner: &mut Interner) -> (Option<MacroDef>, Vec<Diagnostic>) {
        let (tokens, lex_errors) = tokenize(src.as_bytes(), 0, Options::new(), interner);
        assert!(lex_errors.is_empty(), "the test input should lex cleanly");
        let body: Vec<PpToken> =
            tokens.into_iter().filter(|t| t.kind != PpTokenKind::Eof).collect();
        parse_define(&body, interner)
    }

    #[test]
    fn an_object_like_macro_has_no_parameter_list() {
        let mut i = Interner::new();
        let (def, errors) = define("PI 3.14", &mut i);
        let def = def.expect("should parse");
        assert!(errors.is_empty());
        assert!(!def.function_like);
        assert_eq!(def.body.len(), 1);
    }

    #[test]
    fn a_space_before_the_parenthesis_makes_it_object_like() {
        let mut i = Interner::new();
        let (def, _) = define("A (x)", &mut i);
        let def = def.expect("should parse");
        assert!(!def.function_like, "`#define A (x)` defines A as the token sequence `(x)`");
        assert_eq!(def.body.len(), 3);
    }

    #[test]
    fn a_function_like_macro_with_no_parameters_is_not_object_like() {
        let mut i = Interner::new();
        let (def, _) = define("A() 1", &mut i);
        let def = def.expect("should parse");
        assert!(def.function_like);
        assert_eq!(def.arity(), 0);
    }

    #[test]
    fn the_standard_ellipsis_names_the_variadic_va_args() {
        let mut i = Interner::new();
        let (def, errors) = define("F(a, ...) a", &mut i);
        let def = def.expect("should parse");
        assert!(errors.is_empty());
        assert_eq!(def.arity(), 1);
        assert_eq!(def.variadic, Some(i.intern("__VA_ARGS__")));
    }

    #[test]
    fn the_gnu_form_names_the_variadic_itself() {
        let mut i = Interner::new();
        let (def, errors) = define("F(a, rest...) a", &mut i);
        let def = def.expect("should parse");
        assert!(errors.is_empty());
        assert_eq!(def.variadic, Some(i.intern("rest")));
        assert_eq!(def.param_index(i.intern("rest")), Some(1));
    }

    #[test]
    fn a_duplicate_parameter_is_rejected() {
        let mut i = Interner::new();
        let (def, errors) = define("F(a, a) a", &mut i);
        assert!(def.is_none());
        assert_eq!(errors[0].code, Some("E0303"));
    }

    #[test]
    fn paste_cannot_start_or_end_a_replacement_list() {
        let mut i = Interner::new();
        let (_, start) = define("A ## b", &mut i);
        assert_eq!(start[0].code, Some("E0304"));
        let (_, end) = define("A b ##", &mut i);
        assert_eq!(end[0].code, Some("E0304"));
    }

    #[test]
    fn stringify_must_name_a_parameter_but_only_in_a_function_like_macro() {
        let mut i = Interner::new();
        let (_, bad) = define("F(a) # b", &mut i);
        assert_eq!(bad[0].code, Some("E0305"));
        let (_, fine) = define("HASH #", &mut i);
        assert!(fine.is_empty(), "a bare `#` in an object-like macro is just a token");
    }

    #[test]
    fn va_args_outside_a_variadic_macro_is_rejected() {
        let mut i = Interner::new();
        let (_, errors) = define("F(a) __VA_ARGS__", &mut i);
        assert_eq!(errors[0].code, Some("E0306"));
    }

    #[test]
    fn va_opt_must_be_called() {
        let mut i = Interner::new();
        let (_, errors) = define("F(...) __VA_OPT__", &mut i);
        assert_eq!(errors[0].code, Some("E0307"));
    }

    #[test]
    fn redefining_a_macro_to_the_same_thing_is_silent() {
        let mut i = Interner::new();
        let mut table = MacroTable::new();
        let (first, _) = define("A 1 + 2", &mut i);
        let (again, _) = define("A 1 + 2", &mut i);
        assert!(table.define(first.expect("should parse"), &i).is_none());
        assert!(table.define(again.expect("should parse"), &i).is_none());
        assert_eq!(table.len(), 1);
    }

    #[test]
    fn redefining_a_macro_differently_warns_and_takes_the_new_one() {
        let mut i = Interner::new();
        let mut table = MacroTable::new();
        let (first, _) = define("A 1", &mut i);
        let (again, _) = define("A 2", &mut i);
        table.define(first.expect("should parse"), &i);
        let warning = table.define(again.expect("should parse"), &i).expect("should warn");
        assert_eq!(warning.severity, Severity::Warning);
        assert_eq!(warning.code, Some("W0301"));
        assert_eq!(table.lookup(i.intern("A")).expect("still defined").body.len(), 1);
    }

    #[test]
    fn whitespace_inside_the_replacement_list_is_part_of_the_definition() {
        let mut i = Interner::new();
        let (a, _) = define("A x+y", &mut i);
        let (b, _) = define("A x + y", &mut i);
        assert!(
            !a.expect("should parse").same_definition_as(&b.expect("should parse")),
            "the standard compares spelling including whitespace separation"
        );
    }

    #[test]
    fn a_builtin_macro_is_defined_like_any_other() {
        let mut i = Interner::new();
        let mut table = MacroTable::new();
        let name = i.intern("__LINE__");
        table.define_builtin(name, Builtin::Line, Span::new(0, 0));
        assert!(table.is_defined(name), "`#ifdef __LINE__` is true");
        assert_eq!(table.lookup(name).and_then(|d| d.builtin), Some(Builtin::Line));
        assert!(table.undef(name).is_some(), "`#undef __LINE__` is allowed, as it is in GCC");
    }

    #[test]
    fn redefining_a_builtin_is_a_redefinition() {
        let mut i = Interner::new();
        let mut table = MacroTable::new();
        let name = i.intern("__FILE__");
        table.define_builtin(name, Builtin::File, Span::new(0, 0));
        // An empty body is not the same definition as a question, which is the whole point of
        // the warning: somebody has just taken `__FILE__` away from every header below them.
        let (def, _) = define("__FILE__", &mut i);
        let warning = table.define(def.expect("should parse"), &i).expect("should warn");
        assert_eq!(warning.code, Some("W0301"));
        assert!(table.lookup(name).expect("still defined").builtin.is_none());
    }

    #[test]
    fn undefining_something_that_was_never_defined_is_fine() {
        let mut i = Interner::new();
        let mut table = MacroTable::new();
        assert!(table.undef(i.intern("nothing")).is_none());
    }

    #[test]
    fn a_push_and_a_pop_leave_the_table_where_they_found_it() {
        let mut i = Interner::new();
        let mut table = MacroTable::new();
        let name = i.intern("X");
        let (first, _) = define("X 1", &mut i);
        table.define(first.expect("should parse"), &i);
        table.push_macro(name);
        table.undef(name);
        let (second, _) = define("X 2", &mut i);
        table.define(second.expect("should parse"), &i);
        assert_eq!(table.lookup(name).expect("defined").body.len(), 1);
        table.pop_macro(name);
        assert!(table.is_defined(name), "the pop brought the first definition back");
        assert_eq!(table.len(), 1, "and did not leave the second one behind as well");
    }

    #[test]
    fn pushing_a_name_that_is_not_defined_pushes_the_absence() {
        let mut i = Interner::new();
        let mut table = MacroTable::new();
        let name = i.intern("X");
        table.push_macro(name);
        let (def, _) = define("X 1", &mut i);
        table.define(def.expect("should parse"), &i);
        table.pop_macro(name);
        assert!(!table.is_defined(name), "the state restored is the one that was saved");
    }

    #[test]
    fn a_pop_with_nothing_pushed_changes_nothing() {
        let mut i = Interner::new();
        let mut table = MacroTable::new();
        let name = i.intern("X");
        let (def, _) = define("X 1", &mut i);
        table.define(def.expect("should parse"), &i);
        table.pop_macro(name);
        table.pop_macro(i.intern("never_seen"));
        assert!(table.is_defined(name));
    }
}
