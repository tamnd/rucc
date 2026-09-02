//! Tokens to rules, and the checks that belong in the reading rather than after it.

use std::collections::HashSet;

use crate::ast::{Rule, Term, TermKind};
use crate::error::Error;
use crate::lex::{Spanned, Token, tokens};

/// What a specification calls the value the replacement computes.
const RESULT: &str = "result";

/// The names that mean something at the top of a rule and nowhere else. Refusing them as heads
/// inside a term is what turns a missing parenthesis into a message about the missing
/// parenthesis rather than a rule that parses and means something nobody wrote.
const RESERVED: [&str; 5] = ["rule", "lower", "if", "spec", "bounded"];

/// Read every rule in one file.
///
/// # Errors
///
/// Returns every error found rather than the first. After a malformed rule the reader skips to
/// the next `(rule`, so one missing parenthesis does not turn the rest of the file into noise.
pub fn parse(path: &str, text: &str) -> Result<Vec<Rule>, Vec<Error>> {
    let tokens = match tokens(path, text) {
        Ok(tokens) => tokens,
        Err(error) => return Err(vec![error]),
    };
    let mut reader = Reader { path, tokens: &tokens, at: 0, end: end_of(text) };
    let mut rules = Vec::new();
    let mut errors = Vec::new();

    while reader.at < reader.tokens.len() {
        match reader.rule() {
            Ok(rule) => {
                check(path, &rule, &mut errors);
                rules.push(rule);
            }
            Err(error) => {
                errors.push(error);
                reader.resync();
            }
        }
    }

    if errors.is_empty() { Ok(rules) } else { Err(errors) }
}

/// Read a file of bare terms rather than of rules.
///
/// The machine model is written in the same language as the rules and is not a rule, so this is
/// how it is read. Keeping one reader for both is the point: a model written in a second syntax
/// would be a second thing to get wrong.
///
/// # Errors
///
/// The first malformed term, since a model file has no rule boundaries to resynchronise on.
pub fn parse_terms(path: &str, text: &str) -> Result<Vec<Term>, Vec<Error>> {
    let tokens = match tokens(path, text) {
        Ok(tokens) => tokens,
        Err(error) => return Err(vec![error]),
    };
    let mut reader = Reader { path, tokens: &tokens, at: 0, end: end_of(text) };
    let mut out = Vec::new();
    while reader.at < reader.tokens.len() {
        match reader.term() {
            Ok(term) => out.push(term),
            Err(error) => return Err(vec![error]),
        }
    }
    Ok(out)
}

/// Where the end of the file is, so that running out of tokens can be reported somewhere real.
fn end_of(text: &str) -> (u32, u32) {
    let line = 1 + u32::try_from(text.matches('\n').count()).unwrap_or(u32::MAX);
    let column = 1 + u32::try_from(text.rsplit('\n').next().unwrap_or_default().chars().count())
        .unwrap_or(u32::MAX);
    (line, column)
}

/// One pass over the tokens of one file.
#[derive(Debug)]
struct Reader<'a> {
    path: &'a str,
    tokens: &'a [Spanned<'a>],
    at: usize,
    end: (u32, u32),
}

impl<'a> Reader<'a> {
    fn error(&self, message: String) -> Error {
        let (line, column) = match self.tokens.get(self.at) {
            Some(token) => (token.line, token.column),
            None => self.end,
        };
        Error { path: self.path.to_owned(), line, column, message }
    }

    fn peek(&self) -> Option<&'a Token<'a>> {
        self.tokens.get(self.at).map(|t| &t.token)
    }

    /// Whether a clause of the given name starts here. A guard is optional and this is how its
    /// absence is told from a replacement that happens to be an application.
    fn at_clause(&self, name: &str) -> bool {
        matches!(self.peek(), Some(Token::Open))
            && matches!(self.tokens.get(self.at + 1).map(|t| &t.token), Some(Token::Atom(a)) if *a == name)
    }

    fn open(&mut self) -> Result<(), Error> {
        match self.peek() {
            Some(Token::Open) => {
                self.at += 1;
                Ok(())
            }
            _ => Err(self.error("expected a `(`".to_owned())),
        }
    }

    /// A closing parenthesis, named after what it closes. When the file simply ran out, saying
    /// which thing is still open is the difference between a message that locates the missing
    /// parenthesis and one that only reports where the reader gave up.
    fn close(&mut self, what: &str) -> Result<(), Error> {
        match self.peek() {
            Some(Token::Close) => {
                self.at += 1;
                Ok(())
            }
            None => Err(self.error(format!("`({what}` was never closed"))),
            _ => Err(self.error("expected a `)`".to_owned())),
        }
    }

    fn keyword(&mut self, name: &str) -> Result<(), Error> {
        match self.peek() {
            Some(Token::Atom(a)) if *a == name => {
                self.at += 1;
                Ok(())
            }
            _ => Err(self.error(format!("expected `{name}`"))),
        }
    }

