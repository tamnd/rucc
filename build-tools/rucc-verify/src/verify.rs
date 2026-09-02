//! Turning a rule into a question, and the answers into a report.

use std::collections::BTreeSet;
use std::fmt;

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
    /// The claim holds at every width narrower than the rule's own, and the rule carries a
    /// written reason for taking that as enough. A pass, and a counted one.
    Bounded {
        /// The widths it was proved at, narrowest first.
        widths: Vec<u32>,
        /// The reason the rule gives, which is what a reviewer signed for.
        why: String,
    },
    /// The solver gave up. Not a pass.
    Unknown,
}

impl Verdict {
    /// Whether a rule with this verdict may enter the rule set.
    #[must_use]
    pub fn accepted(&self) -> bool {
        matches!(self, Verdict::Discharged | Verdict::Bounded { .. })
    }

    /// Why it may not, as a sentence, or nothing when it may.
    #[must_use]
    pub fn refusal(&self) -> Option<String> {
        match self {
            Verdict::Discharged | Verdict::Bounded { .. } => None,
            Verdict::Refuted(model) => {
                Some(format!("this rule is not true, and here is what makes it false: {model}"))
            }
            Verdict::Unknown => Some(
                "the solver could not settle this rule, and a rule nobody has proved does not \
                 enter the rule set"
                    .to_owned(),
            ),
        }
    }
}

/// What became of a rule set.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    /// One verdict per rule, in the order the rules were given.
    pub verdicts: Vec<Verdict>,
}

impl Report {
    /// How many rules were discharged at their own width.
    #[must_use]
    pub fn discharged(&self) -> usize {
        self.verdicts.iter().filter(|v| **v == Verdict::Discharged).count()
    }

    /// How many rules got a bounded proof instead.
    ///
    /// `spec/15-testing.md` section 15.5 asks for this number to be reported rather than merely
    /// known, because it going up is the signal that the rule set is drifting towards claims
    /// nobody is checking at the width the compiler runs at.
    #[must_use]
    pub fn bounded(&self) -> usize {
        self.verdicts.iter().filter(|v| matches!(v, Verdict::Bounded { .. })).count()
    }

    /// Whether every rule was discharged at its own width. A bounded proof is not one of these.
    #[must_use]
    pub fn all_discharged(&self) -> bool {
        self.verdicts.iter().all(|v| *v == Verdict::Discharged)
    }

    /// Whether every rule may enter the rule set, which allows a bounded proof and allows
    /// nothing else. A solver that gave up is not a pass, because "we could not tell" is not
    /// "it is correct".
    #[must_use]
    pub fn accepted(&self) -> bool {
        self.verdicts.iter().all(Verdict::accepted)
    }
}

impl fmt::Display for Report {
    /// One line, which is what a build prints and what a person reads in a log.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let refused = self.verdicts.len() - self.discharged() - self.bounded();
        let rules = if self.verdicts.len() == 1 { "rule" } else { "rules" };
        write!(
            f,
            "{} {rules}: {} discharged, {} by bounded proof, {} refused",
            self.verdicts.len(),
            self.discharged(),
            self.bounded(),
            refused
        )
    }
}

/// The SMT-LIB question one rule asks, at the width the rule works in.
///
/// This is separate from asking it so that the query can be read, kept in a test, and handed to
/// a solver by hand when one is arguing with it.
///
/// # Errors
///
/// Anything the model cannot write out, which is any term nobody has said the meaning of.
pub fn query(path: &str, rule: &Rule, model: &Model) -> Result<String, Error> {
    query_at(path, rule, model, width_of(&rule.pattern))
}

