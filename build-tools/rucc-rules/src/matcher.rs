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
//!
//! A name written twice in one pattern is a claim that the two places hold the same thing, which
//! is how the identities of `spec/optimizer/13-rewrite-rules.md` section 13.4 say `x & x` and
//! `x - x`. The second occurrence becomes a test rather than a binding, so it costs one
//! comparison and sits with the other concrete tests, ahead of the wildcard, where a rule about
//! one value in both operands belongs.
//!
//! # Choosing a branch without reading every branch
//!
//! `spec/optimizer/36-lowering-and-isel.md` section 36.5 asks for the decision to be on the shape
//! of the term rather than on the identity of the pattern, which is the difference between a trie
//! that is a tree and a trie that is a tree with a list at every node. Sharing a prefix already
//! means the head of a term is tested once rather than once per rule, but it does not say how the
//! branch is found, and reading a node's branches in the order the rules were written is a scan
//! over all of them. That costs what the widest node is wide: the x86-64 rule set has a hundred
//! and sixty seven different heads a pattern can begin with, so choosing the branch for an
//! `add.i64` meant asking a hundred and sixty six other questions first, once for every
//! instruction the selector looks at, and a term no rule covers asked all of them.
//!
//! So a node holds its branches by the kind of question they ask, and the two kinds that can be
//! searched are kept sorted: the heads by name and then by how many arguments they take, and the
//! literals by value. At most one of either can match a subterm, because a term has one head and
//! a constant has one value, so the order inside those two is not observable and sorting them
//! costs nothing. Finding the branch is then a binary search, which is eight comparisons at that
//! widest node rather than a hundred and sixty seven.
//!
//! # The order the kinds are tried in
//!
//! Which kind is asked first is a heuristic, and section 36.5 is explicit that a heuristic is to
//! be stated rather than left to be discovered by reading what came out. The order is: the head
//! of the term, then its value as a literal, then whether it is what an earlier binding took,
//! then the hole that takes anything. The first three are all concrete and the hole is last,
//! which is the specificity order above and is the part that decides which rule fires.
//!
//! The order among the first three decides nothing in any rule set here, because deciding
//! something would need one node to ask two kinds of question about one place, and none does: a
//! literal is only ever written where a pattern has descended into a constant, and a name written
//! twice is only ever written where the first occurrence put a hole. [`Matcher::shape`] counts
//! the nodes that mix kinds for exactly this reason, so that a rule set which starts to depend on
//! the order is a number that changed rather than a surprise in the output.

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
    /// The subterm here must be what this binding of the same pattern already took, which is
    /// what the second occurrence of a name means.
    Same(usize),
}

/// One node of the trie.
///
/// The branches are held by the kind of question they ask rather than in one list, which is what
/// lets the two searchable kinds be searched. A hole is not one of them: it is not a question, it
/// is what is left when none of the questions was answered.
#[derive(Debug, Default)]
pub(crate) struct Node {
    /// The branches taken on the head of the subterm, sorted by name and then by how many
    /// arguments it takes.
    pub(crate) heads: Vec<(String, usize, usize)>,
    /// The branches taken on the value of a subterm that is a constant, sorted by value.
    pub(crate) ints: Vec<(i128, usize)>,
    /// The branches taken when the subterm is what an earlier binding took, in the order the
    /// rules were written, because two of them can match one subterm.
    pub(crate) same: Vec<(usize, usize)>,
    /// The branch that takes anything, and the name it binds it under.
    pub(crate) wildcard: Option<(String, usize)>,
    /// The rules that end here, in the order they were written. Every one but the last has a
    /// guard, and the first whose guard holds is the one that fires.
    pub(crate) accept: Vec<usize>,
}

impl Node {
    /// Every branch this node has, in the order the walk tries them, which is what printing it
    /// and counting it are both written against.
    fn branches(&self) -> impl Iterator<Item = (Shown<'_>, usize)> {
        let heads = self.heads.iter().map(|(head, arity, next)| (Shown::App(head, *arity), *next));
        let ints = self.ints.iter().map(|&(value, next)| (Shown::Int(value), next));
        let same = self.same.iter().map(|&(index, next)| (Shown::Same(index), next));
        heads.chain(ints).chain(same)
    }

    /// How many kinds of question this node asks. More than one means the order the kinds are
    /// tried in decides which rule fires here.
    fn kinds(&self) -> usize {
        usize::from(!self.heads.is_empty())
            + usize::from(!self.ints.is_empty())
            + usize::from(!self.same.is_empty())
    }
}

