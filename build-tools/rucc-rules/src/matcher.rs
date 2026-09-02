//! Rules to the automaton that matches them.
//!
//! A pattern is a tree and the subject is a tree, and the obvious way to match one against the
//! other is a chain of conditionals per rule. That is what `spec/10-backend.md` says not to
//! build: with several hundred rules per target it re-tests the same opcode hundreds of times,
//! and it puts the order the rules are tried in beyond anybody's control.
//!
//! What is built instead is a trie over the patterns, flattened. Every pattern becomes a
//! sequence of steps read in pre-order, and patterns that begin the same way share the steps
//! they agree on, so testing that a term is an `add.i64` happens once no matter how many rules
//! begin with one. Matching walks the subject in the same pre-order, which is what makes the
//! sequence well defined: at any node of the trie, every rule that reaches it has consumed the
//! same shape of subject, so there is one stack of remaining subterms rather than one per rule.
//!
//! Specificity falls out of the shape rather than being sorted for. At each node the concrete
//! tests are tried before the wildcard, so a rule that names an operand is always tried before
//! a rule that takes anything there, which is the maximal munch that document asks for. Among
//! rules that are equally specific the first one written wins, which is what `-O0` wants and is
//! what the single-pass mode in section 10.3 is defined to do.

use std::fmt;

use crate::ast::{Rule, Term, TermKind};
use crate::error::Error;

/// One step of a flattened pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    /// The subterm here must be this head applied to this many arguments.
    App { head: String, arity: usize },
    /// The subterm here must be this literal.
    Int(i128),
    /// Anything goes here, and it is remembered under this name.
    Bind(String),
}

/// A test on one subterm. This is [`Step`] without the wildcard, because a wildcard is not a
/// test: it is the branch taken when no test matched.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Test {
    App { head: String, arity: usize },
    Int(i128),
}

/// One node of the trie.
#[derive(Debug, Default)]
struct Node {
    /// The concrete tests, in the order they were first written, tried before the wildcard.
    tests: Vec<(Test, usize)>,
    /// The branch that takes anything, and the name it binds it under.
    wildcard: Option<(String, usize)>,
    /// The rule that ends here, if one does.
    accept: Option<usize>,
}

/// The automaton a rule set compiles into.
#[derive(Debug)]
pub struct Matcher {
    nodes: Vec<Node>,
}

/// What a successful match found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match<'t> {
    /// The index into the rule set of the rule that fired.
    pub rule: usize,
    /// What the pattern's variables were bound to, in the order the pattern binds them.
    pub bindings: Vec<(String, &'t Term)>,
}

impl<'t> Match<'t> {
    /// What one name was bound to, or nothing if the pattern never bound it.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&'t Term> {
        self.bindings.iter().find(|(bound, _)| bound == name).map(|(_, term)| *term)
    }
}

impl Matcher {
    /// Compile a rule set.
    ///
    /// # Errors
    ///
    /// A rule whose pattern is one an earlier rule already has can never fire, and that is
    /// reported rather than silently dropped. It is always a mistake: either the second rule was
    /// meant to say something else, or one of the two should not be there.
    pub fn build(path: &str, rules: &[Rule]) -> Result<Matcher, Vec<Error>> {
        let mut matcher = Matcher { nodes: vec![Node::default()] };
        let mut errors = Vec::new();

        for (index, rule) in rules.iter().enumerate() {
            let mut at = 0;
            for step in flatten(&rule.pattern) {
                at = matcher.follow(at, step);
            }
            match matcher.nodes[at].accept {
                Some(first) => errors.push(Error {
                    path: path.to_owned(),
                    line: rule.line,
                    column: rule.column,
                    message: format!(
                        "this rule can never fire, because the rule on line {} matches everything it does",
                        rules[first].line
                    ),
                }),
                None => matcher.nodes[at].accept = Some(index),
            }
        }

        if errors.is_empty() { Ok(matcher) } else { Err(errors) }
    }

