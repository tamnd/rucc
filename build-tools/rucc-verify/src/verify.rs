//! Turning a rule into a question, and the answers into a report.

use std::collections::BTreeSet;

use rucc_rules::{Error, Rule, Term, TermKind};

use crate::model::Model;
use crate::solver::{Answer, Solver};

/// What became of one rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Nothing makes the claim false.
    Discharged,
    /// Something does, and this is what the solver printed of it.
    Refuted(String),
    /// The solver gave up. Not a pass.
    Unknown,
}

/// What became of a rule set.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    /// One verdict per rule, in the order the rules were given.
    pub verdicts: Vec<Verdict>,
}

impl Report {
    /// How many rules were discharged.
    #[must_use]
    pub fn discharged(&self) -> usize {
        self.verdicts.iter().filter(|v| **v == Verdict::Discharged).count()
    }

    /// Whether every rule was discharged. Anything else keeps the rule set out, including a
    /// solver that gave up, because "we could not tell" is not "it is correct".
    #[must_use]
    pub fn all_discharged(&self) -> bool {
        self.verdicts.iter().all(|v| *v == Verdict::Discharged)
    }
}

/// The SMT-LIB question one rule asks.
///
/// This is separate from asking it so that the query can be read, kept in a test, and handed to
/// a solver by hand when one is arguing with it.
///
/// # Errors
///
/// Anything the model cannot write out, which is any term nobody has said the meaning of.
pub fn query(path: &str, rule: &Rule, model: &Model) -> Result<String, Error> {
    let width = width_of(&rule.pattern);
    let mut out = String::from("(set-logic QF_BV)\n");

    // Declared in sorted order rather than in the order the pattern binds them, because the
    // query is something a test pins and a diff is easier to read than it is to regenerate.
    for name in variables(&rule.pattern) {
        out.push_str(&format!("(declare-const {name} (_ BitVec {width}))\n"));
    }

    if let Some(guard) = &rule.guard {
        // An assumption, not part of the claim. A rule that only holds for some constants is
        // only being asked about those constants.
        out.push_str(&format!("(assert {})\n", model.write(path, guard, width)?));
    }

    // Two obligations, asked as one question. The first is the one that matters: what the
    // pattern means and what the replacement means have to be the same thing, both read out of
    // the model rather than out of anybody's description of them. The second is the rule's own
    // `spec` clause, which is written by hand and so is worth checking rather than trusting: a
    // rule whose stated claim is not what its pattern actually means would otherwise verify
    // against its own mistake.
    let matched = model.write(path, &rule.pattern, width)?;
    let produced = model.write(path, &rule.replacement, width)?;
    let claim = model.write(path, &substitute(&rule.spec, &produced), width)?;
    out.push_str(&format!("(assert (not (and (= {matched} {produced}) {claim})))\n"));
    out.push_str("(check-sat)\n(get-model)\n");
    Ok(out)
}

/// Ask about every rule.
///
/// # Errors
///
/// Anything the model cannot write out, and anything that stops the solver from running.
pub fn verify(
    path: &str,
    rules: &[Rule],
    model: &Model,
    solver: &Solver,
) -> Result<Report, Vec<Error>> {
    let mut report = Report::default();
    let mut errors = Vec::new();

    for rule in rules {
        let asked = match query(path, rule, model) {
            Ok(asked) => asked,
            Err(error) => {
                errors.push(error);
                continue;
            }
        };
        match solver.ask(&asked) {
            Ok(Answer::Unsat) => report.verdicts.push(Verdict::Discharged),
            Ok(Answer::Sat(model)) => report.verdicts.push(Verdict::Refuted(model)),
            Ok(Answer::Unknown) => report.verdicts.push(Verdict::Unknown),
            Err(problem) => errors.push(Error {
                path: path.to_owned(),
                line: rule.line,
                column: rule.column,
                message: format!("the solver could not be run: {problem}"),
            }),
        }
    }

    if errors.is_empty() { Ok(report) } else { Err(errors) }
}

/// Put the replacement's meaning where the specification says `(result)`.
///
/// This is a substitution on the written form rather than on the term, because what the
/// replacement means is SMT-LIB text by the time it is known and there is nothing to put back
/// into a term.
fn substitute(spec: &Term, produced: &str) -> Term {
    match &spec.kind {
        TermKind::App { head, args } if head == "result" && args.is_empty() => {
            Term { kind: TermKind::Var(produced.to_owned()), line: spec.line, column: spec.column }
        }
        TermKind::App { head, args } => Term {
            kind: TermKind::App {
                head: head.clone(),
                args: args.iter().map(|arg| substitute(arg, produced)).collect(),
            },
            line: spec.line,
            column: spec.column,
        },
        _ => spec.clone(),
    }
}

/// Every name the pattern binds, sorted.
fn variables(pattern: &Term) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    pattern.walk(&mut |term| {
        if let TermKind::Var(name) = &term.kind {
            out.insert(name.clone());
        }
    });
    out
}

/// The width a rule works in, taken from the suffix on its pattern's opcode.
///
/// One width for the whole rule is a simplification and the crate documentation says so. It is
/// right for every rule that does not change width, which is most of them, and a rule that does
/// needs the terms to carry types.
fn width_of(pattern: &Term) -> u32 {
    let TermKind::App { head, .. } = &pattern.kind else {
        return DEFAULT_WIDTH;
    };
    head.rsplit_once('.')
        .and_then(|(_, suffix)| suffix.strip_prefix('i'))
        .and_then(|bits| bits.parse::<u32>().ok())
        .unwrap_or(DEFAULT_WIDTH)
}

/// What a rule works in when its opcode does not say. Every opcode in the IR does say, so this
/// is what a hand written test rule gets rather than something the real rule set relies on.
const DEFAULT_WIDTH: u32 = 64;