/// One branch of a node as something to print.
enum Shown<'a> {
    App(&'a str, usize),
    Int(i128),
    Same(usize),
}

/// What a rule set costs to match against, which is what the header of a generated table says
/// and what a test that the tree is a tree asserts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shape {
    /// How many nodes the trie has.
    pub nodes: usize,
    /// How many branches the widest node has, which is what a scan over it would cost.
    pub widest: usize,
    /// How many comparisons a binary search over that many branches takes.
    pub search: usize,
    /// How many nodes ask more than one kind of question, and so depend on the order the kinds
    /// are tried in. Nothing shipped here does.
    pub mixed: usize,
}

/// The automaton a rule set compiles into.
#[derive(Debug)]
pub struct Matcher {
    pub(crate) nodes: Vec<Node>,
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
    /// A rule whose pattern is one an earlier rule without a guard already has can never fire,
    /// and that is reported rather than silently dropped. It is always a mistake: either the
    /// second rule was meant to say something else, or one of the two should not be there. After
    /// a rule with a guard it is the next thing tried when the guard does not hold.
    pub fn build(path: &str, rules: &[Rule]) -> Result<Matcher, Vec<Error>> {
        let mut matcher = Matcher { nodes: vec![Node::default()] };
        let mut errors = Vec::new();

        for (index, rule) in rules.iter().enumerate() {
            let mut at = 0;
            for step in flatten(&rule.pattern) {
                at = matcher.follow(at, step);
            }
            let accept = &mut matcher.nodes[at].accept;
            match accept.iter().find(|&&first| rules[first].guard.is_none()) {
                Some(&first) => errors.push(Error {
                    path: path.to_owned(),
                    line: rule.line,
                    column: rule.column,
                    message: format!(
                        "this rule can never fire, because the rule on line {} matches everything it does",
                        rules[first].line
                    ),
                }),
                None => accept.push(index),
            }
        }

        if !errors.is_empty() {
            return Err(errors);
        }
        matcher.sort();
        Ok(matcher)
    }

    /// Add one step at one node, reusing the branch if it is already there.
    fn follow(&mut self, at: usize, step: Step) -> usize {
        match step {
            Step::App { head, arity } => {
                let found = self.nodes[at]
                    .heads
                    .iter()
                    .find(|(have, count, _)| *have == head && *count == arity);
                if let Some(&(_, _, next)) = found {
                    return next;
                }
                let next = self.push();
                self.nodes[at].heads.push((head, arity, next));
                next
            }
            Step::Int(value) => {
                if let Some(&(_, next)) =
                    self.nodes[at].ints.iter().find(|(have, _)| *have == value)
                {
                    return next;
                }
                let next = self.push();
                self.nodes[at].ints.push((value, next));
                next
            }
            Step::Same(index) => {
                if let Some(&(_, next)) =
                    self.nodes[at].same.iter().find(|(have, _)| *have == index)
                {
                    return next;
                }
                let next = self.push();
                self.nodes[at].same.push((index, next));
                next
            }
            Step::Bind(name) => {
                if let Some((_, next)) = &self.nodes[at].wildcard {
                    // The name is the first one written. Two rules that put different names in
                    // the same hole are the same automaton, and the binding is reported back
                    // under the name of the rule that fired rather than under this one.
                    return *next;
                }
                let next = self.push();
                self.nodes[at].wildcard = Some((name, next));
                next
            }
        }
    }

    /// Put the searchable branches in the order a search needs them.
    ///
    /// This is the last thing the build does, so that everything before it can add a branch by
    /// pushing. Nothing about which rule fires depends on it: one head matches a term and one
    /// value matches a constant, so the order inside either list is not something a match can
    /// observe. What it buys is that the walk can binary search rather than read the list.
    fn sort(&mut self) {
        for node in &mut self.nodes {
            node.heads.sort_by(|(head, arity, _), (other, count, _)| {
                head.cmp(other).then(arity.cmp(count))
            });
            node.ints.sort_by_key(|&(value, _)| value);
        }
    }

    fn push(&mut self) -> usize {
        self.nodes.push(Node::default());
        self.nodes.len() - 1
    }