    /// Add one step at one node, reusing the branch if it is already there.
    fn follow(&mut self, at: usize, step: Step) -> usize {
        let test = match step {
            Step::App { head, arity } => Test::App { head, arity },
            Step::Int(value) => Test::Int(value),
            Step::Bind(name) => {
                if let Some((_, next)) = &self.nodes[at].wildcard {
                    // The name is the first one written. Two rules that put different names in
                    // the same hole are the same automaton, and the binding is reported back
                    // under the name of the rule that fired rather than under this one.
                    return *next;
                }
                let next = self.push();
                self.nodes[at].wildcard = Some((name, next));
                return next;
            }
        };
        if let Some((_, next)) = self.nodes[at].tests.iter().find(|(have, _)| *have == test) {
            return *next;
        }
        let next = self.push();
        self.nodes[at].tests.push((test, next));
        next
    }

    fn push(&mut self) -> usize {
        self.nodes.push(Node::default());
        self.nodes.len() - 1
    }

    /// Match one term against the whole rule set, returning the rule that fires.
    ///
    /// The term is matched as a whole. Finding the subterms of a function worth matching is the
    /// selector's job and not this one's.
    #[must_use]
    pub fn find<'t>(&self, term: &'t Term) -> Option<Match<'t>> {
        let mut bindings = Vec::new();
        let rule = self.run(0, vec![term], &mut bindings)?;
        Some(Match { rule, bindings })
    }

    /// Walk the trie and the subject together.
    ///
    /// `left` is the subterms still to be matched, innermost last, so that popping gives the
    /// pre-order the patterns were flattened in.
    fn run<'t>(
        &self,
        at: usize,
        mut left: Vec<&'t Term>,
        bindings: &mut Vec<(String, &'t Term)>,
    ) -> Option<usize> {
        let Some(subject) = left.pop() else {
            return self.nodes[at].accept;
        };
        let node = &self.nodes[at];

        for (test, next) in &node.tests {
            let matched = match (test, &subject.kind) {
                (Test::Int(want), TermKind::Int(have)) => want == have,
                (Test::App { head, arity }, TermKind::App { head: name, args }) => {
                    head == name && *arity == args.len()
                }
                _ => false,
            };
            if !matched {
                continue;
            }
            let mut deeper = left.clone();
            if let TermKind::App { args, .. } = &subject.kind {
                deeper.extend(args.iter().rev());
            }
            let depth = bindings.len();
            if let Some(rule) = self.run(*next, deeper, bindings) {
                return Some(rule);
            }
            bindings.truncate(depth);
        }

        // The wildcard is last, which is the whole of what "specificity order" means here.
        let (name, next) = node.wildcard.as_ref()?;
        let depth = bindings.len();
        bindings.push((name.clone(), subject));
        if let Some(rule) = self.run(*next, left, bindings) {
            return Some(rule);
        }
        bindings.truncate(depth);
        None
    }

    /// How many nodes the trie has, which is what a rule set costs to match against.
    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether the rule set was empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.len() <= 1
    }
}

/// Flatten a pattern into the steps that match it, in the pre-order the matcher walks.
fn flatten(pattern: &Term) -> Vec<Step> {
    let mut out = Vec::new();
    push_steps(pattern, &mut out);
    out
}

fn push_steps(term: &Term, out: &mut Vec<Step>) {
    match &term.kind {
        TermKind::Var(name) => out.push(Step::Bind(name.clone())),
        TermKind::Int(value) => out.push(Step::Int(*value)),
        TermKind::App { head, args } => {
            out.push(Step::App { head: head.clone(), arity: args.len() });
            for arg in args {
                push_steps(arg, out);
            }
        }
    }
}

impl fmt::Display for Matcher {
    /// Prints the trie, one branch to a line, indented by depth. This is what makes a rule set's
    /// shape reviewable: two rules that share a prefix share a line, and a rule that can only be
    /// reached through a wildcard is visibly the last thing tried.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.show(f, 0, 0)
    }
}

impl Matcher {
    fn show(&self, f: &mut fmt::Formatter<'_>, at: usize, depth: usize) -> fmt::Result {
        let pad = "  ".repeat(depth);
        let node = &self.nodes[at];
        if let Some(rule) = node.accept {
            writeln!(f, "{pad}=> rule {rule}")?;
        }
        for (test, next) in &node.tests {
            match test {
                Test::App { head, arity } => writeln!(f, "{pad}{head}/{arity}")?,
                Test::Int(value) => writeln!(f, "{pad}{value}")?,
            }
            self.show(f, *next, depth + 1)?;
        }
        if let Some((name, next)) = &node.wildcard {
            writeln!(f, "{pad}bind {name}")?;
            self.show(f, *next, depth + 1)?;
        }
        Ok(())
    }
}