/// The same question asked at a width somebody chose.
///
/// This is what a bounded proof is made of: the rule's own claim, in narrower bitvectors than
/// the ones it will run in.
///
/// # Errors
///
/// Anything the model cannot write out, which is any term nobody has said the meaning of.
pub fn query_at(path: &str, rule: &Rule, model: &Model, width: u32) -> Result<String, Error> {
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

/// The widths a bounded proof is taken over, narrowest first.
///
/// Two of them rather than one, because a claim that holds at a single width can hold for
/// reasons that are about that width. Both of them small, because the claims that need a
/// bounded proof at all are the ones mixing multiplication with division, and one of those is
/// as far out of reach at sixteen bits as it is at sixty four: the rule the tests use is
/// answered in hundredths of a second at eight bits and not at all at sixteen. Only widths
/// narrower than the rule's own are used, so a rule that already works in four bits has nothing
/// to fall back to.
pub const BOUNDED_WIDTHS: [u32; 2] = [4, 8];

/// Ask about every rule.
///
/// A rule that the solver settles at its own width is discharged and that is the end of it. A
/// rule it gives up on is asked again at [`BOUNDED_WIDTHS`], but only if the rule carries a
/// written reason for taking narrow widths as enough, because a bounded proof is a judgement
/// somebody makes and not a fallback a tool takes on its own.
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
        let width = width_of(&rule.pattern);
        match ask(path, rule, model, solver, width) {
            Err(error) => errors.push(error),
            Ok(Answer::Unsat) => report.verdicts.push(Verdict::Discharged),
            Ok(Answer::Sat(found)) => report.verdicts.push(Verdict::Refuted(found)),
            Ok(Answer::Unknown) => match &rule.bounded {
                None => report.verdicts.push(Verdict::Unknown),
                Some(why) => match bounded(path, rule, model, solver, width, why) {
                    Ok(verdict) => report.verdicts.push(verdict),
                    Err(error) => errors.push(error),
                },
            },
        }
    }

    if errors.is_empty() { Ok(report) } else { Err(errors) }
}

/// Verify a rule set and refuse the whole of it if anything in it cannot enter.
///
/// This is the gate `spec/17-milestones.md` asks for. It refuses the file rather than dropping
/// the rules that failed, because a compiler built from the rules that happened to pass is a
/// compiler nobody described: what it does with the terms the dropped rules matched is then a
/// question about the order of the rest.
///
/// # Errors
///
/// One error per rule that may not enter, at the line the rule starts on, and anything that
/// stopped the verification from happening at all.
pub fn admit(
    path: &str,
    rules: &[Rule],
    model: &Model,
    solver: &Solver,
) -> Result<Report, Vec<Error>> {
    let report = verify(path, rules, model, solver)?;
    let mut errors = Vec::new();
    for (rule, verdict) in rules.iter().zip(&report.verdicts) {
        if let Some(said) = verdict.refusal() {
            errors.push(Error {
                path: path.to_owned(),
                line: rule.line,
                column: rule.column,
                message: said,
            });
        }
    }
    if errors.is_empty() { Ok(report) } else { Err(errors) }
}

/// Put one question to the solver.
fn ask(
    path: &str,
    rule: &Rule,
    model: &Model,
    solver: &Solver,
    width: u32,
) -> Result<Answer, Error> {
    let asked = query_at(path, rule, model, width)?;
    solver.ask(&asked).map_err(|problem| Error {
        path: path.to_owned(),
        line: rule.line,
        column: rule.column,
        message: format!("the solver could not be run: {problem}"),
    })
}

/// Ask the rule again at the narrow widths, once the real one has come back a shrug.
///
/// Every width has to come back `unsat`. A counterexample at a narrow width is reported as the
/// refutation it looks like, named with the width it was found at, because the two things it
/// can be are a rule that is wrong and a rule whose constants do not fit in four bits, and both
/// are for a person to look at rather than for this to decide.
fn bounded(
    path: &str,
    rule: &Rule,
    model: &Model,
    solver: &Solver,
    width: u32,
    why: &str,
) -> Result<Verdict, Error> {
    let mut proved = Vec::new();
    for narrow in BOUNDED_WIDTHS.iter().copied().filter(|narrow| *narrow < width) {
        match ask(path, rule, model, solver, narrow)? {
            Answer::Unsat => proved.push(narrow),
            Answer::Sat(found) => {
                let said = format!("at {narrow} bits, where the rule works in {width}: {found}");
                return Ok(Verdict::Refuted(said));
            }
            Answer::Unknown => return Ok(Verdict::Unknown),
        }
    }
    if proved.is_empty() {
        return Ok(Verdict::Unknown);
    }
    Ok(Verdict::Bounded { widths: proved, why: why.to_owned() })
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