    /// Match one term against the whole rule set, returning the rule that fires.
    ///
    /// Guards are not evaluated here, so of rules that share a pattern it is the first.
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
            return self.nodes[at].accept.first().copied();
        };
        let node = &self.nodes[at];

        // The head, the value and the repeat, in that order, which is the heuristic the module
        // doc states. At most one head and at most one value can match, so each of those is a
        // search rather than a walk, which is the same shape the compiler's own walk has.
        let mut taken: Vec<usize> = Vec::new();
        if let TermKind::App { head, args } = &subject.kind {
            let found = node
                .heads
                .binary_search_by(|(have, count, _)| {
                    have.as_str().cmp(head.as_str()).then(count.cmp(&args.len()))
                })
                .ok();
            taken.extend(found.map(|at| node.heads[at].2));
        }
        if let TermKind::Int(value) = &subject.kind {
            let found = node.ints.binary_search_by(|(have, _)| have.cmp(value)).ok();
            taken.extend(found.map(|at| node.ints[at].1));
        }
        // Written out rather than compared with `==`, because a term carries where it was
        // written and two occurrences of one name are in two different places.
        for &(index, next) in &node.same {
            if bindings.get(index).is_some_and(|(_, bound)| alike(bound, subject)) {
                taken.push(next);
            }
        }

        for next in taken {
            let mut deeper = left.clone();
            if let TermKind::App { args, .. } = &subject.kind {
                deeper.extend(args.iter().rev());
            }
            let depth = bindings.len();
            if let Some(rule) = self.run(next, deeper, bindings) {
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

    /// What this rule set costs to match against.
    ///
    /// The widest node is the measurement that matters, because it is the one the shape of the
    /// tree was changed for: it is what a scan would read to the end of and what a search reads
    /// eight of. It goes in the header of the generated table, where somebody reviewing a rule
    /// they added can see what adding it did.
    #[must_use]
    pub fn shape(&self) -> Shape {
        let widest = self.nodes.iter().map(|node| node.branches().count()).max().unwrap_or(0);
        Shape {
            nodes: self.nodes.len(),
            widest,
            search: usize::try_from(widest.next_power_of_two().trailing_zeros()).unwrap_or(0),
            mixed: self.nodes.iter().filter(|node| node.kinds() > 1).count(),
        }
    }
}

/// Whether two terms say the same thing, ignoring where each of them was written.
///
/// A [`Term`] holds its line and column, so the derived equality is equality of two occurrences
/// and not of two terms. What a repeated name asks is about the terms.
fn alike(left: &Term, right: &Term) -> bool {
    match (&left.kind, &right.kind) {
        (TermKind::Var(a), TermKind::Var(b)) => a == b,
        (TermKind::Int(a), TermKind::Int(b)) => a == b,
        (TermKind::App { head: a, args: xs }, TermKind::App { head: b, args: ys }) => {
            a == b && xs.len() == ys.len() && xs.iter().zip(ys).all(|(x, y)| alike(x, y))
        }
        _ => false,
    }
}

/// Flatten a pattern into the steps that match it, in the pre-order the matcher walks.
fn flatten(pattern: &Term) -> Vec<Step> {
    let mut out = Vec::new();
    let mut bound: Vec<&str> = Vec::new();
    push_steps(pattern, &mut bound, &mut out);
    out
}

/// `bound` is the names this pattern has bound so far, in order, so that a name written again
/// becomes a test against the position the first occurrence took. The position is well defined
/// across rules that share a prefix: sharing a prefix means having consumed the same shape of
/// subject, so the same number of bindings have been made at any node of the trie.
fn push_steps<'t>(term: &'t Term, bound: &mut Vec<&'t str>, out: &mut Vec<Step>) {
    match &term.kind {
        TermKind::Var(name) => match bound.iter().position(|have| *have == name.as_str()) {
            Some(index) => out.push(Step::Same(index)),
            None => {
                bound.push(name.as_str());
                out.push(Step::Bind(name.clone()));
            }
        },
        TermKind::Int(value) => out.push(Step::Int(*value)),
        TermKind::App { head, args } => {
            out.push(Step::App { head: head.clone(), arity: args.len() });
            for arg in args {
                push_steps(arg, bound, out);
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
        for rule in &node.accept {
            writeln!(f, "{pad}=> rule {rule}")?;
        }
        for (branch, next) in node.branches() {
            match branch {
                Shown::App(head, arity) => writeln!(f, "{pad}{head}/{arity}")?,
                Shown::Int(value) => writeln!(f, "{pad}{value}")?,
                Shown::Same(index) => writeln!(f, "{pad}same as binding {index}")?,
            }
            self.show(f, next, depth + 1)?;
        }
        if let Some((name, next)) = &node.wildcard {
            writeln!(f, "{pad}bind {name}")?;
            self.show(f, *next, depth + 1)?;
        }
        Ok(())
    }
}
