//! What a rule is, once it has been read.

use std::fmt;

/// A term: the pattern a rule matches, the replacement it produces, and the two clauses that
/// constrain it are all one shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Term {
    /// Which of the three kinds this is.
    pub kind: TermKind,
    /// The line it starts on, counted from one.
    pub line: u32,
    /// The column it starts at, counted from one.
    pub column: u32,
}

/// The three kinds of term.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TermKind {
    /// A name standing for whatever the pattern bound it to.
    Var(String),
    /// A literal.
    Int(i128),
    /// A head applied to arguments, which is every opcode, every constructor and every operator
    /// in a specification.
    App {
        /// The name in head position.
        head: String,
        /// What it is applied to, possibly nothing, as in `(result)`.
        args: Vec<Term>,
    },
}

impl Term {
    /// Walk this term and everything under it, outermost first.
    ///
    /// The lifetime is written out so that what the visitor is handed lives as long as the term
    /// does, which is what lets a caller collect the places it found rather than only count them.
    pub fn walk<'t>(&'t self, visit: &mut impl FnMut(&'t Term)) {
        visit(self);
        if let TermKind::App { args, .. } = &self.kind {
            for arg in args {
                arg.walk(visit);
            }
        }
    }
}

impl fmt::Display for Term {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            TermKind::Var(name) => f.write_str(name),
            TermKind::Int(value) => write!(f, "{value}"),
            TermKind::App { head, args } => {
                write!(f, "({head}")?;
                for arg in args {
                    write!(f, " {arg}")?;
                }
                f.write_str(")")
            }
        }
    }
}

/// What a rule rewrites into.
///
/// The two kinds are matched by the same trie and verified by the same obligation, and the only
/// thing that separates them is what the replacement is written in. Keeping them one language
/// rather than two is the whole reason `spec/09-optimizer.md` section 9.3 and
/// `spec/10-backend.md` section 10.2 ask for a rule DSL at all, because a rewrite and a
/// lowering are the same claim about two terms and there is no reason to say it twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleKind {
    /// IR to IR. The replacement is IR, so a rewrite can be applied over and over and the
    /// result is still something later rules match. `spec/optimizer/13-rewrite-rules.md`.
    Simplify,
    /// IR to machine. The replacement is a machine term, so a lowering is the last thing that
    /// happens to a value and nothing matches what it produces. `spec/10-backend.md`.
    Lower,
}

impl RuleKind {
    /// The keyword that introduces a rule of this kind.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Simplify => "simplify",
            Self::Lower => "lower",
        }
    }
}

impl fmt::Display for RuleKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One rule: what it matches, what it produces, and what makes that sound.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    /// Whether the replacement is IR or a machine term.
    pub kind: RuleKind,
    /// The term to match, which is IR for a rewrite and IR for a lowering.
    pub pattern: Term,
    /// A condition on the match, which is where a rule that only holds for some constants says
    /// so. It sits between the pattern and the replacement because it is part of deciding
    /// whether the rule fires, not part of what firing produces.
    pub guard: Option<Term>,
    /// What to put in the matched term's place.
    pub replacement: Term,
    /// The bitvector claim relating the two, which is what `rucc-verify` discharges. It is not
    /// optional, because a rule set that lets one rule through without a specification is a rule
    /// set with an unverified rule in it.
    pub spec: Term,
    /// Why a proof at narrower widths is enough for this rule, when there is a reason to think
    /// the solver will not manage the real one. A rule carrying this is not excused anything:
    /// it is still asked at its own width first, and the clause only says what a person is
    /// willing to sign for if the answer comes back as a shrug.
    pub bounded: Option<String>,
    /// The line the rule starts on.
    pub line: u32,
    /// The column the rule starts at.
    pub column: u32,
}

impl fmt::Display for Rule {
    /// Prints the rule back in the shape `spec/10-backend.md` writes it: one clause to a line,
    /// with the continuation lines under the pattern.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "(rule ({} {})", self.kind, self.pattern)?;
        if let Some(guard) = &self.guard {
            writeln!(f, "      (if {guard})")?;
        }
        writeln!(f, "      {}", self.replacement)?;
        match &self.bounded {
            Some(why) => {
                writeln!(f, "      (spec {})", self.spec)?;
                write!(f, "      (bounded \"{why}\"))")
            }
            None => write!(f, "      (spec {}))", self.spec),
        }
    }
}