    /// A whole rule, from its `(` to its `)`.
    fn rule(&mut self) -> Result<Rule, Error> {
        let (line, column) = match self.tokens.get(self.at) {
            Some(token) => (token.line, token.column),
            None => self.end,
        };
        self.open()?;
        self.keyword("rule")?;

        self.open()?;
        self.keyword("lower")?;
        let pattern = self.term()?;
        self.close("lower")?;

        let guard = if self.at_clause("if") {
            self.at += 1;
            self.at += 1;
            let guard = self.term()?;
            self.close("if")?;
            Some(guard)
        } else {
            None
        };

        let replacement = self.term()?;

        self.open()?;
        self.keyword("spec")?;
        let spec = self.term()?;
        self.close("spec")?;

        // Last, because it is about what happens to the claim rather than part of it, and
        // optional, because most rules have no reason to expect the solver to struggle.
        let bounded = if self.at_clause("bounded") {
            self.at += 2;
            let why = self.string()?;
            self.close("bounded")?;
            Some(why)
        } else {
            None
        };

        self.close("rule")?;
        Ok(Rule { pattern, guard, replacement, spec, bounded, line, column })
    }

    /// The prose in a `(bounded ...)` clause.
    fn string(&mut self) -> Result<String, Error> {
        match self.peek() {
            Some(Token::Str(text)) if !text.trim().is_empty() => {
                let text = (*text).to_owned();
                self.at += 1;
                Ok(text)
            }
            Some(Token::Str(_)) => {
                Err(self.error("a bounded proof needs a reason somebody signed for".to_owned()))
            }
            _ => Err(self.error("expected a reason, in quotation marks".to_owned())),
        }
    }

    fn term(&mut self) -> Result<Term, Error> {
        let Some(token) = self.tokens.get(self.at) else {
            return Err(self.error("expected a term and the file ended".to_owned()));
        };
        let (line, column) = (token.line, token.column);
        match &token.token {
            Token::Int(value) => {
                self.at += 1;
                Ok(Term { kind: TermKind::Int(*value), line, column })
            }
            // A bare name is a variable and a parenthesised one is an application. That is the
            // whole of the distinction, which is why a constructor that takes nothing is still
            // written `(result)`: without the parentheses there would be no way to tell it from
            // a variable nobody bound.
            Token::Atom(name) => {
                self.at += 1;
                Ok(Term { kind: TermKind::Var((*name).to_owned()), line, column })
            }
            Token::Close => Err(self.error("expected a term and found a `)`".to_owned())),
            Token::Str(_) => Err(self.error(
                "a string is prose for a person and is not something a term can be".to_owned(),
            )),
            Token::Open => {
                self.at += 1;
                let head = match self.peek() {
                    Some(Token::Atom(head)) => {
                        let head = *head;
                        self.at += 1;
                        head
                    }
                    _ => return Err(self.error("expected a name after the `(`".to_owned())),
                };
                if RESERVED.contains(&head) {
                    // Reported at the parenthesis rather than at the name, because what is
                    // actually missing is a parenthesis somewhere above and this is the first
                    // place that is visible.
                    let message =
                        format!("`{head}` belongs to a rule's own shape, not inside a term");
                    self.at -= 2;
                    return Err(self.error(message));
                }
                let mut args = Vec::new();
                while !matches!(self.peek(), Some(Token::Close)) {
                    if self.peek().is_none() {
                        return Err(self.error(format!("`({head}` was never closed")));
                    }
                    args.push(self.term()?);
                }
                self.at += 1;
                Ok(Term { kind: TermKind::App { head: head.to_owned(), args }, line, column })
            }
        }
    }

    /// Skip to the next thing that looks like the start of a rule, so that one bad rule costs
    /// one error rather than every error after it.
    fn resync(&mut self) {
        self.at += 1;
        while self.at < self.tokens.len() && !self.at_clause("rule") {
            self.at += 1;
        }
    }
}

/// The checks that every consumer of a rule would otherwise have to make for itself.
fn check(path: &str, rule: &Rule, errors: &mut Vec<Error>) {
    let mut found = Vec::new();

    if !matches!(rule.pattern.kind, TermKind::App { .. }) {
        found.push((&rule.pattern, "a pattern has to name something to match".to_owned()));
    }

    // Bound at the first occurrence in the pattern, and only there. A name that occurs twice in
    // one pattern is asking for the two places to be equal, which the matcher has no test for,
    // so it is refused rather than silently read as two independent holes.
    let mut bound: HashSet<&str> = HashSet::new();
    let mut twice = Vec::new();
    let mut in_pattern = Vec::new();
    rule.pattern.walk(&mut |term| match &term.kind {
        TermKind::Var(name) => {
            if !bound.insert(name.as_str()) {
                twice.push((term, format!("`{name}` is bound twice in one pattern")));
            }
        }
        TermKind::App { head, .. } if head == RESULT => {
            let said = "`(result)` is what the replacement produces, so it means nothing here";
            in_pattern.push((term, said.to_owned()));
        }
        _ => {}
    });
    found.extend(twice);
    found.extend(in_pattern);

    let mut clauses =
        vec![("the replacement", &rule.replacement), ("the specification", &rule.spec)];
    if let Some(guard) = &rule.guard {
        clauses.insert(0, ("the guard", guard));
    }
    let mut loose = Vec::new();
    for (what, term) in clauses {
        term.walk(&mut |term| match &term.kind {
            TermKind::Var(name) if !bound.contains(name.as_str()) => {
                let said = format!("`{name}` is used in {what} and the pattern never bound it");
                loose.push((term, said));
            }
            // The specification is the one place that can talk about what the rule produced,
            // because it is the only clause written after the fact rather than to make it.
            TermKind::App { head, .. } if head == RESULT && what != "the specification" => {
                let said = format!("`(result)` belongs in the specification, not in {what}");
                loose.push((term, said));
            }
            _ => {}
        });
    }
    found.extend(loose);

    for (term, message) in found {
        errors.push(Error { path: path.to_owned(), line: term.line, column: term.column, message });
    }
}
